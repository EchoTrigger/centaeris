//! Opt-in, bounded timing records; no bodies, credentials or error messages.
pub(crate) mod power;

use std::cell::RefCell;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
#[derive(Default)]
struct ProtectionRecords {
    events: Vec<serde_json::Value>,
    missing: Vec<&'static str>,
}
#[cfg(test)]
type ProtectionLog = Arc<std::sync::Mutex<ProtectionRecords>>;
#[cfg(test)]
thread_local! { static PROTECTION_LOG: RefCell<Option<ProtectionLog>> = const { RefCell::new(None) }; }

#[derive(Clone, Copy)]
pub enum ProtectionMetric {
    SpoolWrittenBytes,
    RawCaptureSpoolWrittenBytes,
    SdkDownloadFrameBytes,
    WorkspaceCommitElapsedMs,
    SnapshotLogicalBytes,
    UploadAttempts,
    UploadedBytes,
    CaptureElapsedMs,
    UploadElapsedMs,
    RestoreDownloadElapsedMs,
    RestoreApplyElapsedMs,
}

#[derive(Clone, Copy)]
pub enum ProtectionTrigger {
    TerminalCommit,
    RecoveryCheckpoint,
    SessionRestore,
    RecoveryRestore,
}
impl ProtectionTrigger {
    fn name(self) -> &'static str {
        match self {
            Self::TerminalCommit => "terminalCommit",
            Self::RecoveryCheckpoint => "recoveryCheckpoint",
            Self::SessionRestore => "sessionRestore",
            Self::RecoveryRestore => "recoveryRestore",
        }
    }
}
impl ProtectionMetric {
    fn name(self) -> &'static str {
        match self {
            Self::SpoolWrittenBytes => "runtimeSpoolWrittenBytes",
            Self::RawCaptureSpoolWrittenBytes => "rawCaptureSpoolWrittenBytes",
            Self::SdkDownloadFrameBytes => "sdkDownloadFrameBytes",
            Self::WorkspaceCommitElapsedMs => "workspaceCommitElapsedMs",
            Self::SnapshotLogicalBytes => "snapshotLogicalBytes",
            Self::UploadAttempts => "uploadAttempts",
            Self::UploadedBytes => "uploadedBytes",
            Self::CaptureElapsedMs => "captureElapsedMs",
            Self::UploadElapsedMs => "uploadElapsedMs",
            Self::RestoreDownloadElapsedMs => "restoreDownloadElapsedMs",
            Self::RestoreApplyElapsedMs => "restoreApplyElapsedMs",
        }
    }
}

/// One local operation, independent of mutable file paths and request payloads.
#[derive(Clone)]
pub struct WorkspaceProtection {
    id: String,
    enabled: bool,
    trigger: Option<ProtectionTrigger>,
    run_id: Option<String>,
    job_id: Option<String>,
    #[cfg(test)]
    log: Option<ProtectionLog>,
}
impl WorkspaceProtection {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        #[cfg(test)]
        let log = PROTECTION_LOG.with(|log| log.borrow().clone());
        let enabled = enabled();
        #[cfg(test)]
        let enabled = enabled || log.is_some();
        Self {
            id: if enabled {
                format!(
                    "protection-{}-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )
            } else {
                String::new()
            },
            enabled,
            trigger: None,
            run_id: None,
            job_id: None,
            #[cfg(test)]
            log,
        }
    }

    pub fn operation_id(&self) -> &str {
        &self.id
    }

