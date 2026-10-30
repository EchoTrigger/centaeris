use super::*;
use crate::environment_authorization::maintenance::MaintenanceBinding;
use crate::postgres_store::maintenance::MaintenanceTarget;
use centaeris_core::execution::{EnvironmentTaskEnvelope, EnvironmentTaskResult};
use hosted_execution::host_supervisor::HostLaunchTarget;

pub(crate) fn maintenance_task(
    binding: &MaintenanceBinding,
    target: &MaintenanceTarget,
    envelope: &EnvironmentTaskEnvelope,
) -> Result<EnvironmentTaskResult, String> {
    crate::environment_supervisor::task(launch_target(binding, target)?, envelope.clone())
}

pub(crate) fn maintenance_helper(
    binding: &MaintenanceBinding,
    target: &MaintenanceTarget,
    request: hosted_execution::host_supervisor::HostHelperRequest,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    let launch = launch_target(binding, target)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let task = crate::environment_supervisor::helper(
        &launch,
        request,
        &mut Cursor::new(input),
        &mut stdout,
        &mut stderr,
    )?;
    if task.exit_code != Some(0) {
        return Err("maintenance_helper_failed".into());
    }
    Ok(stdout)
}
fn launch_target(
    binding: &MaintenanceBinding,
    target: &MaintenanceTarget,
) -> Result<HostLaunchTarget, String> {
    if resident_host_context()?.0 != target.host_id {
        return Err("maintenance_host_rejected".into());
    }
    if crate::runsc_rotation::Config::from_env()?.is_some() {
        let allocation = target.allocation(binding);
        if runsc_power::set_power(
            &allocation,
            &target.profile,
            runsc_power::RunscPowerAction::Resume,
        )? != runsc_power::RunscPowerObservation::Running
        {
            return Err("maintenance_resume_unconfirmed".into());
        }
    }
    let facts = crate::docker_engine::inspect(&target.container_id)?
        .ok_or("maintenance_container_missing")?;
    if !physical_identity(&facts, binding, target) {
        return Err("maintenance_physical_identity_rejected".into());
    }
    Ok(HostLaunchTarget {
        environment_instance_id: binding.instance_id.clone(),
        container_id: target.container_id.clone(),
        environment_id: binding.environment_id.clone(),
        profile_digest: target.profile_digest.clone(),
        image_digest: target.image_digest.clone(),
    })
}

fn physical_identity(
    facts: &serde_json::Value,
    binding: &MaintenanceBinding,
    target: &MaintenanceTarget,
) -> bool {
    let profile = &target.profile;
    let labels = &facts["Config"]["Labels"];
    facts["Id"].as_str() == Some(&target.container_id)
        && facts["Image"].as_str() == Some(&target.image_digest)
        && facts["State"]["Running"] == true
        && facts["State"]["Paused"] == false
        && facts["Config"]["User"].as_str() == Some(&profile.user)
        && facts["Config"]["WorkingDir"].as_str() == Some("/")
        && facts["HostConfig"]["Memory"].as_u64() == Some(profile.resources.memory_bytes)
        && facts["HostConfig"]["CpuQuota"].as_u64()
            == Some(u64::from(profile.resources.cpu_milli) * 100)
        && facts["HostConfig"]["CpuPeriod"] == 100000
        && facts["HostConfig"]["PidsLimit"].as_u64()
            == Some(u64::from(profile.resources.pids_limit))
        && facts["HostConfig"]["NetworkMode"].as_str() == Some(network::mode(profile.network))
        && labels["centaeris.allocation_owner"]
            .as_str()
            .and_then(|v| {
                serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(v).ok()
            })
            .as_ref()
            == Some(&target.allocation_owner)
        && labels["centaeris.agent_run_id"].as_str() == target.allocation_agent_run_id.as_deref()
        && [
            ("centaeris.managed", "true"),
            ("centaeris.environment_id", binding.environment_id.as_str()),
            ("centaeris.instance_id", binding.instance_id.as_str()),
            (
                "centaeris.execution_id",
                target.allocation_execution_id.as_str(),
            ),
            (
                "centaeris.environment_authorization_digest",
                target.binding_digest.as_str(),
            ),
            (
                "centaeris.environment_profile_digest",
                target.profile_digest.as_str(),
            ),
        ]
        .iter()
        .all(|(name, value)| labels[name].as_str() == Some(*value))
}

