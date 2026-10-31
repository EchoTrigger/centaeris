use super::*;
use crate::agent_run_authorization::EnvironmentResources;
use centaeris_core::execution::EnvironmentTaskOwner;
use std::collections::VecDeque;

struct FakeIo {
    host: String,
    later_hosts: VecDeque<String>,
    inspected_ids: Vec<String>,
    observations: VecDeque<Result<Option<serde_json::Value>, String>>,
    mutations: Vec<(String, RunscPowerAction)>,
    mutation_error: bool,
    removals: Vec<String>,
    events: std::rc::Rc<std::cell::RefCell<Vec<&'static str>>>,
}
impl PowerIo for FakeIo {
    fn remove_stopped(&mut self, id: &str) -> Result<(), String> {
        self.events.borrow_mut().push("remove");
        self.removals.push(id.into());
        if self.mutation_error {
            Err("lost Docker removal acknowledgement".into())
        } else {
            Ok(())
        }
    }
    fn host_id(&mut self) -> Result<String, String> {
        Ok(self
            .later_hosts
            .pop_front()
            .unwrap_or_else(|| self.host.clone()))
    }
    fn inspect(&mut self, id: &str) -> Result<Option<serde_json::Value>, String> {
        self.events.borrow_mut().push("inspect");
        self.inspected_ids.push(id.into());
        self.observations
            .pop_front()
            .expect("fixture must account for every physical observation")
    }
    fn change(&mut self, id: &str, action: RunscPowerAction) -> Result<(), String> {
        self.mutations.push((id.into(), action));
        if self.mutation_error {
            Err("lost Docker acknowledgement".into())
        } else {
            Ok(())
        }
    }
}
fn fixture() -> (RetainedInstanceAllocation, EnvironmentProfile) {
    let profile = EnvironmentProfile {
        schema: "runtime.environment_profile.v1".into(),
        image_digest: format!("sha256:{}", "a".repeat(64)),
        workspace_root: "/app".into(),
        user: "10001:10001".into(),
        network: ExecutionNetwork::Disabled,
        resources: EnvironmentResources {
            memory_bytes: 1024 * 1024 * 1024,
            cpu_milli: 1000,
            pids_limit: 128,
            workspace_bytes: 64 * 1024 * 1024,
            storage_bytes: 1024 * 1024 * 1024,
        },
    };
    let allocation = RetainedInstanceAllocation {
        allocation_execution_id: "execution".into(),
        allocation_agent_run_id: Some("run".into()),
        allocation_owner: EnvironmentTaskOwner::AgentRun {
            run_id: "run".into(),
        },
        instance_id: "instance".into(),
        environment_id: "environment".into(),
        host_id: "daemon".into(),
        image_digest: profile.image_digest.clone(),
        authorization_digest: "authorization".into(),
        binding_digest: "binding".into(),
        profile_digest: profile.digest().unwrap(),
        container_id: Some("c".repeat(64)),
        reused: true,
    };
    (allocation, profile)
}
fn facts(
    a: &RetainedInstanceAllocation,
    p: &EnvironmentProfile,
    paused: bool,
) -> serde_json::Value {
    serde_json::json!({"Id":a.container_id,"Image":a.image_digest,
        "State":{"Running":true,"Paused":paused,"Restarting":false,"Dead":false},
        "Config":{"User":p.user,"WorkingDir":"/","Labels":{
            "centaeris.managed":"true","centaeris.environment_id":a.environment_id,
            "centaeris.instance_id":a.instance_id,"centaeris.execution_id":a.allocation_execution_id,
            "centaeris.agent_run_id":"run","centaeris.allocation_owner":serde_json::to_string(&a.allocation_owner).unwrap(),
            "centaeris.environment_authorization_digest":a.binding_digest,"centaeris.environment_profile_digest":a.profile_digest}},
        "HostConfig":{"Runtime":"runsc","Memory":p.resources.memory_bytes,"CpuQuota":100000,
            "CpuPeriod":100000,"PidsLimit":128,"NetworkMode":"none"}})
}
fn io(values: Vec<serde_json::Value>) -> FakeIo {
    FakeIo {
        host: "daemon".into(),
        later_hosts: VecDeque::new(),
        inspected_ids: Vec::new(),
        observations: values.into_iter().map(|v| Ok(Some(v))).collect(),
        mutations: Vec::new(),
        mutation_error: false,
        removals: Vec::new(),
        events: Default::default(),
    }
}
#[test]
fn pauses_and_resumes_the_exact_container_without_changing_its_identity() {
    let (a, p) = fixture();
    let mut docker = io(vec![
        facts(&a, &p, false),
        facts(&a, &p, true),
        facts(&a, &p, true),
        facts(&a, &p, false),
    ]);
    assert_eq!(
        set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause),
        Ok(RunscPowerObservation::Paused)
    );
    assert_eq!(
        set_power_with(&mut docker, &a, &p, RunscPowerAction::Resume),
        Ok(RunscPowerObservation::Running)
    );
    assert_eq!(
        docker.mutations,
        vec![
            (a.container_id.clone().unwrap(), RunscPowerAction::Pause),
            (a.container_id.clone().unwrap(), RunscPowerAction::Resume)
        ]
    );
}
#[test]
fn replaying_an_observed_state_does_not_dispatch_another_mutation() {
    let (a, p) = fixture();
    let mut docker = io(vec![facts(&a, &p, true)]);
    assert_eq!(
        set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause),
        Ok(RunscPowerObservation::Paused)
    );
    assert!(docker.mutations.is_empty());
}
#[test]
fn a_lost_acknowledgement_is_resolved_only_by_the_observed_physical_state() {
    let (a, p) = fixture();
    let mut docker = io(vec![facts(&a, &p, false), facts(&a, &p, true)]);
    docker.mutation_error = true;
    assert_eq!(
        set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause),
        Ok(RunscPowerObservation::Paused)
    );
    assert_eq!(docker.mutations.len(), 1);
}
#[test]
fn mutation_acknowledgement_does_not_replace_physical_confirmation() {
    let (a, p) = fixture();
    let mut docker = io(vec![facts(&a, &p, false), facts(&a, &p, false)]);
    assert!(set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause).is_err());
    assert_eq!(docker.mutations.len(), 1);
}
#[test]
fn foreign_runtime_owner_instance_profile_or_container_never_mutates() {
    let (a, p) = fixture();
    for pointer in [
        "/Id",
        "/Image",
        "/Config/Labels/centaeris.managed",
        "/Config/Labels/centaeris.environment_id",
        "/Config/Labels/centaeris.instance_id",
        "/Config/Labels/centaeris.execution_id",
        "/Config/Labels/centaeris.allocation_owner",
        "/Config/Labels/centaeris.environment_authorization_digest",
        "/Config/Labels/centaeris.environment_profile_digest",
        "/HostConfig/Runtime",
    ] {
        let mut value = facts(&a, &p, false);
        *value.pointer_mut(pointer).unwrap() = serde_json::json!("foreign");
        let mut docker = io(vec![value]);
        assert!(
            set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause).is_err(),
            "foreign field {pointer}"
        );
        assert!(docker.mutations.is_empty(), "foreign field {pointer}");
    }
}
#[test]
fn foreign_host_or_stopped_container_never_mutates() {
    let (a, p) = fixture();
    let mut docker = io(vec![]);
    docker.host = "other daemon".into();
    assert!(set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause).is_err());
    assert!(docker.mutations.is_empty());
    let mut value = facts(&a, &p, false);
    value["State"]["Running"] = false.into();
    let mut docker = io(vec![value]);
    assert!(set_power_with(&mut docker, &a, &p, RunscPowerAction::Resume).is_err());
    assert!(docker.mutations.is_empty());
}
#[test]
fn disappearance_or_failed_inspection_after_dispatch_does_not_release_capacity() {
    let (a, p) = fixture();
    for after in [Ok(None), Err("Docker unavailable".into())] {
        let mut docker = io(vec![facts(&a, &p, false)]);
        docker.observations.push_back(after);
        assert!(set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause).is_err());
        assert_eq!(docker.mutations.len(), 1);
    }
}
#[test]
fn changed_physical_identity_after_mutation_is_not_an_acknowledgement() {
    let (a, p) = fixture();
    let mut after = facts(&a, &p, true);
    after["Id"] = "replacement".into();
    let mut docker = io(vec![facts(&a, &p, false), after]);
    assert!(set_power_with(&mut docker, &a, &p, RunscPowerAction::Pause).is_err());
}

