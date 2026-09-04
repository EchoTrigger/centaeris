use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub mod external_context;
pub mod manager;
pub mod reliability;
pub mod state;
pub mod store;
pub mod supplement;

use crate::execution::MAX_PUBLISHED_ARTIFACT_BYTES;
use crate::model::prepared_prompt::{ModelMessageRoleV1, ModelMessageV1, PREPARED_PROMPT_SCHEMA};
use crate::runtime::contracts::{
    CheckpointRecord, ProviderTokenUsageV1, ProviderUsageV1, RuntimeProcessState,
    PROVIDER_USAGE_SCHEMA_V1,
};
use crate::runtime::event::{
    RuntimeEventProjection, RuntimeEventVisibility, RUNTIME_EVENT_VERSION,
};
use crate::runtime::{ContextTokenBreakdownV1, ModelObservationV1};
use crate::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, ModelToolCallStateV1, SessionStateSnapshot,
};
use crate::tool::knowledge::KnowledgeLocatorV1;
use crate::tool::layer::ToolResultState;
use crate::tool::ModelToolChoice;

mod wire;

pub use wire::{
    parse_manifest, parse_wire_record, wire_record_value, SessionManifestV1, SessionProtocolError,
    SessionProtocolErrorKind, SESSION_MANIFEST_SCHEMA_VERSION, SESSION_PROTOCOL_MAJOR,
};

pub const SESSION_EVENT_SCHEMA_VERSION: &str = "session.event.v1";
pub const SESSION_EVENT_VERSION: u32 = 1;
pub const SESSION_EVENT_ID_MAX_BYTES: usize = 160;

pub fn stable_session_event_id(kind: &str, components: &[&str]) -> String {
    let preimage = serde_json::to_vec(&(kind, components))
        .expect("stable session event identity contains only strings");
    format!("evt_v1_{kind}:sha256:{:x}", Sha256::digest(preimage))
}

pub type SessionLogFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SessionCommitReceipt, String>> + Send + 'a>>;
pub const RUNTIME_JOB_LEASE_FENCE_REJECTED: &str = "runtime_job_lease_fence_rejected";

#[derive(Debug, Clone, PartialEq)]
pub struct CommittedSessionRecord {
    pub sequence: u64,
    pub event: SessionLogRecord,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionCommitReceipt {
    pub records: Vec<CommittedSessionRecord>,
}

impl SessionCommitReceipt {
    pub fn last_sequence(&self) -> Result<u64, String> {
        self.records
            .last()
            .map(|record| record.sequence)
            .ok_or_else(|| "session commit receipt must not be empty".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJobLeaseFence {
    pub job_id: String,
    pub job_kind: String,
    pub lease_owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteLastUserTailRequest {
    pub target_message_id: String,
    pub expected_tail_message_id: String,
    pub new_turn_id: String,
    pub new_agent_run_id: String,
    pub created_at_ms: i64,
}

pub trait SessionLogPort: Send + Sync {
    fn append_session_records<'a>(
        &'a self,
        agent_run_id: &'a str,
        events: &'a [SequencedSessionRecord],
    ) -> SessionLogFuture<'a>;

    fn append_session_records_with_runtime_job_lease<'a>(
        &'a self,
        agent_run_id: &'a str,
        events: &'a [SequencedSessionRecord],
        fence: &'a RuntimeJobLeaseFence,
    ) -> SessionLogFuture<'a>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRecordType {
    SessionMeta,
    AgentRunStarted,
    AgentRunExecutionStarted,
    AgentRunExecutionEnded,
    UserMessage,
    TurnSupplement,
    AssistantMessage,
    ToolCall,
    ToolResult,
    ModelRequestStarted,
    ProviderUsage,
    PhaseEvent,
    ExternalEvidenceRef,
    CitationRecorded,
    ArtifactPublished,
    Compaction,
    CheckpointRef,
    Tombstone,
    FileFact,
    AgentRunCompleted,
    AgentRunFailed,
    AgentRunInterrupted,
}

impl SessionRecordType {
    pub const fn event_version(self) -> u32 {
        SESSION_EVENT_VERSION
    }

    pub fn allowed_type_names() -> &'static [&'static str] {
        &[
            "session_meta",
            "agent_run_started",
            "agent_run_execution_started",
            "agent_run_execution_ended",
            "user_message",
            "turn_supplement",
            "assistant_message",
            "tool_call",
            "tool_result",
            "model_request_started",
            "provider_usage",
            "phase_event",
            "external_evidence_ref",
            "citation_recorded",
            "artifact_published",
            "compaction",
            "checkpoint_ref",
            "tombstone",
            "file_fact",
            "agent_run_completed",
            "agent_run_failed",
            "agent_run_interrupted",
        ]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionMeta => "session_meta",
            Self::AgentRunStarted => "agent_run_started",
            Self::AgentRunExecutionStarted => "agent_run_execution_started",
            Self::AgentRunExecutionEnded => "agent_run_execution_ended",
            Self::UserMessage => "user_message",
            Self::TurnSupplement => "turn_supplement",
            Self::AssistantMessage => "assistant_message",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::ModelRequestStarted => "model_request_started",
            Self::ProviderUsage => "provider_usage",
            Self::PhaseEvent => "phase_event",
            Self::ExternalEvidenceRef => "external_evidence_ref",
            Self::CitationRecorded => "citation_recorded",
            Self::ArtifactPublished => "artifact_published",
            Self::Compaction => "compaction",
            Self::CheckpointRef => "checkpoint_ref",
            Self::Tombstone => "tombstone",
            Self::FileFact => "file_fact",
            Self::AgentRunCompleted => "agent_run_completed",
            Self::AgentRunFailed => "agent_run_failed",
            Self::AgentRunInterrupted => "agent_run_interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    pub object_id: String,
    pub content_type: String,
    pub sha256: String,
    pub byte_length: u64,
    pub storage_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageAttachmentRef {
    input_ref: String,
    display_name: String,
    content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    placeholder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionLogRecord {
    pub schema_version: String,
    pub event_version: u32,
    #[serde(rename = "type")]
    pub event_type: SessionRecordType,
    pub event_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_run_id: Option<String>,
    pub created_at_ms: i64,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionMetadataV1 {
    pub record_id: String,
    pub title: String,
    pub cwd: String,
    pub session_kind: String,
    pub parent_session_id: Option<String>,
    pub runtime_job_id: Option<String>,
    pub sort_order: Option<i64>,
    pub is_pinned: bool,
    pub is_unread: bool,
}

impl SessionMetadataV1 {
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("recordId", self.record_id.as_str()),
            ("title", self.title.as_str()),
            ("cwd", self.cwd.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("session_meta {field} is required"));
            }
        }
        match self.session_kind.as_str() {
            "main" => {
                if self.parent_session_id.is_some() || self.runtime_job_id.is_some() {
                    return Err(
                        "session_meta main Session cannot carry subagent identity".to_string()
                    );
                }
                if self.sort_order.is_none() {
                    return Err("session_meta main Session sortOrder is required".to_string());
                }
            }
            "subagent" => {
                if self
                    .parent_session_id
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                    || self
                        .runtime_job_id
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(
                        "session_meta subagent Session parentSessionId and runtimeJobId are required"
                            .to_string(),
                    );
                }
                if self.sort_order.is_some() {
                    return Err("session_meta subagent Session cannot carry sortOrder".to_string());
                }
            }
            value => return Err(format!("unsupported session_meta sessionKind: {value}")),
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SequencedSessionRecord {
    pub sequence: u64,
    pub event: SessionLogRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAgentRunExecution {
    pub execution_id: String,
    pub authorization_digest: String,
}

#[derive(Debug, Clone)]
pub struct AgentRunSessionState {
    session_id: String,
    agent_run_id: String,
    next: u64,
    committed_session_sequence: u64,
    final_assistant_message_ids: HashSet<String>,
    assistant_turn_ids: HashMap<String, String>,
    committed_tool_call_ids: HashSet<String>,
    committed_tool_result_ids: HashSet<String>,
    committed_phase_turn_ids: HashSet<String>,
    agent_composition_digest: Option<String>,
    provider_usages: HashMap<String, Value>,
    committed_supplement_ids: HashSet<String>,
    committed_checkpoint_ids: HashSet<String>,
    tool_ledger_checkpointed: bool,
    citation_identities: HashMap<String, Value>,
    citation_products: Vec<(u64, Value)>,
    external_evidence_identities: HashMap<String, Value>,
    file_fact_identities: HashSet<String>,
    artifact_publication_identities: HashMap<String, Value>,
    artifact_links: Vec<(String, String)>,
    active_execution: Option<ActiveAgentRunExecution>,
    ended_execution_ids: HashSet<String>,
    used_recovery_checkpoint_ids: HashSet<String>,
}

impl AgentRunSessionState {
    pub fn new(
        session_id: impl Into<String>,
        agent_run_id: impl Into<String>,
    ) -> Result<Self, String> {
        let session_id = session_id.into();
        let agent_run_id = agent_run_id.into();
        if session_id.trim().is_empty() {
            return Err("AgentRun Session state sessionId is required".to_string());
        }
        if agent_run_id.trim().is_empty() {
            return Err("AgentRun Session state agentRunId is required".to_string());
        }
        Ok(Self {
            session_id,
            agent_run_id,
            next: 0,
            committed_session_sequence: 0,
            final_assistant_message_ids: HashSet::new(),
            assistant_turn_ids: HashMap::new(),
            committed_tool_call_ids: HashSet::new(),
            committed_tool_result_ids: HashSet::new(),
            committed_phase_turn_ids: HashSet::new(),
            agent_composition_digest: None,
            provider_usages: HashMap::new(),
            committed_supplement_ids: HashSet::new(),
            committed_checkpoint_ids: HashSet::new(),
            tool_ledger_checkpointed: false,
            citation_identities: HashMap::new(),
            citation_products: Vec::new(),
            external_evidence_identities: HashMap::new(),
            file_fact_identities: HashSet::new(),
            artifact_publication_identities: HashMap::new(),
            artifact_links: Vec::new(),
            active_execution: None,
            ended_execution_ids: HashSet::new(),
            used_recovery_checkpoint_ids: HashSet::new(),
        })
    }

    pub fn session_id(&self) -> &str {
        self.session_id.as_str()
    }

    pub fn agent_run_id(&self) -> &str {
        self.agent_run_id.as_str()
    }

    pub fn next_sequence(&self) -> u64 {
        self.next
    }

    pub fn is_empty(&self) -> bool {
        self.next == 0
    }

    pub fn reserve_rewritten_user_predecessor(&mut self) -> Result<(), String> {
        if self.next != 0 {
            return Err(
                "rewritten user predecessor requires an empty AgentRun Session state".to_string(),
            );
        }
        self.next = 1;
        Ok(())
    }

    pub fn committed_session_sequence(&self) -> u64 {
        self.committed_session_sequence
    }

    pub fn set_committed_session_sequence(&mut self, sequence: u64) {
        self.committed_session_sequence = sequence;
    }

    pub fn has_tool_call(&self, tool_call_id: &str) -> bool {
        self.committed_tool_call_ids.contains(tool_call_id)
    }

    pub fn has_tool_result(&self, tool_call_id: &str) -> bool {
        self.committed_tool_result_ids.contains(tool_call_id)
    }

    pub fn tool_result_ids(&self) -> &HashSet<String> {
        &self.committed_tool_result_ids
    }

    pub fn open_tool_call_ids(&self) -> Vec<String> {
        let mut ids = self
            .committed_tool_call_ids
            .difference(&self.committed_tool_result_ids)
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }

    pub fn has_checkpoint(&self, checkpoint_id: &str) -> bool {
        self.committed_checkpoint_ids.contains(checkpoint_id)
    }

    pub fn tool_ledger_is_checkpointed(&self) -> bool {
        self.tool_ledger_checkpointed
    }

    pub fn has_used_recovery_checkpoint(&self, checkpoint_id: &str) -> bool {
        self.used_recovery_checkpoint_ids.contains(checkpoint_id)
    }

    pub fn has_phase_turn(&self, turn_id: &str) -> bool {
        self.committed_phase_turn_ids.contains(turn_id)
    }

    pub fn provider_usage(&self, turn_id: &str) -> Option<&Value> {
        self.provider_usages.get(turn_id)
    }

    pub fn has_supplement(&self, supplement_id: &str) -> bool {
        self.committed_supplement_ids.contains(supplement_id)
    }

    pub fn assistant_is_final(&self, message_id: &str) -> bool {
        self.final_assistant_message_ids.contains(message_id)
    }

    pub fn citation_products(&self) -> &[(u64, Value)] {
        self.citation_products.as_slice()
    }

    pub fn artifact_refs(&self) -> Vec<String> {
        self.artifact_links
            .iter()
            .map(|(artifact_ref, _)| artifact_ref.clone())
            .collect()
    }

    pub fn active_execution_id(&self) -> Option<&str> {
        self.active_execution
            .as_ref()
            .map(|execution| execution.execution_id.as_str())
    }

    pub fn active_execution(&self) -> Option<&ActiveAgentRunExecution> {
        self.active_execution.as_ref()
    }

    pub(crate) fn artifact_fact_matches(&self, payload: &Value) -> Result<bool, String> {
        payload_identity_matches(
            &self.artifact_publication_identities,
            "publicationId",
            payload,
        )
    }

    pub(crate) fn citation_fact_matches(&self, payload: &Value) -> Result<bool, String> {
        let citation_id = state_payload_string(payload, "citationId")?;
        let mut identity = payload.clone();
        identity
            .as_object_mut()
            .ok_or_else(|| "citation payload must be an object".to_string())?
            .remove("sourceToolCallId");
        match self.citation_identities.get(citation_id) {
            Some(existing) if existing == &identity => Ok(true),
            Some(_) => Err(format!(
                "citation identity conflict for citationId {citation_id}"
            )),
            None => Ok(false),
        }
    }

    pub(crate) fn external_evidence_fact_matches(&self, payload: &Value) -> Result<bool, String> {
        payload_identity_matches(&self.external_evidence_identities, "objectRef", payload)
    }

    pub(crate) fn has_file_fact(&self, payload: &Value) -> Result<bool, String> {
        Ok(self
            .file_fact_identities
            .contains(session_payload_digest(payload)?.as_str()))
    }

    pub fn restore(&mut self, record: SequencedSessionRecord) -> Result<(), String> {
        let expected = self
            .next
            .checked_add(1)
            .ok_or_else(|| "AgentRun Session sequence exhausted".to_string())?;
        if record.sequence != expected {
            return Err("existing AgentRun Session sequence is not contiguous".to_string());
        }
        self.validate_identity(&record.event)?;
        self.track(&record.event, record.sequence)?;
        self.next = record.sequence;
        Ok(())
    }

    pub fn record(&mut self, event: SessionLogRecord) -> Result<SequencedSessionRecord, String> {
        self.validate_identity(&event)?;
        let sequence = self
            .next
            .checked_add(1)
            .ok_or_else(|| "AgentRun Session sequence exhausted".to_string())?;
        self.track(&event, sequence)?;
        self.next = sequence;
        Ok(SequencedSessionRecord { sequence, event })
    }

    pub fn event_for_turn(
        &mut self,
        turn_id: &str,
        event_type: SessionRecordType,
        payload: Value,
        created_at_ms: i64,
    ) -> Result<SequencedSessionRecord, String> {
        if turn_id.trim().is_empty() {
            return Err("session record turnId is required".to_string());
        }
        let sequence = self
            .next
            .checked_add(1)
            .ok_or_else(|| "AgentRun Session sequence exhausted".to_string())?;
        let event = canonical_session_record(
            format!("event:{}:{sequence}", self.agent_run_id),
            event_type,
            self.session_id.clone(),
            Some(turn_id.to_string()),
            Some(self.agent_run_id.clone()),
            created_at_ms,
            payload,
        )?;
        self.record(event)
    }

    pub fn start(
        &mut self,
        turn_id: &str,
        user_objective: &str,
        attachments: Vec<Value>,
        created_at_ms: i64,
    ) -> Result<Vec<SequencedSessionRecord>, String> {
        started_agent_run_records_with_attachments(
            self.session_id.as_str(),
            turn_id,
            self.agent_run_id.as_str(),
            user_objective,
            attachments,
            created_at_ms,
        )?
        .into_iter()
        .map(|event| self.record(event))
        .collect()
    }

    pub fn start_execution(
        &mut self,
        turn_id: &str,
        execution_id: &str,
        authorization_digest: &str,
        recovered_from_checkpoint_id: Option<&str>,
        created_at_ms: i64,
    ) -> Result<SequencedSessionRecord, String> {
        self.event_for_turn(
            turn_id,
            SessionRecordType::AgentRunExecutionStarted,
            serde_json::json!({
                "executionId": execution_id,
                "authorizationDigest": authorization_digest,
                "recoveredFromCheckpointId": recovered_from_checkpoint_id,
            }),
            created_at_ms,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "execution terminal record keeps durable outcome fields explicit"
    )]
    pub fn end_execution(
        &mut self,
        turn_id: &str,
        execution_id: &str,
        outcome: &str,
        reason_code: &str,
        retryable: bool,
        last_checkpoint_id: Option<&str>,
        indeterminate_tool_call_ids: Vec<String>,
        created_at_ms: i64,
    ) -> Result<SequencedSessionRecord, String> {
        self.event_for_turn(
            turn_id,
            SessionRecordType::AgentRunExecutionEnded,
            serde_json::json!({
                "executionId": execution_id,
                "outcome": outcome,
                "reasonCode": reason_code,
                "retryable": retryable,
                "lastCheckpointId": last_checkpoint_id,
                "indeterminateToolCallIds": indeterminate_tool_call_ids,
            }),
            created_at_ms,
        )
    }

    pub fn supplement(
        &mut self,
        turn_id: &str,
        supplement_id: &str,
        message: &str,
        created_at_ms: i64,
    ) -> Result<Option<SequencedSessionRecord>, String> {
        if self.has_supplement(supplement_id) {
            return Ok(None);
        }
        self.record(turn_supplement_record(
            self.session_id.as_str(),
            turn_id,
            self.agent_run_id.as_str(),
            supplement_id,
            message,
            created_at_ms,
        )?)
        .map(Some)
    }

    pub fn assistant(
        &mut self,
        turn_id: &str,
        model_markdown: &str,
        artifact_refs: Vec<String>,
        status: &str,
        created_at_ms: i64,
    ) -> Result<Option<SequencedSessionRecord>, String> {
        let message_id = format!("message:{turn_id}:assistant");
        if self.assistant_is_final(message_id.as_str()) {
            return Ok(None);
        }
        self.record(sealed_assistant_message_record_with_artifact_refs(
            self.session_id.as_str(),
            turn_id,
            self.agent_run_id.as_str(),
            model_markdown,
            artifact_refs,
            status,
            created_at_ms,
        )?)
        .map(Some)
    }

    pub fn provider_usage_record(
        &mut self,
        turn_id: &str,
        usage: &ProviderTokenUsageV1,
        created_at_ms: i64,
    ) -> Result<Option<SequencedSessionRecord>, String> {
        let record = provider_usage_record(
            self.session_id.as_str(),
            turn_id,
            self.agent_run_id.as_str(),
            usage,
            created_at_ms,
        )?;
        if let Some(committed) = self.provider_usage(turn_id) {
            return if committed == &record.payload {
                Ok(None)
            } else {
                Err(format!(
                    "provider usage turn idempotency conflict: {turn_id}"
                ))
            };
        }
        self.record(record).map(Some)
    }

    pub fn complete(
        &mut self,
        turn_id: &str,
        completion_reason: &str,
        created_at_ms: i64,
    ) -> Result<SequencedSessionRecord, String> {
        self.record(completed_agent_run_record(
            self.session_id.as_str(),
            turn_id,
            self.agent_run_id.as_str(),
            completion_reason,
            created_at_ms,
        )?)
    }

    pub fn fail(
        &mut self,
        turn_id: &str,
        failure_kind: &str,
        error: &str,
        created_at_ms: i64,
    ) -> Result<SequencedSessionRecord, String> {
        self.record(failed_agent_run_record(
            self.session_id.as_str(),
            turn_id,
            self.agent_run_id.as_str(),
            failure_kind,
            error,
            created_at_ms,
        )?)
    }

    pub fn interrupt(
        &mut self,
        turn_id: &str,
        reason: &str,
        message: &str,
        retryable: bool,
        created_at_ms: i64,
    ) -> Result<SequencedSessionRecord, String> {
        self.record(interrupted_agent_run_record(
            self.session_id.as_str(),
            turn_id,
            self.agent_run_id.as_str(),
            reason,
            message,
            retryable,
            created_at_ms,
        )?)
    }

    pub fn checkpoint_ref(
        &mut self,
        checkpoint: &CheckpointRecord,
    ) -> Result<SequencedSessionRecord, String> {
        if checkpoint.session_id != self.session_id {
            return Err("checkpoint Session identity mismatch".to_string());
        }
        let payload_bytes = checkpoint.payload_json.as_bytes();
        if payload_bytes.is_empty() {
            return Err("checkpoint payload_json is required".to_string());
        }
        let payload_sha256 = format!("sha256:{:x}", Sha256::digest(payload_bytes));
        self.record(canonical_session_record(
            format!(
                "evt:checkpoint_ref:{}:{}",
                self.session_id, checkpoint.checkpoint_id
            ),
            SessionRecordType::CheckpointRef,
            self.session_id.clone(),
            Some(checkpoint.turn_id.clone()),
            Some(self.agent_run_id.clone()),
            checkpoint.updated_at_ms,
            serde_json::json!({
                "checkpointId": checkpoint.checkpoint_id,
                "kind": checkpoint.kind.as_str(),
                "objectRef": format!("checkpoint-object:{payload_sha256}"),
                "status": checkpoint.status,
                "payloadSha256": payload_sha256,
                "payloadByteLength": payload_bytes.len(),
                "updatedAtMs": checkpoint.updated_at_ms,
            }),
        )?)
    }

    pub fn file_fact(
        &mut self,
        turn_id: &str,
        payload: Value,
        created_at_ms: i64,
    ) -> Result<SequencedSessionRecord, String> {
        let digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&payload)
                    .map_err(|error| format!("serialize file fact failed: {error}"))?,
            )
        );
        self.record(canonical_session_record(
            format!(
                "evt:file_fact:{}:{}:{}",
                self.agent_run_id,
                self.next,
                &digest[..16]
            ),
            SessionRecordType::FileFact,
            self.session_id.clone(),
            Some(turn_id.to_string()),
            Some(self.agent_run_id.clone()),
            created_at_ms,
            payload,
        )?)
    }

    fn validate_identity(&self, event: &SessionLogRecord) -> Result<(), String> {
        if event.session_id != self.session_id
            || event.agent_run_id.as_deref() != Some(self.agent_run_id.as_str())
        {
            return Err("canonical Session record AgentRun identity mismatch".to_string());
        }
        Ok(())
    }

    fn track(&mut self, event: &SessionLogRecord, sequence: u64) -> Result<(), String> {
        match event.event_type {
            SessionRecordType::AgentRunExecutionStarted => {
                let execution_id = state_payload_string(&event.payload, "executionId")?;
                if self.active_execution.is_some() {
                    return Err("AgentRun already has an active Execution".to_string());
                }
                if self.ended_execution_ids.contains(execution_id) {
                    return Err(format!("AgentRun Execution cannot restart: {execution_id}"));
                }
                let recovered_from_checkpoint_id = match event
                    .payload
                    .get("recoveredFromCheckpointId")
                {
                    Some(Value::Null) => None,
                    Some(Value::String(value)) => Some(value.clone()),
                    _ => return Err("AgentRun Execution recovery identity is invalid".to_string()),
                };
                if self.ended_execution_ids.is_empty() != recovered_from_checkpoint_id.is_none() {
                    return Err("AgentRun Execution recovery lineage is invalid".to_string());
                }
                if recovered_from_checkpoint_id
                    .as_ref()
                    .is_some_and(|checkpoint_id| {
                        !self.committed_checkpoint_ids.contains(checkpoint_id)
                    })
                {
                    return Err("AgentRun Execution recovery checkpoint is missing".to_string());
                }
                if recovered_from_checkpoint_id
                    .as_ref()
                    .is_some_and(|checkpoint_id| {
                        !self
                            .used_recovery_checkpoint_ids
                            .insert(checkpoint_id.clone())
                    })
                {
                    return Err(
                        "AgentRun recovery checkpoint already started a replacement Execution"
                            .to_string(),
                    );
                }
                self.active_execution = Some(ActiveAgentRunExecution {
                    execution_id: execution_id.to_string(),
                    authorization_digest: state_payload_string(
                        &event.payload,
                        "authorizationDigest",
                    )?
                    .to_string(),
                });
            }
            SessionRecordType::AgentRunExecutionEnded => {
                let execution_id = state_payload_string(&event.payload, "executionId")?;
                if self.active_execution_id() != Some(execution_id) {
                    return Err(format!("AgentRun Execution is not active: {execution_id}"));
                }
                self.active_execution = None;
                self.ended_execution_ids.insert(execution_id.to_string());
            }
            SessionRecordType::AgentRunCompleted
            | SessionRecordType::AgentRunFailed
            | SessionRecordType::AgentRunInterrupted => {
                if self.active_execution.is_some() {
                    return Err(
                        "AgentRun terminal requires the active Execution to end".to_string()
                    );
                }
            }
            SessionRecordType::AssistantMessage => {
                let message_id = state_payload_string(&event.payload, "messageId")?;
                let status = state_payload_string(&event.payload, "status")?;
                if !matches!(status, "done" | "error") {
                    return Err(format!("assistant status is unsupported: {status}"));
                }
                let turn_id = event
                    .turn_id
                    .as_deref()
                    .ok_or_else(|| "assistant message turn is missing".to_string())?;
                if self
                    .assistant_turn_ids
                    .get(message_id)
                    .is_some_and(|previous_turn| previous_turn != turn_id)
                {
                    return Err(format!("assistant message turn mismatch: {message_id}"));
                }
                if self.assistant_is_final(message_id) {
                    return Err(format!(
                        "assistant message written after final: {message_id}"
                    ));
                }
                self.assistant_turn_ids
                    .insert(message_id.to_string(), turn_id.to_string());
                self.final_assistant_message_ids
                    .insert(message_id.to_string());
            }
            SessionRecordType::ToolCall => {
                self.tool_ledger_checkpointed = false;
                let call_id = state_payload_string(&event.payload, "callId")?;
                if !self.committed_tool_call_ids.insert(call_id.to_string()) {
                    return Err(format!("duplicate committed tool call: {call_id}"));
                }
            }
            SessionRecordType::ToolResult => {
                self.tool_ledger_checkpointed = false;
                let call_id = state_payload_string(&event.payload, "callId")?;
                if !self.committed_tool_call_ids.contains(call_id) {
                    return Err(format!("tool result precedes tool call: {call_id}"));
                }
                if !self.committed_tool_result_ids.insert(call_id.to_string()) {
                    return Err(format!("duplicate committed tool result: {call_id}"));
                }
            }
            SessionRecordType::PhaseEvent => {
                let turn_id = event
                    .turn_id
                    .as_deref()
                    .ok_or_else(|| "phase event turn is missing".to_string())?;
                if !self.committed_phase_turn_ids.insert(turn_id.to_string()) {
                    return Err(format!("duplicate committed phase event: {turn_id}"));
                }
            }
            SessionRecordType::ModelRequestStarted => {
                if state_payload_string(&event.payload, "purpose")? == "main" {
                    let digest = event
                        .payload
                        .pointer("/agentComposition/compositionDigest")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            "model request agent composition digest is missing".to_string()
                        })?;
                    match self.agent_composition_digest.as_deref() {
                        Some(existing) if existing != digest => {
                            return Err(
                                "model request changes immutable AgentRun composition".to_string()
                            )
                        }
                        Some(_) => {}
                        None => self.agent_composition_digest = Some(digest.to_string()),
                    }
                }
            }
            SessionRecordType::ProviderUsage => {
                let turn_id = event
                    .turn_id
                    .as_deref()
                    .ok_or_else(|| "provider usage turn is missing".to_string())?;
                if self
                    .provider_usages
                    .insert(turn_id.to_string(), event.payload.clone())
                    .is_some()
                {
                    return Err(format!("duplicate committed provider usage: {turn_id}"));
                }
            }
            SessionRecordType::CheckpointRef => {
                let checkpoint_id = state_payload_string(&event.payload, "checkpointId")?;
                if !self
                    .committed_checkpoint_ids
                    .insert(checkpoint_id.to_string())
                {
                    return Err(format!(
                        "duplicate committed checkpoint ref: {checkpoint_id}"
                    ));
                }
                if state_payload_string(&event.payload, "kind")? == "recovery" {
                    self.tool_ledger_checkpointed = self.open_tool_call_ids().is_empty();
                }
            }
            SessionRecordType::TurnSupplement => {
                let supplement_id = state_payload_string(&event.payload, "supplementId")?;
                if !self
                    .committed_supplement_ids
                    .insert(supplement_id.to_string())
                {
                    return Err(format!(
                        "duplicate committed turn supplement: {supplement_id}"
                    ));
                }
            }
            SessionRecordType::CitationRecorded => {
                register_citation_identity(&mut self.citation_identities, &event.payload)?;
                self.citation_products
                    .push((sequence, event.payload.clone()));
            }
            SessionRecordType::ExternalEvidenceRef => {
                register_payload_identity(
                    &mut self.external_evidence_identities,
                    "objectRef",
                    &event.payload,
                )?;
            }
            SessionRecordType::FileFact => {
                self.file_fact_identities
                    .insert(session_payload_digest(&event.payload)?);
            }
            SessionRecordType::ArtifactPublished => {
                register_payload_identity(
                    &mut self.artifact_publication_identities,
                    "publicationId",
                    &event.payload,
                )?;
                let artifact_ref = state_payload_string(&event.payload, "artifactRef")?;
                let filename = state_payload_string(&event.payload, "filename")?;
                match self
                    .artifact_links
                    .iter()
                    .find(|(existing, _)| existing == artifact_ref)
                {
                    Some((_, existing_filename)) if existing_filename == filename => {}
                    Some(_) => {
                        return Err(format!(
                            "artifact filename conflict for artifactRef {artifact_ref}"
                        ));
                    }
                    None => self
                        .artifact_links
                        .push((artifact_ref.to_string(), filename.to_string())),
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn register_payload_identity(
    seen: &mut HashMap<String, Value>,
    key: &str,
    payload: &Value,
) -> Result<bool, String> {
    let identity = state_payload_string(payload, key)?;
    match seen.get(identity) {
        Some(existing) if existing == payload => Ok(false),
        Some(_) => Err(format!(
            "session fact identity conflict for {key} {identity}"
        )),
        None => {
            seen.insert(identity.to_string(), payload.clone());
            Ok(true)
        }
    }
}

fn payload_identity_matches(
    seen: &HashMap<String, Value>,
    key: &str,
    payload: &Value,
) -> Result<bool, String> {
    let identity = state_payload_string(payload, key)?;
    match seen.get(identity) {
        Some(existing) if existing == payload => Ok(true),
        Some(_) => Err(format!(
            "session fact identity conflict for {key} {identity}"
        )),
        None => Ok(false),
    }
}

fn session_payload_digest(payload: &Value) -> Result<String, String> {
    serde_json::to_vec(payload)
        .map(|encoded| format!("{:x}", Sha256::digest(encoded)))
        .map_err(|error| format!("serialize Session fact failed: {error}"))
}

fn register_citation_identity(
    seen: &mut HashMap<String, Value>,
    payload: &Value,
) -> Result<bool, String> {
    let citation_id = payload
        .get("citationId")
        .and_then(Value::as_str)
        .ok_or_else(|| "citation payload is missing citationId".to_string())?;
    let mut identity = payload.clone();
    identity
        .as_object_mut()
        .ok_or_else(|| "citation payload must be an object".to_string())?
        .remove("sourceToolCallId");
    match seen.get(citation_id) {
        Some(existing) if existing == &identity => Ok(false),
        Some(_) => Err(format!(
            "citation identity conflict for citationId {citation_id}"
        )),
        None => {
            seen.insert(citation_id.to_string(), identity);
            Ok(true)
        }
    }
}

fn state_payload_string<'a>(payload: &'a Value, key: &str) -> Result<&'a str, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("session record payload field {key} must be a non-empty string"))
}

pub fn canonical_session_record(
    event_id: impl Into<String>,
    event_type: SessionRecordType,
    session_id: impl Into<String>,
    turn_id: Option<String>,
    agent_run_id: Option<String>,
    created_at_ms: i64,
    payload: Value,
) -> Result<SessionLogRecord, String> {
    let event = SessionLogRecord {
        schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
        event_version: event_type.event_version(),
        event_type,
        event_id: event_id.into(),
        session_id: session_id.into(),
        turn_id,
        agent_run_id,
        created_at_ms,
        payload,
    };
    validate_event_shape(&event)?;
    Ok(event)
}

pub fn provider_usage_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    usage: &ProviderTokenUsageV1,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    usage.validate()?;
    canonical_session_record(
        format!("evt:provider_usage:{session_id}:{turn_id}"),
        SessionRecordType::ProviderUsage,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::to_value(usage)
            .map_err(|error| format!("serialize provider usage failed: {error}"))?,
    )
}

pub fn started_agent_run_records(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    user_objective: &str,
    created_at_ms: i64,
) -> Result<[SessionLogRecord; 2], String> {
    started_agent_run_records_with_attachments(
        session_id,
        turn_id,
        agent_run_id,
        user_objective,
        Vec::new(),
        created_at_ms,
    )
}

pub fn started_agent_run_records_with_attachments(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    user_objective: &str,
    attachments: Vec<Value>,
    created_at_ms: i64,
) -> Result<[SessionLogRecord; 2], String> {
    let turn = canonical_session_record(
        format!("evt:agent_run_started:{session_id}:{turn_id}:{created_at_ms}"),
        SessionRecordType::AgentRunStarted,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({"userObjective": user_objective}),
    )?;
    let message = canonical_session_record(
        format!("evt:user_message:{session_id}:{turn_id}:{created_at_ms}"),
        SessionRecordType::UserMessage,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({
            "messageId": format!("message:{turn_id}:user"),
            "text": user_objective,
            "attachments": attachments,
        }),
    )?;
    Ok([turn, message])
}

pub fn sealed_assistant_message_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    model_markdown: &str,
    status: &str,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    sealed_assistant_message_record_with_artifact_refs(
        session_id,
        turn_id,
        agent_run_id,
        model_markdown,
        Vec::new(),
        status,
        created_at_ms,
    )
}

pub fn rewrite_last_user_tail_tombstone(
    existing: &[SessionLogRecord],
    session_id: &str,
    target_message_id: &str,
    expected_tail_message_id: &str,
    new_turn_id: &str,
    new_agent_run_id: &str,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    let active = active_session_records(session_id, existing)?;
    let messages = active
        .iter()
        .filter_map(|event| {
            let message_id = match event.event_type {
                SessionRecordType::UserMessage
                | SessionRecordType::TurnSupplement
                | SessionRecordType::AssistantMessage => {
                    event.payload.get("messageId").and_then(Value::as_str)
                }
                _ => None,
            }?;
            Some((event, message_id))
        })
        .collect::<Vec<_>>();
    let target_index = messages
        .iter()
        .position(|(_, message_id)| *message_id == target_message_id)
        .ok_or_else(|| "rewrite target user message was not found".to_string())?;
    if messages[target_index].0.event_type != SessionRecordType::UserMessage
        || messages.iter().rposition(|(event, _)| {
            matches!(
                event.event_type,
                SessionRecordType::UserMessage | SessionRecordType::TurnSupplement
            )
        }) != Some(target_index)
    {
        return Err("rewrite only supports the latest user message".to_string());
    }
    let actual_tail_message_id = messages
        .last()
        .map(|(_, message_id)| *message_id)
        .ok_or_else(|| "rewrite requires an active tail message".to_string())?;
    if actual_tail_message_id != expected_tail_message_id {
        return Err("rewrite active tail changed".to_string());
    }
    let agent_run_ids = messages[target_index..]
        .iter()
        .map(|(event, _)| {
            event
                .agent_run_id
                .as_deref()
                .ok_or_else(|| "rewrite tail message is missing agentRunId".to_string())
        })
        .collect::<Result<HashSet<_>, _>>()?;
    if active.iter().any(|event| {
        event.event_type == SessionRecordType::FileFact
            && event
                .agent_run_id
                .as_deref()
                .is_some_and(|agent_run_id| agent_run_ids.contains(agent_run_id))
    }) {
        return Err("cannot rewrite the last user input after file mutations".to_string());
    }
    let projection = reduce_events(session_id, active.iter())?;
    if agent_run_ids.iter().any(|agent_run_id| {
        projection
            .agent_runs
            .get(*agent_run_id)
            .is_none_or(|agent_run| agent_run.state == ReducedAgentRunState::Running)
    }) {
        return Err("rewrite target tail contains a non-terminal AgentRun".to_string());
    }
    let target_event_ids = active
        .iter()
        .filter(|event| {
            event
                .agent_run_id
                .as_deref()
                .is_some_and(|agent_run_id| agent_run_ids.contains(agent_run_id))
        })
        .map(|event| event.event_id.clone())
        .collect::<Vec<_>>();
    if target_event_ids.is_empty() {
        return Err("rewrite target tail has no active records".to_string());
    }
    canonical_session_record(
        format!("evt:tombstone:{session_id}:{new_agent_run_id}:{created_at_ms}"),
        SessionRecordType::Tombstone,
        session_id,
        Some(new_turn_id.to_string()),
        Some(new_agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({
            "tombstoneId": format!("tombstone:{session_id}:{new_agent_run_id}"),
            "targetEventIds": target_event_ids,
            "reasonType": "rewrite_last_user_input",
        }),
    )
}

pub fn turn_supplement_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    supplement_id: &str,
    message: &str,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    let supplement_id = crate::session::supplement::validate_turn_supplement_id(supplement_id)
        .map_err(|error| error.to_string())?;
    let message = crate::session::supplement::validate_turn_supplement_message(message)
        .map_err(|error| error.to_string())?;
    let event_identity = format!("{session_id}\0{turn_id}\0{supplement_id}");
    canonical_session_record(
        format!(
            "evt:turn_supplement:{:x}",
            Sha256::digest(event_identity.as_bytes())
        ),
        SessionRecordType::TurnSupplement,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({
            "supplementId": supplement_id,
            "messageId": format!("message:{turn_id}:supplement:{supplement_id}"),
            "message": message,
        }),
    )
}

pub fn sealed_assistant_message_record_with_artifact_refs(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    model_markdown: &str,
    artifact_refs: Vec<String>,
    status: &str,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    canonical_session_record(
        format!("evt:assistant_message:{session_id}:{turn_id}:{created_at_ms}"),
        SessionRecordType::AssistantMessage,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({
            "messageId": format!("message:{turn_id}:assistant"),
            "modelMarkdown": model_markdown,
            "artifactRefs": artifact_refs,
            "status": status,
        }),
    )
}

pub fn completed_agent_run_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    done_reason: &str,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    canonical_session_record(
        format!("evt:agent_run_completed:{session_id}:{turn_id}:{created_at_ms}"),
        SessionRecordType::AgentRunCompleted,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({"doneReason": done_reason}),
    )
}

pub fn failed_agent_run_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    reason_type: &str,
    message: &str,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    canonical_session_record(
        format!("evt:agent_run_failed:{session_id}:{turn_id}:{created_at_ms}"),
        SessionRecordType::AgentRunFailed,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({"reasonType": reason_type, "message": message}),
    )
}

