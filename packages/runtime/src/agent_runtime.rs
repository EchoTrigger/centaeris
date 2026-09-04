use crate::http_transport::ReqwestJsonHttpTransport;
use crate::runtime_config;
use crate::runtime_rpc_transport::EventWriter;
use crate::runtime_server::{
    ActiveAgentRun, AgentRunLease, LiveTextJournal, LiveTextJournalKey, LiveTextOperation,
    OwnerExitDisposition,
};
use crate::sqlite_store::SqliteRuntimeStore;
use crate::{
    agent_runs, mcp, message_log, operation_receipts, sessions, skills, user_data_layout,
    workspaces,
};
use centaeris_core::execution::sandbox::{NetworkSandboxPolicy, SandboxPolicy};
use centaeris_core::execution::{
    ExecutionCancellationProbe, ExecutionHostBinding, ExecutionHostMode,
};
use centaeris_core::model::prepared_prompt::{
    ModelMessageRoleV1, ModelMessageV1, PreparedPromptV1,
};
use centaeris_core::model::prompt::PromptCompactionScopeV1;
use centaeris_core::model::{
    AnthropicMessagesModelClient, AuthSpec, CapabilityProfile, JsonHttpFuture, JsonHttpRequest,
    JsonHttpResponse, JsonHttpTransport, ModelClient, ModelClientError, ModelProviderInfo,
    ModelProviderKind, ModelProviderRegistry, ModelSessionConfig, OpenAiCompatibleModelClient,
    OpenAiResponsesModelClient, WireApi, DEFAULT_MODEL_MAX_RETRIES,
    DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS, DEFAULT_MODEL_RETRY_BACKOFF_MS,
};
use centaeris_core::runtime::contracts::{
    current_timestamp_ms, new_turn_id, AgentRunInterventionV1, RuntimeAgentRunIdentityV1,
    RuntimeAwaitJobCheckpointV1,
};
use centaeris_core::runtime::projection::{
    headless_transcript_lines_from_stream_items, project_turn_update, HeadlessTranscriptLine,
};
use centaeris_core::runtime::subagent::{
    cancel_subagent_run_job_async, cancel_subagent_run_jobs_async, load_subagent_work_packet_async,
    subagent_work_packet_runtime_binding, CancelSubagentRunJobRequest,
    CancelSubagentRunJobsRequest,
};
use centaeris_core::runtime::{
    build_subagent_scheduler_runtime_event, completed_turn_projection_from_result,
    persist_answer_now_requested_fact, persist_subagent_result_projection_from_scheduler_events,
    AgentRunRequest, AgentRunResumeIntent, AgentRunStop, AgentRuntime, AgentRuntimeConfig,
    AnswerNowEnqueueDisposition, ModelRequestPurposeV1, QueryLifecycleHookRuntime,
    ToolConcurrencyCoordinator, ToolSafePoint, TurnControl, TurnStepResult, TurnUpdate,
};
use centaeris_core::session::manager::SessionManager;
use centaeris_core::session::store::RuntimeStoreActor;
use centaeris_core::tool::layer::{FileMutationCommitPort, FileMutationCommitRequest, ToolLayer};
use centaeris_core::tool::{DynamicToolRegistry, ModelToolChoice};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

static AGENT_RUN_COUNTER: AtomicU64 = AtomicU64::new(1);
static AGENT_RUNTIME_STORE_ACTOR: OnceLock<Mutex<Option<AgentRuntimeStoreActorState>>> =
    OnceLock::new();
static SESSION_PROMPT_OPERATION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
const SESSION_PROMPT_METHOD: &str = "session/prompt";
const RUNTIME_JOB_WAIT_POLL_MS: u64 = 500;
const LIVE_TEXT_FLUSH_INTERVAL_MS: u64 = 75;

#[derive(Clone)]
struct AgentRuntimeStoreActorState {
    db_path: PathBuf,
    actor: RuntimeStoreActor,
}

struct ActiveAgentRunRegistration {
    event_writer: EventWriter,
    agent_run_lease: AgentRunLease,
}

impl Drop for ActiveAgentRunRegistration {
    fn drop(&mut self) {
        if let Err(error) = self
            .event_writer
            .finish_agent_run(self.agent_run_lease.lease_id.as_str())
        {
            eprintln!("centaeris electron active agent run lease finish failed: {error}");
        }
    }
}

struct LiveTextAccumulator {
    key: LiveTextJournalKey,
    journal_root: PathBuf,
    journal: Option<LiveTextJournal>,
    pending_operations: Vec<LiveTextOperation>,
    pending_payloads: Vec<serde_json::Value>,
    content: String,
    last_flush: Option<Instant>,
}

impl LiveTextAccumulator {
    fn new(session_id: String, turn_id: String, agent_run_id: String) -> Self {
        Self {
            key: LiveTextJournalKey {
                session_id,
                turn_id,
                agent_run_id,
            },
            journal_root: user_data_layout::runtime_live_text_journal_dir_path(),
            journal: None,
            pending_operations: Vec::new(),
            pending_payloads: Vec::new(),
            content: String::new(),
            last_flush: None,
        }
    }

    fn push(&mut self, payload: serde_json::Value) -> Result<Vec<serde_json::Value>, String> {
        let operation = live_text_operation_from_payload(&payload)?;
        match operation {
            LiveTextOperation::Append { text } => {
                self.content.push_str(text.as_str());
                if let Some(LiveTextOperation::Append { text: pending }) =
                    self.pending_operations.last_mut()
                {
                    pending.push_str(text.as_str());
                } else {
                    self.pending_operations
                        .push(LiveTextOperation::Append { text: text.clone() });
                }
                if let Some(last_payload) = self.pending_payloads.last_mut() {
                    if is_model_text_delta_payload(last_payload) {
                        let pending = last_payload
                            .get_mut("event")
                            .and_then(|event| event.get_mut("payload"))
                            .and_then(|body| body.get_mut("delta"))
                            .and_then(|delta| delta.as_str())
                            .ok_or_else(|| {
                                "queued ModelTextDelta payload is missing payload.delta".to_string()
                            })?
                            .to_string();
                        last_payload["event"]["payload"]["delta"] =
                            serde_json::Value::String(format!("{pending}{text}"));
                    } else {
                        self.pending_payloads.push(payload);
                    }
                } else {
                    self.pending_payloads.push(payload);
                }
            }
            LiveTextOperation::Replace { text } => {
                self.content = text.clone();
                self.pending_operations.clear();
                self.pending_payloads.clear();
                self.pending_operations
                    .push(LiveTextOperation::Replace { text });
                self.pending_payloads.push(payload);
            }
        }
        if self.last_flush.is_none_or(|last_flush| {
            last_flush.elapsed().as_millis() >= LIVE_TEXT_FLUSH_INTERVAL_MS as u128
        }) {
            return self.flush();
        }
        Ok(Vec::new())
    }

    fn begin_model_request(
        &mut self,
        turn_id: &str,
        initial_content: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let turn_id = required_string(turn_id, "turnId")?;
        if self.key.turn_id == turn_id {
            if self.content != initial_content {
                return Err("duplicate model request start changed initialContent".to_string());
            }
            return Ok(Vec::new());
        }
        let payloads = self.flush()?;
        self.seal()?;
        self.key.turn_id = turn_id;
        self.content = initial_content.to_string();
        if !initial_content.is_empty() {
            self.pending_operations.push(LiveTextOperation::Replace {
                text: initial_content.to_string(),
            });
        }
        self.last_flush = None;
        Ok(payloads)
    }

    fn flush(&mut self) -> Result<Vec<serde_json::Value>, String> {
        if self.pending_operations.is_empty() {
            return Ok(Vec::new());
        }
        if self.journal.is_none() {
            self.journal = Some(LiveTextJournal::create(
                self.journal_root.as_path(),
                self.key.clone(),
            )?);
        }
        self.journal
            .as_mut()
            .expect("live text journal is initialized")
            .append(self.pending_operations.as_slice())?;
        self.pending_operations.clear();
        self.last_flush = Some(Instant::now());
        Ok(std::mem::take(&mut self.pending_payloads))
    }

    fn content(&self) -> &str {
        self.content.as_str()
    }

    fn turn_id(&self) -> &str {
        self.key.turn_id.as_str()
    }

    fn seal(&mut self) -> Result<(), String> {
        if let Some(journal) = self.journal.take() {
            journal.seal()?;
        }
        Ok(())
    }
}

fn is_model_text_delta_payload(payload: &serde_json::Value) -> bool {
    payload
        .get("event")
        .and_then(|event| event.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("ModelTextDelta")
}

fn is_live_text_payload(payload: &serde_json::Value) -> bool {
    matches!(
        payload
            .get("event")
            .and_then(|event| event.get("type"))
            .and_then(serde_json::Value::as_str),
        Some("ModelTextDelta" | "ModelTextReplace")
    )
}

fn live_text_operation_from_payload(
    payload: &serde_json::Value,
) -> Result<LiveTextOperation, String> {
    let event = payload
        .get("event")
        .ok_or_else(|| "live text payload is missing event".to_string())?;
    let event_type = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "live text payload is missing event.type".to_string())?;
    let body = event
        .get("payload")
        .ok_or_else(|| "live text payload is missing event.payload".to_string())?;
    match event_type {
        "ModelTextDelta" => Ok(LiveTextOperation::Append {
            text: body
                .get("delta")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "ModelTextDelta is missing payload.delta".to_string())?
                .to_string(),
        }),
        "ModelTextReplace" => Ok(LiveTextOperation::Replace {
            text: body
                .get("content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "ModelTextReplace is missing payload.content".to_string())?
                .to_string(),
        }),
        other => Err(format!("unsupported live text event type: {other}")),
    }
}

fn active_agent_run_for_cancel(
    event_writer: &EventWriter,
    request: &agent_runs::AgentRunCancelRequest,
) -> Result<Option<ActiveAgentRun>, String> {
    if let Some(agent_run_id) = request
        .agent_run_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return event_writer.active_agent_run(agent_run_id);
    }
    let Some(session_id) = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    event_writer.active_agent_run_for_session(session_id)
}

fn request_active_agent_run_cancellation(
    active: &ActiveAgentRun,
    request: agent_runs::AgentRunCancelRequest,
    reason: &str,
) -> Result<agent_runs::AgentRunCancelResponse, String> {
    active.close_with_cancellation(reason, || agent_runs::request_cancel(request))
}

pub(crate) fn cancel_agent_run(
    event_writer: EventWriter,
    request: agent_runs::AgentRunCancelRequest,
) -> Result<agent_runs::AgentRunCancelResponse, String> {
    let active = active_agent_run_for_cancel(&event_writer, &request)?;
    let session_id = active
        .as_ref()
        .map(|turn| turn.lease.session_id.clone())
        .or_else(|| request.session_id.clone())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("user_cancelled")
        .to_string();
    let terminal_was_committed = active.is_none();
    let response = if let Some(active) = active {
        request_active_agent_run_cancellation(&active, request, reason.as_str())?
    } else {
        cancel_agent_run_after_tool_closure(request)?
    };
    if response.cancelled {
        if terminal_was_committed {
            let agent_run = response
                .agent_run
                .as_ref()
                .ok_or_else(|| "cancelled agent run is missing its terminal summary".to_string())?;
            emit_agent_run_terminal_payload(&event_writer, agent_run)?;
        }
        if let Some(session_id) = session_id {
            schedule_child_job_cancellation(event_writer, session_id, reason)?;
        }
    }
    Ok(response)
}

fn cancel_agent_run_after_tool_closure(
    request: agent_runs::AgentRunCancelRequest,
) -> Result<agent_runs::AgentRunCancelResponse, String> {
    let agent_run = match request.agent_run_id.as_deref() {
        Some(agent_run_id) => message_log::project_agent_run(agent_run_id)?,
        None => {
            let session_id = request
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let mut matches = message_log::project_agent_runs()?
                .into_iter()
                .filter(|run| {
                    session_id.is_some_and(|value| run.session_id == value)
                        && matches!(run.status.as_str(), "running" | "stalled")
                });
            let first = matches.next();
            if matches.next().is_some() {
                return Err("active AgentRun is ambiguous for Session cancellation".to_string());
            }
            first
        }
    };
    if let Some(agent_run) =
        agent_run.filter(|run| matches!(run.status.as_str(), "running" | "stalled"))
    {
        close_incomplete_tool_calls(&agent_run)?;
    }
    agent_runs::cancel(request)
}

fn close_incomplete_tool_calls(agent_run: &message_log::ProjectedAgentRun) -> Result<(), String> {
    let incomplete = message_log::project_incomplete_tool_calls(
        agent_run.session_id.as_str(),
        agent_run.agent_run_id.as_str(),
    )?;
    if incomplete.is_empty() {
        return Ok(());
    }
    let store = agent_runtime_store_actor()?;
    for pending in incomplete {
        let result = AgentRuntime::<RuntimeStoreActor>::recover_incomplete_session_tool_call(
            &store,
            agent_run.session_id.as_str(),
            pending.turn_id.as_str(),
            agent_run.agent_run_id.as_str(),
            &pending.call,
            pending.recorded_at_ms,
        )?;
        message_log::append_tool_result(
            agent_run.session_id.as_str(),
            pending.turn_id.as_str(),
            agent_run.agent_run_id.as_str(),
            &pending.call,
            &result,
        )?;
    }
    Ok(())
}

fn recover_incomplete_tool_calls_before_new_turn(session_id: &str) -> Result<(), String> {
    for agent_run in message_log::project_agent_runs_for_session(session_id)?
        .into_iter()
        .filter(|agent_run| {
            agent_run.session_id == session_id
                && matches!(agent_run.status.as_str(), "running" | "stalled")
        })
    {
        if message_log::project_incomplete_tool_calls(session_id, agent_run.agent_run_id.as_str())?
            .is_empty()
        {
            continue;
        }
        close_incomplete_tool_calls(&agent_run)?;
        let _ = agent_runs::fail_agent_run(
            agent_run.agent_run_id.as_str(),
            session_id,
            agent_run.turn_id.as_str(),
            "Recovered incomplete ToolCall before new user turn",
        )?;
    }
    Ok(())
}

pub(crate) fn close_incomplete_tool_calls_for_agent_run(
    session_id: &str,
    agent_run_id: &str,
) -> Result<(), String> {
    let agent_run = message_log::project_agent_run(agent_run_id)?
        .ok_or_else(|| format!("AgentRun not found: {agent_run_id}"))?;
    if agent_run.session_id != session_id {
        return Err(format!("AgentRun sessionId mismatch: {agent_run_id}"));
    }
    close_incomplete_tool_calls(&agent_run)
}

fn schedule_child_job_cancellation(
    event_writer: EventWriter,
    session_id: String,
    reason: String,
) -> Result<(), String> {
    let binding = sessions::agent_runtime_binding(session_id.as_str())?;
    let store = agent_runtime_store_actor()?;
    tokio::runtime::Handle::try_current()
        .map_err(|error| format!("Agent cancellation requires Tokio runtime: {error}"))?
        .spawn(async move {
            let cancellation = async {
                let (parent_session_id, events) =
                    if let Some((parent_session_id, runtime_job_id)) = binding {
                        let event = cancel_subagent_run_job_async(
                            &store,
                            CancelSubagentRunJobRequest {
                                job_id: runtime_job_id,
                                reason,
                                cancelled_at_ms: current_timestamp_ms(),
                            },
                        )
                        .await?;
                        (parent_session_id, vec![event])
                    } else {
                        let result = cancel_subagent_run_jobs_async(
                            &store,
                            CancelSubagentRunJobsRequest {
                                session_id: Some(session_id.clone()),
                                parent_turn_id: None,
                                subagent_id: None,
                                reason,
                                cancelled_at_ms: current_timestamp_ms(),
                                limit: 1_024,
                                include_running: true,
                            },
                        )
                        .await?;
                        (session_id.clone(), result.events)
                    };
                persist_subagent_result_projection_from_scheduler_events(
                    &store,
                    parent_session_id.as_str(),
                    events.as_slice(),
                )?;
                for event in events {
                    let job = store
                        .get_runtime_job(event.job_id.as_str())
                        .await?
                        .ok_or_else(|| {
                            format!("cancelled Agent runtime job missing: {}", event.job_id)
                        })?;
                    let packet = load_subagent_work_packet_async(&store, &job).await?;
                    let binding = subagent_work_packet_runtime_binding(&packet, &job)?;
                    let bridge = build_subagent_scheduler_runtime_event(
                        parent_session_id.as_str(),
                        event.parent_turn_id.as_str(),
                        &event,
                    );
                    emit_background_turn_update_blocking(
                        &event_writer,
                        binding.parent_agent_run_id.as_str(),
                        parent_session_id.as_str(),
                        TurnUpdate::RuntimeEvent { event: bridge },
                    )?;
                }
                Ok::<(), String>(())
            }
            .await;
            if let Err(error) = cancellation {
                eprintln!("centaeris Agent cancellation failed: {error}");
            }
        });
    Ok(())
}

pub(crate) fn interrupt_owner_agent_runs(
    event_writer: EventWriter,
    dispositions: Vec<OwnerExitDisposition>,
) -> Result<(), String> {
    for disposition in dispositions {
        let OwnerExitDisposition::Interrupt(lease) = disposition else {
            continue;
        };
        let _ = cancel_agent_run(
            event_writer.clone(),
            agent_runs::AgentRunCancelRequest {
                agent_run_id: Some(lease.agent_run_id),
                session_id: Some(lease.session_id),
                reason: Some(String::from("host_owner_exited")),
            },
        )?;
    }
    Ok(())
}

struct ElectronFileMutationCommitPort {
    session_id: String,
    turn_id: String,
    agent_run_id: String,
}

pub(crate) fn file_mutation_commit_port(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
) -> Arc<dyn FileMutationCommitPort + Send + Sync> {
    Arc::new(ElectronFileMutationCommitPort {
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        agent_run_id: agent_run_id.to_string(),
    })
}

