//! Only bounded structured SDK timings may cross the diagnostic boundary.
use crate::observations::power::PowerObservation;
use std::io::Read;

pub(super) fn start_reader(
    observation: PowerObservation,
) -> Option<std::sync::mpsc::SyncSender<std::process::ChildStderr>> {
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    // Allocate the optional reader before spawning a child with piped stderr.
    // If no thread is available, retain the ordinary null diagnostic sink.
    std::thread::Builder::new()
        .name("e2b-power-timings".into())
        .spawn(move || {
            if let Ok(reader) = receive.recv() {
                drain(reader, &observation);
            }
        })
        .ok()
        .map(|_| send)
}

pub(super) fn drain(mut reader: impl Read, observation: &PowerObservation) {
    let mut buffer = [0u8; 4096];
    let mut line = Vec::with_capacity(2048);
    let mut oversized = false;
    let mut candidates = 0usize;
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => return,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
            Ok(count) => count,
        };
        for &byte in &buffer[..count] {
            if byte == b'\n' {
                if !oversized && candidates < 64 {
                    observation.forward_sdk_line(&line);
                    candidates += 1;
                }
                line.clear();
                oversized = false;
            } else if !oversized && candidates < 64 {
                if line.len() == 2048 {
                    line.clear();
                    oversized = true;
                } else {
                    line.push(byte);
                }
            }
        }
        // Excess diagnostics are still drained, with fixed memory usage.
        // Nothing except a strictly parsed measurement is forwarded.
    }
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
}
