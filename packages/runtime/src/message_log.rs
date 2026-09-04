use centaeris_core::model::ToolCallEnvelope;
use centaeris_core::runtime::contracts::{current_timestamp_ms, CheckpointRecord};
use centaeris_core::runtime::event::RuntimeEventProjection;
use centaeris_core::runtime::ModelRequestStartedV1;
use centaeris_core::session::supplement::DurableTurnSupplement;
use centaeris_core::session::{
    active_session_records, canonical_session_record, parse_manifest, parse_wire_record,
    project_committed_session_record, reduce_events, session_record_projects_to_agent_run_stream,
    validate_event_shape, wire_record_value, ReducedMessageRole, SequencedSessionRecord,
    SessionLogRecord, SessionManifestV1, SessionMetadataV1, SessionRecordType,
};
use centaeris_core::tool::layer::ToolExecutionResult;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::sync::{OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

mod observation_cas;

static SESSION_LOG_LOCK: OnceLock<RwLock<()>> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) struct ProjectedChatMessage {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) turn_id: String,
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) status: Option<String>,
    pub(crate) created_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) agent_run_id: Option<String>,
    pub(crate) image_data: Option<Value>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectedAgentRun {
    pub(crate) agent_run_id: String,
    pub(crate) session_id: String,
    pub(crate) turn_id: String,
    pub(crate) cwd: Option<String>,
    pub(crate) status: String,
    pub(crate) started_at_ms: i64,
    pub(crate) updated_at_ms: i64,
    pub(crate) completed_at_ms: Option<i64>,
    pub(crate) last_event_at_ms: Option<i64>,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct IncompleteToolCall {
    pub(crate) turn_id: String,
    pub(crate) call: ToolCallEnvelope,
    pub(crate) recorded_at_ms: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct RewriteLastUserInputRequest<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) target_chat_message_id: &'a str,
    pub(crate) expected_tail_chat_message_id: &'a str,
    pub(crate) new_turn_id: &'a str,
    pub(crate) new_agent_run_id: &'a str,
    pub(crate) new_content: &'a str,
    pub(crate) reason: &'a str,
    pub(crate) at_ms: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct RewriteLastUserInputResult {
    pub(crate) tombstoned_count: usize,
}

#[derive(Debug)]
pub(crate) struct ReplayResult {
    pub(crate) agent_run_id: String,
    pub(crate) cwd: Option<String>,
    pub(crate) items: Vec<Value>,
    pub(crate) next_cursor: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ProjectedAgentRunReplay {
    pub(crate) agent_run_id: String,
    pub(crate) items: Vec<Value>,
    pub(crate) next_cursor: u64,
}

#[derive(Debug)]
pub(crate) struct ProjectedSessionLog {
    pub(crate) messages: Vec<ProjectedChatMessage>,
    pub(crate) agent_runs: Vec<ProjectedAgentRun>,
    pub(crate) agent_run_replays: Vec<ProjectedAgentRunReplay>,
}

pub(crate) struct SessionDocument {
    pub(crate) manifest: SessionManifestV1,
    pub(crate) records: Vec<SessionLogRecord>,
}

pub(crate) enum CreateSessionDocumentError {
    AlreadyExists,
    Other(String),
}

impl From<String> for CreateSessionDocumentError {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}

impl CreateSessionDocumentError {
    pub(crate) fn into_string(self) -> String {
        match self {
            Self::AlreadyExists => "session log already exists".to_string(),
            Self::Other(error) => error,
        }
    }
}

pub(crate) fn create_session_document(
    path: &Path,
    manifest: SessionManifestV1,
    mut metadata: SessionMetadataV1,
) -> Result<(), CreateSessionDocumentError> {
    let _guard = lock_session_logs_for_write().map_err(CreateSessionDocumentError::Other)?;
    observation_cas::validate_session_log_path(path, manifest.session_id.as_str())
        .map_err(CreateSessionDocumentError::Other)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            CreateSessionDocumentError::Other(format!("create session log parent failed: {error}"))
        })?;
    }
    metadata.record_id = format!("session:{}:meta:1", manifest.session_id);
    let metadata_record =
        session_metadata_record(&manifest.session_id, metadata, 1, manifest.created_at_ms)?;
    let manifest_json = serde_json::to_string(&manifest)
        .map_err(|error| format!("serialize session manifest failed: {error}"))?;
    let metadata_json = serde_json::to_string(
        &wire_record_value(&SequencedSessionRecord {
            sequence: 1,
            event: metadata_record,
        })
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("serialize session metadata failed: {error}"))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                CreateSessionDocumentError::AlreadyExists
            } else {
                CreateSessionDocumentError::Other(format!(
                    "create session log failed for {}: {error}",
                    path.display()
                ))
            }
        })?;
    let result = writeln!(file, "{manifest_json}")
        .and_then(|_| writeln!(file, "{metadata_json}"))
        .and_then(|_| file.sync_data())
        .map_err(|error| {
            format!(
                "write session document failed for {}: {error}",
                path.display()
            )
        });
    if let Err(error) = result {
        drop(file);
        fs::remove_file(path).map_err(|cleanup_error| {
            format!(
                "{error}; cleanup incomplete Session log failed for {}: {cleanup_error}",
                path.display()
            )
        })?;
        return Err(CreateSessionDocumentError::Other(error));
    }
    Ok(())
}

pub(crate) fn append_session_metadata(
    path: &Path,
    session_id: &str,
    mut metadata: SessionMetadataV1,
    created_at_ms: i64,
) -> Result<(), String> {
    let session_id = required_string(session_id, "sessionId")?;
    let _guard = lock_session_logs_for_write()?;
    let existing = read_records_from_path_unlocked(path, Some(session_id.as_str()))?;
    let sequence = u64::try_from(existing.len() + 1)
        .map_err(|_| "session record sequence overflow".to_string())?;
    metadata.record_id = format!("session:{session_id}:meta:{sequence}");
    let record = session_metadata_record(session_id.as_str(), metadata, sequence, created_at_ms)?;
    append_records_unlocked(
        session_id.as_str(),
        existing.as_slice(),
        vec![record],
        Some(path),
    )
}

pub(crate) fn read_session_document(path: &Path) -> Result<SessionDocument, String> {
    let _guard = lock_session_logs_for_read()?;
    read_session_document_unlocked(path, None)
}

pub(crate) fn delete_session_document(path: &Path, session_id: &str) -> Result<(), String> {
    let _guard = lock_session_logs_for_write()?;
    observation_cas::delete_session_document(path, session_id)
}

pub(crate) fn cleanup_orphan_observation_content_directories(
    sessions_dir: &Path,
) -> Result<(), String> {
    let _guard = lock_session_logs_for_write()?;
    observation_cas::cleanup_orphan_content_directories(sessions_dir)
}

pub(crate) struct ProjectedAgentContextState {
    pub(crate) provider_usage: Option<centaeris_core::runtime::contracts::ProviderUsageV1>,
    pub(crate) context_token_estimate: Option<u64>,
    pub(crate) context_token_breakdown: Option<centaeris_core::runtime::ContextTokenBreakdownV1>,
    pub(crate) context_token_estimate_updated_at_ms: Option<i64>,
    pub(crate) latest_provider_usage_context_token_estimate: Option<u64>,
    pub(crate) is_compacting: bool,
}

