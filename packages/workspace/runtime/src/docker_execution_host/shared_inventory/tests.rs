//! Exercise the exact identity boundary before active preparations can be omitted.
use super::*;
use crate::resident_capacity::PreparingResidentSandbox;
use centaeris_core::execution::{EnvironmentTaskOwner, ResidentResourceUsage};

fn fixture() -> (ContainerFacts, PreparingResidentSandbox) {
    let owner = EnvironmentTaskOwner::AgentRun {
        run_id: "run-active".into(),
    };
    let resources = ResidentResourceUsage {
        sandbox_count: 1,
        memory_bytes: 1024,
        cpu_milli: 1000,
        pids: 64,
        workspace_bytes: 4096,
    };
    let pending = PreparingResidentSandbox {
        execution_id: "execution-active".into(),
        allocation_owner: owner.clone(),
        instance_id: "instance-active".into(),
        environment_id: "environment-active".into(),
        image_digest: format!("sha256:{}", "a".repeat(64)),
        profile_digest: format!("sha256:{}", "b".repeat(64)),
        container_id: None,
        resources,
    };
    let facts = ContainerFacts {
        id: "c".repeat(64),
        raw: serde_json::Value::Null,
        running: false,
        image: pending.image_digest.clone(),
        runtime: "runsc".into(),
        labels: HashMap::from([
            ("centaeris.managed".into(), "true".into()),
            (
                "centaeris.execution_id".into(),
                pending.execution_id.clone(),
            ),
            (
                "centaeris.allocation_owner".into(),
                serde_json::to_string(&owner).unwrap(),
            ),
            ("centaeris.instance_id".into(), pending.instance_id.clone()),
            (
                "centaeris.environment_id".into(),
                pending.environment_id.clone(),
            ),
            (
                "centaeris.environment_profile_digest".into(),
                pending.profile_digest.clone(),
            ),
            (
                "centaeris.data_budget".into(),
                resources.workspace_bytes.to_string(),
            ),
        ]),
        memory: resources.memory_bytes,
        cpu_period: 100_000,
        cpu_quota: 100_000,
        pids_limit: Some(resources.pids as i64),
        readonly_rootfs: false,
        network_mode: "none".into(),
        mounts: Vec::new(),
    };
    (facts, pending)
}

#[test]
fn active_preparation_is_omitted_only_for_its_exact_owned_incarnation() {
    let (facts, mut pending) = fixture();
    let sandbox = observed_resident_sandbox(&facts).unwrap();
    assert!(preparing_resident(&facts, &sandbox, &[pending.clone()]).unwrap());
    pending.container_id = Some(facts.id.clone());
    assert!(preparing_resident(&facts, &sandbox, &[pending]).unwrap());
}

#[test]
fn no_active_preparer_keeps_the_sandbox_visible_for_fresh_adoption() {
    let (facts, _) = fixture();
    let sandbox = observed_resident_sandbox(&facts).unwrap();
    assert!(!preparing_resident(&facts, &sandbox, &[]).unwrap());
}

#[test]
fn active_preparation_identity_or_resource_mismatch_fails_closed() {
    for change in [
        "owner",
        "instance",
        "environment",
        "image",
        "profile",
        "memory",
        "cpu",
        "pids",
        "workspace",
        "knownContainer",
    ] {
        let (mut facts, mut pending) = fixture();
        match change {
            "owner" => {
                facts.labels.insert(
                    "centaeris.allocation_owner".into(),
                    serde_json::to_string(&EnvironmentTaskOwner::MaintenanceOperation {
                        operation_id: "run-active".into(),
                    })
                    .unwrap(),
                );
            }
            "instance" => {
                facts
                    .labels
                    .insert("centaeris.instance_id".into(), "other-instance".into());
            }
            "environment" => {
                facts.labels.insert(
                    "centaeris.environment_id".into(),
                    "other-environment".into(),
                );
            }
            "image" => facts.image = format!("sha256:{}", "d".repeat(64)),
            "profile" => {
                facts.labels.insert(
                    "centaeris.environment_profile_digest".into(),
                    format!("sha256:{}", "e".repeat(64)),
                );
            }
            "memory" => facts.memory += 1,
            "cpu" => facts.cpu_quota += 100,
            "pids" => facts.pids_limit = Some(resources_pids(&pending) + 1),
            "workspace" => {
                facts.labels.insert(
                    "centaeris.data_budget".into(),
                    (pending.resources.workspace_bytes + 1).to_string(),
                );
            }
            "knownContainer" => pending.container_id = Some("f".repeat(64)),
            _ => unreachable!(),
        }
        let sandbox = observed_resident_sandbox(&facts).unwrap();
        assert!(
            preparing_resident(&facts, &sandbox, &[pending]).is_err(),
            "mismatched {change} was hidden as an active preparation"
        );
    }
}

fn resources_pids(pending: &PreparingResidentSandbox) -> i64 {
    pending.resources.pids.try_into().unwrap()
}

#[test]
fn equal_instance_label_does_not_hide_a_conflicting_execution() {
    let (mut facts, pending) = fixture();
    facts
        .labels
        .insert("centaeris.execution_id".into(), "foreign-execution".into());
    let sandbox = observed_resident_sandbox(&facts).unwrap();
    assert_eq!(facts.labels["centaeris.instance_id"], pending.instance_id);
    assert!(
        !preparing_resident(&facts, &sandbox, &[pending]).unwrap(),
        "a foreign execution must remain visible to normal reconciliation"
    );
}

#[test]
fn inventory_filter_preserves_single_exact_preparation_with_or_without_known_container() {
    for known_container in [false, true] {
        let (facts, mut pending) = fixture();
        if known_container {
            pending.container_id = Some(facts.id.clone());
        }
        let sandbox = observed_resident_sandbox(&facts).unwrap();
        let mut filter = PreparingInventoryFilter::new(std::slice::from_ref(&pending));
        assert!(filter.should_skip(&facts, &sandbox).unwrap());
    }
}

#[test]
fn one_inventory_rejects_distinct_containers_with_the_same_preparing_identity() {
    let (first, pending) = fixture();
    let (mut second, _) = fixture();
    second.id = "d".repeat(64);
    let first_sandbox = observed_resident_sandbox(&first).unwrap();
    let second_sandbox = observed_resident_sandbox(&second).unwrap();
    assert_eq!(first_sandbox.execution_id, second_sandbox.execution_id);
    assert_eq!(
        first_sandbox.allocation_owner,
        second_sandbox.allocation_owner
    );
    assert_ne!(first_sandbox.container_id, second_sandbox.container_id);
    let mut filter = PreparingInventoryFilter::new(std::slice::from_ref(&pending));
    assert!(filter.should_skip(&first, &first_sandbox).unwrap());
    assert!(
        filter.should_skip(&second, &second_sandbox).is_err(),
        "one pending declaration cannot hide two distinct physical containers"
    );
}
