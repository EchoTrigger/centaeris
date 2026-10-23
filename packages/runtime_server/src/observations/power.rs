//! Bounded provider-power timings without provider diagnostics or workload data.
use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
type PowerLog = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;
#[cfg(test)]
thread_local! {
    static POWER_LOG: std::cell::RefCell<Option<PowerLog>> = const { std::cell::RefCell::new(None) };
}

const SDK_LINE_BYTES: usize = 2048;

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

#[derive(Clone, Copy)]
pub(crate) enum PowerMetric {
    StorePrepare,
    StoreDispatchCas,
    StoreSideEffectPins,
    StoreCallback,
    StoreAckCommit,
    StoreAdvance,
    RuntimePower,
    ContextDrop,
    AdapterSpawn,
    AdapterHandshake,
    AdapterRoundTrip,
    AdapterClose,
    SdkImport,
    SdkClientSetup,
    SdkIdentityGet,
    SdkPause,
    SdkConnect,
    SdkResumeVerifyGet,
    SdkHandle,
}

#[derive(Clone, Copy)]
pub(crate) enum PowerOutcome {
    Ok,
    Error,
    Pending,
    Replayed,
}

impl PowerMetric {
    fn name(self) -> &'static str {
        match self {
            Self::StorePrepare => "storePrepareElapsedMs",
            Self::StoreDispatchCas => "storeDispatchCasElapsedMs",
            Self::StoreSideEffectPins => "storeSideEffectPinsElapsedMs",
            Self::StoreCallback => "storeCallbackElapsedMs",
            Self::StoreAckCommit => "storeAckCommitElapsedMs",
            Self::StoreAdvance => "storeAdvanceElapsedMs",
            Self::RuntimePower => "runtimePowerElapsedMs",
            Self::ContextDrop => "contextDropElapsedMs",
            Self::AdapterSpawn => "adapterSpawnElapsedMs",
            Self::AdapterHandshake => "adapterHandshakeElapsedMs",
            Self::AdapterRoundTrip => "adapterRoundTripElapsedMs",
            Self::AdapterClose => "adapterCloseElapsedMs",
            Self::SdkImport => "sdkImportElapsedMs",
            Self::SdkClientSetup => "sdkClientSetupElapsedMs",
            Self::SdkIdentityGet => "sdkIdentityGetElapsedMs",
            Self::SdkPause => "sdkPauseElapsedMs",
            Self::SdkConnect => "sdkConnectElapsedMs",
            Self::SdkResumeVerifyGet => "sdkResumeVerifyGetElapsedMs",
            Self::SdkHandle => "sdkHandleElapsedMs",
        }
    }
}