    pub fn with_context(
        mut self,
        trigger: ProtectionTrigger,
        run_id: &str,
        job_id: Option<&str>,
    ) -> Self {
        if self.enabled {
            let identity = |value: &str| {
                (!value.is_empty()
                    && value.len() <= 160
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte)))
                .then(|| value.to_string())
            };
            self.trigger = Some(trigger);
            self.run_id = identity(run_id);
            self.job_id = job_id.and_then(identity);
        }
        self
    }

    fn missing(&self, metric: &'static str) {
        #[cfg(test)]
        if self.enabled {
            if let Some(log) = &self.log {
                log.lock().unwrap().missing.push(metric);
            }
        }
        self.record("runtime", metric, serde_json::Value::Null, "unavailable");
    }

    pub fn unavailable(&self, metric: ProtectionMetric) {
        self.missing(metric.name());
    }

    /// The only accepted strings are fixed public commit dispositions.
    pub fn workspace_commit_disposition(&self, disposition: &str) {
        if [
            "full",
            "unchanged",
            "alreadyAdvanced",
            "pending",
            "rejected",
        ]
        .contains(&disposition)
        {
            self.record(
                "runtime",
                "workspaceCommitDisposition",
                disposition.into(),
                if disposition == "pending" {
                    "interrupted"
                } else if disposition == "rejected" {
                    "error"
                } else {
                    "ok"
                },
            );
        }
    }

    fn record(&self, source: &str, metric: &str, value: serde_json::Value, outcome: &str) {
        if !self.enabled {
            return;
        }
        let mut event = serde_json::json!({"schema":"workspace.perf.v1", "source":source,
            "phase":"workspaceProtection", "operationId":self.operation_id(), "metric":metric,
            "value":value, "outcome":outcome,
            "atUnixMs":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64});
        if let Some(trigger) = self.trigger {
            event["trigger"] = trigger.name().into();
        }
        if let Some(run_id) = &self.run_id {
            event["runId"] = run_id.as_str().into();
        }
        if let Some(job_id) = &self.job_id {
            event["jobId"] = job_id.as_str().into();
        }
        #[cfg(test)]
        if let Some(log) = &self.log {
            log.lock().unwrap().events.push(event);
            return;
        }
        let _ = writeln!(std::io::stderr().lock(), "{event}");
    }

    pub fn count(&self, metric: ProtectionMetric, value: u64, ok: bool) {
        self.record(
            "runtime",
            metric.name(),
            value.into(),
            if ok { "ok" } else { "error" },
        );
    }

    pub fn elapsed(&self, metric: ProtectionMetric, duration: std::time::Duration, ok: bool) {
        self.record(
            "runtime",
            metric.name(),
            (duration.as_secs_f64() * 1000.0).into(),
            if ok { "ok" } else { "error" },
        );
    }

    pub fn timed<T, E>(
        &self,
        metric: ProtectionMetric,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        if !self.enabled {
            return operation();
        }
        let started = Instant::now();
        let result = operation();
        self.elapsed(metric, started.elapsed(), result.is_ok());
        result
    }

    pub fn download<R: Read, T, E>(
        &self,
        source: R,
        operation: impl FnOnce(&mut DownloadReader<R>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut source = DownloadReader {
            inner: source,
            elapsed: std::time::Duration::ZERO,
            enabled: self.enabled,
        };
        let result = operation(&mut source);
        self.elapsed(
            ProtectionMetric::RestoreDownloadElapsedMs,
            source.elapsed,
            result.is_ok(),
        );
        result
    }

    pub fn forward_sandbox(&self, bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        let mut observed = [false; 2];
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| line.len() <= 1024)
        {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            let Some(fields) = value.as_object() else {
                continue;
            };
            const FIELDS: [&str; 8] = [
                "schema",
                "source",
                "phase",
                "operationId",
                "metric",
                "value",
                "outcome",
                "atUnixMs",
            ];
            if fields.keys().any(|key| !FIELDS.contains(&key.as_str()))
                || value["schema"] != "workspace.perf.v1"
                || value["source"] != "sandbox"
                || value["phase"] != "workspaceProtection"
                || value["operationId"] != self.id
            {
                continue;
            }
            let (
                Some(metric @ ("scannedEntries" | "sourceReadBytes")),
                Some(count),
                Some(outcome @ ("ok" | "error" | "interrupted")),
            ) = (
                value["metric"].as_str(),
                value["value"].as_u64(),
                value["outcome"].as_str(),
            )
            else {
                continue;
            };
            observed[usize::from(metric == "sourceReadBytes")] = true;
            self.record("sandbox", metric, count.into(), outcome);
        }
        for (metric, present) in ["scannedEntries", "sourceReadBytes"]
            .into_iter()
            .zip(observed)
        {
            if !present {
                self.missing(metric);
            }
        }
    }

    pub fn upload_reader<R: Read>(&self, source: R, size: u64) -> UploadReader<R> {
        UploadReader {
            source,
            observation: self.clone(),
            count: 0,
            size,
            outcome: "interrupted",
        }
    }
}