#[test]
fn idle_pause_preserves_busy_and_unknown_without_direct_power_mutations() {
    for outcome in [
        IdlePauseOutcome::Busy,
        IdlePauseOutcome::Unknown {
            reason: "task_state_unknown".into(),
        },
    ] {
        let (a, p) = fixture();
        let mut docker = io(vec![facts(&a, &p, false)]);
        let actual = pause_if_idle_with(&mut docker, &a, &p, |target| {
            assert_eq!(target.container_id, a.container_id.clone().unwrap());
            assert_eq!(target.environment_instance_id, a.instance_id);
            Ok(outcome.clone())
        })
        .unwrap();
        assert_eq!(actual, outcome);
        assert!(docker.mutations.is_empty());
    }
}

#[test]
fn idle_pause_requires_the_same_container_to_be_observed_paused_after_host_ack() {
    let (a, p) = fixture();
    let mut docker = io(vec![facts(&a, &p, false), facts(&a, &p, false)]);
    assert_eq!(
        pause_if_idle_with(&mut docker, &a, &p, |_| Ok(IdlePauseOutcome::Paused)),
        Err("runsc_idle_pause_not_observed".into())
    );
    let mut docker = io(vec![facts(&a, &p, false), facts(&a, &p, true)]);
    assert_eq!(
        pause_if_idle_with(&mut docker, &a, &p, |_| Ok(IdlePauseOutcome::Paused)),
        Ok(IdlePauseOutcome::Paused)
    );
}

