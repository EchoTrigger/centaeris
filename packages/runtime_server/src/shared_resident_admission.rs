//! Hosted shared-resource admission; durable declarations retain their original meaning.
use crate::resident_capacity::ResidentCapacity;
use centaeris_core::execution::{admit_resident_resources, ResidentResourceUsage};

#[derive(Clone, Copy, Debug)]
pub(crate) struct MeasuredUsage {
    pub memory_current_bytes: u64,
    pub swap_current_bytes: u64,
    pub pids_current: u64,
    pub workspace_upper_bound_bytes: Option<u64>,
}

pub(crate) fn charge(
    declared: ResidentResourceUsage,
    observed: Option<MeasuredUsage>,
) -> Result<ResidentResourceUsage, String> {
    let Some(observed) = observed else {
        return Ok(declared);
    };
    if observed.pids_current == 0 {
        return Err("shared resident observation has no live process".into());
    }
    let memory_bytes = observed
        .memory_current_bytes
        .checked_add(observed.swap_current_bytes)
        .ok_or("shared resident memory observation overflow")?;
    Ok(ResidentResourceUsage {
        memory_bytes,
        pids: observed.pids_current,
        workspace_bytes: observed
            .workspace_upper_bound_bytes
            .unwrap_or(declared.workspace_bytes),
        ..declared
    })
}

pub(crate) const DEFAULT_STARTUP_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn startup_memory_bytes() -> Result<u64, String> {
    crate::positive_u64_env(
        "RESIDENT_STARTUP_MEMORY_BYTES",
        DEFAULT_STARTUP_MEMORY_BYTES,
    )
}

pub(crate) struct Admission {
    pub global_used: ResidentResourceUsage,
    pub tenant_used: ResidentResourceUsage,
    pub host_used: ResidentResourceUsage,
    pub requested: ResidentResourceUsage,
    pub limits: ResidentCapacity,
    pub host_available_bytes: u64,
    pub host_headroom_bytes: u64,
    pub startup_memory_bytes: u64,
    pub unobserved_host_memory_bytes: u64,
}

pub(crate) fn admits(r: &Admission) -> Result<bool, String> {
    // Reuse Core's bounded one-environment request validation. Shared host
    // scheduling changes admission policy, never the immutable declaration.
    admit_resident_resources(
        Default::default(),
        Default::default(),
        r.requested,
        r.requested,
        r.requested,
    )?;
    let fits_shared = |usage: ResidentResourceUsage, limit: ResidentResourceUsage| {
        // CPU remains a per-environment cgroup ceiling, not a reservation.
        ResidentResourceUsage {
            cpu_milli: 0,
            ..usage
        }
        .fits(limit)
    };
    // A new preparation uses a startup allowance, not its cgroup ceiling.
    // Unknown attempts retain their full durable charge below; aggregate
    // declaration checks and the actual cgroup ceiling remain unchanged.
    let startup_memory = r.startup_memory_bytes.min(r.requested.memory_bytes);
    let required_available = r
        .host_headroom_bytes
        .checked_add(r.unobserved_host_memory_bytes)
        .and_then(|value| value.checked_add(startup_memory))
        .ok_or("shared resident host memory accounting overflow")?;
    let cpu_ceiling = r
        .limits
        .global
        .cpu_milli
        .min(r.limits.tenant.cpu_milli)
        .min(r.limits.host.cpu_milli);
    Ok(r.requested.cpu_milli <= cpu_ceiling
        && fits_shared(r.global_used.checked_add(r.requested)?, r.limits.global)
        && fits_shared(r.tenant_used.checked_add(r.requested)?, r.limits.tenant)
        && fits_shared(r.host_used.checked_add(r.requested)?, r.limits.host)
        && r.host_available_bytes >= required_available)
}