pub fn interrupted_agent_run_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    reason_type: &str,
    message: &str,
    retryable: bool,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    canonical_session_record(
        format!("evt:agent_run_interrupted:{session_id}:{turn_id}:{created_at_ms}"),
        SessionRecordType::AgentRunInterrupted,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        serde_json::json!({
            "reasonType": reason_type,
            "message": message,
            "retryable": retryable,
        }),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SessionStreamProjection {
    SessionEvent {
        agent_run_id: String,
        cursor: String,
        event: RuntimeEventProjection,
    },
}

pub fn session_record_projects_to_agent_run_stream(event_type: SessionRecordType) -> bool {
    match event_type {
        SessionRecordType::AgentRunStarted
        | SessionRecordType::UserMessage
        | SessionRecordType::TurnSupplement
        | SessionRecordType::AssistantMessage
        | SessionRecordType::ToolCall
        | SessionRecordType::ToolResult
        | SessionRecordType::PhaseEvent
        | SessionRecordType::ExternalEvidenceRef
        | SessionRecordType::CitationRecorded
        | SessionRecordType::ArtifactPublished
        | SessionRecordType::Compaction
        | SessionRecordType::Tombstone
        | SessionRecordType::AgentRunCompleted
        | SessionRecordType::AgentRunFailed
        | SessionRecordType::AgentRunInterrupted => true,
        SessionRecordType::SessionMeta
        | SessionRecordType::AgentRunExecutionStarted
        | SessionRecordType::AgentRunExecutionEnded
        | SessionRecordType::ModelRequestStarted
        | SessionRecordType::ProviderUsage
        | SessionRecordType::CheckpointRef
        | SessionRecordType::FileFact => false,
    }
}

pub fn project_committed_session_record(
    record: &SessionLogRecord,
    cursor: u64,
) -> Result<SessionStreamProjection, String> {
    validate_event_shape(record)?;
    let agent_run_id = required_event_agent_run_id(record)?;
    Ok(SessionStreamProjection::SessionEvent {
        agent_run_id: agent_run_id.clone(),
        cursor: cursor.to_string(),
        event: committed_runtime_projection(record, agent_run_id.as_str())?,
    })
}

fn committed_runtime_projection(
    record: &SessionLogRecord,
    agent_run_id: &str,
) -> Result<RuntimeEventProjection, String> {
    let turn_id = required_event_turn_id(record)?;
    let payload_object = record.payload.as_object().ok_or_else(|| {
        format!(
            "session.event.v1 {} payload must be an object",
            record.event_id
        )
    })?;
    let (event_type, status, visibility, process_state, tool_name, payload): (
        &str,
        String,
        RuntimeEventVisibility,
        Option<RuntimeProcessState>,
        Option<String>,
        Value,
    ) = match record.event_type {
        SessionRecordType::AssistantMessage => (
            "Final",
            required_payload_string(payload_object, "status", record)?.to_string(),
            RuntimeEventVisibility::User,
            Some(RuntimeProcessState::Reviewing),
            None,
            serde_json::json!({
                "content": required_payload_string_allow_empty(payload_object, "modelMarkdown", record)?,
                "artifactRefs": record.payload.get("artifactRefs").cloned().unwrap_or_else(|| Value::Array(vec![])),
            }),
        ),
        SessionRecordType::PhaseEvent => (
            "Status",
            "running".to_string(),
            RuntimeEventVisibility::User,
            Some(RuntimeProcessState::Thinking),
            None,
            record.payload.clone(),
        ),
        SessionRecordType::ToolCall => (
            "ToolCall",
            "running".to_string(),
            RuntimeEventVisibility::User,
            Some(RuntimeProcessState::Executing),
            Some(required_payload_string(payload_object, "toolName", record)?.to_string()),
            record.payload.clone(),
        ),
        SessionRecordType::ToolResult => {
            let result_state = parse_result_state(payload_object, record)?;
            (
                "ToolResult",
                (if result_state.is_success() {
                    "done"
                } else {
                    "error"
                })
                .to_string(),
                RuntimeEventVisibility::User,
                Some(RuntimeProcessState::Executing),
                Some(required_payload_string(payload_object, "toolName", record)?.to_string()),
                record.payload.clone(),
            )
        }
        SessionRecordType::ModelRequestStarted => {
            return Err(format!(
                "{} has no AgentRun stream projection",
                record.event_type.as_str()
            ))
        }
        SessionRecordType::ProviderUsage => {
            return Err("provider_usage has no AgentRun stream projection".to_string())
        }
        SessionRecordType::CitationRecorded => (
            "Citation",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::ArtifactPublished => (
            "Artifact",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::AgentRunStarted => (
            "AgentRunStarted",
            "running".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::AgentRunExecutionStarted => (
            "AgentRunExecutionStarted",
            "running".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::AgentRunExecutionEnded => (
            "AgentRunExecutionEnded",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::UserMessage => (
            "UserMessage",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::TurnSupplement => (
            "TurnSupplement",
            "done".to_string(),
            RuntimeEventVisibility::User,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::ExternalEvidenceRef => (
            "ExternalEvidenceRef",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::Compaction => (
            "PromptCompaction",
            "done".to_string(),
            RuntimeEventVisibility::User,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::CheckpointRef => (
            "CheckpointRef",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::Tombstone => (
            "Tombstone",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::FileFact => (
            "FileFact",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::AgentRunCompleted => (
            "AgentRunCompleted",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::AgentRunFailed => (
            "AgentRunFailed",
            "error".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::AgentRunInterrupted => (
            "AgentRunInterrupted",
            "done".to_string(),
            RuntimeEventVisibility::Internal,
            None,
            None,
            record.payload.clone(),
        ),
        SessionRecordType::SessionMeta => {
            return Err("session_meta has no AgentRun stream projection".to_string())
        }
    };
    let event = RuntimeEventProjection {
        event_id: record.event_id.clone(),
        version: RUNTIME_EVENT_VERSION.to_string(),
        event_type: event_type.to_string(),
        at_ms: record.created_at_ms,
        session_id: record.session_id.clone(),
        turn_id: turn_id.to_string(),
        task_id: agent_run_id.to_string(),
        parent_task_id: turn_id.to_string(),
        status,
        visibility,
        tool_name,
        process_state,
        payload,
        meta: serde_json::json!({"source": "core.session_log", "durable": true}),
    };
    event.validate()?;
    Ok(event)
}

pub fn validate_sequenced_session_records(events: &[SequencedSessionRecord]) -> Result<(), String> {
    if events.is_empty() {
        return Err("session record batch must not be empty".to_string());
    }
    let expected_session_id = events[0].event.session_id.as_str();
    let mut previous_sequence = 0;
    let mut event_ids = HashSet::new();
    for item in events {
        if item.sequence <= previous_sequence {
            return Err("session record sequence must increase".to_string());
        }
        previous_sequence = item.sequence;
        validate_event_shape(&item.event)?;
        if item.event.session_id != expected_session_id {
            return Err("session record batch contains multiple sessions".to_string());
        }
        if !event_ids.insert(item.event.event_id.as_str()) {
            return Err(format!(
                "session record batch contains duplicate eventId: {}",
                item.event.event_id
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedMessage {
    pub message_id: String,
    pub role: ReducedMessageRole,
    pub turn_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub text: String,
    pub artifact_refs: Vec<String>,
    pub status: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReducedMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub turn_id: String,
    pub agent_run_id: String,
    pub normalized_input: Value,
    pub result_state: Option<ToolResultState>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedAgentRun {
    pub agent_run_id: String,
    pub initial_turn_id: String,
    pub state: ReducedAgentRunState,
    pub reason_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedAgentRunExecution {
    pub execution_id: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub recovered_from_checkpoint_id: Option<String>,
    pub outcome: Option<String>,
    pub reason_code: Option<String>,
    pub retryable: Option<bool>,
    pub last_checkpoint_id: Option<String>,
    pub indeterminate_tool_call_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedCitation {
    pub citation_id: String,
    pub input_ref: String,
    pub owner_ref: String,
    pub owner_kind: String,
    pub display_name: String,
    pub evidence_kind: String,
    pub owner_sha256: String,
    pub locator: Value,
    pub source_tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedArtifact {
    pub publication_id: String,
    pub artifact_ref: String,
    pub tool_call_id: String,
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReducedAgentRunState {
    Running,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionProjection {
    pub messages: BTreeMap<String, ReducedMessage>,
    pub tool_calls: BTreeMap<String, ReducedToolCall>,
    pub agent_runs: BTreeMap<String, ReducedAgentRun>,
    pub agent_run_executions: BTreeMap<String, ReducedAgentRunExecution>,
    pub citations: BTreeMap<String, ReducedCitation>,
    pub artifacts: BTreeMap<String, ReducedArtifact>,
    pub artifact_order: Vec<String>,
    pub tombstoned_event_ids: HashSet<String>,
    agent_composition_by_agent_run: HashMap<String, String>,
    provider_usage_by_turn: HashMap<(String, String), ProviderTokenUsageV1>,
    latest_provider_usage_turn_id: Option<String>,
    latest_provider_usage: Option<ProviderTokenUsageV1>,
    latest_provider_usage_updated_at_ms: Option<i64>,
    latest_provider_usage_context_token_estimate: Option<u64>,
    session_provider_usage: ProviderTokenUsageV1,
    latest_model_request_purpose: Option<String>,
    latest_model_request_agent_run_id: Option<String>,
    latest_main_context_token_estimate: Option<u64>,
    latest_main_context_token_breakdown: Option<ContextTokenBreakdownV1>,
    context_token_estimate_updated_at_ms: Option<i64>,
}

impl SessionProjection {
    pub fn provider_usage(&self) -> Option<ProviderUsageV1> {
        Some(ProviderUsageV1 {
            schema: PROVIDER_USAGE_SCHEMA_V1.to_string(),
            latest_turn_id: self.latest_provider_usage_turn_id.clone()?,
            latest: self.latest_provider_usage.clone()?,
            session: self.session_provider_usage.clone(),
        })
    }

    pub fn context_token_estimate(&self) -> Option<u64> {
        self.latest_main_context_token_estimate
    }

    pub fn context_token_breakdown(&self) -> Option<&ContextTokenBreakdownV1> {
        self.latest_main_context_token_breakdown.as_ref()
    }

    pub fn context_token_estimate_updated_at_ms(&self) -> Option<i64> {
        self.context_token_estimate_updated_at_ms
    }

    pub fn latest_provider_usage_updated_at_ms(&self) -> Option<i64> {
        self.latest_provider_usage_updated_at_ms
    }

    pub fn latest_provider_usage_context_token_estimate(&self) -> Option<u64> {
        self.latest_provider_usage_context_token_estimate
    }

    pub fn is_compacting(&self) -> bool {
        self.latest_model_request_purpose.as_deref() == Some("compaction")
            && self
                .latest_model_request_agent_run_id
                .as_ref()
                .and_then(|agent_run_id| self.agent_runs.get(agent_run_id))
                .is_some_and(|agent_run| agent_run.state == ReducedAgentRunState::Running)
    }
}

pub fn parse_event(value: &Value) -> Result<SessionLogRecord, String> {
    reject_unsupported_event_type(value)?;
    let event: SessionLogRecord = serde_json::from_value(value.clone())
        .map_err(|err| format!("session.event.v1 decode failed: {err}"))?;
    validate_event_shape(&event)?;
    Ok(event)
}

pub fn validate_event_shape(event: &SessionLogRecord) -> Result<(), String> {
    if event.event_version != event.event_type.event_version() {
        return Err(format!(
            "unsupported {} eventVersion: {}",
            event.event_type.as_str(),
            event.event_version
        ));
    }
    if event.schema_version != SESSION_EVENT_SCHEMA_VERSION {
        return Err(format!(
            "session.event.v1 schemaVersion mismatch: expected {SESSION_EVENT_SCHEMA_VERSION}, got {}",
            event.schema_version
        ));
    }
    if event.event_id.trim().is_empty() {
        return Err("session.event.v1 eventId is required".to_string());
    }
    if event.event_id.len() > SESSION_EVENT_ID_MAX_BYTES {
        return Err(format!(
            "session.event.v1 eventId exceeds {SESSION_EVENT_ID_MAX_BYTES} bytes"
        ));
    }
    if event.session_id.trim().is_empty() {
        return Err("session.event.v1 sessionId is required".to_string());
    }
    require_payload_object(event)?;
    match event.event_type {
        SessionRecordType::SessionMeta => validate_session_meta(event),
        SessionRecordType::AgentRunStarted => validate_agent_run_started(event),
        SessionRecordType::AgentRunExecutionStarted => validate_agent_run_execution_started(event),
        SessionRecordType::AgentRunExecutionEnded => validate_agent_run_execution_ended(event),
        SessionRecordType::UserMessage => validate_user_message(event),
        SessionRecordType::TurnSupplement => validate_turn_supplement(event),
        SessionRecordType::AssistantMessage => validate_assistant_message(event),
        SessionRecordType::ToolCall => validate_tool_call(event),
        SessionRecordType::ToolResult => validate_tool_result(event),
        SessionRecordType::ModelRequestStarted => validate_model_request_started(event),
        SessionRecordType::ProviderUsage => validate_provider_usage(event),
        SessionRecordType::PhaseEvent => validate_phase_event(event),
        SessionRecordType::ExternalEvidenceRef => validate_external_evidence_ref(event),
        SessionRecordType::FileFact => validate_file_fact(event),
        SessionRecordType::CitationRecorded => validate_citation_recorded(event),
        SessionRecordType::ArtifactPublished => validate_artifact_published(event),
        SessionRecordType::Compaction => validate_compaction(event),
        SessionRecordType::CheckpointRef => validate_checkpoint_ref(event),
        SessionRecordType::Tombstone => validate_tombstone(event),
        SessionRecordType::AgentRunCompleted => validate_agent_run_terminal(event),
        SessionRecordType::AgentRunFailed => validate_agent_run_failed(event),
        SessionRecordType::AgentRunInterrupted => validate_agent_run_interrupted(event),
    }
}

pub fn validate_event_log(
    expected_session_id: &str,
    values: &[Value],
) -> Result<SessionProjection, String> {
    let events = values
        .iter()
        .map(parse_event)
        .collect::<Result<Vec<_>, _>>()?;
    reduce_events(expected_session_id, events.iter())
}

pub fn reduce_events<'a>(
    expected_session_id: &str,
    events: impl IntoIterator<Item = &'a SessionLogRecord>,
) -> Result<SessionProjection, String> {
    let expected_session_id = expected_session_id.trim();
    if expected_session_id.is_empty() {
        return Err("expected sessionId is required".to_string());
    }

    let events = events.into_iter().collect::<Vec<_>>();
    let tombstoned_event_ids = active_tombstone_targets(expected_session_id, &events)?;
    let mut projection = SessionProjection {
        tombstoned_event_ids: tombstoned_event_ids.clone(),
        ..SessionProjection::default()
    };
    let mut seen_event_ids = HashSet::<String>::new();
    let mut open_tool_call_ids = HashMap::<String, (String, String, String)>::new();

    for event in events {
        if event.event_type == SessionRecordType::Tombstone
            || tombstoned_event_ids.contains(event.event_id.as_str())
        {
            continue;
        }
        reduce_event(
            expected_session_id,
            &mut projection,
            &mut seen_event_ids,
            &mut open_tool_call_ids,
            event,
        )?;
    }

    Ok(projection)
}

pub fn active_session_records(
    expected_session_id: &str,
    events: &[SessionLogRecord],
) -> Result<Vec<SessionLogRecord>, String> {
    let references = events.iter().collect::<Vec<_>>();
    let targets = active_tombstone_targets(expected_session_id, references.as_slice())?;
    Ok(events
        .iter()
        .filter(|event| {
            event.event_type != SessionRecordType::Tombstone
                && !targets.contains(event.event_id.as_str())
        })
        .cloned()
        .collect())
}

pub fn restore_runtime_snapshot_from_session_records(
    expected_session_id: &str,
    events: &[SessionLogRecord],
) -> Result<SessionStateSnapshot, String> {
    let active = active_session_records(expected_session_id, events)?;
    let projection = reduce_events(expected_session_id, active.iter())?;
    let latest_request_index = active.iter().rposition(|event| {
        event.event_type == SessionRecordType::ModelRequestStarted
            && event.payload.get("purpose").and_then(Value::as_str) == Some("main")
    });
    let mut snapshot = SessionStateSnapshot::new(
        expected_session_id.to_string(),
        active
            .last()
            .map(|event| event.created_at_ms)
            .unwrap_or_default(),
    );

    let tail_start = if let Some(request_index) = latest_request_index {
        let request = &active[request_index];
        let observations = serde_json::from_value::<Vec<ModelObservationV1>>(
            request
                .payload
                .get("observations")
                .expect("validated model request observations")
                .clone(),
        )
        .map_err(|error| format!("decode Session model observations failed: {error}"))?;
        let observed_messages = observations
            .iter()
            .filter_map(|observation| match observation {
                ModelObservationV1::ContextMessage { message } => {
                    Some((message.clone(), request.created_at_ms))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut observed_image_sources =
            HashMap::<String, Vec<crate::model::prepared_prompt::ModelInputImageSourceRefV1>>::new(
            );
        for image in observations
            .iter()
            .filter_map(|observation| match observation {
                ModelObservationV1::InputImage { image } => Some(image),
                _ => None,
            })
        {
            observed_image_sources
                .entry(image.message_id.clone())
                .or_default()
                .push(image.source.clone());
        }
        let observed_tool_result_ids = observed_messages
            .iter()
            .filter_map(|(message, _)| {
                (message.role == ModelMessageRoleV1::Tool)
                    .then(|| message.tool_call_id.clone())
                    .flatten()
            })
            .collect::<HashSet<_>>();
        for (message, created_at_ms) in observed_messages {
            let missing_tool_result_ids = if message.role == ModelMessageRoleV1::Assistant {
                message
                    .tool_calls
                    .iter()
                    .filter(|call| !observed_tool_result_ids.contains(call.id.as_str()))
                    .map(|call| call.id.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            append_observed_model_message(&mut snapshot, message, created_at_ms, &projection)?;
            for call_id in missing_tool_result_ids {
                let Some(result_event) = active[..request_index].iter().find(|event| {
                    event.event_type == SessionRecordType::ToolResult
                        && event.payload.get("callId").and_then(Value::as_str)
                            == Some(call_id.as_str())
                }) else {
                    continue;
                };
                let payload = payload_object(result_event)?;
                append_observed_model_message(
                    &mut snapshot,
                    ModelMessageV1 {
                        message_id: format!(
                            "session_restore:{}:tool:{call_id}",
                            result_event.event_id
                        ),
                        role: ModelMessageRoleV1::Tool,
                        content: required_payload_string_allow_empty(
                            payload,
                            "modelContent",
                            result_event,
                        )?,
                        tool_calls: Vec::new(),
                        tool_call_id: Some(call_id),
                        reasoning_content: None,
                    },
                    result_event.created_at_ms,
                    &projection,
                )?;
            }
        }
        for (message_id, sources) in observed_image_sources {
            let message = snapshot
                .messages
                .iter_mut()
                .find(|message| message.message_id == message_id)
                .ok_or_else(|| {
                    format!(
                        "Session model input image observation message is missing: {message_id}"
                    )
                })?;
            message.metadata.insert(
                crate::runtime::keys::metadata::MODEL_INPUT_IMAGE_SOURCES.to_string(),
                serde_json::to_string(&sources).map_err(|error| {
                    format!("encode restored model input image sources failed: {error}")
                })?,
            );
        }
        request_index.saturating_add(1)
    } else {
        0
    };

    let mut assistant_tool_call_ids = snapshot
        .model_semantics
        .values()
        .filter_map(|semantics| match semantics {
            ModelMessageSemanticsV1::Assistant { tool_calls, .. } => Some(tool_calls),
            _ => None,
        })
        .flatten()
        .map(|call| call.id.clone())
        .collect::<HashSet<_>>();
    let mut tool_result_ids = snapshot
        .model_semantics
        .values()
        .filter_map(|semantics| match semantics {
            ModelMessageSemanticsV1::ToolResult { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut emitted_tool_turns = HashSet::<(String, String)>::new();

    for event in &active[tail_start..] {
        match event.event_type {
            SessionRecordType::UserMessage => append_plain_session_message(
                &mut snapshot,
                event,
                "messageId",
                "text",
                MessageRole::User,
            )?,
            SessionRecordType::TurnSupplement => append_plain_session_message(
                &mut snapshot,
                event,
                "messageId",
                "message",
                MessageRole::User,
            )?,
            SessionRecordType::AssistantMessage => append_plain_session_message(
                &mut snapshot,
                event,
                "messageId",
                "modelMarkdown",
                MessageRole::Assistant,
            )?,
            SessionRecordType::ToolCall => {
                let turn_id = required_event_turn_id(event)?;
                let agent_run_id = required_event_agent_run_id(event)?;
                let turn_key = (agent_run_id.clone(), turn_id.clone());
                if emitted_tool_turns.contains(&turn_key) {
                    continue;
                }
                let tool_calls = active[tail_start..]
                    .iter()
                    .filter(|candidate| {
                        candidate.event_type == SessionRecordType::ToolCall
                            && candidate.turn_id.as_deref() == Some(turn_id.as_str())
                            && candidate.agent_run_id.as_deref() == Some(agent_run_id.as_str())
                    })
                    .map(|candidate| {
                        let payload = payload_object(candidate)?;
                        let call_id = required_payload_string(payload, "callId", candidate)?;
                        let tool_name = required_payload_string(payload, "toolName", candidate)?;
                        let args_json = serde_json::to_string(
                            payload
                                .get("normalizedInput")
                                .expect("validated normalizedInput"),
                        )
                        .map_err(|error| format!("encode restored tool input failed: {error}"))?;
                        Ok(ModelToolCallStateV1 {
                            id: call_id,
                            name: tool_name,
                            args_json,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?
                    .into_iter()
                    .filter(|call| !assistant_tool_call_ids.contains(call.id.as_str()))
                    .collect::<Vec<_>>();
                if !tool_calls.is_empty() {
                    let message_id =
                        format!("session_restore:{agent_run_id}:{turn_id}:assistant_tools");
                    append_restored_message(
                        &mut snapshot,
                        ChatMessage {
                            message_id: message_id.clone(),
                            role: MessageRole::Assistant,
                            content: String::new(),
                            created_at_ms: event.created_at_ms,
                            metadata: HashMap::new(),
                        },
                        ModelMessageSemanticsV1::Assistant {
                            reasoning_content: None,
                            tool_calls: tool_calls.clone(),
                        },
                    )?;
                    assistant_tool_call_ids.extend(tool_calls.iter().map(|call| call.id.clone()));
                    for call in &tool_calls {
                        if let Some(result_event) = active[tail_start..].iter().find(|candidate| {
                            candidate.event_type == SessionRecordType::ToolResult
                                && candidate.payload.get("callId").and_then(Value::as_str)
                                    == Some(call.id.as_str())
                        }) {
                            append_restored_tool_result(
                                &mut snapshot,
                                result_event,
                                &mut tool_result_ids,
                            )?;
                        }
                    }
                }
                emitted_tool_turns.insert(turn_key);
            }
            SessionRecordType::ToolResult => {
                append_restored_tool_result(&mut snapshot, event, &mut tool_result_ids)?;
            }
            _ => {}
        }
    }

    for event in active
        .iter()
        .filter(|event| event.event_type == SessionRecordType::UserMessage)
    {
        let payload = payload_object(event)?;
        let message_id = required_payload_string(payload, "messageId", event)?;
        let images = model_input_image_refs(payload)?;
        if images.is_empty() {
            continue;
        }
        if let Some(message) = snapshot
            .messages
            .iter_mut()
            .find(|message| message.message_id == message_id)
        {
            message.metadata.insert(
                crate::runtime::keys::metadata::MODEL_INPUT_IMAGES.to_string(),
                serde_json::to_string(&images).map_err(|error| {
                    format!("encode restored model input images failed: {error}")
                })?,
            );
        }
    }
    snapshot.context_window = snapshot.messages.clone();
    Ok(snapshot)
}

fn append_restored_tool_result(
    snapshot: &mut SessionStateSnapshot,
    event: &SessionLogRecord,
    tool_result_ids: &mut HashSet<String>,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let call_id = required_payload_string(payload, "callId", event)?;
    if tool_result_ids.contains(call_id.as_str()) {
        return Ok(());
    }
    let tool_name = required_payload_string(payload, "toolName", event)?;
    let result_state = required_payload_string(payload, "resultState", event)?;
    let message_id = format!("session_restore:{}:tool:{call_id}", event.event_id);
    append_restored_message(
        snapshot,
        ChatMessage {
            message_id: message_id.clone(),
            role: MessageRole::Tool,
            content: required_payload_string_allow_empty(payload, "modelContent", event)?,
            created_at_ms: event.created_at_ms,
            metadata: {
                let mut metadata = HashMap::new();
                let sources = serde_json::from_value::<
                    Vec<crate::model::prepared_prompt::ModelInputImageSourceRefV1>,
                >(
                    payload.get("modelInputImages").cloned().ok_or_else(|| {
                        format!(
                            "session.event.v1 {} payload.modelInputImages is required",
                            event.event_id
                        )
                    })?,
                )
                .map_err(|error| {
                    format!(
                        "session.event.v1 {} payload.modelInputImages is invalid: {error}",
                        event.event_id
                    )
                })?;
                if !sources.is_empty() {
                    metadata.insert(
                        crate::runtime::keys::metadata::MODEL_INPUT_IMAGE_SOURCES.to_string(),
                        serde_json::to_string(&sources).map_err(|error| {
                            format!("encode restored model input image sources failed: {error}")
                        })?,
                    );
                }
                metadata
            },
        },
        ModelMessageSemanticsV1::ToolResult {
            tool_call_id: call_id.clone(),
            tool_name,
            status: if parse_result_state(payload, event)?.is_success() {
                "ok".to_string()
            } else {
                "error".to_string()
            },
            result_state,
            error_kind: None,
            object_refs: Vec::new(),
            transition_reason: None,
        },
    )?;
    tool_result_ids.insert(call_id);
    Ok(())
}

fn append_observed_model_message(
    snapshot: &mut SessionStateSnapshot,
    message: ModelMessageV1,
    created_at_ms: i64,
    projection: &SessionProjection,
) -> Result<(), String> {
    let role = match message.role {
        ModelMessageRoleV1::System => MessageRole::System,
        ModelMessageRoleV1::User => MessageRole::User,
        ModelMessageRoleV1::Assistant => MessageRole::Assistant,
        ModelMessageRoleV1::Tool => MessageRole::Tool,
    };
    let semantics = match role {
        MessageRole::Assistant => ModelMessageSemanticsV1::Assistant {
            reasoning_content: message.reasoning_content,
            tool_calls: message
                .tool_calls
                .into_iter()
                .map(|call| ModelToolCallStateV1 {
                    id: call.id,
                    name: call.name,
                    args_json: call.args_json,
                })
                .collect(),
        },
        MessageRole::Tool => {
            let call_id = message.tool_call_id.clone().ok_or_else(|| {
                format!(
                    "Session model observation tool message is missing toolCallId: {}",
                    message.message_id
                )
            })?;
            let tool_call = projection.tool_calls.get(call_id.as_str()).ok_or_else(|| {
                format!("Session model observation tool message has no durable ToolCall: {call_id}")
            })?;
            ModelMessageSemanticsV1::ToolResult {
                tool_call_id: call_id,
                tool_name: tool_call.tool_name.clone(),
                status: if tool_call
                    .result_state
                    .is_some_and(ToolResultState::is_success)
                {
                    "ok".to_string()
                } else {
                    "error".to_string()
                },
                result_state: tool_call
                    .result_state
                    .map(|state| state.as_str().to_string())
                    .unwrap_or_else(|| "failed".to_string()),
                error_kind: None,
                object_refs: Vec::new(),
                transition_reason: None,
            }
        }
        MessageRole::System | MessageRole::User => ModelMessageSemanticsV1::Plain,
    };
    append_restored_message(
        snapshot,
        ChatMessage {
            message_id: message.message_id,
            role,
            content: message.content,
            created_at_ms,
            metadata: HashMap::new(),
        },
        semantics,
    )
}

fn append_plain_session_message(
    snapshot: &mut SessionStateSnapshot,
    event: &SessionLogRecord,
    message_id_field: &str,
    content_field: &str,
    role: MessageRole,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let message_id = required_payload_string(payload, message_id_field, event)?;
    if snapshot
        .messages
        .iter()
        .any(|message| message.message_id == message_id)
    {
        return Ok(());
    }
    let mut metadata = HashMap::new();
    if role == MessageRole::User {
        let images = model_input_image_refs(payload)?;
        if !images.is_empty() {
            metadata.insert(
                crate::runtime::keys::metadata::MODEL_INPUT_IMAGES.to_string(),
                serde_json::to_string(&images).map_err(|error| {
                    format!("encode restored model input images failed: {error}")
                })?,
            );
        }
    }
    append_restored_message(
        snapshot,
        ChatMessage {
            message_id,
            role,
            content: required_payload_string_allow_empty(payload, content_field, event)?,
            created_at_ms: event.created_at_ms,
            metadata,
        },
        ModelMessageSemanticsV1::Plain,
    )
}

fn model_input_image_refs(
    payload: &serde_json::Map<String, Value>,
) -> Result<Vec<crate::model::prepared_prompt::ModelInputImageRefV1>, String> {
    payload
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|attachment| {
            serde_json::from_value::<MessageAttachmentRef>(attachment.clone())
                .map_err(|error| format!("decode model input image attachment failed: {error}"))
        })
        .filter_map(|attachment| match attachment {
            Ok(attachment) => attachment.placeholder.map(|placeholder| {
                Ok(crate::model::prepared_prompt::ModelInputImageRefV1 {
                    input_ref: attachment.input_ref,
                    content_type: attachment.content_type,
                    placeholder,
                })
            }),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn append_restored_message(
    snapshot: &mut SessionStateSnapshot,
    message: ChatMessage,
    semantics: ModelMessageSemanticsV1,
) -> Result<(), String> {
    if snapshot
        .model_semantics
        .insert(message.message_id.clone(), semantics)
        .is_some()
    {
        return Err(format!(
            "Session runtime restore duplicates messageId: {}",
            message.message_id
        ));
    }
    snapshot.messages.push(message);
    Ok(())
}

fn active_tombstone_targets(
    expected_session_id: &str,
    events: &[&SessionLogRecord],
) -> Result<HashSet<String>, String> {
    let mut prior_types = HashMap::<String, SessionRecordType>::new();
    let mut targets = HashSet::new();
    for event in events {
        validate_event_shape(event)?;
        if event.session_id != expected_session_id {
            return Err(format!(
                "session.event.v1 cross-session event {} belongs to {} but expected {}",
                event.event_id, event.session_id, expected_session_id
            ));
        }
        if prior_types.contains_key(event.event_id.as_str()) {
            return Err(format!(
                "session.event.v1 duplicate eventId: {}",
                event.event_id
            ));
        }
        if event.event_type == SessionRecordType::Tombstone {
            for target in required_payload_array(payload_object(event)?, "targetEventIds", event)? {
                let target = target.as_str().ok_or_else(|| {
                    format!(
                        "session.event.v1 {} payload.targetEventIds must contain strings",
                        event.event_id
                    )
                })?;
                let target_type = prior_types.get(target).ok_or_else(|| {
                    format!(
                        "session.event.v1 {} tombstone target must reference a prior event: {}",
                        event.event_id, target
                    )
                })?;
                if *target_type == SessionRecordType::Tombstone {
                    return Err(format!(
                        "session.event.v1 {} must not tombstone another tombstone",
                        event.event_id
                    ));
                }
                if !targets.insert(target.to_string()) {
                    return Err(format!(
                        "session.event.v1 {} tombstone target is already inactive: {}",
                        event.event_id, target
                    ));
                }
            }
        }
        prior_types.insert(event.event_id.clone(), event.event_type);
    }
    Ok(targets)
}

/// 增量 reduce：把单条事件应用到既有 projection（供 append 增量校验复用）。
/// 与 `reduce_events` 逐事件行为完全等价；`seen_event_ids` 必须是跨调用持续维护的集合。
pub fn reduce_event(
    expected_session_id: &str,
    projection: &mut SessionProjection,
    seen_event_ids: &mut HashSet<String>,
    open_tool_call_ids: &mut HashMap<String, (String, String, String)>,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let expected_session_id = expected_session_id.trim();
    validate_event_shape(event)?;
    if event.session_id != expected_session_id {
        return Err(format!(
            "session.event.v1 cross-session event {} belongs to {} but expected {}",
            event.event_id, event.session_id, expected_session_id
        ));
    }
    if !seen_event_ids.insert(event.event_id.clone()) {
        return Err(format!(
            "session.event.v1 duplicate eventId: {}",
            event.event_id
        ));
    }

    match event.event_type {
        SessionRecordType::AgentRunStarted => reduce_agent_run_started(projection, event)?,
        SessionRecordType::AgentRunExecutionStarted => {
            reduce_agent_run_execution_started(projection, event)?
        }
        SessionRecordType::AgentRunExecutionEnded => {
            reduce_agent_run_execution_ended(projection, event)?
        }
        SessionRecordType::UserMessage => reduce_user_message(projection, event)?,
        SessionRecordType::TurnSupplement => reduce_turn_supplement(projection, event)?,
        SessionRecordType::AssistantMessage => reduce_assistant_message(projection, event)?,
        SessionRecordType::ToolCall => {
            let payload = payload_object(event)?;
            let call_id = required_payload_string(payload, "callId", event)?;
            let tool_name = required_payload_string(payload, "toolName", event)?;
            let turn_id = required_event_turn_id(event)?;
            let agent_run_id = required_event_agent_run_id(event)?;
            open_tool_call_ids.insert(
                call_id.clone(),
                (tool_name.clone(), turn_id.clone(), agent_run_id.clone()),
            );
            projection.tool_calls.insert(
                call_id.clone(),
                ReducedToolCall {
                    call_id,
                    tool_name,
                    turn_id,
                    agent_run_id,
                    normalized_input: payload
                        .get("normalizedInput")
                        .expect("validated normalizedInput")
                        .clone(),
                    result_state: None,
                    summary: None,
                },
            );
        }
        SessionRecordType::ToolResult => reduce_tool_result(projection, open_tool_call_ids, event)?,
        SessionRecordType::ModelRequestStarted => {
            let purpose = event
                .payload
                .get("purpose")
                .and_then(Value::as_str)
                .expect("validated model request purpose");
            let agent_run_id = required_event_agent_run_id(event)?;
            projection.latest_model_request_purpose = Some(purpose.to_string());
            projection.latest_model_request_agent_run_id = Some(agent_run_id.clone());
            if purpose != "main" {
                return Ok(());
            }
            projection.latest_main_context_token_estimate = Some(
                event
                    .payload
                    .get("contextTokenEstimate")
                    .and_then(Value::as_u64)
                    .expect("validated context token estimate"),
            );
            projection.latest_main_context_token_breakdown = Some(
                serde_json::from_value(
                    event
                        .payload
                        .get("contextTokenBreakdown")
                        .cloned()
                        .expect("validated context token breakdown"),
                )
                .expect("validated context token breakdown"),
            );
            projection.context_token_estimate_updated_at_ms = Some(event.created_at_ms);
            let digest = event
                .payload
                .get("agentComposition")
                .and_then(|value| value.get("compositionDigest"))
                .and_then(Value::as_str)
                .expect("validated agent composition digest");
            match projection
                .agent_composition_by_agent_run
                .get(agent_run_id.as_str())
            {
                Some(existing) if existing != digest => {
                    return Err(format!(
                        "session.event.v1 {} changes immutable agent composition for AgentRun {}",
                        event.event_id, agent_run_id
                    ));
                }
                Some(_) => {}
                None => {
                    projection
                        .agent_composition_by_agent_run
                        .insert(agent_run_id, digest.to_string());
                }
            }
        }
        SessionRecordType::ProviderUsage => reduce_provider_usage(projection, event)?,
        SessionRecordType::CitationRecorded => reduce_citation_recorded(projection, event)?,
        SessionRecordType::ArtifactPublished => reduce_artifact_published(projection, event)?,
        SessionRecordType::Tombstone => reduce_tombstone(projection, event)?,
        SessionRecordType::AgentRunCompleted => {
            reduce_terminal_agent_run(projection, event, ReducedAgentRunState::Completed)?
        }
        SessionRecordType::AgentRunFailed => {
            reduce_terminal_agent_run(projection, event, ReducedAgentRunState::Failed)?
        }
        SessionRecordType::AgentRunInterrupted => {
            reduce_terminal_agent_run(projection, event, ReducedAgentRunState::Interrupted)?
        }
        SessionRecordType::SessionMeta
        | SessionRecordType::PhaseEvent
        | SessionRecordType::ExternalEvidenceRef
        | SessionRecordType::FileFact
        | SessionRecordType::Compaction
        | SessionRecordType::CheckpointRef => {}
    }
    Ok(())
}

fn validate_artifact_published(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "publicationId",
            "artifactRef",
            "toolCallId",
            "filename",
            "sizeBytes",
            "sha256",
        ],
        event,
    )?;
    for name in [
        "publicationId",
        "artifactRef",
        "toolCallId",
        "filename",
        "sha256",
    ] {
        required_payload_string(payload, name, event)?;
    }
    let publication_id = required_payload_string(payload, "publicationId", event)?;
    let publication_hash = publication_id.strip_prefix("pub_").unwrap_or_default();
    let artifact_ref = required_payload_string(payload, "artifactRef", event)?;
    let artifact_id = artifact_ref.strip_prefix("artifact:").unwrap_or_default();
    let sha256 = required_payload_string(payload, "sha256", event)?;
    let hash = sha256.strip_prefix("sha256:").unwrap_or_default();
    let filename = required_payload_string(payload, "filename", event)?;
    let size_bytes = payload
        .get("sizeBytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} artifact sizeBytes is invalid",
                event.event_id
            )
        })?;
    if publication_hash.len() != 64
        || hash.len() != 64
        || publication_hash
            .bytes()
            .chain(hash.bytes())
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        || artifact_id.is_empty()
        || filename.len() > 255
        || filename.contains('/')
        || filename.contains('\\')
        || size_bytes > MAX_PUBLISHED_ARTIFACT_BYTES
    {
        return Err(format!(
            "session.event.v1 {} artifact publication payload is invalid",
            event.event_id
        ));
    }
    Ok(())
}

fn reduce_artifact_published(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let tool_call_id = required_payload_string(payload, "toolCallId", event)?;
    let call = projection
        .tool_calls
        .get(tool_call_id.as_str())
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} artifact publication has no tool call",
                event.event_id
            )
        })?;
    if call.turn_id != required_event_turn_id(event)?
        || call.agent_run_id != required_event_agent_run_id(event)?
    {
        return Err(format!(
            "session.event.v1 {} artifact publication tool belongs to another turn/task",
            event.event_id
        ));
    }
    if !call.result_state.is_some_and(ToolResultState::is_success) {
        return Err(format!(
            "session.event.v1 {} artifact publication tool result is not successful",
            event.event_id
        ));
    }
    let publication = ReducedArtifact {
        publication_id: required_payload_string(payload, "publicationId", event)?,
        artifact_ref: required_payload_string(payload, "artifactRef", event)?,
        tool_call_id,
        filename: required_payload_string(payload, "filename", event)?,
        size_bytes: payload["sizeBytes"]
            .as_u64()
            .expect("validated artifact sizeBytes"),
        sha256: required_payload_string(payload, "sha256", event)?,
    };
    match projection
        .artifacts
        .get(publication.publication_id.as_str())
    {
        Some(existing) if existing == &publication => Ok(()),
        Some(_) => Err(format!(
            "session.event.v1 {} artifact publication identity conflict",
            event.event_id
        )),
        None => {
            if !projection
                .artifact_order
                .contains(&publication.artifact_ref)
            {
                projection
                    .artifact_order
                    .push(publication.artifact_ref.clone());
            }
            projection
                .artifacts
                .insert(publication.publication_id.clone(), publication);
            Ok(())
        }
    }
}

fn validate_citation_recorded(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    let payload = payload_object(event)?;
    for name in [
        "citationId",
        "inputRef",
        "ownerRef",
        "ownerKind",
        "displayName",
        "evidenceKind",
        "ownerSha256",
        "sourceToolCallId",
        "sourceToolName",
    ] {
        required_payload_string(payload, name, event)?;
    }
    let owner_kind = required_payload_string(payload, "ownerKind", event)?;
    if !matches!(
        owner_kind.as_str(),
        "sourceObject" | "userLibraryObject" | "artifact"
    ) {
        return Err(format!(
            "session.event.v1 {} citation ownerKind is unsupported: {}",
            event.event_id, owner_kind
        ));
    }
    let evidence_kind = required_payload_string(payload, "evidenceKind", event)?;
    if !matches!(
        evidence_kind.as_str(),
        "workspaceSource" | "userProvided" | "generatedArtifact"
    ) {
        return Err(format!(
            "session.event.v1 {} citation evidenceKind is unsupported: {}",
            event.event_id, evidence_kind
        ));
    }
    let citation_id = required_payload_string(payload, "citationId", event)?;
    let citation_hash = citation_id.strip_prefix("citation:").unwrap_or_default();
    let owner_sha256 = required_payload_string(payload, "ownerSha256", event)?;
    let owner_hash = owner_sha256.strip_prefix("sha256:").unwrap_or_default();
    if citation_hash.len() != 64
        || owner_hash.len() != 64
        || citation_hash
            .bytes()
            .chain(owner_hash.bytes())
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "session.event.v1 {} citation hashes are invalid",
            event.event_id
        ));
    }
    let locator_value = payload.get("locator").ok_or_else(|| {
        format!(
            "session.event.v1 {} citation locator is required",
            event.event_id
        )
    })?;
    let has_knowledge_locator = locator_value.get("kind").is_some();
    if has_knowledge_locator {
        let locator = serde_json::from_value::<KnowledgeLocatorV1>(locator_value.clone()).map_err(
            |error| {
                format!(
                    "session.event.v1 {} knowledge citation locator is invalid: {error}",
                    event.event_id
                )
            },
        )?;
        locator.validate()?;
    } else {
        validate_direct_citation_locator(locator_value, event)?;
    }
    let derived_fields = [
        "ownerGeneration",
        "representationId",
        "specDigest",
        "evidenceSha256",
    ];
    if has_knowledge_locator
        || derived_fields
            .iter()
            .any(|name| payload.contains_key(*name))
    {
        if !derived_fields
            .iter()
            .all(|name| payload.contains_key(*name))
        {
            return Err(format!(
                "session.event.v1 {} knowledge citation identity is incomplete",
                event.event_id
            ));
        }
        if payload["ownerGeneration"]
            .as_u64()
            .is_none_or(|value| value == 0)
        {
            return Err(format!(
                "session.event.v1 {} knowledge citation ownerGeneration is invalid",
                event.event_id
            ));
        }
        for (name, prefix) in [
            ("representationId", "representation:sha256:"),
            ("specDigest", "sha256:"),
            ("evidenceSha256", "sha256:"),
        ] {
            let value = required_payload_string(payload, name, event)?;
            let digest = value.strip_prefix(prefix).unwrap_or_default();
            if digest.len() != 64
                || digest
                    .bytes()
                    .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
            {
                return Err(format!(
                    "session.event.v1 {} knowledge citation {name} is invalid",
                    event.event_id
                ));
            }
        }
    }
    Ok(())
}

fn validate_direct_citation_locator(
    locator_value: &Value,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let locator = locator_value.as_object().ok_or_else(|| {
        format!(
            "session.event.v1 {} citation locator is required",
            event.event_id
        )
    })?;
    let range = if locator.len() == 2
        && locator.contains_key("startLine")
        && locator.contains_key("endLine")
    {
        (
            locator.get("startLine").and_then(Value::as_u64),
            locator.get("endLine").and_then(Value::as_u64),
        )
    } else if locator.len() == 2
        && locator.contains_key("pageStart")
        && locator.contains_key("pageEnd")
    {
        (
            locator.get("pageStart").and_then(Value::as_u64),
            locator.get("pageEnd").and_then(Value::as_u64),
        )
    } else {
        return Err(format!(
            "session.event.v1 {} citation locator must contain exactly one line/page range",
            event.event_id
        ));
    };
    if !matches!(range, (Some(start), Some(end)) if start > 0 && start <= end) {
        return Err(format!(
            "session.event.v1 {} citation locator range is invalid",
            event.event_id
        ));
    }
    Ok(())
}

fn validate_session_meta(event: &SessionLogRecord) -> Result<(), String> {
    serde_json::from_value::<SessionMetadataV1>(event.payload.clone())
        .map_err(|error| {
            format!(
                "session.event.v1 {} session_meta payload is invalid: {error}",
                event.event_id
            )
        })?
        .validate()
}

fn validate_agent_run_started(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["userObjective"], event)?;
    required_payload_string(payload, "userObjective", event)?;
    Ok(())
}

fn validate_agent_run_execution_started(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "executionId",
            "authorizationDigest",
            "recoveredFromCheckpointId",
        ],
        event,
    )?;
    required_payload_string(payload, "executionId", event)?;
    optional_payload_string(payload, "recoveredFromCheckpointId", event)?;
    let digest = required_payload_string(payload, "authorizationDigest", event)?;
    validate_sha256_value(digest.as_str(), "authorizationDigest", event)
}

fn validate_agent_run_execution_ended(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "executionId",
            "outcome",
            "reasonCode",
            "retryable",
            "lastCheckpointId",
            "indeterminateToolCallIds",
        ],
        event,
    )?;
    required_payload_string(payload, "executionId", event)?;
    let outcome = required_payload_string(payload, "outcome", event)?;
    if !matches!(
        outcome.as_str(),
        "completed" | "failed" | "lost" | "cancelled"
    ) {
        return Err(format!(
            "session.event.v1 {} payload.outcome is unsupported: {outcome}",
            event.event_id
        ));
    }
    required_payload_string(payload, "reasonCode", event)?;
    required_payload_bool(payload, "retryable", event)?;
    optional_payload_string(payload, "lastCheckpointId", event)?;
    let tool_call_ids = required_payload_array(payload, "indeterminateToolCallIds", event)?;
    let mut seen = HashSet::new();
    for value in tool_call_ids {
        let id = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!(
                    "session.event.v1 {} payload.indeterminateToolCallIds must contain non-empty strings",
                    event.event_id
                )
            })?;
        if !seen.insert(id) {
            return Err(format!(
                "session.event.v1 {} payload.indeterminateToolCallIds contains duplicates",
                event.event_id
            ));
        }
    }
    Ok(())
}

fn validate_user_message(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["messageId", "text", "attachments"], event)?;
    required_payload_string(payload, "messageId", event)?;
    let text = required_payload_string(payload, "text", event)?;
    validate_message_attachment_array(payload.get("attachments"), text.as_str(), event)
}

fn validate_turn_supplement(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["supplementId", "messageId", "message"], event)?;
    let supplement_id = required_payload_string(payload, "supplementId", event)?;
    crate::session::supplement::validate_turn_supplement_id(supplement_id.as_str())
        .map_err(|error| error.to_string())?;
    let message = required_payload_string(payload, "message", event)?;
    crate::session::supplement::validate_turn_supplement_message(message.as_str())
        .map_err(|error| error.to_string())?;
    let expected_message_id = format!(
        "message:{}:supplement:{}",
        required_event_turn_id(event)?,
        supplement_id
    );
    if required_payload_string(payload, "messageId", event)? != expected_message_id {
        return Err(format!(
            "session.event.v1 {} turn supplement messageId mismatch",
            event.event_id
        ));
    }
    Ok(())
}

fn validate_message_attachment_array(
    value: Option<&Value>,
    message_text: &str,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    let items = value.as_array().ok_or_else(|| {
        format!(
            "session.event.v1 {} payload.attachments must be an array",
            event.event_id
        )
    })?;
    let mut image_placeholders = HashSet::new();
    for item in items {
        let reference =
            serde_json::from_value::<MessageAttachmentRef>(item.clone()).map_err(|error| {
                format!(
                    "session.event.v1 {} payload.attachments item is invalid: {error}",
                    event.event_id
                )
            })?;
        if reference.input_ref.trim().is_empty()
            || reference.display_name.trim().is_empty()
            || reference.content_type.trim().is_empty()
        {
            return Err(format!(
                "session.event.v1 {} payload.attachments item has an empty field",
                event.event_id
            ));
        }
        if let Some(placeholder) = reference.placeholder.as_deref() {
            if placeholder.trim().is_empty()
                || !reference.content_type.starts_with("image/")
                || message_text.match_indices(placeholder).count() != 1
                || !image_placeholders.insert(placeholder.to_string())
            {
                return Err(format!(
                    "session.event.v1 {} payload.attachments image placeholder is invalid",
                    event.event_id
                ));
            }
        }
    }
    Ok(())
}

fn validate_assistant_message(event: &SessionLogRecord) -> Result<(), String> {
    let turn_id = required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &["messageId", "modelMarkdown", "artifactRefs", "status"],
        event,
    )?;
    let message_id = required_payload_string(payload, "messageId", event)?;
    if message_id != format!("message:{turn_id}:assistant") {
        return Err(format!(
            "session.event.v1 {} assistant messageId does not match turnId",
            event.event_id
        ));
    }
    required_payload_string_allow_empty(payload, "modelMarkdown", event)?;
    let artifact_refs = payload
        .get("artifactRefs")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} payload.artifactRefs must be an array",
                event.event_id
            )
        })?;
    if artifact_refs.len() > 64 {
        return Err(format!(
            "session.event.v1 {} payload.artifactRefs exceeds 64 items",
            event.event_id
        ));
    }
    let mut seen = HashSet::new();
    for value in artifact_refs {
        let reference = value.as_str().unwrap_or_default();
        if reference
            .strip_prefix("artifact:")
            .unwrap_or_default()
            .is_empty()
            || !seen.insert(reference)
        {
            return Err(format!(
                "session.event.v1 {} payload.artifactRefs is invalid",
                event.event_id
            ));
        }
    }
    let status = required_payload_string(payload, "status", event)?;
    if !matches!(status.as_str(), "done" | "error") {
        return Err(format!(
            "session.event.v1 {} payload.status is unsupported: {}",
            event.event_id, status
        ));
    }
    Ok(())
}

fn validate_phase_event(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["stage", "message"], event)?;
    let stage = required_payload_string(payload, "stage", event)?;
    if stage != "model_process_summary" {
        return Err(format!(
            "session.event.v1 {} payload.stage is unsupported: {stage}",
            event.event_id
        ));
    }
    required_payload_string(payload, "message", event)?;
    Ok(())
}

fn validate_tool_call(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "callId",
            "toolName",
            "toolContractDigest",
            "providerId",
            "normalizedInput",
            "displayTarget",
        ],
        event,
    )?;
    required_payload_string(payload, "callId", event)?;
    required_payload_string(payload, "toolName", event)?;
    require_sha256_payload(payload, "toolContractDigest", event)?;
    required_payload_string(payload, "providerId", event)?;
    let display_target = required_payload_string(payload, "displayTarget", event)?;
    if display_target.chars().count() > 256 {
        return Err(format!(
            "session.event.v1 {} payload.displayTarget is too long",
            event.event_id
        ));
    }
    if !payload.get("normalizedInput").is_some_and(Value::is_object) {
        return Err(format!(
            "session.event.v1 {} payload.normalizedInput must be an object",
            event.event_id
        ));
    }
    Ok(())
}

fn validate_external_evidence_ref(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "objectRef",
            "contentType",
            "sha256",
            "byteLength",
            "sourceKind",
            "locator",
        ],
        event,
    )?;
    required_payload_string(payload, "objectRef", event)?;
    required_payload_string(payload, "contentType", event)?;
    require_sha256_payload(payload, "sha256", event)?;
    required_payload_u64(payload, "byteLength", event)?;
    required_payload_string(payload, "sourceKind", event)?;
    required_payload_string(payload, "locator", event)?;
    Ok(())
}

fn validate_tool_result(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "callId",
            "toolName",
            "resultState",
            "modelContent",
            "fullOutputPath",
            "outputStartByte",
            "outputByteLength",
            "outputComplete",
            "summary",
            "operations",
            "modelInputImages",
            "latencyMs",
        ],
        event,
    )?;
    required_payload_string(payload, "callId", event)?;
    required_payload_string(payload, "toolName", event)?;
    let model_content = required_payload_string_allow_empty(payload, "modelContent", event)?;
    let full_output_path = optional_payload_string(payload, "fullOutputPath", event)?;
    let output_start_byte = optional_payload_u64(payload, "outputStartByte", event)?;
    let output_byte_length = required_payload_u64(payload, "outputByteLength", event)?;
    let output_complete = required_payload_bool(payload, "outputComplete", event)?;
    match (
        full_output_path.as_deref(),
        output_start_byte,
        output_complete,
    ) {
        (None, None, true) if output_byte_length == model_content.len() as u64 => {}
        (Some(path), Some(start), true)
            if start > 0
                && output_byte_length > crate::tool::layer::MODEL_TOOL_RESULT_MAX_BYTES as u64
                && is_temporary_tool_result_path(path) => {}
        (None, None, false) => {}
        _ => {
            return Err(format!(
                "session.event.v1 {} tool result capture metadata is inconsistent",
                event.event_id
            ))
        }
    }
    required_payload_string(payload, "summary", event)?;
    parse_result_state(payload, event)?;
    required_payload_array(payload, "operations", event)?;
    let image_sources = serde_json::from_value::<
        Vec<crate::model::prepared_prompt::ModelInputImageSourceRefV1>,
    >(payload.get("modelInputImages").cloned().ok_or_else(|| {
        format!(
            "session.event.v1 {} payload.modelInputImages is required",
            event.event_id
        )
    })?)
    .map_err(|error| {
        format!(
            "session.event.v1 {} payload.modelInputImages is invalid: {error}",
            event.event_id
        )
    })?;
    for source in image_sources {
        source.validate().map_err(|error| {
            format!(
                "session.event.v1 {} payload.modelInputImages is invalid: {error}",
                event.event_id
            )
        })?;
    }
    required_payload_u64(payload, "latencyMs", event)?;
    Ok(())
}

fn is_temporary_tool_result_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let file_name = normalized.rsplit('/').next().unwrap_or_default();
    let digest = file_name
        .strip_prefix("tool-result-")
        .and_then(|value| value.strip_suffix(".log"));
    normalized
        .split('/')
        .any(|part| matches!(part, "agent-tool-results" | ".agent-tool-results"))
        && digest.is_some_and(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn validate_model_observation(
    observation: &ModelObservationV1,
    event: &SessionLogRecord,
) -> Result<(), String> {
    match observation {
        ModelObservationV1::SystemPrompt { content } => {
            if content.trim().is_empty() {
                return Err(format!(
                    "session.event.v1 {} system prompt observation is empty",
                    event.event_id
                ));
            }
        }
        ModelObservationV1::ContextMessage { message }
        | ModelObservationV1::CompactionPrompt { message } => {
            if message.message_id.trim().is_empty() {
                return Err(format!(
                    "session.event.v1 {} model observation messageId is required",
                    event.event_id
                ));
            }
            let fields_valid = match message.role {
                ModelMessageRoleV1::System | ModelMessageRoleV1::User => {
                    message.tool_calls.is_empty()
                        && message.tool_call_id.is_none()
                        && message.reasoning_content.is_none()
                }
                ModelMessageRoleV1::Assistant => {
                    message.tool_call_id.is_none()
                        && message.tool_calls.iter().all(|call| {
                            !call.id.trim().is_empty()
                                && !call.name.trim().is_empty()
                                && !call.args_json.trim().is_empty()
                        })
                }
                ModelMessageRoleV1::Tool => {
                    message.tool_calls.is_empty()
                        && message
                            .tool_call_id
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty())
                        && message.reasoning_content.is_none()
                }
            };
            if !fields_valid
                || matches!(observation, ModelObservationV1::CompactionPrompt { .. })
                    && message.role != ModelMessageRoleV1::User
            {
                return Err(format!(
                    "session.event.v1 {} model observation message role fields are invalid",
                    event.event_id
                ));
            }
        }
        ModelObservationV1::InputImage { image } => {
            if image.message_id.trim().is_empty() {
                return Err(format!(
                    "session.event.v1 {} model observation image messageId is required",
                    event.event_id
                ));
            }
            image.source.validate().map_err(|error| {
                format!(
                    "session.event.v1 {} model observation image source is invalid: {error}",
                    event.event_id
                )
            })?;
        }
        ModelObservationV1::ToolCatalog { tool_definitions } => {
            let mut names = HashSet::new();
            if tool_definitions.is_empty()
                || tool_definitions.iter().any(|definition| {
                    definition.name.trim().is_empty()
                        || definition.description.trim().is_empty()
                        || !definition.input_schema.is_object()
                        || !names.insert(definition.name.as_str())
                })
            {
                return Err(format!(
                    "session.event.v1 {} model observation toolDefinitions are invalid",
                    event.event_id
                ));
            }
        }
    }
    Ok(())
}

fn validate_model_request_started(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "requestId",
            "purpose",
            "loopIndex",
            "toolChoice",
            "maxOutputTokens",
            "promptCacheKey",
            "promptCacheRetention",
            "preparedPromptSchema",
            "contextTokenEstimate",
            "contextTokenBreakdown",
            "agentComposition",
            "observations",
        ],
        event,
    )?;
    required_payload_string(payload, "requestId", event)?;
    let purpose = required_payload_string(payload, "purpose", event)?;
    if !matches!(purpose.as_str(), "main" | "compaction") {
        return Err(format!(
            "session.event.v1 {} payload.purpose is unsupported",
            event.event_id
        ));
    }
    let observations = serde_json::from_value::<Vec<ModelObservationV1>>(
        payload.get("observations").cloned().ok_or_else(|| {
            format!(
                "session.event.v1 {} payload.observations is required",
                event.event_id
            )
        })?,
    )
    .map_err(|error| {
        format!(
            "session.event.v1 {} payload.observations is invalid: {error}",
            event.event_id
        )
    })?;
    for observation in &observations {
        validate_model_observation(observation, event)?;
    }
    if purpose == "compaction" {
        if !matches!(
            observations.as_slice(),
            [ModelObservationV1::CompactionPrompt { .. }]
        ) {
            return Err(format!(
                "session.event.v1 {} compaction observations are invalid",
                event.event_id
            ));
        }
    } else {
        let mut last_rank = 0;
        let mut system_prompts = 0;
        let mut tool_catalogs = 0;
        for observation in &observations {
            let rank = match observation {
                ModelObservationV1::SystemPrompt { .. } => {
                    system_prompts += 1;
                    0
                }
                ModelObservationV1::ContextMessage { .. } => 1,
                ModelObservationV1::InputImage { .. } => 2,
                ModelObservationV1::ToolCatalog { .. } => {
                    tool_catalogs += 1;
                    3
                }
                ModelObservationV1::CompactionPrompt { .. } => {
                    return Err(format!(
                        "session.event.v1 {} main request contains compaction observation",
                        event.event_id
                    ));
                }
            };
            if rank < last_rank || system_prompts > 1 || tool_catalogs > 1 {
                return Err(format!(
                    "session.event.v1 {} model observations are not canonical",
                    event.event_id
                ));
            }
            last_rank = rank;
        }
    }
    required_payload_u64(payload, "loopIndex", event)?;
    let tool_choice = serde_json::from_value::<ModelToolChoice>(
        payload.get("toolChoice").cloned().ok_or_else(|| {
            format!(
                "session.event.v1 {} payload.toolChoice is required",
                event.event_id
            )
        })?,
    )
    .map_err(|error| {
        format!(
            "session.event.v1 {} payload.toolChoice is invalid: {error}",
            event.event_id
        )
    })?;
    if purpose == "compaction" && tool_choice != ModelToolChoice::None {
        return Err(format!(
            "session.event.v1 {} compaction toolChoice must be none",
            event.event_id
        ));
    }
    if required_payload_u64(payload, "maxOutputTokens", event)? == 0 {
        return Err(format!(
            "session.event.v1 {} payload.maxOutputTokens must be positive",
            event.event_id
        ));
    }
    validate_nullable_string(payload, "promptCacheKey", event)?;
    validate_nullable_string(payload, "promptCacheRetention", event)?;
    if required_payload_string(payload, "preparedPromptSchema", event)? != PREPARED_PROMPT_SCHEMA {
        return Err(format!(
            "session.event.v1 {} payload.preparedPromptSchema is unsupported",
            event.event_id
        ));
    }
    let context_token_estimate = required_payload_u64(payload, "contextTokenEstimate", event)?;
    let context_token_breakdown = serde_json::from_value::<ContextTokenBreakdownV1>(
        payload
            .get("contextTokenBreakdown")
            .cloned()
            .ok_or_else(|| {
                format!(
                    "session.event.v1 {} payload.contextTokenBreakdown is required",
                    event.event_id
                )
            })?,
    )
    .map_err(|error| {
        format!(
            "session.event.v1 {} payload.contextTokenBreakdown is invalid: {error}",
            event.event_id
        )
    })?;
    context_token_breakdown.validate(u32::try_from(context_token_estimate).map_err(|_| {
        format!(
            "session.event.v1 {} payload.contextTokenEstimate is outside u32",
            event.event_id
        )
    })?)?;
    let composition = serde_json::from_value::<
        crate::extension::composition::ResolvedAgentCompositionV1,
    >(payload.get("agentComposition").cloned().ok_or_else(|| {
        format!(
            "session.event.v1 {} payload.agentComposition is required",
            event.event_id
        )
    })?)
    .map_err(|error| {
        format!(
            "session.event.v1 {} payload.agentComposition is invalid: {error}",
            event.event_id
        )
    })?;
    crate::extension::composition::validate_resolved_agent_composition(&composition)?;
    Ok(())
}

fn validate_provider_usage(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "inputTokens",
            "outputTokens",
            "totalTokens",
            "promptCacheHitTokens",
            "promptCacheMissTokens",
        ],
        event,
    )?;
    serde_json::from_value::<ProviderTokenUsageV1>(event.payload.clone())
        .map_err(|error| {
            format!(
                "session.event.v1 {} provider_usage payload is invalid: {error}",
                event.event_id
            )
        })?
        .validate()
}

fn validate_compaction(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "compactionId",
            "summaryMessageId",
            "summaryMarkdown",
            "firstKeptMessageId",
            "createdReason",
        ],
        event,
    )?;
    required_payload_string(payload, "compactionId", event)?;
    required_payload_string(payload, "summaryMessageId", event)?;
    required_payload_string(payload, "summaryMarkdown", event)?;
    required_payload_string(payload, "createdReason", event)?;
    match payload.get("firstKeptMessageId") {
        Some(Value::Null) => Ok(()),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(()),
        _ => Err(format!(
            "session.event.v1 {} payload.firstKeptMessageId must be a non-empty string or null",
            event.event_id
        )),
    }
}

fn validate_checkpoint_ref(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "checkpointId",
            "kind",
            "objectRef",
            "status",
            "payloadSha256",
            "payloadByteLength",
            "updatedAtMs",
        ],
        event,
    )?;
    for forbidden_key in [
        "checkpointPayload",
        "checkpoint",
        "snapshot",
        "sessionSnapshot",
        "messages",
        "state",
    ] {
        if payload.contains_key(forbidden_key) {
            return Err(format!(
                "session.event.v1 {} checkpoint_ref must not inline checkpoint payload field {}",
                event.event_id, forbidden_key
            ));
        }
    }
    required_payload_string(payload, "checkpointId", event)?;
    let kind = required_payload_string(payload, "kind", event)?;
    crate::runtime::contracts::CheckpointKindV1::parse(kind.as_str())?;
    required_payload_string(payload, "objectRef", event)?;
    let status = required_payload_string(payload, "status", event)?;
    if !matches!(status.as_str(), "paused_question" | "waiting" | "committed") {
        return Err(format!(
            "session.event.v1 {} checkpoint status is unsupported: {}",
            event.event_id, status
        ));
    }
    require_sha256_payload(payload, "payloadSha256", event)?;
    required_payload_u64(payload, "payloadByteLength", event)?;
    required_payload_u64(payload, "updatedAtMs", event)?;
    Ok(())
}