#[test]
fn idle_pause_transport_loss_keeps_the_outcome_uncertain() {
    let (a, p) = fixture();
    let mut docker = io(vec![facts(&a, &p, false)]);
    assert_eq!(
        pause_if_idle_with(&mut docker, &a, &p, |_| Err(
            "environment_host_transport_unavailable".into()
        )),
        Err("environment_host_transport_unavailable".into())
    );
    assert!(docker.mutations.is_empty());
}

#[test]
fn stopped_original_is_observed_separately_from_identity_and_never_powered() {
    let (a, p) = fixture();
    let mut stopped = facts(&a, &p, false);
    stopped["State"]["Running"] = false.into();
    let mut docker = io(vec![stopped.clone()]);
    assert_eq!(
        observe_with(&mut docker, a.container_id.as_deref().unwrap(), &a, &p),
        Ok(RunscPowerObservation::Stopped)
    );
    for action in [RunscPowerAction::Pause, RunscPowerAction::Resume] {
        let mut docker = io(vec![stopped.clone()]);
        assert_eq!(
            set_power_with(&mut docker, &a, &p, action),
            Err("runsc_power_container_stopped".into())
        );
        assert!(docker.mutations.is_empty());
    }
    let mut docker = io(vec![stopped]);
    assert_eq!(
        pause_if_idle_with(&mut docker, &a, &p, |_| {
            panic!("a stopped container must never enter host pause")
        }),
        Err("runsc_power_container_stopped".into())
    );
}