fn agent_run_session_state(
    session_id: &str,
    agent_run_id: &str,
) -> Result<centaeris_core::session::AgentRunSessionState, String> {
    let mut state = centaeris_core::session::AgentRunSessionState::new(session_id, agent_run_id)?;
    for event in read_session_records(session_id)?
        .into_iter()
        .filter(|event| event.agent_run_id.as_deref() == Some(agent_run_id))
    {
        state.restore(SequencedSessionRecord {
            sequence: state.next_sequence() + 1,
            event,
        })?;
    }
    Ok(state)
}

fn append_agent_run_records(
    session_id: &str,
    records: Vec<SequencedSessionRecord>,
) -> Result<Vec<String>, String> {
    let event_ids = records
        .iter()
        .map(|record| record.event.event_id.clone())
        .collect::<Vec<_>>();
    append_records(
        session_id,
        records.into_iter().map(|record| record.event).collect(),
    )?;
    Ok(event_ids)
}

#[cfg(test)]
pub(crate) fn append_user_message(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    content: &str,
    at_ms: i64,
) -> Result<ProjectedChatMessage, String> {
    let agent_run_id = required_string(agent_run_id, "agentRunId")?;
    let mut state = agent_run_session_state(session_id, agent_run_id.as_str())?;
    let records = state.start(
        required_string(turn_id, "turnId")?.as_str(),
        required_string(content, "content")?.as_str(),
        Vec::new(),
        at_ms,
    )?;
    append_agent_run_records(session_id, records)?;
    project_chat_messages(session_id)?
        .into_iter()
        .find(|message| message.id == format!("message:{turn_id}:user"))
        .ok_or_else(|| format!("projected user message missing after append: {turn_id}"))
}

pub(crate) fn append_agent_run_started(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    content: &str,
    at_ms: i64,
) -> Result<(), String> {
    append_agent_run_started_with_attachments(
        session_id,
        turn_id,
        agent_run_id,
        content,
        Vec::new(),
        at_ms,
    )
}

pub(crate) fn append_agent_run_started_with_attachments(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    content: &str,
    attachments: Vec<Value>,
    at_ms: i64,
) -> Result<(), String> {
    let agent_run_id = required_string(agent_run_id, "agentRunId")?;
    let mut state = agent_run_session_state(session_id, agent_run_id.as_str())?;
    let records = state.start(
        required_string(turn_id, "turnId")?.as_str(),
        required_string(content, "content")?.as_str(),
        attachments,
        at_ms,
    )?;
    append_agent_run_records(session_id, records).map(|_| ())
}

pub(crate) fn append_turn_supplements(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    supplements: &[DurableTurnSupplement],
) -> Result<Vec<Value>, String> {
    let mut state = agent_run_session_state(session_id, agent_run_id)?;
    let mut records = Vec::new();
    for supplement in supplements {
        if let Some(record) = state.supplement(
            turn_id,
            supplement.supplement_id.as_str(),
            supplement.message.as_str(),
            supplement.created_at_ms,
        )? {
            records.push(record);
        }
    }
    let event_ids = append_agent_run_records(session_id, records)?;
    projected_stream_items_for_event_ids(session_id, agent_run_id, event_ids.as_slice())
}

pub(crate) fn append_agent_turn_queued(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    content: &str,
    at_ms: i64,
) -> Result<(), String> {
    append_agent_run_started(session_id, turn_id, agent_run_id, content, at_ms)
}

pub(crate) fn append_agent_turn_running(
    session_id: &str,
    agent_run_id: &str,
) -> Result<(), String> {
    project_agent_run(agent_run_id)?
        .filter(|agent_run| agent_run.session_id == session_id)
        .map(|_| ())
        .ok_or_else(|| {
            format!("agent_run_started missing before running agent_run transition: {agent_run_id}")
        })
}

pub(crate) fn append_assistant_message(
    session_id: &str,
    turn_id: &str,
    agent_run_id: Option<&str>,
    content: &str,
    status: &str,
    at_ms: i64,
) -> Result<ProjectedChatMessage, String> {
    let agent_run_id = required_string(agent_run_id.unwrap_or_default(), "agentRunId")?;
    let mut state = agent_run_session_state(session_id, agent_run_id.as_str())?;
    if let Some(record) = state.assistant(turn_id, content, Vec::new(), status, at_ms)? {
        append_agent_run_records(session_id, vec![record])?;
    }
    project_chat_messages(session_id)?
        .into_iter()
        .find(|message| message.id == format!("message:{turn_id}:assistant"))
        .ok_or_else(|| format!("projected assistant message missing after append: {turn_id}"))
}

pub(crate) fn append_runtime_event_fact(
    event: &RuntimeEventProjection,
    agent_run_id: &str,
) -> Result<Option<Value>, String> {
    let mut state = agent_run_session_state(event.session_id.as_str(), agent_run_id)?;
    let Some(record) = state.record_runtime_event(event)? else {
        return Ok(None);
    };
    let event_id = record.event.event_id.clone();
    append_agent_run_records(event.session_id.as_str(), vec![record])?;
    projected_stream_items_for_event_ids(
        event.session_id.as_str(),
        agent_run_id,
        std::slice::from_ref(&event_id),
    )?
    .into_iter()
    .next()
    .map(Some)
    .ok_or_else(|| format!("committed session_event projection missing: {event_id}"))
}

pub(crate) fn append_tool_call(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    call: &ToolCallEnvelope,
    provider_id: &str,
    tool_contract_digest: &str,
    recorded_at_ms: i64,
) -> Result<Vec<Value>, String> {
    let mut state = agent_run_session_state(session_id, agent_run_id)?;
    let Some(record) = state.record_tool_call(
        turn_id,
        call,
        provider_id,
        tool_contract_digest,
        call.name.as_str(),
        recorded_at_ms,
    )?
    else {
        return Ok(Vec::new());
    };
    let event_id = record.event.event_id.clone();
    append_agent_run_records(session_id, vec![record])?;
    projected_stream_items_for_event_ids(session_id, agent_run_id, &[event_id])
}

pub(crate) fn append_tool_result(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    call: &ToolCallEnvelope,
    result: &ToolExecutionResult,
) -> Result<Vec<Value>, String> {
    let mut state = agent_run_session_state(session_id, agent_run_id)?;
    let records = state.record_tool_result(turn_id, call, result, current_timestamp_ms())?;
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let event_ids = records
        .iter()
        .map(|record| record.event.event_id.clone())
        .collect::<Vec<_>>();
    append_agent_run_records(session_id, records)?;
    projected_stream_items_for_event_ids(session_id, agent_run_id, event_ids.as_slice())
}

pub(crate) fn append_model_request_started(
    agent_run_id: &str,
    started: &ModelRequestStartedV1,
    recorded_at_ms: i64,
) -> Result<(), String> {
    let mut state = agent_run_session_state(started.session_id(), agent_run_id)?;
    let records = state.record_model_request_started(started, recorded_at_ms)?;
    append_agent_run_records(started.session_id(), records).map(|_| ())
}

pub(crate) fn append_provider_usage(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    usage: &centaeris_core::runtime::contracts::ProviderTokenUsageV1,
    recorded_at_ms: i64,
) -> Result<(), String> {
    let mut state = agent_run_session_state(session_id, agent_run_id)?;
    if let Some(record) = state.provider_usage_record(turn_id, usage, recorded_at_ms)? {
        append_agent_run_records(session_id, vec![record])?;
    }
    Ok(())
}