fn validate_file_fact(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &[
            "schema",
            "toolName",
            "toolCallId",
            "operation",
            "path",
            "targetPath",
            "previousFileHash",
            "readSnapshotHash",
            "fileHash",
            "bytesWritten",
            "addedLines",
            "removedLines",
            "sessionId",
            "executionOwner",
        ],
        event,
    )?;
    if required_payload_string(payload, "schema", event)? != "file_mutation_pre_apply_fact_v1" {
        return Err(format!(
            "session.event.v1 {} file_fact schema is unsupported",
            event.event_id
        ));
    }
    required_payload_string(payload, "toolName", event)?;
    required_payload_string(payload, "toolCallId", event)?;
    let operation = required_payload_string(payload, "operation", event)?;
    if !matches!(operation.as_str(), "create" | "overwrite" | "update") {
        return Err(format!(
            "session.event.v1 {} file_fact operation is unsupported: {}",
            event.event_id, operation
        ));
    }
    required_payload_string(payload, "path", event)?;
    optional_payload_string(payload, "targetPath", event)?;
    for key in ["previousFileHash", "readSnapshotHash", "fileHash"] {
        if let Some(value) = optional_payload_string(payload, key, event)? {
            validate_sha256_value(value.as_str(), key, event)?;
        }
    }
    for key in ["bytesWritten", "addedLines", "removedLines"] {
        optional_payload_u64(payload, key, event)?;
    }
    let session_id = required_payload_string(payload, "sessionId", event)?;
    if session_id != event.session_id {
        return Err(format!(
            "session.event.v1 {} file_fact sessionId mismatch",
            event.event_id
        ));
    }
    required_payload_string(payload, "executionOwner", event)?;
    Ok(())
}

