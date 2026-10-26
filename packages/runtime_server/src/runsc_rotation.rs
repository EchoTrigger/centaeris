//! Opt-in retained-memory scheduling for native runsc environments.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) max_running: u32,
    pub(crate) idle_pause_ms: u64,
}

impl Config {
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let running = get("RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS").filter(|v| !v.is_empty());
        let idle = get("RUNTIME_RUNSC_IDLE_PAUSE_MS").filter(|v| !v.is_empty());
        if running.is_none() && idle.is_none() {
            return Ok(None);
        }
        if get("RUNTIME_EXECUTION_PROVIDER")
            .as_deref()
            .unwrap_or("docker")
            != "docker"
            || get("OCI_RUNTIME").as_deref().unwrap_or("runsc") != "runsc"
        {
            return Err("runsc_rotation_backend_invalid".into());
        }
        let running = running.ok_or("runsc_rotation_bounds_required")?;
        let idle = idle.ok_or("runsc_rotation_bounds_required")?;
        if running.is_empty()
            || idle.is_empty()
            || !running.bytes().all(|v| v.is_ascii_digit())
            || !idle.bytes().all(|v| v.is_ascii_digit())
        {
            return Err("runsc_rotation_bounds_invalid".into());
        }
        let max_running = running
            .parse::<u32>()
            .map_err(|_| "runsc_running_limit_invalid")?;
        let idle_pause_ms = idle
            .parse::<u64>()
            .map_err(|_| "runsc_idle_pause_invalid")?;
        if max_running == 0 || idle_pause_ms == 0 || idle_pause_ms > i64::MAX as u64 {
            return Err("runsc_rotation_bounds_invalid".into());
        }
        Ok(Some(Self {
            max_running,
            idle_pause_ms,
        }))
    }
}