pub(crate) fn append_checkpoint_ref(
    checkpoint: &CheckpointRecord,
    agent_run_id: &str,
) -> Result<(), String> {
    let session_id = required_string(checkpoint.session_id.as_str(), "sessionId")?;
    let agent_run_id = required_string(agent_run_id, "agentRunId")?;
    let mut state = agent_run_session_state(session_id.as_str(), agent_run_id.as_str())?;
    let record = state.checkpoint_ref(checkpoint)?;
    append_agent_run_records(session_id.as_str(), vec![record]).map(|_| ())
}

pub(crate) fn append_agent_run_terminal(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    status: &str,
    error: Option<&str>,
    at_ms: i64,
) -> Result<ProjectedAgentRun, String> {
    let session_id = required_string(session_id, "sessionId")?;
    let turn_id = required_string(turn_id, "turnId")?;
    let agent_run_id = required_string(agent_run_id, "agentRunId")?;
    let incomplete = project_incomplete_tool_calls(session_id.as_str(), agent_run_id.as_str())?;
    if let Some(call) = incomplete.first() {
        return Err(format!(
            "AgentRun terminal requires every ToolCall to be closed: callId={}",
            call.call.id
        ));
    }
    let mut state = agent_run_session_state(session_id.as_str(), agent_run_id.as_str())?;
    let record = match status {
        "succeeded" => state.complete(turn_id.as_str(), "finalized", at_ms)?,
        "failed" => state.fail(
            turn_id.as_str(),
            "runtime_error",
            required_string(error.unwrap_or_default(), "error")?.as_str(),
            at_ms,
        )?,
        "cancelled" => state.interrupt(
            turn_id.as_str(),
            "cancelled",
            error.unwrap_or("AgentRun cancelled"),
            false,
            at_ms,
        )?,
        "stopped" => state.interrupt(
            turn_id.as_str(),
            "stopped",
            error.unwrap_or("AgentRun stopped"),
            false,
            at_ms,
        )?,
        other => return Err(format!("unsupported terminal agent_run status: {other}")),
    };
    append_agent_run_records(session_id.as_str(), vec![record])?;
    project_agent_run(agent_run_id.as_str())?
        .ok_or_else(|| format!("projected agent_run missing after terminal append: {agent_run_id}"))
}

pub(crate) fn append_file_mutation_pre_apply_fact(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    file_fact: Value,
) -> Result<(), String> {
    if file_fact.get("schema").and_then(Value::as_str) != Some("file_mutation_pre_apply_fact_v1") {
        return Err(
            "file mutation pre-apply append requires file_mutation_pre_apply_fact_v1".to_string(),
        );
    }
    let agent_run = project_agent_run(agent_run_id)?.ok_or_else(|| {
        format!("agent_run_started missing before file mutation fact: {agent_run_id}")
    })?;
    if agent_run.session_id != session_id || agent_run.turn_id != turn_id {
        return Err(format!(
            "file mutation AgentRun identity mismatch: {agent_run_id}"
        ));
    }
    let at_ms = current_timestamp_ms();
    let mut state = agent_run_session_state(session_id, agent_run_id)?;
    let record = state.file_fact(turn_id, file_fact, at_ms)?;
    append_agent_run_records(session_id, vec![record])?;
    Ok(())
}

pub(crate) fn rewrite_last_user_input(
    request: RewriteLastUserInputRequest<'_>,
) -> Result<RewriteLastUserInputResult, String> {
    validate_positive_timestamp(request.at_ms, "rewrite atMs")?;
    let session_id = required_string(request.session_id, "sessionId")?;
    if request.reason != "rewrite_last_user_input" {
        return Err(format!("unsupported rewrite reason: {}", request.reason));
    }
    let _guard = lock_session_logs_for_write()?;
    let existing = read_session_records_unlocked(session_id.as_str())?;
    let tombstone = centaeris_core::session::rewrite_last_user_tail_tombstone(
        existing.as_slice(),
        session_id.as_str(),
        request.target_chat_message_id,
        request.expected_tail_chat_message_id,
        request.new_turn_id,
        request.new_agent_run_id,
        request.at_ms,
    )?;
    let tombstoned_count = tombstone
        .payload
        .get("targetEventIds")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| "rewrite tombstone targetEventIds missing".to_string())?;
    let mut state = centaeris_core::session::AgentRunSessionState::new(
        session_id.as_str(),
        request.new_agent_run_id,
    )?;
    state.record(tombstone.clone())?;
    let started = state.start(
        required_string(request.new_turn_id, "newTurnId")?.as_str(),
        required_string(request.new_content, "newContent")?.as_str(),
        Vec::new(),
        request.at_ms.saturating_add(1),
    )?;
    let mut records = Vec::with_capacity(started.len() + 1);
    records.push(tombstone);
    records.extend(started.into_iter().map(|record| record.event));
    append_records_unlocked(session_id.as_str(), existing.as_slice(), records, None)?;
    Ok(RewriteLastUserInputResult { tombstoned_count })
}

pub(crate) fn project_chat_messages(session_id: &str) -> Result<Vec<ProjectedChatMessage>, String> {
    let session_id = required_string(session_id, "sessionId")?;
    let records = active_records(read_session_records(session_id.as_str())?.as_slice())?;
    project_chat_messages_from_records(session_id.as_str(), records.as_slice())
}

pub(crate) fn project_agent_run_assistant(
    session_id: &str,
    agent_run_id: &str,
) -> Result<Option<ProjectedChatMessage>, String> {
    let agent_run_id = required_string(agent_run_id, "agentRunId")?;
    Ok(project_chat_messages(session_id)?
        .into_iter()
        .find(|message| {
            message.role == "assistant"
                && message.agent_run_id.as_deref() == Some(agent_run_id.as_str())
        }))
}

pub(crate) fn project_session_log(session_id: &str) -> Result<ProjectedSessionLog, String> {
    let session_id = required_string(session_id, "sessionId")?;
    let records = active_records(read_session_records(session_id.as_str())?.as_slice())?;
    let messages = project_chat_messages_from_records(session_id.as_str(), records.as_slice())?;
    let agent_runs = project_agent_runs_from_records(records.as_slice())?;
    let mut agent_run_replays = Vec::with_capacity(agent_runs.len());
    for agent_run in &agent_runs {
        let items = agent_run_replay_items(records.as_slice(), agent_run)?;
        agent_run_replays.push(ProjectedAgentRunReplay {
            agent_run_id: agent_run.agent_run_id.clone(),
            next_cursor: items.len() as u64,
            items,
        });
    }
    Ok(ProjectedSessionLog {
        messages,
        agent_runs,
        agent_run_replays,
    })
}

pub(crate) fn restore_runtime_snapshot(
    session_id: &str,
) -> Result<centaeris_core::session::state::SessionStateSnapshot, String> {
    let records = read_session_records(session_id)?;
    centaeris_core::session::restore_runtime_snapshot_from_session_records(
        session_id,
        records.as_slice(),
    )
}

