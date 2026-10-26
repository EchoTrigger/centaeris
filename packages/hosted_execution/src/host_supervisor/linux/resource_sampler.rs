//! Host-owned cgroup sampling. No path is accepted from the resource request.
use super::Registration;
use crate::host_supervisor::resources::{
    parse, CgroupIdentity, ContainerResourceObservation, ContainerResources, MeasuredBytes,
};
use std::path::{Path, PathBuf};
mod cgroup;
mod host;
use cgroup::Directory;
struct ProbeRoots {
    proc: PathBuf,
    cgroup: PathBuf,
}
impl ProbeRoots {
    fn system() -> Self {
        Self {
            proc: "/proc".into(),
            cgroup: "/sys/fs/cgroup".into(),
        }
    }
}
fn process_identity(reg: &Registration, roots: &ProbeRoots) -> Result<(), String> {
    let process = Directory::open(&roots.proc, Path::new(&reg.init_pid.to_string()))?;
    let stat = process.read("stat")?;
    let start = stat
        .rsplit_once(')')
        .and_then(|(_, s)| s.split_whitespace().nth(19))
        .ok_or("host_resource_process_unknown")?;
    if parse::scalar(start)? != reg.init_start_time {
        return Err("host_resource_process_changed".into());
    }
    let membership = process.read("cgroup")?;
    let unified = membership
        .lines()
        .filter_map(|line| line.strip_prefix("0::"))
        .collect::<Vec<_>>();
    if unified.len() != 1 || !unified[0].starts_with('/') || unified[0] == "/" {
        return Err("host_resource_cgroup_unknown".into());
    }
    let expected = roots.cgroup.join(&unified[0][1..]);
    if expected != Path::new(&reg.cgroup) {
        return Err("host_resource_cgroup_changed".into());
    }
    Ok(())
}
fn open_group(reg: &Registration, roots: &ProbeRoots) -> Result<Directory, String> {
    reg.target.validate()?;
    let group = Path::new(&reg.cgroup);
    let relative = group
        .strip_prefix(&roots.cgroup)
        .map_err(|_| "host_resource_cgroup_outside_root")?;
    let leaf = group
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("host_resource_cgroup_unknown")?;
    if leaf != reg.target.container_id
        && leaf != format!("docker-{}.scope", reg.target.container_id)
    {
        return Err("host_resource_cgroup_target_mismatch".into());
    }
    Directory::open(&roots.cgroup, relative)
}
fn contains_init(group: &Directory, pid: u32) -> Result<(), String> {
    let contents = group.read("cgroup.procs")?;
    let mut present = false;
    for line in contents.lines() {
        if parse::scalar(line)? == u64::from(pid) {
            present = true;
        }
    }
    if !present {
        return Err("host_resource_cgroup_process_missing".into());
    }
    Ok(())
}
fn try_sample_target(
    reg: &Registration,
    roots: &ProbeRoots,
    confirm: impl FnOnce() -> Result<Registration, String>,
) -> Result<ContainerResources, String> {
    process_identity(reg, roots)?;
    let group = open_group(reg, roots)?;
    contains_init(&group, reg.init_pid)?;
    let identity = group.identity()?;
    let resources = ContainerResources {
        target: reg.target.clone(),
        init_pid: reg.init_pid,
        init_start_ticks: reg.init_start_time,
        cgroup_identity: CgroupIdentity {
            path: reg.cgroup.clone(),
            device: identity.0,
            inode: identity.1,
        },
        memory_current_bytes: parse::scalar(&group.read("memory.current")?)?,
        memory_swap_current_bytes: parse::scalar(&group.read("memory.swap.current")?)?,
        pids_current: parse::scalar(&group.read("pids.current")?)?,
        cpu_counters: parse::cpu(&group.read("cpu.stat")?)?,
        memory_events: parse::memory_events(&group.read("memory.events")?)?,
        memory_pressure: parse::pressure(&group.read("memory.pressure")?)?,
        cpu_pressure: parse::pressure(&group.read("cpu.pressure")?)?,
        workspace_usage: MeasuredBytes::Unknown {
            reason: "workspace_storage_not_observed".into(),
        },
    };
    if confirm()? != *reg {
        return Err("host_resource_target_changed".into());
    }
    process_identity(reg, roots)?;
    let confirmed = open_group(reg, roots)?;
    contains_init(&confirmed, reg.init_pid)?;
    if confirmed.identity()? != identity {
        return Err("host_resource_cgroup_replaced".into());
    }
    Ok(resources)
}
fn sample_target(
    reg: &Registration,
    roots: &ProbeRoots,
    confirm: impl FnOnce() -> Result<Registration, String>,
) -> ContainerResourceObservation {
    match try_sample_target(reg, roots, confirm) {
        Ok(resources) => ContainerResourceObservation::Observed {
            resources: Box::new(resources),
        },
        Err(reason) => ContainerResourceObservation::Unknown {
            target: reg.target.clone(),
            reason,
        },
    }
}
struct ResourceRequest<'a> {
    host: &'a str,
    targets: &'a [super::HostLaunchTarget],
}
#[derive(Clone)]
struct DaemonIdentity {
    id: String,
    memory_bytes: u64,
    cpu_count: u32,
}
fn collect(
    request: ResourceRequest<'_>,
    roots: &ProbeRoots,
    mut attest: impl FnMut(&super::HostLaunchTarget) -> Result<Registration, String>,
    mut daemon: impl FnMut() -> Result<DaemonIdentity, String>,
) -> Result<crate::host_supervisor::resources::HostResourceObservation, String> {
    use crate::host_supervisor::resources::{
        validate_resource_request, HostResourceObservation, HOST_RESOURCE_SCHEMA,
    };
    validate_resource_request(request.host, request.targets)?;
    let before = daemon()?;
    let began = host::sample(roots)?;
    if before.id != request.host
        || before.memory_bytes != began.memory.total
        || before.cpu_count != began.cpu_count
    {
        return Err("host_resource_topology_mismatch".into());
    }
    let mut containers = Vec::with_capacity(request.targets.len());
    for target in request.targets {
        let sample = match attest(target) {
            Ok(reg) if reg.target == *target => sample_target(&reg, roots, || attest(target)),
            _ => ContainerResourceObservation::Unknown {
                target: target.clone(),
                reason: "host_resource_target_unavailable".into(),
            },
        };
        containers.push(sample);
    }
    let ended = host::sample(roots)?;
    let after = daemon()?;
    if ended.boot_id != began.boot_id
        || ended.at_ms < began.at_ms
        || after.id != before.id
        || after.memory_bytes != ended.memory.total
        || after.cpu_count != ended.cpu_count
    {
        return Err("host_resource_identity_changed".into());
    }
    Ok(HostResourceObservation {
        schema: HOST_RESOURCE_SCHEMA.into(),
        host_id: after.id,
        boot_id: ended.boot_id,
        observed_at_boot_ms: ended.at_ms,
        collection_duration_ms: ended.at_ms - began.at_ms,
        mem_total_bytes: ended.memory.total,
        mem_available_bytes: ended.memory.available,
        cpu_count: ended.cpu_count,
        memory_pressure: ended.memory_pressure,
        cpu_pressure: ended.cpu_pressure,
        containers,
    })
}
pub(super) fn observe(
    config: &super::HostSupervisorConfig,
    expected_host_id: &str,
    targets: &[super::HostLaunchTarget],
) -> Result<crate::host_supervisor::resources::HostResourceObservation, String> {
    collect(
        ResourceRequest {
            host: expected_host_id,
            targets,
        },
        &ProbeRoots::system(),
        |target| super::attest_observation(config, target),
        || {
            let endpoint = format!("unix://{}", config.docker_socket);
            let value = super::command_json(
                &config.docker_path,
                &[
                    "--host",
                    &endpoint,
                    "info",
                    "--format",
                    r#"{"id":{{json .ID}},"memoryBytes":{{.MemTotal}},"cpuCount":{{.NCPU}}}"#,
                ],
            )?;
            Ok(DaemonIdentity {
                id: value["id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or("host_resource_daemon_unknown")?
                    .into(),
                memory_bytes: value["memoryBytes"]
                    .as_u64()
                    .filter(|v| *v > 0)
                    .ok_or("host_resource_daemon_unknown")?,
                cpu_count: value["cpuCount"]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|v| *v > 0)
                    .ok_or("host_resource_daemon_unknown")?,
            })
        },
    )
}
#[cfg(test)]
mod tests;
