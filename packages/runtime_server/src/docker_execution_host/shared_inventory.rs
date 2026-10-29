//! Host inventory for measured admission. Immutable ceilings remain in the resident ledger.
use super::*;
use crate::resident_capacity::{ResidentInventory, SharedHostInventory};
use hosted_execution::host_supervisor::{resources::ObservationClock, HostLaunchTarget};

pub(crate) fn collect(host: &str) -> Result<ResidentInventory, String> {
    collect_excluding(host, &[])
}

pub(crate) fn collect_excluding(
    host: &str,
    preparing: &[crate::resident_capacity::PreparingResidentSandbox],
) -> Result<ResidentInventory, String> {
    if !crate::shared_resident_admission::enabled()? {
        return observed_resident_sandboxes_excluding(preparing)
            .map(ResidentInventory::declarations);
    }
    let mut sandboxes = Vec::new();
    let mut targets = Vec::new();
    let mut filter = PreparingInventoryFilter::new(preparing);
    for id in crate::docker_engine::list_managed(None)? {
        let Some(facts) = inspect_container(&id)? else {
            continue;
        };
        let sandbox = observed_resident_sandbox(&facts)?;
        let label = |name: &str| {
            facts
                .labels
                .get(name)
                .cloned()
                .ok_or("shared_inventory_target_incomplete")
        };
        if facts.runtime != "runsc" {
            return Err("shared_inventory_runtime_rejected".into());
        }
        // A partial native startup may not answer resource RPCs yet. Its frozen
        // declaration remains fully charged until a later complete inventory.
        if filter.should_skip(&facts, &sandbox)? {
            continue;
        }
        let target = HostLaunchTarget {
            container_id: facts.id.clone(),
            environment_instance_id: label("centaeris.instance_id")?,
            environment_id: label("centaeris.environment_id")?,
            image_digest: facts.image.clone(),
            profile_digest: label("centaeris.environment_profile_digest")?,
        };
        target.validate()?;
        sandboxes.push(sandbox);
        targets.push(target);
    }
    let observation = crate::environment_supervisor::observe_resources(host, &targets)?;
    let headroom = crate::positive_u64_env(
        "RESIDENT_HOST_MEMORY_HEADROOM_BYTES",
        (observation.mem_total_bytes / 5).max(512 * 1024 * 1024),
    )?;
    let clock = ObservationClock::from_host_kernel(5_000)?;
    Ok(ResidentInventory {
        sandboxes,
        shared: Some(SharedHostInventory::verified(
            observation,
            host,
            &targets,
            &clock,
            headroom,
        )?),
    })
}

#[cfg(test)]
mod tests;