pub(crate) fn reconcile_startup(
    store: &crate::postgres_store::PostgresRuntimeStore,
    host: &str,
) -> Result<(), String> {
    use crate::docker_execution_host::runsc_power::{observe_power, RunscPowerObservation};
    let mut cursor: Option<String> = None;
    loop {
        let page =
            store.reconcile_runsc_paused(host, cursor.as_deref(), |allocation, profile| {
                observe_power(allocation, profile)
                    .map(|state| state == RunscPowerObservation::Paused)
            })?;
        cursor = page.next;
        if cursor.is_none() {
            return Ok(());
        }
    }
}
pub(crate) struct RotationTask {
    stop: tokio::sync::watch::Sender<bool>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl RotationTask {
    pub(crate) fn start(
        runtime: &tokio::runtime::Runtime,
        store: std::sync::Arc<crate::postgres_store::PostgresRuntimeStore>,
        host: String,
        config: Config,
    ) -> Self {
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let join = runtime.spawn(async move {
            let mut cursor: Option<String> = None;
            let mut last: Option<String> = None;
            let mut pending = false;
            loop {
                if is_stopping(&stopping) {
                    break;
                }
                let jobs = store.clone();
                let host = host.clone();
                let after = cursor.clone();
                let previous = last.clone();
                let stop_signal = stopping.clone();
                let result = tokio::task::spawn_blocking(move || {
                    tick(
                        &jobs,
                        &host,
                        after.as_deref(),
                        previous.as_deref(),
                        config,
                        &stop_signal,
                    )
                })
                .await;
                if is_stopping(&stopping) {
                    break;
                }
                match result {
                    Ok(Ok(next)) => {
                        cursor = next.cursor;
                        last = next.last;
                        if next.pending && !pending {
                            eprintln!("runsc_rotation_observation_pending");
                        }
                        pending = next.pending;
                    }
                    _ => {
                        if !pending {
                            eprintln!("runsc_rotation_worker_unavailable");
                        }
                        pending = true;
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {},
                    _ = stopping.changed() => break,
                }
            }
        });
        Self {
            stop,
            join: Some(join),
        }
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), &'static str> {
        let _ = self.stop.send(true);
        if let Some(join) = self.join.take() {
            tokio::time::timeout(std::time::Duration::from_secs(45), join)
                .await
                .map_err(|_| "runsc_rotation_shutdown_unconfirmed")?
                .map_err(|_| "runsc_rotation_worker_failed")?;
        }
        Ok(())
    }
}

fn is_stopping(stopping: &tokio::sync::watch::Receiver<bool>) -> bool {
    *stopping.borrow() || stopping.has_changed().is_err()
}

fn pause_if_active<T>(
    stopping: &tokio::sync::watch::Receiver<bool>,
    action: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    if is_stopping(stopping) {
        return Err("runsc_rotation_stopping".into());
    }
    action()
}

struct Tick {
    cursor: Option<String>,
    last: Option<String>,
    pending: bool,
}

fn tick(
    store: &crate::postgres_store::PostgresRuntimeStore,
    host: &str,
    after: Option<&str>,
    last: Option<&str>,
    config: Config,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> Result<Tick, String> {
    use crate::docker_execution_host::runsc_power::{pause_if_idle, RunscPowerObservation};
    let observed = store.reconcile_runsc_paused_until(
        host,
        after,
        || is_stopping(stopping),
        |allocation, profile| {
            crate::docker_execution_host::runsc_power::observe_power(allocation, profile)
                .map(|state| state == RunscPowerObservation::Paused)
        },
    )?;
    if is_stopping(stopping) {
        return Ok(Tick {
            cursor: after.map(str::to_owned),
            last: last.map(str::to_owned),
            pending: observed.unconfirmed > 0,
        });
    }
    let mut page = store.runsc_idle_candidates(host, after, config.idle_pause_ms)?;
    page.candidates.sort_by_key(|candidate| {
        (
            last.is_some_and(|old| candidate.allocation.environment_id.as_str() <= old),
            candidate.allocation.environment_id.clone(),
        )
    });
    let mut next = Tick {
        cursor: page.next,
        last: last.map(str::to_owned),
        pending: observed.unconfirmed > 0,
    };
    if let Some(candidate) = page.candidates.into_iter().next() {
        next.last = Some(candidate.allocation.environment_id.clone());
        next.pending |= store
            .with_runsc_idle_pause(&candidate, |allocation, profile| {
                pause_if_active(stopping, || pause_if_idle(allocation, profile))
            })
            .is_err();
    }
    Ok(next)
}
#[cfg(test)]
mod tests {
    #[test]
    fn runsc_stop_received_before_pause_prevents_physical_dispatch() {
        let (stop, stopping) = tokio::sync::watch::channel(false);
        stop.send(true).unwrap();
        let dispatched = std::cell::Cell::new(false);
        let result = super::pause_if_active(&stopping, || {
            dispatched.set(true);
            Ok(())
        });
        assert_eq!(result, Err("runsc_rotation_stopping".into()));
        assert!(!dispatched.get());
    }

    #[test]
    fn runsc_closed_rotation_owner_never_starts_a_new_pause() {
        let (stop, stopping) = tokio::sync::watch::channel(false);
        drop(stop);
        let result = super::pause_if_active::<()>(&stopping, || panic!("rotation owner is gone"));
        assert_eq!(result, Err("runsc_rotation_stopping".into()));
    }

    #[test]
    fn runsc_stop_does_not_erase_an_already_dispatched_pause_observation() {
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let result = super::pause_if_active(&stopping, || {
            stop.send(true).unwrap();
            Ok(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            super::pause_if_active::<()>(&stopping, || panic!("second pause after stop")),
            Err("runsc_rotation_stopping".into())
        );
    }

    use super::*;
    fn selected(values: &[(&str, &str)]) -> Result<Option<Config>, String> {
        Config::from_lookup(|name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).into())
        })
    }
    #[test]
    fn absent_configuration_preserves_existing_deployments() {
        assert_eq!(selected(&[]).unwrap(), None);
    }
    #[test]
    fn native_rotation_requires_both_explicit_positive_bounds() {
        for values in [
            vec![("RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS", "1")],
            vec![("RUNTIME_RUNSC_IDLE_PAUSE_MS", "3000")],
            vec![
                ("RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS", "0"),
                ("RUNTIME_RUNSC_IDLE_PAUSE_MS", "3000"),
            ],
            vec![
                ("RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS", "1"),
                ("RUNTIME_RUNSC_IDLE_PAUSE_MS", "0"),
            ],
        ] {
            assert!(
                selected(&values).is_err(),
                "invalid rotation bounds must fail before startup: {values:?}"
            );
        }
    }
    #[test]
    fn enabled_rotation_requires_the_native_runsc_backend() {
        for (provider, runtime) in [("e2b", "runsc"), ("docker", "runc")] {
            assert!(
                selected(&[
                    ("RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS", "1"),
                    ("RUNTIME_RUNSC_IDLE_PAUSE_MS", "3000"),
                    ("RUNTIME_EXECUTION_PROVIDER", provider),
                    ("OCI_RUNTIME", runtime)
                ])
                .is_err(),
                "foreign backend must not activate runsc scheduling"
            );
        }
    }
    #[test]
    fn configured_rotation_preserves_running_and_resident_boundary() {
        assert_eq!(
            selected(&[
                ("RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS", "1"),
                ("RUNTIME_RUNSC_IDLE_PAUSE_MS", "3000"),
                ("RUNTIME_EXECUTION_PROVIDER", "docker"),
                ("OCI_RUNTIME", "runsc")
            ])
            .unwrap(),
            Some(Config {
                max_running: 1,
                idle_pause_ms: 3000
            })
        );
    }
}
