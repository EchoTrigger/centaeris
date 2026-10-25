//! Observe and change only one durably pinned, already-created runsc environment.
use super::*;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunscPowerAction {
    Pause,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunscPowerObservation {
    Running,
    Paused,
}

trait PowerIo {
    fn host_id(&mut self) -> Result<String, String>;
    fn inspect(&mut self, id: &str) -> Result<Option<serde_json::Value>, String>;
    fn change(&mut self, id: &str, action: RunscPowerAction) -> Result<(), String>;
}

struct DockerPowerIo;
impl PowerIo for DockerPowerIo {
    fn host_id(&mut self) -> Result<String, String> {
        resident_host_id()
    }
    fn inspect(&mut self, id: &str) -> Result<Option<serde_json::Value>, String> {
        crate::docker_engine::inspect(id)
    }
    fn change(&mut self, id: &str, action: RunscPowerAction) -> Result<(), String> {
        match action {
            RunscPowerAction::Pause => crate::docker_engine::pause(id),
            RunscPowerAction::Resume => crate::docker_engine::unpause(id),
        }
    }
}

/// Observe the original physical container without changing its power state.
/// The caller pins the authoritative allocation and lifecycle owner during this call.
pub(crate) fn observe_power(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<RunscPowerObservation, String> {
    let mut io = DockerPowerIo;
    let id = validate_target(&mut io, allocation, profile)?;
    observe_with(&mut io, id, allocation, profile)
}

/// Change power while the caller holds the durable idle-or-demand authority pin.
/// Only an observed desired state acknowledges the operation. Residency remains charged.
pub(crate) fn set_power(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    action: RunscPowerAction,
) -> Result<RunscPowerObservation, String> {
    set_power_with(&mut DockerPowerIo, allocation, profile, action)
}

fn validate_target<'a>(
    io: &mut impl PowerIo,
    allocation: &'a RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<&'a str, String> {
    let legacy = profile;
    profile
        .validate()
        .map_err(|_| "runsc_power_profile_rejected")?;
    let id = allocation
        .container_id
        .as_deref()
        .ok_or("runsc_power_container_missing")?;
    if id.len() != 64
        || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || allocation.image_digest != legacy.image_digest
        || profile.digest()? != allocation.profile_digest
    {
        return Err("runsc_power_allocation_rejected".into());
    }
    if io.host_id().map_err(|_| "runsc_power_host_unavailable")? != allocation.host_id {
        return Err("runsc_power_host_rejected".into());
    }
    Ok(id)
}

fn observe_with(
    io: &mut impl PowerIo,
    id: &str,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<RunscPowerObservation, String> {
    let facts = io
        .inspect(id)
        .map_err(|_| "runsc_power_observation_unknown")?
        .ok_or("runsc_power_container_missing")?;
    physical_state(&facts, allocation, profile)
}

fn set_power_with(
    io: &mut impl PowerIo,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    action: RunscPowerAction,
) -> Result<RunscPowerObservation, String> {
    let id = validate_target(io, allocation, profile)?;
    let desired = match action {
        RunscPowerAction::Pause => RunscPowerObservation::Paused,
        RunscPowerAction::Resume => RunscPowerObservation::Running,
    };
    if observe_with(io, id, allocation, profile)? == desired {
        return Ok(desired);
    }
    // Even a transport error can follow a completed mutation. Conversely, an ACK
    // does not prove that the original physical container reached the new state.
    let changed = io.change(id, action);
    let observed = observe_with(io, id, allocation, profile)?;
    if observed == desired {
        Ok(observed)
    } else if changed.is_err() {
        Err("runsc_power_change_unknown".into())
    } else {
        Err("runsc_power_change_unconfirmed".into())
    }
}

fn physical_state(
    facts: &serde_json::Value,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<RunscPowerObservation, String> {
    let labels = &facts["Config"]["Labels"];
    let owner = labels["centaeris.allocation_owner"]
        .as_str()
        .and_then(|value| {
            serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(value).ok()
        });
    if facts["Id"].as_str() != allocation.container_id.as_deref()
        || facts["Image"].as_str() != Some(allocation.image_digest.as_str())
        || facts["HostConfig"]["Runtime"].as_str() != Some("runsc")
        || facts["State"]["Running"] != true
        || facts["State"]["Restarting"] != false
        || facts["State"]["Dead"] != false
        || facts["Config"]["User"].as_str() != Some(profile.user.as_str())
        || facts["Config"]["WorkingDir"].as_str() != Some("/")
        || facts["HostConfig"]["Memory"].as_u64() != Some(profile.resources.memory_bytes)
        || facts["HostConfig"]["CpuQuota"].as_u64()
            != Some(u64::from(profile.resources.cpu_milli) * 100)
        || facts["HostConfig"]["CpuPeriod"] != 100000
        || facts["HostConfig"]["PidsLimit"].as_u64()
            != Some(u64::from(profile.resources.pids_limit))
        || facts["HostConfig"]["NetworkMode"].as_str() != Some(network::mode(profile.network))
        || labels["centaeris.agent_run_id"].as_str()
            != allocation.allocation_agent_run_id.as_deref()
        || owner.as_ref() != Some(&allocation.allocation_owner)
        || [
            ("centaeris.managed", "true"),
            (
                "centaeris.environment_id",
                allocation.environment_id.as_str(),
            ),
            ("centaeris.instance_id", allocation.instance_id.as_str()),
            (
                "centaeris.execution_id",
                allocation.allocation_execution_id.as_str(),
            ),
            (
                "centaeris.environment_authorization_digest",
                allocation.binding_digest.as_str(),
            ),
            (
                "centaeris.environment_profile_digest",
                allocation.profile_digest.as_str(),
            ),
        ]
        .iter()
        .any(|(key, value)| labels[key].as_str() != Some(*value))
    {
        return Err("runsc_power_physical_identity_rejected".into());
    }
    match facts["State"]["Paused"].as_bool() {
        Some(true) => Ok(RunscPowerObservation::Paused),
        Some(false) => Ok(RunscPowerObservation::Running),
        None => Err("runsc_power_observation_unknown".into()),
    }
}

#[cfg(test)]
mod tests;
