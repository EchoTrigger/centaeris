//! Bounded, disposable reducer checkpoints. All semantic validation stays in Core.
use super::*;
use centaeris_core::session::{reduce_event, SessionProjection};
use std::sync::Mutex;

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_SESSIONS: usize = 8;
static CACHE: OnceLock<Mutex<Vec<(PathBuf, State)>>> = OnceLock::new();

pub(super) struct State {
    identity: SessionLogIdentity,
    manifest: SessionManifestV1,
    metadata: SessionMetadataV1,
    projection: SessionProjection,
    seen: HashSet<String>,
    open: HashMap<String, (String, String, String)>,
    known: HashMap<String, [u8; 32]>,
    runs: HashMap<String, ProjectedAgentRun>,
    catalog_sequence: Option<u64>,
    changed_runs: HashSet<String>,
    stream_indices: HashMap<String, (String, u64)>,
    stream_counts: HashMap<String, u64>,
    terminal_ids: HashMap<String, String>,
    message_times: HashMap<String, i64>,
    message_order: std::collections::BTreeSet<(i64, String)>,
    assistant_ids: HashMap<String, String>,
    run_states: HashMap<String, centaeris_core::session::AgentRunSessionState>,
    pub sequence: usize,
    updated: i64,
    bytes: usize,
}

fn digest(record: &SessionLogRecord) -> Result<[u8; 32], String> {
    Ok(Sha256::digest(serde_json::to_vec(record).map_err(|e| e.to_string())?).into())
}

pub(super) fn take(path: &Path) -> Result<State, String> {
    let identity = session_log_identity(path)?;
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(vec![]))
        .lock()
        .map_err(|_| "read-state lock poisoned")?;
    if let Some(index) = cache.iter().position(|(p, _)| p == path) {
        let (_, state) = cache.remove(index);
        if state.identity == identity {
            return Ok(state);
        }
    }
    drop(cache);
    let document = read_session_document_unlocked(path, None)?;
    let metadata = serde_json::from_value::<SessionMetadataV1>(
        document
            .records
            .iter()
            .rev()
            .find(|r| r.event_type == SessionRecordType::SessionMeta)
            .ok_or("session metadata missing")?
            .payload
            .clone(),
    )
    .map_err(|e| e.to_string())?;
    metadata.validate()?;
    if path.file_stem().and_then(|v| v.to_str()) != Some(document.manifest.session_id.as_str()) {
        return Err("Session manifest identity mismatch".into());
    }
    if document
        .records
        .first()
        .is_none_or(|r| r.event_type != SessionRecordType::SessionMeta)
    {
        return Err("first Session record must be session_meta".into());
    }
    let active = active_session_records(&document.manifest.session_id, &document.records)?;
    let mut state = State {
        identity,
        manifest: document.manifest,
        metadata,
        projection: SessionProjection::default(),
        seen: HashSet::new(),
        open: HashMap::new(),
        known: HashMap::new(),
        runs: HashMap::new(),
        catalog_sequence: None,
        changed_runs: HashSet::new(),
        run_states: HashMap::new(),
        stream_indices: HashMap::new(),
        stream_counts: HashMap::new(),
        terminal_ids: HashMap::new(),
        message_times: HashMap::new(),
        message_order: std::collections::BTreeSet::new(),
        assistant_ids: HashMap::new(),
        sequence: document.records.len(),
        updated: 0,
        bytes: 0,
    };
    for record in &document.records {
        state.restore_run(record)?;
        state.known.insert(record.event_id.clone(), digest(record)?);
        state.updated = state.updated.max(record.created_at_ms);
        state.bytes = state.bytes.saturating_add(
            serde_json::to_vec(record)
                .map_err(|e| e.to_string())?
                .len()
                .saturating_mul(3)
                + 512,
        );
    }
    for record in &active {
        state.index_stream(record);
        reduce_event(
            &state.manifest.session_id,
            &mut state.projection,
            &mut state.seen,
            &mut state.open,
            record,
        )?;
    }
    state.message_order = state
        .projection
        .messages
        .values()
        .map(|m| (m.updated_at_ms, m.message_id.clone()))
        .collect();
    state.runs = project_agent_runs_from_records(&active)?
        .into_iter()
        .map(|r| (r.agent_run_id.clone(), r))
        .collect();
    Ok(state)
}

pub(super) fn put(path: &Path, state: State) {
    if state.bytes > MAX_BYTES {
        return;
    }
    if let Ok(mut cache) = CACHE.get_or_init(|| Mutex::new(vec![])).lock() {
        cache.retain(|(p, _)| p != path);
        while cache.len() >= MAX_SESSIONS
            || cache
                .iter()
                .map(|(_, s)| s.bytes)
                .sum::<usize>()
                .saturating_add(state.bytes)
                > MAX_BYTES
        {
            if cache.is_empty() {
                break;
            }
            cache.remove(0);
        }
        cache.push((path.to_path_buf(), state));
    }
}

