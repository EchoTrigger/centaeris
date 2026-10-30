//! Explicit, run-scoped observation identity shared across async thread migration.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

type Sink = Arc<dyn Fn(Value) + Send + Sync>;
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;
type ToolStart = (Instant, Option<String>);

#[derive(Default)]
struct State {
    requests: HashMap<String, String>,
    calls: HashMap<String, String>,
    tools: HashMap<String, ToolStart>,
}

#[derive(Clone)]
pub struct RunTimings {
    job_id: String,
    run_id: String,
    sink: Option<Sink>,
    clock: Clock,
    state: Arc<Mutex<State>>,
}

pub struct StartupProfile {
    pub server_count: usize,
    pub credential_count: usize,
    pub mcp_connection_count: usize,
    pub runtime_prelude_ms: u128,
    pub sandbox_ensure_ms: u128,
    pub mcp_credential_resolve_ms: u128,
    pub mcp_discovery_ms: u128,
    pub workspace_restore_ms: u128,
    pub session_start_commit_ms: u128,
    pub startup_total_ms: u128,
}

pub(super) fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-.".contains(&b))
}

fn milliseconds(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

impl RunTimings {
    pub fn new(job_id: &str, run_id: &str) -> Self {
        Self {
            job_id: job_id.to_string(),
            run_id: run_id.to_string(),
            sink: (super::enabled() && safe_id(job_id) && safe_id(run_id))
                .then(|| Arc::new(super::emit_explicit) as Sink),
            clock: Arc::new(Instant::now),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        job_id: &str,
        run_id: &str,
        clock: Clock,
    ) -> (Self, Arc<Mutex<Vec<Value>>>) {
        let records = Arc::new(Mutex::new(Vec::new()));
        let captured = records.clone();
        (
            Self {
                job_id: job_id.to_string(),
                run_id: run_id.to_string(),
                sink: (safe_id(job_id) && safe_id(run_id)).then(|| {
                    Arc::new(move |record| {
                        captured.lock().unwrap().push(record);
                    }) as Sink
                }),
                clock,
                state: Arc::new(Mutex::new(State::default())),
            },
            records,
        )
    }

    fn emit(&self, mut record: Value) {
        let Some(sink) = &self.sink else {
            return;
        };
        record["jobId"] = self.job_id.clone().into();
        record["runId"] = self.run_id.clone().into();
        sink(record);
    }

    pub fn startup_profile(&self, profile: StartupProfile) {
        if self.sink.is_none() {
            return;
        }
        self.emit(json!({
            "phase": "startupProfile", "outcome": "ok",
            "elapsedMs": milliseconds(profile.startup_total_ms),
            "serverCount": profile.server_count, "credentialCount": profile.credential_count,
            "mcpConnectionCount": profile.mcp_connection_count,
            "runtimePreludeMs": milliseconds(profile.runtime_prelude_ms),
            "sandboxEnsureMs": milliseconds(profile.sandbox_ensure_ms),
            "mcpCredentialResolveMs": milliseconds(profile.mcp_credential_resolve_ms),
            "mcpDiscoveryMs": milliseconds(profile.mcp_discovery_ms),
            "workspaceRestoreMs": milliseconds(profile.workspace_restore_ms),
            "sessionStartCommitMs": milliseconds(profile.session_start_commit_ms),
            "startupTotalMs": milliseconds(profile.startup_total_ms),
        }));
    }

    pub fn request_started(&self, turn_id: &str, request_id: &str) {
        if self.sink.is_none() || !safe_id(turn_id) || !safe_id(request_id) {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            state
                .requests
                .insert(turn_id.to_string(), request_id.to_string());
        }
    }

    pub fn model_http(&self, turn_id: &str, attempt: u32, streaming: bool) -> ModelHttp {
        let request_id = self.sink.as_ref().and_then(|_| {
            self.state
                .lock()
                .ok()
                .and_then(|state| state.requests.get(turn_id).cloned())
        });
        ModelHttp {
            run: self.clone(),
            start: (self.clock)(),
            request_id,
            attempt,
            streaming,
            first_event: false,
            first_text: false,
            finished: false,
        }
    }

    pub fn tool_started(&self, call_id: &str) {
        if self.sink.is_none() || !safe_id(call_id) {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            let request_id = state.calls.remove(call_id);
            state
                .tools
                .entry(call_id.to_string())
                .or_insert_with(|| ((self.clock)(), request_id));
        }
    }

    fn finish_tool(&self, call_id: &str, outcome: &'static str, missing: bool) {
        if self.sink.is_none() || !safe_id(call_id) {
            return;
        }
        let start = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.tools.remove(call_id));
        let mut record = json!({"phase":"toolService", "callId":call_id, "outcome":outcome});
        match start {
            Some((start, request_id)) => {
                record["elapsedMs"] = ((self.clock)()
                    .saturating_duration_since(start)
                    .as_secs_f64()
                    * 1000.0)
                    .into();
                if let Some(request_id) = request_id {
                    record["requestId"] = request_id.into();
                }
            }
            None if missing => {
                record["outcome"] = "unpaired".into();
            }
            None => return,
        }
        self.emit(record);
    }

    /// Called only after the durable result transaction returns its commit receipt.
    pub fn tool_committed(&self, call_id: &str, tool_ok: bool) {
        self.finish_tool(call_id, if tool_ok { "ok" } else { "error" }, true);
    }

    pub fn tool_commit_failed(&self, call_id: &str) {
        self.finish_tool(call_id, "commitError", false);
    }

    pub fn unpaired_guard(&self) -> UnpairedOnDrop {
        UnpairedOnDrop(self.clone())
    }

    pub fn finish_unpaired(&self) {
        let calls = self
            .state
            .lock()
            .ok()
            .map(|state| state.tools.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for call_id in calls {
            self.finish_tool(&call_id, "unpaired", false);
        }
    }
}

pub struct UnpairedOnDrop(RunTimings);
impl Drop for UnpairedOnDrop {
    fn drop(&mut self) {
        self.0.finish_unpaired();
    }
}

pub struct ModelHttp {
    run: RunTimings,
    start: Instant,
    request_id: Option<String>,
    attempt: u32,
    streaming: bool,
    first_event: bool,
    first_text: bool,
    finished: bool,
}

impl ModelHttp {
    fn emit(&self, phase: &'static str, outcome: &'static str) {
        if self.request_id.is_none() || !(1..=1000).contains(&self.attempt) {
            return;
        }
        let mut record = json!({
            "phase":phase, "outcome":outcome, "attempt":self.attempt, "streaming":self.streaming,
            "elapsedMs":(self.run.clock)().saturating_duration_since(self.start).as_secs_f64()*1000.0,
        });
        if let Some(id) = &self.request_id {
            record["requestId"] = id.clone().into();
        }
        self.run.emit(record);
    }

    pub fn first_event(&mut self) {
        if !self.first_event {
            self.first_event = true;
            self.emit("modelFirstEvent", "ok");
        }
    }

    pub fn text_delta(&mut self, delta: &str) {
        if !delta.is_empty() && !self.first_text {
            self.first_text = true;
            self.emit("modelFirstText", "ok");
        }
    }

    pub fn complete<'a>(&mut self, call_ids: impl Iterator<Item = &'a str>) {
        if let Some(request_id) = &self.request_id {
            if let Ok(mut state) = self.run.state.lock() {
                for call_id in call_ids.filter(|id| safe_id(id)) {
                    state.calls.insert(call_id.to_string(), request_id.clone());
                }
            }
        }
        self.finish(true);
    }

    pub fn finish(&mut self, ok: bool) {
        if !self.finished {
            self.finished = true;
            self.emit("modelHttp", if ok { "ok" } else { "error" });
        }
    }
}

impl Drop for ModelHttp {
    fn drop(&mut self) {
        if !self.finished {
            self.emit("modelHttp", "interrupted");
        }
    }
}

#[cfg(test)]
#[path = "run_timings_tests.rs"]
mod tests;