pub struct UploadReader<R> {
    source: R,
    observation: WorkspaceProtection,
    count: u64,
    size: u64,
    outcome: &'static str,
}
impl<R: Read> Read for UploadReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if !self.observation.enabled {
            return self.source.read(bytes);
        }
        match self.source.read(bytes) {
            Ok(count) => {
                self.count = self.count.saturating_add(count as u64);
                if self.count == self.size {
                    self.outcome = "ok";
                } else if count == 0 && !bytes.is_empty() {
                    self.outcome = "error";
                }
                Ok(count)
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::Interrupted {
                    self.outcome = "error";
                }
                Err(error)
            }
        }
    }
}
impl<R> Drop for UploadReader<R> {
    fn drop(&mut self) {
        self.observation.record(
            "runtime",
            ProtectionMetric::UploadedBytes.name(),
            self.count.into(),
            self.outcome,
        );
    }
}

pub struct MeasuredWriter<W> {
    pub inner: W,
    pub written: u64,
}
impl<W: Write> Write for MeasuredWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(count as u64);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub struct DownloadReader<R> {
    inner: R,
    elapsed: std::time::Duration,
    enabled: bool,
}
impl<R: Read> Read for DownloadReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if !self.enabled {
            return self.inner.read(bytes);
        }
        let started = Instant::now();
        let result = self.inner.read(bytes);
        self.elapsed += started.elapsed();
        result
    }
}

#[cfg(test)]
pub(crate) fn capture_protection<T>(operation: impl FnOnce() -> T) -> (T, Vec<serde_json::Value>) {
    let (result, records) = capture_protection_records(operation);
    (result, records.events)
}

#[cfg(test)]
fn capture_protection_records<T>(operation: impl FnOnce() -> T) -> (T, ProtectionRecords) {
    let log = Arc::new(std::sync::Mutex::new(ProtectionRecords::default()));
    struct Reset(Option<ProtectionLog>);
    impl Drop for Reset {
        fn drop(&mut self) {
            PROTECTION_LOG.with(|log| log.replace(self.0.take()));
        }
    }
    let _reset = Reset(PROTECTION_LOG.with(|current| current.replace(Some(log.clone()))));
    let result = operation();
    let records = std::mem::take(&mut *log.lock().unwrap());
    (result, records)
}

thread_local! { static JOB: RefCell<Option<String>> = const { RefCell::new(None) }; }
#[cfg(test)]
type TimingLog = std::rc::Rc<RefCell<Vec<serde_json::Value>>>;
#[cfg(test)]
thread_local! { static TIMING_LOG: RefCell<Option<(bool, TimingLog)>> = const { RefCell::new(None) }; }
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
    #[cfg(test)]
    if let Some(enabled) = TIMING_LOG.with(|log| log.borrow().as_ref().map(|(enabled, _)| *enabled))
    {
        return enabled;
    }
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
    #[cfg(test)]
    if TIMING_LOG.with(|log| {
        if let Some((_, log)) = log.borrow().as_ref() {
            log.borrow_mut().push(event.clone());
            true
        } else {
            false
        }
    }) {
        return;
    }
    let _ = writeln!(std::io::stderr().lock(), "{event}");
}

#[cfg(test)]
pub(crate) fn capture_timings<T>(
    enabled: bool,
    operation: impl FnOnce() -> T,
) -> (T, Vec<serde_json::Value>) {
    let log = std::rc::Rc::new(RefCell::new(Vec::new()));
    struct Reset(Option<(bool, TimingLog)>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TIMING_LOG.with(|log| log.replace(self.0.take()));
        }
    }
    let _reset = Reset(TIMING_LOG.with(|current| current.replace(Some((enabled, log.clone())))));
    let result = operation();
    let records = std::mem::take(&mut *log.borrow_mut());
    (result, records)
}

pub struct Context(Option<String>);
/// A safe identity already admitted by Context, owned by one adapter instance.
pub(crate) struct ContextSnapshot(Option<String>);
impl Context {
    pub(crate) fn snapshot() -> ContextSnapshot {
        ContextSnapshot(JOB.with(|job| job.borrow().clone()))
    }

