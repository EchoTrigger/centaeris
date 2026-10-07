//! Opt-in, bounded timing records; no bodies, credentials or error messages.
use std::cell::RefCell;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

thread_local! { static JOB: RefCell<Option<String>> = const { RefCell::new(None) }; }
static IN_FLIGHT: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
const LANES: [&str; 3] = ["ordinary", "control", "listener"];

pub struct Span {
    phase: &'static str,
    start: Instant,
    outcome: &'static str,
}
impl Span {
    pub fn new(phase: &'static str) -> Self {
        Self {
            phase,
            start: Instant::now(),
            outcome: "interrupted",
        }
    }
    pub fn finished(&mut self, ok: bool) {
        self.outcome = if ok { "ok" } else { "error" };
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        timing(self.phase, self.start, self.outcome);
    }
}

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("WORKSPACE_PERF_OBSERVATIONS").as_deref() == Ok("1"))
}

fn emit(mut event: serde_json::Value) {
    if !enabled() {
        return;
    }
    event["schema"] = "workspace.perf.v1".into();
    event["source"] = "runtime".into();
    event["atUnixMs"] = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64)
        .into();
    JOB.with(|job| {
        if let Some(job) = &*job.borrow() {
            event["jobId"] = job.clone().into();
        }
    });
    let _ = writeln!(std::io::stderr().lock(), "{event}");
}

pub struct Context(Option<String>);
impl Context {
    pub fn enter(body: &[u8]) -> Self {
        let identity = if enabled() {
            serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .and_then(|value| {
                    value
                        .get("jobId")
                        .and_then(|id| id.as_str())
                        .map(str::to_string)
                        .or_else(|| {
                            value
                                .pointer("/agentRunStart/agentRunId")
                                .and_then(|id| id.as_str())
                                .map(|id| format!("agent_run.lifecycle:{id}"))
                        })
                })
                .filter(|id| {
                    id.len() <= 160
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_:-.".contains(&b))
                })
        } else {
            None
        };
        Self(JOB.with(|job| job.replace(identity)))
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        JOB.with(|job| job.replace(self.0.take()));
    }
}

pub fn timed<T>(
    phase: &'static str,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if !enabled() {
        return operation();
    }
    let start = Instant::now();
    let result = operation();
    timing(phase, start, if result.is_ok() { "ok" } else { "error" });
    result
}

pub fn timing(phase: &'static str, start: Instant, outcome: &'static str) {
    if !enabled() {
        return;
    }
    emit(
        serde_json::json!({"phase":phase,"elapsedMs":start.elapsed().as_secs_f64()*1000.0,"outcome":outcome}),
    );
}

pub fn route(path: &str) -> &'static str {
    match path {
        "/agent-runs/step" => "/agent-runs/step",
        "/agent-runs/teardown" => "/agent-runs/teardown",
        "/agent-runs/cancel" => "/agent-runs/cancel",
        "/internal/jobs/claim" => "/internal/jobs/claim",
        "/internal/jobs/complete" => "/internal/jobs/complete",
        "/internal/jobs/start" => "/internal/jobs/start",
        "/internal/jobs/heartbeat" => "/internal/jobs/heartbeat",
        "/internal/jobs/wait" => "/internal/jobs/wait",
        "/internal/jobs/fail" => "/internal/jobs/fail",
        "/internal/jobs/yield" => "/internal/jobs/yield",
        "/internal/jobs/reconcile" => "/internal/jobs/reconcile",
        _ => "other",
    }
}

pub struct Request {
    route: &'static str,
    lane: usize,
    started: Instant,
}
impl Request {
    pub fn admitted(route: &'static str, lane: usize, started: Instant) -> Option<Self> {
        if !enabled() {
            return None;
        }
        let count = IN_FLIGHT[lane].fetch_add(1, Ordering::Relaxed) + 1;
        emit(
            serde_json::json!({"phase":"requestAdmission","route":route,"lane":LANES[lane],"inFlight":count,"outcome":"admitted","elapsedMs":started.elapsed().as_secs_f64()*1000.0}),
        );
        Some(Self {
            route,
            lane,
            started: Instant::now(),
        })
    }
    pub fn rejected(route: &'static str, lane: usize, started: Instant) {
        if !enabled() {
            return;
        }
        emit(
            serde_json::json!({"phase":"requestAdmission","route":route,"lane":LANES[lane],"inFlight":IN_FLIGHT[lane].load(Ordering::Relaxed),"outcome":"rejected","elapsedMs":started.elapsed().as_secs_f64()*1000.0}),
        );
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        let count = IN_FLIGHT[self.lane].fetch_sub(1, Ordering::Relaxed) - 1;
        emit(
            serde_json::json!({"phase":"requestReleased","route":self.route,"lane":LANES[self.lane],"inFlight":count,"elapsedMs":self.started.elapsed().as_secs_f64()*1000.0}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timings_preserve_results_and_routes_have_bounded_cardinality() {
        assert_eq!(timed("test", || Ok(7)), Ok(7));
        assert_eq!(
            timed::<()>("test", || Err("private error".into())),
            Err("private error".into())
        );
        assert_eq!(route("/arbitrary/private/path"), "other");
        assert_eq!(route("/agent-runs/step"), "/agent-runs/step");
    }
}
