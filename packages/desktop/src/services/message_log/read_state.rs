//! Incremental reducer state. Active runs own retention; idle states are disposable.
use super::*;
use centaeris_core::session::{reduce_event, SessionProjection};
use std::sync::Mutex;

const MAX_IDLE_SESSIONS: usize = 8;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

#[derive(Default)]
struct Cache {
    states: Vec<(PathBuf, State)>,
    active: HashMap<PathBuf, usize>,
}

impl Cache {
    fn trim_idle(&mut self) {
        while self
            .states
            .iter()
            .filter(|(path, _)| !self.active.contains_key(path))
            .count()
            > MAX_IDLE_SESSIONS
        {
            let index = self
                .states
                .iter()
                .position(|(path, _)| !self.active.contains_key(path))
                .expect("idle state count implies an idle entry");
            self.states.remove(index);
        }
    }
}

pub(crate) struct ActiveReadState {
    path: PathBuf,
}

pub(super) fn retain_active(path: &Path) -> Result<ActiveReadState, String> {
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .map_err(|_| "read-state lock poisoned")?;
    *cache.active.entry(path.to_path_buf()).or_default() += 1;
    Ok(ActiveReadState {
        path: path.to_path_buf(),
    })
}

impl Drop for ActiveReadState {
    fn drop(&mut self) {
        let Some(cache) = CACHE.get() else {
            return;
        };
        if let Ok(mut cache) = cache.lock() {
            if let Some(count) = cache.active.get_mut(&self.path) {
                *count -= 1;
                if *count == 0 {
                    cache.active.remove(&self.path);
                    cache.trim_idle();
                }
            }
        }
    }
}

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
    runtime_checkpoint: Option<RuntimeCheckpoint>,
    validation_generation: String,
}

struct RuntimeCheckpoint {
    snapshot_digest: [u8; 32],
    admission_records: Vec<SessionLogRecord>,
}

fn runtime_snapshot_digest(
    snapshot: &centaeris_core::session::state::SessionStateSnapshot,
) -> Result<[u8; 32], String> {
    use centaeris_core::session::state::{ChatMessage, MessageRole, SessionStateSnapshot};
    use std::collections::BTreeMap;

    #[derive(serde::Serialize)]
    struct Message<'a> {
        message_id: &'a str,
        role: &'a MessageRole,
        content: &'a str,
        created_at_ms: i64,
        metadata: BTreeMap<&'a str, &'a str>,
    }
    fn messages(source: &[ChatMessage]) -> Vec<Message<'_>> {
        source
            .iter()
            .map(|message| {
                let ChatMessage {
                    message_id,
                    role,
                    content,
                    created_at_ms,
                    metadata,
                } = message;
                Message {
                    message_id,
                    role,
                    content,
                    created_at_ms: *created_at_ms,
                    metadata: metadata
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.as_str()))
                        .collect(),
                }
            })
            .collect()
    }
    struct DigestWriter(Sha256);
    impl std::io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // Exhaustive destructuring keeps new state fields part of this independent
    // store/source binding. Sort borrowed map entries without copying W bodies.
    let SessionStateSnapshot {
        history,
        session_id,
        messages: source,
        context_window,
        model_semantics,
        completed_turn,
        updated_at_ms,
        metadata,
    } = snapshot;
    let mut writer = DigestWriter(Sha256::new());
    writer
        .0
        .update(b"centaeris.desktop.runtime_checkpoint.v1\0");
    serde_json::to_writer(
        &mut writer,
        &(
            history,
            session_id,
            messages(source),
            messages(context_window),
            model_semantics,
            completed_turn,
            updated_at_ms,
            metadata
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect::<BTreeMap<_, _>>(),
        ),
    )
    .map_err(|error| error.to_string())?;
    Ok(writer.0.finalize().into())
}

fn digest(record: &SessionLogRecord) -> Result<[u8; 32], String> {
    Ok(Sha256::digest(serde_json::to_vec(record).map_err(|e| e.to_string())?).into())
}