fn validate_tombstone(event: &SessionLogRecord) -> Result<(), String> {
    let payload = payload_object(event)?;
    require_exact_payload_fields(
        payload,
        &["tombstoneId", "targetEventIds", "reasonType"],
        event,
    )?;
    required_payload_string(payload, "tombstoneId", event)?;
    validate_non_empty_string_array(payload, "targetEventIds", event)?;
    required_payload_string(payload, "reasonType", event)?;
    Ok(())
}

fn validate_agent_run_terminal(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["doneReason"], event)?;
    required_payload_string(payload, "doneReason", event)?;
    Ok(())
}

fn validate_agent_run_failed(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["reasonType", "message"], event)?;
    required_payload_string(payload, "reasonType", event)?;
    required_payload_string(payload, "message", event)?;
    Ok(())
}

fn validate_agent_run_interrupted(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["reasonType", "message", "retryable"], event)?;
    let reason_type = required_payload_string(payload, "reasonType", event)?;
    if !matches!(
        reason_type.as_str(),
        "cancelled" | "stopped" | "shutdown" | "provider_interrupted"
    ) {
        return Err(format!(
            "session.event.v1 {} agent_run_interrupted reasonType is unsupported: {}",
            event.event_id, reason_type
        ));
    }
    required_payload_string(payload, "message", event)?;
    required_payload_bool(payload, "retryable", event)?;
    Ok(())
}

