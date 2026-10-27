//! Physical pause is fenced by the same admission lock as every native task start.
use super::*;

impl Host {
    fn pause_registered_with(
        &self,
        target: &HostLaunchTarget,
        mut observe: impl FnMut() -> Result<(Registration, bool), String>,
        pause: impl FnOnce() -> Result<(), String>,
    ) -> Result<IdlePauseOutcome, String> {
        use crate::task_service::linux::IdleEnvironmentOperation;
        target.validate()?;
        // The retained Arc prevents spool retirement; never hold this registry
        // over native task admission or any physical control operation.
        let (expected, manager) = {
            let managers = self.managers.lock().map_err(|_| "host_service_failed")?;
            let (registration, manager) = managers
                .get(&target.environment_instance_id)
                .ok_or("host_pause_unregistered")?;
            if &registration.target != target {
                return Err("host_pause_target_mismatch".into());
            }
            (registration.clone(), manager.clone())
        };
        let unknown = || IdlePauseOutcome::Unknown {
            reason: "host_pause_unconfirmed".into(),
        };
        let result = manager.with_idle_environment_operation(|| {
            if self.retirement.blocks(&target.environment_instance_id)? {
                return Ok(unknown());
            }
            let (before, already_paused) = match observe() {
                Ok(value) => value,
                Err(_) => return Ok(unknown()),
            };
            if before != expected {
                return Ok(unknown());
            }
            if already_paused {
                return Ok(IdlePauseOutcome::Paused);
            }
            if pause().is_err() {
                return Ok(unknown());
            }
            match observe() {
                Ok((after, true)) if after == expected => Ok(IdlePauseOutcome::Paused),
                _ => Ok(unknown()),
            }
        })?;
        Ok(match result {
            IdleEnvironmentOperation::Completed(outcome) => outcome,
            IdleEnvironmentOperation::NotIdle => IdlePauseOutcome::Busy,
            IdleEnvironmentOperation::Unknown => IdlePauseOutcome::Unknown {
                reason: "host_task_state_unknown".into(),
            },
        })
    }

    pub(super) fn pause_if_idle(
        &self,
        target: &HostLaunchTarget,
    ) -> Result<IdlePauseOutcome, String> {
        if self.config.oci_runtime != HostOciRuntime::Runsc {
            return Err("host_idle_pause_runtime_unsupported".into());
        }
        // A host restart may reopen an existing attested journal; an arbitrary
        // request must never create a new journal and declare its task list idle.
        if !self
            .managers
            .lock()
            .map_err(|_| "host_service_failed")?
            .contains_key(&target.environment_instance_id)
        {
            self.existing(&target.environment_instance_id)?;
        }
        self.pause_registered_with(
            target,
            || observe_power(&self.config, target),
            || {
                let endpoint = format!("unix://{}", self.config.docker_socket);
                let gate = commands::instance_gate(
                    Path::new(&self.config.runtime_path),
                    Path::new(&self.config.runtime_root),
                    &target.container_id,
                );
                let _command = gate.lock().map_err(|_| "host_command_failed")?;
                command_bytes_unlocked(
                    &self.config.docker_path,
                    &["--host", &endpoint, "pause", &target.container_id],
                )?;
                Ok(())
            },
        )
    }
}

fn observe_power(
    config: &HostSupervisorConfig,
    target: &HostLaunchTarget,
) -> Result<(Registration, bool), String> {
    attest_power(config, target, AttestationPower::Live)
}