impl FileMutationCommitPort for ElectronFileMutationCommitPort {
    fn commit_file_mutation(&self, request: FileMutationCommitRequest) -> Result<(), String> {
        let tool_name = request.tool_name.clone();
        let tool_call_id = request.tool_call_id.clone();
        let file_fact = serde_json::json!({
                "schema": "file_mutation_pre_apply_fact_v1",
                "toolName": tool_name,
                "toolCallId": tool_call_id,
                "operation": request.operation,
                "path": request.path,
                "targetPath": request.target_path,
                "previousFileHash": request.previous_file_hash,
                "readSnapshotHash": request.read_snapshot_hash,
                "fileHash": request.file_hash,
                "bytesWritten": request.bytes_written,
                "addedLines": request.added_lines,
                "removedLines": request.removed_lines,
                "sessionId": request.session_id,
                "executionOwner": request.execution_owner,
        });
        append_file_mutation_pre_apply_fact_blocking(
            self.agent_run_id.clone(),
            self.session_id.clone(),
            self.turn_id.clone(),
            file_fact,
        )
        .map_err(|error| format!("file mutation commit append failed: {error}"))?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct TranscriptProjectionRequest {
    pub(crate) stream_items: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentInputRequest {
    #[serde(deserialize_with = "operation_receipts::deserialize_operation_id")]
    pub(crate) operation_id: String,
    pub(crate) session_id: Option<String>,
    pub(crate) message: String,
    pub(crate) tail_policy: Option<String>,
    pub(crate) rewrite_target_message_id: Option<String>,
    pub(crate) rewrite_expected_tail_message_id: Option<String>,
    pub(crate) auto_continue_after_resume_wait: Option<bool>,
    #[serde(default)]
    pub(crate) attachments: Vec<crate::local_attachments::LocalImageInputRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSupplementRequest {
    pub(crate) session_id: String,
    pub(crate) agent_run_id: String,
    pub(crate) message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentQuestionAnswerRequest {
    pub(crate) session_id: Option<String>,
    pub(crate) question_id: String,
    pub(crate) answers: Option<Vec<String>>,
    pub(crate) answer_text: Option<String>,
    pub(crate) auto_continue_after_resume_wait: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentContextCompactRequest {
    pub(crate) session_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentContextCompactResponse {
    pub(crate) session_id: String,
    pub(crate) compacted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TranscriptProjectionResponse {
    pub(crate) lines: Vec<HeadlessTranscriptLine>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentInputResponse {
    pub(crate) session_id: String,
    pub(crate) agent_run_id: String,
    pub(crate) turn_id: String,
    pub(crate) stream_items: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentSupplementResponse {
    pub(crate) accepted: bool,
    pub(crate) session_id: String,
    pub(crate) agent_run_id: String,
    pub(crate) queued_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentAnswerNowResponse {
    pub(crate) accepted: bool,
    pub(crate) disposition: &'static str,
    pub(crate) session_id: String,
    pub(crate) agent_run_id: String,
    pub(crate) intervention_id: String,
}

pub(crate) fn project_session_events_to_transcript(
    request: TranscriptProjectionRequest,
) -> TranscriptProjectionResponse {
    TranscriptProjectionResponse {
        lines: headless_transcript_lines_from_stream_items(request.stream_items.as_slice()),
    }
}

#[derive(Debug)]
pub(crate) struct AgentInputCommandError {
    code: &'static str,
    message: String,
}

impl AgentInputCommandError {
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }

    pub(crate) fn message(&self) -> &str {
        self.message.as_str()
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            code: "session_prompt_failed",
            message: message.into(),
        }
    }

    fn conflict(operation_id: &str) -> Self {
        Self {
            code: "operation_id_conflict",
            message: format!(
                "operationId was already used for a different {SESSION_PROMPT_METHOD} request: {operation_id}"
            ),
        }
    }
}

impl Display for AgentInputCommandError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message.as_str())
    }
}

impl std::error::Error for AgentInputCommandError {}

impl From<String> for AgentInputCommandError {
    fn from(message: String) -> Self {
        Self::failed(message)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalAgentInputRequest<'a> {
    session_id: &'a str,
    message: &'a str,
    tail_policy: &'a str,
    rewrite_target_message_id: Option<&'a str>,
    rewrite_expected_tail_message_id: Option<&'a str>,
    auto_continue_after_resume_wait: Option<bool>,
    attachments: Vec<CanonicalLocalImageInputRequest<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalLocalImageInputRequest<'a> {
    placeholder: &'a str,
    local_path: &'a str,
}

pub(crate) fn input(
    event_writer: EventWriter,
    request: AgentInputRequest,
) -> Result<AgentInputResponse, AgentInputCommandError> {
    let session_id = normalize_session_id(request.session_id.as_deref());
    let message = required_string(request.message.as_str(), "message")?;
    let rewrite_last_user_input = rewrite_last_user_input_from_request(&request)?;
    if rewrite_last_user_input.is_some() && !request.attachments.is_empty() {
        return Err(AgentInputCommandError::failed(
            "rewriteLastUser does not support input images",
        ));
    }
    let tail_policy = if rewrite_last_user_input.is_some() {
        "rewriteLastUser"
    } else {
        "append"
    };
    let canonical = CanonicalAgentInputRequest {
        session_id: session_id.as_str(),
        message: message.as_str(),
        tail_policy,
        rewrite_target_message_id: rewrite_last_user_input
            .as_ref()
            .map(|rewrite| rewrite.target_chat_message_id.as_str()),
        rewrite_expected_tail_message_id: rewrite_last_user_input
            .as_ref()
            .map(|rewrite| rewrite.expected_tail_chat_message_id.as_str()),
        auto_continue_after_resume_wait: request.auto_continue_after_resume_wait,
        attachments: request
            .attachments
            .iter()
            .map(|attachment| CanonicalLocalImageInputRequest {
                placeholder: attachment.placeholder.as_str(),
                local_path: attachment.local_path.as_str(),
            })
            .collect(),
    };
    let request_digest = operation_receipts::request_digest(&canonical)?;
    let expected_response = AgentInputResponse {
        session_id: session_id.clone(),
        agent_run_id: operation_receipts::deterministic_identity(
            "agent-run-",
            SESSION_PROMPT_METHOD,
            request.operation_id.as_str(),
        ),
        turn_id: operation_receipts::deterministic_identity(
            "turn-",
            SESSION_PROMPT_METHOD,
            request.operation_id.as_str(),
        ),
        stream_items: Vec::new(),
    };
    let _guard = SESSION_PROMPT_OPERATION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| AgentInputCommandError::failed("session/prompt operation lock poisoned"))?;
    if let Some(receipt) =
        operation_receipts::read(SESSION_PROMPT_METHOD, request.operation_id.as_str())?
    {
        if receipt.request_digest != request_digest {
            return Err(AgentInputCommandError::conflict(
                request.operation_id.as_str(),
            ));
        }
        let response =
            serde_json::from_value::<AgentInputResponse>(receipt.result).map_err(|error| {
                AgentInputCommandError::failed(format!(
                    "decode persisted session/prompt result failed: {error}"
                ))
            })?;
        if response != expected_response {
            return Err(AgentInputCommandError::failed(
                "persisted session/prompt result identity mismatch",
            ));
        }
        if message_log::project_agent_run(response.agent_run_id.as_str())?.is_some() {
            return Ok(response);
        }
        return start_agent_input_from_receipt(
            event_writer,
            request,
            response,
            message,
            rewrite_last_user_input,
        );
    }
    let response = expected_response;
    operation_receipts::write(
        SESSION_PROMPT_METHOD,
        request.operation_id.as_str(),
        request_digest,
        serde_json::to_value(&response).map_err(|error| {
            AgentInputCommandError::failed(format!(
                "serialize session/prompt receipt result failed: {error}"
            ))
        })?,
    )?;
    start_agent_input_from_receipt(
        event_writer,
        request,
        response,
        message,
        rewrite_last_user_input,
    )
}

fn start_agent_input_from_receipt(
    event_writer: EventWriter,
    request: AgentInputRequest,
    response: AgentInputResponse,
    message: String,
    rewrite_last_user_input: Option<RewriteLastUserInput>,
) -> Result<AgentInputResponse, AgentInputCommandError> {
    let agent_run = start_agent_run(StartAgentRunRequest {
        event_writer,
        session_id: response.session_id.clone(),
        message,
        rewrite_last_user_input,
        auto_continue_after_resume_wait: request.auto_continue_after_resume_wait,
        resume_from_turn_id: None,
        attachments: request.attachments,
        requested_agent_run_id: Some(response.agent_run_id.clone()),
        requested_turn_id: Some(response.turn_id.clone()),
    })?;
    if agent_run.agent_run_id != response.agent_run_id || agent_run.turn_id != response.turn_id {
        return Err(AgentInputCommandError::failed(
            "session/prompt started with identities that differ from its durable receipt",
        ));
    }
    Ok(response)
}

pub(crate) async fn compact_context(
    event_writer: EventWriter,
    request: AgentContextCompactRequest,
) -> Result<AgentContextCompactResponse, String> {
    let session_id = required_string(request.session_id.as_str(), "sessionId")?;
    if event_writer
        .active_agent_run_for_session(session_id.as_str())?
        .is_some()
    {
        return Err("cannot compact context while an AgentRun is active".to_string());
    }
    let projection = message_log::project_session_log(session_id.as_str())?;
    let agent_run_id = projection
        .agent_runs
        .iter()
        .max_by_key(|run| run.updated_at_ms)
        .map(|run| run.agent_run_id.clone())
        .ok_or_else(|| "context compaction requires an existing AgentRun".to_string())?;
    let turn_id = new_turn_id();
    let runtime_config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {})?;
    let agent_runtime = resolve_agent_runtime(session_id.as_str())?;
    let store = agent_runtime_store_actor()?;
    let identity = native_agent_run_identity(
        agent_run_id.as_str(),
        session_id.as_str(),
        agent_runtime.cwd.as_path(),
    )?;
    let runtime = build_agent_runtime(AgentRuntimeBuildRequest {
        store,
        session_id: session_id.clone(),
        concurrency_scope_id: session_id.clone(),
        cwd: agent_runtime.cwd,
        execution_owner: identity.agent_run_id,
        bash_path: runtime_config.bash_path.as_deref().map(PathBuf::from),
        runtime_config: agent_runtime_config(session_id.as_str(), &runtime_config, None)?,
        native_plugin_activation: None,
        execution_cancellation_probe: None,
        file_mutation_commit_port: None,
    })?;
    let mut tool_safe_point = |safe_point: ToolSafePoint| match safe_point {
        ToolSafePoint::ModelRequestStarted(started) => message_log::append_model_request_started(
            agent_run_id.as_str(),
            &started,
            current_timestamp_ms(),
        ),
        _ => Err("manual context compaction emitted an unsupported safe point".to_string()),
    };
    let compacted = run_manual_context_compaction(
        &runtime,
        &runtime_config,
        session_id.as_str(),
        turn_id.as_str(),
        &mut tool_safe_point,
    )
    .await?;
    Ok(AgentContextCompactResponse {
        session_id,
        compacted,
    })
}

fn rewrite_last_user_input_from_request(
    request: &AgentInputRequest,
) -> Result<Option<RewriteLastUserInput>, String> {
    let Some(policy) = request
        .tail_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    match policy {
        "append" => Ok(None),
        "rewriteLastUser" => Ok(Some(RewriteLastUserInput {
            target_chat_message_id: required_string(
                request
                    .rewrite_target_message_id
                    .as_deref()
                    .unwrap_or_default(),
                "rewriteTargetMessageId",
            )?,
            expected_tail_chat_message_id: required_string(
                request
                    .rewrite_expected_tail_message_id
                    .as_deref()
                    .unwrap_or_default(),
                "rewriteExpectedTailMessageId",
            )?,
        })),
        other => Err(format!("unsupported tailPolicy: {other}")),
    }
}

pub(crate) fn supplement(
    event_writer: EventWriter,
    request: AgentSupplementRequest,
) -> Result<AgentSupplementResponse, String> {
    enqueue_supplement(&event_writer, request)
}

fn enqueue_supplement(
    event_writer: &EventWriter,
    request: AgentSupplementRequest,
) -> Result<AgentSupplementResponse, String> {
    let agent_run_id = required_string(request.agent_run_id.as_str(), "agentRunId")?;
    let session_id = required_string(request.session_id.as_str(), "sessionId")?;
    let message = required_string(request.message.as_str(), "message")?;
    let active = event_writer
        .active_agent_run(agent_run_id.as_str())?
        .ok_or_else(|| {
            format!(
            "turn supplement rejected: active agent run not found for agentRunId={agent_run_id}"
        )
        })?;
    if session_id != active.lease.session_id {
        return Err(format!(
            "turn supplement sessionId mismatch: expected={} actual={session_id}",
            active.lease.session_id
        ));
    }
    let queued_count = active.control.enqueue_supplement_with(message, || Ok(()))?;
    Ok(AgentSupplementResponse {
        accepted: true,
        session_id: active.lease.session_id,
        agent_run_id,
        queued_count,
    })
}

pub(crate) fn answer_now(
    event_writer: EventWriter,
    intervention: AgentRunInterventionV1,
) -> Result<AgentAnswerNowResponse, String> {
    intervention.validate()?;
    let agent_run_id = required_string(intervention.agent_run_id.as_str(), "agentRunId")?;
    let active = event_writer
        .active_agent_run(agent_run_id.as_str())?
        .ok_or_else(|| "agentRunNotActive".to_string())?;
    let store = agent_runtime_store_actor()?;
    let mut requested_event = None;
    let disposition = active
        .control
        .enqueue_answer_now_with(intervention.clone(), || {
            requested_event = Some(persist_answer_now_requested_fact(
                &store,
                active.lease.session_id.as_str(),
                active.turn_id.as_str(),
                &intervention,
                "electron.host_bridge",
            )?);
            Ok(())
        })?;
    if disposition == AnswerNowEnqueueDisposition::AlreadyConverging {
        requested_event = match persist_answer_now_requested_fact(
            &store,
            active.lease.session_id.as_str(),
            active.turn_id.as_str(),
            &intervention,
            "electron.host_bridge",
        ) {
            Ok(event) => Some(event),
            Err(error) if error == "alreadyConverging" => None,
            Err(error) => return Err(error),
        };
    }
    let _ = requested_event;
    let (accepted, disposition) = match disposition {
        AnswerNowEnqueueDisposition::Accepted => (true, "accepted"),
        AnswerNowEnqueueDisposition::AlreadyConverging => (false, "alreadyConverging"),
    };
    Ok(AgentAnswerNowResponse {
        accepted,
        disposition,
        session_id: active.lease.session_id,
        agent_run_id,
        intervention_id: intervention.intervention_id,
    })
}

pub(crate) async fn question_answer_async(
    event_writer: EventWriter,
    request: AgentQuestionAnswerRequest,
) -> Result<AgentInputResponse, String> {
    let session_id = normalize_session_id(request.session_id.as_deref());
    let question_id = required_string(request.question_id.as_str(), "questionId")?;
    let answer_message = build_question_answer_message(
        question_id.as_str(),
        request.answers.as_deref().unwrap_or(&[]),
        request.answer_text.as_deref(),
    )?;
    let actor = agent_runtime_store_actor()?;
    let resume_from_turn_id = latest_paused_question_turn_id(&actor, session_id.as_str()).await?;
    if let Some(active) = event_writer.active_agent_run_for_session(session_id.as_str())? {
        let agent_run_id = active.lease.agent_run_id.clone();
        enqueue_supplement(
            &event_writer,
            AgentSupplementRequest {
                session_id: session_id.clone(),
                agent_run_id: agent_run_id.clone(),
                message: answer_message,
            },
        )?;
        return Ok(AgentInputResponse {
            session_id,
            agent_run_id,
            turn_id: active.turn_id,
            stream_items: Vec::new(),
        });
    }
    let agent_run = start_agent_run_blocking(StartAgentRunRequest {
        event_writer,
        session_id: session_id.clone(),
        message: answer_message,
        rewrite_last_user_input: None,
        auto_continue_after_resume_wait: request.auto_continue_after_resume_wait,
        resume_from_turn_id: Some(resume_from_turn_id),
        attachments: Vec::new(),
        requested_agent_run_id: None,
        requested_turn_id: None,
    })
    .await?;
    Ok(AgentInputResponse {
        session_id,
        agent_run_id: agent_run.agent_run_id,
        turn_id: agent_run.turn_id,
        stream_items: Vec::new(),
    })
}

struct StartAgentRunRequest {
    event_writer: EventWriter,
    session_id: String,
    message: String,
    rewrite_last_user_input: Option<RewriteLastUserInput>,
    auto_continue_after_resume_wait: Option<bool>,
    resume_from_turn_id: Option<String>,
    attachments: Vec<crate::local_attachments::LocalImageInputRequest>,
    requested_agent_run_id: Option<String>,
    requested_turn_id: Option<String>,
}

#[derive(Clone, Debug)]
struct RewriteLastUserInput {
    target_chat_message_id: String,
    expected_tail_chat_message_id: String,
}

struct StartedAgentRun {
    agent_run_id: String,
    turn_id: String,
}

async fn start_agent_run_blocking(
    request: StartAgentRunRequest,
) -> Result<StartedAgentRun, String> {
    tokio::task::spawn_blocking(move || start_agent_run(request))
        .await
        .map_err(|error| format!("agent stream agent run blocking owner join failed: {error}"))?
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedAgentRuntime {
    cwd: PathBuf,
}

fn resolve_agent_runtime(session_id: &str) -> Result<ResolvedAgentRuntime, String> {
    let normalized_session_id = session_id.trim();
    if normalized_session_id.is_empty() {
        return Err(String::from("sessionId is required"));
    }
    let binding = sessions::runtime_binding_for_session_id(normalized_session_id)?;
    resolve_agent_runtime_from_binding(&binding)
}

fn resolve_agent_runtime_from_binding(
    binding: &sessions::AgentRuntimeBinding,
) -> Result<ResolvedAgentRuntime, String> {
    let cwd = workspaces::normalize_workspace_root_text(binding.cwd.as_str())
        .ok_or_else(|| format!("working directory is not a directory: {}", binding.cwd))?;
    Ok(ResolvedAgentRuntime {
        cwd: PathBuf::from(cwd),
    })
}

fn start_agent_run(mut request: StartAgentRunRequest) -> Result<StartedAgentRun, String> {
    let agent_runtime = resolve_agent_runtime(request.session_id.as_str())?;
    let started_at_ms = current_timestamp_ms();
    let turn_id = request
        .requested_turn_id
        .clone()
        .unwrap_or_else(new_turn_id);
    let started_turn_id = turn_id.clone();
    let agent_run_id = request.requested_agent_run_id.clone().unwrap_or_else(|| {
        let sequence = AGENT_RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("agent-run-{started_at_ms}-{sequence}")
    });
    let started_agent_run_id = agent_run_id.clone();
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|error| format!("session/prompt requires Tokio runtime: {error}"))?;
    let supplement_event_writer = request.event_writer.clone();
    let supplement_session_id = request.session_id.clone();
    let supplement_agent_run_id = agent_run_id.clone();
    let turn_control =
        TurnControl::new_with_supplement_materializer(Arc::new(move |turn_id, supplements| {
            let payloads = run_message_log_blocking("persist consumed turn supplements", || {
                message_log::append_turn_supplements(
                    supplement_session_id.as_str(),
                    turn_id,
                    supplement_agent_run_id.as_str(),
                    supplements,
                )
            })?;
            emit_annotated_agent_run_payloads(
                &supplement_event_writer,
                supplement_agent_run_id.as_str(),
                supplement_session_id.as_str(),
                payloads,
            )?;
            Ok(())
        }));
    let agent_run_lease = request.event_writer.start_agent_run(
        request.session_id.as_str(),
        agent_run_id.as_str(),
        turn_id.as_str(),
        turn_control.clone(),
    )?;
    let active_agent_run_registration = ActiveAgentRunRegistration {
        event_writer: request.event_writer.clone(),
        agent_run_lease,
    };
    request.event_writer = request.event_writer.for_agent_run(
        active_agent_run_registration
            .agent_run_lease
            .lease_id
            .as_str(),
    )?;
    recover_incomplete_tool_calls_before_new_turn(request.session_id.as_str())?;
    restore_runtime_snapshot_from_session(request.session_id.as_str())?;
    let runtime_config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {})
        .map_err(|error| {
            persist_agent_turn_setup_error(
                request.session_id.as_str(),
                turn_id.as_str(),
                agent_run_id.as_str(),
                error,
            )
        })?;
    if !request.attachments.is_empty() {
        ensure_selected_model_supports_vision(&runtime_config)?;
    }
    let attachments = crate::local_attachments::import_local_images(
        &request.attachments,
        request.message.as_str(),
    )?;
    persist_agent_run_started(
        &request,
        turn_id.as_str(),
        agent_run_id.as_str(),
        started_at_ms,
        attachments,
    )?;
    restore_runtime_snapshot_from_session(request.session_id.as_str())?;
    if request.rewrite_last_user_input.is_some() {
        agent_runs::start_agent_run(
            agent_run_id.as_str(),
            request.session_id.as_str(),
            turn_id.as_str(),
        )
        .map_err(|error| {
            persist_agent_turn_setup_error(
                request.session_id.as_str(),
                turn_id.as_str(),
                agent_run_id.as_str(),
                error,
            )
        })?;
    }
    handle.spawn(async move {
        let _active_agent_run_registration = active_agent_run_registration;
        let event_writer_for_error = request.event_writer.clone();
        let session_id_for_error = request.session_id.clone();
        let agent_run_id_for_error = agent_run_id.clone();
        let turn_id_for_error = turn_id.clone();
        let result = run_session_agent_run(
            request,
            agent_run_id.clone(),
            turn_id.clone(),
            runtime_config,
            agent_runtime,
            turn_control.clone(),
        )
        .await;
        if let Err(error) = result {
            let _ = emit_agent_run_payload_blocking(
                &event_writer_for_error,
                agent_run_id_for_error.as_str(),
                session_id_for_error.as_str(),
                turn_id_for_error.as_str(),
                runtime_error_stream_payload(
                    session_id_for_error.as_str(),
                    turn_id_for_error.as_str(),
                    error.as_str(),
                    "session_prompt_agent_run_failed",
                    false,
                ),
            );
            let _ = run_message_log_blocking("persist failed agent run", || {
                close_incomplete_tool_calls_for_agent_run(
                    session_id_for_error.as_str(),
                    agent_run_id_for_error.as_str(),
                )?;
                persist_agent_run_failed(
                    session_id_for_error.as_str(),
                    turn_id_for_error.as_str(),
                    agent_run_id_for_error.as_str(),
                    error.as_str(),
                )?;
                let _ = agent_runs::fail_agent_run(
                    agent_run_id_for_error.as_str(),
                    session_id_for_error.as_str(),
                    turn_id_for_error.as_str(),
                    error.as_str(),
                )?;
                Ok(())
            });
            eprintln!("centaeris electron session/prompt agent run failed: {error}");
        }
    });
    Ok(StartedAgentRun {
        agent_run_id: started_agent_run_id,
        turn_id: started_turn_id,
    })
}

fn restore_runtime_snapshot_from_session(session_id: &str) -> Result<(), String> {
    let snapshot = message_log::restore_runtime_snapshot(session_id)?;
    SessionManager::new(agent_runtime_store_actor()?).save_session(&snapshot)
}

fn persist_agent_run_started(
    request: &StartAgentRunRequest,
    turn_id: &str,
    agent_run_id: &str,
    started_at_ms: i64,
    attachments: Vec<Value>,
) -> Result<(), String> {
    if let Some(rewrite) = &request.rewrite_last_user_input {
        let result =
            message_log::rewrite_last_user_input(message_log::RewriteLastUserInputRequest {
                session_id: request.session_id.as_str(),
                target_chat_message_id: rewrite.target_chat_message_id.as_str(),
                expected_tail_chat_message_id: rewrite.expected_tail_chat_message_id.as_str(),
                new_turn_id: turn_id,
                new_agent_run_id: agent_run_id,
                new_content: request.message.as_str(),
                reason: "rewrite_last_user_input",
                at_ms: started_at_ms,
            })?;
        if result.tombstoned_count == 0 {
            return Err(String::from("rewrite did not tombstone any tail records"));
        }
    } else {
        message_log::append_agent_run_started_with_attachments(
            request.session_id.as_str(),
            turn_id,
            agent_run_id,
            request.message.as_str(),
            attachments,
            started_at_ms,
        )?;
    }
    sessions::record_session_activity(
        request.session_id.as_str(),
        Some(request.message.as_str()),
        Some(request.message.as_str()),
        started_at_ms,
    )
}

fn persist_agent_run_completed(
    session_id: &str,
    final_turn_id: &str,
    agent_run_id: &str,
    final_answer: Option<&str>,
    stop: &AgentRunStop,
) -> Result<(), String> {
    let final_answer = match final_answer {
        Some(final_answer) => final_answer,
        None if *stop == AgentRunStop::Finalized => {
            return Err(String::from(
                "finalized agent turn is missing the final assistant event",
            ));
        }
        None => "",
    };
    let completed_at_ms = current_timestamp_ms();
    let assistant = message_log::project_agent_run_assistant(session_id, agent_run_id)?
        .filter(|message| message.turn_id == final_turn_id)
        .ok_or_else(|| format!("committed final assistant message is missing: {final_turn_id}"))?;
    if assistant.id != format!("message:{final_turn_id}:assistant")
        || assistant.content != final_answer
        || assistant.status.as_deref() != Some("done")
    {
        return Err(format!(
            "committed final assistant message mismatch: {final_turn_id}"
        ));
    }
    sessions::record_session_activity(session_id, None, Some(final_answer), completed_at_ms)
}

fn persist_agent_run_failed(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    error: &str,
) -> Result<(), String> {
    let failed_at_ms = current_timestamp_ms();
    let preview = match message_log::project_agent_run_assistant(session_id, agent_run_id)? {
        Some(committed) if matches!(committed.status.as_deref(), Some("done" | "error")) => {
            committed.content
        }
        Some(committed) => {
            return Err(format!(
                "committed assistant message has unsupported status: {:?}",
                committed.status
            ));
        }
        None => {
            message_log::append_assistant_message(
                session_id,
                turn_id,
                Some(agent_run_id),
                error,
                "error",
                failed_at_ms,
            )?
            .content
        }
    };
    sessions::record_session_activity(session_id, None, Some(preview.as_str()), failed_at_ms)
}

fn persist_agent_checkpoint_refs(
    session_id: &str,
    agent_run_id: &str,
    turn_responses: &[TurnStepResult],
) -> Result<(), String> {
    for turn_response in turn_responses {
        let Some(checkpoint) = turn_response.checkpoint.as_ref() else {
            continue;
        };
        if checkpoint.session_id != session_id {
            return Err(format!(
                "checkpoint sessionId mismatch: expected {session_id}, got {}",
                checkpoint.session_id
            ));
        }
        message_log::append_checkpoint_ref(checkpoint, agent_run_id)?;
    }
    Ok(())
}

fn persist_agent_turn_setup_error(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    error: String,
) -> String {
    if let Err(close_error) = close_incomplete_tool_calls_for_agent_run(session_id, agent_run_id) {
        return format!(
            "{error}; additionally failed to close incomplete tool calls: {close_error}"
        );
    }
    if let Err(message_error) =
        persist_agent_run_failed(session_id, turn_id, agent_run_id, error.as_str())
    {
        return format!(
            "{error}; additionally failed to persist assistant error message: {message_error}"
        );
    }
    match agent_runs::fail_agent_run(agent_run_id, session_id, turn_id, error.as_str()) {
        Ok(_) => error,
        Err(lifecycle_error) => format!(
            "{error}; additionally failed to persist failed agent run lifecycle: {lifecycle_error}"
        ),
    }
}

async fn run_agent_query_loop(
    runtime: &AgentRuntime<RuntimeStoreActor>,
    runtime_config: &runtime_config::AgentRuntimeConfigResponse,
    request: AgentRunRequest,
    stream_sink: &mut (dyn FnMut(TurnUpdate) + Send),
    cancellation_probe: &(dyn Fn() -> Result<Option<String>, String> + Sync),
    turn_control: &TurnControl,
    tool_safe_point: &mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send),
) -> Result<centaeris_core::runtime::AgentRunResult, String> {
    let (model_config, registry) = model_session_config_and_registry(runtime_config)?;
    let config_store = SingleModelSessionConfigStore {
        session_id: request.session_id.clone(),
        config: model_config.clone(),
    };
    let transport = ReqwestJsonHttpTransport::new()?;
    match model_config_wire_api(&registry, &model_config)? {
        WireApi::AnthropicMessages => {
            let client = AnthropicMessagesModelClient::new(registry, transport);
            runtime
                .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                    request,
                    &client,
                    &config_store,
                    stream_sink,
                    cancellation_probe,
                    turn_control,
                    tool_safe_point,
                )
                .await
        }
        WireApi::OpenAiResponses => {
            let client = OpenAiResponsesModelClient::new(registry, transport);
            let response = runtime
                .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                    request,
                    &client,
                    &config_store,
                    stream_sink,
                    cancellation_probe,
                    turn_control,
                    tool_safe_point,
                )
                .await;
            response
        }
        WireApi::OpenAiChatCompletions => {
            let client = OpenAiCompatibleModelClient::new(registry, transport);
            let response = runtime
                .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                    request,
                    &client,
                    &config_store,
                    stream_sink,
                    cancellation_probe,
                    turn_control,
                    tool_safe_point,
                )
                .await;
            response
        }
        unsupported => Err(format!(
            "model wire API {:?} is not supported by the Local Runtime Host",
            unsupported
        )),
    }
}

