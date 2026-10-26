use super::{cgroup::Directory, ProbeRoots};
use crate::host_supervisor::resources::{parse, valid_boot_id, Pressure};
use std::path::Path;
pub(super) struct HostFields {
    pub boot_id: String,
    pub at_ms: u64,
    pub memory: parse::HostMemory,
    pub cpu_count: u32,
    pub memory_pressure: Pressure,
    pub cpu_pressure: Pressure,
}
pub(super) fn sample(roots: &ProbeRoots) -> Result<HostFields, String> {
    let proc = Directory::open(&roots.proc, Path::new(""))?;
    let boot = Directory::open(&roots.proc, Path::new("sys/kernel/random"))?.read("boot_id")?;
    let boot_id = boot.trim().to_owned();
    if !valid_boot_id(&boot_id) {
        return Err("host_resource_boot_id_invalid".into());
    }
    let pressure = Directory::open(&roots.proc, Path::new("pressure"))?;
    let memory_pressure = parse::pressure(&pressure.read("memory")?)?;
    if memory_pressure.full.is_none() {
        return Err("host_resource_memory_pressure_incomplete".into());
    }
    Ok(HostFields {
        boot_id,
        at_ms: parse::uptime_ms(&proc.read("uptime")?)?,
        memory: parse::memory(&proc.read("meminfo")?)?,
        cpu_count: parse::cpu_count(&proc.read("stat")?)?,
        memory_pressure,
        cpu_pressure: parse::pressure(&pressure.read("cpu")?)?,
    })
}
