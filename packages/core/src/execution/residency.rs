//! Resident resources remain charged while execution waits. A reservation is
//! durable before creation and is released only after the host confirms absence.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentResourceUsage {
    pub sandbox_count: u64,
    pub memory_bytes: u64,
    pub cpu_milli: u64,
    pub pids: u64,
    /// A separate workspace-space ceiling. It is already charged to the host's
    /// memory cgroup and must not be added to memory_bytes a second time.
    pub workspace_bytes: u64,
}

impl ResidentResourceUsage {
    pub fn checked_add(self, other: Self) -> Result<Self, String> {
        let add = |left: u64, right: u64| {
            left.checked_add(right)
                .ok_or_else(|| "resident resource accounting overflow".to_string())
        };
        Ok(Self {
            sandbox_count: add(self.sandbox_count, other.sandbox_count)?,
            memory_bytes: add(self.memory_bytes, other.memory_bytes)?,
            cpu_milli: add(self.cpu_milli, other.cpu_milli)?,
            pids: add(self.pids, other.pids)?,
            workspace_bytes: add(self.workspace_bytes, other.workspace_bytes)?,
        })
    }

    pub fn fits(self, limit: Self) -> bool {
        self.sandbox_count <= limit.sandbox_count
            && self.memory_bytes <= limit.memory_bytes
            && self.cpu_milli <= limit.cpu_milli
            && self.pids <= limit.pids
            && self.workspace_bytes <= limit.workspace_bytes
    }
}

pub fn admit_resident_resources(
    global_used: ResidentResourceUsage,
    tenant_used: ResidentResourceUsage,
    requested: ResidentResourceUsage,
    global_limit: ResidentResourceUsage,
    tenant_limit: ResidentResourceUsage,
) -> Result<bool, String> {
    if requested.sandbox_count != 1
        || requested.memory_bytes == 0
        || requested.cpu_milli == 0
        || requested.pids == 0
        || requested.workspace_bytes == 0
    {
        return Err("resident reservation must describe one bounded sandbox".into());
    }
    Ok(global_used.checked_add(requested)?.fits(global_limit)
        && tenant_used.checked_add(requested)?.fits(tenant_limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_usage() -> ResidentResourceUsage {
        ResidentResourceUsage {
            sandbox_count: 1,
            memory_bytes: 1024,
            cpu_milli: 1000,
            pids: 16,
            workspace_bytes: 512,
        }
    }

    #[test]
    fn waiting_residents_keep_all_declared_resources_charged() {
        let one = box_usage();
        let two = one.checked_add(one).unwrap();
        assert!(admit_resident_resources(one, one, one, two, two).unwrap());
        assert!(!admit_resident_resources(two, one, one, two, two).unwrap());
        assert!(!admit_resident_resources(one, one, one, two, one).unwrap());
        let mut memory_full = two;
        memory_full.sandbox_count = 20;
        assert!(!admit_resident_resources(
            two,
            ResidentResourceUsage::default(),
            one,
            memory_full,
            two
        )
        .unwrap());
        assert_eq!(
            two.memory_bytes, 2048,
            "workspace tmpfs must not be counted twice as memory"
        );
    }

    #[test]
    fn admission_checks_each_resource_and_rejects_accounting_overflow() {
        let one = box_usage();
        let two = one.checked_add(one).unwrap();
        for limit in [
            ResidentResourceUsage {
                memory_bytes: 1024,
                ..two
            },
            ResidentResourceUsage {
                cpu_milli: 1000,
                ..two
            },
            ResidentResourceUsage { pids: 16, ..two },
            ResidentResourceUsage {
                workspace_bytes: 512,
                ..two
            },
        ] {
            assert!(!admit_resident_resources(one, one, one, limit, two).unwrap());
        }
        assert!(admit_resident_resources(
            ResidentResourceUsage {
                memory_bytes: u64::MAX,
                ..one
            },
            one,
            one,
            two,
            two
        )
        .is_err());
    }
}