pub(crate) fn enabled() -> Result<bool, String> {
    enabled_from_lookup(|name| std::env::var(name).ok())
}
fn enabled_from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<bool, String> {
    match get("RUNTIME_NATIVE_SHARED_ADMISSION").as_deref() {
        None | Some("0") => Ok(false),
        Some("1") => {
            if get("RUNTIME_EXECUTION_PROVIDER")
                .as_deref()
                .unwrap_or("docker")
                != "docker"
                || get("OCI_RUNTIME").as_deref().unwrap_or("runsc") != "runsc"
            {
                return Err("native_shared_admission_backend_invalid".into());
            }
            Ok(true)
        }
        Some(_) => Err("native_shared_admission_config_invalid".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GIB: u64 = 1024 * 1024 * 1024;
    fn declaration() -> ResidentResourceUsage {
        ResidentResourceUsage {
            sandbox_count: 1,
            memory_bytes: 4 * GIB,
            cpu_milli: 2000,
            pids: 512,
            workspace_bytes: 4 * GIB,
        }
    }
    fn measured() -> MeasuredUsage {
        MeasuredUsage {
            memory_current_bytes: GIB / 4,
            swap_current_bytes: GIB / 8,
            pids_current: 12,
            workspace_upper_bound_bytes: Some(GIB / 16),
        }
    }
    fn request(existing: ResidentResourceUsage) -> Admission {
        let limit = ResidentResourceUsage {
            sandbox_count: 16,
            memory_bytes: 12 * GIB,
            cpu_milli: 8000,
            pids: 8192,
            workspace_bytes: 32 * GIB,
        };
        let tenant = ResidentResourceUsage {
            sandbox_count: 8,
            memory_bytes: 8 * GIB,
            cpu_milli: 4000,
            pids: 2048,
            workspace_bytes: 32 * GIB,
        };
        Admission {
            global_used: existing,
            tenant_used: existing,
            host_used: existing,
            requested: declaration(),
            limits: ResidentCapacity {
                global: limit,
                tenant,
                host: limit,
            },
            host_available_bytes: 10 * GIB,
            host_headroom_bytes: 2 * GIB,
            startup_memory_bytes: DEFAULT_STARTUP_MEMORY_BYTES,
            unobserved_host_memory_bytes: 0,
        }
    }
    #[test]
    fn measured_residents_charge_actual_memory_and_swap_without_mutating_caps() {
        let d = declaration();
        let actual = charge(d, Some(measured())).unwrap();
        assert_eq!(actual.memory_bytes, 3 * GIB / 8);
        assert_eq!(actual.workspace_bytes, GIB / 16);
        assert_eq!(actual.pids, 12);
        assert_eq!(
            actual.cpu_milli, 2000,
            "CPU declaration remains an upper limit fact"
        );
        assert_eq!(d.memory_bytes, 4 * GIB);
    }
    #[test]
    fn unknown_resident_keeps_every_declared_debit() {
        assert_eq!(charge(declaration(), None).unwrap(), declaration());
    }
    #[test]
    fn unknown_storage_retains_declared_budget_instead_of_using_memory_as_disk_usage() {
        let mut m = measured();
        m.workspace_upper_bound_bytes = None;
        assert_eq!(
            charge(declaration(), Some(m)).unwrap().workspace_bytes,
            4 * GIB
        );
    }
    #[test]
    fn fourth_default_environment_can_share_cpus_with_three_measured_residents() {
        let one = charge(declaration(), Some(measured())).unwrap();
        let three = one.checked_add(one).unwrap().checked_add(one).unwrap();
        assert!(admits(&request(three)).unwrap(),"8 GiB/4 CPU declaration gate must not limit a low-memory workspace to two environments");
    }
    #[test]
    fn real_host_pressure_blocks_new_creation_even_below_static_totals() {
        let mut r = request(Default::default());
        r.host_available_bytes = 2 * GIB;
        assert!(!admits(&r).unwrap());
    }
    #[test]
    fn cold_creation_uses_startup_headroom_without_reducing_the_runtime_ceiling() {
        let mut r = request(Default::default());
        r.host_available_bytes = 3 * GIB;
        assert!(admits(&r).unwrap());
        assert_eq!(r.requested.memory_bytes, 4 * GIB);
        r.global_used.memory_bytes = 9 * GIB;
        assert!(
            !admits(&r).unwrap(),
            "aggregate declaration checks remain enforced"
        );
    }
    #[test]
    fn startup_headroom_is_bounded_by_the_requested_cgroup_ceiling() {
        let mut r = request(Default::default());
        r.requested.memory_bytes = GIB / 8;
        r.host_available_bytes = r.host_headroom_bytes + GIB / 8;
        assert!(admits(&r).unwrap());
        r.host_available_bytes -= 1;
        assert!(!admits(&r).unwrap());
    }
    #[test]
    fn known_memory_already_in_host_available_is_not_subtracted_twice() {
        let mut r = request(charge(declaration(), Some(measured())).unwrap());
        r.host_available_bytes = 6 * GIB;
        assert!(admits(&r).unwrap());
    }
    #[test]
    fn unknown_creation_still_reserves_full_memory_against_host_headroom() {
        let mut r = request(declaration());
        r.host_available_bytes = 6 * GIB;
        r.unobserved_host_memory_bytes = 4 * GIB;
        assert!(!admits(&r).unwrap());
    }
    #[test]
    fn observed_space_exhaustion_still_blocks_admission() {
        let mut r = request(Default::default());
        r.tenant_used.workspace_bytes = 30 * GIB;
        assert!(!admits(&r).unwrap());
    }
    #[test]
    fn count_and_pid_limits_remain_effective() {
        let mut r = request(Default::default());
        r.tenant_used.sandbox_count = 8;
        assert!(!admits(&r).unwrap());
        r.tenant_used.sandbox_count = 0;
        r.tenant_used.pids = 2000;
        assert!(!admits(&r).unwrap());
    }
    #[test]
    fn invalid_observation_and_overflow_are_not_zero_usage() {
        let mut m = measured();
        m.pids_current = 0;
        assert!(charge(declaration(), Some(m)).is_err());
        m.pids_current = 1;
        m.memory_current_bytes = u64::MAX;
        assert!(charge(declaration(), Some(m)).is_err());
    }
    #[test]
    fn shared_admission_is_explicit_and_rejects_invalid_or_foreign_backend_config() {
        let lookup = |flag: Option<&str>, provider: &str, runtime: &str| {
            enabled_from_lookup(|name| match name {
                "RUNTIME_NATIVE_SHARED_ADMISSION" => flag.map(str::to_owned),
                "RUNTIME_EXECUTION_PROVIDER" => Some(provider.into()),
                "OCI_RUNTIME" => Some(runtime.into()),
                _ => None,
            })
        };
        assert!(!lookup(None, "docker", "runsc").unwrap());
        assert!(!lookup(Some("0"), "docker", "runsc").unwrap());
        assert!(lookup(Some("1"), "docker", "runsc").unwrap());
        for value in ["", "true", "yes", "2", " 1"] {
            assert!(lookup(Some(value), "docker", "runsc").is_err());
        }
        assert!(lookup(Some("1"), "e2b", "runsc").is_err());
        assert!(lookup(Some("1"), "docker", "runc").is_err());
    }
    #[test]
    fn cpu_sharing_never_allows_a_single_request_above_host_ceiling() {
        let mut r = request(Default::default());
        r.requested.cpu_milli = 9000;
        assert!(!admits(&r).unwrap());
    }
}