fn reduce_agent_run_started(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let turn_id = required_event_turn_id(event)?;
    let agent_run_id = required_event_agent_run_id(event)?;
    projection.agent_runs.insert(
        agent_run_id.clone(),
        ReducedAgentRun {
            agent_run_id,
            initial_turn_id: turn_id,
            state: ReducedAgentRunState::Running,
            reason_type: None,
        },
    );
    Ok(())
}

fn reduce_agent_run_execution_started(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let agent_run_id = required_event_agent_run_id(event)?;
    let agent_run = projection
        .agent_runs
        .get(agent_run_id.as_str())
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} Execution precedes AgentRun start",
                event.event_id
            )
        })?;
    if agent_run.state != ReducedAgentRunState::Running {
        return Err(format!(
            "session.event.v1 {} cannot start Execution for terminal AgentRun",
            event.event_id
        ));
    }
    if projection
        .agent_run_executions
        .values()
        .any(|execution| execution.agent_run_id == agent_run_id && execution.outcome.is_none())
    {
        return Err(format!(
            "session.event.v1 {} AgentRun already has an active Execution",
            event.event_id
        ));
    }
    let payload = payload_object(event)?;
    let execution_id = required_payload_string(payload, "executionId", event)?;
    if projection
        .agent_run_executions
        .contains_key(execution_id.as_str())
    {
        return Err(format!(
            "session.event.v1 {} duplicates executionId {execution_id}",
            event.event_id
        ));
    }
    let recovered_from_checkpoint_id =
        optional_payload_string(payload, "recoveredFromCheckpointId", event)?;
    let prior_executions = projection
        .agent_run_executions
        .values()
        .filter(|execution| execution.agent_run_id == agent_run_id)
        .collect::<Vec<_>>();
    if prior_executions.is_empty() != recovered_from_checkpoint_id.is_none() {
        return Err(format!(
            "session.event.v1 {} Execution recovery lineage is invalid",
            event.event_id
        ));
    }
    if recovered_from_checkpoint_id
        .as_ref()
        .is_some_and(|checkpoint_id| {
            prior_executions.iter().any(|execution| {
                execution.recovered_from_checkpoint_id.as_deref() == Some(checkpoint_id.as_str())
            })
        })
    {
        return Err(format!(
            "session.event.v1 {} recovery checkpoint already started a replacement Execution",
            event.event_id
        ));
    }
    projection.agent_run_executions.insert(
        execution_id.clone(),
        ReducedAgentRunExecution {
            execution_id,
            agent_run_id,
            authorization_digest: required_payload_string(payload, "authorizationDigest", event)?,
            recovered_from_checkpoint_id,
            outcome: None,
            reason_code: None,
            retryable: None,
            last_checkpoint_id: None,
            indeterminate_tool_call_ids: Vec::new(),
        },
    );
    Ok(())
}

