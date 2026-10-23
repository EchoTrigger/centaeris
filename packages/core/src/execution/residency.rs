//! Resident resources remain charged while execution waits. A reservation is
//! durable before creation and is released only with durable settlement evidence.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentResourceUsage {
    pub sandbox_count: u64,
    pub memory_bytes: u64,
    pub cpu_milli: u64,
    pub pids: u64,
    /// A separate workspace-space allowance. When backed by tmpfs, its memory
    /// is already charged to the host cgroup; do not add it to memory_bytes.
    /// Physical storage enforcement remains a host obligation.
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

/// Immutable admission terms, distinct from the host-enforced resource limits.
/// Observed usage does not alter this commitment or release a physical holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResidentResourceClaim {
    limits: ResidentResourceUsage,
    reservation: ResidentResourceUsage,
}

impl ResidentResourceClaim {
    /// Memory and CPU commitments may be below their ceilings. CPU may be
    /// best effort; memory must retain a positive admission commitment. Count,
    /// PID and workspace-space ceilings remain fully reserved by this contract.
    /// This does not configure cgroups or grant execution authority.
    pub fn new(
        limits: ResidentResourceUsage,
        reservation: ResidentResourceUsage,
    ) -> Result<Self, String> {
        if limits.sandbox_count != 1
            || limits.memory_bytes == 0
            || limits.cpu_milli == 0
            || limits.pids == 0
            || limits.workspace_bytes == 0
            || reservation.sandbox_count != 1
            || reservation.memory_bytes == 0
            || !reservation.fits(limits)
            || reservation.pids != limits.pids
            || reservation.workspace_bytes != limits.workspace_bytes
        {
            return Err("resident claim must bind bounded limits and a valid reservation".into());
        }
        Ok(Self {
            limits,
            reservation,
        })
    }

    pub fn fully_reserved(limits: ResidentResourceUsage) -> Result<Self, String> {
        Self::new(limits, limits)
    }

    pub fn limits(self) -> ResidentResourceUsage {
        self.limits
    }

    pub fn reservation(self) -> ResidentResourceUsage {
        self.reservation
    }
}

pub fn admit_resident_resources(
    global_used: ResidentResourceUsage,
    tenant_used: ResidentResourceUsage,
    requested: ResidentResourceClaim,
    global_limit: ResidentResourceUsage,
    tenant_limit: ResidentResourceUsage,
) -> Result<bool, String> {
    let requested = requested.reservation();
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

    fn full(limits: ResidentResourceUsage) -> ResidentResourceClaim {
        ResidentResourceClaim::fully_reserved(limits).unwrap()
    }

    #[test]
    fn shared_admission_charges_the_commitment_and_preserves_the_limits() {
        let reservation = box_usage();
        let limits = ResidentResourceUsage {
            memory_bytes: reservation.memory_bytes * 8,
            cpu_milli: reservation.cpu_milli * 4,
            ..reservation
        };
        let claim = ResidentResourceClaim::new(limits, reservation).unwrap();
        let budget = reservation.checked_add(reservation).unwrap();
        assert!(admit_resident_resources(reservation, reservation, claim, budget, budget).unwrap());
        assert_eq!(claim.limits(), limits);
        assert_eq!(claim.reservation(), reservation);
        assert!(!admit_resident_resources(budget, reservation, claim, budget, budget).unwrap());
        assert!(!admit_resident_resources(reservation, budget, claim, budget, budget).unwrap());
    }

    #[test]
    fn claims_reject_unbounded_or_inconsistent_commitments() {
        let limits = box_usage();
        for reservation in [
            ResidentResourceUsage {
                sandbox_count: 0,
                ..limits
            },
            ResidentResourceUsage {
                sandbox_count: 2,
                ..limits
            },
            ResidentResourceUsage {
                memory_bytes: 0,
                ..limits
            },
            ResidentResourceUsage {
                memory_bytes: limits.memory_bytes + 1,
                ..limits
            },
            ResidentResourceUsage {
                cpu_milli: limits.cpu_milli + 1,
                ..limits
            },
            ResidentResourceUsage {
                pids: limits.pids - 1,
                ..limits
            },
            ResidentResourceUsage {
                pids: limits.pids + 1,
                ..limits
            },
            ResidentResourceUsage {
                workspace_bytes: limits.workspace_bytes - 1,
                ..limits
            },
        ] {
            assert!(ResidentResourceClaim::new(limits, reservation).is_err());
        }
        for invalid_limits in [
            ResidentResourceUsage {
                sandbox_count: 0,
                ..limits
            },
            ResidentResourceUsage {
                sandbox_count: 2,
                ..limits
            },
            ResidentResourceUsage {
                memory_bytes: 0,
                ..limits
            },
            ResidentResourceUsage {
                cpu_milli: 0,
                ..limits
            },
            ResidentResourceUsage { pids: 0, ..limits },
            ResidentResourceUsage {
                workspace_bytes: 0,
                ..limits
            },
        ] {
            assert!(ResidentResourceClaim::fully_reserved(invalid_limits).is_err());
        }
        let best_effort_cpu = ResidentResourceUsage {
            cpu_milli: 0,
            ..limits
        };
        assert_eq!(
            ResidentResourceClaim::new(limits, best_effort_cpu)
                .unwrap()
                .reservation()
                .cpu_milli,
            0
        );
    }

    #[test]
    fn waiting_residents_keep_all_declared_resources_charged() {
        let one = box_usage();
        let two = one.checked_add(one).unwrap();
        assert!(admit_resident_resources(one, one, full(one), two, two).unwrap());
        assert!(!admit_resident_resources(two, one, full(one), two, two).unwrap());
        assert!(!admit_resident_resources(one, one, full(one), two, one).unwrap());
        let mut memory_full = two;
        memory_full.sandbox_count = 20;
        assert!(!admit_resident_resources(
            two,
            ResidentResourceUsage::default(),
            full(one),
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
            assert!(!admit_resident_resources(one, one, full(one), limit, two).unwrap());
        }
        assert!(admit_resident_resources(
            ResidentResourceUsage {
                memory_bytes: u64::MAX,
                ..one
            },
            one,
            full(one),
            two,
            two
        )
        .is_err());
    }
}
