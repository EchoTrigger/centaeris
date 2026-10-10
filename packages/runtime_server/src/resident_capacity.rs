use centaeris_core::execution::ResidentResourceUsage;

use crate::agent_run_authorization::SandboxResources;

pub(crate) const RESIDENT_CAPACITY_WAIT: &str = "execution_resident_capacity_wait";

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
            tenant: vector(4, 8, 4_000, 2048),
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
                // Workspace tmpfs consumes cgroup memory. This is a separate
                // space ceiling, capped by available RAM without summing twice.
                workspace_bytes: self.host.workspace_bytes.min(memory_available),
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
    pub agent_run_id: String,
    pub container_id: String,
    pub resources: ResidentResourceUsage,
}

pub(crate) struct ResidentSandboxRequest<'a> {
    pub execution_id: &'a str,
    pub agent_run_id: &'a str,
    pub workspace_id: &'a str,
    pub lifecycle_job_id: &'a str,
    pub lifecycle_lease_owner: &'a str,
    pub must_exist: bool,
    pub resources: ResidentResourceUsage,
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
        assert_eq!(effective.host.workspace_bytes, 3 * 1024);
        assert_eq!(effective.host.cpu_milli, 1800);
        assert!(configured.bound_to_host(1024, 2, 1024, 200).is_err());
        assert!(configured.bound_to_host(4 * 1024, 1, 1024, 1000).is_err());
    }
}
