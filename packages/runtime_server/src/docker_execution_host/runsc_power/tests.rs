use super::*;
use crate::agent_run_authorization::EnvironmentResources;
use centaeris_core::execution::EnvironmentTaskOwner;
use std::collections::VecDeque;

struct FakeIo {
    host: String,
    observations: VecDeque<Result<Option<serde_json::Value>, String>>,
    mutations: Vec<(String, RunscPowerAction)>,
    mutation_error: bool,
}
impl PowerIo for FakeIo {
    fn host_id(&mut self) -> Result<String, String> {
        Ok(self.host.clone())
    }
    fn inspect(&mut self, _id: &str) -> Result<Option<serde_json::Value>, String> {
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
            data_tmpfs_bytes: 64 * 1024 * 1024,
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
        observations: values.into_iter().map(|v| Ok(Some(v))).collect(),
        mutations: Vec::new(),
        mutation_error: false,
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