fn reduce_agent_run_execution_ended(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let agent_run_id = required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    let execution_id = required_payload_string(payload, "executionId", event)?;
    let execution = projection
        .agent_run_executions
        .get_mut(execution_id.as_str())
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} ends unknown Execution {execution_id}",
                event.event_id
            )
        })?;
    if execution.agent_run_id != agent_run_id || execution.outcome.is_some() {
        return Err(format!(
            "session.event.v1 {} Execution terminal identity is invalid",
            event.event_id
        ));
    }
    execution.outcome = Some(required_payload_string(payload, "outcome", event)?);
    execution.reason_code = Some(required_payload_string(payload, "reasonCode", event)?);
    execution.retryable = Some(required_payload_bool(payload, "retryable", event)?);
    execution.last_checkpoint_id = optional_payload_string(payload, "lastCheckpointId", event)?;
    execution.indeterminate_tool_call_ids =
        required_payload_array(payload, "indeterminateToolCallIds", event)?
            .iter()
            .map(|value| value.as_str().expect("validated tool call id").to_string())
            .collect();
    Ok(())
}

fn reduce_user_message(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let message_id = required_payload_string(payload, "messageId", event)?;
    let text = required_payload_string(payload, "text", event)?;
    projection.messages.insert(
        message_id.clone(),
        ReducedMessage {
            message_id,
            role: ReducedMessageRole::User,
            turn_id: event.turn_id.clone(),
            agent_run_id: event.agent_run_id.clone(),
            text,
            artifact_refs: Vec::new(),
            status: Some("done".to_string()),
            updated_at_ms: event.created_at_ms,
        },
    );
    Ok(())
}

fn reduce_turn_supplement(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let message_id = required_payload_string(payload, "messageId", event)?;
    let text = required_payload_string(payload, "message", event)?;
    if projection.messages.contains_key(message_id.as_str()) {
        return Err(format!(
            "session.event.v1 {} turn supplement messageId is duplicated",
            event.event_id
        ));
    }
    projection.messages.insert(
        message_id.clone(),
        ReducedMessage {
            message_id,
            role: ReducedMessageRole::User,
            turn_id: event.turn_id.clone(),
            agent_run_id: event.agent_run_id.clone(),
            text,
            artifact_refs: Vec::new(),
            status: Some("done".to_string()),
            updated_at_ms: event.created_at_ms,
        },
    );
    Ok(())
}

fn reduce_assistant_message(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let message_id = required_payload_string(payload, "messageId", event)?;
    let agent_run_id = required_event_agent_run_id(event)?;
    if projection.messages.values().any(|message| {
        message.role == ReducedMessageRole::Assistant
            && message.agent_run_id.as_deref() == Some(agent_run_id.as_str())
            && message.message_id != message_id
    }) {
        return Err(format!(
            "session.event.v1 {} task has multiple sealed assistant messages",
            event.event_id
        ));
    }
    let text = required_payload_string_allow_empty(payload, "modelMarkdown", event)?;
    let artifact_refs = payload["artifactRefs"]
        .as_array()
        .expect("validated artifactRefs")
        .iter()
        .map(|value| value.as_str().expect("validated artifact ref").to_string())
        .collect::<Vec<_>>();
    if artifact_refs != projection.artifact_order {
        return Err(format!(
            "session.event.v1 {} assistant artifactRefs do not match published order",
            event.event_id
        ));
    }
    let status = required_payload_string(payload, "status", event)?;
    if let Some(existing) = projection.messages.get(&message_id) {
        if existing.role != ReducedMessageRole::Assistant {
            return Err(format!(
                "session.event.v1 {} assistant messageId collides with a user message",
                event.event_id
            ));
        }
        if existing.turn_id.as_deref() != event.turn_id.as_deref() {
            return Err(format!(
                "session.event.v1 {} assistant message turn identity mismatch",
                event.event_id
            ));
        }
        if existing.agent_run_id.as_deref() != event.agent_run_id.as_deref() {
            return Err(format!(
                "session.event.v1 {} assistant message AgentRun identity mismatch",
                event.event_id
            ));
        }
        if existing
            .status
            .as_deref()
            .is_some_and(|committed| matches!(committed, "done" | "error"))
        {
            return Err(format!(
                "session.event.v1 {} assistant message written after final",
                event.event_id
            ));
        }
    }
    projection.messages.insert(
        message_id.clone(),
        ReducedMessage {
            message_id: message_id.clone(),
            role: ReducedMessageRole::Assistant,
            turn_id: event.turn_id.clone(),
            agent_run_id: event.agent_run_id.clone(),
            text,
            artifact_refs,
            status: Some(status),
            updated_at_ms: event.created_at_ms,
        },
    );
    Ok(())
}

fn reduce_tool_result(
    projection: &mut SessionProjection,
    open_tool_call_ids: &mut HashMap<String, (String, String, String)>,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let call_id = required_payload_string(payload, "callId", event)?;
    let tool_name = required_payload_string(payload, "toolName", event)?;
    let result_state = parse_result_state(payload, event)?;
    let summary = required_payload_string(payload, "summary", event)?;
    let Some((expected_tool_name, turn_id, agent_run_id)) = open_tool_call_ids.remove(&call_id)
    else {
        return Err(format!(
            "session.event.v1 {} tool_result has no matching tool_call for callId {}",
            event.event_id, call_id
        ));
    };
    if expected_tool_name != tool_name {
        return Err(format!(
            "session.event.v1 {} tool_result toolName {} does not match tool_call {}",
            event.event_id, tool_name, expected_tool_name
        ));
    }
    let normalized_input = projection
        .tool_calls
        .get(&call_id)
        .expect("matching tool_call projection")
        .normalized_input
        .clone();
    projection.tool_calls.insert(
        call_id.clone(),
        ReducedToolCall {
            call_id,
            tool_name,
            turn_id,
            agent_run_id,
            normalized_input,
            result_state: Some(result_state),
            summary: Some(summary),
        },
    );
    Ok(())
}

fn reduce_provider_usage(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let turn_id = required_event_turn_id(event)?;
    let agent_run_id = required_event_agent_run_id(event)?;
    let usage = serde_json::from_value::<ProviderTokenUsageV1>(event.payload.clone())
        .expect("validated provider usage");
    if projection
        .provider_usage_by_turn
        .insert((agent_run_id, turn_id.clone()), usage.clone())
        .is_some()
    {
        return Err(format!(
            "session.event.v1 {} duplicates provider_usage for turn {}",
            event.event_id, turn_id
        ));
    }
    projection.session_provider_usage = projection.session_provider_usage.checked_add(&usage)?;
    projection.latest_provider_usage_turn_id = Some(turn_id);
    projection.latest_provider_usage = Some(usage);
    projection.latest_provider_usage_updated_at_ms = Some(event.created_at_ms);
    projection.latest_provider_usage_context_token_estimate =
        projection.latest_main_context_token_estimate;
    Ok(())
}

fn reduce_tombstone(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let target_event_ids = required_payload_array(payload, "targetEventIds", event)?;
    for target in target_event_ids {
        let Some(target_id) = target
            .as_str()
            .map(str::trim)
            .filter(|item| !item.is_empty())
        else {
            return Err(format!(
                "session.event.v1 {} payload.targetEventIds must contain strings",
                event.event_id
            ));
        };
        projection
            .tombstoned_event_ids
            .insert(target_id.to_string());
    }
    Ok(())
}

fn reduce_citation_recorded(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let payload = payload_object(event)?;
    let citation_id = required_payload_string(payload, "citationId", event)?;
    let source_tool_call_id = required_payload_string(payload, "sourceToolCallId", event)?;
    let source_tool_call = projection
        .tool_calls
        .get(source_tool_call_id.as_str())
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} citation has no source tool call",
                event.event_id
            )
        })?;
    if source_tool_call.turn_id != required_event_turn_id(event)?
        || source_tool_call.agent_run_id != required_event_agent_run_id(event)?
    {
        return Err(format!(
            "session.event.v1 {} citation source tool belongs to another turn/task",
            event.event_id
        ));
    }
    let expected_tool_name = required_payload_string(payload, "sourceToolName", event)?;
    if source_tool_call.tool_name != expected_tool_name
        || !source_tool_call
            .result_state
            .is_some_and(ToolResultState::is_success)
    {
        return Err(format!(
            "session.event.v1 {} citation source must be a successful knowledge result",
            event.event_id
        ));
    }
    if projection.citations.contains_key(citation_id.as_str()) {
        return Err(format!(
            "session.event.v1 {} duplicates citationId {}",
            event.event_id, citation_id
        ));
    }
    projection.citations.insert(
        citation_id.clone(),
        ReducedCitation {
            citation_id,
            input_ref: required_payload_string(payload, "inputRef", event)?,
            owner_ref: required_payload_string(payload, "ownerRef", event)?,
            owner_kind: required_payload_string(payload, "ownerKind", event)?,
            display_name: required_payload_string(payload, "displayName", event)?,
            evidence_kind: required_payload_string(payload, "evidenceKind", event)?,
            owner_sha256: required_payload_string(payload, "ownerSha256", event)?,
            locator: payload.get("locator").cloned().ok_or_else(|| {
                format!(
                    "session.event.v1 {} citation locator missing",
                    event.event_id
                )
            })?,
            source_tool_call_id,
        },
    );
    Ok(())
}

fn reduce_terminal_agent_run(
    projection: &mut SessionProjection,
    event: &SessionLogRecord,
    state: ReducedAgentRunState,
) -> Result<(), String> {
    let turn_id = required_event_turn_id(event)?;
    let agent_run_id = required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    if projection
        .agent_run_executions
        .values()
        .any(|execution| execution.agent_run_id == agent_run_id && execution.outcome.is_none())
    {
        return Err(format!(
            "session.event.v1 {} terminal AgentRun has an active Execution",
            event.event_id
        ));
    }
    let reason_type = payload
        .get("reasonType")
        .or_else(|| payload.get("doneReason"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let assistant = projection.messages.values().find(|message| {
        message.role == ReducedMessageRole::Assistant
            && message.agent_run_id.as_deref() == Some(agent_run_id.as_str())
    });
    match (
        state,
        assistant.and_then(|message| message.status.as_deref()),
    ) {
        (ReducedAgentRunState::Completed, Some("done"))
        | (
            ReducedAgentRunState::Failed | ReducedAgentRunState::Interrupted,
            None | Some("done" | "error"),
        ) => {}
        (ReducedAgentRunState::Completed, _) => {
            return Err(format!(
                "session.event.v1 {} completed AgentRun requires one done assistant message",
                event.event_id
            ));
        }
        (ReducedAgentRunState::Failed | ReducedAgentRunState::Interrupted, Some(_)) => {
            return Err(format!(
                "session.event.v1 {} terminal AgentRun assistant state is invalid",
                event.event_id
            ));
        }
        (ReducedAgentRunState::Running, _) => {
            unreachable!("terminal reducer received running state")
        }
    }
    projection.agent_runs.insert(
        agent_run_id.clone(),
        ReducedAgentRun {
            agent_run_id,
            initial_turn_id: turn_id,
            state,
            reason_type,
        },
    );
    Ok(())
}

fn reject_unsupported_event_type(value: &Value) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err("session.event.v1 line must be a JSON object".to_string());
    };
    let Some(item_type) = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if !SessionRecordType::allowed_type_names().contains(&item_type) {
        return Err(format!(
            "unsupported session.event.v1 session event type: {item_type}; allowed: {}",
            SessionRecordType::allowed_type_names().join(", ")
        ));
    }
    Ok(())
}

fn parse_result_state(
    payload: &Map<String, Value>,
    event: &SessionLogRecord,
) -> Result<ToolResultState, String> {
    let Some(value) = payload.get("resultState") else {
        return Err(format!(
            "session.event.v1 {} payload.resultState is required",
            event.event_id
        ));
    };
    serde_json::from_value::<ToolResultState>(value.clone()).map_err(|err| {
        format!(
            "session.event.v1 {} payload.resultState is invalid: {err}",
            event.event_id
        )
    })
}

fn require_payload_object(event: &SessionLogRecord) -> Result<(), String> {
    payload_object(event).map(|_| ())
}

fn payload_object(event: &SessionLogRecord) -> Result<&Map<String, Value>, String> {
    event.payload.as_object().ok_or_else(|| {
        format!(
            "session.event.v1 {} payload must be a JSON object",
            event.event_id
        )
    })
}

fn require_exact_payload_fields(
    payload: &Map<String, Value>,
    expected: &[&str],
    event: &SessionLogRecord,
) -> Result<(), String> {
    if payload.len() != expected.len() || expected.iter().any(|name| !payload.contains_key(*name)) {
        return Err(format!(
            "session.event.v1 {} payload fields mismatch",
            event.event_id
        ));
    }
    Ok(())
}

fn required_event_turn_id(event: &SessionLogRecord) -> Result<String, String> {
    event
        .turn_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("session.event.v1 {} turnId is required", event.event_id))
}

fn required_event_agent_run_id(event: &SessionLogRecord) -> Result<String, String> {
    event
        .agent_run_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("session.event.v1 {} agentRunId is required", event.event_id))
}

fn required_payload_string(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<String, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} payload.{key} is required",
                event.event_id
            )
        })
}

fn require_sha256_payload(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<String, String> {
    let value = required_payload_string(payload, key, event)?;
    validate_sha256_value(value.as_str(), key, event)?;
    Ok(value)
}

fn validate_sha256_value(value: &str, key: &str, event: &SessionLogRecord) -> Result<(), String> {
    let hex = value.strip_prefix("sha256:").unwrap_or_default();
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "session.event.v1 {} payload.{key} must be sha256:<64 lowercase hex>",
            event.event_id
        ));
    }
    Ok(())
}

fn optional_payload_string(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<Option<String>, String> {
    match payload.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        _ => Err(format!(
            "session.event.v1 {} payload.{key} must be a non-empty string or null",
            event.event_id
        )),
    }
}

fn optional_payload_u64(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<Option<u64>, String> {
    match payload.get(key) {
        Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            format!(
                "session.event.v1 {} payload.{key} must be a u64 or null",
                event.event_id
            )
        }),
        None => Err(format!(
            "session.event.v1 {} payload.{key} is required",
            event.event_id
        )),
    }
}

fn required_payload_string_allow_empty(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<String, String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "session.event.v1 {} payload.{key} must be a string",
                event.event_id
            )
        })
}

