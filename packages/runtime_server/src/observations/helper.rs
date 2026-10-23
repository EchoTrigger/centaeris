//! Closed, opt-in helper timings. No request, command or guest bytes are retained.
use hosted_execution::host_supervisor::TrustedHelperKind;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
pub(crate) enum HelperMetric {
    Mkdir,
    Upload,
    Execute,
    Wait,
    Download,
    Cleanup,
    Control,
    Total,
}

impl HelperMetric {
    fn name(self) -> &'static str {
        match self {
            Self::Mkdir => "mkdirElapsedMs",
            Self::Upload => "uploadElapsedMs",
            Self::Execute => "executeElapsedMs",
            Self::Wait => "waitElapsedMs",
            Self::Download => "downloadElapsedMs",
            Self::Cleanup => "cleanupElapsedMs",
            Self::Control => "controlElapsedMs",
            Self::Total => "totalElapsedMs",
        }
    }
}

pub(crate) struct HelperObservation {
    operation_id: String,
    kind: TrustedHelperKind,
    dispatch_id: Option<String>,
}

impl HelperObservation {
    pub(crate) fn new(kind: TrustedHelperKind) -> Self {
        // Correlation only: this sequence grants no execution or replay authority.
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let enabled = super::enabled();
        Self {
            operation_id: if enabled {
                format!(
                    "helper-{}-{}-{}",
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
            kind,
            dispatch_id: if enabled {
                super::provider_dispatch::current_id()
            } else {
                None
            },
        }
    }

    pub(crate) fn timed<T, E>(
        &self,
        metric: HelperMetric,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        if self.operation_id.is_empty() {
            return operation();
        }
        let mut span = HelperSpan {
            observation: self,
            metric,
            started: Instant::now(),
            outcome: "interrupted",
        };
        let result = operation();
        span.outcome = if result.is_ok() { "ok" } else { "error" };
        result
    }

    fn record(&self, metric: HelperMetric, started: Instant, outcome: &'static str) {
        let value = started.elapsed().as_secs_f64() * 1000.0;
        if !value.is_finite() || value < 0.0 {
            return;
        }
        let mut event = serde_json::json!({"phase":"e2bHelper", "operationId":self.operation_id,
            "helperKind":self.kind, "metric":metric.name(), "value":value, "outcome":outcome});
        if let Some(dispatch_id) = &self.dispatch_id {
            event["dispatchId"] = dispatch_id.clone().into();
        }
        // Use the existing bounded record format and safe Job context. Its IO
        // errors are deliberately ignored, independently of the product result.
        super::emit(event);
    }
}

struct HelperSpan<'a> {
    observation: &'a HelperObservation,
    metric: HelperMetric,
    started: Instant,
    outcome: &'static str,
}

impl Drop for HelperSpan<'_> {
    fn drop(&mut self) {
        self.observation
            .record(self.metric, self.started, self.outcome);
    }
}
