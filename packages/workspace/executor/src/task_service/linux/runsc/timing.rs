//! Host receive timestamps for accepted runner receipts; no guest execution timestamp.
use std::io::Write;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) struct Sample {
    elapsed: Duration,
    at_unix_ms: u64,
}

pub(super) struct TaskTiming {
    task_id: Option<String>,
    start: Instant,
}

impl TaskTiming {
    pub(super) fn new(task_id: &str) -> Self {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        let enabled = *ENABLED
            .get_or_init(|| std::env::var("WORKSPACE_PERF_OBSERVATIONS").as_deref() == Ok("1"));
        Self {
            task_id: (enabled && safe_identity(task_id)).then(|| task_id.to_owned()),
            start: Instant::now(),
        }
    }

    pub(super) fn sample(&self) -> Sample {
        Sample {
            elapsed: self.start.elapsed(),
            at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    pub(super) fn record(&self, phase: &'static str, sample: Sample) {
        let Some(task_id) = self.task_id.as_deref() else {
            return;
        };
        if let Some(record) = record(task_id, phase, sample) {
            let _ = writeln!(std::io::stderr().lock(), "{record}");
        }
    }
}

fn safe_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}

fn record(task_id: &str, phase: &'static str, sample: Sample) -> Option<serde_json::Value> {
    if !safe_identity(task_id) || !matches!(phase, "taskReady" | "taskLeaderExited") {
        return None;
    }
    Some(serde_json::json!({
        "schema": "workspace.perf.v1", "source": "host", "phase": phase,
        "taskId": task_id, "atUnixMs": sample.at_unix_ms,
        "elapsedMs": sample.elapsed.as_secs_f64() * 1000.0, "outcome": "ok",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_host_clock_has_exact_bounded_fields() {
        let event = record(
            "task-123",
            "taskReady",
            Sample {
                elapsed: Duration::from_millis(37),
                at_unix_ms: 991,
            },
        )
        .unwrap();
        assert_eq!(
            event,
            serde_json::json!({
                "schema":"workspace.perf.v1", "source":"host", "phase":"taskReady",
                "taskId":"task-123", "atUnixMs":991, "elapsedMs":37.0, "outcome":"ok",
            })
        );
    }

    #[test]
    fn unsafe_identity_and_unknown_phase_are_omitted() {
        for identity in ["", "task\nsecret", "task/private", &"x".repeat(161)] {
            assert!(record(
                identity,
                "taskReady",
                Sample {
                    elapsed: Duration::ZERO,
                    at_unix_ms: 0
                }
            )
            .is_none());
        }
        assert!(record(
            "task-1",
            "command-output",
            Sample {
                elapsed: Duration::ZERO,
                at_unix_ms: 0
            }
        )
        .is_none());
    }
}