#[test]
fn ambiguous_or_inconsistent_power_fields_remain_unknown() {
    let (a, p) = fixture();
    for (field, value) in [
        ("Running", serde_json::Value::Null),
        ("Paused", serde_json::Value::Null),
        ("Restarting", true.into()),
        ("Restarting", serde_json::Value::Null),
        ("Dead", true.into()),
        ("Dead", serde_json::Value::Null),
    ] {
        let mut uncertain = facts(&a, &p, false);
        uncertain["State"][field] = value;
        let mut docker = io(vec![uncertain]);
        assert_eq!(
            observe_with(&mut docker, a.container_id.as_deref().unwrap(), &a, &p),
            Err("runsc_power_observation_unknown".into()),
            "ambiguous power field {field}"
        );
        assert!(docker.mutations.is_empty());
    }
    let mut inconsistent = facts(&a, &p, true);
    inconsistent["State"]["Running"] = false.into();
    let mut docker = io(vec![inconsistent]);
    assert_eq!(
        observe_with(&mut docker, a.container_id.as_deref().unwrap(), &a, &p),
        Err("runsc_power_observation_unknown".into())
    );
}
fn stopped(a: &RetainedInstanceAllocation, p: &EnvironmentProfile) -> serde_json::Value {
    let mut value = facts(a, p, false);
    value["State"]["Running"] = false.into();
    value
}

#[test]
fn recovery_observation_distinguishes_exact_absence_from_unknown() {
    let (a, p) = fixture();
    for (inspection, expected) in [
        (Ok(None), Ok(None)),
        (
            Ok(Some(stopped(&a, &p))),
            Ok(Some(RunscPowerObservation::Stopped)),
        ),
        (
            Err("Docker timeout".into()),
            Err("runsc_power_observation_unknown".into()),
        ),
        (
            Ok(Some(serde_json::json!({}))),
            Err("runsc_power_physical_identity_rejected".into()),
        ),
    ] {
        let mut docker = io(vec![]);
        docker.observations.push_back(inspection);
        assert_eq!(observe_recovery_target_with(&mut docker, &a, &p), expected);
        assert!(docker.mutations.is_empty());
        assert!(docker.removals.is_empty());
    }
}

#[test]
fn stopped_cleanup_preserves_before_nonforcing_removal_and_confirms_exact_absence() {
    let (a, p) = fixture();
    let mut docker = io(vec![stopped(&a, &p), stopped(&a, &p)]);
    docker.observations.push_back(Ok(None));
    let preserved = std::cell::Cell::new(false);
    let events = docker.events.clone();
    let removed = remove_stopped_after_preservation_with(&mut docker, &a, &p, || {
        preserved.set(true);
        events.borrow_mut().push("preserve");
        Ok(())
    })
    .unwrap();
    assert!(preserved.get());
    assert!(docker
        .inspected_ids
        .iter()
        .all(|id| Some(id.as_str()) == a.container_id.as_deref()));
    assert_eq!(
        *events.borrow(),
        vec!["inspect", "preserve", "inspect", "remove", "inspect"]
    );
    assert_eq!(docker.removals, vec![a.container_id.clone().unwrap()]);
    assert!(docker.mutations.is_empty());
    assert_eq!(
        removed,
        crate::postgres_store::cleanup_receipts::CleanupPhysicalResult::Removed {
            instance_id: a.instance_id.clone(),
            container_id: a.container_id.clone().unwrap()
        }
    );
}

#[test]
fn stopped_cleanup_requires_preservation_and_retains_responsibility_on_uncertainty() {
    let (a, p) = fixture();
    let mut docker = io(vec![stopped(&a, &p)]);
    assert_eq!(
        remove_stopped_after_preservation_with(&mut docker, &a, &p, || {
            Err("workspace_preservation_unconfirmed".into())
        }),
        Err("workspace_preservation_unconfirmed".into())
    );
    assert!(docker.removals.is_empty());
    for after in [Err("Docker timeout".into()), Ok(Some(stopped(&a, &p)))] {
        let mut docker = io(vec![stopped(&a, &p), stopped(&a, &p)]);
        docker.observations.push_back(after);
        assert!(remove_stopped_after_preservation_with(&mut docker, &a, &p, || Ok(())).is_err());
        assert_eq!(docker.removals.len(), 1);
    }
}

#[test]
fn stopped_cleanup_lost_remove_ack_requires_independent_exact_absence() {
    let (a, p) = fixture();
    let mut docker = io(vec![stopped(&a, &p), stopped(&a, &p)]);
    docker.mutation_error = true;
    docker.observations.push_back(Ok(None));
    assert!(remove_stopped_after_preservation_with(&mut docker, &a, &p, || Ok(())).is_ok());
    assert_eq!(docker.removals.len(), 1);
}

