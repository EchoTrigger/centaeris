use base64::{engine::general_purpose::STANDARD, Engine};
use centaeris_runtime::local_execution_host::LocalExecutionHostRunner;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    io::Read,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

const MAX_ACTIVE: usize = 32;
const MAX_RECORDS: usize = 64;
const OUTPUT_CAP: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 4096;
const MAX_OUTPUT_CHUNKS: usize = 4096;
const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_WAIT_MS: u64 = 30_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StartRequest {
    pub session_id: String,
    pub service_instance_id: String,
    #[serde(deserialize_with = "crate::operation_receipts::deserialize_operation_id")]
    pub operation_id: String,
    pub program: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_ms: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Target {
    pub session_id: String,
    pub process_session_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ListRequest {
    pub session_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReadRequest {
    pub session_id: String,
    pub process_session_id: String,
    pub cursor: String,
    pub wait_ms: u64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub process_session_id: String,
    pub session_id: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub state: &'static str,
    pub source: &'static str,
    pub cleanup_complete: bool,
    pub exit_code: Option<i32>,
    pub termination_reason: Option<&'static str>,
    pub output_complete: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Chunk {
    pub cursor: String,
    pub stream: &'static str,
    pub data_base64: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Output {
    pub process: Snapshot,
    pub chunks: Vec<Chunk>,
    pub next_cursor: String,
    pub earliest_cursor: String,
    pub gap: bool,
    pub has_more: bool,
}
struct Log {
    chunks: VecDeque<(u64, &'static str, Vec<u8>)>,
    next: u64,
    bytes: usize,
}
impl Log {
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            next: 0,
            bytes: 0,
        }
    }
    fn append(&mut self, stream: &'static str, bytes: &[u8]) {
        for part in bytes.chunks(CHUNK_BYTES) {
            self.chunks.push_back((self.next, stream, part.to_vec()));
            self.next += 1;
            self.bytes += part.len();
            while self.bytes > OUTPUT_CAP || self.chunks.len() > MAX_OUTPUT_CHUNKS {
                self.bytes -= self.chunks.pop_front().expect("bounded log").2.len();
            }
        }
    }
    fn earliest(&self) -> u64 {
        self.chunks.front().map_or(self.next, |c| c.0)
    }
}
struct EntryState {
    snapshot: Snapshot,
    log: Log,
    readers: usize,
    stop: bool,
    notification_pending: bool,
}
struct Entry {
    state: Mutex<EntryState>,
    changed: Condvar,
}
impl Entry {
    fn active(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.snapshot.state != "exited" || !s.snapshot.output_complete || !s.snapshot.cleanup_complete
    }
    fn snapshot(&self) -> Snapshot {
        self.state.lock().unwrap().snapshot.clone()
    }
    fn request_stop(&self) {
        let mut s = self.state.lock().unwrap();
        if s.snapshot.state != "exited" {
            s.stop = true;
            s.snapshot.state = "stopping";
        }
        self.changed.notify_all();
    }
}
#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Entry>>,
    order: VecDeque<String>,
    receipts: crate::local_work_store::Receipts,
    closing: bool,
}
pub(crate) struct Manager {
    service_instance_id: String,
    registry: Mutex<Registry>,
    pub deletion_gate: Mutex<()>,
}
impl Default for Manager {
    fn default() -> Self {
        Self {
            service_instance_id: identity().expect("OS randomness for process service identity"),
            registry: Mutex::new(Registry::default()),
            deletion_gate: Mutex::new(()),
        }
    }
}
static MANAGER: OnceLock<Manager> = OnceLock::new();
pub(crate) fn manager() -> &'static Manager {
    MANAGER.get_or_init(Manager::default)
}
fn identity() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(format!(
        "process-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
impl Manager {
    pub fn start(&self, request: StartRequest) -> Result<Snapshot, String> {
        self.start_with_source(request, false)
    }
    pub(crate) fn start_for_agent(&self, request: StartRequest) -> Result<Snapshot, String> {
        self.start_with_source(request, true)
    }
    pub(crate) fn service_instance_id(&self) -> &str {
        &self.service_instance_id
    }
    fn start_with_source(&self, request: StartRequest, agent: bool) -> Result<Snapshot, String> {
        if request.service_instance_id != self.service_instance_id {
            return Err("process_service_instance_changed".into());
        }
        let _gate = self.deletion_gate.lock().unwrap();
        // Resolve inside the same gate as deletion; no caller-selected workspace.
        let cwd = PathBuf::from(crate::sessions::cwd_for_session_id(&request.session_id)?);
        let id = self.start_at_mode(request, cwd, agent)?;
        self.registry
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .map(|e| e.snapshot())
            .ok_or_else(|| "process_session_expired".into())
    }
    #[cfg(test)]
    fn start_at(&self, request: StartRequest, cwd: PathBuf) -> Result<String, String> {
        self.start_at_mode(request, cwd, false)
    }
    #[cfg(test)]
    pub(crate) fn start_for_agent_at(
        &self,
        request: StartRequest,
        cwd: PathBuf,
    ) -> Result<String, String> {
        self.start_at_mode(request, cwd, true)
    }
    fn start_at_mode(
        &self,
        request: StartRequest,
        cwd: PathBuf,
        agent: bool,
    ) -> Result<String, String> {
        if request.service_instance_id != self.service_instance_id {
            return Err("process_service_instance_changed".into());
        }
        if request.session_id.trim().is_empty()
            || request.operation_id.trim().is_empty()
            || request.operation_id.len() > 128
            || request.program.trim().is_empty()
            || request.program.len() > 4096
            || request.args.len() > 1024
            || request.args.iter().map(String::len).sum::<usize>() > 64 * 1024
        {
            return Err("invalid_process_request".into());
        }
        let mut r = self.registry.lock().unwrap();
        if r.closing {
            return Err("process_service_stopping".into());
        }
        let key = crate::operation_receipts::deterministic_identity(
            "",
            &request.session_id,
            &request.operation_id,
        );
        let namespace = format!("process:{}", self.service_instance_id);
        let digest = crate::operation_receipts::request_digest(&(&request, agent))?;
        if let Some(result) = r.receipts.get(&namespace, &key, &digest).map_err(|e| {
            if e == "operation request conflict" {
                "process_operation_conflict".into()
            } else {
                e
            }
        })? {
            return result;
        }
        if r.entries.values().filter(|e| e.active()).count() >= MAX_ACTIVE {
            return Err("process_capacity_reached".into());
        }
        while r.entries.len() >= MAX_RECORDS {
            let Some(index) = r.order.iter().position(|id| {
                !r.entries[id].active() && !r.entries[id].state.lock().unwrap().notification_pending
            }) else {
                return Err("process_capacity_reached".into());
            };
            let id = r.order.remove(index).unwrap();
            r.entries.remove(&id);
        }
        r.receipts.put(
            &namespace,
            &key,
            &digest,
            &Err("process_start_outcome_unknown".into()),
        )?;
        let result = (|| {
            let id = identity()?;
            let runner =
                LocalExecutionHostRunner::new(None).map_err(|e| e.internal_debug_message())?;
            let policy = crate::local_execution_policy::for_workspace(&cwd, &[]);
            let mut process = runner
                .spawn_piped_process(
                    request.program.clone(),
                    request.args.clone(),
                    cwd.clone(),
                    policy,
                )
                .map_err(|e| e.internal_debug_message())?;
            let stdout = process.take_stdout().ok_or("stdout pipe unavailable")?;
            let stderr = process.take_stderr().ok_or("stderr pipe unavailable")?;
            let entry = Arc::new(Entry {
                state: Mutex::new(EntryState {
                    snapshot: Snapshot {
                        process_session_id: id.clone(),
                        session_id: request.session_id.clone(),
                        program: request.program.clone(),
                        args: request.args.clone(),
                        cwd: cwd.to_string_lossy().into(),
                        state: "running",
                        source: if agent { "agentTool" } else { "hostCommand" },
                        cleanup_complete: false,
                        exit_code: None,
                        termination_reason: None,
                        output_complete: false,
                        error: None,
                    },
                    log: Log::new(),
                    readers: 2,
                    stop: false,
                    notification_pending: agent,
                }),
                changed: Condvar::new(),
            });
            // Reader creation failures drop the owned process and kill its tree.
            spawn_reader(entry.clone(), "stdout", stdout)?;
            spawn_reader(entry.clone(), "stderr", stderr)?;
            let owned = entry.clone();
            let timeout_ms = request.timeout_ms;
            thread::Builder::new()
                .name("process-session-owner".into())
                .spawn(move || {
                    let started = Instant::now();
                    loop {
                        let mut s = owned.state.lock().unwrap();
                        match process.try_wait() {
                            Ok(Some(status)) => {
                                // Descendants belong to this process session, not to the OS shell.
                                match process.kill() {
                                    Ok(()) => s.snapshot.cleanup_complete = true,
                                    Err(error) => {
                                        s.snapshot.error =
                                            Some(format!("descendant cleanup failed: {error}"))
                                    }
                                }
                                s.snapshot.exit_code = status.code();
                                s.snapshot.state = "exited";
                                s.snapshot.output_complete = s.readers == 0;
                                owned.changed.notify_all();
                                break;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                s.snapshot.error =
                                    Some(format!("exit observation failed: {error}"));
                            }
                        }
                        let timed_out = timeout_ms > 0
                            && started.elapsed() >= Duration::from_millis(timeout_ms);
                        if s.stop || timed_out {
                            s.snapshot.state = "stopping";
                            let reason = if s.stop { "stopped" } else { "timedOut" };
                            match process.kill() {
                                Ok(()) => {
                                    s.snapshot.termination_reason = Some(reason);
                                }
                                Err(error) => {
                                    s.snapshot.error =
                                        Some(format!("process tree stop not confirmed: {error}"));
                                }
                            }
                        }
                        drop(s);
                        thread::sleep(Duration::from_millis(25));
                    }
                })
                .map_err(|e| e.to_string())?;
            r.order.push_back(id.clone());
            r.entries.insert(id.clone(), entry);
            Ok(id)
        })();
        r.receipts.put(&namespace, &key, &digest, &result)?;
        result
    }
    fn entry(&self, target: &Target) -> Result<Arc<Entry>, String> {
        let r = self.registry.lock().unwrap();
        let e = r
            .entries
            .get(&target.process_session_id)
            .filter(|e| e.snapshot().session_id == target.session_id)
            .ok_or("process_session_not_found_or_expired")?;
        Ok(e.clone())
    }
    pub fn get(&self, target: Target) -> Result<Snapshot, String> {
        Ok(self.entry(&target)?.snapshot())
    }
    pub fn list(&self, session: &str) -> Vec<Snapshot> {
        let r = self.registry.lock().unwrap();
        r.order
            .iter()
            .filter_map(|id| r.entries.get(id))
            .map(|e| e.snapshot())
            .filter(|s| s.session_id == session)
            .collect()
    }
    pub(crate) fn started_operation(
        &self,
        session: &str,
        operation: &str,
    ) -> Option<Result<String, String>> {
        let key = crate::operation_receipts::deterministic_identity("", session, operation);
        match self
            .registry
            .lock()
            .unwrap()
            .receipts
            .result(&format!("process:{}", self.service_instance_id), &key)
        {
            Ok(result) => result,
            Err(error) => Some(Err(error)),
        }
    }
    pub(crate) fn acknowledge_notification(&self, target: Target) {
        if let Ok(entry) = self.entry(&target) {
            entry.state.lock().unwrap().notification_pending = false;
        }
    }
    pub(crate) fn has_pending_notifications(&self) -> bool {
        self.registry
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| e.state.lock().unwrap().notification_pending)
    }
    pub fn stop(&self, target: Target) -> Result<Snapshot, String> {
        let e = self.entry(&target)?;
        e.request_stop();
        Ok(e.snapshot())
    }
    pub fn read(&self, request: ReadRequest) -> Result<Output, String> {
        if request.wait_ms > MAX_WAIT_MS {
            return Err("invalid_process_wait".into());
        }
        let cursor = request
            .cursor
            .parse::<u64>()
            .map_err(|_| "invalid_process_cursor")?;
        if cursor.to_string() != request.cursor {
            return Err("invalid_process_cursor".into());
        }
        let e = self.entry(&Target {
            session_id: request.session_id,
            process_session_id: request.process_session_id,
        })?;
        let deadline = Instant::now() + Duration::from_millis(request.wait_ms);
        let mut s = e.state.lock().unwrap();
        if cursor > s.log.next {
            return Err("process_cursor_ahead".into());
        }
        while cursor == s.log.next && !s.snapshot.output_complete && s.snapshot.error.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            s = e.changed.wait_timeout(s, remaining).unwrap().0;
        }
        let earliest = s.log.earliest();
        let mut next = cursor.max(earliest);
        let mut bytes = 0;
        let mut chunks = Vec::new();
        for (seq, stream, data) in &s.log.chunks {
            if *seq < next {
                continue;
            }
            if bytes + data.len() > MAX_READ_BYTES {
                break;
            }
            chunks.push(Chunk {
                cursor: seq.to_string(),
                stream,
                data_base64: STANDARD.encode(data),
            });
            bytes += data.len();
            next = seq + 1;
        }
        Ok(Output {
            process: s.snapshot.clone(),
            chunks,
            next_cursor: next.to_string(),
            earliest_cursor: earliest.to_string(),
            gap: cursor < earliest,
            has_more: next < s.log.next,
        })
    }
    pub fn has_active(&self) -> bool {
        self.registry
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| e.active())
    }
    pub fn ensure_deletable(&self, sessions: &[String]) -> Result<(), String> {
        if self
            .registry
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| sessions.contains(&e.snapshot().session_id) && e.active())
        {
            Err("session_has_active_processes".into())
        } else {
            Ok(())
        }
    }
    pub fn forget_closed(&self, sessions: &[String]) {
        let mut r = self.registry.lock().unwrap();
        let removed: Vec<_> = r
            .entries
            .iter()
            .filter(|(_, e)| sessions.contains(&e.snapshot().session_id) && !e.active())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &removed {
            r.entries.remove(id);
        }
        r.order.retain(|id| !removed.contains(id));
        // Keep receipt tombstones: eviction must never make retries spawn again.
    }
    pub fn begin_shutdown(&self) {
        let mut r = self.registry.lock().unwrap();
        r.closing = true;
        for e in r.entries.values() {
            e.request_stop();
        }
    }
    pub fn shutdown(&self) -> Result<(), String> {
        self.begin_shutdown();
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.has_active() {
            if Instant::now() >= deadline {
                return Err("process_shutdown_not_confirmed".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    }
}
fn spawn_reader(
    entry: Arc<Entry>,
    stream: &'static str,
    mut reader: impl Read + Send + 'static,
) -> Result<(), String> {
    thread::Builder::new()
        .name(format!("process-{stream}"))
        .spawn(move || {
            let mut buffer = [0; CHUNK_BYTES];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut s = entry.state.lock().unwrap();
                        s.log.append(stream, &buffer[..n]);
                        entry.changed.notify_all();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        entry.state.lock().unwrap().snapshot.error =
                            Some(format!("{stream} read failed: {e}"));
                        break;
                    }
                }
            }
            let mut s = entry.state.lock().unwrap();
            s.readers -= 1;
            s.snapshot.output_complete = s.readers == 0 && s.snapshot.state == "exited";
            entry.changed.notify_all();
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub(crate) fn handle(
    command: crate::commands::RuntimeHostCommand,
    payload: serde_json::Value,
) -> Result<serde_json::Value, crate::errors::RuntimeHostError> {
    use crate::{commands::RuntimeHostCommand as C, errors::RuntimeHostError as E};
    let m = manager();
    let result: Result<serde_json::Value, String> = match command {
        C::ProcessSessionStart => m
            .start(crate::runtime_bridge::deserialize_request(payload)?)
            .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
        C::ProcessSessionGet => m
            .get(crate::runtime_bridge::deserialize_request(payload)?)
            .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
        C::ProcessSessionStop => m
            .stop(crate::runtime_bridge::deserialize_request(payload)?)
            .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
        C::ProcessSessionRead => m
            .read(crate::runtime_bridge::deserialize_request(payload)?)
            .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
        C::ProcessSessionList => {
            let r: ListRequest = crate::runtime_bridge::deserialize_request(payload)?;
            if r.session_id.trim().is_empty() {
                Err("sessionId is required".into())
            } else {
                Ok(
                    serde_json::json!({"serviceInstanceId": m.service_instance_id, "processes":m.list(&r.session_id)}),
                )
            }
        }
        _ => return Err(E::invalid_request("not a process session method")),
    };
    result.map_err(|message| E::new("process_session_failed", message))
}

impl Drop for Manager {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests;
