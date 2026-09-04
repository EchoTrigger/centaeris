use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::execution::ExecutionWorkspaceGeneration;

pub type TimestampMs = i64;
pub type JsonMap = HashMap<String, String>;

static NEXT_TURN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub fn current_unix_epoch_ms_u128() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_millis()
}

pub fn current_timestamp_ms() -> TimestampMs {
    TimestampMs::try_from(current_unix_epoch_ms_u128()).expect("current timestamp overflows i64")
}

pub fn new_turn_id() -> String {
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_nanos();
    let sequence = NEXT_TURN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("turn-{now_ns:x}-{:x}-{sequence:x}", std::process::id())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
    Created,
    Running,
    Waiting,
    Done,
    Error,
    Stopped,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventVisibility {
    User,
    Internal,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProcessState {
    Thinking,
    Searching,
    Reading,
    Executing,
    Reviewing,
    Synthesizing,
    Compressing,
    Recovering,
    Retrying,
    Waiting,
    ProviderWaiting,
    AuthFailed,
    ProviderUnavailable,
    ProviderInterrupted,
    Unknown,
}

impl RuntimeProcessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Searching => "searching",
            Self::Reading => "reading",
            Self::Executing => "executing",
            Self::Reviewing => "reviewing",
            Self::Synthesizing => "synthesizing",
            Self::Compressing => "compressing",
            Self::Recovering => "recovering",
            Self::Retrying => "retrying",
            Self::Waiting => "waiting",
            Self::ProviderWaiting => "provider_waiting",
            Self::AuthFailed => "auth_failed",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderInterrupted => "provider_interrupted",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_provider_error_reason(reason: &str) -> Self {
        match reason.trim() {
            "provider_busy_or_rate_limited" => Self::ProviderWaiting,
            "auth_failed" => Self::AuthFailed,
            "provider_unavailable" => Self::ProviderUnavailable,
            "provider_response_interrupted" => Self::ProviderInterrupted,
            "timeout" | "network" => Self::Retrying,
            _ => Self::Unknown,
        }
    }

    pub fn from_tool_name(tool_name: &str) -> Self {
        let normalized = tool_name.replace([' ', '_', '-'], "").to_lowercase();
        if normalized.contains("glob")
            || normalized.contains("grep")
            || normalized.contains("search")
            || normalized.contains("rg")
        {
            return Self::Searching;
        }
        if normalized.contains("read") || normalized.contains("open") {
            return Self::Reading;
        }
        if normalized.contains("bash")
            || normalized.contains("script")
            || normalized.contains("exec")
            || normalized.contains("write")
            || normalized.contains("edit")
            || normalized.contains("patch")
            || normalized.contains("apply")
        {
            return Self::Executing;
        }
        Self::Reviewing
    }

    pub fn is_watchdog_progress_heartbeat(self) -> bool {
        matches!(self, Self::ProviderWaiting | Self::Retrying)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKindV1 {
    Wait,
    Recovery,
}

impl CheckpointKindV1 {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Recovery => "recovery",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "wait" => Ok(Self::Wait),
            "recovery" => Ok(Self::Recovery),
            other => Err(format!("unsupported_checkpoint_kind:{other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointRecord {
    pub checkpoint_id: String,
    pub kind: CheckpointKindV1,
    pub session_id: String,
    pub turn_id: String,
    pub status: String,
    pub done_reason: Option<String>,
    pub updated_at_ms: TimestampMs,
    pub payload_json: String,
}

pub const RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1: &str = "runtime.recovery_checkpoint.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryWorkspaceSnapshotV1 {
    pub object_ref: Option<String>,
    pub snapshot_sha256: String,
    pub snapshot_size_bytes: u64,
    pub expanded_size_bytes: u64,
    pub file_count: u32,
}

impl RecoveryWorkspaceSnapshotV1 {
    pub fn validate(&self) -> Result<(), String> {
        let empty = self.object_ref.is_none()
            && self.snapshot_sha256.is_empty()
            && self.snapshot_size_bytes == 0
            && self.expanded_size_bytes == 0
            && self.file_count == 0;
        if empty {
            return Ok(());
        }
        validate_required_identity(self.object_ref.as_deref().unwrap_or_default(), "objectRef")?;
        validate_sha256_digest(self.snapshot_sha256.as_str(), "snapshotSha256")?;
        if self.snapshot_size_bytes == 0 || self.file_count == 0 {
            return Err("recovery_workspace_snapshot_non_empty_shape_invalid".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRecoveryCheckpointV1 {
    pub schema: String,
    pub checkpoint_id: String,
    pub session_id: String,
    pub agent_run_id: String,
    pub execution_id: String,
    pub authorization_digest: String,
    pub session_sequence: u64,
    pub model_request_id: String,
    pub workspace_snapshot: RecoveryWorkspaceSnapshotV1,
    pub workspace_generation: ExecutionWorkspaceGeneration,
    pub created_at_ms: TimestampMs,
}

impl RuntimeRecoveryCheckpointV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1 {
            return Err(format!(
                "unsupported_runtime_recovery_checkpoint_schema:{}",
                self.schema
            ));
        }
        for (field, value) in [
            ("checkpointId", self.checkpoint_id.as_str()),
            ("sessionId", self.session_id.as_str()),
            ("agentRunId", self.agent_run_id.as_str()),
            ("executionId", self.execution_id.as_str()),
            ("modelRequestId", self.model_request_id.as_str()),
        ] {
            validate_required_identity(value, field)?;
        }
        validate_sha256_digest(self.authorization_digest.as_str(), "authorizationDigest")?;
        if self.session_sequence == 0 || self.created_at_ms < 0 {
            return Err("runtime_recovery_checkpoint_boundary_invalid".to_string());
        }
        self.workspace_snapshot.validate()?;
        self.workspace_generation.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEvent {
    pub event_id: String,
    pub session_id: String,
    pub task_id: Option<String>,
    pub event_type: String,
    pub at_ms: TimestampMs,
    pub visibility: EventVisibility,
    pub payload_json: String,
}

pub const AGENT_RUN_INTERVENTION_SCHEMA_V1: &str = "agent_run.intervention.v1";
pub const AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1: &str = "agent_run_intervention_changed.v1";
pub const RUNTIME_AWAIT_QUESTION_SCHEMA_V1: &str = "runtime.await_question.v1";
pub const RUNTIME_AWAIT_JOB_SCHEMA_V1: &str = "runtime.await_job.v1";
pub const RUNTIME_WAIT_CHANGED_SCHEMA_V1: &str = "runtime_wait_changed.v1";
pub const PROVIDER_USAGE_SCHEMA_V1: &str = "provider_usage.v1";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTokenUsageV1 {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub prompt_cache_hit_tokens: Option<u64>,
    pub prompt_cache_miss_tokens: Option<u64>,
}

impl ProviderTokenUsageV1 {
    pub fn has_values(&self) -> bool {
        self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.total_tokens.is_some()
            || self.prompt_cache_hit_tokens.is_some()
            || self.prompt_cache_miss_tokens.is_some()
    }

    pub fn validate(&self) -> Result<(), String> {
        if matches!((self.input_tokens, self.total_tokens), (Some(input), Some(total)) if total < input)
        {
            return Err("provider_usage_total_tokens_less_than_input_tokens".to_string());
        }
        Ok(())
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, String> {
        fn add(left: Option<u64>, right: Option<u64>, field: &str) -> Result<Option<u64>, String> {
            match (left, right) {
                (None, None) => Ok(None),
                (left, right) => left
                    .unwrap_or_default()
                    .checked_add(right.unwrap_or_default())
                    .map(Some)
                    .ok_or_else(|| format!("provider_usage_{field}_overflow")),
            }
        }
        Ok(Self {
            input_tokens: add(self.input_tokens, other.input_tokens, "input_tokens")?,
            output_tokens: add(self.output_tokens, other.output_tokens, "output_tokens")?,
            total_tokens: add(self.total_tokens, other.total_tokens, "total_tokens")?,
            prompt_cache_hit_tokens: add(
                self.prompt_cache_hit_tokens,
                other.prompt_cache_hit_tokens,
                "prompt_cache_hit_tokens",
            )?,
            prompt_cache_miss_tokens: add(
                self.prompt_cache_miss_tokens,
                other.prompt_cache_miss_tokens,
                "prompt_cache_miss_tokens",
            )?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderUsageV1 {
    pub schema: String,
    pub latest_turn_id: String,
    pub latest: ProviderTokenUsageV1,
    pub session: ProviderTokenUsageV1,
}

impl ProviderUsageV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PROVIDER_USAGE_SCHEMA_V1 {
            return Err(format!("unsupported_provider_usage_schema:{}", self.schema));
        }
        validate_required_identity(self.latest_turn_id.as_str(), "latestTurnId")?;
        self.latest.validate()?;
        self.session.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunInterventionKindV1 {
    AnswerNow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunInterventionV1 {
    pub schema: String,
    pub intervention_id: String,
    pub agent_run_id: String,
    pub kind: AgentRunInterventionKindV1,
}

impl AgentRunInterventionV1 {
    pub fn answer_now(intervention_id: impl Into<String>, agent_run_id: impl Into<String>) -> Self {
        Self {
            schema: AGENT_RUN_INTERVENTION_SCHEMA_V1.to_string(),
            intervention_id: intervention_id.into(),
            agent_run_id: agent_run_id.into(),
            kind: AgentRunInterventionKindV1::AnswerNow,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != AGENT_RUN_INTERVENTION_SCHEMA_V1 {
            return Err(format!(
                "unsupported_agent_run_intervention_schema:{}",
                self.schema
            ));
        }
        validate_required_identity(self.intervention_id.as_str(), "interventionId")?;
        validate_required_identity(self.agent_run_id.as_str(), "agentRunId")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunInterventionStatusV1 {
    Requested,
    Applied,
    SatisfiedByFinal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunInterventionChangedV1 {
    pub schema: String,
    pub intervention_id: String,
    pub agent_run_id: String,
    pub kind: AgentRunInterventionKindV1,
    pub status: AgentRunInterventionStatusV1,
    pub actor_id: String,
    pub at_ms: TimestampMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_boundary: Option<String>,
}

impl AgentRunInterventionChangedV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1 {
            return Err(format!(
                "unsupported_agent_run_intervention_changed_schema:{}",
                self.schema
            ));
        }
        validate_required_identity(self.intervention_id.as_str(), "interventionId")?;
        validate_required_identity(self.agent_run_id.as_str(), "agentRunId")?;
        validate_required_identity(self.actor_id.as_str(), "actorId")?;
        match self.status {
            AgentRunInterventionStatusV1::Requested if self.safe_boundary.is_some() => {
                Err("agent_run_intervention_requested_must_not_have_safe_boundary".to_string())
            }
            AgentRunInterventionStatusV1::Applied
            | AgentRunInterventionStatusV1::SatisfiedByFinal => validate_required_identity(
                self.safe_boundary.as_deref().unwrap_or_default(),
                "safeBoundary",
            ),
            AgentRunInterventionStatusV1::Requested => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAgentRunIdentityV1 {
    pub agent_run_id: String,
    pub execution_id: String,
    pub authorization_digest: String,
}

impl RuntimeAgentRunIdentityV1 {
    pub fn validate(&self) -> Result<(), String> {
        Self::validate_agent_run(
            self.agent_run_id.as_str(),
            self.authorization_digest.as_str(),
        )?;
        validate_required_identity(self.execution_id.as_str(), "executionId")?;
        Ok(())
    }

    pub(crate) fn validate_agent_run(
        agent_run_id: &str,
        authorization_digest: &str,
    ) -> Result<(), String> {
        validate_required_identity(agent_run_id, "agentRunId")?;
        validate_sha256_digest(authorization_digest, "authorizationDigest")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeJobWaitV1 {
    pub tool_call_id: String,
    pub source_tool_name: String,
    pub tool_definition_digest: String,
    pub job_id: String,
    pub job_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAwaitQuestionCheckpointV1 {
    pub schema: String,
    pub continuation_id: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub turn_id: String,
    pub question_id: String,
}

impl RuntimeAwaitQuestionCheckpointV1 {
    pub fn new(
        identity: &RuntimeAgentRunIdentityV1,
        turn_id: &str,
        question_id: &str,
    ) -> Result<Self, String> {
        identity.validate()?;
        validate_required_identity(turn_id, "turnId")?;
        validate_required_identity(question_id, "questionId")?;
        let continuation_id =
            question_continuation_id(identity.agent_run_id.as_str(), turn_id, question_id)?;
        let checkpoint = Self {
            schema: RUNTIME_AWAIT_QUESTION_SCHEMA_V1.to_string(),
            continuation_id,
            agent_run_id: identity.agent_run_id.clone(),
            authorization_digest: identity.authorization_digest.clone(),
            turn_id: turn_id.to_string(),
            question_id: question_id.to_string(),
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RUNTIME_AWAIT_QUESTION_SCHEMA_V1 {
            return Err(format!(
                "unsupported_runtime_await_question_schema:{}",
                self.schema
            ));
        }
        validate_wait_identity(
            self.continuation_id.as_str(),
            self.agent_run_id.as_str(),
            self.authorization_digest.as_str(),
            self.turn_id.as_str(),
        )?;
        validate_required_identity(self.question_id.as_str(), "questionId")?;
        let expected = question_continuation_id(
            self.agent_run_id.as_str(),
            self.turn_id.as_str(),
            self.question_id.as_str(),
        )?;
        if self.continuation_id != expected {
            return Err("runtime_await_question_continuation_id_mismatch".to_string());
        }
        Ok(())
    }
}

impl RuntimeJobWaitV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_required_identity(self.tool_call_id.as_str(), "toolCallId")?;
        validate_canonical_tool_name(self.source_tool_name.as_str())?;
        validate_sha256_digest(self.tool_definition_digest.as_str(), "toolDefinitionDigest")?;
        validate_required_identity(self.job_id.as_str(), "jobId")?;
        validate_required_identity(self.job_kind.as_str(), "jobKind")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAwaitJobCheckpointV1 {
    pub schema: String,
    pub continuation_id: String,
    pub agent_run_id: String,
    pub execution_id: String,
    pub authorization_digest: String,
    pub turn_id: String,
    pub waits: Vec<RuntimeJobWaitV1>,
}

impl RuntimeAwaitJobCheckpointV1 {
    pub fn new(
        identity: &RuntimeAgentRunIdentityV1,
        turn_id: &str,
        waits: Vec<RuntimeJobWaitV1>,
    ) -> Result<Self, String> {
        identity.validate()?;
        validate_required_identity(turn_id, "turnId")?;
        let continuation_id =
            runtime_continuation_id(identity.agent_run_id.as_str(), turn_id, &waits)?;
        let checkpoint = Self {
            schema: RUNTIME_AWAIT_JOB_SCHEMA_V1.to_string(),
            continuation_id,
            agent_run_id: identity.agent_run_id.clone(),
            execution_id: identity.execution_id.clone(),
            authorization_digest: identity.authorization_digest.clone(),
            turn_id: turn_id.to_string(),
            waits,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RUNTIME_AWAIT_JOB_SCHEMA_V1 {
            return Err(format!(
                "unsupported_runtime_await_job_schema:{}",
                self.schema
            ));
        }
        validate_required_identity(self.continuation_id.as_str(), "continuationId")?;
        validate_required_identity(self.agent_run_id.as_str(), "agentRunId")?;
        validate_required_identity(self.execution_id.as_str(), "executionId")?;
        validate_sha256_digest(self.authorization_digest.as_str(), "authorizationDigest")?;
        validate_required_identity(self.turn_id.as_str(), "turnId")?;
        if self.waits.is_empty() {
            return Err("runtime_await_job_waits_must_not_be_empty".to_string());
        }
        let mut tool_call_ids = HashSet::with_capacity(self.waits.len());
        for wait in &self.waits {
            wait.validate()?;
            if !tool_call_ids.insert(wait.tool_call_id.as_str()) {
                return Err(format!(
                    "runtime_await_job_duplicate_tool_call_id:{}",
                    wait.tool_call_id
                ));
            }
        }
        let expected = runtime_continuation_id(
            self.agent_run_id.as_str(),
            self.turn_id.as_str(),
            self.waits.as_slice(),
        )?;
        if self.continuation_id != expected {
            return Err("runtime_await_job_continuation_id_mismatch".to_string());
        }
        Ok(())
    }
}

fn validate_wait_identity(
    continuation_id: &str,
    agent_run_id: &str,
    authorization_digest: &str,
    turn_id: &str,
) -> Result<(), String> {
    validate_required_identity(continuation_id, "continuationId")?;
    validate_required_identity(agent_run_id, "agentRunId")?;
    validate_sha256_digest(authorization_digest, "authorizationDigest")?;
    validate_required_identity(turn_id, "turnId")
}

fn question_continuation_id(
    agent_run_id: &str,
    turn_id: &str,
    question_id: &str,
) -> Result<String, String> {
    validate_required_identity(agent_run_id, "agentRunId")?;
    validate_required_identity(turn_id, "turnId")?;
    validate_required_identity(question_id, "questionId")?;
    let preimage = serde_json::to_vec(&("question", agent_run_id, turn_id, question_id))
        .map_err(|error| format!("serialize question continuation identity failed: {error}"))?;
    Ok(format!(
        "runtime_continuation:{:x}",
        Sha256::digest(preimage)
    ))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeWaitStatusV1 {
    Waiting,
    Resumed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeWaitChangedV1 {
    pub schema: String,
    pub continuation_id: String,
    pub agent_run_id: String,
    pub status: RuntimeWaitStatusV1,
    pub transition_reason: String,
    pub at_ms: TimestampMs,
}

impl RuntimeWaitChangedV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RUNTIME_WAIT_CHANGED_SCHEMA_V1 {
            return Err(format!(
                "unsupported_runtime_wait_changed_schema:{}",
                self.schema
            ));
        }
        validate_required_identity(self.continuation_id.as_str(), "continuationId")?;
        validate_required_identity(self.agent_run_id.as_str(), "agentRunId")?;
        validate_required_identity(self.transition_reason.as_str(), "transitionReason")
    }
}

pub fn runtime_continuation_id(
    agent_run_id: &str,
    turn_id: &str,
    waits: &[RuntimeJobWaitV1],
) -> Result<String, String> {
    validate_required_identity(agent_run_id, "agentRunId")?;
    validate_required_identity(turn_id, "turnId")?;
    if waits.is_empty() {
        return Err("runtime_await_job_waits_must_not_be_empty".to_string());
    }
    let preimage = serde_json::to_vec(&(agent_run_id, turn_id, waits))
        .map_err(|error| format!("serialize runtime continuation identity failed: {error}"))?;
    Ok(format!(
        "runtime_continuation:{:x}",
        Sha256::digest(preimage)
    ))
}

fn validate_required_identity(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("agent_run_intervention_{field}_is_required"));
    }
    if value.trim() != value {
        return Err(format!(
            "agent_run_intervention_{field}_has_outer_whitespace"
        ));
    }
    Ok(())
}

fn validate_sha256_digest(value: &str, field: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(format!("agent_run_intervention_{field}_must_be_sha256"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "agent_run_intervention_{field}_must_have_64_lowercase_hex_characters"
        ));
    }
    Ok(())
}

fn validate_canonical_tool_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.starts_with('_')
        || value.ends_with('_')
        || value.contains("__")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(format!(
            "runtime_await_job_invalid_source_tool_name:{value}"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args_json: String,
}

/// Raw diagnostic data preserved for audit/replay.  Never injected into LLM
/// context or UI projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalDiagnostic {
    pub id: String,
    pub source: String,
    pub failure_kind: Option<String>,
    pub summary: String,
    pub raw_detail: Option<String>,
    pub created_at_ms: TimestampMs,
}
