use centaeris_core::execution::ResidentResourceUsage;

use crate::agent_run_authorization::SandboxResources;

pub(crate) const RESIDENT_CAPACITY_WAIT: &str = "execution_resident_capacity_wait";
pub(crate) const HOST_PREPARATION_WAIT: &str = "execution_host_preparation_wait";

#[derive(Clone, Copy)]
pub(crate) struct ResidentCapacity {
    pub global: ResidentResourceUsage,
    pub tenant: ResidentResourceUsage,
    pub host: ResidentResourceUsage,
}

impl Default for ResidentCapacity {
    fn default() -> Self {
        let vector = |count: u64, gib: u64, cpu: u64, pids: u64| ResidentResourceUsage {
            sandbox_count: count,
            memory_bytes: gib * 1024 * 1024 * 1024,
            cpu_milli: cpu,
            pids,
            workspace_bytes: gib * 1024 * 1024 * 1024,
        };
        let global = vector(16, 32, 16_000, 8192);
        Self {
            global,
            tenant: ResidentResourceUsage {
                workspace_bytes: 16 * 1024 * 1024 * 1024,
                ..vector(4, 8, 4_000, 2048)
            },
            host: global,
        }
    }
}

impl ResidentCapacity {
    pub fn from_env() -> Result<Self, String> {
        let defaults = Self::default();
        let limits = |prefix: &str,
                      defaults: ResidentResourceUsage|
         -> Result<ResidentResourceUsage, String> {
            Ok(ResidentResourceUsage {
                sandbox_count: crate::positive_u64_env(
                    &format!("{prefix}_COUNT"),
                    defaults.sandbox_count,
                )?,
                memory_bytes: crate::positive_u64_env(
                    &format!("{prefix}_MEMORY_BYTES"),
                    defaults.memory_bytes,
                )?,
                cpu_milli: crate::positive_u64_env(
                    &format!("{prefix}_CPU_MILLI"),
                    defaults.cpu_milli,
                )?,
                pids: crate::positive_u64_env(&format!("{prefix}_PIDS"), defaults.pids)?,
                workspace_bytes: crate::positive_u64_env(
                    &format!("{prefix}_WORKSPACE_BYTES"),
                    defaults.workspace_bytes,
                )?,
            })
        };
        let global = limits("RESIDENT_GLOBAL", defaults.global)?;
        Ok(Self {
            global,
            tenant: limits("RESIDENT_TENANT", defaults.tenant)?,
            host: global,
        })
    }

    pub fn bound_to_host(
        self,
        memory_bytes: u64,
        cpu_count: u64,
        memory_headroom: u64,
        cpu_headroom_milli: u64,
    ) -> Result<Self, String> {
        let memory_available = memory_bytes
            .checked_sub(memory_headroom)
            .filter(|value| *value > 0)
            .ok_or("resident host memory headroom leaves no sandbox capacity")?;
        let cpu_available = cpu_count
            .checked_mul(1000)
            .and_then(|value| value.checked_sub(cpu_headroom_milli))
            .filter(|value| *value > 0)
            .ok_or("resident host CPU headroom leaves no sandbox capacity")?;
        Ok(Self {
            host: ResidentResourceUsage {
                memory_bytes: self.host.memory_bytes.min(memory_available),
                cpu_milli: self.host.cpu_milli.min(cpu_available),
                // Native workspace storage is independent of host RAM. Unknown
                // usage keeps its full declared disk budget during admission.
                workspace_bytes: self.host.workspace_bytes,
                ..self.host
            },
            global: self.global,
            tenant: self.tenant,
        })
    }
}

