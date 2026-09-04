use super::tool_projection::{build_generate_tool_contracts, build_generate_tool_projection};
use super::*;

type PreparedToolCall = (
    Option<ToolCallEnvelope>,
    Vec<String>,
    Option<ToolExecutionResult>,
);
use crate::extension::hooks::{
    compose_permission_decision_with_hook, LifecycleHookPermissionDecisionV1,
};
use sha2::{Digest, Sha256};

const TOOL_EXECUTION_INTENT_SCHEMA_V1: &str = "tool_execution.intent.v1";
const TOOL_EXECUTION_RECEIPT_SCHEMA_V1: &str = "tool_execution.receipt.v1";
const POST_TOOL_HOOK_INTENT_SCHEMA_V1: &str = "post_tool_hook.intent.v1";
const POST_TOOL_HOOK_RECEIPT_SCHEMA_V1: &str = "post_tool_hook.receipt.v1";
const AGENT_TOOL_RESULT_SCHEMA_V1: &str = "agent_tool_result_v1";
const TASK_OUTPUT_REF_SCHEMA_V1: &str = "task_output_ref_v1";
const AGENT_TASK_OUTPUT_WAIT_SCHEMA_V1: &str = "agent_task_output_wait_v1";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentToolArgsV1 {
    prompt: String,
    description: String,
    #[serde(default)]
    budget: Option<AgentToolBudgetV1>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentToolBudgetV1 {
    max_summary_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentTaskOutputRefV1 {
    schema: String,
    kind: String,
    runtime_job_id: String,
    child_session_id: String,
    result_ref: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskOutputArgsV1 {
    output_ref: AgentTaskOutputModelRefV1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentTaskOutputModelRefV1 {
    schema: String,
    kind: String,
    runtime_job_id: String,
    child_session_id: String,
    result_ref: String,
}

impl From<AgentTaskOutputModelRefV1> for AgentTaskOutputRefV1 {
    fn from(value: AgentTaskOutputModelRefV1) -> Self {
        Self {
            schema: value.schema,
            kind: value.kind,
            runtime_job_id: value.runtime_job_id,
            child_session_id: value.child_session_id,
            result_ref: value.result_ref,
        }
    }
}

impl AgentToolArgsV1 {
    fn validate(mut self) -> Result<Self, String> {
        self.prompt = required_agent_arg(self.prompt, "prompt")?;
        self.description = required_agent_arg(self.description, "description")?;
        let max_summary_chars = self
            .budget
            .as_ref()
            .map(|budget| budget.max_summary_chars)
            .unwrap_or(4_000);
        if !(1..=16_000).contains(&max_summary_chars) {
            return Err("Agent budget.max_summary_chars must be between 1 and 16000".to_string());
        }
        Ok(self)
    }

    fn max_summary_chars(&self) -> usize {
        self.budget
            .as_ref()
            .map(|budget| budget.max_summary_chars)
            .unwrap_or(4_000)
    }
}

impl AgentTaskOutputRefV1 {
    fn validate(&self) -> Result<(), String> {
        if self.schema != TASK_OUTPUT_REF_SCHEMA_V1 {
            return Err(format!(
                "TaskOutput output_ref schema mismatch: expected={TASK_OUTPUT_REF_SCHEMA_V1} actual={}",
                self.schema
            ));
        }
        if self.kind != "agent" {
            return Err(format!(
                "TaskOutput output_ref kind mismatch: expected=agent actual={}",
                self.kind
            ));
        }
        for (field, value) in [
            ("runtime_job_id", self.runtime_job_id.as_str()),
            ("child_session_id", self.child_session_id.as_str()),
            ("result_ref", self.result_ref.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("TaskOutput output_ref {field} is required"));
            }
        }
        if !self
            .result_ref
            .starts_with(runtime_external_context_keys::SUBAGENT_RESULT_PREFIX)
        {
            return Err(format!(
                "TaskOutput output_ref result_ref is invalid: {}",
                self.result_ref
            ));
        }
        Ok(())
    }
}

fn required_agent_arg(value: String, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("Agent requires {field}"))
    } else {
        Ok(value.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ToolExecutionIntentV1 {
    schema: String,
    pub(super) session_id: String,
    pub(super) turn_id: String,
    pub(super) agent_run_identity: Option<RuntimeAgentRunIdentityV1>,
    tool_call_id: String,
    source_tool_name: String,
    session_tool_call_event_id: String,
    pub(super) provider_id: String,
    pub(super) tool_contract_digest: String,
    model_args_digest: String,
    args_digest: String,
    effective_args_json: String,
    pub(super) recorded_at_ms: i64,
}

#[expect(
    clippy::too_many_arguments,
    reason = "tool commit boundary keeps durable identity fields explicit"
)]
fn commit_session_tool_call(
    sink: Option<&ToolSafePointDispatcher<'_>>,
    session_id: &str,
    turn_id: &str,
    agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
    call: &ToolCallEnvelope,
    provider_id: &str,
    tool_contract_digest: &str,
    recorded_at_ms: i64,
) -> Result<(), String> {
    let Some(sink) = sink else {
        #[cfg(test)]
        return Ok(());
        #[cfg(not(test))]
        return Err("tool execution requires a durable Session tool_call commit port".to_string());
    };
    let agent_run_id = agent_run_identity
        .ok_or_else(|| "durable Session tool_call requires AgentRun identity".to_string())?
        .agent_run_id
        .clone();
    sink.commit(ToolSafePoint::DurableToolCall {
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        agent_run_id,
        call: call.clone(),
        provider_id: provider_id.to_string(),
        tool_contract_digest: tool_contract_digest.to_string(),
        recorded_at_ms,
    })
}

fn notify_tool_safe_point(
    sink: Option<&ToolSafePointDispatcher<'_>>,
    intent: &ToolExecutionIntentV1,
    call: &ToolCallEnvelope,
    result: &ToolExecutionResult,
) -> Result<(), String> {
    if let Some(sink) = sink {
        let agent_run_id = intent
            .agent_run_identity
            .as_ref()
            .ok_or_else(|| "durable Session tool_result requires AgentRun identity".to_string())?
            .agent_run_id
            .clone();
        sink.commit(ToolSafePoint::DurableReceipt {
            session_id: intent.session_id.clone(),
            turn_id: intent.turn_id.clone(),
            agent_run_id,
            call: call.clone(),
            result: result.clone(),
        })?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ToolExecutionReceiptV1 {
    schema: String,
    session_id: String,
    turn_id: String,
    tool_call_id: String,
    source_tool_name: String,
    args_digest: String,
    effective_args_json: String,
    pre_hook_contexts: Vec<String>,
    run_post_hook: bool,
    result_json: String,
}

impl ToolExecutionReceiptV1 {
    fn decode_result(&self) -> Result<ToolExecutionResult, String> {
        serde_json::from_str(self.result_json.as_str())
            .map_err(|error| format!("decode tool execution receipt result failed: {error}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PostToolHookIntentV1 {
    schema: String,
    session_id: String,
    turn_id: String,
    tool_call_id: String,
    result_digest: String,
    recorded_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PostToolHookReceiptV1 {
    schema: String,
    session_id: String,
    turn_id: String,
    tool_call_id: String,
    result_digest: String,
    contexts: Vec<String>,
}

#[derive(Default)]
struct ToolExecutionFactIndex {
    intents: HashMap<String, ToolExecutionIntentV1>,
    receipts: HashMap<String, ToolExecutionReceiptV1>,
    post_hook_intents: HashMap<String, PostToolHookIntentV1>,
    post_hook_receipts: HashMap<String, PostToolHookReceiptV1>,
}

fn build_permission_tool_result(
    call: ToolCallEnvelope,
    permission: PermissionDecision,
    status: &str,
    reason: &str,
    transition_reason: &str,
) -> ToolExecutionResult {
    let now = now_ms();
    let permission_reason = permission.reason.clone();
    let diagnostic_id = call.id.clone();
    ToolExecutionResult {
        tool_call_id: call.id,
        tool_name: call.name,
        status: status.to_string(),
        content: permission_reason.clone(),
        details: json!({
            "schema": "permission_tool_result_v1",
            "status": status,
            "reason": reason,
            "transitionReason": transition_reason,
            "message": permission_reason,
            "permissionDecision": permission.audit_json(),
        }),
        facts: Vec::new(),
        error: Some(
            ToolErrorInfo::new(
                ToolFailureKind::PermissionDenied,
                permission_reason.clone(),
                permission_reason,
            )
            .with_diagnostic(format!("permission_decision_v1:{diagnostic_id}")),
        ),
        started_at_ms: now,
        completed_at_ms: now,
        latency_ms: 0,
        parallel_group: None,
        transition_reason: Some(transition_reason.to_string()),
    }
}

fn build_lifecycle_hook_tool_result(
    call: ToolCallEnvelope,
    status: &str,
    reason: &str,
    message: &str,
) -> ToolExecutionResult {
    let now = now_ms();
    ToolExecutionResult {
        tool_call_id: call.id,
        tool_name: call.name,
        status: status.to_string(),
        content: message.to_string(),
        details: json!({
            "schema": "lifecycle_hook_tool_result_v1",
            "status": status,
            "reason": reason,
            "message": message,
        }),
        facts: Vec::new(),
        error: Some(
            ToolErrorInfo::new(
                ToolFailureKind::PermissionDenied,
                message.to_string(),
                message.to_string(),
            )
            .with_diagnostic(format!("lifecycle_hook:{reason}")),
        ),
        started_at_ms: now,
        completed_at_ms: now,
        latency_ms: 0,
        parallel_group: None,
        transition_reason: Some(reason.to_string()),
    }
}

fn parse_lifecycle_hook_tool_input(args_json: &str) -> Value {
    serde_json::from_str::<Value>(args_json).unwrap_or_else(|_| json!({ "rawArgsJson": args_json }))
}

fn lifecycle_hook_context_texts(outcome: &QueryLifecycleHookOutcome) -> Vec<String> {
    outcome
        .additional_context
        .iter()
        .map(|item| item.text.clone())
        .collect()
}

fn lifecycle_hook_post_tool_payload(
    call: &ToolCallEnvelope,
    report: &ToolExecutionResult,
) -> Value {
    json!({
        "toolCallId": report.tool_call_id,
        "toolName": report.tool_name,
        "status": report.status,
        "transitionReason": report.transition_reason,
        "hasError": report.error.is_some(),
        "errorKind": report.error.as_ref().map(|error| error.kind.as_str()),
        "toolInput": parse_lifecycle_hook_tool_input(call.args_json.as_str()),
    })
}

fn sha256_digest(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn tool_fact_event_id(prefix: &str, session_id: &str, turn_id: &str, tool_call_id: &str) -> String {
    let identity = format!("{session_id}\0{turn_id}\0{tool_call_id}");
    format!("{prefix}:{}", sha256_digest(identity.as_bytes()))
}

fn tool_execution_intent_event_id(session_id: &str, turn_id: &str, tool_call_id: &str) -> String {
    tool_fact_event_id("tool_execution.intent", session_id, turn_id, tool_call_id)
}

fn tool_execution_receipt_event_id(session_id: &str, turn_id: &str, tool_call_id: &str) -> String {
    tool_fact_event_id("tool_execution.receipt", session_id, turn_id, tool_call_id)
}

fn post_tool_hook_intent_event_id(session_id: &str, turn_id: &str, tool_call_id: &str) -> String {
    tool_fact_event_id("post_tool_hook.intent", session_id, turn_id, tool_call_id)
}

fn post_tool_hook_receipt_event_id(session_id: &str, turn_id: &str, tool_call_id: &str) -> String {
    tool_fact_event_id("post_tool_hook.receipt", session_id, turn_id, tool_call_id)
}

fn internal_tool_fact_event<T: Serialize>(
    event_id: String,
    session_id: &str,
    turn_id: &str,
    event_type: &str,
    at_ms: i64,
    payload: &T,
) -> Result<RuntimeEvent, String> {
    Ok(RuntimeEvent {
        event_id,
        session_id: session_id.to_string(),
        task_id: Some(turn_id.to_string()),
        event_type: event_type.to_string(),
        at_ms,
        visibility: EventVisibility::Internal,
        payload_json: serde_json::to_string(payload)
            .map_err(|error| format!("serialize {event_type} failed: {error}"))?,
    })
}

fn indeterminate_tool_execution_result(intent: &ToolExecutionIntentV1) -> ToolExecutionResult {
    let at_ms = now_ms();
    ToolExecutionResult {
        tool_call_id: intent.tool_call_id.clone(),
        tool_name: intent.source_tool_name.clone(),
        status: "error".to_string(),
        content: "The previous tool attempt ended without a durable completion receipt. It was not repeated because its side effects may already have happened. Inspect current state before deciding how to recover."
            .to_string(),
        details: json!({
            "schema": "tool_execution_indeterminate.v1",
            "toolCallId": intent.tool_call_id,
            "toolName": intent.source_tool_name,
            "argsDigest": intent.args_digest,
            "reexecuted": false,
        }),
        facts: Vec::new(),
        error: Some(
            ToolErrorInfo::new(
                ToolFailureKind::Unknown,
                "Tool outcome is indeterminate after runtime recovery; inspect state before retrying",
                "Tool outcome could not be confirmed after recovery",
            )
            .with_diagnostic(format!(
                "tool_execution_indeterminate:{}:{}",
                intent.turn_id, intent.tool_call_id
            )),
        ),
        started_at_ms: intent.recorded_at_ms,
        completed_at_ms: at_ms,
        latency_ms: at_ms.saturating_sub(intent.recorded_at_ms),
        parallel_group: None,
        transition_reason: Some(
            crate::tool::layer::EXECUTION_CANCELLATION_INDETERMINATE.to_string(),
        ),
    }
}

fn unstarted_session_tool_execution_result(
    call: &ToolCallEnvelope,
    started_at_ms: i64,
) -> ToolExecutionResult {
    let at_ms = now_ms();
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status: "cancelled".to_string(),
        content: "The tool call was cancelled before execution and was not replayed.".to_string(),
        details: json!({
            "schema": "tool_result_tombstone_v1",
            "status": "cancelled",
            "reason": "agent_run_interrupted_before_tool_execution",
            "toolCallId": call.id,
            "toolName": call.name,
            "argsDigest": sha256_digest(call.args_json.as_bytes()),
            "reexecuted": false,
        }),
        facts: Vec::new(),
        error: Some(
            ToolErrorInfo::new(
                ToolFailureKind::Cancelled,
                "tool call cancelled before execution",
                "Tool call cancelled before execution",
            )
            .with_diagnostic(format!("tool_execution_not_started:{}", call.id)),
        ),
        started_at_ms,
        completed_at_ms: at_ms,
        latency_ms: at_ms.saturating_sub(started_at_ms),
        parallel_group: None,
        transition_reason: Some("agent_run_interrupted_before_tool_execution".to_string()),
    }
}

impl<
        S: RuntimeStore
            + ExternalContextStorePort
            + RuntimeJobStorePort
            + RuntimeStoreTransactionPort
            + AgentRuntimeSnapshotStorePort
            + Clone
            + Send
            + Sync
            + 'static,
    > AgentRuntime<S>
{
    fn load_tool_execution_facts(
        runtime_store: &S,
        session_id: &str,
        turn_id: &str,
    ) -> Result<ToolExecutionFactIndex, String> {
        const PAGE_SIZE: usize = 256;
        let mut facts = ToolExecutionFactIndex::default();
        let mut offset = 0usize;
        loop {
            let events = runtime_store
                .list_events(session_id, PAGE_SIZE, offset)
                .map_err(|error| error.to_string())?;
            let count = events.len();
            for event in events.into_iter().filter(|event| {
                event.task_id.as_deref() == Some(turn_id)
                    && matches!(
                        event.event_type.as_str(),
                        TOOL_EXECUTION_INTENT_SCHEMA_V1
                            | TOOL_EXECUTION_RECEIPT_SCHEMA_V1
                            | POST_TOOL_HOOK_INTENT_SCHEMA_V1
                            | POST_TOOL_HOOK_RECEIPT_SCHEMA_V1
                    )
            }) {
                match event.event_type.as_str() {
                    TOOL_EXECUTION_INTENT_SCHEMA_V1 => {
                        let intent = serde_json::from_str::<ToolExecutionIntentV1>(
                            event.payload_json.as_str(),
                        )
                        .map_err(|error| format!("decode tool execution intent failed: {error}"))?;
                        if intent.schema != TOOL_EXECUTION_INTENT_SCHEMA_V1
                            || intent.session_id != session_id
                            || intent.turn_id != turn_id
                            || intent.session_tool_call_event_id
                                != crate::runtime::canonical_tool_call_event_id(
                                    session_id,
                                    turn_id,
                                    intent.tool_call_id.as_str(),
                                )
                            || intent.provider_id.trim().is_empty()
                            || intent
                                .tool_contract_digest
                                .strip_prefix("sha256:")
                                .is_none_or(|digest| {
                                    digest.len() != 64
                                        || !digest.bytes().all(|byte| {
                                            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                                        })
                                })
                            || intent.args_digest
                                != sha256_digest(intent.effective_args_json.as_bytes())
                            || event.event_id
                                != tool_execution_intent_event_id(
                                    session_id,
                                    turn_id,
                                    intent.tool_call_id.as_str(),
                                )
                        {
                            return Err("tool execution intent identity mismatch".to_string());
                        }
                        if facts
                            .intents
                            .insert(intent.tool_call_id.clone(), intent)
                            .is_some()
                        {
                            return Err("duplicate tool execution intent".to_string());
                        }
                    }
                    TOOL_EXECUTION_RECEIPT_SCHEMA_V1 => {
                        let receipt = serde_json::from_str::<ToolExecutionReceiptV1>(
                            event.payload_json.as_str(),
                        )
                        .map_err(|error| {
                            format!("decode tool execution receipt failed: {error}")
                        })?;
                        let result = receipt.decode_result()?;
                        if receipt.schema != TOOL_EXECUTION_RECEIPT_SCHEMA_V1
                            || receipt.session_id != session_id
                            || receipt.turn_id != turn_id
                            || result.tool_call_id != receipt.tool_call_id
                            || result.tool_name != receipt.source_tool_name
                            || event.event_id
                                != tool_execution_receipt_event_id(
                                    session_id,
                                    turn_id,
                                    receipt.tool_call_id.as_str(),
                                )
                        {
                            return Err("tool execution receipt identity mismatch".to_string());
                        }
                        if facts
                            .receipts
                            .insert(receipt.tool_call_id.clone(), receipt)
                            .is_some()
                        {
                            return Err("duplicate tool execution receipt".to_string());
                        }
                    }
                    POST_TOOL_HOOK_INTENT_SCHEMA_V1 => {
                        let intent = serde_json::from_str::<PostToolHookIntentV1>(
                            event.payload_json.as_str(),
                        )
                        .map_err(|error| format!("decode post-tool hook intent failed: {error}"))?;
                        if intent.schema != POST_TOOL_HOOK_INTENT_SCHEMA_V1
                            || intent.session_id != session_id
                            || intent.turn_id != turn_id
                            || event.event_id
                                != post_tool_hook_intent_event_id(
                                    session_id,
                                    turn_id,
                                    intent.tool_call_id.as_str(),
                                )
                        {
                            return Err("post-tool hook intent identity mismatch".to_string());
                        }
                        if facts
                            .post_hook_intents
                            .insert(intent.tool_call_id.clone(), intent)
                            .is_some()
                        {
                            return Err("duplicate post-tool hook intent".to_string());
                        }
                    }
                    POST_TOOL_HOOK_RECEIPT_SCHEMA_V1 => {
                        let receipt = serde_json::from_str::<PostToolHookReceiptV1>(
                            event.payload_json.as_str(),
                        )
                        .map_err(|error| {
                            format!("decode post-tool hook receipt failed: {error}")
                        })?;
                        if receipt.schema != POST_TOOL_HOOK_RECEIPT_SCHEMA_V1
                            || receipt.session_id != session_id
                            || receipt.turn_id != turn_id
                            || event.event_id
                                != post_tool_hook_receipt_event_id(
                                    session_id,
                                    turn_id,
                                    receipt.tool_call_id.as_str(),
                                )
                        {
                            return Err("post-tool hook receipt identity mismatch".to_string());
                        }
                        if facts
                            .post_hook_receipts
                            .insert(receipt.tool_call_id.clone(), receipt)
                            .is_some()
                        {
                            return Err("duplicate post-tool hook receipt".to_string());
                        }
                    }
                    _ => unreachable!("filtered tool execution fact type"),
                }
            }
            if count < PAGE_SIZE {
                break;
            }
            offset = offset.saturating_add(PAGE_SIZE);
        }
        for (tool_call_id, receipt) in &facts.receipts {
            let intent = facts.intents.get(tool_call_id).ok_or_else(|| {
                format!("tool execution receipt missing intent: callId={tool_call_id}")
            })?;
            if receipt.source_tool_name != intent.source_tool_name
                || receipt.args_digest != intent.args_digest
            {
                return Err(format!(
                    "tool execution receipt intent mismatch: callId={tool_call_id}"
                ));
            }
        }
        for (tool_call_id, receipt) in &facts.post_hook_receipts {
            let intent = facts.post_hook_intents.get(tool_call_id).ok_or_else(|| {
                format!("post-tool hook receipt missing intent: callId={tool_call_id}")
            })?;
            if receipt.result_digest != intent.result_digest {
                return Err(format!(
                    "post-tool hook receipt intent mismatch: callId={tool_call_id}"
                ));
            }
        }
        Ok(facts)
    }

    pub(super) fn recover_interrupted_tool_execution_result(
        &self,
        session_id: &str,
        call: &crate::runtime::contracts::ToolCall,
    ) -> Result<Option<(ToolExecutionIntentV1, ToolCallEnvelope, ToolExecutionResult)>, String>
    {
        const PAGE_SIZE: usize = 256;
        let mut source_turn_id = None;
        let mut offset = 0usize;
        loop {
            let events = self
                .runtime_store
                .list_events(session_id, PAGE_SIZE, offset)
                .map_err(|error| error.to_string())?;
            let count = events.len();
            for event in events.into_iter().filter(|event| {
                matches!(
                    event.event_type.as_str(),
                    TOOL_EXECUTION_INTENT_SCHEMA_V1 | TOOL_EXECUTION_RECEIPT_SCHEMA_V1
                )
            }) {
                let (tool_call_id, turn_id) = match event.event_type.as_str() {
                    TOOL_EXECUTION_INTENT_SCHEMA_V1 => {
                        let intent = serde_json::from_str::<ToolExecutionIntentV1>(
                            event.payload_json.as_str(),
                        )
                        .map_err(|error| format!("decode tool execution intent failed: {error}"))?;
                        (intent.tool_call_id, intent.turn_id)
                    }
                    TOOL_EXECUTION_RECEIPT_SCHEMA_V1 => {
                        let receipt = serde_json::from_str::<ToolExecutionReceiptV1>(
                            event.payload_json.as_str(),
                        )
                        .map_err(|error| {
                            format!("decode tool execution receipt failed: {error}")
                        })?;
                        (receipt.tool_call_id, receipt.turn_id)
                    }
                    _ => unreachable!("filtered tool execution fact type"),
                };
                if tool_call_id != call.tool_call_id {
                    continue;
                }
                if source_turn_id
                    .replace(turn_id.clone())
                    .is_some_and(|existing| existing != turn_id)
                {
                    return Err(format!(
                        "tool execution intent call identity reused across turns: callId={}",
                        call.tool_call_id
                    ));
                }
            }
            if count < PAGE_SIZE {
                break;
            }
            offset = offset.saturating_add(PAGE_SIZE);
        }

        let Some(source_turn_id) = source_turn_id else {
            return Ok(None);
        };
        let mut facts = Self::load_tool_execution_facts(
            &self.runtime_store,
            session_id,
            source_turn_id.as_str(),
        )?;
        let intent = facts
            .intents
            .get(call.tool_call_id.as_str())
            .cloned()
            .ok_or_else(|| {
                format!(
                    "tool execution intent missing after lookup: callId={}",
                    call.tool_call_id
                )
            })?;
        let source_tool_name =
            canonicalize_tool_name(call.tool_name.as_str()).unwrap_or(call.tool_name.as_str());
        if intent.source_tool_name != source_tool_name
            || intent.model_args_digest != sha256_digest(call.args_json.as_bytes())
        {
            return Err(format!(
                "tool execution intent does not match open tool call: callId={}",
                call.tool_call_id
            ));
        }
        let model_call = ToolCallEnvelope {
            id: call.tool_call_id.clone(),
            name: intent.source_tool_name.clone(),
            args_json: intent.effective_args_json.clone(),
        };
        if let Some(receipt) = facts.receipts.get(call.tool_call_id.as_str()) {
            return receipt
                .decode_result()
                .map(|result| Some((intent, model_call, result)));
        }
        let result = indeterminate_tool_execution_result(&intent);
        let result = self.persist_tool_execution_receipt(
            &mut facts,
            session_id,
            source_turn_id.as_str(),
            &model_call,
            &model_call,
            &[],
            false,
            result,
            None,
        )?;
        Ok(Some((intent, model_call, result)))
    }

    pub fn recover_incomplete_session_tool_call(
        runtime_store: &S,
        session_id: &str,
        turn_id: &str,
        agent_run_id: &str,
        call: &ToolCallEnvelope,
        recorded_at_ms: i64,
    ) -> Result<ToolExecutionResult, String> {
        let facts = Self::load_tool_execution_facts(runtime_store, session_id, turn_id)?;
        let Some(intent) = facts.intents.get(call.id.as_str()).cloned() else {
            return Ok(unstarted_session_tool_execution_result(
                call,
                recorded_at_ms,
            ));
        };
        if intent.session_id != session_id
            || intent.turn_id != turn_id
            || intent.tool_call_id != call.id
            || intent.source_tool_name != call.name
            || intent.session_tool_call_event_id
                != crate::runtime::canonical_tool_call_event_id(
                    session_id,
                    turn_id,
                    call.id.as_str(),
                )
            || intent
                .agent_run_identity
                .as_ref()
                .map(|identity| identity.agent_run_id.as_str())
                != Some(agent_run_id)
        {
            return Err(format!(
                "incomplete Session ToolCall identity mismatch: callId={}",
                call.id
            ));
        }
        let effective_call = ToolCallEnvelope {
            id: intent.tool_call_id.clone(),
            name: intent.source_tool_name.clone(),
            args_json: intent.effective_args_json.clone(),
        };
        if let Some(receipt) = facts.receipts.get(call.id.as_str()) {
            return receipt.decode_result();
        }
        let result = indeterminate_tool_execution_result(&intent);
        let result_json = serde_json::to_string(&result)
            .map_err(|error| format!("encode tool result for receipt failed: {error}"))?;
        let receipt = ToolExecutionReceiptV1 {
            schema: TOOL_EXECUTION_RECEIPT_SCHEMA_V1.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            tool_call_id: result.tool_call_id.clone(),
            source_tool_name: intent.source_tool_name,
            args_digest: intent.args_digest,
            effective_args_json: effective_call.args_json,
            pre_hook_contexts: Vec::new(),
            run_post_hook: false,
            result_json,
        };
        runtime_store
            .append_event_idempotent(internal_tool_fact_event(
                tool_execution_receipt_event_id(session_id, turn_id, receipt.tool_call_id.as_str()),
                session_id,
                turn_id,
                TOOL_EXECUTION_RECEIPT_SCHEMA_V1,
                result.completed_at_ms,
                &receipt,
            )?)
            .map_err(|error| error.to_string())?;
        Ok(result)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "tool intent boundary keeps frozen contract fields explicit"
    )]
    fn ensure_tool_execution_intent(
        &self,
        facts: &mut ToolExecutionFactIndex,
        session_id: &str,
        turn_id: &str,
        model_call: &ToolCallEnvelope,
        effective_call: &ToolCallEnvelope,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        provider_id: &str,
        tool_contract_digest: &str,
        session_tool_call_event_id: &str,
    ) -> Result<(ToolExecutionIntentV1, bool), String> {
        let source_tool_name = canonicalize_tool_name(effective_call.name.as_str())
            .unwrap_or(effective_call.name.as_str())
            .to_string();
        let model_args_digest = sha256_digest(model_call.args_json.as_bytes());
        let args_digest = sha256_digest(effective_call.args_json.as_bytes());
        if let Some(existing) = facts.intents.get(effective_call.id.as_str()) {
            if existing.source_tool_name != source_tool_name
                || existing.session_tool_call_event_id != session_tool_call_event_id
                || existing.provider_id != provider_id
                || existing.tool_contract_digest != tool_contract_digest
                || existing.model_args_digest != model_args_digest
                || existing.args_digest != args_digest
                || existing.session_id != session_id
                || existing.turn_id != turn_id
                || existing.agent_run_identity.as_ref() != agent_run_identity
            {
                return Err(format!(
                    "tool execution intent idempotency conflict: callId={}",
                    effective_call.id
                ));
            }
            return Ok((existing.clone(), false));
        }
        let intent = ToolExecutionIntentV1 {
            schema: TOOL_EXECUTION_INTENT_SCHEMA_V1.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            agent_run_identity: agent_run_identity.cloned(),
            tool_call_id: effective_call.id.clone(),
            source_tool_name,
            session_tool_call_event_id: session_tool_call_event_id.to_string(),
            provider_id: provider_id.to_string(),
            tool_contract_digest: tool_contract_digest.to_string(),
            model_args_digest,
            args_digest,
            effective_args_json: effective_call.args_json.clone(),
            recorded_at_ms: now_ms(),
        };
        self.runtime_store
            .append_event_idempotent(internal_tool_fact_event(
                tool_execution_intent_event_id(session_id, turn_id, effective_call.id.as_str()),
                session_id,
                turn_id,
                TOOL_EXECUTION_INTENT_SCHEMA_V1,
                intent.recorded_at_ms,
                &intent,
            )?)
            .map_err(|error| error.to_string())?;
        facts
            .intents
            .insert(effective_call.id.clone(), intent.clone());
        Ok((intent, true))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "tool receipt boundary keeps result and safe point explicit"
    )]
    fn persist_tool_execution_receipt(
        &self,
        facts: &mut ToolExecutionFactIndex,
        session_id: &str,
        turn_id: &str,
        _model_call: &ToolCallEnvelope,
        effective_call: &ToolCallEnvelope,
        pre_hook_contexts: &[String],
        run_post_hook: bool,
        result: ToolExecutionResult,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<ToolExecutionResult, String> {
        let intent = facts
            .intents
            .get(result.tool_call_id.as_str())
            .ok_or_else(|| {
                format!(
                    "tool execution receipt missing intent: callId={}",
                    result.tool_call_id
                )
            })?;
        if result.tool_call_id != effective_call.id
            || result.tool_name != effective_call.name
            || intent.tool_call_id != effective_call.id
            || intent.source_tool_name != effective_call.name
        {
            return Err(format!(
                "tool execution receipt identity mismatch: callId={}",
                result.tool_call_id
            ));
        }
        let result_json = serde_json::to_string(&result)
            .map_err(|error| format!("encode tool result for receipt failed: {error}"))?;
        if let Some(existing) = facts.receipts.get(result.tool_call_id.as_str()) {
            if result_json != existing.result_json
                || existing.effective_args_json != effective_call.args_json
                || existing.pre_hook_contexts != pre_hook_contexts
                || existing.run_post_hook != run_post_hook
            {
                return Err(format!(
                    "tool execution receipt idempotency conflict: callId={}",
                    result.tool_call_id
                ));
            }
            let result = existing.decode_result()?;
            notify_tool_safe_point(tool_safe_point, intent, effective_call, &result)?;
            return Ok(result);
        }
        let completed_at_ms = result.completed_at_ms;
        let receipt = ToolExecutionReceiptV1 {
            schema: TOOL_EXECUTION_RECEIPT_SCHEMA_V1.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            tool_call_id: result.tool_call_id.clone(),
            source_tool_name: intent.source_tool_name.clone(),
            args_digest: intent.args_digest.clone(),
            effective_args_json: effective_call.args_json.clone(),
            pre_hook_contexts: pre_hook_contexts.to_vec(),
            run_post_hook,
            result_json,
        };
        self.runtime_store
            .append_event_idempotent(internal_tool_fact_event(
                tool_execution_receipt_event_id(session_id, turn_id, receipt.tool_call_id.as_str()),
                session_id,
                turn_id,
                TOOL_EXECUTION_RECEIPT_SCHEMA_V1,
                completed_at_ms,
                &receipt,
            )?)
            .map_err(|error| error.to_string())?;
        facts.receipts.insert(receipt.tool_call_id.clone(), receipt);
        notify_tool_safe_point(tool_safe_point, intent, effective_call, &result)?;
        Ok(result)
    }

    fn run_post_tool_use_lifecycle_hook_exactly_once(
        &self,
        facts: &mut ToolExecutionFactIndex,
        session_id: &str,
        turn_id: &str,
        call: &ToolCallEnvelope,
        report: &ToolExecutionResult,
    ) -> Result<Vec<String>, String> {
        let result_json = serde_json::to_vec(report)
            .map_err(|error| format!("serialize tool result for hook receipt failed: {error}"))?;
        let result_digest = sha256_digest(result_json.as_slice());
        if let Some(receipt) = facts.post_hook_receipts.get(call.id.as_str()) {
            if receipt.result_digest != result_digest {
                return Err(format!(
                    "post-tool hook receipt idempotency conflict: callId={}",
                    call.id
                ));
            }
            return Ok(receipt.contexts.clone());
        }
        if let Some(intent) = facts.post_hook_intents.get(call.id.as_str()) {
            if intent.result_digest != result_digest {
                return Err(format!(
                    "post-tool hook intent idempotency conflict: callId={}",
                    call.id
                ));
            }
            let contexts = vec![format!(
                "PostToolUse hook outcome for call {} is indeterminate after runtime recovery; it was not repeated because the hook may have side effects.",
                call.id
            )];
            let receipt = PostToolHookReceiptV1 {
                schema: POST_TOOL_HOOK_RECEIPT_SCHEMA_V1.to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.to_string(),
                tool_call_id: call.id.clone(),
                result_digest,
                contexts: contexts.clone(),
            };
            self.runtime_store
                .append_event_idempotent(internal_tool_fact_event(
                    post_tool_hook_receipt_event_id(session_id, turn_id, call.id.as_str()),
                    session_id,
                    turn_id,
                    POST_TOOL_HOOK_RECEIPT_SCHEMA_V1,
                    now_ms(),
                    &receipt,
                )?)
                .map_err(|error| error.to_string())?;
            facts.post_hook_receipts.insert(call.id.clone(), receipt);
            return Ok(contexts);
        }
        let intent = PostToolHookIntentV1 {
            schema: POST_TOOL_HOOK_INTENT_SCHEMA_V1.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            tool_call_id: call.id.clone(),
            result_digest: result_digest.clone(),
            recorded_at_ms: now_ms(),
        };
        self.runtime_store
            .append_event_idempotent(internal_tool_fact_event(
                post_tool_hook_intent_event_id(session_id, turn_id, call.id.as_str()),
                session_id,
                turn_id,
                POST_TOOL_HOOK_INTENT_SCHEMA_V1,
                intent.recorded_at_ms,
                &intent,
            )?)
            .map_err(|error| error.to_string())?;
        facts.post_hook_intents.insert(call.id.clone(), intent);
        let contexts = self.run_post_tool_use_lifecycle_hook(session_id, call, report)?;
        let receipt = PostToolHookReceiptV1 {
            schema: POST_TOOL_HOOK_RECEIPT_SCHEMA_V1.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            tool_call_id: call.id.clone(),
            result_digest,
            contexts: contexts.clone(),
        };
        self.runtime_store
            .append_event_idempotent(internal_tool_fact_event(
                post_tool_hook_receipt_event_id(session_id, turn_id, call.id.as_str()),
                session_id,
                turn_id,
                POST_TOOL_HOOK_RECEIPT_SCHEMA_V1,
                now_ms(),
                &receipt,
            )?)
            .map_err(|error| error.to_string())?;
        facts.post_hook_receipts.insert(call.id.clone(), receipt);
        Ok(contexts)
    }

    pub(super) fn run_waited_post_tool_use_hooks_exactly_once(
        &self,
        session_id: &str,
        turn_id: &str,
        calls: &[ToolCallEnvelope],
        results: &[ToolExecutionResult],
        waits: &[RuntimeJobWaitV1],
    ) -> Result<Vec<String>, String> {
        let calls_by_id = calls
            .iter()
            .map(|call| (call.id.as_str(), call))
            .collect::<HashMap<_, _>>();
        let results_by_id = results
            .iter()
            .map(|result| (result.tool_call_id.as_str(), result))
            .collect::<HashMap<_, _>>();
        if calls_by_id.len() != calls.len() || results_by_id.len() != results.len() {
            return Err("runtime wait post-tool hook contains duplicate call identity".to_string());
        }

        let mut facts = Self::load_tool_execution_facts(&self.runtime_store, session_id, turn_id)?;
        let mut contexts = Vec::new();
        for wait in waits {
            let call = calls_by_id.get(wait.tool_call_id.as_str()).ok_or_else(|| {
                format!(
                    "runtime wait post-tool hook missing call: callId={}",
                    wait.tool_call_id
                )
            })?;
            let result = results_by_id
                .get(wait.tool_call_id.as_str())
                .ok_or_else(|| {
                    format!(
                        "runtime wait post-tool hook missing result: callId={}",
                        wait.tool_call_id
                    )
                })?;
            let receipt = facts
                .receipts
                .get(wait.tool_call_id.as_str())
                .ok_or_else(|| {
                    format!(
                        "runtime wait post-tool hook missing execution receipt: callId={}",
                        wait.tool_call_id
                    )
                })?;
            if call.name != wait.source_tool_name
                || result.tool_name != wait.source_tool_name
                || receipt.source_tool_name != wait.source_tool_name
                || !receipt.run_post_hook
            {
                return Err(format!(
                    "runtime wait post-tool hook identity mismatch: callId={}",
                    wait.tool_call_id
                ));
            }
            let effective_call = ToolCallEnvelope {
                id: wait.tool_call_id.clone(),
                name: receipt.source_tool_name.clone(),
                args_json: receipt.effective_args_json.clone(),
            };
            contexts.extend(self.run_post_tool_use_lifecycle_hook_exactly_once(
                &mut facts,
                session_id,
                turn_id,
                &effective_call,
                result,
            )?);
        }
        Ok(contexts)
    }

    fn prepare_tool_call_with_lifecycle_hooks(
        &self,
        session_id: &str,
        call: ToolCallEnvelope,
        _session: &SessionStateSnapshot,
    ) -> Result<PreparedToolCall, String> {
        let pre_tool = self.run_pre_tool_use_hook(
            session_id,
            call.name.as_str(),
            parse_lifecycle_hook_tool_input(call.args_json.as_str()),
        )?;
        let mut contexts = lifecycle_hook_context_texts(&pre_tool);
        if pre_tool.blocked {
            let reason = pre_tool
                .block_reason
                .unwrap_or_else(|| "blocked by lifecycle hook".to_string());
            return Ok((
                None,
                contexts,
                Some(build_lifecycle_hook_tool_result(
                    call,
                    "blocked",
                    "pre_tool_use_blocked",
                    reason.as_str(),
                )),
            ));
        }

        let mut call = call;
        if let Some(updated_input) = pre_tool.updated_input {
            call.args_json = serde_json::to_string(&updated_input).map_err(|error| {
                format!("serialize lifecycle hook updatedInput failed: {error}")
            })?;
        }

        let permission = self
            .evaluate_tool_permission_decision(call.name.as_str(), Some(call.args_json.as_str()));
        let permission_hook =
            self.run_permission_request_hook(session_id, call.name.as_str(), &permission)?;
        contexts.extend(lifecycle_hook_context_texts(&permission_hook));
        let denied_by_lifecycle_hook = matches!(
            permission_hook.permission_decision,
            Some(LifecycleHookPermissionDecisionV1::Deny)
        );
        let permission =
            compose_permission_decision_with_hook(permission, permission_hook.permission_decision);
        if !permission.allowed {
            let reason = if denied_by_lifecycle_hook {
                "lifecycle_hook_denied"
            } else {
                "permission_blocked"
            };
            return Ok((
                None,
                contexts,
                Some(build_permission_tool_result(
                    call,
                    permission,
                    "blocked",
                    reason,
                    "permission_blocked",
                )),
            ));
        }
        Ok((Some(call), contexts, None))
    }

    fn run_post_tool_use_lifecycle_hook(
        &self,
        session_id: &str,
        call: &ToolCallEnvelope,
        report: &ToolExecutionResult,
    ) -> Result<Vec<String>, String> {
        let outcome = self.run_post_tool_use_hook(
            session_id,
            call.name.as_str(),
            lifecycle_hook_post_tool_payload(call, report),
        )?;
        if outcome.blocked {
            return Err(outcome
                .block_reason
                .unwrap_or_else(|| "PostToolUse lifecycle hook failed".to_string()));
        }
        Ok(lifecycle_hook_context_texts(&outcome))
    }

    #[cfg(test)]
    pub(super) async fn execute_tool_calls_async(
        &self,
        session_id: &str,
        turn_id: &str,
        session: &SessionStateSnapshot,
        generate_result: GenerateResult,
        stream_sink: Option<&mut (dyn FnMut(TurnUpdate) + Send + '_)>,
    ) -> Result<ToolExecutionBatch, String> {
        self.execute_tool_calls_with_safe_point_async(
            session_id,
            turn_id,
            session,
            generate_result,
            None,
            stream_sink,
            None,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "tool execution orchestration keeps runtime sinks explicit"
    )]
    pub(super) async fn execute_tool_calls_with_safe_point_async(
        &self,
        session_id: &str,
        turn_id: &str,
        session: &SessionStateSnapshot,
        generate_result: GenerateResult,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        mut stream_sink: Option<&mut (dyn FnMut(TurnUpdate) + Send + '_)>,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<ToolExecutionBatch, String> {
        let mut execution_facts =
            Self::load_tool_execution_facts(&self.runtime_store, session_id, turn_id)?;
        let mut indexed_reports: Vec<(usize, ToolExecutionResult)> = vec![];
        let mut local_tool_requests: Vec<(usize, ToolInvocationRequest)> = vec![];
        let mut recovery_policy_trace_json = vec![];
        let mut tool_progress_events = vec![];
        let mut lifecycle_hook_contexts: Vec<String> = vec![];
        let mut lifecycle_hook_calls_by_id: HashMap<String, ToolCallEnvelope> = HashMap::new();
        let mut model_calls_by_id: HashMap<String, ToolCallEnvelope> = HashMap::new();
        let mut pre_hook_contexts_by_id: HashMap<String, Vec<String>> = HashMap::new();
        let mut transition_reason = "model_response_complete_before_tool_execution".to_string();
        let mut expected_call_ids = HashSet::new();
        for call in &generate_result.tool_calls {
            if !expected_call_ids.insert(call.id.clone()) {
                return Err(format!(
                    "tool_batch_duplicate_call_id: turn {turn_id} contains duplicate callId {}",
                    call.id
                ));
            }
        }

        for (index, call) in generate_result.tool_calls.into_iter().enumerate() {
            if !self.tool_is_projected_for_execution(session, call.name.as_str())? {
                return Err(format!(
                    "tool call is not permitted by the projected tool policy: {}",
                    call.name
                ));
            }
            let original_call = call.clone();
            model_calls_by_id.insert(original_call.id.clone(), original_call.clone());
            if let Some(intent) = execution_facts.intents.get(call.id.as_str()).cloned() {
                let contract = self
                    .tools_port
                    .tool_contract(intent.source_tool_name.as_str())?;
                let provider_id = contract
                    .provider_id
                    .as_deref()
                    .ok_or_else(|| "tool contract providerId is required".to_string())?;
                let tool_contract_digest = contract.contract_digest()?;
                if intent.model_args_digest != sha256_digest(call.args_json.as_bytes())
                    || intent.tool_call_id != call.id
                    || intent.source_tool_name != call.name
                    || intent.provider_id != provider_id
                    || intent.tool_contract_digest != tool_contract_digest
                    || intent.agent_run_identity.as_ref() != agent_run_identity
                {
                    return Err(format!(
                        "tool execution intent idempotency conflict: callId={}",
                        call.id
                    ));
                }
                let effective_call = ToolCallEnvelope {
                    id: intent.tool_call_id.clone(),
                    name: intent.source_tool_name.clone(),
                    args_json: intent.effective_args_json.clone(),
                };
                commit_session_tool_call(
                    tool_safe_point,
                    session_id,
                    turn_id,
                    intent.agent_run_identity.as_ref(),
                    &effective_call,
                    provider_id,
                    tool_contract_digest.as_str(),
                    intent.recorded_at_ms,
                )?;
                if let Some(receipt) = execution_facts.receipts.get(call.id.as_str()) {
                    let result = receipt.decode_result()?;
                    lifecycle_hook_contexts.extend(receipt.pre_hook_contexts.clone());
                    pre_hook_contexts_by_id
                        .insert(call.id.clone(), receipt.pre_hook_contexts.clone());
                    notify_tool_safe_point(tool_safe_point, &intent, &effective_call, &result)?;
                    if receipt.run_post_hook {
                        lifecycle_hook_calls_by_id.insert(call.id.clone(), effective_call);
                    }
                    indexed_reports.push((index, result));
                    continue;
                }
                let report = indeterminate_tool_execution_result(&intent);
                let report = self.persist_tool_execution_receipt(
                    &mut execution_facts,
                    session_id,
                    turn_id,
                    &original_call,
                    &effective_call,
                    &[],
                    true,
                    report,
                    tool_safe_point,
                )?;
                lifecycle_hook_calls_by_id.insert(call.id.clone(), effective_call);
                indexed_reports.push((index, report));
                continue;
            }
            let (prepared_call, contexts, hook_report) =
                self.prepare_tool_call_with_lifecycle_hooks(session_id, call, session)?;
            lifecycle_hook_contexts.extend(contexts.clone());
            let effective_call = prepared_call.as_ref().unwrap_or(&original_call);
            let contract = self
                .tools_port
                .tool_contract(effective_call.name.as_str())?;
            let provider_id = contract
                .provider_id
                .as_deref()
                .ok_or_else(|| "tool contract providerId is required".to_string())?;
            let tool_contract_digest = contract.contract_digest()?;
            let session_tool_call_event_id = crate::runtime::canonical_tool_call_event_id(
                session_id,
                turn_id,
                effective_call.id.as_str(),
            );
            let (intent, _) = self.ensure_tool_execution_intent(
                &mut execution_facts,
                session_id,
                turn_id,
                &original_call,
                effective_call,
                agent_run_identity,
                provider_id,
                tool_contract_digest.as_str(),
                session_tool_call_event_id.as_str(),
            )?;
            commit_session_tool_call(
                tool_safe_point,
                session_id,
                turn_id,
                intent.agent_run_identity.as_ref(),
                effective_call,
                provider_id,
                tool_contract_digest.as_str(),
                intent.recorded_at_ms,
            )?;
            if let Some(report) = hook_report {
                let report = self.persist_tool_execution_receipt(
                    &mut execution_facts,
                    session_id,
                    turn_id,
                    &original_call,
                    effective_call,
                    contexts.as_slice(),
                    false,
                    report,
                    tool_safe_point,
                )?;
                pre_hook_contexts_by_id.insert(report.tool_call_id.clone(), contexts);
                indexed_reports.push((index, report));
                continue;
            }
            let Some(call) = prepared_call else {
                continue;
            };
            pre_hook_contexts_by_id.insert(call.id.clone(), contexts);
            lifecycle_hook_calls_by_id.insert(call.id.clone(), call.clone());
            let has_dynamic_contract = self
                .tools_port
                .dynamic_tool_registry()
                .find_contract(call.name.as_str())
                .is_some();

            if call.name == "agent" && !has_dynamic_contract {
                let mut report =
                    self.execute_agent_tool_call(session_id, turn_id, agent_run_identity, &call);
                report.parallel_group = Some("serial".to_string());
                let report = self.persist_tool_execution_receipt(
                    &mut execution_facts,
                    session_id,
                    turn_id,
                    &original_call,
                    &call,
                    pre_hook_contexts_by_id
                        .get(call.id.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    true,
                    report,
                    tool_safe_point,
                )?;
                collect_recovery_trace(&mut recovery_policy_trace_json, &report, "tools");
                indexed_reports.push((index, report));
                continue;
            }

            if is_task_runtime_tool_name(call.name.as_str()) && !has_dynamic_contract {
                let mut report = self.execute_task_runtime_tool_call(session_id, &call);
                report.parallel_group = Some("serial".to_string());
                report.transition_reason = Some("serial_task_runtime_exec".to_string());
                let report = self.persist_tool_execution_receipt(
                    &mut execution_facts,
                    session_id,
                    turn_id,
                    &original_call,
                    &call,
                    pre_hook_contexts_by_id
                        .get(call.id.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    true,
                    report,
                    tool_safe_point,
                )?;
                collect_recovery_trace(&mut recovery_policy_trace_json, &report, "tools");
                indexed_reports.push((index, report));
                continue;
            }

            let request = ToolInvocationRequest {
                tool_call_id: call.id,
                tool_name: canonicalize_tool_name(call.name.as_str())
                    .unwrap_or(call.name.as_str())
                    .to_string(),
                args_json: call.args_json,
            };
            local_tool_requests.push((index, request));
        }

        if !local_tool_requests.is_empty() {
            let executor = self.build_tool_batch_executor_async();
            let mut progress_sink = |event| {
                let progress_event =
                    build_runtime_event_tool_progress_event(session_id, turn_id, &event);
                if let Some(sink) = stream_sink.as_deref_mut() {
                    sink(TurnUpdate::RuntimeEvent {
                        event: progress_event.clone(),
                    });
                }
                tool_progress_events.push(progress_event);
            };
            let mut persist_result = |_index: usize, report: ToolExecutionResult| {
                let call = lifecycle_hook_calls_by_id
                    .get(report.tool_call_id.as_str())
                    .ok_or_else(|| {
                        format!(
                            "tool execution result missing effective call: callId={}",
                            report.tool_call_id
                        )
                    })?;
                let model_call = model_calls_by_id
                    .get(report.tool_call_id.as_str())
                    .ok_or_else(|| {
                        format!(
                            "tool execution result missing model call: callId={}",
                            report.tool_call_id
                        )
                    })?;
                self.persist_tool_execution_receipt(
                    &mut execution_facts,
                    session_id,
                    turn_id,
                    model_call,
                    call,
                    pre_hook_contexts_by_id
                        .get(report.tool_call_id.as_str())
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    true,
                    report,
                    tool_safe_point,
                )
            };
            let local_execution = executor
                .execute_local_tools_with_result_sink_async(
                    local_tool_requests,
                    Some(&mut progress_sink),
                    Some(&mut persist_result),
                )
                .await?;
            transition_reason = local_execution.transition_reason;
            merge_recovery_traces(
                &mut recovery_policy_trace_json,
                local_execution.recovery_policy_trace_json.as_slice(),
            );
            for (index, report) in local_execution.reports {
                collect_recovery_trace(&mut recovery_policy_trace_json, &report, "tools");
                indexed_reports.push((index, report));
            }
        }

        indexed_reports.sort_by_key(|(index, _)| *index);
        for (_, report) in &indexed_reports {
            if report.tool_name != "agent" || report.status != "ok" {
                continue;
            }
            let parent_task_id = agent_run_identity
                .map(|identity| identity.agent_run_id.as_str())
                .ok_or_else(|| {
                    "Agent result requires the active runtime AgentRun identity".to_string()
                })?;
            if let Some(event) = build_runtime_event_subagent_spawned_from_tool_result(
                session_id,
                turn_id,
                parent_task_id,
                report,
            )? {
                if let Some(sink) = stream_sink.as_deref_mut() {
                    sink(TurnUpdate::RuntimeEvent {
                        event: event.clone(),
                    });
                }
                tool_progress_events.push(event);
            }
        }
        let mut terminal_reports = Vec::new();
        let mut indexed_waits = Vec::new();
        for (index, report) in indexed_reports {
            let pending_poll = if report.status == "ok" {
                extract_dynamic_tool_pending_poll(&report.details).transpose()?
            } else {
                None
            };
            let agent_wait = if report.status == "ok" {
                extract_agent_task_output_wait(&report.details).transpose()?
            } else {
                None
            };
            if pending_poll.is_some() && agent_wait.is_some() {
                return Err(format!(
                    "tool result cannot request provider and Agent waits together: {}",
                    report.tool_call_id
                ));
            }
            if let Some(pending_poll) = pending_poll {
                indexed_waits.push((
                    index,
                    self.schedule_provider_polling_job(
                        session_id,
                        turn_id,
                        agent_run_identity.ok_or_else(|| {
                            "provider_poll_requires_agent_run_identity".to_string()
                        })?,
                        session,
                        &report,
                        &pending_poll,
                    )?,
                ));
            } else if let Some(agent_wait) = agent_wait {
                indexed_waits.push((
                    index,
                    self.agent_task_output_runtime_job_wait(session, &report, &agent_wait)?,
                ));
            } else {
                terminal_reports.push((index, report));
            }
        }
        for (_, report) in &terminal_reports {
            if let Some(call) = lifecycle_hook_calls_by_id.get(report.tool_call_id.as_str()) {
                lifecycle_hook_contexts.extend(
                    self.run_post_tool_use_lifecycle_hook_exactly_once(
                        &mut execution_facts,
                        session_id,
                        turn_id,
                        call,
                        report,
                    )?,
                );
            }
        }
        let mut reports = terminal_reports
            .into_iter()
            .map(|(_, report)| report)
            .collect::<Vec<_>>();
        let runtime_job_waits = indexed_waits
            .into_iter()
            .map(|(_, wait)| wait)
            .collect::<Vec<_>>();
        let mut result_call_ids = HashSet::new();
        for report in &reports {
            if !expected_call_ids.contains(report.tool_call_id.as_str()) {
                return Err(format!(
                    "tool_batch_orphan_result: turn {turn_id} returned unknown callId {}",
                    report.tool_call_id
                ));
            }
            if !result_call_ids.insert(report.tool_call_id.clone()) {
                return Err(format!(
                    "tool_batch_duplicate_result: turn {turn_id} returned callId {} more than once",
                    report.tool_call_id
                ));
            }
        }
        for wait in &runtime_job_waits {
            if !expected_call_ids.contains(wait.tool_call_id.as_str()) {
                return Err(format!(
                    "tool_batch_orphan_runtime_wait: turn {turn_id} returned unknown callId {}",
                    wait.tool_call_id
                ));
            }
            if !result_call_ids.insert(wait.tool_call_id.clone()) {
                return Err(format!(
                    "tool_batch_duplicate_terminal_or_wait: turn {turn_id} returned callId {} more than once",
                    wait.tool_call_id
                ));
            }
        }
        let mut missing_call_ids = expected_call_ids
            .difference(&result_call_ids)
            .cloned()
            .collect::<Vec<_>>();
        missing_call_ids.sort();
        if !missing_call_ids.is_empty() {
            return Err(format!(
                "tool_batch_missing_results: turn {turn_id} has no terminal result for callIds {}",
                missing_call_ids.join(", ")
            ));
        }
        self.persist_external_context_objects_from_tool_results(
            session_id,
            turn_id,
            reports.as_mut_slice(),
        );
        if reports.iter().any(|item| item.status == "error") {
            transition_reason = format!("{transition_reason}_with_errors");
        }

        Ok(ToolExecutionBatch {
            tool_results: reports,
            runtime_job_waits,
            lifecycle_hook_contexts,
            tool_progress_events,
            transition_reason,
            recovery_policy_trace_json,
        })
    }

    pub(super) fn validate_tool_turn_behavior_preflight(
        &self,
        session: &SessionStateSnapshot,
        generate_result: &GenerateResult,
    ) -> Result<(), String> {
        let mut complete_turn_call_count = 0usize;
        for call in &generate_result.tool_calls {
            if !self.tool_is_projected_for_execution(session, call.name.as_str())? {
                return Err(format!(
                    "tool call is not permitted by the projected tool policy: {}",
                    call.name
                ));
            }
            if self.tools_port.tool_turn_behavior(call.name.as_str())?
                == ToolTurnBehavior::CompleteTurnOnSuccess
            {
                complete_turn_call_count = complete_turn_call_count.saturating_add(1);
            }
        }
        if complete_turn_call_count > 0 && generate_result.tool_calls.len() != 1 {
            return Err(
                "complete-turn tool call must be the only tool call in its provider response"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub(super) fn should_complete_turn_after_tool_success(
        &self,
        generate_result: &GenerateResult,
        tool_results: &[ToolExecutionResult],
    ) -> Result<bool, String> {
        let Some(call) = generate_result.tool_calls.first() else {
            return Ok(false);
        };
        if self.tools_port.tool_turn_behavior(call.name.as_str())?
            != ToolTurnBehavior::CompleteTurnOnSuccess
        {
            return Ok(false);
        }
        if generate_result.tool_calls.len() != 1 || tool_results.len() != 1 {
            return Err(
                "complete-turn tool execution must produce exactly one paired result".to_string(),
            );
        }
        let result = &tool_results[0];
        let canonical_tool_name = canonicalize_tool_name(call.name.as_str()).unwrap_or(&call.name);
        if result.tool_call_id != call.id || result.tool_name != canonical_tool_name {
            return Err("complete-turn tool call/result identity mismatch".to_string());
        }
        Ok(result.error.is_none() && result.result_state().is_success())
    }

    pub(super) fn tool_is_projected_for_execution(
        &self,
        session: &SessionStateSnapshot,
        tool_name: &str,
    ) -> Result<bool, String> {
        let canonical = canonicalize_tool_name(tool_name).unwrap_or(tool_name);
        let (definitions, _) = build_generate_tool_projection(
            session,
            self.tools_port.dynamic_tool_registry(),
            self.config.allowed_tools.as_deref(),
            self.tools_port.execution_host_kind(),
        )?;
        Ok(definitions
            .iter()
            .any(|definition| definition.name == canonical))
    }

    pub(super) fn persist_external_context_objects_from_tool_results(
        &self,
        session_id: &str,
        turn_id: &str,
        reports: &mut [ToolExecutionResult],
    ) {
        for report in reports.iter_mut() {
            if report.status != "ok" {
                continue;
            }
            let object = match extract_external_context_object_from_tool_output(&report.details) {
                Some(Ok(object)) => object,
                Some(Err(err)) => {
                    report.details = annotate_external_context_store_status(
                        &report.details,
                        json!({
                            "persisted": false,
                            "linked": false,
                            "error": err,
                        }),
                    )
                    .expect("tool result details must accept external context status");
                    continue;
                }
                None => continue,
            };
            let object_id = object.object_id.clone();
            let source_provider_id = object.source_provider_id.clone();
            let source_tool_name = object.source_tool_name.clone();
            let linked_at_ms = report.completed_at_ms.max(object.updated_at_ms);

            let persistence_status = match self
                .runtime_store
                .upsert_external_context_object(object)
                .and_then(|_| {
                    self.runtime_store
                        .link_external_context_object(ExternalContextObjectLink {
                            session_id: session_id.to_string(),
                            turn_id: Some(turn_id.to_string()),
                            tool_call_id: Some(report.tool_call_id.clone()),
                            object_id: object_id.clone(),
                            source_provider_id: source_provider_id.clone(),
                            source_tool_name: source_tool_name.clone(),
                            linked_at_ms,
                        })
                }) {
                Ok(()) => json!({
                    "persisted": true,
                    "linked": true,
                    "objectId": object_id,
                    "sourceProviderId": source_provider_id,
                    "sourceToolName": source_tool_name,
                    "linkedAtMs": linked_at_ms,
                }),
                Err(err) => json!({
                    "persisted": false,
                    "linked": false,
                    "objectId": object_id,
                    "sourceProviderId": source_provider_id,
                    "sourceToolName": source_tool_name,
                    "error": err,
                }),
            };
            report.details =
                annotate_external_context_store_status(&report.details, persistence_status)
                    .expect("tool result details must accept external context status");
        }
        self.persist_tool_evidence_rollup(session_id, turn_id, reports);
    }

    pub(super) fn persist_tool_evidence_rollup(
        &self,
        session_id: &str,
        turn_id: &str,
        reports: &mut [ToolExecutionResult],
    ) {
        let Some(object) = build_tool_evidence_rollup_external_context_object(turn_id, reports)
        else {
            return;
        };
        let object_id = object.object_id.clone();
        let source_provider_id = object.source_provider_id.clone();
        let source_tool_name = object.source_tool_name.clone();
        let linked_at_ms = object.updated_at_ms;
        let status = match self
            .runtime_store
            .upsert_external_context_object(object)
            .and_then(|_| {
                self.runtime_store
                    .link_external_context_object(ExternalContextObjectLink {
                        session_id: session_id.to_string(),
                        turn_id: Some(turn_id.to_string()),
                        tool_call_id: Some("tool_evidence_rollup".to_string()),
                        object_id: object_id.clone(),
                        source_provider_id: source_provider_id.clone(),
                        source_tool_name: source_tool_name.clone(),
                        linked_at_ms,
                    })
            }) {
            Ok(()) => json!({
                "persisted": true,
                "linked": true,
                "objectId": object_id,
                "sourceProviderId": source_provider_id,
                "sourceToolName": source_tool_name,
                "linkedAtMs": linked_at_ms,
            }),
            Err(err) => json!({
                "persisted": false,
                "linked": false,
                "objectId": object_id,
                "sourceProviderId": source_provider_id,
                "sourceToolName": source_tool_name,
                "error": err,
            }),
        };
        for report in reports.iter_mut() {
            report.details = annotate_evidence_rollup_store_status(&report.details, status.clone())
                .expect("tool result details must accept evidence rollup status");
        }
    }

    fn schedule_provider_polling_job(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: &RuntimeAgentRunIdentityV1,
        session: &SessionStateSnapshot,
        report: &ToolExecutionResult,
        pending_poll: &DynamicToolPendingPoll,
    ) -> Result<RuntimeJobWaitV1, String> {
        let requested_job = build_provider_poll_runtime_job(
            session_id,
            turn_id,
            agent_run_identity,
            report,
            pending_poll,
        )?;
        let requested_job_id = requested_job.job_id.clone();
        let scheduled = self
            .runtime_store
            .schedule_runtime_job(ScheduleRuntimeJobRequest { job: requested_job })?;
        if scheduled.job.job_id != requested_job_id {
            return Err(format!(
                "provider_poll_schedule_idempotency_conflict: expected={} actual={}",
                requested_job_id, scheduled.job.job_id
            ));
        }
        if scheduled.job.job_kind != PROVIDER_POLL_RUNTIME_JOB_KIND {
            return Err(format!(
                "provider_poll_schedule_job_kind_mismatch: expected={} actual={}",
                PROVIDER_POLL_RUNTIME_JOB_KIND, scheduled.job.job_kind
            ));
        }
        if scheduled.job.session_id.as_deref() != Some(session_id) {
            return Err(format!(
                "provider_poll_schedule_session_mismatch: jobId={} expected={} actual={:?}",
                scheduled.job.job_id, session_id, scheduled.job.session_id
            ));
        }
        let scheduled_payload =
            parse_provider_poll_payload_ref(scheduled.job.payload_ref.as_deref())?;
        if scheduled_payload.source_agent_run_id != agent_run_identity.agent_run_id
            || scheduled_payload.source_turn_id != turn_id
            || scheduled_payload.source_tool_call_id != report.tool_call_id
        {
            return Err(format!(
                "provider_poll_schedule_source_identity_mismatch: jobId={}",
                scheduled.job.job_id
            ));
        }
        let wait = RuntimeJobWaitV1 {
            tool_call_id: report.tool_call_id.clone(),
            source_tool_name: report.tool_name.clone(),
            tool_definition_digest: self
                .projected_tool_definition_digest(session, report.tool_name.as_str())?,
            job_id: scheduled.job.job_id,
            job_kind: scheduled.job.job_kind,
        };
        wait.validate()?;
        Ok(wait)
    }

    fn agent_task_output_runtime_job_wait(
        &self,
        session: &SessionStateSnapshot,
        report: &ToolExecutionResult,
        output_ref: &AgentTaskOutputRefV1,
    ) -> Result<RuntimeJobWaitV1, String> {
        let job = self
            .runtime_store
            .get_runtime_job(output_ref.runtime_job_id.as_str())?
            .ok_or_else(|| {
                format!(
                    "TaskOutput runtime job not found: {}",
                    output_ref.runtime_job_id
                )
            })?;
        if job.job_kind != SUBAGENT_RUN_JOB_KIND {
            return Err(format!(
                "TaskOutput runtime job kind mismatch: jobId={} expected={} actual={}",
                job.job_id, SUBAGENT_RUN_JOB_KIND, job.job_kind
            ));
        }
        let wait = RuntimeJobWaitV1 {
            tool_call_id: report.tool_call_id.clone(),
            source_tool_name: report.tool_name.clone(),
            tool_definition_digest: self
                .projected_tool_definition_digest(session, report.tool_name.as_str())?,
            job_id: job.job_id,
            job_kind: job.job_kind,
        };
        wait.validate()?;
        Ok(wait)
    }

    pub(super) fn projected_tool_definition_digest(
        &self,
        session: &SessionStateSnapshot,
        tool_name: &str,
    ) -> Result<String, String> {
        let definitions = build_generate_tool_projection(
            session,
            self.tools_port.dynamic_tool_registry(),
            self.config.allowed_tools.as_deref(),
            self.tools_port.execution_host_kind(),
        )?
        .0;
        let definition = definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .ok_or_else(|| format!("runtime_wait_tool_definition_missing: toolName={tool_name}"))?;
        let definition_json = serde_json::to_vec(definition)
            .map_err(|error| format!("serialize runtime wait tool definition failed: {error}"))?;
        Ok(format!("sha256:{:x}", Sha256::digest(definition_json)))
    }

    pub(super) fn build_tool_permission_preview(
        &self,
        _session: &SessionStateSnapshot,
        tool_calls: &[ToolCallEnvelope],
    ) -> HashMap<String, PermissionDecision> {
        let mut preview = HashMap::with_capacity(tool_calls.len());
        for call in tool_calls {
            let decision = self.evaluate_tool_permission_decision(
                call.name.as_str(),
                Some(call.args_json.as_str()),
            );
            preview.insert(call.id.clone(), decision);
        }
        preview
    }

    pub(super) fn evaluate_tool_permission_decision(
        &self,
        tool_name: &str,
        args_json: Option<&str>,
    ) -> PermissionDecision {
        let mut request =
            ToolPermissionRequest::new(tool_name.to_string(), args_json.map(ToString::to_string));
        if canonicalize_tool_name(tool_name) == Some("bash") {
            if let Some(workspace_root) = self.tools_port.cwd() {
                request = request.with_bash_cwd(workspace_root.to_path_buf());
            }
        }
        if self
            .tools_port
            .dynamic_tool_registry()
            .find_contract(tool_name)
            .is_some()
        {
            request = request.with_dynamic_contract();
        }
        evaluate_tool_action(&request)
    }

    pub(super) fn execute_agent_tool_call(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        call: &ToolCallEnvelope,
    ) -> ToolExecutionResult {
        let started_at_ms = now_ms();
        let result =
            self.build_and_schedule_agent_run(session_id, turn_id, agent_run_identity, call);
        match result {
            Ok(output) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: "agent".to_string(),
                status: "ok".to_string(),
                content: output.to_string(),
                details: output,
                facts: Vec::new(),
                error: None,
                started_at_ms,
                completed_at_ms: now_ms(),
                latency_ms: now_ms().saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("agent_tool_job_scheduled".to_string()),
            },
            Err(err) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: "agent".to_string(),
                status: "error".to_string(),
                content: err.clone(),
                details: json!({ "message": err }),
                facts: Vec::new(),
                error: Some(ToolErrorInfo::from_unstructured_error(err)),
                started_at_ms,
                completed_at_ms: now_ms(),
                latency_ms: now_ms().saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("agent_tool_exec_error".to_string()),
            },
        }
    }

    fn build_and_schedule_agent_run(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        call: &ToolCallEnvelope,
    ) -> Result<Value, String> {
        let agent_run_identity = agent_run_identity
            .ok_or_else(|| "Agent requires the active runtime AgentRun identity".to_string())?;
        agent_run_identity.validate()?;
        let args = serde_json::from_str::<AgentToolArgsV1>(call.args_json.as_str())
            .map_err(|err| format!("Agent args_json is invalid: {err}"))?
            .validate()?;
        let parent_tool_contracts = build_generate_tool_contracts(
            self.tools_port.dynamic_tool_registry(),
            self.config.allowed_tools.as_deref(),
            self.tools_port.execution_host_kind(),
        )?;
        let delegated_tool_contracts = parent_tool_contracts
            .iter()
            .filter(|contract| !matches!(contract.name.as_str(), "agent" | "task_output"))
            .map(DelegatedToolContractV1::from_tool_contract)
            .collect::<Result<Vec<_>, String>>()?;
        let allowed_tools = delegated_tool_contracts
            .iter()
            .map(|contract| contract.name.clone())
            .collect::<Vec<_>>();
        let max_summary_chars = args.max_summary_chars();
        let workspace_root = self
            .tools_port
            .cwd()
            .ok_or_else(|| "Agent requires a configured workspace root".to_string())?
            .to_string_lossy()
            .to_string();
        let now = now_ms();
        let run_hash = stable_text_hash(
            format!(
                "{}:{}:{}:{}:{}",
                session_id, turn_id, call.id, args.description, args.prompt
            )
            .as_str(),
        );
        let subagent_id = format!("agent-{run_hash}");
        let child_session_id = format!("session-agent-{run_hash}");
        let child_turn_id = new_turn_id();
        let parent_context = AgentRunContext::root(
            session_id,
            turn_id,
            turn_id,
            agent_run_identity.agent_run_id.clone(),
            "main-agent",
            workspace_root,
            now,
        );
        let work_packet_ref =
            runtime_external_context_keys::subagent_work_packet_ref(run_hash.as_str());
        let job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: session_id.to_string(),
            parent_turn_id: turn_id.to_string(),
            tool_call_id: call.id.clone(),
            subagent_id: subagent_id.clone(),
            work_packet_ref: work_packet_ref.clone(),
            checkpoint_id: None,
            run_at_ms: now,
            created_at_ms: now,
            max_retries: 0,
        });
        let child_context = AgentRunContext::child(
            &parent_context,
            child_session_id.clone(),
            child_turn_id.clone(),
            job.job_id.clone(),
            subagent_id.clone(),
            now,
        );
        let mut packet = SubAgentWorkPacket::new(
            child_context,
            TaskBrief {
                task_id: Some(subagent_id.clone()),
                objective: args.prompt,
                success_criteria: vec!["Return a concise, evidence-backed result.".to_string()],
                constraints: vec!["Do not expose internal runtime ids to the user.".to_string()],
                output_hint: Some(args.description.clone()),
            },
            HotView {
                summary: format!("Parent turn {turn_id}: {}", args.description),
                recent_message_ids: vec![],
                state_kv: JsonMap::new(),
            },
            OutputContract {
                response_mode: "bounded_agent_result".to_string(),
                expected_sections: vec!["summary".to_string()],
                require_artifact_refs: false,
                max_summary_chars: Some(max_summary_chars),
            },
            ContextTransferMode::Borrow,
        );
        packet.allowed_tools = allowed_tools;
        packet.delegated_tool_contracts = delegated_tool_contracts;
        if packet
            .allowed_tools
            .iter()
            .any(|tool| matches!(tool.as_str(), "write" | "edit" | "bash"))
        {
            packet.writable_path_prefixes = vec![self
                .tools_port
                .cwd()
                .expect("workspace root was validated above")
                .to_path_buf()
                .to_string_lossy()
                .to_string()];
        }
        packet.validate_for_agent_runtime()?;

        let result_ref = runtime_external_context_keys::subagent_result_ref(job.job_id.as_str());
        let object = ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
            object_id: work_packet_ref.clone(),
            object_kind: "subagent_work_packet".to_string(),
            source_provider_id: "centaeris.core".to_string(),
            source_tool_name: "agent".to_string(),
            title: args.description.clone(),
            content: serde_json::to_string(&json!({ "workPacket": packet }))
                .map_err(|err| format!("serialize Agent work packet failed: {err}"))?,
            metadata: json!({
                "schema": "agent_tool_work_packet_v1",
                "sessionId": session_id,
                "parentTurnId": turn_id,
                "childTurnId": child_turn_id.clone(),
                "toolCallId": call.id,
                "childSessionId": child_session_id,
                "subagentId": subagent_id,
                "runtimeJobId": job.job_id,
                "resultRef": result_ref,
            }),
            updated_at_ms: now,
        };
        let scheduled = self
            .runtime_store
            .upsert_external_context_and_schedule_job(
                UpsertExternalContextAndScheduleJobRequest { object, job },
            )?;
        if scheduled.job.job_kind != SUBAGENT_RUN_JOB_KIND
            || scheduled.job.session_id.as_deref() != Some(session_id)
            || scheduled.job.payload_ref.as_deref() != Some(work_packet_ref.as_str())
        {
            return Err(format!(
                "Agent scheduled runtime job identity mismatch: {}",
                scheduled.job.job_id
            ));
        }
        Ok(json!({
            "schema": AGENT_TOOL_RESULT_SCHEMA_V1,
            "status": "started",
            "description": args.description,
            "subagentId": subagent_id,
            "runtimeJobId": scheduled.job.job_id,
            "workPacketRef": work_packet_ref,
            "childSessionId": child_session_id,
            "childTurnId": child_turn_id,
            "outputRef": {
                "schema": TASK_OUTPUT_REF_SCHEMA_V1,
                "kind": "agent",
                "runtimeJobId": scheduled.job.job_id,
                "childSessionId": child_session_id,
                "resultRef": result_ref,
            }
        }))
    }

    pub(super) fn execute_task_runtime_tool_call(
        &self,
        session_id: &str,
        call: &ToolCallEnvelope,
    ) -> ToolExecutionResult {
        let started_at_ms = now_ms();
        let tool_name =
            canonical_task_runtime_tool_name(call.name.as_str()).unwrap_or(call.name.as_str());
        let args = match serde_json::from_str::<TaskOutputArgsV1>(call.args_json.as_str()) {
            Ok(args) => args,
            Err(err) => {
                return ToolExecutionResult {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    status: "error".to_string(),
                    content: format!("task runtime tool arguments are invalid JSON: {err}"),
                    details: json!({ "argsError": err.to_string() }),
                    facts: Vec::new(),
                    error: Some(ToolErrorInfo::from_unstructured_error(format!(
                        "task runtime tool args_json is invalid JSON: {err}"
                    ))),
                    started_at_ms,
                    completed_at_ms: now_ms(),
                    latency_ms: now_ms().saturating_sub(started_at_ms),
                    parallel_group: None,
                    transition_reason: Some("task_runtime_tool_exec_error".to_string()),
                };
            }
        };
        let output_ref = AgentTaskOutputRefV1::from(args.output_ref);
        let output: Result<(String, Value), String> = match tool_name {
            "task_output" => self
                .validate_agent_task_output_ref(session_id, &output_ref)
                .map(|()| {
                    (
                        "Waiting for the Agent result.".to_string(),
                        json!({
                            "schema": AGENT_TASK_OUTPUT_WAIT_SCHEMA_V1,
                            "outputRef": output_ref,
                        }),
                    )
                }),
            _ => Err(format!("unsupported task runtime tool: {}", call.name)),
        };

        let completed_at_ms = now_ms();
        match output {
            Ok((content, details)) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: "ok".to_string(),
                content,
                details,
                facts: Vec::new(),
                error: None,
                started_at_ms,
                completed_at_ms,
                latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("task_runtime_tool_exec".to_string()),
            },
            Err(err) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: "error".to_string(),
                content: err.clone(),
                details: json!({ "message": err }),
                facts: Vec::new(),
                error: Some(ToolErrorInfo::from_unstructured_error(err)),
                started_at_ms,
                completed_at_ms,
                latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("task_runtime_tool_exec_error".to_string()),
            },
        }
    }

    fn validate_agent_task_output_ref(
        &self,
        session_id: &str,
        output_ref: &AgentTaskOutputRefV1,
    ) -> Result<(), String> {
        output_ref.validate()?;
        let job = self
            .runtime_store
            .get_runtime_job(output_ref.runtime_job_id.as_str())?
            .ok_or_else(|| {
                format!(
                    "TaskOutput runtime job not found: {}",
                    output_ref.runtime_job_id
                )
            })?;
        if job.job_kind != SUBAGENT_RUN_JOB_KIND {
            return Err(format!(
                "TaskOutput runtime job kind mismatch: jobId={} expected={} actual={}",
                job.job_id, SUBAGENT_RUN_JOB_KIND, job.job_kind
            ));
        }
        if job.session_id.as_deref() != Some(session_id) {
            return Err(format!(
                "TaskOutput runtime job session mismatch: jobId={} expected={} actual={:?}",
                job.job_id, session_id, job.session_id
            ));
        }
        let work_packet_ref = job
            .payload_ref
            .as_deref()
            .ok_or_else(|| format!("TaskOutput runtime job payloadRef missing: {}", job.job_id))?;
        let object = self
            .runtime_store
            .load_external_context_object(work_packet_ref)?
            .ok_or_else(|| format!("TaskOutput work packet missing: {work_packet_ref}"))?;
        if object.object_kind != "subagent_work_packet"
            || object.source_provider_id != "centaeris.core"
            || object.source_tool_name != "agent"
        {
            return Err(format!(
                "TaskOutput work packet source mismatch: {work_packet_ref}"
            ));
        }
        for (field, expected) in [
            ("runtimeJobId", output_ref.runtime_job_id.as_str()),
            ("childSessionId", output_ref.child_session_id.as_str()),
            ("resultRef", output_ref.result_ref.as_str()),
        ] {
            if object.metadata.get(field).and_then(Value::as_str) != Some(expected) {
                return Err(format!(
                    "TaskOutput work packet {field} mismatch: {work_packet_ref}"
                ));
            }
        }
        Ok(())
    }
}

fn extract_agent_task_output_wait(details: &Value) -> Option<Result<AgentTaskOutputRefV1, String>> {
    if details.get("schema").and_then(Value::as_str) != Some(AGENT_TASK_OUTPUT_WAIT_SCHEMA_V1) {
        return None;
    }
    Some(
        details
            .get("outputRef")
            .cloned()
            .ok_or_else(|| "Agent TaskOutput wait is missing outputRef".to_string())
            .and_then(|value| {
                serde_json::from_value::<AgentTaskOutputRefV1>(value)
                    .map_err(|error| format!("decode Agent TaskOutput outputRef failed: {error}"))
            })
            .and_then(|output_ref| {
                output_ref.validate()?;
                Ok(output_ref)
            }),
    )
}