#[test]
fn stopped_cleanup_rejects_live_foreign_or_changed_target_without_removal() {
    let (a, p) = fixture();
    let mut foreign = stopped(&a, &p);
    foreign["Config"]["Labels"]["centaeris.allocation_owner"] = "foreign".into();
    for first in [facts(&a, &p, false), facts(&a, &p, true), foreign.clone()] {
        let mut docker = io(vec![first]);
        assert!(
            remove_stopped_after_preservation_with(&mut docker, &a, &p, || {
                panic!("invalid cleanup target must not request preservation")
            })
            .is_err()
        );
        assert!(docker.removals.is_empty());
    }
    for after in [facts(&a, &p, false), foreign] {
        let mut docker = io(vec![stopped(&a, &p), after]);
        assert!(remove_stopped_after_preservation_with(&mut docker, &a, &p, || Ok(())).is_err());
        assert!(docker.removals.is_empty());
    }
}

#[test]
fn stopped_cleanup_exact_absence_replays_without_guest_or_removal() {
    let (a, p) = fixture();
    let mut docker = io(vec![]);
    docker.observations.push_back(Ok(None));
    assert!(
        remove_stopped_after_preservation_with(&mut docker, &a, &p, || {
            panic!("already absent exact target has no writable layer to preserve")
        })
        .is_ok()
    );
    assert!(docker.removals.is_empty());
    assert!(docker.mutations.is_empty());
}

#[test]
fn stopped_cleanup_revalidates_host_after_preservation_without_removal() {
    let (a, p) = fixture();
    let mut docker = io(vec![stopped(&a, &p)]);
    docker.later_hosts = VecDeque::from(["daemon".into(), "daemon".into(), "foreign".into()]);
    let preserved = std::cell::Cell::new(false);
    assert_eq!(
        remove_stopped_after_preservation_with(&mut docker, &a, &p, || {
            preserved.set(true);
            Ok(())
        }),
        Err("runsc_power_host_rejected".into())
    );
    assert!(preserved.get());
    assert!(docker.removals.is_empty());
}

#[test]
fn stopped_cleanup_never_preserves_or_removes_a_foreign_immutable_identity() {
    let (a, p) = fixture();
    for pointer in [
        "/Id",
        "/Image",
        "/HostConfig/Runtime",
        "/Config/Labels/centaeris.managed",
        "/Config/Labels/centaeris.environment_id",
        "/Config/Labels/centaeris.instance_id",
        "/Config/Labels/centaeris.execution_id",
        "/Config/Labels/centaeris.agent_run_id",
        "/Config/Labels/centaeris.allocation_owner",
        "/Config/Labels/centaeris.environment_authorization_digest",
        "/Config/Labels/centaeris.environment_profile_digest",
    ] {
        let mut value = stopped(&a, &p);
        *value.pointer_mut(pointer).unwrap() = "foreign".into();
        let mut docker = io(vec![value]);
        assert_eq!(
            remove_stopped_after_preservation_with(&mut docker, &a, &p, || {
                panic!("foreign identity {pointer} must not request preservation")
            }),
            Err("runsc_power_physical_identity_rejected".into()),
            "foreign identity {pointer}"
        );
        assert!(docker.removals.is_empty(), "foreign identity {pointer}");
    }
}

#[test]
fn ordinary_power_observation_never_reports_stopped_as_unpaused_or_running() {
    let (a, p) = fixture();
    let mut docker = io(vec![stopped(&a, &p)]);
    assert_eq!(
        observe_power_with(&mut docker, &a, &p).map(|state| state == RunscPowerObservation::Paused),
        Err("runsc_power_container_stopped".into())
    );
    assert!(docker.mutations.is_empty());
    assert!(docker.removals.is_empty());
    let mut docker = io(vec![stopped(&a, &p)]);
    assert_eq!(
        observe_recovery_target_with(&mut docker, &a, &p),
        Ok(Some(RunscPowerObservation::Stopped))
    );
}