    pub(crate) fn enter_if_absent(snapshot: &ContextSnapshot) -> Self {
        Self(JOB.with(|job| {
            let mut current = job.borrow_mut();
            let previous = current.clone();
            if current.is_none() {
                *current = snapshot.0.clone();
            }
            previous
        }))
    }

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
    fn protection_context_attributes_runtime_and_helper_records_without_payloads() {
        let (_, events) = capture_protection(|| {
            let operation = WorkspaceProtection::new().with_context(
                ProtectionTrigger::RecoveryCheckpoint,
                "run-1",
                Some("job-2"),
            );
            operation.count(ProtectionMetric::UploadAttempts, 1, true);
            let mut stderr = Vec::new();
            for metric in ["scannedEntries", "sourceReadBytes"] {
                stderr.extend_from_slice(
                    serde_json::json!({"schema":"workspace.perf.v1",
                    "source":"sandbox", "phase":"workspaceProtection", "operationId":operation.id,
                    "metric":metric, "value":1, "outcome":"ok"})
                    .to_string()
                    .as_bytes(),
                );
                stderr.push(b'\n');
            }
            operation.forward_sandbox(&stderr);
        });
        assert_eq!(events.len(), 3);
        for event in &events {
            assert_eq!(event["runId"], "run-1");
            assert_eq!(event["jobId"], "job-2");
            assert_eq!(event["trigger"], "recoveryCheckpoint");
            assert_eq!(event["operationId"], events[0]["operationId"]);
            assert!(event.get("sessionId").is_none());
            assert!(event.get("environmentId").is_none());
        }
        let (_, events) = capture_protection(|| {
            WorkspaceProtection::new()
                .with_context(
                    ProtectionTrigger::SessionRestore,
                    "/private/workspace",
                    Some("secret\nbody"),
                )
                .count(ProtectionMetric::UploadAttempts, 0, true);
        });
        assert!(events[0].get("runId").is_none());
        assert!(events[0].get("jobId").is_none());
        assert!(!serde_json::to_string(&events).unwrap().contains("private"));
        assert!(!serde_json::to_string(&events).unwrap().contains("secret"));
    }

    #[test]
    fn protection_missing_helper_counters_are_reported_without_forwarding_diagnostics() {
        for (counters, expected) in [
            (vec![], vec!["scannedEntries", "sourceReadBytes"]),
            (vec!["sourceReadBytes"], vec!["scannedEntries"]),
            (vec!["scannedEntries", "sourceReadBytes"], vec![]),
        ] {
            let (_, records) = capture_protection_records(|| {
                let operation = WorkspaceProtection::new();
                let mut stderr = b"ordinary private helper diagnostic\n".to_vec();
                for metric in counters {
                    stderr.extend_from_slice(serde_json::json!({"schema":"workspace.perf.v1",
                        "source":"sandbox", "phase":"workspaceProtection", "operationId":operation.id,
                        "metric":metric, "value":1, "outcome":"ok"}).to_string().as_bytes());
                    stderr.push(b'\n');
                }
                operation.forward_sandbox(&stderr);
            });
            assert_eq!(records.missing, expected);
            assert!(!serde_json::to_string(&records.events)
                .unwrap()
                .contains("private"));
        }
    }

    #[test]
    fn protection_forwarding_rejects_payloads_foreign_operations_and_unmeasured_values() {
        let (_, events) = capture_protection(|| {
            let operation = WorkspaceProtection::new();
            let allowed = serde_json::json!({"schema":"workspace.perf.v1", "source":"sandbox",
                "phase":"workspaceProtection", "operationId":operation.id, "metric":"sourceReadBytes",
                "value":41, "outcome":"error"});
            operation.forward_sandbox(allowed.to_string().as_bytes());
            let mut injected = allowed.clone();
            injected["message"] = "private contents".into();
            operation.forward_sandbox(injected.to_string().as_bytes());
            let mut foreign = allowed.clone();
            foreign["operationId"] = "other-operation".into();
            operation.forward_sandbox(foreign.to_string().as_bytes());
            let mut unsupported = allowed.clone();
            unsupported["metric"] = "private contents".into();
            operation.forward_sandbox(unsupported.to_string().as_bytes());
            let mut not_measured = allowed;
            not_measured["value"] = (-1).into();
            operation.forward_sandbox(not_measured.to_string().as_bytes());
            operation.forward_sandbox(b"private diagnostic\n");
        });
        let measured: Vec<_> = events
            .iter()
            .filter(|event| event["outcome"] != "unavailable")
            .collect();
        assert_eq!(measured.len(), 1);
        assert_eq!(measured[0]["source"], "sandbox");
        assert_eq!(measured[0]["value"], 41);
        assert_eq!(measured[0]["outcome"], "error");
        assert!(events
            .iter()
            .filter(|event| event["outcome"] == "unavailable")
            .all(|event| event["value"].is_null()));
        assert!(!serde_json::to_string(&events).unwrap().contains("private"));
    }