pub(crate) fn project_incomplete_tool_calls(
    session_id: &str,
    agent_run_id: &str,
) -> Result<Vec<IncompleteToolCall>, String> {
    let records = active_records(read_session_records(session_id)?.as_slice())?;
    let projection = reduce_events(session_id, records.iter())?;
    projection
        .tool_calls
        .values()
        .filter(|call| call.agent_run_id == agent_run_id && call.result_state.is_none())
        .map(|call| {
            let recorded_at_ms = records
                .iter()
                .find(|event| {
                    event.event_type == SessionRecordType::ToolCall
                        && event.payload.get("callId").and_then(Value::as_str)
                            == Some(call.call_id.as_str())
                })
                .map(|event| event.created_at_ms)
                .ok_or_else(|| {
                    format!(
                        "incomplete ToolCall record is missing: callId={}",
                        call.call_id
                    )
                })?;
            Ok(IncompleteToolCall {
                turn_id: call.turn_id.clone(),
                call: ToolCallEnvelope {
                    id: call.call_id.clone(),
                    name: call.tool_name.clone(),
                    args_json: serde_json::to_string(&call.normalized_input).map_err(|error| {
                        format!("encode incomplete ToolCall input failed: {error}")
                    })?,
                },
                recorded_at_ms,
            })
        })
        .collect()
}

pub(crate) fn project_agent_context_state(
    session_id: &str,
) -> Result<ProjectedAgentContextState, String> {
    let session_id = required_string(session_id, "sessionId")?;
    let records = active_records(read_session_records(session_id.as_str())?.as_slice())?;
    let projection = reduce_events(session_id.as_str(), records.iter())?;
    Ok(ProjectedAgentContextState {
        provider_usage: projection.provider_usage(),
        context_token_estimate: projection.context_token_estimate(),
        context_token_breakdown: projection.context_token_breakdown().cloned(),
        context_token_estimate_updated_at_ms: projection.context_token_estimate_updated_at_ms(),
        latest_provider_usage_context_token_estimate: projection
            .latest_provider_usage_context_token_estimate(),
        is_compacting: projection.is_compacting(),
    })
}

pub(crate) fn project_agent_runs() -> Result<Vec<ProjectedAgentRun>, String> {
    let mut agent_runs = Vec::new();
    for file_path in crate::user_data_layout::session_log_file_paths()? {
        let projected = read_records_from_path(file_path.as_path(), None)
            .and_then(|records| active_records(records.as_slice()))
            .and_then(|records| project_agent_runs_from_records(records.as_slice()));
        match projected {
            Ok(mut projected) => agent_runs.append(&mut projected),
            Err(error) => eprintln!(
                "session_agent_run_projection_isolated: path={} error={error}",
                file_path.display()
            ),
        }
    }
    agent_runs.sort_by(|left, right| {
        left.started_at_ms
            .cmp(&right.started_at_ms)
            .then_with(|| left.agent_run_id.cmp(&right.agent_run_id))
    });
    Ok(agent_runs)
}

pub(crate) fn project_agent_runs_for_session(
    session_id: &str,
) -> Result<Vec<ProjectedAgentRun>, String> {
    let session_id = required_string(session_id, "sessionId")?;
    let records = active_records(read_session_records(session_id.as_str())?.as_slice())?;
    project_agent_runs_from_records(records.as_slice())
}

pub(crate) fn project_agent_run(agent_run_id: &str) -> Result<Option<ProjectedAgentRun>, String> {
    let agent_run_id = required_string(agent_run_id, "agentRunId")?;
    Ok(project_agent_runs()?
        .into_iter()
        .find(|agent_run| agent_run.agent_run_id == agent_run_id))
}

pub(crate) fn replay_agent_run(
    agent_run_id: &str,
    cursor: Option<u64>,
    limit: Option<usize>,
) -> Result<ReplayResult, String> {
    let agent_run_id = required_string(agent_run_id, "agentRunId")?;
    let agent_run = project_agent_run(agent_run_id.as_str())?
        .ok_or_else(|| format!("AgentRun not found: {agent_run_id}"))?;
    let records = active_records(read_session_records(agent_run.session_id.as_str())?.as_slice())?;
    let all_items = agent_run_replay_items(records.as_slice(), &agent_run)?;
    let start = usize::try_from(cursor.unwrap_or(0))
        .map_err(|_| "agent_run replay cursor is too large".to_string())?;
    if start > all_items.len() {
        return Err(format!("agent_run replay cursor is beyond tail: {start}"));
    }
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    let end = start.saturating_add(limit).min(all_items.len());
    Ok(ReplayResult {
        agent_run_id,
        cwd: agent_run.cwd,
        items: all_items[start..end].to_vec(),
        next_cursor: (end < all_items.len()).then_some(end as u64),
    })
}

pub(crate) fn terminal_agent_run_stream_projection(agent_run_id: &str) -> Result<Value, String> {
    let agent_run = project_agent_run(agent_run_id)?
        .ok_or_else(|| format!("AgentRun not found: {agent_run_id}"))?;
    let records = active_records(read_session_records(agent_run.session_id.as_str())?.as_slice())?;
    let terminal = records
        .iter()
        .rev()
        .find(|record| {
            record.agent_run_id.as_deref() == Some(agent_run.agent_run_id.as_str())
                && matches!(
                    record.event_type,
                    SessionRecordType::AgentRunCompleted
                        | SessionRecordType::AgentRunFailed
                        | SessionRecordType::AgentRunInterrupted
                )
        })
        .ok_or_else(|| format!("agent_run has no committed terminal record: {agent_run_id}"))?;
    let items = agent_run_replay_items(records.as_slice(), &agent_run)?;
    let index = items
        .iter()
        .position(|item| {
            item.pointer("/event/id").and_then(Value::as_str) == Some(terminal.event_id.as_str())
        })
        .ok_or_else(|| format!("terminal session_event projection missing: {agent_run_id}"))?;
    let projection = items[index].clone();
    if projection.get("type").and_then(Value::as_str) != Some("session_event") {
        return Err(format!(
            "terminal stream projection is invalid: {agent_run_id}"
        ));
    }
    Ok(projection)
}

