//! Optional observation of successful Core receipts; no dispatch or persistence authority.
use centaeris_core::runtime::ToolSafePoint;

pub(crate) struct FirstToolTiming {
    run_id: String,
    session_id: String,
    enabled: bool,
    first_call: Option<String>,
    committed: bool,
}
fn identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
}
impl FirstToolTiming {
    pub(crate) fn new(run_id: &str, session_id: &str) -> Self {
        let value = Self {
            run_id: run_id.into(),
            session_id: session_id.into(),
            enabled: super::enabled() && identity(run_id),
            first_call: None,
            committed: false,
        };
        value.record("runStepEntered", None);
        value
    }
    pub(crate) fn receipt_ready(&mut self, point: &ToolSafePoint) -> Option<String> {
        if !self.enabled || self.first_call.is_some() {
            return None;
        }
        let ToolSafePoint::DurableReceipt {
            session_id,
            agent_run_id,
            call,
            result,
            ..
        } = point
        else {
            return None;
        };
        if session_id != &self.session_id
            || agent_run_id != &self.run_id
            || call.id != result.tool_call_id
            || !identity(&call.id)
            || !result.result_state().is_success()
        {
            return None;
        }
        self.first_call = Some(call.id.clone());
        self.record("firstToolReceiptReady", Some(&call.id));
        Some(call.id.clone())
    }
    pub(crate) fn receipt_committed(&mut self, call_id: Option<&str>) {
        if !self.enabled
            || self.committed
            || call_id.is_none()
            || call_id != self.first_call.as_deref()
        {
            return;
        }
        self.committed = true;
        self.record("firstToolReceiptCommitted", call_id);
    }
    fn record(&self, milestone: &'static str, call_id: Option<&str>) {
        if !self.enabled {
            return;
        }
        let stamp = clock_stamp();
        let mut event = serde_json::json!({"phase":"runMilestone","milestone":milestone,
            "runId":self.run_id,"clockDomain":stamp.as_ref().map(|(_,domain)|domain),
            "atMonotonicNs":stamp.map(|(ticks,_)|ticks)});
        if let Some(call_id) = call_id {
            event["callId"] = call_id.into();
        }
        super::emit(event);
    }
}

/// Only identical kernel boot and time-namespace fingerprints share this clock.
/// Wall clock, Rust Instant origins and process IDs never establish correlation.
#[cfg(target_os = "linux")]
fn clock_stamp() -> Option<(u64, String)> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    static DOMAIN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let domain = DOMAIN
        .get_or_init(|| {
            let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
            let boot = boot.trim();
            if boot.len() != 36
                || !boot
                    .bytes()
                    .all(|value| value.is_ascii_hexdigit() || value == b'-')
            {
                return None;
            }
            let namespace = std::fs::metadata("/proc/self/ns/time").ok()?.ino();
            Some(format!(
                "{:x}",
                Sha256::digest(format!("{boot}:{namespace}").as_bytes())
            ))
        })
        .as_ref()?;
    let mut stamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one initialized timespec and retains no pointer.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut stamp) } != 0 {
        return None;
    }
    let seconds = u64::try_from(stamp.tv_sec).ok()?;
    let nanos = u64::try_from(stamp.tv_nsec).ok()?;
    if nanos >= 1_000_000_000 {
        return None;
    }
    Some((
        seconds.checked_mul(1_000_000_000)?.checked_add(nanos)?,
        domain.clone(),
    ))
}
#[cfg(not(target_os = "linux"))]
fn clock_stamp() -> Option<(u64, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observations::capture_timings;
    use centaeris_core::model::ToolCallEnvelope;
    use centaeris_core::tool::layer::ToolExecutionResult;
    fn receipt(run: &str, call_id: &str, status: &str) -> ToolSafePoint {
        ToolSafePoint::DurableReceipt {
            session_id: "session-owned".into(),
            turn_id: "turn-owned".into(),
            agent_run_id: run.into(),
            call: ToolCallEnvelope {
                id: call_id.into(),
                name: "read".into(),
                args_json: "private-input".into(),
            },
            result: ToolExecutionResult {
                tool_call_id: call_id.into(),
                tool_name: "read".into(),
                status: status.into(),
                content: "private-output".into(),
                details: serde_json::json!({}),
                facts: vec![],
                error: None,
                started_at_ms: 1,
                completed_at_ms: 2,
                latency_ms: 1,
                parallel_group: None,
                transition_reason: None,
            },
        }
    }
    #[test]
    fn first_tool_timing_records_ready_and_durable_commit_once_without_payloads() {
        let (_, records) = capture_timings(true, || {
            let mut timing = FirstToolTiming::new("run-owned", "session-owned");
            let token = timing.receipt_ready(&receipt("run-owned", "call-owned", "ok"));
            timing.receipt_committed(token.as_deref());
            timing.receipt_committed(token.as_deref());
            assert!(timing
                .receipt_ready(&receipt("run-owned", "second", "ok"))
                .is_none());
        });
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["milestone"], "runStepEntered");
        assert_eq!(records[1]["milestone"], "firstToolReceiptReady");
        assert_eq!(records[2]["milestone"], "firstToolReceiptCommitted");
        assert_eq!(records[1]["callId"], "call-owned");
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
    }
    #[test]
    fn first_tool_timing_rejects_foreign_failed_and_invalid_receipts_without_fabricating_commit() {
        let (_, records) = capture_timings(true, || {
            let mut timing = FirstToolTiming::new("run-owned", "session-owned");
            assert!(timing
                .receipt_ready(&receipt("foreign", "a", "ok"))
                .is_none());
            assert!(timing
                .receipt_ready(&receipt("run-owned", "failed", "error"))
                .is_none());
            assert!(timing
                .receipt_ready(&receipt("run-owned", "private/invalid", "ok"))
                .is_none());
            timing.receipt_committed(Some("never-ready"));
            assert!(timing
                .receipt_ready(&receipt("run-owned", "call-owned", "ok"))
                .is_some());
        });
        assert_eq!(records.len(), 2);
        assert_eq!(records[1]["milestone"], "firstToolReceiptReady");
    }
    #[test]
    fn first_tool_timing_disabled_has_no_records_or_receipt_tokens() {
        let (_, records) = capture_timings(false, || {
            let mut timing = FirstToolTiming::new("run-owned", "session-owned");
            assert!(timing
                .receipt_ready(&receipt("run-owned", "call-owned", "ok"))
                .is_none());
            timing.receipt_committed(Some("call-owned"));
        });
        assert!(records.is_empty());
    }
}
