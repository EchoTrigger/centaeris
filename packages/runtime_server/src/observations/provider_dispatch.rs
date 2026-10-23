//! Timings for the existing provider execution-dispatch scope only.
//!
//! `totalElapsedMs` contains every other stage; do not add it to its children.
//! Checkout includes the existing blocking handoff, pool checkout and timeout
//! setup. Connection return includes pool reset/return (or discard) and the
//! blocking handoff back. Neither is a pure SQL/pool-wait duration. Transaction
//! rollback and bookkeeping can remain unattributed inside total. The return
//! outcome describes the enclosing result, not a separate pool-health verdict.
//! Callback includes the entire physical callback, including framed-helper waits.
//! No pool, lock, authority, callback or transaction policy is changed here.
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

thread_local! {
    // Diagnostic context only. It never participates in dispatch or authority.
    static CURRENT_DISPATCH: RefCell<Option<String>> = const { RefCell::new(None) };
}
static NEXT_DISPATCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
pub(crate) enum DispatchMetric {
    AuthorityBefore,
    Callback,
    AuthorityAfter,
    Commit,
}
impl DispatchMetric {
    fn name(self) -> &'static str {
        match self {
            Self::AuthorityBefore => "authorityBeforeElapsedMs",
            Self::Callback => "callbackElapsedMs",
            Self::AuthorityAfter => "authorityAfterElapsedMs",
            Self::Commit => "commitElapsedMs",
        }
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Ok,
    Error,
    Interrupted,
}
impl Outcome {
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Interrupted => "interrupted",
        }
    }
    fn result(ok: bool) -> Self {
        if ok {
            Self::Ok
        } else {
            Self::Error
        }
    }
}
struct Measurement {
    metric: &'static str,
    duration: Duration,
    outcome: Outcome,
}
struct Stage<'a> {
    records: &'a mut Vec<Measurement>,
    metric: DispatchMetric,
    started: Instant,
    outcome: Outcome,
}
impl Drop for Stage<'_> {
    fn drop(&mut self) {
        self.records.push(Measurement {
            metric: self.metric.name(),
            duration: self.started.elapsed(),
            outcome: self.outcome,
        });
    }
}
struct CallbackContext(Option<String>);
impl Drop for CallbackContext {
    fn drop(&mut self) {
        CURRENT_DISPATCH.with(|current| {
            current.replace(self.0.take());
        });
    }
}

pub(crate) fn current_id() -> Option<String> {
    CURRENT_DISPATCH.with(|current| current.borrow().clone())
}