fn project_chat_messages_from_records(
    session_id: &str,
    records: &[SessionLogRecord],
) -> Result<Vec<ProjectedChatMessage>, String> {
    let projection = reduce_events(session_id, records.iter())?;
    let mut positions = HashMap::new();
    for (index, record) in records.iter().enumerate() {
        if !matches!(
            record.event_type,
            SessionRecordType::UserMessage | SessionRecordType::AssistantMessage
        ) {
            continue;
        }
        if let Some(message_id) = record.payload.get("messageId").and_then(Value::as_str) {
            positions.insert(message_id.to_string(), (index, record.created_at_ms));
        }
    }
    let mut messages = projection
        .messages
        .into_values()
        .map(|message| {
            let (position, created_at_ms) = positions
                .get(message.message_id.as_str())
                .copied()
                .ok_or_else(|| format!("message record missing for {}", message.message_id))?;
            let role = match message.role {
                ReducedMessageRole::User => "user",
                ReducedMessageRole::Assistant => "assistant",
            };
            Ok((
                position,
                ProjectedChatMessage {
                    id: message.message_id,
                    session_id: session_id.to_string(),
                    turn_id: message
                        .turn_id
                        .ok_or_else(|| "projected message turnId is required".to_string())?,
                    role: role.to_string(),
                    content: message.text,
                    status: message.status,
                    created_at_ms,
                    updated_at_ms: message.updated_at_ms,
                    agent_run_id: message.agent_run_id.clone(),
                    image_data: None,
                },
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    messages.sort_by_key(|(position, _)| *position);
    Ok(messages.into_iter().map(|(_, message)| message).collect())
}

fn project_agent_runs_from_records(
    records: &[SessionLogRecord],
) -> Result<Vec<ProjectedAgentRun>, String> {
    let mut agent_runs = HashMap::<String, ProjectedAgentRun>::new();
    for record in records {
        let Some(agent_run_id) = record.agent_run_id.as_deref() else {
            continue;
        };
        if record.event_type == SessionRecordType::AgentRunStarted {
            let turn_id = record
                .turn_id
                .clone()
                .ok_or_else(|| format!("agent_run_started missing turnId: {}", record.event_id))?;
            let cwd = Some(crate::sessions::persisted_cwd_for_session_id(
                record.session_id.as_str(),
            )?);
            agent_runs
                .entry(agent_run_id.to_string())
                .or_insert_with(|| ProjectedAgentRun {
                    agent_run_id: agent_run_id.to_string(),
                    session_id: record.session_id.clone(),
                    turn_id,
                    cwd,
                    status: "running".to_string(),
                    started_at_ms: record.created_at_ms,
                    updated_at_ms: record.created_at_ms,
                    completed_at_ms: None,
                    last_event_at_ms: Some(record.created_at_ms),
                    error: None,
                });
        }
        let Some(agent_run) = agent_runs.get_mut(agent_run_id) else {
            if record.event_type == SessionRecordType::SessionMeta {
                continue;
            }
            return Err(format!(
                "agent_run record appears before agent_run_started: {}",
                record.event_id
            ));
        };
        agent_run.updated_at_ms = agent_run.updated_at_ms.max(record.created_at_ms);
        agent_run.last_event_at_ms = Some(
            agent_run
                .last_event_at_ms
                .unwrap_or(0)
                .max(record.created_at_ms),
        );
        match record.event_type {
            SessionRecordType::AgentRunCompleted => {
                agent_run.status = "succeeded".to_string();
                agent_run.completed_at_ms = Some(record.created_at_ms);
            }
            SessionRecordType::AgentRunFailed => {
                agent_run.status = "failed".to_string();
                agent_run.error = record
                    .payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                agent_run.completed_at_ms = Some(record.created_at_ms);
            }
            SessionRecordType::AgentRunInterrupted => {
                agent_run.status = match record.payload.get("reasonType").and_then(Value::as_str) {
                    Some("cancelled") => "cancelled",
                    Some("stopped" | "shutdown" | "provider_interrupted") => "stopped",
                    Some(other) => return Err(format!("unsupported interruption reason: {other}")),
                    None => return Err("agent_run_interrupted reasonType is required".to_string()),
                }
                .to_string();
                agent_run.error = record
                    .payload
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                agent_run.completed_at_ms = Some(record.created_at_ms);
            }
            _ => {}
        }
    }
    let mut agent_runs = agent_runs.into_values().collect::<Vec<_>>();
    agent_runs.sort_by(|left, right| {
        left.started_at_ms
            .cmp(&right.started_at_ms)
            .then_with(|| left.agent_run_id.cmp(&right.agent_run_id))
    });
    Ok(agent_runs)
}

fn agent_run_replay_items(
    records: &[SessionLogRecord],
    agent_run: &ProjectedAgentRun,
) -> Result<Vec<Value>, String> {
    let mut items = Vec::new();
    for record in records
        .iter()
        .filter(|record| record.agent_run_id.as_deref() == Some(agent_run.agent_run_id.as_str()))
    {
        if !session_record_projects_to_agent_run_stream(record.event_type) {
            continue;
        }
        let projection = project_committed_session_record(record, items.len() as u64)?;
        items.push(
            serde_json::to_value(projection)
                .map_err(|error| format!("serialize session_event projection failed: {error}"))?,
        );
    }
    Ok(items)
}

fn projected_stream_items_for_event_ids(
    session_id: &str,
    agent_run_id: &str,
    event_ids: &[String],
) -> Result<Vec<Value>, String> {
    let wanted = event_ids.iter().map(String::as_str).collect::<HashSet<_>>();
    let agent_run = project_agent_run(agent_run_id)?
        .ok_or_else(|| format!("AgentRun not found: {agent_run_id}"))?;
    if agent_run.session_id != session_id {
        return Err(format!("AgentRun sessionId mismatch: {agent_run_id}"));
    }
    let records = active_records(read_session_records(session_id)?.as_slice())?;
    let items = agent_run_replay_items(records.as_slice(), &agent_run)?;
    let projected = items
        .into_iter()
        .filter(|item| {
            item.get("event")
                .and_then(|event| event.get("id"))
                .and_then(Value::as_str)
                .is_some_and(|event_id| wanted.contains(event_id))
        })
        .collect::<Vec<_>>();
    if projected.len() != wanted.len() {
        return Err(format!(
            "committed stream projection count mismatch: expected {}, got {}",
            wanted.len(),
            projected.len()
        ));
    }
    Ok(projected)
}

fn append_records(session_id: &str, records: Vec<SessionLogRecord>) -> Result<(), String> {
    let session_id = required_string(session_id, "sessionId")?;
    let _guard = lock_session_logs_for_write()?;
    let existing = read_session_records_unlocked(session_id.as_str())?;
    append_records_unlocked(session_id.as_str(), existing.as_slice(), records, None)
}

fn append_records_unlocked(
    session_id: &str,
    existing: &[SessionLogRecord],
    records: Vec<SessionLogRecord>,
    path: Option<&Path>,
) -> Result<(), String> {
    let known = existing
        .iter()
        .map(|record| (record.event_id.as_str(), record))
        .collect::<HashMap<_, _>>();
    let mut pending_ids = HashSet::new();
    let mut pending = Vec::new();
    for record in records {
        validate_event_shape(&record)?;
        if record.session_id != session_id {
            return Err(format!(
                "session record belongs to another sessionId: {}",
                record.event_id
            ));
        }
        if let Some(committed) = known.get(record.event_id.as_str()) {
            if *committed == &record {
                continue;
            }
            return Err(format!(
                "session record eventId conflict: {}",
                record.event_id
            ));
        }
        if !pending_ids.insert(record.event_id.clone()) {
            return Err(format!(
                "session record batch duplicate eventId: {}",
                record.event_id
            ));
        }
        pending.push(record);
    }
    if pending.is_empty() {
        return Ok(());
    }
    let mut combined = existing.to_vec();
    combined.extend(pending.iter().cloned());
    reduce_events(session_id, combined.iter())?;
    let file_path = path
        .map(Path::to_path_buf)
        .map_or_else(|| existing_session_log_file_path(session_id), Ok)?;
    let mut wires = pending
        .into_iter()
        .enumerate()
        .map(|(offset, record)| {
            let sequence = u64::try_from(existing.len() + offset + 1)
                .map_err(|_| "session record sequence overflow".to_string())?;
            wire_record_value(&SequencedSessionRecord {
                sequence,
                event: record,
            })
            .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    observation_cas::compact_and_install_wires(
        file_path.as_path(),
        session_id,
        wires.as_mut_slice(),
    )?;
    let mut file = OpenOptions::new()
        .append(true)
        .open(file_path.as_path())
        .map_err(|error| {
            format!(
                "open session log for append failed for {}: {error}",
                file_path.display()
            )
        })?;
    for wire in wires {
        serde_json::to_writer(&mut file, &wire)
            .map_err(|error| format!("serialize session record failed: {error}"))?;
        file.write_all(b"\n")
            .map_err(|error| format!("append session record failed: {error}"))?;
    }
    file.sync_data()
        .map_err(|error| format!("sync session log failed: {error}"))
}

fn read_session_records(session_id: &str) -> Result<Vec<SessionLogRecord>, String> {
    let file_path = existing_session_log_file_path(session_id)?;
    let _guard = lock_session_logs_for_read()?;
    read_records_from_path_unlocked(file_path.as_path(), Some(session_id))
}

fn read_session_records_unlocked(session_id: &str) -> Result<Vec<SessionLogRecord>, String> {
    let file_path = existing_session_log_file_path(session_id)?;
    read_records_from_path_unlocked(file_path.as_path(), Some(session_id))
}

fn read_records_from_path(
    path: &Path,
    expected_session_id: Option<&str>,
) -> Result<Vec<SessionLogRecord>, String> {
    let _guard = lock_session_logs_for_read()?;
    read_records_from_path_unlocked(path, expected_session_id)
}

fn read_records_from_path_unlocked(
    path: &Path,
    expected_session_id: Option<&str>,
) -> Result<Vec<SessionLogRecord>, String> {
    Ok(read_session_document_unlocked(path, expected_session_id)?.records)
}

fn read_session_document_unlocked(
    path: &Path,
    expected_session_id: Option<&str>,
) -> Result<SessionDocument, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("read session log failed for {}: {error}", path.display()))?;
    if contents.is_empty() {
        return Err(format!(
            "session log manifest is missing: {}",
            path.display()
        ));
    }
    if !contents.ends_with('\n') {
        return Err(format!(
            "session log has a truncated final line: {}",
            path.display()
        ));
    }
    let mut manifest = None;
    let mut wires = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            return Err(format!(
                "session log contains blank line: {}:{}",
                path.display(),
                index + 1
            ));
        }
        let value = serde_json::from_str::<Value>(line)
            .map_err(|error| format!("decode session log line {} failed: {error}", index + 1))?;
        if index == 0 {
            let parsed = parse_manifest(&value).map_err(|error| error.to_string())?;
            if expected_session_id.is_some_and(|expected| parsed.session_id != expected) {
                return Err("session manifest sessionId mismatch".to_string());
            }
            manifest = Some(parsed);
            continue;
        }
        wires.push(value);
    }
    let manifest = manifest.ok_or_else(|| "session log manifest is missing".to_string())?;
    observation_cas::validate_session_log_path(path, manifest.session_id.as_str())?;
    observation_cas::hydrate_wires(path, manifest.session_id.as_str(), wires.as_mut_slice())?;
    let mut records = Vec::with_capacity(wires.len());
    let mut ids = HashSet::new();
    for (index, value) in wires.into_iter().enumerate() {
        let sequenced = parse_wire_record(&value).map_err(|error| error.to_string())?;
        let expected_sequence =
            u64::try_from(index + 1).map_err(|_| "session record sequence overflow".to_string())?;
        if sequenced.sequence != expected_sequence {
            return Err(format!(
                "session record sequence gap: expected {expected_sequence}, got {}",
                sequenced.sequence
            ));
        }
        let record = sequenced.event;
        if expected_session_id.is_some_and(|expected| record.session_id != expected) {
            return Err(format!(
                "session log cross-session record: {}",
                record.event_id
            ));
        }
        if !ids.insert(record.event_id.clone()) {
            return Err(format!(
                "session log duplicate eventId: {}",
                record.event_id
            ));
        }
        records.push(record);
    }
    Ok(SessionDocument { manifest, records })
}

fn active_records(records: &[SessionLogRecord]) -> Result<Vec<SessionLogRecord>, String> {
    let Some(session_id) = records.first().map(|record| record.session_id.as_str()) else {
        return Ok(Vec::new());
    };
    active_session_records(session_id, records)
}

fn existing_session_log_file_path(session_id: &str) -> Result<PathBuf, String> {
    crate::user_data_layout::find_session_log_file_path(session_id)?
        .ok_or_else(|| format!("session log not found: {session_id}"))
}

fn session_metadata_record(
    session_id: &str,
    metadata: SessionMetadataV1,
    sequence: u64,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    metadata.validate()?;
    canonical_session_record(
        format!("evt:session_meta:{session_id}:{sequence}"),
        SessionRecordType::SessionMeta,
        session_id,
        None,
        None,
        created_at_ms,
        serde_json::to_value(metadata)
            .map_err(|error| format!("serialize session metadata failed: {error}"))?,
    )
}

fn lock_session_logs_for_read() -> Result<RwLockReadGuard<'static, ()>, String> {
    SESSION_LOG_LOCK
        .get_or_init(|| RwLock::new(()))
        .read()
        .map_err(|_| "session log lock poisoned".to_string())
}

fn lock_session_logs_for_write() -> Result<RwLockWriteGuard<'static, ()>, String> {
    SESSION_LOG_LOCK
        .get_or_init(|| RwLock::new(()))
        .write()
        .map_err(|_| "session log lock poisoned".to_string())
}

