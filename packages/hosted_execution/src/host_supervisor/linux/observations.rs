//! Opt-in numeric host timings; never include commands, paths or error text.
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Phase {
    Attestation,
    CommandLockWait,
    DockerInspect,
    RuntimeState,
    RuntimeControlRead,
    RuntimeControlWrite,
    OtherCommand,
    RunscLaunch,
    RunscReadyWait,
    RuntimeIdentity,
}

pub(crate) fn measure<T, E>(
    phase: Phase,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled =
        *ENABLED.get_or_init(|| std::env::var("WORKSPACE_PERF_OBSERVATIONS").as_deref() == Ok("1"));
    measure_with(enabled, phase, operation, |record| {
        write_record(&mut std::io::stderr().lock(), &record);
    })
}

fn write_record(writer: &mut impl std::io::Write, record: &serde_json::Value) {
    let _ = writeln!(writer, "host_execution_timing: {record}");
}

fn measure_with<T, E>(
    enabled: bool,
    phase: Phase,
    operation: impl FnOnce() -> Result<T, E>,
    emit: impl FnOnce(serde_json::Value),
) -> Result<T, E> {
    if !enabled {
        return operation();
    }
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let started = Instant::now();
    let result = operation();
    emit(serde_json::json!({
        "phase": phase,
        "startedAtMs": started_at.as_millis() as u64,
        "elapsedMs": started.elapsed().as_secs_f64() * 1000.0,
        "outcome": if result.is_ok() { "ok" } else { "error" },
        "processId": std::process::id(),
    }));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_observation_runs_once_and_emits_nothing() {
        let mut calls = 0;
        let result: Result<_, ()> = measure_with(
            false,
            Phase::DockerInspect,
            || {
                calls += 1;
                Ok(7)
            },
            |_| panic!("disabled observation emitted"),
        );
        assert_eq!(result, Ok(7));
        assert_eq!(calls, 1);
    }

    #[test]
    fn unavailable_diagnostic_output_does_not_change_the_operation_result() {
        struct Unavailable;
        impl std::io::Write for Unavailable {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            measure_with(
                true,
                Phase::DockerInspect,
                || Ok::<_, ()>(7),
                |record| { write_record(&mut Unavailable, &record) }
            ),
            Ok(7)
        );
    }

    #[test]
    fn enabled_observation_preserves_result_and_emits_only_numeric_timing_and_fixed_labels() {
        let mut events = Vec::new();
        let error = "secret command, path and response";
        let result: Result<(), _> = measure_with(
            true,
            Phase::Attestation,
            || Err(error),
            |event| events.push(event),
        );
        assert_eq!(result, Err(error));
        assert_eq!(events.len(), 1);
        let event = &events[0];
        let mut keys: Vec<_> = event
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["elapsedMs", "outcome", "phase", "processId", "startedAtMs"]
        );
        assert_eq!(event["phase"], "attestation");
        assert_eq!(event["outcome"], "error");
        assert!(event["elapsedMs"].as_f64().unwrap() >= 0.0);
        assert!(event["startedAtMs"].as_u64().unwrap() > 0);
        assert!(!event.to_string().contains(error));
        let mut success = None;
        assert_eq!(
            measure_with(
                true,
                Phase::DockerInspect,
                || Ok::<_, ()>(9),
                |event| success = Some(event)
            ),
            Ok(9)
        );
        assert_eq!(success.unwrap()["outcome"], "ok");
    }
}
