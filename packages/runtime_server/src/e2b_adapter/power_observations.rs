//! Only bounded structured SDK timings may cross the diagnostic boundary.
use crate::observations::power::PowerObservation;
use std::io::Read;

pub(super) fn start_reader(
    observation: Option<PowerObservation>,
    context: crate::observations::ContextSnapshot,
) -> Option<std::sync::mpsc::SyncSender<std::process::ChildStderr>> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("e2b-sdk-timings".into())
        .spawn(move || {
            let _context = crate::observations::Context::enter_if_absent(&context);
            if let Ok(reader) = receive.recv() {
                drain_optional(reader, observation.as_ref());
            }
        })
        .ok()
        .map(|_| send)
}
#[cfg(test)]
pub(super) fn drain(reader: impl Read, observation: &PowerObservation) {
    drain_optional(reader, Some(observation));
}
fn drain_optional(mut reader: impl Read, observation: Option<&PowerObservation>) {
    let mut buffer = [0u8; 4096];
    let mut line = Vec::with_capacity(2048);
    let mut oversized = false;
    let mut power_candidates = 0usize;
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
                    if power_candidates < 64 {
                        if let Some(observation) = observation {
                            observation.forward_sdk_line(&line);
                        }
                        power_candidates += 1;
                    }
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
