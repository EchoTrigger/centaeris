//! Only bounded structured SDK timings may cross the diagnostic boundary.
use crate::observations::power::PowerObservation;
use std::io::Read;

use serde::Deserialize;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAX_BOUND_REQUESTS: usize = 64;
const MAX_POWER_RECORDS_PER_REQUEST: usize = 64;

struct Binding {
    request: String,
    operation: String,
    observation: Option<PowerObservation>,
    records: usize,
}
#[derive(Default)]
struct BindingState {
    requests: VecDeque<Binding>,
    startup_records: usize,
}
/// A finite history permits late stderr to retain its original operation.
/// Observation locks never wait on a provider caller or a diagnostic reader.
pub(super) struct PowerBindings {
    initial: Option<PowerObservation>,
    state: Mutex<BindingState>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TimingIdentity {
    schema: String,
    stage: String,
    request_id: Value,
    operation: Value,
    value: f64,
    outcome: String,
}
impl PowerBindings {
    pub(super) fn new(initial: Option<PowerObservation>) -> Self {
        Self {
            initial,
            state: Mutex::new(BindingState::default()),
        }
    }
    pub(super) fn bind_default(&self, request: &str, operation: &str) {
        if let Some(observation) = &self.initial {
            self.bind(request, operation, observation.clone());
        }
    }
    pub(super) fn bind(&self, request: &str, operation: &str, observation: PowerObservation) {
        if !observation.is_enabled()
            || request.is_empty()
            || request.len() > 128
            || !matches!(operation, "pause" | "connect" | "capabilities")
        {
            return;
        }
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        if let Some(previous) = state
            .requests
            .iter_mut()
            .find(|entry| entry.request == request)
        {
            // A repeated wire ID makes late receipt ownership ambiguous. Keep
            // the tombstone rather than assigning either receipt to a new scope.
            previous.observation = None;
            return;
        }
        if state.requests.len() == MAX_BOUND_REQUESTS {
            state.requests.pop_front();
        }
        state.requests.push_back(Binding {
            request: request.into(),
            operation: operation.into(),
            observation: Some(observation),
            records: 0,
        });
    }
    pub(super) fn forward(&self, line: &[u8]) {
        if line.len() > 2048 {
            return;
        }
        let Ok(timing) = serde_json::from_slice::<TimingIdentity>(line) else {
            return;
        };
        if timing.schema != "runtime.e2b.power_timing.v1"
            || !timing.value.is_finite()
            || timing.value < 0.0
            || !matches!(timing.outcome.as_str(), "ok" | "error")
        {
            return;
        }
        let observation = {
            let Ok(mut state) = self.state.try_lock() else {
                return;
            };
            if timing.request_id.is_null()
                && timing.operation.is_null()
                && matches!(timing.stage.as_str(), "sdkImport" | "sdkClientSetup")
            {
                if state.startup_records == MAX_POWER_RECORDS_PER_REQUEST {
                    return;
                }
                state.startup_records += 1;
                self.initial.clone()
            } else {
                let (Some(request), Some(operation)) =
                    (timing.request_id.as_str(), timing.operation.as_str())
                else {
                    return;
                };
                let Some(binding) = state
                    .requests
                    .iter_mut()
                    .find(|entry| entry.request == request && entry.operation == operation)
                else {
                    return;
                };
                if binding.records == MAX_POWER_RECORDS_PER_REQUEST {
                    return;
                }
                binding.records += 1;
                binding.observation.clone()
            }
        };
        // No IO, logger lock or formatting occurs while holding the bindings.
        if let Some(observation) = observation {
            observation.forward_sdk_line(line);
        }
    }
}

pub(super) fn start_reader(
    bindings: Arc<PowerBindings>,
    context: crate::observations::ContextSnapshot,
) -> Option<std::sync::mpsc::SyncSender<std::process::ChildStderr>> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("e2b-sdk-timings".into())
        .spawn(move || {
            let _context = crate::observations::Context::enter_if_absent(&context);
            if let Ok(reader) = receive.recv() {
                drain_optional(reader, &bindings);
            }
        })
        .ok()
        .map(|_| send)
}
#[cfg(test)]
pub(super) fn drain(reader: impl Read, observation: &PowerObservation) {
    let bindings = PowerBindings::new(Some(observation.clone()));
    bindings.bind("request-1", "pause", observation.clone());
    drain_optional(reader, &bindings);
}
fn drain_optional(mut reader: impl Read, bindings: &PowerBindings) {
    let mut buffer = [0u8; 4096];
    let mut line = Vec::with_capacity(2048);
    let mut oversized = false;
    let mut sdk_records = 0u64;
    let mut sdk_dropped = 0u64;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
            Ok(count) => count,
        };
        for &byte in &buffer[..count] {
            if byte == b'\n' {
                if !oversized {
                    bindings.forward(&line);
                    if crate::observations::sdk::forward_sdk_line(&line, sdk_records < 2048) {
                        if sdk_records < 2048 {
                            sdk_records += 1;
                        } else {
                            sdk_dropped = sdk_dropped.saturating_add(1);
                        }
                    }
                }
                line.clear();
                oversized = false;
            } else if !oversized {
                if line.len() == 2048 {
                    line.clear();
                    oversized = true;
                } else {
                    line.push(byte);
                }
            }
        }
    }
    crate::observations::sdk::dropped_records(sdk_dropped);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observations::power::capture_power_observations;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn reused_power_requests_keep_their_exact_operation_and_late_receipt_binding() {
        let (_, records) = capture_power_observations(|| {
            let first = PowerObservation::new("power-first", "pause");
            let bindings = PowerBindings::new(Some(first.clone()));
            bindings.bind("request-first", "pause", first);
            bindings.bind(
                "request-next",
                "pause",
                PowerObservation::new("power-next", "pause"),
            );
            let line = |request: &str| {
                json!({"schema":"runtime.e2b.power_timing.v1", "stage":"sdkPause",
                "value":1.0,"outcome":"ok","requestId":request,"operation":"pause"})
                .to_string()
            };
            bindings.forward(line("request-next").as_bytes());
            bindings.forward(line("request-first").as_bytes());
            bindings.forward(line("unbound-request").as_bytes());
        });
        assert_eq!(
            records.len(),
            2,
            "unbound SDK lines cannot gain an operation identity"
        );
        assert_eq!(records[0]["operationId"], "power-next");
        assert_eq!(records[1]["operationId"], "power-first");
    }

    fn power_line(request: &str, operation: &str, stage: &str) -> Vec<u8> {
        serde_json::to_vec(
            &json!({"schema":"runtime.e2b.power_timing.v1", "stage":stage,
            "value":1.0,"outcome":"ok","requestId":request,"operation":operation}),
        )
        .unwrap()
    }

    #[test]
    fn request_binding_rejects_wrong_operation_and_ambiguous_reused_ids() {
        let (_, records) = capture_power_observations(|| {
            let bindings = PowerBindings::new(None);
            bindings.bind(
                "unique",
                "pause",
                PowerObservation::new("pause-owner", "pause"),
            );
            bindings.forward(&power_line("unique", "connect", "sdkConnect"));
            bindings.forward(&power_line("unique", "pause", "sdkPause"));
            bindings.bind(
                "unique",
                "pause",
                PowerObservation::new("new-owner", "pause"),
            );
            bindings.forward(&power_line("unique", "pause", "sdkPause"));
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["operationId"], "pause-owner");
    }

    #[test]
    fn startup_is_owned_once_and_request_limits_do_not_exhaust_a_reused_process() {
        let (_, records) = capture_power_observations(|| {
            let bindings =
                PowerBindings::new(Some(PowerObservation::new("startup-owner", "pause")));
            let startup = json!({"schema":"runtime.e2b.power_timing.v1", "stage":"sdkImport",
                "value":2.0,"outcome":"ok","requestId":null,"operation":null})
            .to_string();
            for index in 0..70 {
                let id = format!("request-{index}");
                bindings.bind(
                    &id,
                    "pause",
                    PowerObservation::new(&format!("power-{index}"), "pause"),
                );
                bindings.forward(&power_line(&id, "pause", "sdkPause"));
            }
            bindings.forward(startup.as_bytes());
            // An evicted binding is unavailable, not rebound to the latest owner.
            bindings.forward(&power_line("request-0", "pause", "sdkPause"));
        });
        assert_eq!(records.len(), 71);
        assert_eq!(records[69]["operationId"], "power-69");
        assert_eq!(records[70]["operationId"], "startup-owner");
        assert_eq!(records[70]["metric"], "sdkImportElapsedMs");
    }

    #[test]
    fn per_request_diagnostic_budget_is_independent() {
        let (_, records) = capture_power_observations(|| {
            let bindings = PowerBindings::new(None);
            for id in ["first", "second"] {
                bindings.bind(id, "pause", PowerObservation::new(id, "pause"));
                for _ in 0..100 {
                    bindings.forward(&power_line(id, "pause", "sdkPause"));
                }
            }
        });
        assert_eq!(records.len(), 128);
        assert_eq!(
            records
                .iter()
                .filter(|row| row["operationId"] == "second")
                .count(),
            64
        );
    }

    #[test]
    fn diagnostic_contention_cannot_block_registration_or_business_progress() {
        let (_, records) = capture_power_observations(|| {
            let bindings = PowerBindings::new(None);
            let held = bindings.state.lock().unwrap();
            bindings.bind(
                "blocked",
                "pause",
                PowerObservation::new("not-bound", "pause"),
            );
            bindings.forward(&power_line("blocked", "pause", "sdkPause"));
            drop(held);
            bindings.forward(&power_line("blocked", "pause", "sdkPause"));
            bindings.bind(
                "next",
                "pause",
                PowerObservation::new("next-owner", "pause"),
            );
            bindings.forward(&power_line("next", "pause", "sdkPause"));
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["operationId"], "next-owner");
    }

    fn valid_line() -> String {
        json!({"schema":"runtime.e2b.power_timing.v1","stage":"sdkPause","value":5.0,
            "outcome":"ok","requestId":"request-1","operation":"pause"})
        .to_string()
            + "\n"
    }
    #[test]
    fn structured_sdk_diagnostics_forward_only_allowlisted_timings() {
        let input = "api_key=private-provider-diagnostic\n".to_owned() + &valid_line();
        let (_, records) = capture_power_observations(|| {
            drain(
                Cursor::new(input),
                &PowerObservation::new("power-diagnostic", "pause"),
            );
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["metric"], "sdkPauseElapsedMs");
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
    }
    #[test]
    fn oversized_diagnostic_lines_do_not_hide_the_next_valid_record() {
        let input = "x".repeat(8192) + "\n" + &valid_line();
        let (_, records) = capture_power_observations(|| {
            drain(
                Cursor::new(input),
                &PowerObservation::new("power-oversized", "pause"),
            );
        });
        assert_eq!(records.len(), 1);
    }
    #[test]
    fn diagnostic_forwarding_is_bounded_and_still_drains_the_pipe() {
        let data = valid_line().repeat(128);
        let mut reader = Cursor::new(data.as_bytes());
        let (_, records) = capture_power_observations(|| {
            drain(
                &mut reader,
                &PowerObservation::new("power-bounded", "pause"),
            );
        });
        assert_eq!(records.len(), 64);
        assert_eq!(reader.position(), data.len() as u64);
    }
    #[test]
    fn interrupted_diagnostic_read_keeps_the_following_measurement() {
        struct InterruptedOnce<R> {
            reader: R,
            first: bool,
        }
        impl<R: Read> Read for InterruptedOnce<R> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.first {
                    self.first = false;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.reader.read(buffer)
            }
        }
        let (_, records) = capture_power_observations(|| {
            drain(
                InterruptedOnce {
                    reader: Cursor::new(valid_line()),
                    first: true,
                },
                &PowerObservation::new("power-interrupted", "pause"),
            );
        });
        assert_eq!(records.len(), 1);
    }
    fn sdk_line(stage: &str, operation: &str) -> String {
        json!({"schema":"runtime.e2b.sdk_timing.v1","stage":stage,"operation":operation,"requestId":"req-owned","value":2.5,"outcome":"ok"}).to_string()+"\n"
    }
    #[test]
    fn sdk_diagnostics_export_closed_request_timings_without_request_or_private_values() {
        let input = "private-secret=raw-diagnostic\n".to_owned()
            + &sdk_line("sdkCreate", "create")
            + &sdk_line("sdkCreateVerifyGet", "create")
            + &sdk_line("sdkHandle", "create");
        let (_, records) = crate::observations::capture_timings(true, || {
            drain(
                Cursor::new(input),
                &PowerObservation::new("power-owned", "pause"),
            )
        });
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["phase"], "e2bSdk");
        assert_eq!(records[0]["source"], "sdk");
        assert_eq!(records[0]["stage"], "sdkCreate");
        assert_eq!(records[0]["operation"], "create");
        assert_eq!(records[0]["value"], 2.5);
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
        assert!(!serde_json::to_string(&records)
            .unwrap()
            .contains("req-owned"));
    }
    #[test]
    fn sdk_diagnostics_reject_unknown_fields_stages_operations_duplicates_and_invalid_numbers() {
        let valid = sdk_line("sdkCommandStart", "runCommand");
        let mut fields: serde_json::Value = serde_json::from_str(&valid).unwrap();
        fields["private"] = "secret".into();
        let input = fields.to_string()
            + "\n"
            + &valid.replace("sdkCommandStart", "arbitrary")
            + &valid.replace("runCommand", "create")
            + &valid.replace("2.5", "true")
            + &valid.replace("2.5", "-1")
            + &valid.replace("\"value\":2.5", "\"value\":2.5,\"value\":3")
            + &valid;
        let (_, records) = crate::observations::capture_timings(true, || {
            drain(
                Cursor::new(input),
                &PowerObservation::new("power-owned", "pause"),
            )
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["stage"], "sdkCommandStart");
    }
    #[test]
    fn sdk_diagnostics_forward_existing_startup_only_and_preserve_opt_out() {
        let input=json!({"schema":"runtime.e2b.power_timing.v1","stage":"sdkImport","requestId":null,"operation":null,"value":4.0,"outcome":"ok"}).to_string()+"\n";
        let (_, records) = crate::observations::capture_timings(true, || {
            drain(
                Cursor::new(&input),
                &PowerObservation::new("power-owned", "pause"),
            )
        });
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["stage"], "sdkImport");
        assert_eq!(records[0]["operation"], "startup");
        let (_, records) = crate::observations::capture_timings(false, || {
            drain(
                Cursor::new(input),
                &PowerObservation::new("power-owned", "pause"),
            )
        });
        assert!(records.is_empty());
    }

    #[test]
    fn sdk_diagnostics_overflow_is_explicit_and_pipe_is_completely_drained() {
        let input = sdk_line("sdkHandle", "create").repeat(2050);
        let mut reader = Cursor::new(input.as_bytes());
        let (_, records) = crate::observations::capture_timings(true, || {
            drain(&mut reader, &PowerObservation::new("power-owned", "pause"))
        });
        assert_eq!(records.len(), 2049);
        assert_eq!(records.last().unwrap()["phase"], "executionObservationLoss");
        assert_eq!(records.last().unwrap()["metric"], "sdkDroppedRecords");
        assert_eq!(records.last().unwrap()["value"], 2);
        assert_eq!(reader.position(), input.len() as u64);
    }
    #[test]
    fn sdk_diagnostics_do_not_assign_reused_lane_requests_to_the_spawn_run() {
        let (_, records) = crate::observations::capture_timings(true, || {
            let _spawn = crate::observations::Context::enter(
                br#"{"jobId":"agent_run.lifecycle:spawn-run-a"}"#,
            );
            drain(
                Cursor::new(sdk_line("sdkCommandWait", "waitCommand")),
                &PowerObservation::new("power-owned", "pause"),
            );
        });
        assert_eq!(records.len(), 1);
        assert!(records[0].get("jobId").is_none());
    }
}