impl State {
    fn index_stream(&mut self, record: &SessionLogRecord) {
        if matches!(
            record.event_type,
            SessionRecordType::UserMessage
                | SessionRecordType::AssistantMessage
                | SessionRecordType::TurnSupplement
        ) {
            if let Some(id) = record.payload.get("messageId").and_then(Value::as_str) {
                if let Some(old) = self.message_times.insert(id.into(), record.created_at_ms) {
                    self.message_order.remove(&(old, id.into()));
                }
                self.message_order.insert((record.created_at_ms, id.into()));
                if record.event_type == SessionRecordType::AssistantMessage {
                    if let Some(run) = &record.agent_run_id {
                        self.assistant_ids.insert(run.clone(), id.into());
                    }
                }
            }
        }
        if matches!(
            record.event_type,
            SessionRecordType::AgentRunCompleted
                | SessionRecordType::AgentRunFailed
                | SessionRecordType::AgentRunInterrupted
        ) {
            if let Some(id) = &record.agent_run_id {
                self.terminal_ids
                    .insert(id.clone(), record.event_id.clone());
            }
        }
        if session_record_projects_to_agent_run_stream(record.event_type) {
            if let Some(id) = &record.agent_run_id {
                let count = self.stream_counts.entry(id.clone()).or_default();
                self.stream_indices
                    .insert(record.event_id.clone(), (id.clone(), *count));
                *count += 1;
            }
        }
    }

    fn restore_run(&mut self, record: &SessionLogRecord) -> Result<(), String> {
        if let Some(id) = &record.agent_run_id {
            if !self.run_states.contains_key(id) {
                self.run_states.insert(
                    id.clone(),
                    centaeris_core::session::AgentRunSessionState::new(
                        &self.manifest.session_id,
                        id,
                    )?,
                );
            }
            let state = self.run_states.get_mut(id).unwrap();
            state.restore(SequencedSessionRecord {
                sequence: state.next_sequence() + 1,
                event: record.clone(),
            })?;
        }
        Ok(())
    }

    pub fn prepare(
        &mut self,
        records: Vec<SessionLogRecord>,
    ) -> Result<Vec<SessionLogRecord>, String> {
        let mut batch = HashSet::new();
        let mut pending = vec![];
        for record in records {
            validate_event_shape(&record)?;
            if record.session_id != self.manifest.session_id {
                return Err("session record belongs to another sessionId".into());
            }
            if let Some(known) = self.known.get(&record.event_id) {
                if known == &digest(&record)? {
                    continue;
                }
                return Err(format!(
                    "session record eventId conflict: {}",
                    record.event_id
                ));
            }
            if !batch.insert(record.event_id.clone()) {
                return Err(format!(
                    "session record batch duplicate eventId: {}",
                    record.event_id
                ));
            }
            // Rewrites use the full Core tombstone reducer via the explicit slow path.
            if record.event_type == SessionRecordType::Tombstone {
                return Err("tombstone requires history reconstruction".into());
            }
            reduce_event(
                &self.manifest.session_id,
                &mut self.projection,
                &mut self.seen,
                &mut self.open,
                &record,
            )?;
            if record.event_type == SessionRecordType::SessionMeta {
                self.metadata =
                    serde_json::from_value(record.payload.clone()).map_err(|e| e.to_string())?;
                for run in self.runs.values_mut() {
                    self.changed_runs.insert(run.agent_run_id.clone());
                    run.cwd = Some(self.metadata.cwd.clone());
                }
            }
            self.index_stream(&record);
            self.restore_run(&record)?;
            apply_run_record(&mut self.runs, &self.metadata.cwd, &record)?;
            if let Some(id) = &record.agent_run_id {
                self.changed_runs.insert(id.clone());
            }
            self.known.insert(record.event_id.clone(), digest(&record)?);
            self.updated = self.updated.max(record.created_at_ms);
            self.bytes = self.bytes.saturating_add(
                serde_json::to_vec(&record)
                    .map_err(|e| e.to_string())?
                    .len()
                    .saturating_mul(3)
                    + 512,
            );
            pending.push(record);
        }
        Ok(pending)
    }
    pub fn committed(&mut self, path: &Path, count: usize) -> Result<(), String> {
        self.sequence += count;
        self.identity = session_log_identity(path)?;
        Ok(())
    }
}