async fn run_manual_context_compaction(
    runtime: &AgentRuntime<RuntimeStoreActor>,
    runtime_config: &runtime_config::AgentRuntimeConfigResponse,
    session_id: &str,
    turn_id: &str,
    tool_safe_point: &mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send),
) -> Result<bool, String> {
    let (model_config, registry) = model_session_config_and_registry(runtime_config)?;
    let config_store = SingleModelSessionConfigStore {
        session_id: session_id.to_string(),
        config: model_config.clone(),
    };
    let transport = ReqwestJsonHttpTransport::new()?;
    match model_config_wire_api(&registry, &model_config)? {
        WireApi::AnthropicMessages => {
            let client = AnthropicMessagesModelClient::new(registry, transport);
            runtime
                .compact_session_online_with_model_client_and_tool_safe_point_async(
                    session_id,
                    turn_id,
                    &client,
                    &config_store,
                    tool_safe_point,
                )
                .await
        }
        WireApi::OpenAiResponses => {
            let client = OpenAiResponsesModelClient::new(registry, transport);
            runtime
                .compact_session_online_with_model_client_and_tool_safe_point_async(
                    session_id,
                    turn_id,
                    &client,
                    &config_store,
                    tool_safe_point,
                )
                .await
        }
        WireApi::OpenAiChatCompletions => {
            let client = OpenAiCompatibleModelClient::new(registry, transport);
            runtime
                .compact_session_online_with_model_client_and_tool_safe_point_async(
                    session_id,
                    turn_id,
                    &client,
                    &config_store,
                    tool_safe_point,
                )
                .await
        }
        unsupported => Err(format!(
            "model wire API {:?} is not supported by the Local Runtime Host",
            unsupported
        )),
    }
}

pub(crate) struct AgentRuntimeBuildRequest {
    pub(crate) store: RuntimeStoreActor,
    pub(crate) session_id: String,
    pub(crate) concurrency_scope_id: String,
    pub(crate) cwd: PathBuf,
    pub(crate) execution_owner: String,
    pub(crate) bash_path: Option<PathBuf>,
    pub(crate) runtime_config: AgentRuntimeConfig,
    pub(crate) native_plugin_activation: Option<mcp::NativePluginActivation>,
    pub(crate) execution_cancellation_probe: Option<Arc<ExecutionCancellationProbe>>,
    pub(crate) file_mutation_commit_port: Option<Arc<dyn FileMutationCommitPort + Send + Sync>>,
}

pub(crate) fn build_agent_runtime(
    request: AgentRuntimeBuildRequest,
) -> Result<AgentRuntime<RuntimeStoreActor>, String> {
    let AgentRuntimeBuildRequest {
        store,
        session_id,
        concurrency_scope_id,
        cwd,
        execution_owner,
        bash_path,
        runtime_config,
        native_plugin_activation,
        execution_cancellation_probe,
        file_mutation_commit_port,
    } = request;
    if session_id.trim().is_empty() {
        return Err("sessionId is required".to_string());
    }
    if execution_owner.trim().is_empty() {
        return Err("executionOwner is required".to_string());
    }
    if concurrency_scope_id.trim().is_empty() {
        return Err("concurrencyScopeId is required".to_string());
    }
    let (
        dynamic_tool_registry,
        providers,
        plugin_skill_sources,
        command_environment,
        lifecycle_hooks,
    ) = native_plugin_activation
        .map(|activation| {
            (
                activation.dynamic_tool_registry,
                activation.providers,
                activation.skill_sources,
                activation.command_environment,
                activation.lifecycle_hooks,
            )
        })
        .unwrap_or_else(|| {
            (
                Arc::new(DynamicToolRegistry::empty()),
                Vec::new(),
                Vec::new(),
                HashMap::new(),
                QueryLifecycleHookRuntime::empty(),
            )
        });
    let local_runner = Arc::new(
        centaeris_runtime::local_execution_host::LocalExecutionHostRunner::new(bash_path)
            .and_then(|runner| runner.with_environment_overrides(command_environment))
            .map_err(|error| error.internal_debug_message())?,
    );
    let mut skill_catalog_config = skills::skill_catalog_config_for_workspace_root(cwd.as_path())?;
    skill_catalog_config
        .sources_config
        .sources
        .extend(plugin_skill_sources);
    let execution_host_binding = Arc::new(ExecutionHostBinding::new(
        ExecutionHostMode::Local,
        local_runner,
        cwd.clone(),
        SandboxPolicy::workspace_write_no_network(cwd.as_path()),
    )?);
    let mut tool_layer = ToolLayer::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(
            skill_catalog_config,
            dynamic_tool_registry,
            execution_host_binding,
        )?
        .with_network_policy(NetworkSandboxPolicy::PublicInternet)
        .with_session_id(session_id)
        .with_execution_owner(execution_owner)
        .with_resource_claim_store(Arc::new(store.clone()));
    for provider in providers {
        tool_layer.register_dynamic_tool_provider(provider)?;
    }
    if let Some(cancellation_probe) = execution_cancellation_probe {
        tool_layer = tool_layer.with_execution_cancellation_probe(cancellation_probe);
    }
    if let Some(port) = file_mutation_commit_port {
        tool_layer = tool_layer.with_file_mutation_commit_port(port);
    }
    let tool_concurrency = ToolConcurrencyCoordinator::global_for_scope(
        format!("session:{concurrency_scope_id}"),
        runtime_config.tool_parallelism,
    )?;
    Ok(
        AgentRuntime::new(store, tool_layer, runtime_config, tool_concurrency)
            .with_lifecycle_hooks(lifecycle_hooks)
            .with_model_input_image_resolver(Arc::new(
                crate::local_attachments::LocalModelInputImageResolver,
            )),
    )
}

pub(crate) fn native_agent_run_identity(
    agent_run_id: &str,
    session_id: &str,
    cwd: &std::path::Path,
) -> Result<RuntimeAgentRunIdentityV1, String> {
    let preimage = serde_json::to_vec(&serde_json::json!({
        "schema": "native_agent_run_authorization.v1",
        "agentRunId": agent_run_id,
        "sessionId": session_id,
        "cwd": cwd.to_string_lossy(),
    }))
    .map_err(|error| format!("serialize native AgentRun authorization failed: {error}"))?;
    let identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: agent_run_id.to_string(),
        execution_id: format!(
            "execution:{:x}",
            Sha256::digest(format!("native_agent_run_execution_v1:{agent_run_id}").as_bytes())
        ),
        authorization_digest: format!("sha256:{:x}", Sha256::digest(preimage)),
    };
    identity.validate()?;
    Ok(identity)
}