fn required_string(raw: &str, field: &str) -> Result<String, String> {
    raw.trim()
        .is_empty()
        .then(|| format!("{field} is required"))
        .map_or_else(|| Ok(raw.trim().to_string()), Err)
}

fn validate_positive_timestamp(value: i64, field: &str) -> Result<(), String> {
    (value > 0)
        .then_some(())
        .ok_or_else(|| format!("{field} must be positive unix milliseconds"))
}

#[cfg(test)]
pub(crate) fn test_env_mutex() -> &'static Mutex<()> {
    static TEST_ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_ENV_MUTEX.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::extension::composition::{
        resolve_agent_composition, AgentCompositionInputsV1, ResolvedModelBindingV1,
    };
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_model_request_payload(observations: Vec<Value>, request_id: &str) -> Value {
        let digest = format!("sha256:{}", "a".repeat(64));
        let composition = resolve_agent_composition(
            AgentCompositionInputsV1 {
                prompt_digest: digest.clone(),
                model_binding: ResolvedModelBindingV1 {
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
            "requestId": request_id,
            "purpose": "main",
            "loopIndex": 0,
            "toolChoice": {"type": "none"},
            "maxOutputTokens": 1024,
            "promptCacheKey": null,
            "promptCacheRetention": null,
            "preparedPromptSchema": "prepared_prompt.v1",
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

    fn copy_directory_files(source: &Path, destination: &Path) {
        fs::create_dir(destination).expect("create copied CAS directory");
        for entry in fs::read_dir(source).expect("read source CAS directory") {
            let entry = entry.expect("source CAS entry");
            assert!(entry.path().is_file());
            fs::copy(entry.path(), destination.join(entry.file_name())).expect("copy CAS content");
        }
    }

    #[test]
    fn canonical_fixture_projects_start_success_failure_and_interruption() {
        let fixtures = [
            (
                "succeeded",
                "turn-success",
                "agent_run-success",
                "done",
                "AgentRunCompleted",
            ),
            (
                "failed",
                "turn-failed",
                "agent_run-failed",
                "error",
                "AgentRunFailed",
            ),
            (
                "cancelled",
                "turn-cancelled",
                "agent_run-cancelled",
                "error",
                "AgentRunInterrupted",
            ),
        ];
        for (terminal_status, turn_id, agent_run_id, assistant_status, terminal_type) in fixtures {
            let mut state =
                centaeris_core::session::AgentRunSessionState::new("session", agent_run_id)
                    .unwrap();
            let mut records = state
                .start(turn_id, "objective", Vec::new(), 1)
                .unwrap()
                .into_iter()
                .map(|record| record.event)
                .collect::<Vec<_>>();
            records.push(
                state
                    .assistant(turn_id, "answer", Vec::new(), assistant_status, 2)
                    .unwrap()
                    .unwrap()
                    .event,
            );
            let terminal = match terminal_status {
                "succeeded" => state.complete(turn_id, "finalized", 3).unwrap(),
                "failed" => state.fail(turn_id, "runtime_error", "failed", 3).unwrap(),
                "cancelled" => state
                    .interrupt(turn_id, "cancelled", "cancelled", false, 3)
                    .unwrap(),
                _ => unreachable!(),
            };
            records.push(terminal.event);
            let projection = reduce_events("session", records.iter()).unwrap();
            assert_eq!(projection.messages.len(), 2);
            let assistant =
                serde_json::to_value(project_committed_session_record(&records[2], 2).unwrap())
                    .unwrap();
            assert_eq!(assistant["type"], "session_event");
            assert_eq!(assistant["event"]["type"], "Final");
            assert!(project_committed_session_record(records.last().unwrap(), 0).is_ok());
            assert_eq!(
                serde_json::to_value(
                    project_committed_session_record(records.last().unwrap(), 3).unwrap()
                )
                .unwrap()["event"]["type"],
                terminal_type
            );
        }
    }

    #[test]
    fn model_request_records_are_not_agent_run_stream_records() {
        assert!(!session_record_projects_to_agent_run_stream(
            SessionRecordType::ModelRequestStarted
        ));
        assert!(session_record_projects_to_agent_run_stream(
            SessionRecordType::ToolResult
        ));
    }

    #[test]
    fn agent_run_projection_skips_corrupt_session_logs() {
        let _guard = test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "centaeris-agent-run-projection-isolation-{}-{nonce}",
            std::process::id()
        ));
        let sessions_dir = root.join("sessions");
        let day_dir = sessions_dir.join("26").join("09").join("01");
        let workspace = root.join("workspace");
        fs::create_dir_all(day_dir.as_path()).expect("session day directory");
        fs::create_dir_all(workspace.as_path()).expect("workspace directory");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_sessions_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", &sessions_dir);

        let session_id = "session-agent-run-projection-healthy";
        create_session_document(
            day_dir.join(format!("{session_id}.jsonl")).as_path(),
            SessionManifestV1::new(
                session_id,
                1,
                centaeris_core::runtime::CORE_PROTOCOL_VERSION,
            )
            .expect("manifest"),
            SessionMetadataV1 {
                record_id: String::new(),
                title: "healthy".to_string(),
                cwd: workspace.to_string_lossy().to_string(),
                session_kind: "main".to_string(),
                parent_session_id: None,
                runtime_job_id: None,
                sort_order: Some(0),
                is_pinned: false,
                is_unread: false,
            },
        )
        .map_err(CreateSessionDocumentError::into_string)
        .expect("healthy session");
        append_agent_run_started(session_id, "turn-healthy", "agent-run-healthy", "test", 2)
            .expect("agent run");
        let corrupt_path = day_dir.join("session-corrupt.jsonl");
        fs::write(
            &corrupt_path,
            "{\"schemaVersion\":\"session.manifest.v2\"}\n",
        )
        .expect("corrupt fixture");

        let projected = project_agent_runs().expect("isolated projection");
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].agent_run_id, "agent-run-healthy");
        assert!(corrupt_path.is_file());

        match previous_data_dir {
            Some(value) => std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", value),
            None => std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR"),
        }
        match previous_sessions_dir {
            Some(value) => std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", value),
            None => std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR"),
        }
        fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn local_model_request_cas_round_trips_rewrites_copies_and_deletes_with_quantified_growth() {
        let guard = test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "centaeris-local-observation-cas-{}-{nonce}",
            std::process::id()
        ));
        let sessions_dir = root.join("sessions");
        let day_dir = sessions_dir.join("26").join("08").join("31");
        let workspace = root.join("workspace");
        fs::create_dir_all(day_dir.as_path()).expect("Session day directory");
        fs::create_dir_all(workspace.as_path()).expect("workspace directory");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        let previous_sessions_dir = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &root);
        std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", &sessions_dir);

        let result = (|| -> Result<(), String> {
            let session_id = "session-local-observation-cas";
            let turn_id = "turn-local-observation-cas";
            let agent_run_id = "agent-run-local-observation-cas";
            let log_path = day_dir.join(format!("{session_id}.jsonl"));
            create_session_document(
                log_path.as_path(),
                SessionManifestV1::new(
                    session_id,
                    1,
                    centaeris_core::runtime::CORE_PROTOCOL_VERSION,
                )
                .map_err(|error| error.to_string())?,
                SessionMetadataV1 {
                    record_id: String::new(),
                    title: "observation CAS".to_string(),
                    cwd: workspace.to_string_lossy().to_string(),
                    session_kind: "main".to_string(),
                    parent_session_id: None,
                    runtime_job_id: None,
                    sort_order: Some(0),
                    is_pinned: false,
                    is_unread: false,
                },
            )
            .map_err(CreateSessionDocumentError::into_string)?;
            append_agent_run_started(session_id, turn_id, agent_run_id, "objective", 2)?;

            let system_prompt = "system:".to_string() + "s".repeat(16 * 1024).as_str();
            let tool_catalog = json!({
                "kind": "tool_catalog",
                "toolDefinitions": [{
                    "name": "search_laws",
                    "description": "d".repeat(12 * 1024),
                    "inputSchema": {"type": "object"},
                }],
            });
            let mut context = Vec::new();
            let mut last_record = None;
            let mut old_model_bytes = 0usize;
            let mut old_model_rows = 0usize;
            let mut old_sequence = 4u64;
            for round in 1..=20u64 {
                for role in ["user", "assistant"] {
                    context.push(json!({
                        "kind": "message",
                        "message": {
                            "messageId": format!("message-{round}-{role}"),
                            "role": role,
                            "content": format!("{round}:{role}:{}", "m".repeat(512)),
                        },
                    }));
                }
                let observations = std::iter::once(json!({
                    "kind": "system_prompt",
                    "content": system_prompt,
                }))
                .chain(context.iter().cloned())
                .chain(std::iter::once(tool_catalog.clone()))
                .collect::<Vec<_>>();
                let request_id = format!("request-{round}");
                for (index, observation) in observations.iter().enumerate() {
                    let mut payload = observation.as_object().expect("observation object").clone();
                    payload.insert(
                        "observationId".to_string(),
                        json!(format!("{request_id}:observation:{index}")),
                    );
                    old_model_bytes += serde_json::to_vec(&json!({
                        "schemaVersion": "session.event.v1",
                        "eventVersion": 1,
                        "sequence": old_sequence,
                        "type": "model_observation",
                        "eventId": format!("event:{request_id}:observation:{index}"),
                        "sessionId": session_id,
                        "turnId": turn_id,
                        "agentRunId": agent_run_id,
                        "createdAtMs": 2 + round,
                        "payload": payload,
                    }))
                    .expect("encode old observation")
                    .len()
                        + 1;
                    old_model_rows += 1;
                    old_sequence += 1;
                }
                let payload = test_model_request_payload(observations, request_id.as_str());
                let record = canonical_session_record(
                    format!("event:model-request:{round}"),
                    SessionRecordType::ModelRequestStarted,
                    session_id,
                    Some(turn_id.to_string()),
                    Some(agent_run_id.to_string()),
                    2 + i64::try_from(round).expect("round timestamp"),
                    payload.clone(),
                )?;
                let mut old_boundary_payload = payload;
                old_boundary_payload
                    .as_object_mut()
                    .expect("request payload")
                    .remove("observations");
                let old_boundary_value = json!({
                    "schemaVersion": "session.event.v1",
                    "eventVersion": 1,
                    "sequence": old_sequence,
                    "type": "model_request_started",
                    "eventId": format!("event:model-request:{round}"),
                    "sessionId": session_id,
                    "turnId": turn_id,
                    "agentRunId": agent_run_id,
                    "createdAtMs": 2 + round,
                    "payload": old_boundary_payload,
                });
                old_model_bytes += serde_json::to_vec(&old_boundary_value)
                    .expect("encode old request boundary")
                    .len()
                    + 1;
                old_model_rows += 1;
                old_sequence += 1;
                append_records(session_id, vec![record.clone()])?;
                last_record = Some(record);
            }

            let raw = fs::read_to_string(log_path.as_path())
                .map_err(|error| format!("read raw local Session log failed: {error}"))?;
            if raw.contains(system_prompt.as_str())
                || !raw.contains("manifestDigest")
                || raw.contains("contentDigest")
            {
                return Err("local Session JSONL did not compact model observations".to_string());
            }
            let raw_model_lines = raw
                .lines()
                .filter(|line| line.contains("\"type\":\"model_request_started\""))
                .collect::<Vec<_>>();
            if raw_model_lines.len() != 20 {
                return Err("local Session JSONL model request row count mismatch".to_string());
            }
            let content_dir =
                observation_cas::content_directory_path(log_path.as_path(), session_id)?;
            let content_files = fs::read_dir(content_dir.as_path())
                .map_err(|error| format!("read local observation CAS failed: {error}"))?
                .map(|entry| {
                    entry
                        .map(|entry| entry.path())
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let manifest_count = content_files
                .iter()
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("manifest-"))
                })
                .count();
            let unique_contents = content_files.len() - manifest_count;
            if unique_contents != 42 || manifest_count != 20 {
                return Err(format!(
                    "local observation CAS unique content mismatch: {}",
                    unique_contents
                ));
            }
            let hydrated = read_session_document(log_path.as_path())?;
            let model_records = hydrated
                .records
                .iter()
                .filter(|record| record.event_type == SessionRecordType::ModelRequestStarted)
                .collect::<Vec<_>>();
            if model_records.len() != 20
                || model_records
                    .last()
                    .and_then(|record| record.payload["observations"].as_array().map(Vec::len))
                    != Some(42)
            {
                return Err("local observation CAS hydration lost order or content".to_string());
            }
            let restored = restore_runtime_snapshot(session_id)?;
            if restored.messages.len() != 40 {
                return Err("local observation CAS runtime restore lost messages".to_string());
            }

            let before_idempotent = fs::metadata(log_path.as_path())
                .map_err(|error| format!("stat Session log failed: {error}"))?
                .len();
            append_records(
                session_id,
                vec![last_record.expect("last model request record")],
            )?;
            if fs::metadata(log_path.as_path())
                .map_err(|error| format!("stat idempotent Session log failed: {error}"))?
                .len()
                != before_idempotent
            {
                return Err("idempotent model request append grew Session JSONL".to_string());
            }

            let copy_dir = root.join("copy");
            fs::create_dir(copy_dir.as_path())
                .map_err(|error| format!("create copy directory failed: {error}"))?;
            let copied_log = copy_dir.join(format!("{session_id}.jsonl"));
            fs::copy(log_path.as_path(), copied_log.as_path())
                .map_err(|error| format!("copy Session log failed: {error}"))?;
            copy_directory_files(
                content_dir.as_path(),
                copied_log.with_extension("observations").as_path(),
            );
            read_session_document(copied_log.as_path())?;
            let incomplete_copy_dir = root.join("incomplete-copy");
            fs::create_dir(incomplete_copy_dir.as_path())
                .map_err(|error| format!("create incomplete copy directory failed: {error}"))?;
            let incomplete_log = incomplete_copy_dir.join(format!("{session_id}.jsonl"));
            fs::copy(log_path.as_path(), incomplete_log.as_path())
                .map_err(|error| format!("copy incomplete Session log failed: {error}"))?;
            if read_session_document(incomplete_log.as_path()).is_ok() {
                return Err("Session JSONL copy without CAS did not fail loudly".to_string());
            }

            append_assistant_message(session_id, turn_id, Some(agent_run_id), "done", "done", 30)?;
            append_agent_run_terminal(session_id, turn_id, agent_run_id, "succeeded", None, 31)?;
            rewrite_last_user_input(RewriteLastUserInputRequest {
                session_id,
                target_chat_message_id: format!("message:{turn_id}:user").as_str(),
                expected_tail_chat_message_id: format!("message:{turn_id}:assistant").as_str(),
                new_turn_id: "turn-local-observation-cas-rewrite",
                new_agent_run_id: "agent-run-local-observation-cas-rewrite",
                new_content: "rewritten objective",
                reason: "rewrite_last_user_input",
                at_ms: 32,
            })?;
            read_session_document(log_path.as_path())?;

            let new_jsonl_model_bytes = raw_model_lines
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>();
            let cas_bytes = content_files.iter().try_fold(0usize, |total, path| {
                let length = fs::metadata(path)
                    .map_err(|error| format!("stat observation CAS content failed: {error}"))?
                    .len();
                usize::try_from(length)
                    .map(|length| total.saturating_add(length))
                    .map_err(|_| "observation CAS byte count overflow".to_string())
            })?;
            let new_physical_bytes = new_jsonl_model_bytes.saturating_add(cas_bytes);
            println!(
                "RUNTIME_01_ARTIFACT {}",
                json!({
                    "gate": "local_storage_lifecycle_20_round", "measurement": "actual_jsonl_and_cas_file_bytes",
                    "rounds": 20, "oldModelRows": old_model_rows, "newModelRows": 20,
                    "uniqueContents": unique_contents, "manifestNodes": manifest_count,
                    "oldPhysicalObjects": old_model_rows, "newPhysicalObjects": 20 + content_files.len(),
                    "oldModelBytes": old_model_bytes, "newJsonlModelBytes": new_jsonl_model_bytes,
                    "casBytes": cas_bytes, "newPhysicalBytes": new_physical_bytes,
                })
            );
            if old_model_rows != 480
                || 20 + content_files.len() != 82
                || new_physical_bytes.saturating_mul(5) >= old_model_bytes
            {
                return Err("local model observation amplification target was not met".to_string());
            }

            crate::session_files::SessionFiles::new(sessions_dir.clone()).delete(session_id)?;
            if log_path.exists() || content_dir.exists() {
                return Err("Session delete left local observation CAS behind".to_string());
            }
            let orphan = day_dir.join("session-orphan.observations");
            fs::create_dir(orphan.as_path())
                .map_err(|error| format!("create orphan CAS fixture failed: {error}"))?;
            fs::write(orphan.join(format!("{}.json", "a".repeat(64))), "{}")
                .map_err(|error| format!("write orphan CAS fixture failed: {error}"))?;
            crate::session_files::SessionFiles::new(sessions_dir.clone()).list()?;
            if orphan.exists() {
                return Err("orphan observation CAS cleanup did not run".to_string());
            }
            Ok(())
        })();

        if let Some(previous) = previous_data_dir {
            std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", previous);
        } else {
            std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR");
        }
        if let Some(previous) = previous_sessions_dir {
            std::env::set_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", previous);
        } else {
            std::env::remove_var("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR");
        }
        let _ = fs::remove_dir_all(root);
        drop(guard);
        result.expect("local observation CAS lifecycle");
    }
}
