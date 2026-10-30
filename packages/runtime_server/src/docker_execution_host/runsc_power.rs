//! Observe and change only one durably pinned, already-created runsc environment.
use super::*;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use hosted_execution::host_supervisor::{HostLaunchTarget, IdlePauseOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunscPowerAction {
    Pause,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunscPowerObservation {
    Running,
    Paused,
    Stopped,
}

trait PowerIo {
    fn host_id(&mut self) -> Result<String, String>;
    fn inspect(&mut self, id: &str) -> Result<Option<serde_json::Value>, String>;
    fn change(&mut self, id: &str, action: RunscPowerAction) -> Result<(), String>;
    fn remove_stopped(&mut self, id: &str) -> Result<(), String>;
}

struct DockerPowerIo;
impl PowerIo for DockerPowerIo {
    fn remove_stopped(&mut self, id: &str) -> Result<(), String> {
        crate::docker_engine::remove_stopped(id)
    }
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
    observe_power_with(&mut DockerPowerIo, allocation, profile)
}

fn observe_power_with(
    io: &mut impl PowerIo,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<RunscPowerObservation, String> {
    let id = validate_target(io, allocation, profile)?;
    match observe_with(io, id, allocation, profile)? {
        RunscPowerObservation::Stopped => Err("runsc_power_container_stopped".into()),
        observed => Ok(observed),
    }
}

/// Change power while the caller holds the durable idle-or-demand authority pin.
/// Only an observed desired state acknowledges the operation. Residency remains charged.
pub(crate) fn set_power(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    action: RunscPowerAction,
) -> Result<RunscPowerObservation, String> {
    if action == RunscPowerAction::Pause {
        return match pause_if_idle(allocation, profile)? {
            IdlePauseOutcome::Paused => Ok(RunscPowerObservation::Paused),
            IdlePauseOutcome::Busy => Err("runsc_idle_pause_busy".into()),
            IdlePauseOutcome::Unknown { .. } => Err("runsc_idle_pause_unknown".into()),
        };
    }
    set_power_with(&mut DockerPowerIo, allocation, profile, action)
}

/// The host pins task admission and drain state atomically with physical pause.
pub(crate) fn pause_if_idle(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<IdlePauseOutcome, String> {
    pause_if_idle_with(
        &mut DockerPowerIo,
        allocation,
        profile,
        crate::environment_supervisor::pause_if_idle,
    )
}

fn pause_if_idle_with(
    io: &mut impl PowerIo,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    pause: impl FnOnce(HostLaunchTarget) -> Result<IdlePauseOutcome, String>,
) -> Result<IdlePauseOutcome, String> {
    let id = validate_target(io, allocation, profile)?;
    match observe_with(io, id, allocation, profile)? {
        RunscPowerObservation::Paused => return Ok(IdlePauseOutcome::Paused),
        RunscPowerObservation::Stopped => return Err("runsc_power_container_stopped".into()),
        RunscPowerObservation::Running => {}
    }
    let outcome = pause(HostLaunchTarget {
        environment_instance_id: allocation.instance_id.clone(),
        container_id: id.into(),
        environment_id: allocation.environment_id.clone(),
        profile_digest: allocation.profile_digest.clone(),
        image_digest: allocation.image_digest.clone(),
    })?;
    if outcome == IdlePauseOutcome::Paused
        && observe_with(io, id, allocation, profile)? != RunscPowerObservation::Paused
    {
        return Err("runsc_idle_pause_not_observed".into());
    }
    Ok(outcome)
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
    let observed = observe_with(io, id, allocation, profile)?;
    if observed == RunscPowerObservation::Stopped {
        return Err("runsc_power_container_stopped".into());
    }
    if observed == desired {
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

pub(super) fn physical_state(
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
    match (
        facts["State"]["Running"].as_bool(),
        facts["State"]["Paused"].as_bool(),
        facts["State"]["Restarting"].as_bool(),
        facts["State"]["Dead"].as_bool(),
    ) {
        (Some(true), Some(true), Some(false), Some(false)) => Ok(RunscPowerObservation::Paused),
        (Some(true), Some(false), Some(false), Some(false)) => Ok(RunscPowerObservation::Running),
        (Some(false), Some(false), Some(false), Some(false)) => Ok(RunscPowerObservation::Stopped),
        _ => Err("runsc_power_observation_unknown".into()),
    }
}

/// Inspect only the original host/container under the caller's recovery authority pin.
/// Absence means an exact-ID Docker 404; inventory omission is never consulted.
pub(crate) fn observe_recovery_target(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<Option<RunscPowerObservation>, String> {
    observe_recovery_target_with(&mut DockerPowerIo, allocation, profile)
}

fn observe_recovery_target_with(
    io: &mut impl PowerIo,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
) -> Result<Option<RunscPowerObservation>, String> {
    let id = validate_target(io, allocation, profile)?;
    io.inspect(id)
        .map_err(|_| "runsc_power_observation_unknown".to_string())?
        .map(|facts| physical_state(&facts, allocation, profile))
        .transpose()
}

/// Remove an exact stopped original only after the caller commits its file baseline.
/// The caller must hold current cleanup authority and the original allocation pin
/// throughout preservation and removal. This does not change the environment mode.
pub(crate) fn remove_stopped_after_preservation(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    preserve: impl FnOnce() -> Result<(), String>,
) -> Result<crate::postgres_store::cleanup_receipts::CleanupPhysicalResult, String> {
    remove_stopped_after_preservation_with(&mut DockerPowerIo, allocation, profile, preserve)
}

fn remove_stopped_after_preservation_with(
    io: &mut impl PowerIo,
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    preserve: impl FnOnce() -> Result<(), String>,
) -> Result<crate::postgres_store::cleanup_receipts::CleanupPhysicalResult, String> {
    let id = validate_target(io, allocation, profile)?;
    let removed = || crate::postgres_store::cleanup_receipts::CleanupPhysicalResult::Removed {
        instance_id: allocation.instance_id.clone(),
        container_id: id.into(),
    };
    match observe_recovery_target_with(io, allocation, profile)? {
        None => return Ok(removed()),
        Some(RunscPowerObservation::Stopped) => {}
        Some(_) => return Err("cleanup_stopped_target_required".into()),
    }
    preserve()?;
    // Preservation can take time. Revalidate immutable host/configuration and
    // power afterwards; a raced start must never be forcefully terminated.
    match observe_recovery_target_with(io, allocation, profile)? {
        None => return Ok(removed()),
        Some(RunscPowerObservation::Stopped) => {}
        Some(_) => return Err("cleanup_stopped_target_required".into()),
    }
    let _ = io.remove_stopped(id);
    if observe_recovery_target_with(io, allocation, profile)?.is_some() {
        return Err("cleanup_physical_absence_unconfirmed".into());
    }
    Ok(removed())
}
#[cfg(test)]
mod tests;