pub(super) fn take(path: &Path) -> Result<State, String> {
    let identity = session_log_identity(path)?;
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .map_err(|_| "read-state lock poisoned")?;
    if let Some(index) = cache.states.iter().position(|(p, _)| p == path) {
        let (_, state) = cache.states.remove(index);
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
        runtime_checkpoint: None,
        validation_generation: centaeris_core::runtime::contracts::new_turn_id(),
    };
    for record in &document.records {
        state.restore_run(record)?;
        state.known.insert(record.event_id.clone(), digest(record)?);
        state.updated = state.updated.max(record.created_at_ms);
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
    if let Ok(mut cache) = CACHE.get_or_init(|| Mutex::new(Cache::default())).lock() {
        cache.states.retain(|(p, _)| p != path);
        cache.states.push((path.to_path_buf(), state));
        cache.trim_idle();
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
            if record.event_type == SessionRecordType::HostEventInput {
                // This source can arrive outside the running loop's durable callbacks.
                self.validation_generation = centaeris_core::runtime::contracts::new_turn_id();
            }
            if let Some(checkpoint) = &mut self.runtime_checkpoint {
                match record.event_type {
                    SessionRecordType::UserMessage
                    | SessionRecordType::ToolCallClosure
                    | SessionRecordType::AgentRunStarted
                    | SessionRecordType::AgentRunExecutionStarted => {
                        checkpoint.admission_records.push(record.clone());
                    }
                    SessionRecordType::SessionMeta
                    | SessionRecordType::AgentRunExecutionEnded
                    | SessionRecordType::AgentRunCompleted
                    | SessionRecordType::AgentRunFailed
                    | SessionRecordType::AgentRunInterrupted => {}
                    _ => self.runtime_checkpoint = None,
                }
            }
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

pub(super) fn bind_runtime_snapshot(
    path: &Path,
    snapshot: &centaeris_core::session::state::SessionStateSnapshot,
    expected_sequence: Option<usize>,
    expected_generation: &str,
) -> Result<bool, String> {
    let mut state = take(path)?;
    let result = if state.manifest.session_id != snapshot.session_id {
        Err("runtime checkpoint Session identity mismatch".into())
    } else if state.validation_generation != expected_generation {
        state.runtime_checkpoint = None;
        Ok(false)
    } else if expected_sequence.is_some_and(|sequence| sequence != state.sequence) {
        state.runtime_checkpoint = None;
        Err("Session changed before runtime checkpoint binding".into())
    } else {
        state.runtime_checkpoint = Some(RuntimeCheckpoint {
            snapshot_digest: runtime_snapshot_digest(snapshot)?,
            admission_records: Vec::new(),
        });
        Ok(true)
    };
    put(path, state);
    result
}

pub(super) fn advance_runtime_snapshot(
    path: &Path,
    snapshot: centaeris_core::session::state::SessionStateSnapshot,
) -> Result<
    Option<(
        centaeris_core::session::state::SessionStateSnapshot,
        usize,
        bool,
        String,
    )>,
    String,
> {
    let state = take(path)?;
    let result = if let Some(checkpoint) = &state.runtime_checkpoint {
        if state.manifest.session_id == snapshot.session_id
            && checkpoint.snapshot_digest == runtime_snapshot_digest(&snapshot)?
        {
            let changed = !checkpoint.admission_records.is_empty();
            let advanced = if changed {
                centaeris_core::session::recovery::apply_new_user_turn_admission(
                    snapshot,
                    &checkpoint.admission_records,
                )?
            } else {
                snapshot
            };
            Some((
                advanced,
                state.sequence,
                changed,
                state.validation_generation.clone(),
            ))
        } else {
            None
        }
    } else {
        None
    };
    put(path, state);
    Ok(result)
}

pub(super) fn agent_runs(path: &Path) -> Result<Vec<ProjectedAgentRun>, String> {
    let state = take(path)?;
    let runs = state.runs.values().cloned().collect();
    put(path, state);
    Ok(runs)
}

pub(super) fn source_generation(path: &Path) -> Result<String, String> {
    let state = take(path)?;
    let generation = state.validation_generation.clone();
    put(path, state);
    Ok(generation)
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
        cache.lock().unwrap().states.clear();
    }
}

pub(super) fn invalidate(path: &Path) {
    if let Some(cache) = CACHE.get() {
        if let Ok(mut cache) = cache.lock() {
            cache.states.retain(|(p, _)| p != path);
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
        is_compacting: projection.is_compacting(),
    };
    put(path, state);
    Ok(result)
}