pub(crate) fn persist_agent_tool_safe_point(
    event_writer: &EventWriter,
    session_id: &str,
    agent_run_id: &str,
    safe_point: ToolSafePoint,
) -> Result<(), String> {
    match safe_point {
        ToolSafePoint::ModelRequestStarted(started) => {
            let compaction_update =
                (started.purpose() == ModelRequestPurposeV1::Compaction).then(|| {
                    TurnUpdate::ModelRequestStart {
                        session_id: started.session_id().to_string(),
                        turn_id: started.turn_id().to_string(),
                        purpose: started.purpose(),
                        context_token_estimate: started.context_token_estimate(),
                        message: None,
                        process_state:
                            centaeris_core::runtime::contracts::RuntimeProcessState::Compressing,
                        elapsed_ms: 0,
                        initial_content: String::new(),
                    }
                });
            run_message_log_blocking("persist model request start", || {
                message_log::append_model_request_started(
                    agent_run_id,
                    &started,
                    centaeris_core::runtime::contracts::current_timestamp_ms(),
                )
            })?;
            if let Some(update) = compaction_update {
                emit_background_turn_update_blocking(
                    event_writer,
                    agent_run_id,
                    session_id,
                    update,
                )?;
            }
            Ok(())
        }
        ToolSafePoint::ProviderUsage {
            turn_id,
            usage,
            recorded_at_ms,
        } => run_message_log_blocking("persist provider usage", || {
            message_log::append_provider_usage(
                session_id,
                turn_id.as_str(),
                agent_run_id,
                &usage,
                recorded_at_ms,
            )
        }),
        ToolSafePoint::DurableToolCall {
            session_id: source_session_id,
            turn_id: source_turn_id,
            agent_run_id: source_agent_run_id,
            call,
            provider_id,
            tool_contract_digest,
            recorded_at_ms,
        } => {
            if source_session_id != session_id || source_agent_run_id != agent_run_id {
                return Err("tool safe point AgentRun identity mismatch".to_string());
            }
            let payloads = run_message_log_blocking("persist canonical tool call intent", || {
                message_log::append_tool_call(
                    source_session_id.as_str(),
                    source_turn_id.as_str(),
                    source_agent_run_id.as_str(),
                    &call,
                    provider_id.as_str(),
                    tool_contract_digest.as_str(),
                    recorded_at_ms,
                )
            })?;
            emit_annotated_agent_run_payloads(
                event_writer,
                source_agent_run_id.as_str(),
                source_session_id.as_str(),
                payloads,
            )?;
            Ok(())
        }
        ToolSafePoint::DurableReceipt {
            session_id: source_session_id,
            turn_id: source_turn_id,
            agent_run_id: source_agent_run_id,
            call,
            result,
        } => {
            if source_session_id != session_id || source_agent_run_id != agent_run_id {
                return Err("tool safe point AgentRun identity mismatch".to_string());
            }
            let payloads = run_message_log_blocking("persist canonical tool result", || {
                message_log::append_tool_result(
                    source_session_id.as_str(),
                    source_turn_id.as_str(),
                    source_agent_run_id.as_str(),
                    &call,
                    &result,
                )
            })?;
            emit_annotated_agent_run_payloads(
                event_writer,
                source_agent_run_id.as_str(),
                source_session_id.as_str(),
                payloads,
            )?;
            Ok(())
        }
        ToolSafePoint::CompletedTurn(_) => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeJobWaitWake {
    JobsTerminal,
    AnswerNow,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QuestionWaitWake {
    Resume { turn_id: String, message: String },
    AnswerNow,
    Cancelled,
}

fn wait_checkpoint_turn_id(
    response: &centaeris_core::runtime::AgentRunResult,
    done_reason: &str,
) -> Result<String, String> {
    response
        .turn_responses
        .iter()
        .rev()
        .filter_map(|step| step.checkpoint.as_ref())
        .find(|checkpoint| checkpoint.done_reason.as_deref() == Some(done_reason))
        .map(|checkpoint| checkpoint.turn_id.clone())
        .ok_or_else(|| {
            format!(
                "{} response missing {} checkpoint",
                response.stop.reason(),
                done_reason
            )
        })
}

fn runtime_job_wait_checkpoint(
    response: &centaeris_core::runtime::AgentRunResult,
) -> Result<(String, RuntimeAwaitJobCheckpointV1), String> {
    let step = response
        .turn_responses
        .iter()
        .rev()
        .filter_map(|step| step.checkpoint.as_ref())
        .find(|checkpoint| checkpoint.done_reason.as_deref() == Some("runtime_job"))
        .ok_or_else(|| "runtime_job_wait response missing runtime_job checkpoint".to_string())?;
    let checkpoint =
        serde_json::from_str::<RuntimeAwaitJobCheckpointV1>(step.payload_json.as_str())
            .map_err(|error| format!("decode runtime job wait checkpoint failed: {error}"))?;
    checkpoint.validate()?;
    Ok((step.turn_id.clone(), checkpoint))
}

fn resume_execute_agent_run_request(
    session_id: &str,
    agent_run_identity: &RuntimeAgentRunIdentityV1,
    wait_turn_id: &str,
    initial_turn_id: String,
    resume_intent: AgentRunResumeIntent,
    auto_continue_after_resume_wait: Option<bool>,
) -> AgentRunRequest {
    AgentRunRequest {
        session_id: session_id.to_string(),
        agent_run_identity: Some(agent_run_identity.clone()),
        initial_turn_id,
        user_message: resume_intent.into_user_message(),
        runtime_scope: PromptCompactionScopeV1::main(),
        resume_from_turn_id: Some(wait_turn_id.to_string()),
        auto_continue_after_resume_wait,
    }
}

async fn wait_for_runtime_jobs_or_control(
    event_writer: &EventWriter,
    store: &RuntimeStoreActor,
    agent_run_id: &str,
    session_id: &str,
    agent_run_identity: &RuntimeAgentRunIdentityV1,
    checkpoint: &RuntimeAwaitJobCheckpointV1,
    turn_control: &TurnControl,
) -> Result<RuntimeJobWaitWake, String> {
    if checkpoint.agent_run_id != agent_run_identity.agent_run_id
        || checkpoint.authorization_digest != agent_run_identity.authorization_digest
    {
        return Err("runtime job wait AgentRun identity mismatch".to_string());
    }
    loop {
        if event_writer
            .agent_run_cancellation_reason(agent_run_id)?
            .is_some()
        {
            return Ok(RuntimeJobWaitWake::Cancelled);
        }
        if turn_control.is_answer_now_requested()? {
            return Ok(RuntimeJobWaitWake::AnswerNow);
        }
        let mut all_terminal = true;
        for wait in &checkpoint.waits {
            let job = store
                .get_runtime_job(wait.job_id.as_str())
                .await?
                .ok_or_else(|| format!("runtime_job_wait_job_missing: jobId={}", wait.job_id))?;
            if job.session_id.as_deref() != Some(session_id) || job.job_kind != wait.job_kind {
                return Err(format!(
                    "runtime_job_wait_job_identity_mismatch: jobId={}",
                    wait.job_id
                ));
            }
            all_terminal &= job.status.is_terminal();
        }
        if all_terminal {
            return Ok(RuntimeJobWaitWake::JobsTerminal);
        }
        tokio::select! {
            answer_now = turn_control.wait_for_answer_now_or_close() => {
                return Ok(if answer_now? {
                    RuntimeJobWaitWake::AnswerNow
                } else {
                    RuntimeJobWaitWake::Cancelled
                });
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(RUNTIME_JOB_WAIT_POLL_MS)) => {}
        }
    }
}

async fn wait_for_question_input_or_control(
    event_writer: &EventWriter,
    agent_run_id: &str,
    turn_control: &TurnControl,
) -> Result<QuestionWaitWake, String> {
    loop {
        if event_writer
            .agent_run_cancellation_reason(agent_run_id)?
            .is_some()
        {
            return Ok(QuestionWaitWake::Cancelled);
        }
        if turn_control.is_answer_now_requested()? {
            return Ok(QuestionWaitWake::AnswerNow);
        }
        let turn_id = new_turn_id();
        if let Some(message) = turn_control.take_resume_message(turn_id.as_str())? {
            return Ok(QuestionWaitWake::Resume { turn_id, message });
        }
        if !turn_control.wait_for_pending_or_close().await? {
            return Ok(QuestionWaitWake::Cancelled);
        }
    }
}

async fn run_session_agent_run(
    request: StartAgentRunRequest,
    agent_run_id: String,
    turn_id: String,
    runtime_config: runtime_config::AgentRuntimeConfigResponse,
    agent_runtime: ResolvedAgentRuntime,
    turn_control: TurnControl,
) -> Result<(), String> {
    let StartAgentRunRequest {
        event_writer,
        session_id,
        message,
        auto_continue_after_resume_wait,
        resume_from_turn_id,
        ..
    } = request;
    let store = agent_runtime_store_actor()?;
    let cancellation_agent_run_id = agent_run_id.clone();
    let cancellation_event_writer = event_writer.clone();
    let execution_cancellation_probe: Arc<ExecutionCancellationProbe> = Arc::new(move || {
        cancellation_event_writer.agent_run_cancellation_reason(cancellation_agent_run_id.as_str())
    });
    let agent_run_identity = native_agent_run_identity(
        agent_run_id.as_str(),
        session_id.as_str(),
        &agent_runtime.cwd,
    )?;
    let mut engine_config = agent_runtime_config(
        session_id.as_str(),
        &runtime_config,
        auto_continue_after_resume_wait,
    )?;
    let native_plugin_activation = mcp::connect_enabled_plugins().await?;
    engine_config.plugin_activation_digest = Some(native_plugin_activation.digest.clone());
    let runtime = build_agent_runtime(AgentRuntimeBuildRequest {
        store: store.clone(),
        session_id: session_id.clone(),
        concurrency_scope_id: session_id.clone(),
        cwd: agent_runtime.cwd.clone(),
        execution_owner: agent_run_identity.agent_run_id.clone(),
        bash_path: runtime_config.bash_path.as_deref().map(PathBuf::from),
        runtime_config: engine_config,
        native_plugin_activation: Some(native_plugin_activation),
        execution_cancellation_probe: Some(execution_cancellation_probe),
        file_mutation_commit_port: Some(file_mutation_commit_port(
            session_id.as_str(),
            turn_id.as_str(),
            agent_run_id.as_str(),
        )),
    })?;
    let mut request = AgentRunRequest {
        session_id: session_id.clone(),
        agent_run_identity: Some(agent_run_identity.clone()),
        initial_turn_id: turn_id.clone(),
        user_message: message,
        runtime_scope: PromptCompactionScopeV1::main(),
        resume_from_turn_id,
        auto_continue_after_resume_wait,
    };
    let mut final_answer = None::<String>;
    let mut completed_projection = None;
    let mut streamed_runtime_error = false;
    let mut live_text =
        LiveTextAccumulator::new(session_id.clone(), turn_id.clone(), agent_run_id.clone());
    let loop_result: Result<AgentRunStop, String> = async {
        loop {
            let mut stream_persistence_error = None::<String>;
            let cancellation_probe =
                || event_writer.agent_run_cancellation_reason(agent_run_id.as_str());
            let response = {
                let mut tool_safe_point = |safe_point: ToolSafePoint| {
                    persist_agent_tool_safe_point(
                        &event_writer,
                        session_id.as_str(),
                        agent_run_id.as_str(),
                        safe_point,
                    )
                };
                let mut stream_sink = |event: TurnUpdate| {
                    if stream_persistence_error.is_some() {
                        return;
                    }
                    if matches!(event, TurnUpdate::RuntimeError { .. }) {
                        streamed_runtime_error = true;
                    }
                    if let TurnUpdate::ModelRequestStart {
                        turn_id,
                        initial_content,
                        ..
                    } = &event
                    {
                        let live_payloads = match live_text
                            .begin_model_request(turn_id, initial_content.as_str())
                        {
                            Ok(payloads) => payloads,
                            Err(error) => {
                                stream_persistence_error = Some(error);
                                return;
                            }
                        };
                        if let Err(error) = emit_live_text_payloads(
                            &event_writer,
                            agent_run_id.as_str(),
                            session_id.as_str(),
                            live_payloads,
                        ) {
                            stream_persistence_error = Some(error);
                            return;
                        }
                    }
                    if matches!(event, TurnUpdate::ModelDone { .. }) {
                        let live_payloads = match live_text.flush() {
                            Ok(payloads) => payloads,
                            Err(error) => {
                                stream_persistence_error = Some(error);
                                return;
                            }
                        };
                        if let Err(error) = emit_live_text_payloads(
                            &event_writer,
                            agent_run_id.as_str(),
                            session_id.as_str(),
                            live_payloads,
                        ) {
                            stream_persistence_error = Some(error);
                            return;
                        }
                    }
                    if let Err(error) =
                        materialize_spawned_agent_session_blocking(session_id.as_str(), &event)
                    {
                        stream_persistence_error = Some(error);
                        return;
                    }
                    let committed_payload = match &event {
                        TurnUpdate::RuntimeEvent { event } => {
                            run_message_log_blocking("persist canonical runtime event fact", || {
                                message_log::append_runtime_event_fact(event, agent_run_id.as_str())
                            })
                        }
                        _ => Ok(None),
                    };
                    let payload = match committed_payload {
                        Ok(Some(payload)) => payload,
                        Ok(None) => match project_turn_update(event) {
                            Ok(Some(payload)) => payload,
                            Ok(None) => return,
                            Err(error) => {
                                stream_persistence_error = Some(error);
                                return;
                            }
                        },
                        Err(error) => {
                            stream_persistence_error = Some(error);
                            return;
                        }
                    };
                    if let Some(content) = final_answer_from_stream_payload(&payload) {
                        final_answer = Some(content);
                    }
                    if is_live_text_payload(&payload) {
                        let live_payloads = match live_text.push(payload) {
                            Ok(payloads) => payloads,
                            Err(error) => {
                                stream_persistence_error = Some(error);
                                return;
                            }
                        };
                        if let Err(error) = emit_live_text_payloads(
                            &event_writer,
                            agent_run_id.as_str(),
                            session_id.as_str(),
                            live_payloads,
                        ) {
                            stream_persistence_error = Some(error);
                        }
                        return;
                    }
                    let live_payloads = match live_text.flush() {
                        Ok(payloads) => payloads,
                        Err(error) => {
                            stream_persistence_error = Some(error);
                            return;
                        }
                    };
                    if let Err(error) = emit_live_text_payloads(
                        &event_writer,
                        agent_run_id.as_str(),
                        session_id.as_str(),
                        live_payloads,
                    ) {
                        stream_persistence_error = Some(error);
                        return;
                    }
                    if let Err(error) = emit_annotated_agent_run_payloads(
                        &event_writer,
                        agent_run_id.as_str(),
                        session_id.as_str(),
                        vec![payload],
                    ) {
                        stream_persistence_error = Some(error);
                    }
                };
                run_agent_query_loop(
                    &runtime,
                    &runtime_config,
                    request,
                    &mut stream_sink,
                    &cancellation_probe,
                    &turn_control,
                    &mut tool_safe_point,
                )
                .await?
            };
            let live_payloads = live_text.flush()?;
            emit_live_text_payloads(
                &event_writer,
                agent_run_id.as_str(),
                session_id.as_str(),
                live_payloads,
            )?;
            if let Some(error) = stream_persistence_error {
                return Err(format!("persist stream item failed: {error}"));
            }
            persist_agent_checkpoint_refs(
                session_id.as_str(),
                agent_run_id.as_str(),
                response.turn_responses.as_slice(),
            )?;

            match &response.stop {
                AgentRunStop::Finalized | AgentRunStop::TerminalTool => {
                    completed_projection = Some(completed_turn_projection_from_result(
                        session_id.as_str(),
                        &agent_run_identity,
                        &response,
                    )?);
                    return Ok(response.stop.clone());
                }
                AgentRunStop::Cancelled(reason) => {
                    return Ok(AgentRunStop::Cancelled(reason.clone()));
                }
                AgentRunStop::RuntimeJobWait => {
                    let (wait_turn_id, wait_checkpoint) = runtime_job_wait_checkpoint(&response)?;
                    match wait_for_runtime_jobs_or_control(
                        &event_writer,
                        &store,
                        agent_run_id.as_str(),
                        session_id.as_str(),
                        &agent_run_identity,
                        &wait_checkpoint,
                        &turn_control,
                    )
                    .await?
                    {
                        RuntimeJobWaitWake::Cancelled => {
                            return Ok(AgentRunStop::Cancelled("agent_run_cancelled".to_string()));
                        }
                        RuntimeJobWaitWake::JobsTerminal => {
                            request = resume_execute_agent_run_request(
                                session_id.as_str(),
                                &agent_run_identity,
                                wait_turn_id.as_str(),
                                new_turn_id(),
                                AgentRunResumeIntent::RuntimeJobsTerminal,
                                auto_continue_after_resume_wait,
                            );
                        }
                        RuntimeJobWaitWake::AnswerNow => {
                            request = resume_execute_agent_run_request(
                                session_id.as_str(),
                                &agent_run_identity,
                                wait_turn_id.as_str(),
                                new_turn_id(),
                                AgentRunResumeIntent::AnswerNow,
                                auto_continue_after_resume_wait,
                            );
                        }
                    }
                }
                AgentRunStop::QuestionWait => {
                    let wait_turn_id = wait_checkpoint_turn_id(&response, "question")?;
                    let (initial_turn_id, resume_intent) = match wait_for_question_input_or_control(
                        &event_writer,
                        agent_run_id.as_str(),
                        &turn_control,
                    )
                    .await?
                    {
                        QuestionWaitWake::Resume { turn_id, message } => {
                            (turn_id, AgentRunResumeIntent::QuestionAnswered(message))
                        }
                        QuestionWaitWake::AnswerNow => {
                            (new_turn_id(), AgentRunResumeIntent::AnswerNow)
                        }
                        QuestionWaitWake::Cancelled => {
                            return Ok(AgentRunStop::Cancelled("agent_run_cancelled".to_string()));
                        }
                    };
                    request = resume_execute_agent_run_request(
                        session_id.as_str(),
                        &agent_run_identity,
                        wait_turn_id.as_str(),
                        initial_turn_id,
                        resume_intent,
                        auto_continue_after_resume_wait,
                    );
                }
            }
        }
    }
    .await;
    let live_payloads = live_text.flush()?;
    emit_live_text_payloads(
        &event_writer,
        agent_run_id.as_str(),
        session_id.as_str(),
        live_payloads,
    )?;
    match loop_result {
        Ok(AgentRunStop::Cancelled(_)) => {
            persist_interrupted_live_text(
                &mut live_text,
                session_id.as_str(),
                agent_run_id.as_str(),
            )?;
            let reason = event_writer
                .agent_run_cancellation_reason(agent_run_id.as_str())?
                .unwrap_or_else(|| "user_interrupt".to_string());
            let cancelled =
                cancel_agent_run_after_tool_closure(agent_runs::AgentRunCancelRequest {
                    agent_run_id: Some(agent_run_id.clone()),
                    session_id: Some(session_id.clone()),
                    reason: Some(reason),
                })?;
            if !cancelled.cancelled {
                return Err("active AgentRun cancellation did not commit terminal".to_string());
            }
            emit_agent_run_terminal_payload(
                &event_writer,
                cancelled
                    .agent_run
                    .as_ref()
                    .ok_or_else(|| "cancelled AgentRun terminal summary is missing".to_string())?,
            )?;
            Ok(())
        }
        Ok(stop @ (AgentRunStop::Finalized | AgentRunStop::TerminalTool)) => {
            let completed_projection = completed_projection.as_ref().ok_or_else(|| {
                "completed agent turn is missing the Core completion projection".to_string()
            })?;
            persist_agent_run_completed(
                session_id.as_str(),
                completed_projection.final_turn_id.as_str(),
                agent_run_id.as_str(),
                final_answer.as_deref(),
                &stop,
            )?;
            let _ = agent_runs::finish_agent_run(
                agent_run_id.as_str(),
                session_id.as_str(),
                turn_id.as_str(),
            )?
            .map(|agent_run| emit_agent_run_terminal_payload(&event_writer, &agent_run))
            .transpose()?;
            live_text.seal()?;
            Ok(())
        }
        Ok(stop) => Err(format!("unexpected unresolved AgentRun stop: {stop:?}")),
        Err(error) => {
            if !streamed_runtime_error {
                let _ = emit_agent_run_payload_blocking(
                    &event_writer,
                    agent_run_id.as_str(),
                    session_id.as_str(),
                    turn_id.as_str(),
                    runtime_error_stream_payload(
                        session_id.as_str(),
                        turn_id.as_str(),
                        error.as_str(),
                        "session_prompt_loop_failed",
                        session_prompt_loop_failure_retryable(error.as_str()),
                    ),
                )?;
            }
            close_incomplete_tool_calls_for_agent_run(session_id.as_str(), agent_run_id.as_str())?;
            if live_text.content().is_empty() {
                persist_agent_run_failed(
                    session_id.as_str(),
                    turn_id.as_str(),
                    agent_run_id.as_str(),
                    error.as_str(),
                )?;
            } else {
                persist_interrupted_live_text(
                    &mut live_text,
                    session_id.as_str(),
                    agent_run_id.as_str(),
                )?;
            }
            let _ = agent_runs::fail_agent_run(
                agent_run_id.as_str(),
                session_id.as_str(),
                turn_id.as_str(),
                error,
            )?
            .map(|agent_run| emit_agent_run_terminal_payload(&event_writer, &agent_run))
            .transpose()?;
            Ok(())
        }
    }
}

fn agent_runtime_config(
    session_id: &str,
    runtime_config: &runtime_config::AgentRuntimeConfigResponse,
    auto_continue_after_resume_wait: Option<bool>,
) -> Result<AgentRuntimeConfig, String> {
    let mut config = AgentRuntimeConfig::default();
    config.auto_continue_after_resume_wait =
        auto_continue_after_resume_wait.unwrap_or(runtime_config.auto_continue_after_resume_wait);
    config.model_context_tokens = runtime_config
        .model_context_tokens
        .unwrap_or(config.model_context_tokens);
    config.model_max_output_tokens = runtime_config
        .model_max_output_tokens
        .unwrap_or(config.model_max_output_tokens);
    config.tool_parallelism = runtime_config
        .tool_parallelism
        .unwrap_or(config.tool_parallelism);
    let state = message_log::project_agent_context_state(session_id)?;
    if let (Some(actual), Some(estimated)) = (
        state
            .provider_usage
            .as_ref()
            .and_then(|usage| usage.latest.input_tokens),
        state.latest_provider_usage_context_token_estimate,
    ) {
        config.prompt_token_estimate_scale_basis_points =
            prompt_token_estimate_scale_basis_points(actual, estimated);
    }
    Ok(config)
}

fn prompt_token_estimate_scale_basis_points(actual: u64, estimated: u64) -> u32 {
    if estimated == 0 {
        return 10_000;
    }
    actual
        .saturating_mul(10_000)
        .saturating_add(estimated - 1)
        .checked_div(estimated)
        .unwrap_or(10_000)
        .clamp(10_000, 100_000) as u32
}

fn final_answer_from_stream_payload(payload: &serde_json::Value) -> Option<String> {
    let event = payload.get("event")?;
    let event_type = event.get("type")?.as_str()?;
    if event_type != "Final" {
        return None;
    }
    event
        .get("payload")?
        .get("content")?
        .as_str()
        .map(ToString::to_string)
}

fn emit_live_text_payloads(
    event_writer: &EventWriter,
    agent_run_id: &str,
    session_id: &str,
    payloads: Vec<serde_json::Value>,
) -> Result<(), String> {
    for payload in payloads {
        event_writer
            .emit(
                "session/update",
                serde_json::json!({
                    "sessionId": session_id,
                    "agentRunId": agent_run_id,
                    "payload": payload,
                }),
            )
            .map_err(|error| format!("live text stream emit failed: {error}"))?;
    }
    Ok(())
}

fn persist_interrupted_live_text(
    live_text: &mut LiveTextAccumulator,
    session_id: &str,
    agent_run_id: &str,
) -> Result<(), String> {
    if let Some(committed) = message_log::project_agent_run_assistant(session_id, agent_run_id)? {
        if !matches!(committed.status.as_deref(), Some("done" | "error")) {
            return Err(format!(
                "committed assistant message has unsupported status: {:?}",
                committed.status
            ));
        }
        sessions::record_session_activity(
            session_id,
            None,
            Some(committed.content.as_str()),
            current_timestamp_ms(),
        )?;
        return live_text.seal();
    }
    if !live_text.content().is_empty() {
        let interrupted_at_ms = current_timestamp_ms();
        let _ = message_log::append_assistant_message(
            session_id,
            live_text.turn_id(),
            Some(agent_run_id),
            live_text.content(),
            "error",
            interrupted_at_ms,
        )?;
        sessions::record_session_activity(
            session_id,
            None,
            Some(live_text.content()),
            interrupted_at_ms,
        )?;
    }
    live_text.seal()
}

pub(crate) fn recover_unsealed_live_text_journals() -> Result<(), String> {
    let (recovered_journals, diagnostics) = LiveTextJournal::recover_isolated(
        user_data_layout::runtime_live_text_journal_dir_path().as_path(),
    )?;
    for diagnostic in diagnostics {
        eprintln!(
            "live_text_journal_isolated: code={} path={} agentRunId={} error={}",
            diagnostic.code,
            diagnostic.path,
            diagnostic.agent_run_id.as_deref().unwrap_or("unknown"),
            diagnostic.message
        );
    }
    for recovered in recovered_journals {
        let agent_run_id = recovered.key.agent_run_id.clone();
        let recovery = (|| {
            let agent_run = message_log::project_agent_run(recovered.key.agent_run_id.as_str())?
                .ok_or_else(|| {
                    format!(
                        "live text journal agent run is missing: {}",
                        recovered.key.agent_run_id
                    )
                })?;
            if agent_run.session_id != recovered.key.session_id {
                return Err(format!(
                    "live text journal agent run identity mismatch: agentRunId={}",
                    recovered.key.agent_run_id
                ));
            }
            let assistant = message_log::project_agent_run_assistant(
                agent_run.session_id.as_str(),
                agent_run.agent_run_id.as_str(),
            )?;
            let agent_run_is_active = match agent_run.status.as_str() {
                "running" | "stalled" => true,
                "cancelled" | "succeeded" | "failed" => false,
                status => {
                    return Err(format!(
                        "live text journal agent run has unsupported status {status}: {}",
                        agent_run.agent_run_id
                    ));
                }
            };

            match assistant
                .as_ref()
                .and_then(|message| message.status.as_deref())
            {
                Some("done") => {
                    if agent_run_is_active {
                        let _ = agent_runs::finish_agent_run(
                            agent_run.agent_run_id.as_str(),
                            agent_run.session_id.as_str(),
                            agent_run.turn_id.as_str(),
                        )?;
                    }
                }
                None if assistant.is_none() => {
                    if !agent_run_is_active {
                        return Err(format!(
                        "live text journal assistant message is missing for terminal AgentRun: {}",
                        agent_run.turn_id
                    ));
                    }
                    if !recovered.content.is_empty() {
                        let interrupted_at_ms = current_timestamp_ms();
                        let _ = message_log::append_assistant_message(
                            agent_run.session_id.as_str(),
                            recovered.key.turn_id.as_str(),
                            Some(agent_run.agent_run_id.as_str()),
                            recovered.content.as_str(),
                            "error",
                            interrupted_at_ms,
                        )?;
                        sessions::record_session_activity(
                            agent_run.session_id.as_str(),
                            None,
                            Some(recovered.content.as_str()),
                            interrupted_at_ms,
                        )?;
                    }
                    let cancelled =
                        cancel_agent_run_after_tool_closure(agent_runs::AgentRunCancelRequest {
                            agent_run_id: Some(agent_run.agent_run_id.clone()),
                            session_id: Some(agent_run.session_id.clone()),
                            reason: Some("runtime_server_recovered_interrupted".to_string()),
                        })?;
                    if !cancelled.cancelled {
                        return Err(format!(
                            "live text recovery did not cancel active agent run: {}",
                            agent_run.agent_run_id
                        ));
                    }
                }
                Some("error") => {
                    if agent_run_is_active {
                        let cancelled = cancel_agent_run_after_tool_closure(
                            agent_runs::AgentRunCancelRequest {
                                agent_run_id: Some(agent_run.agent_run_id.clone()),
                                session_id: Some(agent_run.session_id.clone()),
                                reason: Some("runtime_server_recovered_interrupted".to_string()),
                            },
                        )?;
                        if !cancelled.cancelled {
                            return Err(format!(
                                "live text recovery did not cancel active agent run: {}",
                                agent_run.agent_run_id
                            ));
                        }
                    }
                }
                Some(status) => {
                    return Err(format!(
                        "live text journal assistant has unsupported status {status}: {}",
                        agent_run.turn_id
                    ));
                }
                None => {
                    return Err(format!(
                        "live text journal assistant has no status: {}",
                        agent_run.turn_id
                    ));
                }
            }
            recovered.seal()?;
            Ok::<(), String>(())
        })();
        if let Err(error) = recovery {
            eprintln!(
                "live_text_journal_isolated: code=live_text_recovery_failed agentRunId={agent_run_id} error={}",
                error.chars().take(1024).collect::<String>()
            );
        }
    }
    for agent_run in message_log::project_agent_runs()?
        .into_iter()
        .filter(|agent_run| matches!(agent_run.status.as_str(), "running" | "stalled"))
    {
        let assistant = message_log::project_agent_run_assistant(
            agent_run.session_id.as_str(),
            agent_run.agent_run_id.as_str(),
        )?;
        match assistant
            .as_ref()
            .and_then(|message| message.status.as_deref())
        {
            Some("done") => {
                let _ = agent_runs::finish_agent_run(
                    agent_run.agent_run_id.as_str(),
                    agent_run.session_id.as_str(),
                    agent_run.turn_id.as_str(),
                )?;
            }
            Some("error") => {
                let cancelled =
                    cancel_agent_run_after_tool_closure(agent_runs::AgentRunCancelRequest {
                        agent_run_id: Some(agent_run.agent_run_id.clone()),
                        session_id: Some(agent_run.session_id.clone()),
                        reason: Some("runtime_server_recovered_interrupted".to_string()),
                    })?;
                if !cancelled.cancelled {
                    return Err(format!(
                        "runtime server recovery did not cancel active agent run: {}",
                        agent_run.agent_run_id
                    ));
                }
            }
            None if assistant.is_none() => {
                let cancelled =
                    cancel_agent_run_after_tool_closure(agent_runs::AgentRunCancelRequest {
                        agent_run_id: Some(agent_run.agent_run_id.clone()),
                        session_id: Some(agent_run.session_id.clone()),
                        reason: Some("runtime_server_recovered_interrupted".to_string()),
                    })?;
                if !cancelled.cancelled {
                    return Err(format!(
                        "runtime server recovery did not cancel active agent run: {}",
                        agent_run.agent_run_id
                    ));
                }
            }
            Some(status) => {
                return Err(format!(
                    "active agent run assistant has unsupported status {status}: {}",
                    agent_run.turn_id
                ));
            }
            None => {
                return Err(format!(
                    "active agent run assistant has no status: {}",
                    agent_run.turn_id
                ));
            }
        }
    }
    Ok(())
}

fn session_prompt_loop_failure_retryable(message: &str) -> bool {
    let Some(metadata) = message.strip_prefix("model_client_error(kind=") else {
        return false;
    };
    let Some((_, retryable_and_message)) = metadata.split_once(",retryable=") else {
        return false;
    };
    retryable_and_message
        .split_once("): ")
        .map(|(retryable, _)| retryable == "true")
        .unwrap_or(false)
}

async fn latest_paused_question_turn_id(
    store: &RuntimeStoreActor,
    session_id: &str,
) -> Result<String, String> {
    store
        .list_checkpoints(session_id, 1, 0)
        .await
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
        .filter(|checkpoint| {
            checkpoint.status == "paused_question"
                || checkpoint.done_reason.as_deref() == Some("question")
        })
        .map(|checkpoint| checkpoint.turn_id)
        .ok_or_else(|| format!("paused question checkpoint not found for sessionId={session_id}"))
}

fn build_question_answer_message(
    question_id: &str,
    answers: &[String],
    answer_text: Option<&str>,
) -> Result<String, String> {
    let normalized_answers = answers
        .iter()
        .map(|answer| answer.trim())
        .filter(|answer| !answer.is_empty())
        .collect::<Vec<_>>();
    let normalized_text = answer_text.map(str::trim).filter(|value| !value.is_empty());
    if normalized_answers.is_empty() && normalized_text.is_none() {
        return Err(String::from("question answer cannot be empty"));
    }
    let mut message = format!("Answer for pending question {question_id}:");
    if !normalized_answers.is_empty() {
        message.push_str("\nSelected options:\n");
        for answer in normalized_answers {
            message.push_str("- ");
            message.push_str(answer);
            message.push('\n');
        }
    }
    if let Some(text) = normalized_text {
        message.push_str("\nFree-form answer:\n");
        message.push_str(text);
    }
    Ok(message)
}

fn runtime_error_stream_payload(
    session_id: &str,
    turn_id: &str,
    message: &str,
    reason: &str,
    retryable: bool,
) -> serde_json::Value {
    project_turn_update(TurnUpdate::RuntimeError {
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        message: message.to_string(),
        reason: reason.to_string(),
        retryable,
        process_state:
            centaeris_core::runtime::contracts::RuntimeProcessState::from_provider_error_reason(
                reason,
            ),
    })
    .expect("Core runtime error projection must be valid")
    .expect("runtime error must produce a live projection")
}

fn run_message_log_blocking<T>(
    label: &str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    tokio::runtime::Handle::try_current()
        .map_err(|error| format!("{label} requires Tokio runtime: {error}"))?;
    std::panic::catch_unwind(AssertUnwindSafe(|| tokio::task::block_in_place(operation)))
        .map_err(|_| format!("{label} panicked inside Tokio blocking boundary"))?
}

fn materialize_spawned_agent_session_blocking(
    parent_session_id: &str,
    update: &TurnUpdate,
) -> Result<(), String> {
    let TurnUpdate::RuntimeEvent { event } = update else {
        return Ok(());
    };
    let Some(readiness) = centaeris_core::runtime::event::subagent_session_readiness(event)? else {
        return Ok(());
    };
    if readiness.parent_session_id != parent_session_id {
        return Err(format!(
            "SubagentSpawned sessionId mismatch: expected={parent_session_id} actual={}",
            readiness.parent_session_id
        ));
    }
    let cwd = sessions::cwd_for_session_id(parent_session_id)?;
    run_message_log_blocking("materialize spawned Agent session", || {
        sessions::ensure_agent_session(
            readiness.child_session_id.as_str(),
            parent_session_id,
            readiness.runtime_job_id.as_str(),
            readiness.title.as_str(),
            cwd.as_str(),
            readiness.at_ms,
        )?;
        let projection = message_log::project_session_log(readiness.child_session_id.as_str())?;
        if let Some(agent_run) = projection.agent_runs.first() {
            if agent_run.agent_run_id == readiness.runtime_job_id {
                return Ok(());
            }
            return Err(format!(
                "AgentRun session mismatch: sessionId={} expected={} actual={}",
                readiness.child_session_id, readiness.runtime_job_id, agent_run.agent_run_id
            ));
        }
        message_log::append_agent_turn_queued(
            readiness.child_session_id.as_str(),
            readiness.child_turn_id.as_str(),
            readiness.runtime_job_id.as_str(),
            readiness.title.as_str(),
            readiness.at_ms,
        )
    })
}

fn append_file_mutation_pre_apply_fact_blocking(
    agent_run_id: String,
    session_id: String,
    turn_id: String,
    file_fact: serde_json::Value,
) -> Result<(), String> {
    run_message_log_blocking("append file mutation pre-apply fact", move || {
        message_log::append_file_mutation_pre_apply_fact(
            session_id.as_str(),
            turn_id.as_str(),
            agent_run_id.as_str(),
            file_fact,
        )
    })
}

fn emit_agent_run_payload_blocking(
    event_writer: &EventWriter,
    agent_run_id: &str,
    session_id: &str,
    _turn_id: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    emit_annotated_agent_run_payloads(event_writer, agent_run_id, session_id, vec![payload])
}

pub(crate) fn emit_background_turn_update_blocking(
    event_writer: &EventWriter,
    runtime_job_id: &str,
    child_session_id: &str,
    event: TurnUpdate,
) -> Result<(), String> {
    let Some(payload) = project_turn_update(event)? else {
        return Ok(());
    };
    let turn_id = payload
        .get("event")
        .and_then(|event| event.get("turnId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(runtime_job_id)
        .to_string();
    emit_agent_run_payload_blocking(
        event_writer,
        runtime_job_id,
        child_session_id,
        turn_id.as_str(),
        payload,
    )?;
    Ok(())
}

fn emit_annotated_agent_run_payloads(
    event_writer: &EventWriter,
    agent_run_id: &str,
    session_id: &str,
    annotated_payloads: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let projected_payload = annotated_payloads
        .iter()
        .rev()
        .find(|payload| {
            matches!(
                payload.get("type").and_then(serde_json::Value::as_str),
                Some("runtime_event" | "session_event")
            )
        })
        .cloned()
        .ok_or_else(|| {
            format!("stream projection returned no event for agentRunId={agent_run_id}")
        })?;
    for annotated in annotated_payloads {
        event_writer
            .emit(
                "session/update",
                serde_json::json!({
                    "sessionId": session_id,
                    "agentRunId": agent_run_id,
                    "payload": annotated,
                }),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(projected_payload)
}

pub(crate) fn emit_agent_run_terminal_payload(
    event_writer: &EventWriter,
    agent_run: &agent_runs::AgentRunSummary,
) -> Result<(), String> {
    let payload =
        message_log::terminal_agent_run_stream_projection(agent_run.agent_run_id.as_str())?;
    event_writer
        .emit(
            "session/update",
            serde_json::json!({
                "sessionId": agent_run.session_id.as_str(),
                "agentRunId": agent_run.agent_run_id.as_str(),
                "payload": payload,
            }),
        )
        .map_err(|error| format!("agent run terminal stream emit failed: {error}"))
}

pub(crate) fn model_session_config_and_registry(
    runtime_config: &runtime_config::AgentRuntimeConfigResponse,
) -> Result<(ModelSessionConfig, ModelProviderRegistry), String> {
    let provider_id = runtime_config
        .model_provider_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "global model is not configured; configure a model in Settings".to_string())?
        .to_string();
    let model = runtime_config
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "global model is not configured; configure a model in Settings".to_string())?
        .to_string();
    model_session_config_and_registry_for(runtime_config, provider_id.as_str(), model.as_str())
}

fn ensure_selected_model_supports_vision(
    runtime_config: &runtime_config::AgentRuntimeConfigResponse,
) -> Result<(), String> {
    let provider_id = runtime_config.model_provider_id.as_deref().ok_or_else(|| {
        "global model is not configured; configure a model in Settings".to_string()
    })?;
    let model = runtime_config.model.as_deref().ok_or_else(|| {
        "global model is not configured; configure a model in Settings".to_string()
    })?;
    let item = runtime_config
        .model_providers
        .iter()
        .flat_map(|provider| provider.models.iter())
        .find(|item| item.provider_id == provider_id && item.model == model)
        .ok_or_else(|| "selected model is unavailable".to_string())?;
    if !item.supports_vision {
        return Err(format!(
            "selected model does not support image input: providerId={provider_id} model={model}"
        ));
    }
    Ok(())
}

pub(crate) fn model_session_config_and_registry_for(
    runtime_config: &runtime_config::AgentRuntimeConfigResponse,
    provider_id: &str,
    model: &str,
) -> Result<(ModelSessionConfig, ModelProviderRegistry), String> {
    let provider_id = provider_id.trim().to_string();
    let model = model.trim().to_string();
    if provider_id.is_empty() || model.is_empty() {
        return Err("providerId and model are required".to_string());
    }
    let model_api_key = runtime_config::model_api_key_for_provider(provider_id.as_str())?;
    let mut registry = ModelProviderRegistry::new();
    let selected_catalog_item = runtime_config
        .model_providers
        .iter()
        .flat_map(|provider| provider.models.iter())
        .find(|item| item.provider_id == provider_id && item.model == model)
        .ok_or_else(|| {
            format!(
                "selected model provider is not configured or model is unavailable: providerId={provider_id} model={model}"
            )
        })?;
    if let Some(diagnostic) = selected_catalog_item.diagnostic.as_deref() {
        return Err(diagnostic.to_string());
    }
    let mut provider = if selected_catalog_item.built_in {
        registry
            .get(provider_id.as_str())
            .cloned()
            .ok_or_else(|| format!("unknown model provider: {provider_id}"))?
    } else {
        let api = selected_catalog_item.model_api.ok_or_else(|| {
            format!("custom model API is missing: providerId={provider_id} model={model}")
        })?;
        custom_model_provider_info(
            provider_id.as_str(),
            api,
            model_api_key.clone(),
            selected_catalog_item.supports_vision,
        )
    };
    if selected_catalog_item.built_in {
        if let Some(api) = selected_catalog_item.model_api {
            provider.wire_api = model_wire_api_to_wire_api(api);
        }
    }
    if provider.auth != AuthSpec::None && model_api_key.is_none() {
        return Err(format!(
            "model provider {} requires API key; configure it in Settings or set the provider-specific API key environment variable",
            provider_id
        ));
    }
    if model_api_key.is_some() && selected_catalog_item.built_in {
        let mut provider_override = provider.clone();
        provider_override.auth =
            auth_spec_with_model_api_key(&provider.auth, model_api_key.clone().expect("checked"));
        registry.insert_user_defined(provider_override);
    } else if !selected_catalog_item.built_in {
        registry.insert_user_defined(provider.clone());
    }
    let max_output_tokens = selected_catalog_item
        .model_max_output_tokens
        .ok_or_else(|| {
            format!(
                "custom model maxOutputTokens is missing: providerId={provider_id} model={model}"
            )
        })?;
    let config = ModelSessionConfig {
        provider_kind: provider.provider_kind.clone(),
        provider_id: provider_id.clone(),
        model: model.clone(),
        api_base: selected_catalog_item.model_api_base.clone(),
        timeout_ms: DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS,
        max_retries: DEFAULT_MODEL_MAX_RETRIES,
        retry_backoff_ms: DEFAULT_MODEL_RETRY_BACKOFF_MS,
        max_output_tokens: Some(max_output_tokens),
        thinking_mode: if runtime_config.model_provider_id.as_deref() == Some(provider_id.as_str())
            && runtime_config.model.as_deref() == Some(model.as_str())
        {
            runtime_config.model_thinking_mode.clone()
        } else {
            selected_catalog_item.model_thinking_mode.clone()
        },
        metadata: HashMap::new(),
    };
    Ok((config, registry))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ModelTestRequest {
    provider_id: String,
    model: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelTestResponse {
    pub(crate) http_status: Option<u16>,
    pub(crate) latency_ms: u64,
    pub(crate) output_preview: Option<String>,
    pub(crate) error_keyword: Option<String>,
}

pub(crate) async fn test_model(request: ModelTestRequest) -> Result<ModelTestResponse, String> {
    let started_at = Instant::now();
    let runtime_config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {})?;
    let available_model = runtime_config
        .model_providers
        .iter()
        .flat_map(|provider| provider.models.iter())
        .find(|item| {
            item.provider_id == request.provider_id.trim() && item.model == request.model.trim()
        });
    if available_model.is_none() {
        return Ok(ModelTestResponse {
            http_status: None,
            latency_ms: elapsed_ms(started_at),
            output_preview: None,
            error_keyword: Some("model_not_available".to_string()),
        });
    }
    let (mut config, registry) = match model_session_config_and_registry_for(
        &runtime_config,
        request.provider_id.as_str(),
        request.model.as_str(),
    ) {
        Ok(value) => value,
        Err(error) => {
            return Ok(ModelTestResponse {
                http_status: None,
                latency_ms: elapsed_ms(started_at),
                output_preview: None,
                error_keyword: Some(model_test_keyword(error.as_str())),
            });
        }
    };
    config.timeout_ms = 15_000;
    config.max_retries = 0;
    config.max_output_tokens = Some(32);
    let prompt = PreparedPromptV1::new(
        None,
        vec![ModelMessageV1 {
            message_id: "model-test-user".to_string(),
            role: ModelMessageRoleV1::User,
            content: "Reply with a short connectivity confirmation.".to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning_content: None,
        }],
        Vec::new(),
        ModelToolChoice::None,
        32,
    )?;
    let request = centaeris_core::model::ModelClientRequest {
        session_id: "model-test".to_string(),
        turn_id: "model-test".to_string(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate: 32,
        prepared_prompt: prompt,
        session_config: config.clone(),
    };
    let transport = ModelTestTransport::new(ReqwestJsonHttpTransport::new()?);
    let http_status = transport.http_status.clone();
    let result = match model_config_wire_api(&registry, &config)? {
        WireApi::AnthropicMessages => {
            AnthropicMessagesModelClient::new(registry, transport)
                .generate(&request)
                .await
        }
        WireApi::OpenAiResponses => {
            OpenAiResponsesModelClient::new(registry, transport)
                .generate(&request)
                .await
        }
        WireApi::OpenAiChatCompletions => {
            OpenAiCompatibleModelClient::new(registry, transport)
                .generate(&request)
                .await
        }
        unsupported => {
            return Err(format!(
                "model test does not support wire API {unsupported:?}"
            ))
        }
    };
    let http_status = http_status.lock().ok().and_then(|status| *status);
    match result {
        Ok(response) => Ok(ModelTestResponse {
            http_status,
            latency_ms: elapsed_ms(started_at),
            output_preview: Some(model_test_preview(
                response.generate_result.content.as_str(),
            )),
            error_keyword: None,
        }),
        Err(error) => Ok(ModelTestResponse {
            http_status,
            latency_ms: elapsed_ms(started_at),
            output_preview: None,
            error_keyword: Some(model_test_error_keyword(&error)),
        }),
    }
}

struct ModelTestTransport {
    inner: ReqwestJsonHttpTransport,
    http_status: Arc<Mutex<Option<u16>>>,
}

impl ModelTestTransport {
    fn new(inner: ReqwestJsonHttpTransport) -> Self {
        Self {
            inner,
            http_status: Arc::new(Mutex::new(None)),
        }
    }

    fn record(&self, response: &Result<JsonHttpResponse, String>) {
        if let Ok(response) = response {
            if let Ok(mut status) = self.http_status.lock() {
                *status = Some(response.status_code);
            }
        }
    }
}

impl JsonHttpTransport for ModelTestTransport {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            let response = self.inner.execute_json(request).await;
            self.record(&response);
            response
        })
    }

    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            let response = self.inner.execute_sse(request, on_data).await;
            self.record(&response);
            response
        })
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn model_test_preview(value: &str) -> String {
    model_test_keyword(value)
}

fn model_test_error_keyword(error: &ModelClientError) -> String {
    error
        .provider_code
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(model_test_keyword)
        .unwrap_or_else(|| model_test_keyword(error.message.as_str()))
}

fn model_test_keyword(value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= 96 {
        return value;
    }
    format!("{}...", value.chars().take(93).collect::<String>())
}

fn custom_model_provider_info(
    provider_id: &str,
    api: runtime_config::CustomModelProviderApi,
    api_key: Option<String>,
    supports_vision: bool,
) -> ModelProviderInfo {
    let (provider_kind, wire_api, auth, http_headers) = match api {
        runtime_config::CustomModelProviderApi::OpenAiCompletions => (
            ModelProviderKind::Custom,
            WireApi::OpenAiChatCompletions,
            api_key
                .as_deref()
                .map(|key| AuthSpec::StaticHeader {
                    header_name: "authorization".to_string(),
                    value: format!("Bearer {}", key.trim()),
                })
                .unwrap_or(AuthSpec::None),
            HashMap::new(),
        ),
        runtime_config::CustomModelProviderApi::OpenAiResponses => (
            ModelProviderKind::Custom,
            WireApi::OpenAiResponses,
            api_key
                .as_deref()
                .map(|key| AuthSpec::StaticHeader {
                    header_name: "authorization".to_string(),
                    value: format!("Bearer {}", key.trim()),
                })
                .unwrap_or(AuthSpec::None),
            HashMap::new(),
        ),
        runtime_config::CustomModelProviderApi::AnthropicMessages => (
            ModelProviderKind::Custom,
            WireApi::AnthropicMessages,
            api_key
                .as_deref()
                .map(|key| AuthSpec::StaticHeader {
                    header_name: "x-api-key".to_string(),
                    value: key.trim().to_string(),
                })
                .unwrap_or(AuthSpec::None),
            HashMap::from([("anthropic-version".to_string(), "2023-06-01".to_string())]),
        ),
    };
    ModelProviderInfo {
        provider_key: provider_id.to_string(),
        name: provider_id.to_string(),
        provider_kind,
        base_url: None,
        wire_api,
        auth,
        http_headers,
        env_http_headers: HashMap::new(),
        default_timeout_ms: Some(DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS),
        default_max_retries: Some(DEFAULT_MODEL_MAX_RETRIES),
        default_retry_backoff_ms: Some(DEFAULT_MODEL_RETRY_BACKOFF_MS),
        capability_profile: CapabilityProfile {
            supports_streaming: true,
            supports_tool_calls: true,
            supports_vision,
            ..CapabilityProfile::default()
        },
        metadata: HashMap::new(),
    }
}

fn model_wire_api_to_wire_api(api: runtime_config::CustomModelProviderApi) -> WireApi {
    match api {
        runtime_config::CustomModelProviderApi::OpenAiCompletions => WireApi::OpenAiChatCompletions,
        runtime_config::CustomModelProviderApi::OpenAiResponses => WireApi::OpenAiResponses,
        runtime_config::CustomModelProviderApi::AnthropicMessages => WireApi::AnthropicMessages,
    }
}

fn auth_spec_with_model_api_key(source: &AuthSpec, model_api_key: String) -> AuthSpec {
    match source {
        AuthSpec::ApiKeyEnv {
            header_name,
            prefix,
            ..
        } => AuthSpec::StaticHeader {
            header_name: header_name.clone(),
            value: apply_auth_prefix(model_api_key.as_str(), prefix.as_deref()),
        },
        AuthSpec::BearerEnv { .. } | AuthSpec::None => AuthSpec::StaticHeader {
            header_name: "authorization".to_string(),
            value: format!("Bearer {}", model_api_key.trim()),
        },
        AuthSpec::StaticHeader { header_name, .. } => AuthSpec::StaticHeader {
            header_name: header_name.clone(),
            value: model_api_key.trim().to_string(),
        },
        AuthSpec::CommandToken {
            header_name,
            prefix,
            ..
        } => AuthSpec::StaticHeader {
            header_name: header_name.clone(),
            value: apply_auth_prefix(model_api_key.as_str(), prefix.as_deref()),
        },
    }
}

fn apply_auth_prefix(value: &str, prefix: Option<&str>) -> String {
    match prefix {
        Some(prefix_value) if !prefix_value.trim().is_empty() => {
            format!("{} {}", prefix_value.trim(), value.trim())
        }
        _ => value.trim().to_string(),
    }
}

pub(crate) fn model_config_wire_api(
    registry: &ModelProviderRegistry,
    config: &ModelSessionConfig,
) -> Result<WireApi, String> {
    Ok(registry
        .get(config.provider_id.as_str())
        .ok_or_else(|| format!("unknown model provider: {}", config.provider_id))?
        .wire_api
        .clone())
}

pub(crate) struct SingleModelSessionConfigStore {
    pub(crate) session_id: String,
    pub(crate) config: ModelSessionConfig,
}

impl centaeris_core::model::ModelSessionConfigStore for SingleModelSessionConfigStore {
    fn get_session_config(&self, session_id: &str) -> Result<Option<ModelSessionConfig>, String> {
        if session_id == self.session_id {
            return Ok(Some(self.config.clone()));
        }
        Ok(Some(self.config.clone()))
    }
}

fn normalize_session_id(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| "chat-default".to_string())
}

fn agent_runtime_store_db_path() -> PathBuf {
    if let Some(path_raw) = std::env::var_os("CENTAERIS_AGENT_RUNTIME_DB_PATH") {
        return PathBuf::from(path_raw);
    }
    user_data_layout::runtime_store_db_path()
}

pub(crate) fn agent_runtime_store_actor() -> Result<RuntimeStoreActor, String> {
    let db_path = agent_runtime_store_db_path();
    let actor_state = AGENT_RUNTIME_STORE_ACTOR.get_or_init(|| Mutex::new(None));
    let mut guard = actor_state
        .lock()
        .map_err(|_| "agent runtime store actor lock poisoned".to_string())?;
    if let Some(state) = guard.as_ref() {
        if state.db_path == db_path {
            return Ok(state.actor.clone());
        }
    }

    let store = SqliteRuntimeStore::new(db_path.as_path())?;
    let actor = RuntimeStoreActor::start(store).map_err(|error| error.to_string())?;
    *guard = Some(AgentRuntimeStoreActorState {
        db_path,
        actor: actor.clone(),
    });
    Ok(actor)
}

fn required_string(raw: &str, field_name: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(format!("{field_name} is required"));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::sandbox::SandboxType;
    use centaeris_core::execution::ExecutionHostRunner;
    use centaeris_core::model::ToolCallEnvelope;
    use std::collections::HashMap;
    use std::fs;
    use std::time::Duration;

    fn prompt_request(operation_id: &str, session_id: &str, message: &str) -> AgentInputRequest {
        serde_json::from_value(serde_json::json!({
            "operationId": operation_id,
            "sessionId": session_id,
            "message": message,
        }))
        .expect("session/prompt request")
    }

    fn prompt_event_writer(
        viewer_id: &str,
    ) -> (
        Arc<crate::runtime_rpc_transport::RuntimeServerClientHub>,
        EventWriter,
        tokio::sync::mpsc::UnboundedReceiver<String>,
    ) {
        let clients = Arc::new(crate::runtime_rpc_transport::RuntimeServerClientHub::default());
        let (event_writer, outbound) = clients
            .connect()
            .expect("connect prompt client")
            .expect("runtime server accepts prompt client");
        event_writer
            .register_client(crate::runtime_server::RuntimeClientKind::Desktop, viewer_id)
            .expect("register prompt client");
        (clients, event_writer, outbound)
    }

    async fn stop_prompt_agent_run(event_writer: &EventWriter, response: &AgentInputResponse) {
        let _ = cancel_agent_run(
            event_writer.clone(),
            agent_runs::AgentRunCancelRequest {
                agent_run_id: Some(response.agent_run_id.clone()),
                session_id: Some(response.session_id.clone()),
                reason: Some("test_cleanup".to_string()),
            },
        );
        for _ in 0..200 {
            if event_writer
                .active_agent_run(response.agent_run_id.as_str())
                .expect("inspect active prompt AgentRun")
                .is_none()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("prompt AgentRun did not stop during test cleanup");
    }

    struct PromptTestEnvironment {
        root: PathBuf,
        workspace: PathBuf,
        previous_data_dir: Option<std::ffi::OsString>,
        previous_log_dir: Option<std::ffi::OsString>,
        previous_db_path: Option<std::ffi::OsString>,
    }

    impl PromptTestEnvironment {
        fn new(name: &str) -> Self {
            let root = unique_test_dir(name);
            let workspace = root.join("workspace");
            fs::create_dir_all(workspace.as_path()).expect("create workspace");
            let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
            let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
            let previous_db_path = std::env::var_os("CENTAERIS_AGENT_RUNTIME_DB_PATH");
            std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
            std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
            std::env::set_var(
                "CENTAERIS_AGENT_RUNTIME_DB_PATH",
                root.join("runtime-state.sqlite3"),
            );
            Self {
                root,
                workspace,
                previous_data_dir,
                previous_log_dir,
                previous_db_path,
            }
        }

        fn create_session(&self, title: &str) -> Result<sessions::SessionItemResponse, String> {
            sessions::create(sessions::SessionCreateRequest {
                title: Some(title.to_string()),
                cwd: self.workspace.to_string_lossy().to_string(),
            })
        }
    }

    impl Drop for PromptTestEnvironment {
        fn drop(&mut self) {
            match self.previous_data_dir.take() {
                Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
                None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
            }
            match self.previous_log_dir.take() {
                Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
                None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
            }
            match self.previous_db_path.take() {
                Some(value) => std::env::set_var("CENTAERIS_AGENT_RUNTIME_DB_PATH", value),
                None => std::env::remove_var("CENTAERIS_AGENT_RUNTIME_DB_PATH"),
            }
            fs::remove_dir_all(self.root.as_path()).ok();
        }
    }

    #[test]
    fn session_prompt_loop_preserves_model_retryability_metadata() {
        assert!(session_prompt_loop_failure_retryable(
            "model_client_error(kind=provider_response_interrupted,retryable=true): read SSE chunk failed"
        ));
        assert!(!session_prompt_loop_failure_retryable(
            "model_client_error(kind=invalid_request,retryable=false): bad request"
        ));
        assert!(!session_prompt_loop_failure_retryable(
            "session store failed before model request"
        ));
    }

    #[test]
    fn actual_provider_usage_calibrates_future_prompt_estimates_conservatively() {
        assert_eq!(
            prompt_token_estimate_scale_basis_points(228_800, 163_000),
            14_037
        );
        assert_eq!(prompt_token_estimate_scale_basis_points(100, 200), 10_000);
        assert_eq!(
            prompt_token_estimate_scale_basis_points(2_000_000, 1),
            100_000
        );
    }

    #[test]
    fn live_text_recovery_request_starts_from_the_preserved_prefix() {
        let mut live_text = LiveTextAccumulator::new(
            "session_1".to_string(),
            "turn_1".to_string(),
            "run_1".to_string(),
        );

        let payloads = live_text
            .begin_model_request("turn_1:2", "partial")
            .expect("recovery request");

        assert!(payloads.is_empty());
        assert_eq!(live_text.content(), "partial");
        assert!(matches!(
            live_text.pending_operations.as_slice(),
            [LiveTextOperation::Replace { text }] if text == "partial"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancellation_closes_open_tool_call_before_agent_run_terminal() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = unique_test_dir("cancel-open-tool-call");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        let previous_db_path = std::env::var_os("CENTAERIS_AGENT_RUNTIME_DB_PATH");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        std::env::set_var(
            "CENTAERIS_AGENT_RUNTIME_DB_PATH",
            root.join("runtime-state.sqlite3"),
        );
        let result = (|| {
            let session = sessions::create(sessions::SessionCreateRequest {
                title: Some("cancel open tool call".to_string()),
                cwd: workspace.to_string_lossy().to_string(),
            })?;
            let agent_run_id = "agent-run-cancel-open-tool";
            let turn_id = "turn-cancel-open-tool";
            message_log::append_agent_run_started(
                session.id.as_str(),
                turn_id,
                agent_run_id,
                "run a tool",
                1,
            )?;
            let call = ToolCallEnvelope {
                id: "call-cancel-open-tool".to_string(),
                name: "bash".to_string(),
                args_json: serde_json::json!({"command": "echo banana"}).to_string(),
            };
            message_log::append_tool_call(
                session.id.as_str(),
                turn_id,
                agent_run_id,
                &call,
                "provider.test",
                format!("sha256:{}", "a".repeat(64)).as_str(),
                2,
            )?;
            let terminal_error = message_log::append_agent_run_terminal(
                session.id.as_str(),
                turn_id,
                agent_run_id,
                "failed",
                Some("banana"),
                3,
            )
            .expect_err("open ToolCall must block AgentRun terminal");
            if !terminal_error.contains("requires every ToolCall to be closed") {
                return Err(format!("unexpected terminal error: {terminal_error}"));
            }

            let cancelled =
                cancel_agent_run_after_tool_closure(agent_runs::AgentRunCancelRequest {
                    agent_run_id: Some(agent_run_id.to_string()),
                    session_id: Some(session.id.clone()),
                    reason: Some("host_owner_exited".to_string()),
                })?;
            if !cancelled.cancelled {
                return Err("expected AgentRun cancellation".to_string());
            }
            if !message_log::project_incomplete_tool_calls(session.id.as_str(), agent_run_id)?
                .is_empty()
            {
                return Err("cancelled AgentRun retained an open ToolCall".to_string());
            }
            let session_path =
                crate::user_data_layout::find_session_log_file_path(session.id.as_str())?
                    .ok_or_else(|| "cancelled Session log is missing".to_string())?;
            let document = message_log::read_session_document(session_path.as_path())?;
            let tool_result_index = document
                .records
                .iter()
                .position(|record| {
                    record.event_type == centaeris_core::session::SessionRecordType::ToolResult
                        && record.payload.get("callId").and_then(Value::as_str)
                            == Some(call.id.as_str())
                        && record.payload.get("toolName").and_then(Value::as_str)
                            == Some(call.name.as_str())
                })
                .ok_or_else(|| "matching error ToolResult is missing".to_string())?;
            let terminal_index = document
                .records
                .iter()
                .position(|record| {
                    record.event_type
                        == centaeris_core::session::SessionRecordType::AgentRunInterrupted
                })
                .ok_or_else(|| "AgentRunInterrupted is missing".to_string())?;
            if tool_result_index >= terminal_index {
                return Err("AgentRun terminated before its ToolCall was closed".to_string());
            }
            restore_runtime_snapshot_from_session(session.id.as_str())?;
            let restored = SessionManager::new(agent_runtime_store_actor()?)
                .load_or_create_session(session.id.as_str())?;
            if !restored.messages.iter().any(|message| {
                restored
                    .model_semantics
                    .get(message.message_id.as_str())
                    .is_some_and(|semantics| {
                        matches!(
                            semantics,
                            centaeris_core::session::state::ModelMessageSemanticsV1::ToolResult {
                                tool_call_id,
                                ..
                            } if tool_call_id == call.id.as_str()
                        )
                    })
            }) {
                return Err(
                    "SQLite runtime snapshot was not rebuilt from Session facts".to_string()
                );
            }
            Ok::<(), String>(())
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        match previous_db_path {
            Some(value) => std::env::set_var("CENTAERIS_AGENT_RUNTIME_DB_PATH", value),
            None => std::env::remove_var("CENTAERIS_AGENT_RUNTIME_DB_PATH"),
        }
        fs::remove_dir_all(root).ok();
        drop(guard);
        result.expect("ToolResult before terminal");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_turn_preflight_closes_incomplete_tool_call_once_and_continues() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = unique_test_dir("new-turn-tool-recovery");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        let previous_db_path = std::env::var_os("CENTAERIS_AGENT_RUNTIME_DB_PATH");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        std::env::set_var(
            "CENTAERIS_AGENT_RUNTIME_DB_PATH",
            root.join("runtime-state.sqlite3"),
        );
        let result = (|| {
            let session = sessions::create(sessions::SessionCreateRequest {
                title: Some("recover before new turn".to_string()),
                cwd: workspace.to_string_lossy().to_string(),
            })?;
            let old_agent_run_id = "agent-run-recover-old";
            let old_turn_id = "turn-recover-old";
            message_log::append_agent_run_started(
                session.id.as_str(),
                old_turn_id,
                old_agent_run_id,
                "run a tool",
                1,
            )?;
            let call = ToolCallEnvelope {
                id: "call-recover-old".to_string(),
                name: "bash".to_string(),
                args_json: serde_json::json!({"command": "echo banana"}).to_string(),
            };
            message_log::append_tool_call(
                session.id.as_str(),
                old_turn_id,
                old_agent_run_id,
                &call,
                "provider.test",
                format!("sha256:{}", "b".repeat(64)).as_str(),
                2,
            )?;

            recover_incomplete_tool_calls_before_new_turn(session.id.as_str())?;
            recover_incomplete_tool_calls_before_new_turn(session.id.as_str())?;
            let recovered = message_log::project_agent_run(old_agent_run_id)?
                .ok_or_else(|| "recovered AgentRun is missing".to_string())?;
            if recovered.status != "failed" {
                return Err(format!("recovered AgentRun status is {}", recovered.status));
            }
            message_log::append_agent_run_started(
                session.id.as_str(),
                "turn-recover-new",
                "agent-run-recover-new",
                "continue",
                3,
            )?;
            restore_runtime_snapshot_from_session(session.id.as_str())?;
            let session_path =
                crate::user_data_layout::find_session_log_file_path(session.id.as_str())?
                    .ok_or_else(|| "recovered Session log is missing".to_string())?;
            let document = message_log::read_session_document(session_path.as_path())?;
            let result_indices = document
                .records
                .iter()
                .enumerate()
                .filter(|record| {
                    record.1.event_type == centaeris_core::session::SessionRecordType::ToolResult
                        && record.1.payload.get("callId").and_then(Value::as_str)
                            == Some(call.id.as_str())
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if result_indices.len() != 1 {
                return Err(format!(
                    "recovery wrote {} ToolResults",
                    result_indices.len()
                ));
            }
            let result_index = result_indices[0];
            let terminal_index = document
                .records
                .iter()
                .position(|record| {
                    record.event_type == centaeris_core::session::SessionRecordType::AgentRunFailed
                        && record.agent_run_id.as_deref() == Some(old_agent_run_id)
                })
                .ok_or_else(|| "recovered AgentRun terminal is missing".to_string())?;
            let new_user_index = document
                .records
                .iter()
                .position(|record| {
                    record.event_type == centaeris_core::session::SessionRecordType::UserMessage
                        && record.agent_run_id.as_deref() == Some("agent-run-recover-new")
                })
                .ok_or_else(|| "new UserMessage is missing".to_string())?;
            if !(result_index < terminal_index && terminal_index < new_user_index) {
                return Err("recovery did not finish before the new UserMessage".to_string());
            }
            Ok::<(), String>(())
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        match previous_db_path {
            Some(value) => std::env::set_var("CENTAERIS_AGENT_RUNTIME_DB_PATH", value),
            None => std::env::remove_var("CENTAERIS_AGENT_RUNTIME_DB_PATH"),
        }
        fs::remove_dir_all(root).ok();
        drop(guard);
        result.expect("new turn recovery");
    }

    #[test]
    fn agent_run_completion_uses_the_core_final_turn_and_failure_keeps_a_sealed_final() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = unique_test_dir("agent-run-final-turn");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        let result = (|| {
            let session = sessions::create(sessions::SessionCreateRequest {
                title: Some("agent run final turn".to_string()),
                cwd: workspace.to_string_lossy().to_string(),
            })?;
            message_log::append_agent_run_started(
                session.id.as_str(),
                "turn-root",
                "agent-run-success",
                "start",
                1,
            )?;
            message_log::append_assistant_message(
                session.id.as_str(),
                "turn-root:2",
                Some("agent-run-success"),
                "final answer",
                "done",
                2,
            )?;
            persist_agent_run_completed(
                session.id.as_str(),
                "turn-root:2",
                "agent-run-success",
                Some("final answer"),
                &AgentRunStop::Finalized,
            )?;
            message_log::append_agent_run_terminal(
                session.id.as_str(),
                "turn-root",
                "agent-run-success",
                "succeeded",
                None,
                3,
            )?;

            message_log::append_agent_run_started(
                session.id.as_str(),
                "turn-failed",
                "agent-run-failed",
                "start",
                4,
            )?;
            message_log::append_assistant_message(
                session.id.as_str(),
                "turn-failed:2",
                Some("agent-run-failed"),
                "preserved final",
                "done",
                5,
            )?;
            let mut live_text = LiveTextAccumulator::new(
                session.id.clone(),
                "turn-failed:2".to_string(),
                "agent-run-failed".to_string(),
            );
            live_text.content.push_str("preserved final");
            persist_interrupted_live_text(&mut live_text, session.id.as_str(), "agent-run-failed")?;
            message_log::append_agent_run_terminal(
                session.id.as_str(),
                "turn-failed",
                "agent-run-failed",
                "failed",
                Some("late finalization failure"),
                6,
            )?;

            let messages = message_log::project_chat_messages(session.id.as_str())?;
            let failed = messages
                .iter()
                .filter(|message| message.agent_run_id.as_deref() == Some("agent-run-failed"))
                .collect::<Vec<_>>();
            if failed.len() != 2
                || failed[1].id != "message:turn-failed:2:assistant"
                || failed[1].content != "preserved final"
                || failed[1].status.as_deref() != Some("done")
            {
                return Err("late failure replaced or duplicated the sealed final".to_string());
            }
            Ok::<(), String>(())
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        fs::remove_dir_all(root).ok();
        drop(guard);
        result.expect("AgentRun final identity");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn spawned_agent_session_is_durable_before_the_parent_tag_is_published() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = unique_test_dir("spawned-agent-session");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        let result = (|| {
            let parent = sessions::create(sessions::SessionCreateRequest {
                title: Some("parent".to_string()),
                cwd: workspace.to_string_lossy().to_string(),
            })?;
            let update = TurnUpdate::RuntimeEvent {
                event: serde_json::from_value(serde_json::json!({
                    "id": "evt-agent-spawned",
                    "version": "v1",
                    "type": "SubagentSpawned",
                    "at": 42,
                    "sessionId": parent.id.as_str(),
                    "turnId": "turn-parent",
                    "taskId": "agent-research",
                    "parentTaskId": "task-parent",
                    "status": "queued",
                    "payload": {
                        "subagentId": "agent-research",
                        "childSessionId": "session-agent-research",
                        "childTurnId": "turn-child",
                        "runtimeJobId": "subagent.run:research",
                        "description": "research the issue"
                    },
                    "visibility": "user",
                    "meta": {},
                }))
                .expect("runtime event fixture"),
            };

            materialize_spawned_agent_session_blocking(parent.id.as_str(), &update)?;
            materialize_spawned_agent_session_blocking(parent.id.as_str(), &update)?;

            let child = sessions::get(sessions::SessionGetRequest {
                session_id: "session-agent-research".to_string(),
            })?;
            if child.parent_session_id.as_deref() != Some(parent.id.as_str())
                || child.runtime_job_id.as_deref() != Some("subagent.run:research")
            {
                return Err("child session binding mismatch".to_string());
            }
            let projection = message_log::project_session_log("session-agent-research")?;
            if projection.agent_runs.len() != 1
                || projection.agent_runs[0].status != "running"
                || projection.agent_runs[0].turn_id != "turn-child"
                || projection.messages.len() != 1
            {
                return Err("child transcript was not materialized once".to_string());
            }
            message_log::append_agent_turn_running(
                "session-agent-research",
                "subagent.run:research",
            )?;
            let running = message_log::project_session_log("session-agent-research")?;
            if running.agent_runs[0].status != "running" || running.messages.len() != 1 {
                return Err("running transition duplicated the child transcript".to_string());
            }
            Ok::<(), String>(())
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        fs::remove_dir_all(root).ok();
        drop(guard);
        result.expect("materialize spawned Agent session");
    }

    #[test]
    fn server_recovery_seals_live_text_as_error_and_cancels_turn() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = unique_test_dir("live-text-recovery");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        let result = (|| {
            let session = sessions::create(sessions::SessionCreateRequest {
                title: Some("live text recovery".to_string()),
                cwd: workspace.to_string_lossy().to_string(),
            })?;
            message_log::append_agent_run_started(
                session.id.as_str(),
                "turn-live-text",
                "agent-run-live-text",
                "start",
                1,
            )?;
            let mut journal = LiveTextJournal::create(
                user_data_layout::runtime_live_text_journal_dir_path().as_path(),
                LiveTextJournalKey {
                    session_id: session.id.clone(),
                    turn_id: "turn-live-text:2".to_string(),
                    agent_run_id: "agent-run-live-text".to_string(),
                },
            )?;
            journal.append(&[LiveTextOperation::Append {
                text: "partial output".to_string(),
            }])?;
            drop(journal);
            message_log::append_agent_run_started(
                session.id.as_str(),
                "turn-live-text-terminal",
                "agent-run-live-text-terminal",
                "start",
                2,
            )?;
            message_log::append_assistant_message(
                session.id.as_str(),
                "turn-live-text-terminal",
                Some("agent-run-live-text-terminal"),
                "already saved",
                "error",
                3,
            )?;
            let mut terminal_journal = LiveTextJournal::create(
                user_data_layout::runtime_live_text_journal_dir_path().as_path(),
                LiveTextJournalKey {
                    session_id: session.id.clone(),
                    turn_id: "turn-live-text-terminal".to_string(),
                    agent_run_id: "agent-run-live-text-terminal".to_string(),
                },
            )?;
            terminal_journal.append(&[LiveTextOperation::Append {
                text: "already saved".to_string(),
            }])?;
            drop(terminal_journal);
            message_log::append_agent_run_started(
                session.id.as_str(),
                "turn-zero-token",
                "agent-run-zero-token",
                "start",
                4,
            )?;

            fs::remove_dir_all(workspace.as_path()).expect("remove stale session cwd");

            recover_unsealed_live_text_journals()?;

            let assistant = message_log::project_chat_messages(session.id.as_str())?
                .into_iter()
                .find(|message| {
                    message.turn_id == "turn-live-text:2" && message.role == "assistant"
                })
                .ok_or_else(|| "recovered assistant is missing".to_string())?;
            if assistant.content != "partial output" || assistant.status.as_deref() != Some("error")
            {
                return Err(format!(
                    "unexpected recovered assistant: content={:?}, status={:?}",
                    assistant.content, assistant.status
                ));
            }
            let agent_run = message_log::project_agent_run("agent-run-live-text")?
                .ok_or_else(|| "recovered AgentRun is missing".to_string())?;
            if agent_run.status != "cancelled" {
                return Err(format!(
                    "unexpected recovered AgentRun status: {}",
                    agent_run.status
                ));
            }
            let terminal_agent_run =
                message_log::project_agent_run("agent-run-live-text-terminal")?
                    .ok_or_else(|| "terminal recovered AgentRun is missing".to_string())?;
            if terminal_agent_run.status != "cancelled" {
                return Err(format!(
                    "unexpected terminal recovered AgentRun status: {}",
                    terminal_agent_run.status
                ));
            }
            let zero_token_agent_run = message_log::project_agent_run("agent-run-zero-token")?
                .ok_or_else(|| "zero-token recovered AgentRun is missing".to_string())?;
            if zero_token_agent_run.status != "cancelled" {
                return Err(format!(
                    "unexpected zero-token recovered AgentRun status: {}",
                    zero_token_agent_run.status
                ));
            }
            if !LiveTextJournal::recover(
                user_data_layout::runtime_live_text_journal_dir_path().as_path(),
            )?
            .is_empty()
            {
                return Err("recovered journal was not sealed".to_string());
            }
            Ok::<(), String>(())
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        fs::remove_dir_all(root).ok();
        drop(guard);
        result.expect("recover live text");
    }

    #[test]
    fn active_cancel_request_does_not_commit_terminal_before_runtime_cleanup() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = unique_test_dir("active-cancel-order");
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.as_path()).expect("create workspace");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_log_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions"));
        let result = (|| {
            let session = sessions::create(sessions::SessionCreateRequest {
                title: Some("active cancel order".to_string()),
                cwd: workspace.to_string_lossy().to_string(),
            })?;
            let agent_run_id = "agent-run-active-cancel-order";
            let turn_id = "turn-active-cancel-order";
            message_log::append_agent_run_started(
                session.id.as_str(),
                turn_id,
                agent_run_id,
                "start",
                1,
            )?;
            let registry = crate::runtime_server::AgentRunRegistry::default();
            let lease = registry
                .start(
                    session.id.as_str(),
                    agent_run_id,
                    turn_id,
                    "runtime-client-test",
                    crate::runtime_server::RuntimeClientKind::Desktop,
                    TurnControl::new(),
                )
                .map_err(|error| format!("start active AgentRun failed: {error:?}"))?;
            let active = registry
                .active(agent_run_id)?
                .ok_or_else(|| "active AgentRun missing".to_string())?;
            let response = request_active_agent_run_cancellation(
                &active,
                agent_runs::AgentRunCancelRequest {
                    agent_run_id: Some(agent_run_id.to_string()),
                    session_id: Some(session.id.clone()),
                    reason: Some("user_interrupt".to_string()),
                },
                "user_interrupt",
            )?;
            if !response.cancelled
                || response.agent_run.as_ref().map(|run| run.status.as_str()) != Some("running")
            {
                return Err("Stop request must return the still-running AgentRun".to_string());
            }
            let projected = message_log::project_agent_run(agent_run_id)?
                .ok_or_else(|| "projected AgentRun missing".to_string())?;
            if projected.status != "running"
                || active.cancellation_reason()?.as_deref() != Some("user_interrupt")
            {
                return Err("Stop request committed terminal before cleanup".to_string());
            }
            registry.finish(lease.lease_id.as_str())?;
            Ok::<(), String>(())
        })();
        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_log_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        fs::remove_dir_all(root).ok();
        drop(guard);
        result.expect("request active AgentRun cancellation");
    }

    #[test]
    fn session_prompt_request_rejects_unknown_field() {
        let error = serde_json::from_value::<AgentInputRequest>(serde_json::json!({
            "operationId": "prompt-operation-strict",
            "sessionId": "chat-strict",
            "message": "hello",
            "unexpectedField": true
        }))
        .expect_err("unknown field must fail loudly");

        assert!(error.to_string().contains("unexpectedField"));
    }

    #[test]
    fn session_prompt_request_requires_operation_identity() {
        let error = serde_json::from_value::<AgentInputRequest>(serde_json::json!({
            "sessionId": "chat-strict",
            "message": "hello"
        }))
        .expect_err("operationId must be required");

        assert!(error.to_string().contains("operationId"));
    }

    #[test]
    fn session_prompt_operation_identity_uses_the_shared_bounded_opaque_contract() {
        for operation_id in [
            "".to_string(),
            " leading".to_string(),
            "path/segment".to_string(),
            "x".repeat(operation_receipts::MAX_OPERATION_ID_BYTES + 1),
        ] {
            let error = serde_json::from_value::<AgentInputRequest>(serde_json::json!({
                "operationId": operation_id,
                "sessionId": "chat-strict",
                "message": "hello"
            }))
            .expect_err("invalid operationId must fail loudly");
            assert!(error.to_string().contains("operationId"));
        }

        serde_json::from_value::<AgentInputRequest>(serde_json::json!({
            "operationId": "session-prompt:client_01.2",
            "sessionId": "chat-strict",
            "message": "hello"
        }))
        .expect("valid operationId");
    }

    #[test]
    fn duplicate_prompt_operation_while_running_returns_the_original_response_once() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let environment = PromptTestEnvironment::new("prompt-operation-running");
        let runtime = tokio::runtime::Runtime::new().expect("prompt test runtime");
        let result = runtime.block_on(async {
            let session = environment.create_session("idempotent prompt")?;
            let (_clients, event_writer, _outbound) = prompt_event_writer("prompt-running-viewer");
            let first = input(
                event_writer.clone(),
                prompt_request("prompt-operation-running", session.id.as_str(), "hello"),
            )
            .map_err(|error| error.to_string())?;
            let duplicate = input(
                event_writer.clone(),
                serde_json::from_value(serde_json::json!({
                    "operationId": "prompt-operation-running",
                    "sessionId": format!(" {} ", session.id),
                    "message": " hello ",
                    "tailPolicy": "append"
                }))
                .expect("normalized duplicate prompt request"),
            );
            stop_prompt_agent_run(&event_writer, &first).await;
            let duplicate = duplicate.map_err(|error| error.to_string())?;
            if duplicate.agent_run_id != first.agent_run_id || duplicate.turn_id != first.turn_id {
                return Err("running duplicate did not return the original identities".to_string());
            }
            let messages = message_log::project_chat_messages(session.id.as_str())?;
            let user_messages = messages
                .iter()
                .filter(|message| message.role == "user")
                .collect::<Vec<_>>();
            let runs = message_log::project_agent_runs_for_session(session.id.as_str())?;
            if user_messages.len() != 1 || runs.len() != 1 {
                return Err(format!(
                    "running duplicate committed messages={} AgentRuns={}",
                    user_messages.len(),
                    runs.len()
                ));
            }
            Ok::<(), String>(())
        });
        drop(runtime);
        drop(environment);
        drop(guard);
        result.expect("running session/prompt replay");
    }

    #[test]
    fn completed_prompt_operation_replays_after_response_loss_and_runtime_reopen() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let environment = PromptTestEnvironment::new("prompt-operation-reopen");
        let runtime = tokio::runtime::Runtime::new().expect("prompt test runtime");
        let result = runtime.block_on(async {
            let session = environment.create_session("reopen prompt")?;
            let (first_clients, first_writer, first_outbound) =
                prompt_event_writer("prompt-first-viewer");
            let committed_before_response_loss = input(
                first_writer.clone(),
                prompt_request("prompt-operation-reopen", session.id.as_str(), "hello"),
            )
            .map_err(|error| error.to_string())?;
            stop_prompt_agent_run(&first_writer, &committed_before_response_loss).await;
            drop(first_writer);
            drop(first_outbound);
            drop(first_clients);

            let (_reopened_clients, reopened_writer, _reopened_outbound) =
                prompt_event_writer("prompt-reopened-viewer");
            let replayed = input(
                reopened_writer.clone(),
                prompt_request("prompt-operation-reopen", session.id.as_str(), "hello"),
            )
            .map_err(|error| error.to_string())?;
            if replayed.agent_run_id != committed_before_response_loss.agent_run_id
                || replayed.turn_id != committed_before_response_loss.turn_id
            {
                stop_prompt_agent_run(&reopened_writer, &replayed).await;
                return Err(
                    "reopened Runtime did not replay the original prompt receipt".to_string(),
                );
            }
            let messages = message_log::project_chat_messages(session.id.as_str())?;
            let runs = message_log::project_agent_runs_for_session(session.id.as_str())?;
            if messages
                .iter()
                .filter(|message| message.role == "user")
                .count()
                != 1
                || runs.len() != 1
            {
                return Err("reopened replay duplicated the prompt commit".to_string());
            }
            Ok::<(), String>(())
        });
        drop(runtime);
        drop(environment);
        drop(guard);
        result.expect("reopen session/prompt replay");
    }

    #[test]
    fn prompt_operation_recovers_when_the_receipt_committed_before_the_agent_run() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let environment = PromptTestEnvironment::new("prompt-operation-receipt-crash-window");
        let runtime = tokio::runtime::Runtime::new().expect("prompt test runtime");
        let result = runtime.block_on(async {
            let session = environment.create_session("receipt crash window")?;
            let operation_id = "prompt-operation-receipt-crash-window";
            let request_digest = operation_receipts::request_digest(&CanonicalAgentInputRequest {
                session_id: session.id.as_str(),
                message: "hello",
                tail_policy: "append",
                rewrite_target_message_id: None,
                rewrite_expected_tail_message_id: None,
                auto_continue_after_resume_wait: None,
                attachments: Vec::new(),
            })?;
            let expected = AgentInputResponse {
                session_id: session.id.clone(),
                agent_run_id: operation_receipts::deterministic_identity(
                    "agent-run-",
                    SESSION_PROMPT_METHOD,
                    operation_id,
                ),
                turn_id: operation_receipts::deterministic_identity(
                    "turn-",
                    SESSION_PROMPT_METHOD,
                    operation_id,
                ),
                stream_items: Vec::new(),
            };
            operation_receipts::write(
                SESSION_PROMPT_METHOD,
                operation_id,
                request_digest,
                serde_json::to_value(&expected).map_err(|error| error.to_string())?,
            )?;

            let (_clients, event_writer, _outbound) =
                prompt_event_writer("prompt-receipt-crash-viewer");
            let recovered = input(
                event_writer.clone(),
                prompt_request(operation_id, session.id.as_str(), "hello"),
            )
            .map_err(|error| error.to_string())?;
            if recovered != expected {
                stop_prompt_agent_run(&event_writer, &recovered).await;
                return Err("receipt-only recovery changed prompt identities".to_string());
            }
            let messages = message_log::project_chat_messages(session.id.as_str())?;
            let runs = message_log::project_agent_runs_for_session(session.id.as_str())?;
            stop_prompt_agent_run(&event_writer, &recovered).await;
            if messages
                .iter()
                .filter(|message| message.role == "user")
                .count()
                != 1
                || runs.len() != 1
            {
                return Err("receipt-only recovery did not commit exactly one prompt".to_string());
            }
            Ok::<(), String>(())
        });
        drop(runtime);
        drop(environment);
        drop(guard);
        result.expect("receipt-only session/prompt recovery");
    }

    #[test]
    fn duplicate_prompt_operation_with_changed_payload_fails_as_a_conflict() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let environment = PromptTestEnvironment::new("prompt-operation-conflict");
        let runtime = tokio::runtime::Runtime::new().expect("prompt test runtime");
        let result = runtime.block_on(async {
            let session = environment.create_session("conflicting prompt")?;
            let (_clients, event_writer, _outbound) = prompt_event_writer("prompt-conflict-viewer");
            let first = input(
                event_writer.clone(),
                prompt_request("prompt-operation-conflict", session.id.as_str(), "first"),
            )
            .map_err(|error| error.to_string())?;
            let conflict = input(
                event_writer.clone(),
                prompt_request("prompt-operation-conflict", session.id.as_str(), "changed"),
            );
            stop_prompt_agent_run(&event_writer, &first).await;
            let error = conflict.expect_err("changed payload must not reuse an operationId");
            if error.code() != "operation_id_conflict" {
                return Err(format!("conflict lacks stable classification: {error}"));
            }
            let messages = message_log::project_chat_messages(session.id.as_str())?;
            let runs = message_log::project_agent_runs_for_session(session.id.as_str())?;
            if messages
                .iter()
                .filter(|message| message.role == "user")
                .count()
                != 1
                || runs.len() != 1
            {
                return Err("conflicting operation mutated the original prompt".to_string());
            }
            Ok::<(), String>(())
        });
        drop(runtime);
        drop(environment);
        drop(guard);
        result.expect("conflicting session/prompt replay");
    }

    #[test]
    fn supplement_request_parses_protocol_fields() {
        let request = serde_json::from_value::<AgentSupplementRequest>(serde_json::json!({
            "sessionId": "chat-supplement",
            "agentRunId": "agent-run-supplement",
            "message": "change direction",
        }))
        .expect("supplement request");
        assert_eq!(request.session_id, "chat-supplement");
        assert_eq!(request.agent_run_id, "agent-run-supplement");
        assert_eq!(request.message, "change direction");
    }

    #[test]
    fn answer_now_request_is_the_exact_agent_run_intervention_contract() {
        let request = serde_json::from_value::<AgentRunInterventionV1>(serde_json::json!({
            "schema": "agent_run.intervention.v1",
            "interventionId": "intervention-1",
            "agentRunId": "agent-run-answer-now",
            "kind": "answer_now"
        }))
        .expect("answer-now intervention request");
        request.validate().expect("valid answer-now intervention");

        let error = serde_json::from_value::<AgentRunInterventionV1>(serde_json::json!({
            "schema": "agent_run.intervention.v1",
            "interventionId": "intervention-1",
            "agentRunId": "agent-run-answer-now",
            "kind": "answer_now",
            "sessionId": "host-owned-field"
        }))
        .expect_err("host wrapper fields must fail loudly");
        assert!(error.to_string().contains("sessionId"));
    }

    #[test]
    fn agent_response_serializes_session_agent_run_and_turn_ids() {
        let input = serde_json::to_value(AgentInputResponse {
            session_id: "chat-response".to_string(),
            agent_run_id: "agent-run-response".to_string(),
            turn_id: "turn-response".to_string(),
            stream_items: Vec::new(),
        })
        .expect("serialize agent input response");

        assert_eq!(input["sessionId"], "chat-response");
        assert_eq!(input["agentRunId"], "agent-run-response");
        assert_eq!(input["turnId"], "turn-response");
        assert!(input.get("SessionId").is_none());
        assert!(input.get("AgentRunId").is_none());

        let supplement = serde_json::to_value(AgentSupplementResponse {
            accepted: true,
            session_id: "chat-supplement".to_string(),
            agent_run_id: "agent-run-supplement".to_string(),
            queued_count: 1,
        })
        .expect("serialize agent supplement response");

        assert_eq!(supplement["accepted"], true);
        assert_eq!(supplement["sessionId"], "chat-supplement");
        assert_eq!(supplement["agentRunId"], "agent-run-supplement");
        assert_eq!(supplement["queuedCount"], 1);
        assert!(supplement.get("Accepted").is_none());
        assert!(supplement.get("QueuedCount").is_none());

        let answer_now = serde_json::to_value(AgentAnswerNowResponse {
            accepted: false,
            disposition: "alreadyConverging",
            session_id: "chat-answer-now".to_string(),
            agent_run_id: "agent-run-answer-now".to_string(),
            intervention_id: "intervention-1".to_string(),
        })
        .expect("serialize answer-now response");
        assert_eq!(answer_now["accepted"], false);
        assert_eq!(answer_now["disposition"], "alreadyConverging");
        assert_eq!(answer_now["interventionId"], "intervention-1");
    }

    #[test]
    fn question_answer_request_parses_protocol_fields() {
        let request = serde_json::from_value::<AgentQuestionAnswerRequest>(serde_json::json!({
            "sessionId": "chat-question",
            "questionId": "q-1",
            "answerText": "Use the release branch",
            "answers": [],
        }))
        .expect("question answer request");

        assert_eq!(request.question_id, "q-1");
        assert_eq!(
            request.answer_text.as_deref(),
            Some("Use the release branch")
        );
    }

    #[test]
    fn custom_anthropic_provider_uses_messages_wire_and_x_api_key() {
        let provider = custom_model_provider_info(
            "custom.anthropic",
            runtime_config::CustomModelProviderApi::AnthropicMessages,
            Some("test-key".to_string()),
            false,
        );

        assert_eq!(provider.provider_kind, ModelProviderKind::Custom);
        assert_eq!(provider.wire_api, WireApi::AnthropicMessages);
        assert_eq!(
            provider.http_headers.get("anthropic-version"),
            Some(&"2023-06-01".to_string())
        );
        assert_eq!(
            provider
                .resolve_auth_headers()
                .expect("auth headers")
                .get("x-api-key"),
            Some(&"test-key".to_string())
        );
    }

    #[test]
    fn custom_provider_without_key_omits_authentication() {
        let provider = custom_model_provider_info(
            "custom.local",
            runtime_config::CustomModelProviderApi::OpenAiCompletions,
            None,
            false,
        );

        assert_eq!(provider.auth, AuthSpec::None);
        assert!(provider
            .resolve_auth_headers()
            .expect("auth headers")
            .is_empty());
    }

    #[test]
    fn catalog_model_api_override_selects_its_wire_adapter() {
        assert_eq!(
            model_wire_api_to_wire_api(runtime_config::CustomModelProviderApi::OpenAiResponses),
            WireApi::OpenAiResponses
        );
        assert_eq!(
            model_wire_api_to_wire_api(runtime_config::CustomModelProviderApi::AnthropicMessages),
            WireApi::AnthropicMessages
        );
    }

    #[test]
    fn desktop_execution_host_reports_its_platform_capability() {
        let runner = centaeris_runtime::local_execution_host::LocalExecutionHostRunner::new(None)
            .expect("local execution host");
        let status = runner
            .status(
                &centaeris_core::execution::sandbox::SandboxPolicy::workspace_write_public_internet(
                    std::env::current_dir().expect("current directory"),
                ),
            )
            .expect("local status");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            status.kind,
            centaeris_core::execution::ExecutionHostKind::SandboxedProcess
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            status.kind,
            centaeris_core::execution::ExecutionHostKind::LocalProcess
        );
        #[cfg(target_os = "linux")]
        assert_eq!(status.sandbox_type, SandboxType::LinuxBubblewrap);
        #[cfg(target_os = "macos")]
        assert_eq!(status.sandbox_type, SandboxType::MacOsSeatbelt);
        #[cfg(target_os = "windows")]
        assert_eq!(status.sandbox_type, SandboxType::HostProcess);
    }

    #[test]
    fn session_prompt_request_rejects_unknown_tail_policy() {
        let request = serde_json::from_value::<AgentInputRequest>(serde_json::json!({
            "operationId": "prompt-operation-tail-policy",
            "sessionId": "chat-tail-policy",
            "message": "hello",
            "tailPolicy": "banana"
        }))
        .expect("deserialize request");
        let error = rewrite_last_user_input_from_request(&request)
            .expect_err("unsupported tailPolicy must fail loudly");

        assert!(error.contains("unsupported tailPolicy"));
    }

    #[test]
    fn session_prompt_request_rejects_rewrite_tail_policy_without_ids() {
        let request = serde_json::from_value::<AgentInputRequest>(serde_json::json!({
            "operationId": "prompt-operation-rewrite-missing-ids",
            "sessionId": "chat-rewrite",
            "message": "hello",
            "tailPolicy": "rewriteLastUser"
        }))
        .expect("deserialize request");
        let error = rewrite_last_user_input_from_request(&request)
            .expect_err("rewriteLastUser requires explicit target ids");

        assert!(error.contains("rewriteTargetMessageId"));
    }

    #[test]
    fn session_prompt_request_accepts_rewrite_tail_policy_with_ids() {
        let request = serde_json::from_value::<AgentInputRequest>(serde_json::json!({
            "operationId": "prompt-operation-rewrite",
            "sessionId": "chat-rewrite",
            "message": "hello",
            "tailPolicy": "rewriteLastUser",
            "rewriteTargetMessageId": "msg:user:turn-1",
            "rewriteExpectedTailMessageId": "msg:assistant:turn-1"
        }))
        .expect("deserialize request");
        let rewrite = rewrite_last_user_input_from_request(&request)
            .expect("parse rewrite policy")
            .expect("rewrite policy");

        assert_eq!(rewrite.target_chat_message_id, "msg:user:turn-1");
        assert_eq!(
            rewrite.expected_tail_chat_message_id,
            "msg:assistant:turn-1"
        );
    }

    #[test]
    fn deepseek_default_model_provider_resolves_to_openai_compatible_wire_api() {
        let registry = ModelProviderRegistry::new();
        let config = ModelSessionConfig {
            provider_kind: centaeris_core::model::ModelProviderKind::DeepSeek,
            provider_id: "deepseek.default".to_string(),
            model: "deepseek-v4-pro".to_string(),
            api_base: None,
            timeout_ms: DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS,
            max_retries: DEFAULT_MODEL_MAX_RETRIES,
            retry_backoff_ms: DEFAULT_MODEL_RETRY_BACKOFF_MS,
            max_output_tokens: None,
            thinking_mode: None,
            metadata: HashMap::new(),
        };

        assert_eq!(
            model_config_wire_api(&registry, &config).expect("deepseek wire api"),
            WireApi::OpenAiChatCompletions
        );
    }

    #[test]
    fn agent_runtime_uses_session_metadata_cwd() {
        let root = unique_test_dir("working-directory");
        let binding = sessions::AgentRuntimeBinding {
            cwd: root.to_string_lossy().to_string(),
        };
        let runtime = resolve_agent_runtime_from_binding(&binding).expect("working directory");

        assert_eq!(
            runtime.cwd.to_string_lossy(),
            workspaces::normalize_workspace_root_text(root.to_string_lossy().as_ref())
                .expect("normalized working directory")
        );
    }

    #[test]
    fn model_test_summary_values_are_raw_and_bounded() {
        assert_eq!(model_test_keyword("invalid_api_key"), "invalid_api_key");
        assert_eq!(model_test_keyword("one\n two\tthree"), "one two three");
        assert_eq!(model_test_keyword(&"x".repeat(100)).chars().count(), 96);
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "centaeris-agent-runtime-{name}-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(path.as_path()).expect("create test dir");
        path
    }
}