fn validate_nullable_string(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<(), String> {
    match payload.get(key) {
        Some(Value::Null) => Ok(()),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(()),
        _ => Err(format!(
            "session.event.v1 {} payload.{key} must be a string or null",
            event.event_id
        )),
    }
}

fn required_payload_u64(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<u64, String> {
    payload.get(key).and_then(Value::as_u64).ok_or_else(|| {
        format!(
            "session.event.v1 {} payload.{key} must be a u64",
            event.event_id
        )
    })
}

fn required_payload_bool(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<bool, String> {
    payload.get(key).and_then(Value::as_bool).ok_or_else(|| {
        format!(
            "session.event.v1 {} payload.{key} must be a bool",
            event.event_id
        )
    })
}

fn required_payload_array<'a>(
    payload: &'a Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<&'a Vec<Value>, String> {
    payload.get(key).and_then(Value::as_array).ok_or_else(|| {
        format!(
            "session.event.v1 {} payload.{key} must be an array",
            event.event_id
        )
    })
}

fn validate_string_array(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<(), String> {
    let values = required_payload_array(payload, key, event)?;
    for value in values {
        if value.as_str().is_none() {
            return Err(format!(
                "session.event.v1 {} payload.{key} must contain only strings",
                event.event_id
            ));
        }
    }
    Ok(())
}

fn validate_non_empty_string_array(
    payload: &Map<String, Value>,
    key: &str,
    event: &SessionLogRecord,
) -> Result<(), String> {
    validate_string_array(payload, key, event)?;
    if required_payload_array(payload, key, event)?.is_empty() {
        return Err(format!(
            "session.event.v1 {} payload.{key} must not be empty",
            event.event_id
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn session_event_ids_are_bounded_and_oversized_ids_loud_fail() {
        let component = "x".repeat(512);
        let event_id = stable_session_event_id(
            "banana",
            &[component.as_str(), component.as_str(), component.as_str()],
        );
        assert!(event_id.len() <= SESSION_EVENT_ID_MAX_BYTES);
        assert_eq!(
            event_id,
            stable_session_event_id(
                "banana",
                &[component.as_str(), component.as_str(), component.as_str()]
            )
        );

        let error = canonical_session_record(
            "x".repeat(SESSION_EVENT_ID_MAX_BYTES + 1),
            SessionRecordType::PhaseEvent,
            "session-1",
            Some("turn-1".to_string()),
            Some("agent-run-1".to_string()),
            1,
            json!({}),
        )
        .expect_err("oversized eventId");
        assert_eq!(
            error,
            format!("session.event.v1 eventId exceeds {SESSION_EVENT_ID_MAX_BYTES} bytes")
        );
    }

    #[test]
    fn execution_lifecycle_records_do_not_project_to_agent_run_stream() {
        assert!(!session_record_projects_to_agent_run_stream(
            SessionRecordType::AgentRunExecutionStarted
        ));
        assert!(!session_record_projects_to_agent_run_stream(
            SessionRecordType::AgentRunExecutionEnded
        ));
    }

    #[test]
    fn runtime_restore_keeps_inline_image_refs_without_image_bytes() {
        let duplicate = json!({
            "inputRef": format!("local-image:{}", "b".repeat(64)),
            "displayName": "Image 1",
            "contentType": "image/png",
            "placeholder": "[Image #1]"
        });
        assert!(started_agent_run_records_with_attachments(
            "session-duplicate-image",
            "turn-duplicate-image",
            "agent-run-duplicate-image",
            "inspect [Image #1]",
            vec![duplicate.clone(), duplicate],
            1,
        )
        .is_err());

        let records = started_agent_run_records_with_attachments(
            "session-image",
            "turn-image",
            "agent-run-image",
            "inspect [Image #1]",
            vec![json!({
                "inputRef": format!("local-image:{}", "a".repeat(64)),
                "displayName": "Image 1",
                "contentType": "image/png",
                "placeholder": "[Image #1]"
            })],
            1,
        )
        .expect("image session records");
        let restored =
            restore_runtime_snapshot_from_session_records("session-image", records.as_slice())
                .expect("restore image refs");
        let raw = restored.messages[0]
            .metadata
            .get(crate::runtime::keys::metadata::MODEL_INPUT_IMAGES)
            .expect("image refs metadata");
        assert!(raw.contains("local-image:"));
        assert!(!raw.contains("dataBase64"));
    }
    use crate::model::ToolCallEnvelope;
    use crate::tool::layer::{ToolExecutionFact, ToolExecutionResult};

    fn base_event(event_type: &str, event_id: &str, payload: Value) -> Value {
        json!({
            "schemaVersion": SESSION_EVENT_SCHEMA_VERSION,
            "eventVersion": SESSION_EVENT_VERSION,
            "type": event_type,
            "eventId": event_id,
            "sessionId": "session-1",
            "turnId": "turn-1",
            "agentRunId": "agent-run-1",
            "createdAtMs": 1,
            "payload": payload
        })
    }

    fn model_request_payload(observations: Vec<Value>) -> Value {
        let digest = format!("sha256:{}", "a".repeat(64));
        let composition = crate::extension::composition::resolve_agent_composition(
            crate::extension::composition::AgentCompositionInputsV1 {
                prompt_digest: digest.clone(),
                model_binding: crate::extension::composition::ResolvedModelBindingV1 {
                    provider_id: "test-provider".to_string(),
                    model_name: "test-model".to_string(),
                    wire_protocol: "test-wire".to_string(),
                    config_digest: digest.clone(),
                },
                skill_catalog_digest: digest.clone(),
                plugin_activation_digest: digest.clone(),
                hook_composition_digest: digest.clone(),
                execution_profile_digest: digest,
                policy_version: "test-v1".to_string(),
            },
            std::iter::empty(),
        )
        .expect("composition");
        json!({
            "requestId": "model-request-1",
            "purpose": "main",
            "loopIndex": 0,
            "toolChoice": ModelToolChoice::None,
            "maxOutputTokens": 1024,
            "promptCacheKey": null,
            "promptCacheRetention": null,
            "preparedPromptSchema": PREPARED_PROMPT_SCHEMA,
            "contextTokenEstimate": 0,
            "contextTokenBreakdown": {
                "systemPromptTokens": 0,
                "systemToolTokens": 0,
                "mcpToolTokens": 0,
                "skillsTokens": 0,
                "messageTokens": 0,
                "mcpTools": [],
            },
            "agentComposition": composition,
            "observations": observations,
        })
    }

    #[test]
    fn committed_compaction_projects_a_user_timeline_marker() {
        let record = canonical_session_record(
            "evt-compaction",
            SessionRecordType::Compaction,
            "session-1",
            Some("turn-1".to_string()),
            Some("agent-run-1".to_string()),
            1,
            json!({
                "compactionId": "compaction-1",
                "summaryMessageId": "summary-1",
                "summaryMarkdown": "summary",
                "firstKeptMessageId": null,
                "createdReason": "context_pressure_threshold_reached",
            }),
        )
        .expect("compaction record");
        let SessionStreamProjection::SessionEvent { event, .. } =
            project_committed_session_record(&record, 1).expect("projection");

        assert_eq!(event.event_type, "PromptCompaction");
        assert_eq!(event.status, "done");
        assert_eq!(event.visibility, RuntimeEventVisibility::User);
    }

    fn valid_log() -> Vec<Value> {
        vec![
            json!({
                "schemaVersion": SESSION_EVENT_SCHEMA_VERSION,
                "eventVersion": SESSION_EVENT_VERSION,
                "type": "session_meta",
                "eventId": "evt-session",
                "sessionId": "session-1",
                "createdAtMs": 1,
                "payload": {
                    "recordId": "session:session-1:meta:1",
                    "title": "Session 1",
                    "cwd": "D:/workspace",
                    "sessionKind": "main",
                    "parentSessionId": null,
                    "runtimeJobId": null,
                    "sortOrder": 0,
                    "isPinned": false,
                    "isUnread": false
                }
            }),
            base_event(
                "agent_run_started",
                "evt-turn-started",
                json!({"userObjective": "inspect runtime"}),
            ),
            base_event(
                "user_message",
                "evt-user",
                json!({"messageId": "msg-user", "text": "inspect runtime", "attachments": []}),
            ),
            base_event(
                "tool_call",
                "evt-tool-call",
                json!({
                    "callId": "call-1",
                    "toolName": "read",
                    "toolContractDigest": format!("sha256:{}", "a".repeat(64)),
                    "providerId": "centaeris.builtin",
                    "normalizedInput": {"path": "notice.md"},
                    "displayTarget": "notice.md"
                }),
            ),
            base_event(
                "tool_result",
                "evt-tool-result",
                json!({
                    "callId": "call-1",
                    "toolName": "read",
                    "resultState": "successWithOutput",
                    "modelContent": "notice contents",
                    "fullOutputPath": null,
                    "outputStartByte": null,
                    "outputByteLength": 15,
                    "outputComplete": true,
                    "summary": "read notice",
                    "operations": [],
                    "modelInputImages": [],
                    "latencyMs": 7
                }),
            ),
            base_event(
                "citation_recorded",
                "evt-citation",
                json!({
                    "citationId": "citation:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "inputRef": "input-1",
                    "ownerRef": "source-object-1",
                    "ownerKind": "sourceObject",
                    "displayName": "notice.md",
                    "evidenceKind": "workspaceSource",
                    "ownerSha256": format!("sha256:{}", "a".repeat(64)),
                    "sourceToolName": "read",
                    "sourceToolCallId": "call-1",
                    "locator": {"startLine": 1, "endLine": 8}
                }),
            ),
            base_event(
                "assistant_message",
                "evt-assistant-final",
                json!({
                    "messageId": "message:turn-1:assistant",
                    "modelMarkdown": "done",
                    "artifactRefs": [],
                    "status": "done"
                }),
            ),
            base_event(
                "agent_run_completed",
                "evt-turn-completed",
                json!({"doneReason": "final_response"}),
            ),
        ]
    }

    #[test]
    fn session_meta_rejects_unknown_fields() {
        let mut event = valid_log().remove(0);
        event
            .get_mut("payload")
            .and_then(Value::as_object_mut)
            .expect("session_meta payload")
            .insert("banana".to_string(), Value::Bool(true));
        assert!(parse_event(&event)
            .expect_err("unknown session_meta field must fail")
            .contains("unknown field"));
    }

    #[test]
    fn runtime_restore_uses_latest_main_request_observations_and_replays_tail() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let composition = crate::extension::composition::resolve_agent_composition(
            crate::extension::composition::AgentCompositionInputsV1 {
                prompt_digest: digest.clone(),
                model_binding: crate::extension::composition::ResolvedModelBindingV1 {
                    provider_id: "test-provider".to_string(),
                    model_name: "test-model".to_string(),
                    wire_protocol: "test-wire".to_string(),
                    config_digest: digest.clone(),
                },
                skill_catalog_digest: digest.clone(),
                plugin_activation_digest: digest.clone(),
                hook_composition_digest: digest.clone(),
                execution_profile_digest: digest.clone(),
                policy_version: "test-v1".to_string(),
            },
            std::iter::empty(),
        )
        .expect("composition");
        let mut records = started_agent_run_records(
            "session-restore",
            "turn-root",
            "agent-run-restore",
            "inspect",
            1,
        )
        .expect("started records")
        .to_vec();
        let request_records = |request_id: &str,
                               turn_id: &str,
                               messages: Vec<ModelMessageV1>,
                               created_at_ms: i64|
         -> Vec<SessionLogRecord> {
            vec![canonical_session_record(
                format!("evt:{request_id}"),
                SessionRecordType::ModelRequestStarted,
                "session-restore",
                Some(turn_id.to_string()),
                Some("agent-run-restore".to_string()),
                created_at_ms,
                json!({
                    "requestId": request_id,
                    "purpose": "main",
                    "loopIndex": 0,
                    "toolChoice": ModelToolChoice::Auto,
                    "maxOutputTokens": 1024,
                    "promptCacheKey": null,
                    "promptCacheRetention": null,
                    "preparedPromptSchema": PREPARED_PROMPT_SCHEMA,
                    "contextTokenEstimate": 12,
                    "contextTokenBreakdown": {
                        "systemPromptTokens": 0,
                        "systemToolTokens": 0,
                        "mcpToolTokens": 0,
                        "skillsTokens": 0,
                        "messageTokens": 12,
                        "mcpTools": [],
                    },
                    "agentComposition": composition,
                    "observations": messages.into_iter().map(|message| json!({
                        "kind": "message",
                        "message": message,
                    })).collect::<Vec<_>>(),
                }),
            )
            .expect("request")]
        };
        records.extend(request_records(
            "request-old",
            "turn-old",
            vec![ModelMessageV1 {
                message_id: "old-message".to_string(),
                role: ModelMessageRoleV1::User,
                content: "old context".to_string(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                reasoning_content: None,
            }],
            2,
        ));
        records.push(
            canonical_session_record(
                "evt:compaction:latest",
                SessionRecordType::Compaction,
                "session-restore",
                Some("turn-compact".to_string()),
                Some("agent-run-restore".to_string()),
                3,
                json!({
                    "compactionId": "compact-latest",
                    "summaryMessageId": "summary-latest",
                    "summaryMarkdown": "summary",
                    "firstKeptMessageId": "tail-latest",
                    "createdReason": "context_pressure_threshold_reached",
                }),
            )
            .expect("compaction"),
        );
        let observed_call = ToolCallEnvelope {
            id: "call-observed".to_string(),
            name: "read".to_string(),
            args_json: json!({"path": "observed.md"}).to_string(),
        };
        records.push(
            crate::runtime::canonical_tool_call_record(
                "session-restore",
                "turn-observed",
                "agent-run-restore",
                &observed_call,
                "centaeris.builtin",
                digest.as_str(),
                "observed.md",
                4,
            )
            .expect("observed tool call"),
        );
        records.push(
            crate::runtime::canonical_tool_result_record(
                "session-restore",
                "turn-observed",
                "agent-run-restore",
                &observed_call,
                &ToolExecutionResult {
                    tool_call_id: observed_call.id.clone(),
                    tool_name: observed_call.name.clone(),
                    status: "ok".to_string(),
                    content: "observed contents".to_string(),
                    details: json!({"path": "observed.md"}),
                    facts: Vec::new(),
                    error: None,
                    started_at_ms: 4,
                    completed_at_ms: 5,
                    latency_ms: 1,
                    parallel_group: None,
                    transition_reason: None,
                },
                5,
            )
            .expect("observed tool result"),
        );
        records.extend(request_records(
            "request-latest",
            "turn-latest",
            vec![
                ModelMessageV1 {
                    message_id: "summary-latest".to_string(),
                    role: ModelMessageRoleV1::System,
                    content: "summary".to_string(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ModelMessageV1 {
                    message_id: "tail-latest".to_string(),
                    role: ModelMessageRoleV1::User,
                    content: "tail".to_string(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ModelMessageV1 {
                    message_id: "assistant-observed".to_string(),
                    role: ModelMessageRoleV1::Assistant,
                    content: String::new(),
                    tool_calls: vec![crate::model::prepared_prompt::ModelToolCallV1 {
                        id: observed_call.id.clone(),
                        name: observed_call.name.clone(),
                        args_json: observed_call.args_json.clone(),
                    }],
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            6,
        ));
        let call = ToolCallEnvelope {
            id: "call-tail".to_string(),
            name: "read".to_string(),
            args_json: json!({"path": "notice.md"}).to_string(),
        };
        records.push(
            crate::runtime::canonical_tool_call_record(
                "session-restore",
                "turn-latest",
                "agent-run-restore",
                &call,
                "centaeris.builtin",
                digest.as_str(),
                "notice.md",
                7,
            )
            .expect("tool call"),
        );
        records.extend(
            started_agent_run_records(
                "session-restore",
                "turn-next",
                "agent-run-next",
                "continue after recovery",
                8,
            )
            .expect("legacy next user records"),
        );
        records.push(
            crate::runtime::canonical_tool_result_record(
                "session-restore",
                "turn-latest",
                "agent-run-restore",
                &call,
                &ToolExecutionResult {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    status: "ok".to_string(),
                    content: "notice contents".to_string(),
                    details: json!({"path": "notice.md"}),
                    facts: Vec::new(),
                    error: None,
                    started_at_ms: 7,
                    completed_at_ms: 9,
                    latency_ms: 2,
                    parallel_group: None,
                    transition_reason: None,
                },
                9,
            )
            .expect("tool result"),
        );

        let restored =
            restore_runtime_snapshot_from_session_records("session-restore", records.as_slice())
                .expect("restore runtime snapshot");

        assert!(!restored
            .messages
            .iter()
            .any(|message| message.message_id == "old-message"));
        assert_eq!(restored.messages[0].message_id, "summary-latest");
        assert_eq!(restored.messages[1].message_id, "tail-latest");
        let observed_assistant_index = restored
            .messages
            .iter()
            .position(|message| message.message_id == "assistant-observed")
            .expect("observed assistant");
        assert_eq!(
            restored.messages[observed_assistant_index + 1].role,
            MessageRole::Tool
        );
        assert_eq!(
            restored.messages[observed_assistant_index + 1].content,
            "observed contents"
        );
        let tail_tool_index = restored
            .messages
            .iter()
            .position(|message| {
                restored
                    .model_semantics
                    .get(message.message_id.as_str())
                    .is_some_and(|semantics| {
                        matches!(
                            semantics,
                            ModelMessageSemanticsV1::ToolResult { tool_call_id, .. }
                                if tool_call_id == "call-tail"
                        )
                    })
            })
            .expect("tail ToolResult");
        let legacy_user_index = restored
            .messages
            .iter()
            .position(|message| message.message_id == "message:turn-next:user")
            .expect("legacy next UserMessage");
        assert!(tail_tool_index < legacy_user_index);
        assert!(restored.model_semantics.values().any(|semantics| matches!(
            semantics,
            ModelMessageSemanticsV1::Assistant { tool_calls, .. }
                if tool_calls.iter().any(|call| call.id == "call-tail")
        )));
        assert!(restored.model_semantics.values().any(|semantics| matches!(
            semantics,
            ModelMessageSemanticsV1::ToolResult { tool_call_id, .. }
                if tool_call_id == "call-tail"
        )));
    }

    #[test]
    fn assistant_message_state_machine_rejects_running_status() {
        let mut log = valid_log();
        let running = base_event(
            "assistant_message",
            "evt-assistant-running-2",
            json!({
                "messageId": "message:turn-1:assistant",
                "modelMarkdown": "第一段第二段",
                "artifactRefs": [],
                "status": "running"
            }),
        );
        log.insert(4, running);
        let error =
            validate_event_log("session-1", log.as_slice()).expect_err("running must be rejected");
        assert!(error.contains("status is unsupported"));
    }

    #[test]
    fn assistant_message_projections_preserve_model_markdown_whitespace() {
        let model_markdown = "\n\n测试成功\n";
        let record = sealed_assistant_message_record(
            "session-1",
            "turn-1",
            "agent-run-1",
            model_markdown,
            "done",
            1,
        )
        .unwrap();

        assert_eq!(record.payload["modelMarkdown"], model_markdown);
        let SessionStreamProjection::SessionEvent { event, .. } =
            project_committed_session_record(&record, 0).unwrap();
        assert_eq!(event.payload["content"], model_markdown);
        let projection = reduce_events("session-1", std::iter::once(&record)).unwrap();
        assert_eq!(
            projection.messages["message:turn-1:assistant"].text,
            model_markdown
        );
    }

    #[test]
    fn agent_run_final_may_use_a_continuation_turn_but_must_be_unique() {
        let mut records =
            started_agent_run_records("session-1", "turn-root", "agent-run-1", "inspect", 1)
                .unwrap()
                .to_vec();
        records.push(
            sealed_assistant_message_record(
                "session-1",
                "turn-root:2",
                "agent-run-1",
                "done",
                "done",
                2,
            )
            .unwrap(),
        );
        records.push(
            completed_agent_run_record("session-1", "turn-root", "agent-run-1", "finalized", 3)
                .unwrap(),
        );

        let projection = reduce_events("session-1", records.iter()).unwrap();
        assert_eq!(
            projection.messages["message:turn-root:2:assistant"]
                .turn_id
                .as_deref(),
            Some("turn-root:2")
        );
        assert_eq!(
            projection.agent_runs["agent-run-1"].state,
            ReducedAgentRunState::Completed
        );

        records.insert(
            records.len() - 1,
            sealed_assistant_message_record(
                "session-1",
                "turn-root:3",
                "agent-run-1",
                "banana",
                "done",
                3,
            )
            .unwrap(),
        );
        assert!(reduce_events("session-1", records.iter())
            .expect_err("one agent run must reject a second sealed assistant")
            .contains("multiple sealed assistant messages"));
    }

    #[test]
    fn assistant_message_state_machine_rejects_writes_after_final() {
        let mut log = valid_log();
        let late = parse_event(&base_event(
            "assistant_message",
            "evt-assistant-late",
            json!({
                "messageId": "message:turn-1:assistant",
                "modelMarkdown": "late",
                "artifactRefs": [],
                "status": "done"
            }),
        ))
        .expect("late final");
        log.push(serde_json::to_value(late).expect("encode late"));
        let error =
            validate_event_log("session-1", log.as_slice()).expect_err("after final must fail");
        assert!(error.contains("written after final"));
    }

    #[test]
    fn turn_supplement_record_is_canonical_user_history() {
        let supplement = turn_supplement_record(
            "session-1",
            "turn-1",
            "agent-run-1",
            "supplement-1",
            "check the cancellation edge",
            2,
        )
        .expect("build turn supplement record");
        let mut log = valid_log();
        log.insert(
            3,
            serde_json::to_value(&supplement).expect("encode turn supplement record"),
        );

        let projection =
            validate_event_log("session-1", log.as_slice()).expect("supplement log validates");
        let message = projection
            .messages
            .get("message:turn-1:supplement:supplement-1")
            .expect("supplement projects as a message");
        assert_eq!(message.role, ReducedMessageRole::User);
        assert_eq!(message.text, "check the cancellation edge");
        assert_eq!(supplement.event_type, SessionRecordType::TurnSupplement);
        assert!(supplement.event_id.len() <= 160);
        assert_eq!(
            turn_supplement_record(
                "session-1",
                "turn-1",
                "agent-run-1",
                &"x".repeat(65),
                "banana",
                3,
            )
            .expect_err("oversized supplement id must fail"),
            "turn_supplement_id_invalid"
        );
    }

    #[test]
    fn assistant_message_state_machine_rejects_cross_turn_identity() {
        let mut log = valid_log();
        let mut cross_turn = valid_log()
            .iter()
            .find(|event| event["type"] == "assistant_message")
            .cloned()
            .expect("assistant message");
        cross_turn["eventId"] = json!("evt-assistant-cross-turn");
        cross_turn["turnId"] = json!("turn-other");
        log.insert(4, cross_turn);
        let error =
            validate_event_log("session-1", log.as_slice()).expect_err("cross turn must fail");
        assert!(error.contains("messageId does not match turnId"));
    }

    #[test]
    fn assistant_message_state_machine_rejects_cross_agent_run_identity() {
        let mut log = valid_log();
        let mut cross_agent_run = valid_log()
            .iter()
            .find(|event| event["type"] == "assistant_message")
            .cloned()
            .expect("assistant message");
        cross_agent_run["eventId"] = json!("evt-assistant-cross-agent-run");
        cross_agent_run["agentRunId"] = json!("agent-run-other");
        log.insert(4, cross_agent_run);
        let error =
            validate_event_log("session-1", log.as_slice()).expect_err("cross task must fail");
        assert!(error.contains("AgentRun identity mismatch"));
    }

    #[test]
    fn sequenced_session_record_batch_requires_order_and_one_session() {
        let started = parse_event(&base_event(
            "agent_run_started",
            "evt-started",
            json!({"userObjective": "inspect runtime"}),
        ))
        .expect("started event");
        let user = parse_event(&base_event(
            "user_message",
            "evt-user-batch",
            json!({"messageId": "msg-user", "text": "inspect runtime", "attachments": []}),
        ))
        .expect("user event");
        let mut events = vec![
            SequencedSessionRecord {
                sequence: 1,
                event: started,
            },
            SequencedSessionRecord {
                sequence: 2,
                event: user,
            },
        ];
        validate_sequenced_session_records(events.as_slice()).expect("valid batch");

        events[1].sequence = 1;
        assert!(validate_sequenced_session_records(events.as_slice()).is_err());
    }

    #[test]
    fn session_log_accepts_direct_state_machine_events() {
        let projection = validate_event_log("session-1", valid_log().as_slice())
            .expect("direct session.event.v1 log should validate");

        assert_eq!(
            projection
                .messages
                .get("message:turn-1:assistant")
                .expect("assistant message")
                .text,
            "done"
        );
        assert_eq!(
            projection
                .tool_calls
                .get("call-1")
                .expect("tool call")
                .result_state,
            Some(ToolResultState::SuccessWithOutput)
        );
        assert_eq!(
            projection
                .agent_runs
                .get("agent-run-1")
                .expect("agent run")
                .state,
            ReducedAgentRunState::Completed
        );
        assert_eq!(
            projection
                .citations
                .get("citation:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .expect("citation")
                .owner_ref,
            "source-object-1"
        );
    }

    #[test]
    fn citation_recorded_accepts_successful_read() {
        let mut log = valid_log();
        log[3]["payload"]["toolName"] = json!("read");
        log[4]["payload"]["toolName"] = json!("read");

        let projection = validate_event_log("session-1", log.as_slice())
            .expect("document extraction citation should validate");
        assert_eq!(
            projection.citations
                ["citation:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"]
                .owner_ref,
            "source-object-1"
        );
    }

    #[test]
    fn citation_recorded_accepts_derived_knowledge_search() {
        let mut log = valid_log();
        log[3]["payload"]["toolName"] = json!("external_search");
        log[4]["payload"]["toolName"] = json!("external_search");
        log[5]["payload"] = json!({
            "citationId": format!("citation:{}", "c".repeat(64)),
            "inputRef": "input-1",
            "ownerRef": "source-object-1",
            "ownerKind": "sourceObject",
            "displayName": "notice.pdf",
            "evidenceKind": "workspaceSource",
            "ownerSha256": format!("sha256:{}", "a".repeat(64)),
            "ownerGeneration": 1,
            "representationId": format!("representation:sha256:{}", "d".repeat(64)),
            "specDigest": format!("sha256:{}", "e".repeat(64)),
            "evidenceSha256": format!("sha256:{}", "f".repeat(64)),
            "sourceToolName": "external_search",
            "sourceToolCallId": "call-1",
            "locator": {
                "kind": "textSpan",
                "pageStart": 2,
                "pageEnd": 2,
                "startByte": 10,
                "endByte": 30,
                "startLine": 4,
                "endLine": 5
            }
        });

        let projection = validate_event_log("session-1", log.as_slice())
            .expect("derived knowledge citation should validate");
        assert!(projection
            .citations
            .contains_key(format!("citation:{}", "c".repeat(64)).as_str()));
    }

    #[test]
    fn citation_recorded_rejects_unknown_owner_kind() {
        let error = parse_event(&base_event(
            "citation_recorded",
            "evt-bad-citation",
            json!({
                "citationId": "citation:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "inputRef": "input-1",
                "ownerRef": "unknown-owner-1",
                "ownerKind": "banana",
                "displayName": "unknown.txt",
                "evidenceKind": "workspaceSource",
                "ownerSha256": format!("sha256:{}", "a".repeat(64)),
                "sourceToolName": "read",
                "sourceToolCallId": "call-1",
                "locator": {"startLine": 1, "endLine": 2}
            }),
        ))
        .expect_err("unknown citation owner must fail");
        assert!(error.contains("ownerKind is unsupported"));

        let mut duplicate_log = valid_log();
        let mut duplicate = duplicate_log[5].clone();
        duplicate["eventId"] = json!("evt-citation-duplicate");
        duplicate_log.insert(6, duplicate);
        let error = validate_event_log("session-1", duplicate_log.as_slice())
            .expect_err("duplicate citation id must fail");
        assert!(error.contains("duplicates citationId"));

        let error = parse_event(&base_event(
            "citation_recorded",
            "evt-bad-citation-range",
            json!({
                "citationId": "citation:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                "inputRef": "input-1",
                "ownerRef": "source-object-1",
                "ownerKind": "sourceObject",
                "displayName": "notice.md",
                "evidenceKind": "workspaceSource",
                "ownerSha256": format!("sha256:{}", "a".repeat(64)),
                "sourceToolName": "read",
                "sourceToolCallId": "call-1",
                "locator": {"startLine": 8, "endLine": 1}
            }),
        ))
        .expect_err("reversed citation range must fail");
        assert!(error.contains("locator range is invalid"));
    }

    #[test]
    fn artifact_publication_requires_successful_tool_and_orders_final_refs() {
        let artifact_ref = "artifact:artifact_1";
        let mut log = vec![
            base_event(
                "agent_run_started",
                "evt-artifact-start",
                json!({"userObjective": "create report"}),
            ),
            base_event(
                "user_message",
                "evt-artifact-user",
                json!({"messageId": "msg-artifact-user", "text": "create report", "attachments": []}),
            ),
            base_event(
                "tool_call",
                "evt-artifact-call",
                json!({
                    "callId": "call-publish",
                    "toolName": "artifact_export",
                    "toolContractDigest": format!("sha256:{}", "a".repeat(64)),
                    "providerId": "centaeris.builtin",
                    "normalizedInput": {"path": "/mnt/data/report.xlsx"},
                    "displayTarget": "artifact_export"
                }),
            ),
            base_event(
                "tool_result",
                "evt-artifact-result",
                json!({
                    "callId": "call-publish",
                    "toolName": "artifact_export",
                    "resultState": "successWithOutput",
                    "modelContent": "published report.xlsx",
                    "fullOutputPath": null,
                    "outputStartByte": null,
                    "outputByteLength": 21,
                    "outputComplete": true,
                    "summary": "published report.xlsx",
                    "operations": [],
                    "modelInputImages": [],
                    "latencyMs": 9
                }),
            ),
            base_event(
                "artifact_published",
                "evt-artifact-published",
                json!({
                    "publicationId": format!("pub_{}", "a".repeat(64)),
                    "artifactRef": artifact_ref,
                    "toolCallId": "call-publish",
                    "filename": "report.xlsx",
                    "sizeBytes": 4,
                    "sha256": format!("sha256:{}", "b".repeat(64))
                }),
            ),
            base_event(
                "assistant_message",
                "evt-artifact-assistant",
                json!({
                    "messageId": "message:turn-1:assistant",
                    "modelMarkdown": "done",
                    "artifactRefs": [artifact_ref],
                    "status": "done"
                }),
            ),
            base_event(
                "agent_run_completed",
                "evt-artifact-completed",
                json!({"doneReason": "finalized"}),
            ),
        ];
        let projection =
            validate_event_log("session-1", log.as_slice()).expect("artifact log validates");
        assert_eq!(projection.artifact_order, vec![artifact_ref]);
        assert_eq!(projection.artifacts.len(), 1);

        log[5]["payload"]["artifactRefs"] = json!([]);
        let error = validate_event_log("session-1", log.as_slice())
            .expect_err("missing final artifact ref must fail");
        assert!(error.contains("do not match published order"));
    }

    #[test]
    fn session_log_rejects_unknown_session_event_type() {
        let error = parse_event(&json!({
            "type": "banana",
            "payload": {}
        }))
        .expect_err("unknown event type must fail");
        assert!(
            error.contains("unsupported session.event.v1 session event type")
                && error.contains("allowed: session_meta"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn session_log_rejects_unknown_schema_and_type() {
        let bad_schema = base_event(
            "user_message",
            "evt-bad-schema",
            json!({"messageId": "msg", "text": "hi"}),
        )
        .as_object()
        .map(|object| {
            let mut object = object.clone();
            object.insert("schemaVersion".to_string(), json!("banana"));
            Value::Object(object)
        })
        .expect("object");
        let schema_error = parse_event(&bad_schema).expect_err("schema mismatch must fail");
        assert!(schema_error.contains("schemaVersion mismatch"));

        let type_error = parse_event(&base_event("banana", "evt-banana", json!({})))
            .expect_err("unknown type must fail");
        assert!(type_error.contains("unsupported session.event.v1 session event type"));
    }

    #[test]
    fn session_log_requires_schema_version() {
        let mut event = base_event(
            "user_message",
            "evt-missing-schema",
            json!({"messageId": "msg", "text": "hi"}),
        );
        event
            .as_object_mut()
            .expect("event object")
            .remove("schemaVersion");

        let error = parse_event(&event).expect_err("schemaVersion must be required");
        assert!(error.contains("missing field `schemaVersion`"));
    }

    #[test]
    fn session_log_rejects_duplicate_and_cross_session_events() {
        let mut duplicate_log = valid_log();
        let mut duplicate = duplicate_log[1].clone();
        duplicate["eventId"] = json!("evt-user");
        duplicate_log.push(duplicate);
        let duplicate_error =
            validate_event_log("session-1", duplicate_log.as_slice()).expect_err("duplicate fails");
        assert!(duplicate_error.contains("duplicate eventId"));

        let mut cross_session_log = valid_log();
        cross_session_log[1]["sessionId"] = json!("session-2");
        let cross_session_error = validate_event_log("session-1", cross_session_log.as_slice())
            .expect_err("cross-session fails");
        assert!(cross_session_error.contains("cross-session"));
    }

    #[test]
    fn session_log_rejects_tool_result_without_matching_tool_call() {
        let error = validate_event_log(
            "session-1",
            &[base_event(
                "tool_result",
                "evt-orphan-result",
                json!({
                    "callId": "missing-call",
                    "toolName": "bash",
                    "resultState": "successNoOutput",
                    "modelContent": "",
                    "fullOutputPath": null,
                    "outputStartByte": null,
                    "outputByteLength": 0,
                    "outputComplete": true,
                    "summary": "no output",
                    "operations": [],
                    "modelInputImages": [],
                    "latencyMs": 1
                }),
            )],
        )
        .expect_err("orphan tool result must fail");

        assert!(error.contains("no matching tool_call"));
    }

    #[test]
    fn session_log_requires_exact_tool_result_payload() {
        let missing_latency = parse_event(&base_event(
            "tool_result",
            "evt-result-missing-latency",
            json!({
                "callId": "call-1",
                "toolName": "bash",
                "resultState": "successNoOutput",
                "modelContent": "",
                "fullOutputPath": null,
                "outputStartByte": null,
                "outputByteLength": 0,
                "outputComplete": true,
                "summary": "missing latency",
                "operations": [],
                "modelInputImages": []
            }),
        ))
        .expect_err("missing latency must fail");
        assert!(missing_latency.contains("payload fields mismatch"));

        let unknown_field = parse_event(&base_event(
            "tool_result",
            "evt-result-unknown-field",
            json!({
                "callId": "call-1",
                "toolName": "bash",
                "resultState": "successNoOutput",
                "modelContent": "",
                "fullOutputPath": null,
                "outputStartByte": null,
                "outputByteLength": 0,
                "outputComplete": true,
                "summary": "unknown field",
                "operations": [],
                "modelInputImages": [],
                "latencyMs": 1,
                "banana": true
            }),
        ))
        .expect_err("unknown field must fail");
        assert!(unknown_field.contains("payload fields mismatch"));

        let negative_latency = parse_event(&base_event(
            "tool_result",
            "evt-result-negative-latency",
            json!({
                "callId": "call-1",
                "toolName": "bash",
                "resultState": "successNoOutput",
                "modelContent": "",
                "fullOutputPath": null,
                "outputStartByte": null,
                "outputByteLength": 0,
                "outputComplete": true,
                "summary": "negative latency",
                "operations": [],
                "modelInputImages": [],
                "latencyMs": -1
            }),
        ))
        .expect_err("negative latency must fail");
        assert!(negative_latency.contains("payload.latencyMs must be a u64"));
    }

    #[test]
    fn session_log_aggregates_raw_provider_usage_records_on_read() {
        let first = provider_usage_record(
            "session-1",
            "turn-1",
            "agent-run-1",
            &ProviderTokenUsageV1 {
                input_tokens: Some(10),
                output_tokens: Some(2),
                total_tokens: Some(12),
                prompt_cache_hit_tokens: Some(4),
                prompt_cache_miss_tokens: Some(6),
            },
            1,
        )
        .expect("first usage record");
        let second = provider_usage_record(
            "session-1",
            "turn-2",
            "agent-run-1",
            &ProviderTokenUsageV1 {
                input_tokens: Some(20),
                output_tokens: Some(3),
                total_tokens: Some(23),
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            2,
        )
        .expect("second usage record");

        let projection = reduce_events("session-1", [&first, &second]).expect("reduce usage");
        let usage = projection.provider_usage().expect("projected usage");
        assert_eq!(usage.latest_turn_id, "turn-2");
        assert_eq!(usage.latest.input_tokens, Some(20));
        assert_eq!(usage.session.input_tokens, Some(30));
        assert_eq!(usage.session.output_tokens, Some(5));

        let error = reduce_events("session-1", [&first, &first])
            .expect_err("duplicate turn usage must fail");
        assert!(error.contains("duplicate eventId"));
    }

    #[test]
    fn session_log_rejects_extra_checkpoint_payload_fields() {
        let error = parse_event(&base_event(
            "checkpoint_ref",
            "evt-checkpoint",
            json!({
                "checkpointId": "checkpoint-1",
                "status": "ready",
                "objectRef": "object:checkpoint-1",
                "payloadSha256": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "payloadByteLength": 123,
                "updatedAtMs": 2,
                "checkpointPayload": {"messages": []}
            }),
        ))
        .expect_err("inline checkpoint payload must fail");

        assert!(error.contains("payload fields mismatch"));
    }

    #[test]
    fn model_input_image_observation_keeps_only_the_logical_execution_reference() {
        let observation = json!({
            "kind": "input_image",
            "image": {
                "messageId": "message:tool:image-observation",
                "source": {
                    "sourceKind": "executionFile",
                    "image": {
                        "path": "out/page-1.png",
                        "contentType": "image/png",
                        "sha256": format!("sha256:{}", "a".repeat(64)),
                        "byteLength": 128,
                        "widthPx": 2,
                        "heightPx": 3,
                        "placeholder": "[Image observation: call-read-image]"
                    }
                }
            }
        });
        let event = parse_event(&base_event(
            "model_request_started",
            "evt-image-observation",
            model_request_payload(vec![observation.clone()]),
        ))
        .expect("logical image observation must validate");
        assert!(!serde_json::to_string(&event)
            .expect("serialize image observation")
            .contains("base64"));

        let mut with_bytes = base_event(
            "model_request_started",
            "evt-image-observation-bytes",
            model_request_payload(vec![observation]),
        );
        with_bytes["payload"]["observations"][0]["image"]["source"]["dataBase64"] = json!("banana");
        assert!(parse_event(&with_bytes)
            .expect_err("inline image bytes must fail")
            .contains("payload.observations is invalid"));
    }

    #[test]
    fn file_fact_accepts_concrete_mutation_operations_only() {
        let payload = |operation: &str| {
            json!({
                "schema": "file_mutation_pre_apply_fact_v1",
                "toolName": "edit",
                "toolCallId": "call-edit",
                "operation": operation,
                "path": "note.txt",
                "targetPath": null,
                "previousFileHash": null,
                "readSnapshotHash": null,
                "fileHash": null,
                "bytesWritten": null,
                "addedLines": null,
                "removedLines": null,
                "sessionId": "session-1",
                "executionOwner": "agent-run-1"
            })
        };

        for operation in ["create", "overwrite", "update"] {
            parse_event(&base_event(
                "file_fact",
                "evt-file-fact",
                payload(operation),
            ))
            .expect("concrete mutation operation must be accepted");
        }
        let mut unknown_field = payload("update");
        unknown_field["banana"] = json!(true);
        let error = parse_event(&base_event(
            "file_fact",
            "evt-file-fact-unknown-field",
            unknown_field,
        ))
        .expect_err("unknown file fact fields must fail");
        assert!(error.contains("payload fields mismatch"));
        for operation in ["write", "edit", "banana"] {
            let error = parse_event(&base_event(
                "file_fact",
                "evt-file-fact-invalid",
                payload(operation),
            ))
            .expect_err("tool names and unknown operations must fail");
            assert!(error.contains("file_fact operation is unsupported"));
        }
    }

    #[test]
    fn session_log_records_tombstone_targets_without_deleting_audit_events() {
        let mut log = valid_log();
        log.push(json!({
            "schemaVersion": SESSION_EVENT_SCHEMA_VERSION,
            "eventVersion": SESSION_EVENT_VERSION,
            "type": "tombstone",
            "eventId": "evt-tombstone",
            "sessionId": "session-1",
            "createdAtMs": 9,
            "payload": {
                "tombstoneId": "tombstone-1",
                "targetEventIds": ["evt-assistant-final", "evt-turn-completed"],
                "reasonType": "rewrite_tail"
            }
        }));

        let projection = validate_event_log("session-1", log.as_slice()).expect("tombstone valid");
        assert!(projection
            .tombstoned_event_ids
            .contains("evt-assistant-final"));
        assert!(!projection.messages.contains_key("message:turn-1:assistant"));
    }

    #[test]
    fn agent_run_session_state_owns_identity_order_and_idempotency() {
        let mut state = AgentRunSessionState::new("session-1", "agent-run-1").expect("state");
        let started = state
            .start("turn-1", "inspect runtime", Vec::new(), 1)
            .expect("start");
        assert_eq!(started[0].sequence, 1);
        assert_eq!(started[1].sequence, 2);

        let assistant = state
            .assistant("turn-1", "done", Vec::new(), "done", 2)
            .expect("assistant")
            .expect("assistant record");
        assert_eq!(assistant.sequence, 3);
        assert!(state
            .assistant("turn-1", "done", Vec::new(), "done", 2)
            .expect("idempotent assistant")
            .is_none());

        let foreign = completed_agent_run_record("banana", "turn-1", "agent-run-1", "finalized", 3)
            .expect("foreign record");
        assert!(state.record(foreign).is_err());
        assert_eq!(state.next_sequence(), 3);

        state.next = u64::MAX;
        assert!(state
            .event_for_turn(
                "turn-2",
                SessionRecordType::PhaseEvent,
                json!({"stage": "model_process_summary", "message": "banana"}),
                4,
            )
            .is_err());
    }

    #[test]
    fn agent_run_execution_requires_one_active_binding_and_unique_recovery_checkpoint() {
        let mut state = AgentRunSessionState::new("session-1", "agent-run-1").expect("state");
        state
            .start("turn-1", "recover safely", Vec::new(), 1)
            .expect("AgentRun start");
        let digest = format!("sha256:{}", "a".repeat(64));
        state
            .start_execution("turn-1", "execution-1", digest.as_str(), None, 2)
            .expect("first Execution");
        assert_eq!(state.active_execution_id(), Some("execution-1"));
        assert!(state
            .start_execution("turn-1", "execution-banana", digest.as_str(), None, 3,)
            .is_err());
        state
            .checkpoint_ref(&CheckpointRecord {
                checkpoint_id: "checkpoint-1".to_string(),
                kind: crate::runtime::contracts::CheckpointKindV1::Recovery,
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                status: "committed".to_string(),
                done_reason: None,
                updated_at_ms: 4,
                payload_json: "{}".to_string(),
            })
            .expect("recovery checkpoint");
        assert!(state.tool_ledger_is_checkpointed());
        let call = ToolCallEnvelope {
            id: "call-1".to_string(),
            name: "read".to_string(),
            args_json: json!({"path": "notes.md"}).to_string(),
        };
        state
            .record(
                crate::runtime::canonical_tool_call_record(
                    "session-1",
                    "turn-1",
                    "agent-run-1",
                    &call,
                    "centaeris.builtin",
                    digest.as_str(),
                    "notes.md",
                    5,
                )
                .expect("tool call record"),
            )
            .expect("tool call");
        state
            .record(
                crate::runtime::canonical_tool_result_record(
                    "session-1",
                    "turn-1",
                    "agent-run-1",
                    &call,
                    &ToolExecutionResult {
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        status: "ok".to_string(),
                        content: "notes".to_string(),
                        details: json!({"path": "notes.md"}),
                        facts: Vec::new(),
                        error: None,
                        started_at_ms: 5,
                        completed_at_ms: 6,
                        latency_ms: 1,
                        parallel_group: None,
                        transition_reason: None,
                    },
                    6,
                )
                .expect("tool result record"),
            )
            .expect("tool result");
        assert!(!state.tool_ledger_is_checkpointed());
        state
            .checkpoint_ref(&CheckpointRecord {
                checkpoint_id: "checkpoint-2".to_string(),
                kind: crate::runtime::contracts::CheckpointKindV1::Recovery,
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                status: "committed".to_string(),
                done_reason: None,
                updated_at_ms: 7,
                payload_json: "{}".to_string(),
            })
            .expect("post-tool recovery checkpoint");
        assert!(state.tool_ledger_is_checkpointed());
        state
            .end_execution(
                "turn-1",
                "execution-1",
                "lost",
                "execution_environment_lost",
                true,
                Some("checkpoint-2"),
                Vec::new(),
                8,
            )
            .expect("end first Execution");
        state
            .start_execution(
                "turn-1",
                "execution-2",
                digest.as_str(),
                Some("checkpoint-2"),
                9,
            )
            .expect("replacement Execution");
        state
            .end_execution(
                "turn-1",
                "execution-2",
                "lost",
                "execution_environment_lost",
                true,
                Some("checkpoint-2"),
                Vec::new(),
                10,
            )
            .expect("end replacement Execution");
        assert!(state
            .start_execution(
                "turn-1",
                "execution-3",
                digest.as_str(),
                Some("checkpoint-2"),
                11,
            )
            .expect_err("checkpoint replacement loop must fail")
            .contains("already started"));
    }

    #[test]
    fn agent_run_session_state_rejects_composition_drift_before_append() {
        let request = |event_id: &str, digest: &str| SessionLogRecord {
            schema_version: SESSION_EVENT_SCHEMA_VERSION.to_string(),
            event_version: SESSION_EVENT_VERSION,
            event_type: SessionRecordType::ModelRequestStarted,
            event_id: event_id.to_string(),
            session_id: "session-1".to_string(),
            turn_id: Some("turn-1".to_string()),
            agent_run_id: Some("agent-run-1".to_string()),
            created_at_ms: 1,
            payload: json!({
                "purpose": "main",
                "agentComposition": { "compositionDigest": digest },
            }),
        };
        let mut state = AgentRunSessionState::new("session-1", "agent-run-1").expect("state");
        let first = format!("sha256:{}", "a".repeat(64));
        let second = format!("sha256:{}", "b".repeat(64));
        state
            .record(request("request-1", first.as_str()))
            .expect("first composition");

        assert!(state
            .record(request("request-2", second.as_str()))
            .expect_err("composition drift must fail")
            .contains("immutable AgentRun composition"));
        assert_eq!(state.next_sequence(), 1);
    }

    #[test]
    fn agent_run_session_state_commits_typed_tool_facts_once() {
        let mut state = AgentRunSessionState::new("session-1", "agent-run-1").expect("state");
        let call = ToolCallEnvelope {
            id: "call-publish".to_string(),
            name: "publish_artifact".to_string(),
            args_json: json!({"path": "/mnt/data/report.txt"}).to_string(),
        };
        state
            .record_tool_call(
                "turn-1",
                &call,
                "banana.provider",
                format!("sha256:{}", "a".repeat(64)).as_str(),
                "report.txt",
                1,
            )
            .expect("tool call");
        let result = ToolExecutionResult {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status: "ok".to_string(),
            content: "published".to_string(),
            details: json!({"schema": "banana.result.v1"}),
            facts: vec![ToolExecutionFact::ArtifactPublished(json!({
                "publicationId": format!("pub_{}", "b".repeat(64)),
                "artifactRef": "artifact:report",
                "toolCallId": call.id,
                "filename": "report.txt",
                "sizeBytes": 1,
                "sha256": format!("sha256:{}", "c".repeat(64)),
            }))],
            error: None,
            started_at_ms: 1,
            completed_at_ms: 2,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: None,
        };
        let records = state
            .record_tool_result("turn-1", &call, &result, 2)
            .expect("tool result and fact");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].event.event_type, SessionRecordType::ToolResult);
        assert_eq!(
            records[1].event.event_type,
            SessionRecordType::ArtifactPublished
        );
        assert!(state
            .record_tool_result("turn-1", &call, &result, 2)
            .expect("idempotent facts")
            .is_empty());
    }
}
