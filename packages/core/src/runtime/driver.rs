use super::*;
use crate::model::prepared_prompt::{
    estimate_text_tokens, ModelInputImageObservationV1, ModelMessageV1, PreparedPromptV1,
};
use crate::model::ModelClientRequest;
use crate::session::supplement::{
    AcknowledgeTurnSupplementsRequest, ClaimTurnSupplementsRequest,
    CloseTurnSupplementQueueRequest, DurableTurnSupplement, TurnSupplementStorePort,
    MAX_PENDING_TURN_SUPPLEMENTS,
};
use crate::tool::ModelToolDefinition;
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub enum TurnInput {
    UserMessage(String),
    TurnSupplement {
        message: String,
        supplement_ids: Vec<String>,
    },
    ToolContinuation {
        objective: String,
    },
    OutputTokenRecovery {
        partial_content: String,
        message: String,
        rejected_tool_calls: Vec<RejectedToolCallIdentity>,
    },
    AnswerNow {
        message: String,
        intervention: AgentRunInterventionV1,
        supplement_ids: Vec<String>,
    },
}

impl TurnInput {
    pub fn objective(&self) -> &str {
        match self {
            Self::UserMessage(message)
            | Self::TurnSupplement { message, .. }
            | Self::OutputTokenRecovery { message, .. } => message,
            Self::ToolContinuation { objective } => objective,
            Self::AnswerNow { message, .. } => message,
        }
    }

    pub fn user_message(&self) -> Option<&str> {
        match self {
            Self::UserMessage(message) | Self::TurnSupplement { message, .. } => Some(message),
            Self::AnswerNow { message, .. } | Self::OutputTokenRecovery { message, .. } => {
                Some(message)
            }
            Self::ToolContinuation { .. } => None,
        }
    }

