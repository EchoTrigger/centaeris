//! Native host observations are measurements, never resource declarations or release evidence.
use super::HostLaunchTarget;
use serde::{Deserialize, Serialize};

pub const HOST_RESOURCE_SCHEMA: &str = "native.host.resources.v1";
pub const MAX_RESOURCE_TARGETS: usize = 64;
#[cfg(any(target_os = "linux", test))]
pub(super) mod parse;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PressureLine {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_micros: u64,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pressure {
    pub some: PressureLine,
    pub full: Option<PressureLine>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpuCounters {
    pub usage_usec: u64,
    pub user_usec: u64,
    pub system_usec: u64,
    pub nr_periods: Option<u64>,
    pub nr_throttled: Option<u64>,
    pub throttled_usec: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryEvents {
    pub low: u64,
    pub high: u64,
    pub max: u64,
    pub oom: u64,
    pub oom_kill: u64,
    pub oom_group_kill: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MeasuredBytes {
    Observed {
        bytes: u64,
    },
    UpperBound {
        bytes: u64,
        source: WorkspaceUsageSource,
    },
    Unknown {
        reason: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceUsageSource {
    ContainerWritableLayer,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CgroupIdentity {
    pub path: String,
    pub device: u64,
    pub inode: u64,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerResources {
    pub target: HostLaunchTarget,
    pub init_pid: u32,
    pub init_start_ticks: u64,
    pub cgroup_identity: CgroupIdentity,
    pub memory_current_bytes: u64,
    pub memory_swap_current_bytes: u64,
    pub pids_current: u64,
    pub cpu_counters: CpuCounters,
    pub memory_events: MemoryEvents,
    pub memory_pressure: Pressure,
    pub cpu_pressure: Pressure,
    pub workspace_usage: MeasuredBytes,
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContainerResourceObservation {
    Observed {
        resources: Box<ContainerResources>,
    },
    Unknown {
        target: HostLaunchTarget,
        reason: String,
    },
}
impl ContainerResourceObservation {
    pub fn target(&self) -> &HostLaunchTarget {
        match self {
            Self::Observed { resources } => &resources.target,
            Self::Unknown { target, .. } => target,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostResourceObservation {
    pub schema: String,
    pub host_id: String,
    pub boot_id: String,
    pub observed_at_boot_ms: u64,
    pub collection_duration_ms: u64,
    pub mem_total_bytes: u64,
    pub mem_available_bytes: u64,
    pub cpu_count: u32,
    pub memory_pressure: Pressure,
    pub cpu_pressure: Pressure,
    pub containers: Vec<ContainerResourceObservation>,
}
#[derive(Clone, Debug)]
pub struct ObservationClock {
    pub boot_id: String,
    pub now_boot_ms: u64,
    pub max_age_ms: u64,
}

impl ObservationClock {
    /// Read the current host boot identity and monotonic clock, not wall time.
    #[cfg(target_os = "linux")]
    pub fn from_host_kernel(max_age_ms: u64) -> Result<Self, String> {
        if max_age_ms == 0 {
            return Err("host_resource_clock_invalid".into());
        }
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(|_| "host_resource_clock_unavailable")?;
        let uptime = std::fs::read_to_string("/proc/uptime")
            .map_err(|_| "host_resource_clock_unavailable")?;
        let boot_id = boot_id.trim().to_owned();
        if !valid_boot_id(&boot_id) {
            return Err("host_resource_clock_invalid".into());
        }
        Ok(Self {
            boot_id,
            now_boot_ms: parse::uptime_ms(&uptime)?,
            max_age_ms,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn from_host_kernel(_max_age_ms: u64) -> Result<Self, String> {
        Err("host_resource_clock_unsupported".into())
    }
}

pub fn validate_resource_request(host: &str, targets: &[HostLaunchTarget]) -> Result<(), String> {
    use std::collections::BTreeSet;
    if host.is_empty()
        || host.len() > 256
        || !host.bytes().all(|b| b.is_ascii_graphic())
        || targets.len() > MAX_RESOURCE_TARGETS
    {
        return Err("host_resource_request_invalid".into());
    }
    let (mut containers, mut instances, mut environments) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for target in targets {
        target.validate()?;
        if !containers.insert(&target.container_id)
            || !instances.insert(&target.environment_instance_id)
            || !environments.insert(&target.environment_id)
        {
            return Err("host_resource_duplicate_target".into());
        }
    }
    Ok(())
}
impl HostResourceObservation {
    pub fn validate_for(
        &self,
        host: &str,
        targets: &[HostLaunchTarget],
        clock: &ObservationClock,
    ) -> Result<(), String> {
        validate_resource_request(host, targets)?;
        let start = self
            .observed_at_boot_ms
            .checked_sub(self.collection_duration_ms)
            .ok_or("host_resource_timestamp_invalid")?;
        if self.schema != HOST_RESOURCE_SCHEMA
            || self.host_id != host
            || self.boot_id != clock.boot_id
            || !valid_boot_id(&self.boot_id)
            || self.mem_total_bytes == 0
            || self.mem_available_bytes > self.mem_total_bytes
            || self.cpu_count == 0
            || clock.max_age_ms == 0
            || self.observed_at_boot_ms > clock.now_boot_ms
            || clock.now_boot_ms.saturating_sub(start) > clock.max_age_ms
            || self.containers.len() != targets.len()
        {
            return Err("host_resource_observation_rejected".into());
        }
        self.memory_pressure.validate()?;
        self.cpu_pressure.validate()?;
        let mut seen = std::collections::BTreeSet::new();
        for observation in &self.containers {
            let target = observation.target();
            if !targets.contains(target) || !seen.insert(&target.container_id) {
                return Err("host_resource_inventory_mismatch".into());
            }
            match observation {
                ContainerResourceObservation::Unknown { reason, .. } if !valid_reason(reason) => {
                    return Err("host_resource_unknown_reason_invalid".into())
                }
                ContainerResourceObservation::Unknown { .. } => {}
                ContainerResourceObservation::Observed { resources } => {
                    let path = &resources.cgroup_identity.path;
                    if resources.init_pid == 0
                        || resources.init_start_ticks == 0
                        || resources.cgroup_identity.inode == 0
                        || !path.starts_with("/sys/fs/cgroup/")
                        || path
                            .split('/')
                            .skip(1)
                            .any(|p| p.is_empty() || p == "." || p == "..")
                    {
                        return Err("host_resource_cgroup_identity_invalid".into());
                    }
                    resources.memory_pressure.validate()?;
                    resources.cpu_pressure.validate()?;
                    if let MeasuredBytes::Unknown { reason } = &resources.workspace_usage {
                        if !valid_reason(reason) {
                            return Err("host_resource_unknown_reason_invalid".into());
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
impl Pressure {
    fn validate(&self) -> Result<(), String> {
        for line in std::iter::once(&self.some).chain(self.full.as_ref()) {
            if [line.avg10, line.avg60, line.avg300]
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=100.0).contains(v))
            {
                return Err("host_resource_pressure_invalid".into());
            }
        }
        Ok(())
    }
}
pub(super) fn valid_boot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn valid_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
#[cfg(test)]
mod tests;