    #[test]
    fn protection_read_and_write_measurements_preserve_partial_io() {
        struct Partial {
            remaining: usize,
        }
        impl Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Err(std::io::Error::other("synthetic disk failure"));
                }
                let count = bytes.len().min(self.remaining);
                self.remaining -= count;
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut output = MeasuredWriter {
            inner: Partial { remaining: 3 },
            written: 0,
        };
        assert!(output.write_all(b"synthetic body").is_err());
        assert_eq!(output.written, 3);
        let (_, events) = capture_protection(|| {
            let operation = WorkspaceProtection::new();
            let mut reader = operation.upload_reader(std::io::Cursor::new(b"synthetic body"), 14);
            let mut bytes = [0; 3];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"syn");
            // Dropping an interrupted request must not claim its intended full size.
        });
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["metric"], "uploadedBytes");
        assert_eq!(events[0]["value"], 3);
        assert_eq!(events[0]["outcome"], "interrupted");
    }

    #[test]
    fn protection_opt_out_keeps_io_and_errors_without_records() {
        let (_, records) = capture_protection_records(|| {
            let mut operation = WorkspaceProtection::new();
            operation.enabled = false;
            operation.forward_sandbox(b"ordinary private diagnostic");
            let mut bytes = Vec::new();
            operation
                .upload_reader(std::io::Cursor::new(b"body"), 4)
                .read_to_end(&mut bytes)
                .unwrap();
            assert_eq!(bytes, b"body");
            assert_eq!(
                operation.timed::<(), _>(ProtectionMetric::CaptureElapsedMs, || Err(
                    "private failure"
                )),
                Err("private failure")
            );
        });
        assert!(records.events.is_empty());
        assert!(records.missing.is_empty());
    }

    #[test]
    fn protection_download_reports_failed_read_time_without_recording_payloads() {
        let (result, events) = capture_protection(|| {
            let operation = WorkspaceProtection::new();
            operation.download(std::io::Cursor::new(b"body"), |source| {
                let mut target = [0; 8];
                source.read_exact(&mut target)
            })
        });
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["metric"], "restoreDownloadElapsedMs");
        assert!(events[0]["value"].as_f64().unwrap() >= 0.0);
        assert_eq!(events[0]["outcome"], "error");
        assert!(!serde_json::to_string(&events).unwrap().contains("body"));
    }

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

#[cfg(test)]
mod measurement_boundary_tests {
    use super::*;

    #[test]
    fn missing_guest_measurements_are_correlated_unavailable_records() {
        let (_, events) = capture_protection(|| {
            WorkspaceProtection::new()
                .with_context(
                    ProtectionTrigger::TerminalCommit,
                    "run-measured",
                    Some("job-measured"),
                )
                .forward_sandbox(b"private diagnostic must never be forwarded");
        });
        assert_eq!(events.len(), 2);
        for (event, metric) in events.iter().zip(["scannedEntries", "sourceReadBytes"]) {
            assert_eq!(event["metric"], metric);
            assert_eq!(event["outcome"], "unavailable");
            assert!(event["value"].is_null());
            assert_eq!(event["runId"], "run-measured");
            assert_eq!(event["jobId"], "job-measured");
            assert_eq!(event["trigger"], "terminalCommit");
            assert_eq!(event["operationId"], events[0]["operationId"]);
        }
        assert!(!serde_json::to_string(&events).unwrap().contains("private"));
    }
}