pub(crate) fn provision_environment(
    binding: &crate::environment_authorization::provision::ProvisionBinding,
    allocation: &crate::postgres_store::execution_instances::RetainedInstanceAllocation,
) -> Result<String, String> {
    let profile = &binding.environment_profile;
    let helper = environment_container::helper_path()?;
    let plugin_volume = docker_volume_name_from_environment(PLUGIN_VOLUME_NAME_ENV)?;
    let mounts = plugin_mounts(&binding.plugin_activation)?;
    let memory = memory_mount(
        &binding.user_id,
        &binding.agent_id,
        docker_volume_name_from_environment(AGENT_MEMORY_VOLUME_NAME_ENV)?,
    )?;
    prepare_memory_scope(&memory)?;
    let name = container_name(&allocation.allocation_execution_id);
    let expected = ContainerExpectation {
        network: profile.network,
        environment_profile: Some(profile),
        benchmark_helper: Some(&helper),
        profile_digest: Some(&allocation.profile_digest),
        name: &name,
        agent_run_id: "",
        allocation_owner: Some(&allocation.allocation_owner),
        execution_id: &allocation.allocation_execution_id,
        environment_id: &allocation.environment_id,
        instance_id: &allocation.instance_id,
        binding_digest: &allocation.binding_digest,
        image_digest: &allocation.image_digest,
        oci_runtime: OciRuntime::from_environment()?,
        resources: profile.resources.sandbox_resources(),
        plugin_volume_name: &plugin_volume,
        plugin_mounts: &mounts,
    };
    ensure_container(&expected, allocation.reused)?;
    let facts = inspect_container(&name)?.ok_or("provision_container_missing")?;
    if allocation
        .container_id
        .as_ref()
        .is_some_and(|id| id != &facts.id)
        || !facts.matches(&expected)
    {
        return Err("provision_physical_identity_rejected".into());
    }
    let fence = binding.fence(&allocation.instance_id)?;
    retained::takeover(&expected, &facts.id, &fence)?;
    Ok(facts.id)
}

pub(crate) fn observe_environment(
    target: &crate::postgres_store::environment_observation::EnvironmentObservation,
) -> &'static str {
    let Some(a) = &target.allocation else {
        return if target.resource_holding {
            "unknown"
        } else {
            "unallocated"
        };
    };
    if target.not_materialized && !target.resource_holding {
        return "notMaterialized";
    }
    let Ok((host, _)) = resident_host_context() else {
        return "unknown";
    };
    if host != a.host_id {
        return "unknown";
    }
    let id = a
        .container_id
        .clone()
        .unwrap_or_else(|| container_name(&a.allocation_execution_id));
    let facts = match crate::docker_engine::inspect(&id) {
        Ok(facts) => facts,
        Err(_) => return "unknown",
    };
    if facts.is_none() {
        return if target.destroyed && !target.resource_holding {
            "destroyed"
        } else if target.holding_state.as_deref() == Some("reserved") {
            "allocationPending"
        } else {
            "unknown"
        };
    }
    let facts = facts.expect("checked");
    let labels = &facts["Config"]["Labels"];
    if a.container_id.as_deref() != facts["Id"].as_str()
        || facts["Image"].as_str() != Some(&a.image_digest)
        || facts["State"]["Running"] != true
        || labels["centaeris.environment_id"].as_str() != Some(&a.environment_id)
        || labels["centaeris.instance_id"].as_str() != Some(&a.instance_id)
        || labels["centaeris.environment_profile_digest"].as_str() != Some(&a.profile_digest)
        || labels["centaeris.allocation_owner"]
            .as_str()
            .and_then(|v| {
                serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(v).ok()
            })
            .as_ref()
            != Some(&a.allocation_owner)
    {
        return "unknown";
    }
    if target.holding_state.as_deref() != Some("resident") || !target.resource_holding {
        return "unknown";
    }
    if facts["State"]["Paused"] == true {
        return match runsc_power::observe_power(a, &target.profile) {
            Ok(runsc_power::RunscPowerObservation::Paused) => "paused",
            _ => "unknown",
        };
    }
    match crate::environment_supervisor::inspect(&a.instance_id) {
        Ok(inspection) if inspection["instanceId"].as_str() == Some(&a.instance_id) => "resident",
        _ => "unknown",
    }
}

pub(crate) fn remove_cleaned_environment(
    a: &crate::postgres_store::execution_instances::RetainedInstanceAllocation,
) -> Result<crate::postgres_store::cleanup_receipts::CleanupPhysicalResult, String> {
    let id = a
        .container_id
        .as_deref()
        .ok_or("cleanup_container_identity_missing")?;
    if let Some(facts) = crate::docker_engine::inspect(id)? {
        let labels = &facts["Config"]["Labels"];
        if facts["Id"].as_str() != Some(id)
            || facts["Image"].as_str() != Some(&a.image_digest)
            || labels["centaeris.managed"] != "true"
            || labels["centaeris.environment_id"].as_str() != Some(&a.environment_id)
            || labels["centaeris.instance_id"].as_str() != Some(&a.instance_id)
            || labels["centaeris.execution_id"].as_str() != Some(&a.allocation_execution_id)
            || labels["centaeris.environment_authorization_digest"].as_str()
                != Some(&a.binding_digest)
            || labels["centaeris.environment_profile_digest"].as_str() != Some(&a.profile_digest)
            || labels["centaeris.allocation_owner"]
                .as_str()
                .and_then(|v| {
                    serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(v).ok()
                })
                .as_ref()
                != Some(&a.allocation_owner)
        {
            return Err("cleanup_physical_identity_rejected".into());
        }
        let _ = crate::docker_engine::remove(id, false);
    }
    if crate::docker_engine::inspect(id)?.is_some() {
        return Err("cleanup_physical_absence_unconfirmed".into());
    }
    Ok(
        crate::postgres_store::cleanup_receipts::CleanupPhysicalResult::Removed {
            instance_id: a.instance_id.clone(),
            container_id: id.into(),
        },
    )
}