/// One instance owns at most the seven fixed-stage records on the dispatch path.
/// Buffer until the caller is back outside its existing connection scope: no
/// diagnostic stderr write is introduced while the transaction pins are held.
pub(crate) struct DispatchObservation {
    started: Option<Instant>,
    dispatch_id: Option<String>,
    records: Vec<Measurement>,
    client_entered: bool,
    connection_return_started: Option<Instant>,
}
impl DispatchObservation {
    pub(crate) fn new() -> Self {
        if !super::enabled() {
            return Self {
                started: None,
                dispatch_id: None,
                records: Vec::new(),
                client_entered: false,
                connection_return_started: None,
            };
        }
        let started = Instant::now();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let ordinal = NEXT_DISPATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            started: Some(started),
            dispatch_id: Some(format!(
                "dispatch-{}-{timestamp}-{ordinal}",
                std::process::id()
            )),
            records: Vec::with_capacity(7),
            client_entered: false,
            connection_return_started: None,
        }
    }
    pub(crate) fn entered_client(&mut self) {
        if let Some(started) = self.started {
            self.records.push(Measurement {
                metric: "checkoutElapsedMs",
                duration: started.elapsed(),
                outcome: Outcome::Ok,
            });
            self.client_entered = true;
        }
    }
    pub(crate) fn leaving_client(&mut self) {
        if self.started.is_some() {
            self.connection_return_started = Some(Instant::now());
        }
    }
    pub(crate) fn timed<T, E>(
        &mut self,
        metric: DispatchMetric,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        if self.started.is_none() {
            return operation();
        }
        let mut stage = Stage {
            records: &mut self.records,
            metric,
            started: Instant::now(),
            outcome: Outcome::Interrupted,
        };
        let result = operation();
        stage.outcome = Outcome::result(result.is_ok());
        result
    }
    fn enter_callback(&self) -> CallbackContext {
        CallbackContext(CURRENT_DISPATCH.with(|current| current.replace(self.dispatch_id.clone())))
    }
    pub(crate) fn callback<T, E>(
        &mut self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let _context = self.enter_callback();
        self.timed(DispatchMetric::Callback, operation)
    }
    #[cfg(test)]
    fn callback_context<T>(&self, operation: impl FnOnce() -> T) -> T {
        let _context = self.enter_callback();
        operation()
    }
    pub(crate) fn finish(mut self, ok: bool) {
        self.emit(Outcome::result(ok));
    }
    fn emit(&mut self, outcome: Outcome) {
        let Some(started) = self.started.take() else {
            return;
        };
        let ended = Instant::now();
        if !self.client_entered {
            self.records.push(Measurement {
                metric: "checkoutElapsedMs",
                duration: ended.duration_since(started),
                outcome,
            });
        }
        if let Some(return_started) = self.connection_return_started {
            self.records.push(Measurement {
                metric: "connectionReturnElapsedMs",
                duration: ended.duration_since(return_started),
                outcome,
            });
        }
        self.records.push(Measurement {
            metric: "totalElapsedMs",
            duration: ended.duration_since(started),
            outcome,
        });
        for record in self.records.drain(..) {
            // Only fixed labels, local identity and numeric duration enter JSON.
            // super::emit supplies schema/source/wall emission time/current Job.
            super::emit(serde_json::json!({
                "phase":"providerDispatch", "dispatchId":self.dispatch_id,
                "metric":record.metric, "value":record.duration.as_secs_f64()*1000.0,
                "outcome":record.outcome.name(),
            }));
        }
    }
}
impl Drop for DispatchObservation {
    fn drop(&mut self) {
        self.emit(Outcome::Interrupted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observations::{capture_timings, Context};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn successful_dispatch() -> Result<u32, &'static str> {
        let mut observation = DispatchObservation::new();
        observation.entered_client();
        observation.timed(
            DispatchMetric::AuthorityBefore,
            || Ok::<_, &'static str>(()),
        )?;
        let value = observation.timed(DispatchMetric::Callback, || Ok::<_, &'static str>(17))?;
        observation.timed(DispatchMetric::AuthorityAfter, || Ok::<_, &'static str>(()))?;
        observation.timed(DispatchMetric::Commit, || Ok::<_, &'static str>(()))?;
        observation.leaving_client();
        observation.finish(true);
        Ok(value)
    }

    #[test]
    fn enabled_dispatch_records_closed_stages_under_one_local_identity() {
        let (result, records) = capture_timings(true, || {
            let _job = Context::enter(br#"{"jobId":"agent_run.lifecycle:dispatch-test"}"#);
            successful_dispatch()
        });
        assert_eq!(result, Ok(17));
        let metrics: Vec<_> = records
            .iter()
            .map(|row| row["metric"].as_str().unwrap())
            .collect();
        assert_eq!(
            metrics,
            [
                "checkoutElapsedMs",
                "authorityBeforeElapsedMs",
                "callbackElapsedMs",
                "authorityAfterElapsedMs",
                "commitElapsedMs",
                "connectionReturnElapsedMs",
                "totalElapsedMs"
            ]
        );
        let identity = records[0]["dispatchId"].as_str().unwrap();
        assert!(
            !identity.is_empty()
                && identity
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
        );
        for row in &records {
            assert_eq!(row["schema"], "workspace.perf.v1");
            assert_eq!(row["source"], "runtime");
            assert_eq!(row["phase"], "providerDispatch");
            assert_eq!(row["jobId"], "agent_run.lifecycle:dispatch-test");
            assert_eq!(row["dispatchId"], identity);
            assert_eq!(row["outcome"], "ok");
            assert!(row["value"].as_f64().unwrap() >= 0.0);
            let mut keys: Vec<_> = row
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    "atUnixMs",
                    "dispatchId",
                    "jobId",
                    "metric",
                    "outcome",
                    "phase",
                    "schema",
                    "source",
                    "value"
                ]
            );
        }
        let children: f64 = records[..6]
            .iter()
            .map(|row| row["value"].as_f64().unwrap())
            .sum();
        assert!(records[6]["value"].as_f64().unwrap() >= children);
    }

    #[test]
    fn enabled_and_disabled_wrappers_preserve_once_only_result_and_error() {
        for enabled in [false, true] {
            for succeeds in [false, true] {
                let calls = AtomicUsize::new(0);
                let (result, records) = capture_timings(enabled, || {
                    let mut observation = DispatchObservation::new();
                    observation.entered_client();
                    let result = observation.timed(DispatchMetric::Callback, || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        if succeeds {
                            Ok(29)
                        } else {
                            Err("private callback failure")
                        }
                    });
                    observation.leaving_client();
                    observation.finish(result.is_ok());
                    result
                });
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    result,
                    if succeeds {
                        Ok(29)
                    } else {
                        Err("private callback failure")
                    }
                );
                assert_eq!(records.is_empty(), !enabled);
                assert!(!serde_json::to_string(&records)
                    .unwrap()
                    .contains("private callback failure"));
            }
        }
    }

    #[test]
    fn callback_error_does_not_invent_unexecuted_authority_or_commit_stages() {
        let (_, records) = capture_timings(true, || {
            let mut observation = DispatchObservation::new();
            observation.entered_client();
            let result = observation.timed(DispatchMetric::Callback, || Err::<(), _>("secret"));
            observation.leaving_client();
            observation.finish(result.is_ok());
        });
        let metrics: Vec<_> = records
            .iter()
            .map(|row| row["metric"].as_str().unwrap())
            .collect();
        assert_eq!(
            metrics,
            [
                "checkoutElapsedMs",
                "callbackElapsedMs",
                "connectionReturnElapsedMs",
                "totalElapsedMs"
            ]
        );
        assert_eq!(records[1]["outcome"], "error");
        assert_eq!(records[3]["outcome"], "error");
    }

    #[test]
    fn failure_before_client_entry_records_only_failed_checkout_and_total() {
        let (_, records) = capture_timings(true, || DispatchObservation::new().finish(false));
        let metrics: Vec<_> = records
            .iter()
            .map(|row| row["metric"].as_str().unwrap())
            .collect();
        assert_eq!(metrics, ["checkoutElapsedMs", "totalElapsedMs"]);
        assert!(records.iter().all(|row| row["outcome"] == "error"));
    }

    #[test]
    fn callback_identity_is_scoped_nested_and_restored_after_panic() {
        let _ = capture_timings(true, || {
            assert_eq!(current_id(), None);
            let outer = DispatchObservation::new();
            outer.callback_context(|| {
                let outer_id = current_id().expect("outer callback identity");
                let inner = DispatchObservation::new();
                let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    inner.callback_context(|| {
                        assert_ne!(current_id().as_deref(), Some(outer_id.as_str()));
                        panic!("diagnostic scope only");
                    })
                }));
                assert!(panic.is_err());
                assert_eq!(current_id().as_deref(), Some(outer_id.as_str()));
            });
            assert_eq!(current_id(), None);
        });
    }

    #[test]
    fn callback_identity_is_thread_local_and_disabled_diagnostics_are_absent() {
        for enabled in [false, true] {
            let _ = capture_timings(enabled, || {
                let observation = DispatchObservation::new();
                observation.callback_context(|| {
                    assert_eq!(current_id().is_some(), enabled);
                    assert_eq!(std::thread::spawn(current_id).join().unwrap(), None);
                });
                assert_eq!(current_id(), None);
            });
        }
    }

    #[test]
    fn panic_stage_is_recorded_as_interrupted_without_error_text() {
        let (_, records) = capture_timings(true, || {
            let mut observation = DispatchObservation::new();
            observation.entered_client();
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                observation.timed(DispatchMetric::Callback, || -> Result<(), ()> {
                    panic!("never retain this secret")
                })
            }));
            assert!(panic.is_err());
            observation.leaving_client();
            observation.finish(false);
        });
        assert!(records
            .iter()
            .any(|row| row["metric"] == "callbackElapsedMs" && row["outcome"] == "interrupted"));
        assert!(!serde_json::to_string(&records)
            .unwrap()
            .contains("never retain this secret"));
    }
}