pub(crate) fn catalog_projection(
    path: &Path,
    root: &Path,
    indexed_sequence: Option<u64>,
) -> Result<
    (
        crate::session_files::SessionFileItem,
        Vec<ProjectedAgentRun>,
        u64,
        bool,
    ),
    String,
> {
    let mut state = take(path)?;
    let item = crate::session_files::SessionFiles::new(root.to_path_buf()).project_summary(
        path,
        &state.manifest,
        &state.metadata,
        (
            state.projection.messages.len(),
            state
                .message_order
                .last()
                .and_then(|(_, id)| state.projection.messages.get(id)),
        ),
        state.updated,
    )?;
    let full = state.catalog_sequence.is_none() || state.catalog_sequence != indexed_sequence;
    let runs = if full {
        state.runs.values().cloned().collect()
    } else {
        state
            .changed_runs
            .iter()
            .filter_map(|id| state.runs.get(id).cloned())
            .collect()
    };
    let sequence = state.sequence as u64;
    state.catalog_sequence = Some(sequence);
    state.changed_runs.clear();
    put(path, state);
    Ok((item, runs, sequence, full))
}

pub(super) fn run_state(
    path: &Path,
    session_id: &str,
    run_id: &str,
) -> Result<centaeris_core::session::AgentRunSessionState, String> {
    let state = take(path)?;
    let run = state.run_states.get(run_id).cloned().map_or_else(
        || centaeris_core::session::AgentRunSessionState::new(session_id, run_id),
        Ok,
    )?;
    put(path, state);
    Ok(run)
}

pub(super) fn stream_indices(
    path: &Path,
    run_id: &str,
    ids: &[String],
) -> Result<Vec<(String, u64)>, String> {
    let state = take(path)?;
    let mut items = vec![];
    for id in ids {
        let (owner, index) = state
            .stream_indices
            .get(id)
            .ok_or("committed stream event missing")?;
        if owner != run_id {
            return Err("stream event belongs to another AgentRun".into());
        }
        items.push((id.clone(), *index));
    }
    items.sort_by_key(|(_, index)| *index);
    items.dedup();
    put(path, state);
    Ok(items)
}

#[cfg(test)]
pub(crate) fn evict_all() {
    if let Some(cache) = CACHE.get() {
        cache.lock().unwrap().clear();
    }
}

pub(super) fn invalidate(path: &Path) {
    if let Some(cache) = CACHE.get() {
        if let Ok(mut cache) = cache.lock() {
            cache.retain(|(p, _)| p != path);
        }
    }
}

pub(super) fn message(
    path: &Path,
    id: Option<&str>,
    run: Option<&str>,
) -> Result<Option<ProjectedChatMessage>, String> {
    let state = take(path)?;
    let message = if let Some(id) = id {
        state.projection.messages.get(id)
    } else {
        run.and_then(|run| state.assistant_ids.get(run))
            .and_then(|id| state.projection.messages.get(id))
    };
    let result = message
        .map(|m| {
            Ok::<_, String>(ProjectedChatMessage {
                id: m.message_id.clone(),
                session_id: state.manifest.session_id.clone(),
                turn_id: m
                    .turn_id
                    .clone()
                    .ok_or("projected message turnId is required")?,
                role: match m.role {
                    ReducedMessageRole::User => "user",
                    ReducedMessageRole::Assistant => "assistant",
                }
                .into(),
                content: m.text.clone(),
                status: m.status.clone(),
                created_at_ms: *state
                    .message_times
                    .get(&m.message_id)
                    .ok_or("message creation time missing")?,
                updated_at_ms: m.updated_at_ms,
                agent_run_id: m.agent_run_id.clone(),
                image_data: None,
            })
        })
        .transpose()?;
    put(path, state);
    Ok(result)
}
pub(super) fn terminal_id(path: &Path, run: &str) -> Result<String, String> {
    let state = take(path)?;
    let id = state
        .terminal_ids
        .get(run)
        .cloned()
        .ok_or("AgentRun has no committed terminal record")?;
    put(path, state);
    Ok(id)
}
pub(super) fn context(path: &Path) -> Result<ProjectedAgentContextState, String> {
    let state = take(path)?;
    let projection = &state.projection;
    let result = ProjectedAgentContextState {
        provider_usage: projection.provider_usage(),
        context_token_estimate: projection.context_token_estimate(),
        context_token_breakdown: projection.context_token_breakdown().cloned(),
        context_token_estimate_updated_at_ms: projection.context_token_estimate_updated_at_ms(),
        latest_provider_usage_context_token_estimate: projection
            .latest_provider_usage_context_token_estimate(),
        is_compacting: projection.is_compacting(),
    };
    put(path, state);
    Ok(result)
}