impl PowerOutcome {
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Pending => "pending",
            Self::Replayed => "replayed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ok" => Some(Self::Ok),
            "error" => Some(Self::Error),
            "pending" => Some(Self::Pending),
            "replayed" => Some(Self::Replayed),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SdkTiming {
    schema: String,
    stage: String,
    value: f64,
    outcome: String,
    // These nullable fields are still required on the wire. Value also keeps
    // wrong primitive types distinct from the only permitted explicit null.
    request_id: Value,
    operation: Value,
}

#[derive(Clone)]
pub(crate) struct PowerObservation {
    enabled: bool,
    operation_id: String,
    action: &'static str,
    #[cfg(test)]
    log: Option<PowerLog>,
}

impl PowerObservation {
    pub(crate) fn new(operation_id: &str, action: &str) -> Self {
        #[cfg(test)]
        let log = POWER_LOG.with(|log| log.borrow().clone());
        let enabled = super::enabled();
        #[cfg(test)]
        let enabled = enabled || log.is_some();
        let action = match action {
            "pause" => "pause",
            "resume" => "resume",
            _ => "",
        };
        let enabled = enabled && valid_identity(operation_id) && !action.is_empty();
        Self {
            enabled,
            operation_id: if enabled {
                operation_id.into()
            } else {
                String::new()
            },
            action,
            #[cfg(test)]
            log,
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn timed<T, E>(
        &self,
        metric: PowerMetric,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        if !self.enabled {
            return operation();
        }
        let started = Instant::now();
        let result = operation();
        self.elapsed(
            metric,
            started.elapsed(),
            if result.is_ok() {
                PowerOutcome::Ok
            } else {
                PowerOutcome::Error
            },
        );
        result
    }

    pub(crate) fn elapsed(&self, metric: PowerMetric, duration: Duration, outcome: PowerOutcome) {
        self.record("runtime", metric, duration.as_secs_f64() * 1000.0, outcome);
    }

    fn record(&self, source: &'static str, metric: PowerMetric, value: f64, outcome: PowerOutcome) {
        if !self.enabled || !value.is_finite() || value < 0.0 {
            return;
        }
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let event = serde_json::json!({"schema":"workspace.perf.v1","source":source,
            "phase":"providerPower","operationId":self.operation_id,"action":self.action,
            "metric":metric.name(),"value":value,"outcome":outcome.name(),
            "atUnixMs":u64::try_from(at_ms).unwrap_or(u64::MAX)});
        #[cfg(test)]
        if let Some(log) = &self.log {
            if let Ok(mut records) = log.lock() {
                records.push(event);
            }
            return;
        }
        let _ = writeln!(std::io::stderr().lock(), "{event}");
    }

    pub(crate) fn forward_sdk_line(&self, line: &[u8]) {
        if !self.enabled || line.len() > SDK_LINE_BYTES {
            return;
        }
        let Ok(timing) = serde_json::from_slice::<SdkTiming>(line) else {
            return;
        };
        if timing.schema != "runtime.e2b.power_timing.v1"
            || !timing.value.is_finite()
            || timing.value < 0.0
            || !matches!(timing.outcome.as_str(), "ok" | "error")
        {
            return;
        }
        let Some(outcome) = PowerOutcome::parse(&timing.outcome) else {
            return;
        };
        let expected = match self.action {
            "pause" => "pause",
            "resume" => "connect",
            _ => return,
        };
        let request = timing
            .request_id
            .as_str()
            .is_some_and(|value| value.len() <= 128 && valid_identity(value));
        let operation = timing.operation.as_str();
        let startup = timing.request_id.is_null() && timing.operation.is_null();
        let metric = match timing.stage.as_str() {
            "sdkImport" if startup => PowerMetric::SdkImport,
            "sdkClientSetup" if startup => PowerMetric::SdkClientSetup,
            "sdkIdentityGet" if request && operation == Some(expected) => {
                PowerMetric::SdkIdentityGet
            }
            "sdkPause" if request && self.action == "pause" && operation == Some("pause") => {
                PowerMetric::SdkPause
            }
            "sdkConnect" if request && self.action == "resume" && operation == Some("connect") => {
                PowerMetric::SdkConnect
            }
            "sdkResumeVerifyGet"
                if request && self.action == "resume" && operation == Some("connect") =>
            {
                PowerMetric::SdkResumeVerifyGet
            }
            "sdkHandle"
                if request
                    && (operation == Some("capabilities") || operation == Some(expected)) =>
            {
                PowerMetric::SdkHandle
            }
            _ => return,
        };
        // The explicitly bound child observer owns correlation. SDK request IDs,
        // provider operation strings and all diagnostic content stay off this log.
        self.record("sdk", metric, timing.value, outcome);
    }
}

#[cfg(test)]
pub(crate) fn capture_power_observations<T>(operation: impl FnOnce() -> T) -> (T, Vec<Value>) {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    struct Reset(Option<PowerLog>);
    impl Drop for Reset {
        fn drop(&mut self) {
            POWER_LOG.with(|log| log.replace(self.0.take()));
        }
    }
    let _reset = Reset(POWER_LOG.with(|current| current.replace(Some(log.clone()))));
    let result = operation();
    let records = match log.lock() {
        Ok(mut records) => std::mem::take(&mut *records),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    };
    (result, records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    fn sdk_line(stage: &str, operation: Option<&str>, request_id: Option<&str>) -> Value {
        json!({"schema":"runtime.e2b.power_timing.v1","stage":stage,"value":2.5,
            "outcome":"ok","operation":operation,"requestId":request_id})
    }

    fn forward(observer: &PowerObservation, value: &Value) {
        observer.forward_sdk_line(&serde_json::to_vec(value).unwrap());
    }

    fn assert_record(
        event: &Value,
        operation_id: &str,
        action: &str,
        source: &str,
        metric: &str,
        outcome: &str,
        value: f64,
    ) {
        assert_eq!(event.as_object().unwrap().len(), 9);
        assert_eq!(event["schema"], "workspace.perf.v1");
        assert_eq!(event["phase"], "providerPower");
        assert_eq!(event["operationId"], operation_id);
        assert_eq!(event["action"], action);
        assert_eq!(event["source"], source);
        assert_eq!(event["metric"], metric);
        assert_eq!(event["outcome"], outcome);
        assert_eq!(event["value"].as_f64(), Some(value));
        assert!(event["atUnixMs"].as_u64().unwrap() > 0);
    }

    #[test]
    fn runtime_timings_use_closed_metric_names_values_and_outcomes() {
        let metrics = [
            (PowerMetric::StorePrepare, "storePrepareElapsedMs"),
            (PowerMetric::StoreDispatchCas, "storeDispatchCasElapsedMs"),
            (
                PowerMetric::StoreSideEffectPins,
                "storeSideEffectPinsElapsedMs",
            ),
            (PowerMetric::StoreCallback, "storeCallbackElapsedMs"),
            (PowerMetric::StoreAckCommit, "storeAckCommitElapsedMs"),
            (PowerMetric::StoreAdvance, "storeAdvanceElapsedMs"),
            (PowerMetric::RuntimePower, "runtimePowerElapsedMs"),
            (PowerMetric::ContextDrop, "contextDropElapsedMs"),
            (PowerMetric::AdapterSpawn, "adapterSpawnElapsedMs"),
            (PowerMetric::AdapterHandshake, "adapterHandshakeElapsedMs"),
            (PowerMetric::AdapterRoundTrip, "adapterRoundTripElapsedMs"),
            (PowerMetric::AdapterClose, "adapterCloseElapsedMs"),
            (PowerMetric::SdkImport, "sdkImportElapsedMs"),
            (PowerMetric::SdkClientSetup, "sdkClientSetupElapsedMs"),
            (PowerMetric::SdkIdentityGet, "sdkIdentityGetElapsedMs"),
            (PowerMetric::SdkPause, "sdkPauseElapsedMs"),
            (PowerMetric::SdkConnect, "sdkConnectElapsedMs"),
            (
                PowerMetric::SdkResumeVerifyGet,
                "sdkResumeVerifyGetElapsedMs",
            ),
            (PowerMetric::SdkHandle, "sdkHandleElapsedMs"),
        ];
        let outcomes = [
            (PowerOutcome::Ok, "ok"),
            (PowerOutcome::Error, "error"),
            (PowerOutcome::Pending, "pending"),
            (PowerOutcome::Replayed, "replayed"),
        ];
        let (_, records) = capture_power_observations(|| {
            let observer = PowerObservation::new("power-1", "pause");
            assert!(observer.is_enabled());
            for (index, (metric, _)) in metrics.iter().enumerate() {
                observer.elapsed(*metric, Duration::from_micros(1250), outcomes[index % 4].0);
            }
        });
        assert_eq!(records.len(), metrics.len());
        for (index, (event, (_, name))) in records.iter().zip(metrics).enumerate() {
            assert_record(
                event,
                "power-1",
                "pause",
                "runtime",
                name,
                outcomes[index % 4].1,
                1.25,
            );
        }
    }

    #[test]
    fn timed_preserves_exact_results_and_does_not_retry_or_expose_errors() {
        let calls = AtomicUsize::new(0);
        let ((success, failure), records) = capture_power_observations(|| {
            let observer = PowerObservation::new("power-result", "resume");
            let success = observer.timed(PowerMetric::StorePrepare, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, &str>("private-success")
            });
            let failure = observer.timed(PowerMetric::StoreCallback, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>("private-error token and workload body")
            });
            (success, failure)
        });
        assert_eq!(success, Ok("private-success"));
        assert_eq!(failure, Err("private-error token and workload body"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["outcome"], "ok");
        assert_eq!(records[1]["outcome"], "error");
        for record in &records {
            assert!(record["value"].as_f64().unwrap().is_finite());
        }
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
    }

    #[test]
    fn disabled_observer_preserves_failure_and_never_forwards_data() {
        let (_, records) = capture_power_observations(|| {
            let mut observer = PowerObservation::new("disabled", "pause");
            observer.enabled = false;
            assert!(!observer.is_enabled());
            let mut called = false;
            let result = observer.timed(PowerMetric::RuntimePower, || {
                called = true;
                Err::<(), _>("original-error")
            });
            assert!(called);
            assert_eq!(result, Err("original-error"));
            observer.elapsed(
                PowerMetric::RuntimePower,
                Duration::ZERO,
                PowerOutcome::Pending,
            );
            forward(
                &observer,
                &sdk_line("sdkPause", Some("pause"), Some("request-1")),
            );
        });
        assert!(records.is_empty());
    }

    #[test]
    fn invalid_correlation_is_disabled_even_inside_capture() {
        let (_, records) = capture_power_observations(|| {
            for (id, action) in [
                ("", "pause"),
                ("private/path", "pause"),
                ("private\nbody", "pause"),
                ("power", "kill"),
                ("power", "PAUSE"),
                ("power", "pause secret"),
                (&"a".repeat(257), "pause"),
            ] {
                let observer = PowerObservation::new(id, action);
                assert!(!observer.is_enabled());
                observer.elapsed(PowerMetric::RuntimePower, Duration::ZERO, PowerOutcome::Ok);
            }
        });
        assert!(records.is_empty());
    }

    #[test]
    fn sdk_startup_and_request_records_drop_wire_identity_and_use_parent_context() {
        let (_, records) = capture_power_observations(|| {
            let observer = PowerObservation::new("parent-pause", "pause");
            for stage in ["sdkImport", "sdkClientSetup"] {
                forward(&observer, &sdk_line(stage, None, None));
            }
            for stage in ["sdkIdentityGet", "sdkPause", "sdkHandle"] {
                forward(
                    &observer,
                    &sdk_line(stage, Some("pause"), Some("private-request-id")),
                );
            }
            forward(
                &observer,
                &sdk_line(
                    "sdkHandle",
                    Some("capabilities"),
                    Some("private-handshake-id"),
                ),
            );
        });
        let metrics = [
            "sdkImportElapsedMs",
            "sdkClientSetupElapsedMs",
            "sdkIdentityGetElapsedMs",
            "sdkPauseElapsedMs",
            "sdkHandleElapsedMs",
            "sdkHandleElapsedMs",
        ];
        assert_eq!(records.len(), metrics.len());
        for (record, metric) in records.iter().zip(metrics) {
            assert_record(record, "parent-pause", "pause", "sdk", metric, "ok", 2.5);
        }
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
    }

    #[test]
    fn sdk_connect_stages_are_valid_only_for_the_bound_resume_request() {
        let (_, records) = capture_power_observations(|| {
            let pause = PowerObservation::new("pause-parent", "pause");
            let resume = PowerObservation::new("resume-parent", "resume");
            for stage in [
                "sdkIdentityGet",
                "sdkConnect",
                "sdkResumeVerifyGet",
                "sdkHandle",
            ] {
                let mut line = sdk_line(stage, Some("connect"), Some("sdk-request"));
                line["outcome"] = json!("error");
                forward(&resume, &line);
                forward(&pause, &line);
            }
        });
        let metrics = [
            "sdkIdentityGetElapsedMs",
            "sdkConnectElapsedMs",
            "sdkResumeVerifyGetElapsedMs",
            "sdkHandleElapsedMs",
        ];
        assert_eq!(records.len(), metrics.len());
        for (record, metric) in records.iter().zip(metrics) {
            assert_record(
                record,
                "resume-parent",
                "resume",
                "sdk",
                metric,
                "error",
                2.5,
            );
        }
    }

    #[test]
    fn sdk_parser_drops_malformed_unknown_nonfinite_and_diagnostic_lines() {
        let valid = sdk_line("sdkPause", Some("pause"), Some("request"));
        let mut invalid = Vec::new();
        for key in valid.as_object().unwrap().keys() {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(key);
            invalid.push(serde_json::to_vec(&missing).unwrap());
        }
        for (key, value) in [
            ("schema", json!("runtime.e2b.power_timing.v2")),
            ("stage", json!("workload-file-secret")),
            ("stage", json!(true)),
            ("outcome", json!("private-error")),
            ("value", json!(-1)),
            ("value", json!(true)),
            ("value", json!("2.5")),
            ("value", Value::Null),
            ("requestId", Value::Null),
            ("requestId", json!("private/path")),
            ("requestId", json!("a".repeat(257))),
            ("operation", Value::Null),
            ("operation", json!("kill")),
            ("body", json!("private-body")),
            ("error", json!({"message":"private-error"})),
        ] {
            let mut changed = valid.clone();
            changed[key] = value;
            invalid.push(serde_json::to_vec(&changed).unwrap());
        }
        for (stage, operation, id) in [
            ("sdkImport", Some("pause"), Some("request")),
            ("sdkClientSetup", None, Some("request")),
            ("sdkPause", Some("capabilities"), Some("request")),
            ("sdkConnect", Some("pause"), Some("request")),
            ("sdkResumeVerifyGet", Some("pause"), Some("request")),
            ("sdkIdentityGet", Some("capabilities"), Some("request")),
        ] {
            invalid.push(serde_json::to_vec(&sdk_line(stage, operation, id)).unwrap());
        }
        invalid.extend([
            b"private stdout diagnostic".to_vec(), vec![0xff], b"[]".to_vec(),
            br#"{"schema":"runtime.e2b.power_timing.v1","stage":"sdkPause","value":NaN,"outcome":"ok","requestId":"request","operation":"pause"}"#.to_vec(),
            br#"{"schema":"runtime.e2b.power_timing.v1","stage":"sdkPause","value":1e999,"outcome":"ok","requestId":"request","operation":"pause"}"#.to_vec(),
            br#"{"schema":"runtime.e2b.power_timing.v1","stage":"sdkPause","value":1,"value":2,"outcome":"ok","requestId":"request","operation":"pause"}"#.to_vec(),
        ]);
        let (_, records) = capture_power_observations(|| {
            let observer = PowerObservation::new("safe-parent", "pause");
            for line in invalid {
                observer.forward_sdk_line(&line);
            }
        });
        assert!(records.is_empty());
    }

    #[test]
    fn sdk_line_limit_is_two_kibibytes_including_whitespace() {
        let mut line =
            serde_json::to_vec(&sdk_line("sdkPause", Some("pause"), Some("request"))).unwrap();
        line.resize(2048, b' ');
        let (_, records) = capture_power_observations(|| {
            let observer = PowerObservation::new("bounded-parent", "pause");
            observer.forward_sdk_line(&line);
            line.push(b' ');
            observer.forward_sdk_line(&line);
        });
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn sdk_wire_rejects_nonterminal_outcomes_and_request_ids_over_128_bytes() {
        let (_, records) = capture_power_observations(|| {
            let observer = PowerObservation::new("wire-contract", "pause");
            for outcome in ["pending", "replayed"] {
                for mut line in [
                    sdk_line("sdkPause", Some("pause"), Some("request")),
                    sdk_line("sdkImport", None, None),
                ] {
                    line["outcome"] = json!(outcome);
                    forward(&observer, &line);
                }
            }
            for length in [129, 256] {
                forward(
                    &observer,
                    &sdk_line("sdkPause", Some("pause"), Some(&"a".repeat(length))),
                );
            }
            forward(
                &observer,
                &sdk_line("sdkPause", Some("pause"), Some(&"a".repeat(128))),
            );
        });
        assert_eq!(records.len(), 1);
        assert_record(
            &records[0],
            "wire-contract",
            "pause",
            "sdk",
            "sdkPauseElapsedMs",
            "ok",
            2.5,
        );
    }

    #[test]
    fn cloned_contexts_follow_db_threads_without_cross_operation_correlation() {
        let (_, records) = capture_power_observations(|| {
            let pause = PowerObservation::new("pause-a", "pause");
            let resume = PowerObservation::new("resume-b", "resume");
            let calls = Arc::new(AtomicUsize::new(0));
            std::thread::scope(|scope| {
                for (observer, action) in [(pause.clone(), "pause"), (resume.clone(), "resume")] {
                    let calls = calls.clone();
                    scope.spawn(move || {
                        assert_eq!(
                            observer.timed(PowerMetric::StoreCallback, || {
                                calls.fetch_add(1, Ordering::SeqCst);
                                Ok::<_, ()>(action)
                            }),
                            Ok(action)
                        );
                    });
                }
            });
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        });
        assert_eq!(records.len(), 2);
        for event in records {
            assert_eq!(
                event["operationId"],
                if event["action"] == "pause" {
                    "pause-a"
                } else {
                    "resume-b"
                }
            );
        }
    }

    #[test]
    fn nested_captures_restore_outer_context_without_mixing_logs() {
        let ((inner, late), outer) = capture_power_observations(|| {
            let before = PowerObservation::new("outer", "pause");
            let (_, inner) = capture_power_observations(|| {
                PowerObservation::new("inner", "resume").elapsed(
                    PowerMetric::RuntimePower,
                    Duration::ZERO,
                    PowerOutcome::Ok,
                );
            });
            before.elapsed(PowerMetric::RuntimePower, Duration::ZERO, PowerOutcome::Ok);
            let after = PowerObservation::new("outer-after", "pause");
            after.elapsed(PowerMetric::RuntimePower, Duration::ZERO, PowerOutcome::Ok);
            (inner, after)
        });
        let (_, unrelated) = capture_power_observations(|| {
            late.elapsed(
                PowerMetric::StoreAdvance,
                Duration::ZERO,
                PowerOutcome::Replayed,
            );
        });
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0]["operationId"], "inner");
        assert_eq!(outer.len(), 2);
        assert_eq!(outer[0]["operationId"], "outer");
        assert_eq!(outer[1]["operationId"], "outer-after");
        assert!(
            unrelated.is_empty(),
            "a captured observer retains its original sink"
        );
    }
}