pub(super) fn confirmed_power(
    docker_paused: bool,
    runtime_state: Option<&str>,
) -> Result<bool, String> {
    match (docker_paused, runtime_state) {
        (true, Some("paused")) => Ok(true),
        (false, Some("running")) => Ok(false),
        _ => Err("host_power_unknown".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    #[test]
    fn physical_pause_requires_docker_and_runtime_agreement() {
        assert!(confirmed_power(true, Some("paused")).unwrap());
        assert!(!confirmed_power(false, Some("running")).unwrap());
        for (docker, runtime) in [
            (true, Some("running")),
            (false, Some("paused")),
            (true, None),
            (true, Some("stopped")),
        ] {
            assert!(confirmed_power(docker, runtime).is_err());
        }
    }
    struct Fixture {
        root: PathBuf,
        host: Host,
        registration: Registration,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "host-idle-pause-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let config = HostSupervisorConfig {
                schema: HOST_SUPERVISOR_SCHEMA.into(),
                socket_path: root.join("socket").to_string_lossy().into(),
                control_root: root.join("control").to_string_lossy().into(),
                output_root: root.join("output").to_string_lossy().into(),
                oci_runtime: HostOciRuntime::Runsc,
                runtime_path: "/unused/runsc".into(),
                runtime_root: "/unused/runtime".into(),
                docker_path: "/bin/false".into(),
                docker_socket: "/unused/docker".into(),
                controller_uid: 0,
                helper_path: Some("/unused/helper".into()),
            };
            fs::create_dir(&config.control_root).unwrap();
            fs::create_dir(&config.output_root).unwrap();
            let registration = Registration {
                target: HostLaunchTarget {
                    environment_instance_id: format!("instance_{}", "a".repeat(64)),
                    container_id: "b".repeat(64),
                    environment_id: "environment".into(),
                    profile_digest: format!("sha256:{}", "c".repeat(64)),
                    image_digest: format!("sha256:{}", "d".repeat(64)),
                },
                init_pid: 123,
                init_start_time: 456,
                bundle: "/trusted/bundle".into(),
                cgroup: "/trusted/cgroup".into(),
                spec_digest: "e".repeat(64),
                process: serde_json::json!({}),
            };
            let control =
                Path::new(&config.control_root).join(&registration.target.environment_instance_id);
            let output =
                Path::new(&config.output_root).join(&registration.target.environment_instance_id);
            fs::create_dir(&control).unwrap();
            fs::create_dir(&output).unwrap();
            let manager = Arc::new(
                NativeManager::retirement_fixture(
                    control,
                    output,
                    &registration.target.environment_instance_id,
                )
                .unwrap(),
            );
            let host = Host {
                retirement: retirement::RetirementStore::new(&config),
                config,
                managers: Mutex::new(
                    [(
                        registration.target.environment_instance_id.clone(),
                        (registration.clone(), manager),
                    )]
                    .into(),
                ),
                _lease: File::open(&root).unwrap(),
                helper_binary: None,
            };
            Self {
                root,
                host,
                registration,
            }
        }
        fn run(
            &self,
            observe: impl FnMut() -> Result<(Registration, bool), String>,
            pause: impl FnOnce() -> Result<(), String>,
        ) -> Result<IdlePauseOutcome, String> {
            self.host
                .pause_registered_with(&self.registration.target, observe, pause)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn idle_pause_requires_positive_post_pause_observation_and_releases_registry_mutex() {
        let f = Fixture::new();
        let paused = AtomicBool::new(false);
        assert_eq!(
            f.run(
                || Ok((f.registration.clone(), paused.load(Ordering::SeqCst))),
                || {
                    assert!(
                        f.host.managers.try_lock().is_ok(),
                        "physical callback must not hold global registry"
                    );
                    paused.store(true, Ordering::SeqCst);
                    Ok(())
                }
            )
            .unwrap(),
            IdlePauseOutcome::Paused
        );
    }
    #[test]
    fn idle_pause_serializes_with_other_task_admission_and_preserves_busy() {
        let f = Fixture::new();
        let paused = AtomicBool::new(false);
        let manager = f
            .host
            .managers
            .lock()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .1
            .clone();
        assert_eq!(
            f.run(
                || Ok((f.registration.clone(), paused.load(Ordering::SeqCst))),
                || {
                    let second = f
                        .run(
                            || panic!("busy must not observe or mutate physical state"),
                            || panic!("busy must not pause"),
                        )
                        .unwrap();
                    assert_eq!(second, IdlePauseOutcome::Busy);
                    assert!(matches!(
                        manager
                            .with_idle_environment_operation(|| panic!("task gate overlaps pause"))
                            .unwrap(),
                        crate::task_service::linux::IdleEnvironmentOperation::<()>::NotIdle
                    ));
                    paused.store(true, Ordering::SeqCst);
                    Ok(())
                }
            )
            .unwrap(),
            IdlePauseOutcome::Paused
        );
    }
    #[test]
    fn already_paused_requires_same_incarnation_without_dispatch() {
        let f = Fixture::new();
        assert_eq!(
            f.run(
                || Ok((f.registration.clone(), true)),
                || panic!("already paused")
            )
            .unwrap(),
            IdlePauseOutcome::Paused
        );
    }
    #[test]
    fn wrong_target_never_enters_task_gate_or_physical_callback() {
        let f = Fixture::new();
        let mut target = f.registration.target.clone();
        target.container_id = "f".repeat(64);
        assert!(f
            .host
            .pause_registered_with(
                &target,
                || panic!("wrong target"),
                || panic!("wrong target")
            )
            .is_err());
    }
    #[test]
    fn changed_incarnation_and_unknown_observation_do_not_pause() {
        let f = Fixture::new();
        let mut changed = f.registration.clone();
        changed.init_start_time += 1;
        for observation in [Ok((changed, false)), Err("host_target_unknown".into())] {
            assert!(matches!(
                f.run(|| observation.clone(), || panic!("unverified identity"))
                    .unwrap(),
                IdlePauseOutcome::Unknown { .. }
            ));
        }
    }
    #[test]
    fn dispatch_failure_and_unconfirmed_pause_remain_unknown() {
        let f = Fixture::new();
        assert!(matches!(
            f.run(
                || Ok((f.registration.clone(), false)),
                || Err("host_command_timeout".into())
            )
            .unwrap(),
            IdlePauseOutcome::Unknown { .. }
        ));
        assert!(matches!(
            f.run(|| Ok((f.registration.clone(), false)), || Ok(()))
                .unwrap(),
            IdlePauseOutcome::Unknown { .. }
        ));
    }
    #[test]
    fn replacement_after_dispatch_never_confirms_pause() {
        let f = Fixture::new();
        let dispatched = AtomicBool::new(false);
        assert!(matches!(
            f.run(
                || {
                    let mut r = f.registration.clone();
                    if dispatched.load(Ordering::SeqCst) {
                        r.init_pid += 1;
                    }
                    Ok((r, dispatched.load(Ordering::SeqCst)))
                },
                || {
                    dispatched.store(true, Ordering::SeqCst);
                    Ok(())
                }
            )
            .unwrap(),
            IdlePauseOutcome::Unknown { .. }
        ));
    }
    #[test]
    fn pause_protocol_is_strict_and_echoes_immutable_target() {
        let f = Fixture::new();
        let request = serde_json::json!({"schema":HOST_SUPERVISOR_SCHEMA,"request":{"operation":"pauseIfIdle","target":f.registration.target}});
        let envelope: HostSupervisorEnvelope = serde_json::from_value(request.clone()).unwrap();
        envelope.validate().unwrap();
        let mut invalid = request;
        invalid["request"]["force"] = true.into();
        assert!(serde_json::from_value::<HostSupervisorEnvelope>(invalid).is_err());
        let result = HostSupervisorResult::IdlePause {
            target: f.registration.target.clone(),
            outcome: IdlePauseOutcome::Busy,
        };
        let encoded = serde_json::to_value(result).unwrap();
        assert_eq!(
            encoded["target"]["containerId"],
            f.registration.target.container_id
        );
        assert_eq!(encoded["outcome"]["state"], "busy");
    }
}