    pub fn semantic_kind(&self) -> &'static str {
        match self {
            Self::UserMessage(_) => MESSAGE_SEMANTIC_USER_REQUEST,
            Self::TurnSupplement { .. } => MESSAGE_SEMANTIC_TURN_SUPPLEMENT,
            Self::ToolContinuation { .. } => MESSAGE_SEMANTIC_TOOL_CONTINUATION,
            Self::OutputTokenRecovery { .. } => MESSAGE_SEMANTIC_OUTPUT_TOKEN_RECOVERY,
            Self::AnswerNow { .. } => MESSAGE_SEMANTIC_ANSWER_NOW,
        }
    }

    pub(super) fn turn_supplement(supplements: Vec<TurnSupplementInput>) -> Self {
        Self::TurnSupplement {
            message: supplements
                .iter()
                .map(|supplement| supplement.message.as_str())
                .collect::<Vec<_>>()
                .join("\n\n"),
            supplement_ids: supplements
                .into_iter()
                .map(|supplement| supplement.supplement_id)
                .collect(),
        }
    }

    pub(super) fn answer_now(
        intervention: AgentRunInterventionV1,
        supplements: Vec<TurnSupplementInput>,
    ) -> Self {
        let mut parts = supplements
            .iter()
            .map(|supplement| format!("此前已接受的补充要求：\n{}", supplement.message))
            .collect::<Vec<_>>();
        parts.push(
            "用户希望立刻得到回答。停止新增研究或工具调用；基于已经提交的证据给出当前最佳答案，并明确说明尚未覆盖或无法确认的部分。"
                .to_string(),
        );
        Self::AnswerNow {
            message: parts.join("\n\n"),
            intervention,
            supplement_ids: supplements
                .into_iter()
                .map(|supplement| supplement.supplement_id)
                .collect(),
        }
    }

    pub(super) fn answer_now_intervention(&self) -> Option<&AgentRunInterventionV1> {
        match self {
            Self::AnswerNow { intervention, .. } => Some(intervention),
            _ => None,
        }
    }

    pub(super) fn output_token_recovery(
        partial_content: String,
        message: String,
        rejected_tool_calls: Vec<RejectedToolCallIdentity>,
    ) -> Self {
        Self::OutputTokenRecovery {
            partial_content,
            message,
            rejected_tool_calls,
        }
    }

    pub(super) fn output_token_recovery_partial(&self) -> Option<&str> {
        match self {
            Self::OutputTokenRecovery {
                partial_content, ..
            } => Some(partial_content),
            _ => None,
        }
    }

    pub(super) fn output_token_recovery_tool_calls(&self) -> &[RejectedToolCallIdentity] {
        match self {
            Self::OutputTokenRecovery {
                rejected_tool_calls,
                ..
            } => rejected_tool_calls,
            _ => &[],
        }
    }

    pub(super) fn supplement_ids(&self) -> &[String] {
        match self {
            Self::TurnSupplement { supplement_ids, .. }
            | Self::AnswerNow { supplement_ids, .. } => supplement_ids,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedToolCallIdentity {
    pub call_id: String,
    pub tool_name: String,
}

#[derive(Debug)]
struct TurnControlState {
    accepting: bool,
    answer_now_intervention_id: Option<String>,
    pending: VecDeque<TurnControlInput>,
}

#[derive(Debug, Clone)]
pub(super) enum TurnControlInput {
    Supplement(TurnSupplementInput),
    AnswerNow(AgentRunInterventionV1),
}

#[derive(Debug, Clone)]
pub(super) struct TurnSupplementInput {
    pub(super) supplement_id: String,
    pub(super) message: String,
    pub(super) created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerNowEnqueueDisposition {
    Accepted,
    AlreadyConverging,
}

type TurnSupplementMaterializer =
    dyn Fn(&str, &[DurableTurnSupplement]) -> Result<(), String> + Send + Sync;

#[derive(Debug, Clone)]
pub struct DurableTurnControlBinding {
    pub agent_run_id: String,
    pub lifecycle_job_id: String,
    pub session_id: String,
    pub authorization_digest: String,
    pub lease_owner: String,
    pub claim_token: String,
}

#[derive(Clone)]
pub struct TurnControl {
    state: Arc<Mutex<TurnControlState>>,
    changed: Arc<tokio::sync::Notify>,
    durable: Option<Arc<DurableTurnControl>>,
    materialize: Option<Arc<TurnSupplementMaterializer>>,
}

struct DurableTurnControl {
    store: Arc<dyn TurnSupplementStorePort>,
    agent_run_id: String,
    lifecycle_job_id: String,
    session_id: String,
    authorization_digest: String,
    lease_owner: String,
    claim_token: String,
}

impl std::fmt::Debug for TurnControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TurnControl")
            .field("durable", &self.durable.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for TurnControl {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnControl {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TurnControlState {
                accepting: true,
                answer_now_intervention_id: None,
                pending: VecDeque::new(),
            })),
            changed: Arc::new(tokio::sync::Notify::new()),
            durable: None,
            materialize: None,
        }
    }

    pub fn new_with_supplement_materializer(materialize: Arc<TurnSupplementMaterializer>) -> Self {
        let mut control = Self::new();
        control.materialize = Some(materialize);
        control
    }

    pub fn new_durable(
        store: Arc<dyn TurnSupplementStorePort>,
        binding: DurableTurnControlBinding,
        materialize: Arc<TurnSupplementMaterializer>,
    ) -> Result<Self, String> {
        let DurableTurnControlBinding {
            agent_run_id,
            lifecycle_job_id,
            session_id,
            authorization_digest,
            lease_owner,
            claim_token,
        } = binding;
        for (name, value) in [
            ("agentRunId", agent_run_id.as_str()),
            ("lifecycleJobId", lifecycle_job_id.as_str()),
            ("sessionId", session_id.as_str()),
            ("authorizationDigest", authorization_digest.as_str()),
            ("leaseOwner", lease_owner.as_str()),
            ("claimToken", claim_token.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("durable turn control {name} is required"));
            }
        }
        let mut control = Self::new();
        control.durable = Some(Arc::new(DurableTurnControl {
            store,
            agent_run_id,
            lifecycle_job_id,
            session_id,
            authorization_digest,
            lease_owner,
            claim_token,
        }));
        control.materialize = Some(materialize);
        Ok(control)
    }

    pub fn enqueue_supplement_with<F>(&self, message: String, persist: F) -> Result<usize, String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        if self.durable.is_some() {
            return Err("durable_turn_control_requires_store_admission".to_string());
        }
        let message = message.trim().to_string();
        if message.is_empty() {
            return Err("turn supplement message is required".to_string());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "turn supplement queue lock poisoned".to_string())?;
        if state.answer_now_intervention_id.is_some() {
            return Err("turn_supplement_rejected:answer_now_requested".to_string());
        }
        if !state.accepting {
            return Err("turn supplement rejected: task is no longer accepting input".to_string());
        }
        if state.pending.len() >= MAX_PENDING_TURN_SUPPLEMENTS {
            return Err(format!(
                "turn supplement queue is full: max={MAX_PENDING_TURN_SUPPLEMENTS}"
            ));
        }
        persist()?;
        state
            .pending
            .push_back(TurnControlInput::Supplement(TurnSupplementInput {
                supplement_id: format!("supplement-{}", new_turn_id()),
                message,
                created_at_ms: now_ms(),
            }));
        let pending_len = state.pending.len();
        drop(state);
        self.changed.notify_one();
        Ok(pending_len)
    }

    pub fn enqueue_answer_now_with<F>(
        &self,
        intervention: AgentRunInterventionV1,
        persist: F,
    ) -> Result<AnswerNowEnqueueDisposition, String>
    where
        F: FnOnce() -> Result<(), String>,
    {
        intervention.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "turn control queue lock poisoned".to_string())?;
        if state.answer_now_intervention_id.is_some() {
            return Ok(AnswerNowEnqueueDisposition::AlreadyConverging);
        }
        if !state.accepting {
            return Err("agentRunNotActive".to_string());
        }
        persist()?;
        state.accepting = false;
        state.answer_now_intervention_id = Some(intervention.intervention_id.clone());
        state
            .pending
            .push_back(TurnControlInput::AnswerNow(intervention));
        drop(state);
        self.changed.notify_one();
        Ok(AnswerNowEnqueueDisposition::Accepted)
    }

    pub fn is_answer_now_requested(&self) -> Result<bool, String> {
        self.state
            .lock()
            .map(|state| state.answer_now_intervention_id.is_some())
            .map_err(|_| "turn control queue lock poisoned".to_string())
    }

    pub fn close(&self) -> Result<(), String> {
        if let Some(durable) = self.durable.as_ref() {
            durable
                .store
                .close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
                    agent_run_id: durable.agent_run_id.clone(),
                    lifecycle_job_id: durable.lifecycle_job_id.clone(),
                    session_id: durable.session_id.clone(),
                    authorization_digest: durable.authorization_digest.clone(),
                    lease_owner: Some(durable.lease_owner.clone()),
                    reason: "turn_control_closed".to_string(),
                    closed_at_ms: now_ms(),
                })
                .map_err(|error| error.to_string())?;
        }
        self.close_local()
    }

    pub(super) fn close_after_loop(&self) -> Result<(), String> {
        if self.durable.is_some() {
            self.close_local()
        } else {
            self.close()
        }
    }

    fn close_local(&self) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "turn supplement queue lock poisoned".to_string())?;
        state.accepting = false;
        state.pending.clear();
        drop(state);
        self.changed.notify_one();
        Ok(())
    }

    pub fn close_with<T, F>(&self, close: F) -> Result<T, String>
    where
        F: FnOnce() -> Result<T, String>,
    {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "turn supplement queue lock poisoned".to_string())?;
        let result = close();
        if let Some(durable) = self.durable.as_ref() {
            durable
                .store
                .close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
                    agent_run_id: durable.agent_run_id.clone(),
                    lifecycle_job_id: durable.lifecycle_job_id.clone(),
                    session_id: durable.session_id.clone(),
                    authorization_digest: durable.authorization_digest.clone(),
                    lease_owner: Some(durable.lease_owner.clone()),
                    reason: "turn_control_closed".to_string(),
                    closed_at_ms: now_ms(),
                })
                .map_err(|error| error.to_string())?;
        }
        state.accepting = false;
        state.pending.clear();
        drop(state);
        self.changed.notify_one();
        result
    }

    pub async fn wait_for_answer_now_or_close(&self) -> Result<bool, String> {
        loop {
            let notified = self.changed.notified();
            {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| "turn control queue lock poisoned".to_string())?;
                if state.answer_now_intervention_id.is_some() {
                    return Ok(true);
                }
                if !state.accepting {
                    return Ok(false);
                }
            }
            notified.await;
        }
    }

    pub async fn wait_for_pending_or_close(&self) -> Result<bool, String> {
        loop {
            let notified = self.changed.notified();
            {
                let state = self
                    .state
                    .lock()
                    .map_err(|_| "turn control queue lock poisoned".to_string())?;
                if !state.pending.is_empty() {
                    return Ok(true);
                }
                if !state.accepting {
                    return Ok(false);
                }
            }
            notified.await;
        }
    }

    pub fn take_resume_message(&self, turn_id: &str) -> Result<Option<String>, String> {
        if self.durable.is_some() {
            return Err("durable_turn_control_resume_uses_runtime_safe_point".to_string());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "turn control queue lock poisoned".to_string())?;
        let Some(index) = state
            .pending
            .iter()
            .position(|input| matches!(input, TurnControlInput::Supplement(_)))
        else {
            return Ok(None);
        };
        let input = state
            .pending
            .remove(index)
            .ok_or_else(|| "turn control resume input changed while locked".to_string())?;
        drop(state);
        if let Err(error) = self.materialize_inputs(turn_id, std::slice::from_ref(&input)) {
            self.state
                .lock()
                .map_err(|_| "turn supplement queue lock poisoned".to_string())?
                .pending
                .insert(index, input);
            return Err(error);
        }
        match input {
            TurnControlInput::Supplement(supplement) => Ok(Some(supplement.message)),
            TurnControlInput::AnswerNow(_) => {
                Err("turn control resume input changed while locked".to_string())
            }
        }
    }

    pub(super) fn take_pending(&self, turn_id: &str) -> Result<Vec<TurnControlInput>, String> {
        let mut pending = self.take_local_pending(turn_id, false)?;
        pending.extend(self.claim_durable(turn_id, false)?);
        Ok(pending)
    }

    pub(super) fn take_pending_or_close(
        &self,
        turn_id: &str,
    ) -> Result<Vec<TurnControlInput>, String> {
        let mut pending = self.take_local_pending(turn_id, true)?;
        pending.extend(self.claim_durable(turn_id, true)?);
        Ok(pending)
    }

    pub(super) fn acknowledge_supplements(&self, supplement_ids: &[String]) -> Result<(), String> {
        if supplement_ids.is_empty() || self.durable.is_none() {
            return Ok(());
        }
        let durable = self
            .durable
            .as_ref()
            .expect("durable turn control checked above");
        durable
            .store
            .acknowledge_turn_supplements(AcknowledgeTurnSupplementsRequest {
                agent_run_id: durable.agent_run_id.clone(),
                lifecycle_job_id: durable.lifecycle_job_id.clone(),
                session_id: durable.session_id.clone(),
                authorization_digest: durable.authorization_digest.clone(),
                lease_owner: durable.lease_owner.clone(),
                claim_token: durable.claim_token.clone(),
                supplement_ids: supplement_ids.to_vec(),
                acknowledged_at_ms: now_ms(),
            })
            .map_err(|error| error.to_string())
    }

    fn claim_durable(
        &self,
        turn_id: &str,
        close_if_empty: bool,
    ) -> Result<Vec<TurnControlInput>, String> {
        let Some(durable) = self.durable.as_ref() else {
            return Ok(Vec::new());
        };
        let claimed = durable
            .store
            .claim_turn_supplements(ClaimTurnSupplementsRequest {
                agent_run_id: durable.agent_run_id.clone(),
                lifecycle_job_id: durable.lifecycle_job_id.clone(),
                session_id: durable.session_id.clone(),
                authorization_digest: durable.authorization_digest.clone(),
                lease_owner: durable.lease_owner.clone(),
                claim_token: durable.claim_token.clone(),
                now_ms: now_ms(),
                close_if_empty,
                limit: MAX_PENDING_TURN_SUPPLEMENTS,
            })
            .map_err(|error| error.to_string())?;
        if claimed.is_empty() {
            return Ok(Vec::new());
        }
        self.materialize_supplements(turn_id, claimed.as_slice())?;
        Ok(claimed
            .into_iter()
            .map(|supplement| {
                TurnControlInput::Supplement(TurnSupplementInput {
                    supplement_id: supplement.supplement_id,
                    message: supplement.message,
                    created_at_ms: supplement.created_at_ms,
                })
            })
            .collect())
    }

    fn take_local_pending(
        &self,
        turn_id: &str,
        close_if_empty: bool,
    ) -> Result<Vec<TurnControlInput>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "turn supplement queue lock poisoned".to_string())?;
        if state.pending.is_empty() && close_if_empty && self.durable.is_none() {
            state.accepting = false;
            return Ok(Vec::new());
        }
        let pending = state.pending.drain(..).collect::<Vec<_>>();
        drop(state);
        if let Err(error) = self.materialize_inputs(turn_id, pending.as_slice()) {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "turn supplement queue lock poisoned".to_string())?;
            for input in pending.iter().cloned().rev() {
                state.pending.push_front(input);
            }
            return Err(error);
        }
        Ok(pending)
    }

    fn materialize_inputs(&self, turn_id: &str, inputs: &[TurnControlInput]) -> Result<(), String> {
        let supplements = inputs
            .iter()
            .filter_map(|input| match input {
                TurnControlInput::Supplement(supplement) => Some(DurableTurnSupplement {
                    supplement_id: supplement.supplement_id.clone(),
                    sequence: 0,
                    message: supplement.message.clone(),
                    created_at_ms: supplement.created_at_ms,
                    claim_token: None,
                    claim_lease_owner: None,
                }),
                TurnControlInput::AnswerNow(_) => None,
            })
            .collect::<Vec<_>>();
        self.materialize_supplements(turn_id, supplements.as_slice())
    }

    fn materialize_supplements(
        &self,
        turn_id: &str,
        supplements: &[DurableTurnSupplement],
    ) -> Result<(), String> {
        if supplements.is_empty() {
            return Ok(());
        }
        if let Some(materialize) = self.materialize.as_ref() {
            materialize(turn_id, supplements)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessTurnRequest {
    pub session_id: String,
    pub turn_id: String,
    pub input: TurnInput,
    pub agent_run_identity: Option<RuntimeAgentRunIdentityV1>,
    pub generate_result: GenerateResult,
    pub agent_run_resource_usage: AgentRunResourceUsageV1,
}

#[derive(Debug, Clone)]
pub struct TurnStepResult {
    pub turn_id: String,
    pub continuation: QueryContinuation,
    pub checkpoint: Option<CheckpointRecord>,
    pub provider_tool_calls: Vec<ToolCallEnvelope>,
    pub tool_results: Vec<ToolExecutionResult>,
    pub tool_use_summary: Option<String>,
    pub tool_operations_json: Option<String>,
    pub agent_run_resource_usage: AgentRunResourceUsageV1,
    pub runtime_events: Vec<RuntimeEventProjection>,
    pub session_snapshot: SessionStateSnapshot,
}

pub enum ToolSafePoint {
    ModelRequestStarted(ModelRequestStartedV1),
    ProviderUsage {
        turn_id: String,
        usage: ProviderTokenUsageV1,
        recorded_at_ms: i64,
    },
    DurableToolCall {
        session_id: String,
        turn_id: String,
        agent_run_id: String,
        call: ToolCallEnvelope,
        provider_id: String,
        tool_contract_digest: String,
        recorded_at_ms: i64,
    },
    DurableReceipt {
        session_id: String,
        turn_id: String,
        agent_run_id: String,
        call: ToolCallEnvelope,
        result: ToolExecutionResult,
    },
    CompletedTurn(TurnStepResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRequestPurposeV1 {
    Main,
    Compaction,
}

impl ModelRequestPurposeV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Compaction => "compaction",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ModelObservationV1 {
    SystemPrompt {
        content: String,
    },
    #[serde(rename = "message")]
    ContextMessage {
        message: ModelMessageV1,
    },
    InputImage {
        image: ModelInputImageObservationV1,
    },
    ToolCatalog {
        tool_definitions: Vec<ModelToolDefinition>,
    },
    CompactionPrompt {
        message: ModelMessageV1,
    },
}

#[derive(Debug, Clone)]
pub struct ModelRequestStartedV1 {
    pub(super) purpose: ModelRequestPurposeV1,
    pub(super) session_id: String,
    pub(super) turn_id: String,
    pub(super) loop_index: u32,
    pub(super) provider_prompt_cache_key: Option<String>,
    pub(super) provider_prompt_cache_retention: Option<String>,
    pub(super) context_token_estimate: u32,
    pub(super) context_token_breakdown: ContextTokenBreakdownV1,
    pub(super) prepared_prompt_schema: String,
    pub(super) tool_choice: ModelToolChoice,
    pub(super) max_output_tokens: u32,
    pub(super) observations: Vec<ModelObservationV1>,
    pub(super) agent_composition: crate::extension::composition::ResolvedAgentCompositionV1,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextToolTokenEstimateV1 {
    pub provider_id: String,
    pub name: String,
    pub tokens: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextTokenBreakdownV1 {
    pub system_prompt_tokens: u32,
    pub system_tool_tokens: u32,
    pub mcp_tool_tokens: u32,
    pub skills_tokens: u32,
    pub message_tokens: u32,
    pub mcp_tools: Vec<ContextToolTokenEstimateV1>,
}

impl ContextTokenBreakdownV1 {
    pub fn total_tokens(&self) -> u32 {
        self.system_prompt_tokens
            .saturating_add(self.system_tool_tokens)
            .saturating_add(self.mcp_tool_tokens)
            .saturating_add(self.skills_tokens)
            .saturating_add(self.message_tokens)
    }

    pub fn validate(&self, expected_total_tokens: u32) -> Result<(), String> {
        if self.total_tokens() != expected_total_tokens {
            return Err("context token breakdown total mismatch".to_string());
        }
        let mut identities = std::collections::HashSet::new();
        let mcp_total = self.mcp_tools.iter().try_fold(0u32, |total, tool| {
            if tool.provider_id.trim().is_empty()
                || tool.name.trim().is_empty()
                || !identities.insert((tool.provider_id.as_str(), tool.name.as_str()))
            {
                return Err("context token breakdown MCP tool identity is invalid".to_string());
            }
            Ok(total.saturating_add(tool.tokens))
        })?;
        if mcp_total != self.mcp_tool_tokens {
            return Err("context token breakdown MCP tool total mismatch".to_string());
        }
        Ok(())
    }
}

impl ModelRequestStartedV1 {
    pub(super) fn from_request(
        purpose: ModelRequestPurposeV1,
        request: &ModelClientRequest,
        observations: Vec<ModelObservationV1>,
        agent_composition: crate::extension::composition::ResolvedAgentCompositionV1,
    ) -> Result<Self, String> {
        request.prepared_prompt.validate()?;
        crate::extension::composition::validate_resolved_agent_composition(&agent_composition)?;
        let context_token_breakdown = context_token_breakdown(request, &agent_composition)?;
        context_token_breakdown.validate(request.context_token_estimate)?;
        for observation in &observations {
            match observation {
                ModelObservationV1::SystemPrompt { content }
                    if request.prepared_prompt.system_prompt.as_deref()
                        != Some(content.as_str()) =>
                {
                    return Err("model request system prompt observation mismatch".to_string())
                }
                ModelObservationV1::ContextMessage { message }
                    if !request
                        .prepared_prompt
                        .messages
                        .iter()
                        .any(|candidate| candidate == message) =>
                {
                    return Err(format!(
                        "model request context observation is absent from prepared prompt: {}",
                        message.message_id
                    ))
                }
                ModelObservationV1::InputImage { image }
                    if !request
                        .prepared_prompt
                        .input_images
                        .iter()
                        .any(|candidate| {
                            candidate.message_id == image.message_id
                                && candidate.content_type == image.source.content_type()
                                && candidate.placeholder == image.source.placeholder()
                        }) =>
                {
                    return Err(format!(
                        "model request input image observation is absent from prepared prompt: {}",
                        image.message_id
                    ))
                }
                ModelObservationV1::ToolCatalog { tool_definitions }
                    if &request.prepared_prompt.tool_definitions != tool_definitions =>
                {
                    return Err("model request tool catalog observation mismatch".to_string())
                }
                ModelObservationV1::CompactionPrompt { message }
                    if purpose != ModelRequestPurposeV1::Compaction
                        || request.prepared_prompt.messages.len() != 1
                        || request.prepared_prompt.messages.first() != Some(message) =>
                {
                    return Err("model compaction prompt observation mismatch".to_string())
                }
                _ => {}
            }
        }
        let has_system_prompt = observations
            .iter()
            .any(|item| matches!(item, ModelObservationV1::SystemPrompt { .. }));
        if request.prepared_prompt.system_prompt.is_some() != has_system_prompt {
            return Err("model request system prompt observation coverage mismatch".to_string());
        }
        let has_tool_catalog = observations
            .iter()
            .any(|item| matches!(item, ModelObservationV1::ToolCatalog { .. }));
        if request.prepared_prompt.tool_definitions.is_empty() == has_tool_catalog {
            return Err("model request tool catalog observation coverage mismatch".to_string());
        }
        let context_messages = observations
            .iter()
            .filter_map(|item| match item {
                ModelObservationV1::ContextMessage { message } => Some(message),
                _ => None,
            })
            .collect::<Vec<_>>();
        let input_image_observations = observations
            .iter()
            .filter_map(|item| match item {
                ModelObservationV1::InputImage { image } => Some(image),
                _ => None,
            })
            .collect::<Vec<_>>();
        if input_image_observations.len() != request.prepared_prompt.input_images.len() {
            return Err("model request input image observation coverage mismatch".to_string());
        }
        let mut image_identities = std::collections::HashSet::new();
        for image in input_image_observations {
            image.source.validate()?;
            if !image_identities.insert((image.message_id.as_str(), image.source.placeholder())) {
                return Err("model request input image observation duplicate".to_string());
            }
        }
        match purpose {
            ModelRequestPurposeV1::Main
                if !context_messages
                    .iter()
                    .copied()
                    .eq(request.prepared_prompt.messages.iter()) =>
            {
                return Err(
                    "model request context observation coverage or order mismatch".to_string(),
                )
            }
            ModelRequestPurposeV1::Compaction if !context_messages.is_empty() => {
                return Err(
                    "compaction model request cannot contain context message observations"
                        .to_string(),
                )
            }
            _ => {}
        }
        let compaction_prompts = observations
            .iter()
            .filter(|item| matches!(item, ModelObservationV1::CompactionPrompt { .. }))
            .count();
        if matches!(purpose, ModelRequestPurposeV1::Main) && compaction_prompts != 0 {
            return Err(
                "main model request cannot contain a compaction prompt observation".to_string(),
            );
        }
        if matches!(purpose, ModelRequestPurposeV1::Compaction) && compaction_prompts != 1 {
            return Err(
                "compaction model request requires one compaction prompt observation".to_string(),
            );
        }
        Ok(Self {
            purpose,
            session_id: request.session_id.clone(),
            turn_id: request.turn_id.clone(),
            loop_index: request.loop_index,
            provider_prompt_cache_key: request.provider_prompt_cache_key.clone(),
            provider_prompt_cache_retention: request.provider_prompt_cache_retention.clone(),
            context_token_estimate: request.context_token_estimate,
            context_token_breakdown,
            prepared_prompt_schema: request.prepared_prompt.schema.clone(),
            tool_choice: request.prepared_prompt.tool_choice.clone(),
            max_output_tokens: request.prepared_prompt.max_output_tokens,
            observations,
            agent_composition,
        })
    }

    pub fn session_id(&self) -> &str {
        self.session_id.as_str()
    }

    pub fn turn_id(&self) -> &str {
        self.turn_id.as_str()
    }

    pub fn purpose(&self) -> ModelRequestPurposeV1 {
        self.purpose
    }

    pub fn context_token_estimate(&self) -> u32 {
        self.context_token_estimate
    }

    pub fn context_token_breakdown(&self) -> &ContextTokenBreakdownV1 {
        &self.context_token_breakdown
    }
}

fn context_token_breakdown(
    request: &ModelClientRequest,
    composition: &crate::extension::composition::ResolvedAgentCompositionV1,
) -> Result<ContextTokenBreakdownV1, String> {
    let system_prompt_tokens = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .map(estimate_text_tokens)
        .unwrap_or_default();
    let mut skills_tokens = 0u32;
    let mut message_tokens = 0u32;
    for message in &request.prepared_prompt.messages {
        let serialized = serde_json::to_string(message).map_err(|error| {
            format!("serialize model message for context breakdown failed: {error}")
        })?;
        let tokens = estimate_text_tokens(serialized.as_str());
        if message.message_id.ends_with(":skill_catalog") {
            skills_tokens = skills_tokens.saturating_add(tokens);
        } else {
            message_tokens = message_tokens.saturating_add(tokens);
        }
    }
    message_tokens = message_tokens
        .saturating_add((request.prepared_prompt.input_images.len() as u32).saturating_mul(1_024));

    let tool_tokens = if request.prepared_prompt.tool_definitions.is_empty() {
        0
    } else {
        let serialized =
            serde_json::to_string(&request.prepared_prompt.tool_definitions).map_err(|error| {
                format!("serialize tool catalog for context breakdown failed: {error}")
            })?;
        estimate_text_tokens(serialized.as_str())
    };
    let mut weighted_tools = Vec::with_capacity(request.prepared_prompt.tool_definitions.len());
    for definition in &request.prepared_prompt.tool_definitions {
        let contract = composition
            .tool_contracts
            .iter()
            .find(|contract| contract.name == definition.name)
            .ok_or_else(|| {
                format!(
                    "context breakdown tool contract missing: {}",
                    definition.name
                )
            })?;
        let serialized = serde_json::to_string(definition)
            .map_err(|error| format!("serialize tool for context breakdown failed: {error}"))?;
        weighted_tools.push((
            contract.provider_id.clone(),
            definition.name.clone(),
            contract.category == "external.mcp",
            estimate_text_tokens(serialized.as_str()),
        ));
    }
    let total_weight = weighted_tools
        .iter()
        .fold(0u32, |total, (_, _, _, weight)| {
            total.saturating_add(*weight)
        });
    let mut allocated_tokens = 0u32;
    let mut system_tool_tokens = 0u32;
    let mut mcp_tool_tokens = 0u32;
    let mut mcp_tools = Vec::new();
    let weighted_tool_count = weighted_tools.len();
    for (index, (provider_id, name, is_mcp, weight)) in weighted_tools.into_iter().enumerate() {
        let tokens = if index + 1 == weighted_tool_count {
            tool_tokens.saturating_sub(allocated_tokens)
        } else if total_weight == 0 {
            0
        } else {
            ((u64::from(tool_tokens) * u64::from(weight)) / u64::from(total_weight)) as u32
        };
        allocated_tokens = allocated_tokens.saturating_add(tokens);
        if is_mcp {
            mcp_tool_tokens = mcp_tool_tokens.saturating_add(tokens);
            mcp_tools.push(ContextToolTokenEstimateV1 {
                provider_id,
                name,
                tokens,
            });
        } else {
            system_tool_tokens = system_tool_tokens.saturating_add(tokens);
        }
    }
    Ok(ContextTokenBreakdownV1 {
        system_prompt_tokens,
        system_tool_tokens,
        mcp_tool_tokens,
        skills_tokens,
        message_tokens,
        mcp_tools,
    })
}

#[derive(Debug, Clone)]
pub struct AgentRunRequest {
    pub session_id: String,
    pub initial_turn_id: String,
    pub user_message: String,
    pub agent_run_identity: Option<RuntimeAgentRunIdentityV1>,
    pub runtime_scope: PromptCompactionScopeV1,
    pub resume_from_turn_id: Option<String>,
    pub auto_continue_after_resume_wait: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct GenerateDriverRequest {
    pub session_id: String,
    pub turn_id: String,
    pub loop_index: u32,
    pub provider_prompt_cache_key: Option<String>,
    pub provider_prompt_cache_retention: Option<String>,
    pub system_prompt_manifest_json: Option<String>,
    pub compression_stats_json: Option<String>,
    pub context_token_estimate: u32,
    pub prepared_prompt: PreparedPromptV1,
    pub observations: Vec<ModelObservationV1>,
    pub live_content_prefix: String,
}

#[derive(Debug, Clone)]
pub struct GenerateDriverOutcome {
    pub generate_result: GenerateResult,
    pub provider_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct GenerateDriverError {
    pub message: String,
    pub provider_code: Option<String>,
    pub provider_attempts: u32,
    pub partial_content: String,
    pub truncated_tool_calls: Vec<crate::model::TruncatedToolCall>,
}

impl GenerateDriverError {
    pub fn from_model_client(
        error: crate::model::ModelClientError,
        partial_content: String,
    ) -> Self {
        let provider_code = error.provider_code.clone();
        let provider_attempts = error.provider_attempts;
        let truncated_tool_calls = error.truncated_tool_calls.clone();
        Self {
            message: format!(
                "model_client_error(kind={},retryable={},providerCode={},providerAttempts={}): {}",
                error.kind.as_str(),
                error.retryable,
                provider_code.as_deref().unwrap_or("none"),
                provider_attempts,
                error.message
            ),
            provider_code,
            provider_attempts,
            partial_content,
            truncated_tool_calls,
        }
    }

    pub fn is_output_token_limit(&self) -> bool {
        self.provider_code.as_deref() == Some("incomplete_output_token_limit")
    }
}

impl From<GenerateDriverError> for String {
    fn from(error: GenerateDriverError) -> Self {
        error.message
    }
}

impl From<String> for GenerateDriverError {
    fn from(message: String) -> Self {
        Self {
            message,
            provider_code: None,
            provider_attempts: 0,
            partial_content: String::new(),
            truncated_tool_calls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnUpdate {
    ModelRequestStart {
        session_id: String,
        turn_id: String,
        purpose: ModelRequestPurposeV1,
        context_token_estimate: u32,
        message: Option<String>,
        process_state: RuntimeProcessState,
        elapsed_ms: u64,
        initial_content: String,
    },
    Token {
        session_id: String,
        turn_id: String,
        content: String,
    },
    ReplaceContent {
        session_id: String,
        turn_id: String,
        content: String,
    },
    Status {
        session_id: String,
        turn_id: String,
        message: Option<String>,
        process_state: RuntimeProcessState,
    },
    ToolCallPreparing {
        session_id: String,
        turn_id: String,
        name: String,
        process_state: RuntimeProcessState,
    },
    ToolCallReady {
        session_id: String,
        turn_id: String,
        call_id: String,
        provider_item_id: Option<String>,
        name: String,
        process_state: RuntimeProcessState,
        args_json: String,
        args_preview: String,
    },
    ModelDone {
        session_id: String,
        turn_id: String,
        finish_reason: Option<String>,
        process_state: RuntimeProcessState,
    },
    RuntimeError {
        session_id: String,
        turn_id: String,
        message: String,
        reason: String,
        retryable: bool,
        process_state: RuntimeProcessState,
    },
    RuntimeEvent {
        event: RuntimeEventProjection,
    },
}

pub type GenerateDriverFuture<'a, TValue> =
    Pin<Box<dyn Future<Output = Result<TValue, GenerateDriverError>> + Send + 'a>>;

#[derive(Debug)]
pub struct GenerateDriverPromptCompactionOutcome {
    pub result: Result<String, PromptCompactionError>,
    pub resource_usage: AgentRunResourceUsageV1,
}

pub type GenerateDriverPromptCompactionFuture<'a> =
    Pin<Box<dyn Future<Output = Option<GenerateDriverPromptCompactionOutcome>> + Send + 'a>>;

pub trait AsyncGenerateDriver: Send + Sync {
    fn generate_next_async<'a>(
        &'a self,
        req: &'a GenerateDriverRequest,
    ) -> GenerateDriverFuture<'a, GenerateDriverOutcome>;

    fn generate_prompt_compaction_summary_async<'a>(
        &'a self,
        _request: &'a ModelCompactionSummaryCandidateRequest,
    ) -> GenerateDriverPromptCompactionFuture<'a> {
        Box::pin(async { None })
    }

    fn generate_next_with_sink_async<'a>(
        &'a self,
        req: &'a GenerateDriverRequest,
        _sink: &'a mut (dyn FnMut(TurnUpdate) + Send),
    ) -> GenerateDriverFuture<'a, GenerateDriverOutcome> {
        self.generate_next_async(req)
    }
}

pub(super) struct AsyncGenerateDriverPromptCompactionCandidateProducer<'a, D: AsyncGenerateDriver> {
    pub(super) driver: &'a D,
    pub(super) resource_usage: Mutex<AgentRunResourceUsageV1>,
}

impl<D: AsyncGenerateDriver> AsyncModelCompactionSummaryCandidateProducer
    for AsyncGenerateDriverPromptCompactionCandidateProducer<'_, D>
{
    fn produce_model_compaction_summary_async<'a>(
        &'a self,
        request: &'a ModelCompactionSummaryCandidateRequest,
    ) -> crate::model::prompt::ModelCompactionSummaryFuture<'a> {
        Box::pin(async move {
            let outcome = self
                .driver
                .generate_prompt_compaction_summary_async(request)
                .await
                .unwrap_or_else(|| GenerateDriverPromptCompactionOutcome {
                    result: Err(PromptCompactionError::provider(
                        "async generate driver does not support model prompt compaction",
                    )),
                    resource_usage: AgentRunResourceUsageV1::default(),
                });
            self.resource_usage
                .lock()
                .map_err(|_| {
                    PromptCompactionError::provider(
                        "root AgentRun resource usage lock poisoned after prompt compaction",
                    )
                })?
                .merge(&outcome.resource_usage);
            outcome.result
        })
    }
}

#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub turn_responses: Vec<TurnStepResult>,
    pub stop: AgentRunStop,
}

impl AgentRunResult {
    pub(super) fn new(turn_responses: Vec<TurnStepResult>, stop: AgentRunStop) -> Self {
        Self {
            turn_responses,
            stop,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunStop {
    Finalized,
    TerminalTool,
    QuestionWait,
    RuntimeJobWait,
    Cancelled(String),
}

impl AgentRunStop {
    pub fn reason(&self) -> &str {
        match self {
            Self::Finalized => "finalized",
            Self::TerminalTool => "terminal_tool",
            Self::QuestionWait => "question_wait",
            Self::RuntimeJobWait => "runtime_job_wait",
            Self::Cancelled(reason) => reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunResumeIntent {
    PermissionResolved { approved: bool },
    QuestionAnswered(String),
    RuntimeJobsTerminal,
    AnswerNow,
}

impl AgentRunResumeIntent {
    pub fn into_user_message(self) -> String {
        match self {
            Self::PermissionResolved { approved: true } => {
                "Approved. Continue the paused tool execution.".to_string()
            }
            Self::PermissionResolved { approved: false } => {
                "Rejected. Continue without executing the rejected tool call.".to_string()
            }
            Self::QuestionAnswered(message) => message,
            Self::RuntimeJobsTerminal => {
                "The scheduled tool work reached a terminal state. Continue from its durable result."
                    .to_string()
            }
            Self::AnswerNow => "The user requested an immediate answer. Stop adding research or tool work and converge from the evidence already committed. State any remaining uncertainty."
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRunResumeIntent, AgentRunStop, AnswerNowEnqueueDisposition, TurnControl,
        TurnControlInput,
    };
    use crate::runtime::contracts::AgentRunInterventionV1;
    use std::sync::{Arc, Mutex};

    #[test]
    fn agent_run_stop_contract_is_typed_for_every_runtime_boundary() {
        assert_eq!(AgentRunStop::Finalized.reason(), "finalized");
        assert_eq!(AgentRunStop::TerminalTool.reason(), "terminal_tool");
        assert_eq!(AgentRunStop::QuestionWait.reason(), "question_wait");
        assert_eq!(AgentRunStop::RuntimeJobWait.reason(), "runtime_job_wait");
    }

    #[test]
    fn agent_run_resume_intent_owns_semantic_messages() {
        assert!(AgentRunResumeIntent::PermissionResolved { approved: true }
            .into_user_message()
            .starts_with("Approved."));
        assert!(AgentRunResumeIntent::PermissionResolved { approved: false }
            .into_user_message()
            .starts_with("Rejected."));
        assert_eq!(
            AgentRunResumeIntent::QuestionAnswered("answer".to_string()).into_user_message(),
            "answer"
        );
        assert!(AgentRunResumeIntent::RuntimeJobsTerminal
            .into_user_message()
            .contains("terminal state"));
        assert!(AgentRunResumeIntent::AnswerNow
            .into_user_message()
            .contains("immediate answer"));
    }

    #[test]
    fn memory_supplement_is_materialized_on_its_consuming_turn() {
        let materialized = Arc::new(Mutex::new(None));
        let captured = materialized.clone();
        let control =
            TurnControl::new_with_supplement_materializer(Arc::new(move |turn_id, supplements| {
                *captured.lock().expect("capture materialized supplement") = Some((
                    turn_id.to_string(),
                    supplements
                        .first()
                        .expect("one materialized supplement")
                        .message
                        .clone(),
                ));
                Ok(())
            }));
        control
            .enqueue_supplement_with("new constraint".to_string(), || Ok(()))
            .expect("enqueue supplement");

        control
            .take_pending("turn-consuming")
            .expect("consume supplement");

        assert_eq!(
            *materialized.lock().expect("read materialized supplement"),
            Some(("turn-consuming".to_string(), "new constraint".to_string()))
        );
    }

    #[test]
    fn failed_close_callback_still_closes_supplement_queue() {
        let control = TurnControl::new();
        control
            .enqueue_supplement_with("queued".to_string(), || Ok(()))
            .expect("enqueue supplement");

        let error = control
            .close_with::<(), _>(|| Err("cancel persistence failed".to_string()))
            .expect_err("close callback failure");
        assert_eq!(error, "cancel persistence failed");
        assert!(control
            .enqueue_supplement_with("late".to_string(), || Ok(()))
            .expect_err("failed cancellation still closes supplement input")
            .contains("no longer accepting input"));
    }

    #[test]
    fn answer_now_preserves_earlier_supplements_and_closes_admission() {
        let control = TurnControl::new();
        control
            .enqueue_supplement_with("earlier evidence".to_string(), || Ok(()))
            .expect("enqueue supplement");
        assert_eq!(
            control
                .enqueue_answer_now_with(
                    AgentRunInterventionV1::answer_now("intervention-1", "agent-run-1"),
                    || Ok(())
                )
                .expect("enqueue answer now"),
            AnswerNowEnqueueDisposition::Accepted
        );
        assert_eq!(
            control
                .enqueue_answer_now_with(
                    AgentRunInterventionV1::answer_now("intervention-1", "agent-run-1"),
                    || Err("duplicate must not persist".to_string())
                )
                .expect("duplicate answer now"),
            AnswerNowEnqueueDisposition::AlreadyConverging
        );
        assert_eq!(
            control
                .enqueue_supplement_with("late".to_string(), || Ok(()))
                .expect_err("late supplement must fail"),
            "turn_supplement_rejected:answer_now_requested"
        );

        let pending = control.take_pending("turn-1").expect("drain control");
        assert!(matches!(
            pending.as_slice(),
            [TurnControlInput::Supplement(message), TurnControlInput::AnswerNow(intervention)]
                if message.message == "earlier evidence" && intervention.intervention_id == "intervention-1"
        ));
    }

    #[tokio::test]
    async fn waiters_observe_queued_input_and_answer_now() {
        let supplement_control = TurnControl::new();
        let producer = supplement_control.clone();
        tokio::spawn(async move {
            producer
                .enqueue_supplement_with("answer".to_string(), || Ok(()))
                .expect("enqueue answer");
        })
        .await
        .expect("supplement producer");
        assert!(supplement_control
            .wait_for_pending_or_close()
            .await
            .expect("wait for supplement"));
        assert_eq!(
            supplement_control
                .take_resume_message("turn-resume")
                .expect("take resume message")
                .as_deref(),
            Some("answer")
        );

        let answer_now_control = TurnControl::new();
        answer_now_control
            .enqueue_answer_now_with(
                AgentRunInterventionV1::answer_now("intervention-wait", "agent-run-wait"),
                || Ok(()),
            )
            .expect("enqueue answer now");
        assert!(answer_now_control
            .wait_for_answer_now_or_close()
            .await
            .expect("wait for answer now"));
    }
}