pub(crate) fn declared_usage(resources: SandboxResources) -> ResidentResourceUsage {
    ResidentResourceUsage {
        sandbox_count: 1,
        memory_bytes: resources.memory_bytes,
        cpu_milli: u64::from(resources.cpu_milli),
        pids: u64::from(resources.pids_limit),
        workspace_bytes: resources.data_tmpfs_bytes,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ObservedResidentSandbox {
    pub execution_id: String,
    pub allocation_owner: centaeris_core::execution::EnvironmentTaskOwner,
    pub container_id: String,
    pub resources: ResidentResourceUsage,
}

/// A trusted, identity-checked host sample for one admission attempt.
#[derive(Clone, Debug)]
pub(crate) struct SharedHostInventory {
    pub host_id: String,
    pub available_memory_bytes: u64,
    pub memory_headroom_bytes: u64,
    pub valid_until: std::time::Instant,
    pub measured_by_container:
        std::collections::BTreeMap<String, crate::shared_resident_admission::MeasuredUsage>,
}

impl SharedHostInventory {
    pub(crate) fn ensure_fresh(&self, expected_host: &str) -> Result<(), String> {
        if self.host_id != expected_host {
            return Err("shared resident host identity mismatch".into());
        }
        if std::time::Instant::now() >= self.valid_until {
            return Err(RESIDENT_CAPACITY_WAIT.into());
        }
        Ok(())
    }

    pub(crate) fn verified(
        observation: hosted_execution::host_supervisor::resources::HostResourceObservation,
        expected_host: &str,
        targets: &[hosted_execution::host_supervisor::HostLaunchTarget],
        clock: &hosted_execution::host_supervisor::resources::ObservationClock,
        memory_headroom_bytes: u64,
    ) -> Result<Self, String> {
        use hosted_execution::host_supervisor::resources::{
            ContainerResourceObservation, MeasuredBytes,
        };
        let validation_started = std::time::Instant::now();
        observation.validate_for(expected_host, targets, clock)?;
        let sample_start_ms = observation.observed_at_boot_ms - observation.collection_duration_ms;
        let remaining_ms = clock.max_age_ms - (clock.now_boot_ms - sample_start_ms);
        let valid_until = validation_started
            .checked_add(std::time::Duration::from_millis(remaining_ms))
            .ok_or("shared_resource_expiry_overflow")?;
        let mut measured_by_container = std::collections::BTreeMap::new();
        for entry in observation.containers {
            if let ContainerResourceObservation::Observed { resources } = entry {
                let workspace_upper_bound_bytes = match resources.workspace_usage {
                    MeasuredBytes::Observed { bytes } | MeasuredBytes::UpperBound { bytes, .. } => {
                        Some(bytes)
                    }
                    MeasuredBytes::Unknown { .. } => None,
                };
                let usage = crate::shared_resident_admission::MeasuredUsage {
                    memory_current_bytes: resources.memory_current_bytes,
                    swap_current_bytes: resources.memory_swap_current_bytes,
                    pids_current: resources.pids_current,
                    workspace_upper_bound_bytes,
                };
                if measured_by_container
                    .insert(resources.target.container_id, usage)
                    .is_some()
                {
                    return Err("duplicate shared resource container".into());
                }
            }
        }
        Ok(Self {
            host_id: expected_host.into(),
            available_memory_bytes: observation.mem_available_bytes,
            memory_headroom_bytes,
            valid_until,
            measured_by_container,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResidentInventory {
    pub sandboxes: Vec<ObservedResidentSandbox>,
    pub shared: Option<SharedHostInventory>,
}
impl ResidentInventory {
    pub(crate) fn declarations(sandboxes: Vec<ObservedResidentSandbox>) -> Self {
        Self {
            sandboxes,
            shared: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_resident_limits_cannot_overbook_a_smaller_daemon() {
        let configured = ResidentCapacity {
            global: ResidentResourceUsage {
                sandbox_count: 16,
                memory_bytes: 32 * 1024,
                cpu_milli: 16_000,
                pids: 8192,
                workspace_bytes: 32 * 1024,
            },
            tenant: ResidentResourceUsage {
                sandbox_count: 4,
                memory_bytes: 8 * 1024,
                cpu_milli: 4_000,
                pids: 2048,
                workspace_bytes: 8 * 1024,
            },
            host: ResidentResourceUsage {
                sandbox_count: 16,
                memory_bytes: 32 * 1024,
                cpu_milli: 16_000,
                pids: 8192,
                workspace_bytes: 32 * 1024,
            },
        };
        let effective = configured.bound_to_host(4 * 1024, 2, 1024, 200).unwrap();
        assert_eq!(effective.host.memory_bytes, 3 * 1024);
        assert_eq!(effective.host.workspace_bytes, 32 * 1024);
        assert_eq!(effective.host.cpu_milli, 1800);
        assert!(configured.bound_to_host(1024, 2, 1024, 200).is_err());
        assert!(configured.bound_to_host(4 * 1024, 1, 1024, 1000).is_err());
    }
    #[test]
    fn workspace_disk_budget_is_independent_of_host_ram_headroom() {
        let configured = ResidentCapacity::default();
        let effective = configured
            .bound_to_host(16 * 1024 * 1024 * 1024, 8, 3 * 1024 * 1024 * 1024, 800)
            .unwrap();
        assert_eq!(
            effective.host.workspace_bytes, configured.host.workspace_bytes,
            "native data budget is not a tmpfs RAM reservation"
        );
        assert_eq!(effective.host.memory_bytes, 13 * 1024 * 1024 * 1024);
    }
    #[test]
    fn shared_sample_expiry_includes_collection_duration_and_existing_age() {
        use hosted_execution::host_supervisor::resources::{
            HostResourceObservation, ObservationClock,
        };
        let pressure = serde_json::json!({"some":{"avg10":0.0,"avg60":0.0,"avg300":0.0,"totalMicros":0},"full":null});
        let boot = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let observation: HostResourceObservation = serde_json::from_value(serde_json::json!({
            "schema":"native.host.resources.v1", "hostId":"host", "bootId":boot,
            "observedAtBootMs":1000,"collectionDurationMs":200,
            "memTotalBytes":16*1024_u64*1024*1024,"memAvailableBytes":10*1024_u64*1024*1024,
            "cpuCount":8,"memoryPressure":pressure,"cpuPressure":pressure,"containers":[]
        }))
        .unwrap();
        let mut clock = ObservationClock {
            boot_id: boot.into(),
            now_boot_ms: 1100,
            max_age_ms: 300,
        };
        let expired = SharedHostInventory::verified(
            observation.clone(),
            "host",
            &[],
            &clock,
            2 * 1024 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(
            expired.ensure_fresh("host"),
            Err(RESIDENT_CAPACITY_WAIT.into())
        );
        clock.max_age_ms = 5300;
        let fresh =
            SharedHostInventory::verified(observation, "host", &[], &clock, 2 * 1024 * 1024 * 1024)
                .unwrap();
        assert!(fresh.ensure_fresh("host").is_ok());
        assert!(fresh.ensure_fresh("foreign-host").is_err());
    }
}
