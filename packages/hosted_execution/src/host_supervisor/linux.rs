use super::*;
use crate::task_service::linux::{launcher::OciTarget, HelperBinary, NativeManager};
pub(crate) mod commands;
mod helpers;
mod idle_pause;
pub(crate) mod probe;
mod resource_sampler;
mod retirement;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

static PROBE_ADMISSION: probe::ProbeAdmission = probe::ProbeAdmission::new();
const MAX_REQUEST: usize = 128 * 1024;
const MAX_RESPONSE: usize = 1024 * 1024;
// Header readers and dispatched operations consume independent bounded slots.
const HEADER_LIMIT: usize = 8;
const EXECUTION_LIMIT: usize = 8;
const HEADER_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Registration {
    target: HostLaunchTarget,
    init_pid: u32,
    init_start_time: u64,
    bundle: String,
    cgroup: String,
    spec_digest: String,
    process: serde_json::Value,
}

pub(crate) fn protected(path: &Path, directory: bool) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "host_path_unavailable")?;
    if meta.file_type().is_symlink()
        || meta.uid() != 0
        || (meta.mode() & 0o022 != 0 && !(directory && meta.mode() & 0o1000 != 0))
        || (directory && !meta.is_dir())
        || (!directory && !meta.is_file())
    {
        return Err("host_path_untrusted".into());
    }
    if let Some(parent) = path.parent().filter(|p| p.as_os_str() != "/") {
        protected(parent, true)?;
    }
    Ok(())
}
fn directory(path: &Path) -> Result<(), String> {
    if fs::symlink_metadata(path).is_ok() {
        protected(path, true)?;
        if fs::metadata(path)
            .map_err(|_| "host_path_unavailable")?
            .mode()
            & 0o077
            != 0
        {
            return Err("host_private_directory_required".into());
        }
        return Ok(());
    }
    let parent = path.parent().ok_or("host_path_untrusted")?;
    if parent.exists() {
        protected(parent, true)?;
    } else {
        directory(parent)?;
    }
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| "host_state_unavailable")?;
    protected(path, true)
}
fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    protected(path, false)?;
    let mut data = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "host_state_unavailable")?
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| "host_state_unavailable")?;
    if data.len() > MAX_RESPONSE {
        return Err("host_state_oversized".into());
    }
    serde_json::from_slice(&data).map_err(|_| "host_state_invalid".into())
}
pub(crate) fn read_bundle_spec(bundle: &Path) -> Result<serde_json::Value, String> {
    protected(bundle, true)?;
    // containerd writes config.json as 0666 inside a root-only 0700 bundle.
    // The private directory, rather than the file's DAC bits, protects that
    // particular runtime-owned record. Never apply this exception to controls.
    if fs::metadata(bundle)
        .map_err(|_| "host_bundle_unknown")?
        .mode()
        & 0o077
        != 0
    {
        return Err("host_bundle_untrusted".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(bundle.join("config.json"))
        .map_err(|_| "host_bundle_unknown")?;
    let metadata = file.metadata().map_err(|_| "host_bundle_unknown")?;
    if metadata.uid() != 0 || !metadata.is_file() {
        return Err("host_bundle_untrusted".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_RESPONSE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "host_bundle_unknown")?;
    if bytes.len() > MAX_RESPONSE {
        return Err("host_bundle_unknown".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "host_bundle_unknown".into())
}
fn command_json(path: &str, args: &[&str]) -> Result<serde_json::Value, String> {
    command_json_with_budget(path, args, None)
}
pub(crate) fn command_json_unlocked(
    path: &str,
    args: &[&str],
) -> Result<serde_json::Value, String> {
    command_json(path, args)
}
fn command_json_with_budget(
    path: &str,
    args: &[&str],
    budget: Option<&probe::ProbeBudget<'_>>,
) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&command_bytes_with_budget(path, args, budget)?)
        .map_err(|_| "host_command_response_invalid".into())
}
pub(crate) fn command_bytes_unlocked(path: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    command_bytes_with_budget(path, args, None)
}
fn command_bytes_with_budget(
    path: &str,
    args: &[&str],
    budget: Option<&probe::ProbeBudget<'_>>,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let check = || {
        if let Some(budget) = budget {
            budget.check()?;
        }
        if Instant::now() >= deadline {
            return Err("host_command_timeout".into());
        }
        Ok(())
    };
    let mut command = Command::new(path);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = commands::OwnedChild::spawn_checked(&mut command, check)?;
    let result = (|| {
        let mut stdout = child.stdout.take().ok_or("host_command_failed")?;
        // Nonblocking reads keep inherited pipe writers within the same deadline;
        // a reader thread joined after child exit could otherwise wait forever.
        let fd = stdout.as_raw_fd();
        // SAFETY: stdout owns this live descriptor; only its nonblocking flag changes.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err("host_command_failed".into());
        }
        let mut bytes = Vec::new();
        let mut status = None;
        let mut eof = false;
        loop {
            check()?;
            if status.is_none() {
                status = child.try_wait().map_err(|_| "host_command_failed")?;
            }
            if !eof {
                let mut buffer = [0u8; 16 * 1024];
                loop {
                    check()?;
                    match stdout.read(&mut buffer) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(count) => {
                            bytes.extend_from_slice(&buffer[..count]);
                            if bytes.len() > MAX_RESPONSE {
                                return Err("host_command_failed".into());
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => return Err("host_command_failed".into()),
                    }
                }
            }
            if let Some(status) = status {
                if eof {
                    return if status.success() {
                        Ok(bytes)
                    } else {
                        Err("host_command_failed".into())
                    };
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        // Ownership stays with this handle or the cleanup worker until reaped.
        let _ = child.kill();
    }
    result
}

pub(crate) fn start_time(pid: u32) -> Result<u64, String> {
    let stat =
        fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|_| "host_target_unknown")?;
    stat.rsplit_once(')')
        .and_then(|(_, s)| s.split_whitespace().nth(19))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "host_target_unknown".into())
}
#[derive(Clone, Copy)]
enum AttestationPower {
    Running,
    Live,
}
fn attest(
    config: &HostSupervisorConfig,
    target: &HostLaunchTarget,
) -> Result<Registration, String> {
    attest_with_power(config, target, AttestationPower::Running)
}
fn attest_observation(
    config: &HostSupervisorConfig,
    target: &HostLaunchTarget,
    budget: &probe::ProbeBudget<'_>,
) -> Result<Registration, String> {
    attest_power_with_budget(config, target, AttestationPower::Live, Some(budget))
        .map(|(registration, _)| registration)
}
fn attest_with_power(
    config: &HostSupervisorConfig,
    target: &HostLaunchTarget,
    power: AttestationPower,
) -> Result<Registration, String> {
    attest_power(config, target, power).map(|(registration, _)| registration)
}
fn attest_power(
    config: &HostSupervisorConfig,
    target: &HostLaunchTarget,
    power: AttestationPower,
) -> Result<(Registration, bool), String> {
    attest_power_with_budget(config, target, power, None)
}
fn attest_power_with_budget(
    config: &HostSupervisorConfig,
    target: &HostLaunchTarget,
    power: AttestationPower,
    budget: Option<&probe::ProbeBudget<'_>>,
) -> Result<(Registration, bool), String> {
    if let Some(budget) = budget {
        budget.check()?;
    }
    target.validate()?;
    let endpoint = format!("unix://{}", config.docker_socket);
    let values = command_json_with_budget(
        &config.docker_path,
        &[
            "--host",
            &endpoint,
            "inspect",
            "--type",
            "container",
            &target.container_id,
        ],
        budget,
    )?;
    let value = values
        .as_array()
        .filter(|v| v.len() == 1)
        .and_then(|v| v.first())
        .ok_or("host_target_unknown")?;
    let labels = &value["Config"]["Labels"];
    if value["Id"] != target.container_id
        || value["Image"] != target.image_digest
        || value["State"]["Running"] != true
        || labels["centaeris.managed"] != "true"
        || labels["centaeris.instance_id"] != target.environment_instance_id
        || labels["centaeris.environment_id"] != target.environment_id
        || labels["centaeris.environment_profile_digest"] != target.profile_digest
        || value["HostConfig"]["Runtime"] != config.oci_runtime.name()
        || value["HostConfig"]["Privileged"] != false
        || value["HostConfig"]["PidMode"] == "host"
        || value["HostConfig"]["IpcMode"] == "host"
        || value["HostConfig"]["CgroupnsMode"] == "host"
    {
        return Err("host_target_identity_rejected".into());
    }
    for mount in value["Mounts"]
        .as_array()
        .ok_or("host_target_mounts_unknown")?
    {
        let source = mount["Source"]
            .as_str()
            .ok_or("host_target_mounts_unknown")?;
        if [
            &config.control_root,
            &config.output_root,
            &config.runtime_root,
            &config.socket_path,
        ]
        .iter()
        .any(|protected| overlaps(source, protected))
            || overlaps(source, "/proc")
            || overlaps(source, "/sys/fs/cgroup")
            || overlaps(source, &config.docker_socket)
        {
            return Err("host_control_exposed_to_workload".into());
        }
    }
    let pid = u32::try_from(
        value["State"]["Pid"]
            .as_u64()
            .ok_or("host_target_unknown")?,
    )
    .map_err(|_| "host_target_unknown")?;
    for namespace in ["pid", "mnt"] {
        let workload = fs::metadata(format!("/proc/{pid}/ns/{namespace}"))
            .map_err(|_| "host_target_unknown")?;
        let host = fs::metadata(format!("/proc/self/ns/{namespace}"))
            .map_err(|_| "host_target_unknown")?;
        if workload.ino() == host.ino() && workload.dev() == host.dev() {
            return Err("host_namespace_isolation_required".into());
        }
    }
    let state = command_json_with_budget(
        &config.runtime_path,
        &[
            "--root",
            &config.runtime_root,
            "state",
            &target.container_id,
        ],
        budget,
    )?;
    let live = state["status"] == "running"
        || (matches!(power, AttestationPower::Live) && state["status"] == "paused");
    if state["id"] != target.container_id || state["pid"] != pid || !live {
        return Err("host_runtime_identity_rejected".into());
    }
    let paused = idle_pause::confirmed_power(
        value["State"]["Paused"]
            .as_bool()
            .ok_or("host_power_unknown")?,
        state["status"].as_str(),
    )?;
    let bundle = state["bundle"]
        .as_str()
        .filter(|p| absolute_path(p))
        .ok_or("host_bundle_unknown")?;
    protected(Path::new(bundle), true)?;
    let spec = read_bundle_spec(Path::new(bundle))?;
    let process = task_process(&spec["process"], "/bin/true", &[], "/", &BTreeMap::new())
        .map(|_| spec["process"].clone())?;
    if spec["linux"]["seccomp"].is_null() {
        return Err("host_seccomp_required".into());
    }
    for capability in [
        "bounding",
        "effective",
        "permitted",
        "inheritable",
        "ambient",
    ] {
        if let Some(caps) = process["capabilities"][capability].as_array() {
            if config.oci_runtime == HostOciRuntime::Runsc && !caps.is_empty() {
                return Err("host_runsc_workload_capabilities_rejected".into());
            }
            if caps.iter().any(|c| {
                matches!(
                    c.as_str(),
                    Some(
                        "CAP_SYS_ADMIN"
                            | "CAP_SYS_PTRACE"
                            | "CAP_SYS_MODULE"
                            | "CAP_SYS_RAWIO"
                            | "CAP_DAC_READ_SEARCH"
                            | "CAP_BPF"
                            | "CAP_PERFMON"
                    )
                )
            }) {
                return Err("host_workload_capabilities_rejected".into());
            }
        }
    }
    if config.oci_runtime == HostOciRuntime::Runsc
        && !spec["mounts"].as_array().is_some_and(|mounts| {
            mounts.iter().any(|mount| {
                mount["destination"] == "/sys/fs/cgroup"
                    && mount["options"]
                        .as_array()
                        .is_some_and(|options| options.iter().any(|v| v == "ro"))
            })
        })
    {
        return Err("host_runsc_readonly_cgroup_required".into());
    }
    let membership = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .map_err(|_| "host_target_cgroup_unknown")?;
    let relative = membership
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .filter(|p| absolute_path(p) && *p != "/")
        .ok_or("host_cgroup_v2_required")?;
    let cgroup = format!("/sys/fs/cgroup{relative}");
    if !Path::new(&cgroup).file_name().is_some_and(|n| {
        n == format!("docker-{}.scope", target.container_id).as_str()
            || n == target.container_id.as_str()
    }) {
        return Err("host_target_cgroup_rejected".into());
    }
    let init_start_time = start_time(pid)?;
    Ok((
        Registration {
            target: target.clone(),
            init_pid: pid,
            init_start_time,
            bundle: bundle.into(),
            cgroup,
            spec_digest: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&spec).map_err(|_| "host_state_invalid")?)
            ),
            process,
        },
        paused,
    ))
}

struct Host {
    config: HostSupervisorConfig,
    managers: Mutex<BTreeMap<String, (Registration, Arc<NativeManager>)>>,
    _lease: File,
    helper_binary: Option<Arc<HelperBinary>>,
    retirement: retirement::RetirementStore,
}
impl Host {
    fn registration_path(&self, id: &str) -> PathBuf {
        Path::new(&self.config.control_root).join(format!("{id}.target.json"))
    }
    fn manager(&self, target: &HostLaunchTarget) -> Result<Arc<NativeManager>, String> {
        self.manager_with_budget(target, None)
    }
    fn manager_with_budget(
        &self,
        target: &HostLaunchTarget,
        budget: Option<&probe::ProbeBudget<'_>>,
    ) -> Result<Arc<NativeManager>, String> {
        if let Some(budget) = budget {
            budget.check()?;
        }
        if self.retirement.blocks(&target.environment_instance_id)? {
            return Err("host_retirement_pending".into());
        }
        let actual = if let Some(budget) = budget {
            attest_power_with_budget(
                &self.config,
                target,
                AttestationPower::Running,
                Some(budget),
            )?
            .0
        } else {
            attest(&self.config, target)?
        };
        let mut managers = if let Some(budget) = budget {
            budget.lock(&self.managers)?
        } else {
            self.managers.lock().map_err(|_| "host_service_failed")?
        };
        if self.retirement.blocks(&target.environment_instance_id)? {
            return Err("host_retirement_pending".into());
        }
        if let Some((expected, manager)) = managers.get(&target.environment_instance_id) {
            if expected != &actual {
                return Err("host_registered_target_changed".into());
            }
            return Ok(manager.clone());
        }
        let path = self.registration_path(&target.environment_instance_id);
        if path.exists() {
            protected(&path, false)?;
            let expected: Registration =
                serde_json::from_value(read_json(&path)?).map_err(|_| "host_state_invalid")?;
            if expected != actual {
                return Err("host_registered_target_changed".into());
            }
        } else {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|_| "host_registration_failed")?;
            serde_json::to_writer(&mut file, &actual).map_err(|_| "host_registration_failed")?;
            file.sync_all().map_err(|_| "host_registration_failed")?;
            File::open(&self.config.control_root)
                .and_then(|f| f.sync_all())
                .map_err(|_| "host_registration_failed")?;
        }
        let control = Path::new(&self.config.control_root).join(&target.environment_instance_id);
        let output = Path::new(&self.config.output_root).join(&target.environment_instance_id);
        let launch = control.join("launch");
        let helper_root = Path::new(&self.config.output_root)
            .join(format!("{}.helpers", target.environment_instance_id));
        directory(&control)?;
        directory(&output)?;
        directory(&launch)?;
        directory(&helper_root)?;
        let launch_target = OciTarget {
            runtime: self.config.oci_runtime,
            helper_binary: self.helper_binary.clone(),
            runtime_path: (&self.config.runtime_path).into(),
            runtime_root: (&self.config.runtime_root).into(),
            container_id: target.container_id.clone(),
            process: actual.process.clone(),
            launch_root: launch,
            init_pid: actual.init_pid,
            init_start_time: actual.init_start_time,
            bundle: actual.bundle.clone().into(),
            spec_digest: actual.spec_digest.clone(),
        };
        let manager = Arc::new(NativeManager::open(
            control,
            output,
            if self.config.oci_runtime == HostOciRuntime::Runsc {
                Path::new(&self.config.control_root)
                    .join(&target.environment_instance_id)
                    .join("domains")
            } else {
                Path::new(&actual.cgroup).join("tasks")
            },
            &target.environment_instance_id,
            launch_target,
            helper_root,
            self.helper_binary.clone(),
        )?);
        managers.insert(
            target.environment_instance_id.clone(),
            (actual, manager.clone()),
        );
        Ok(manager)
    }
    fn existing(&self, id: &str) -> Result<Arc<NativeManager>, String> {
        let registration: Registration =
            serde_json::from_value(read_json(&self.registration_path(id))?)
                .map_err(|_| "host_state_invalid")?;
        if registration.target.environment_instance_id != id {
            return Err("host_registered_target_changed".into());
        }
        self.manager(&registration.target)
    }
    fn inspect_with_budget(
        &self,
        id: &str,
        budget: &probe::ProbeBudget<'_>,
    ) -> Result<serde_json::Value, String> {
        budget.check()?;
        let saved: Registration = serde_json::from_value(read_json(&self.registration_path(id))?)
            .map_err(|_| "host_state_invalid")?;
        if saved.target.environment_instance_id != id {
            return Err("host_registered_target_changed".into());
        }
        let existing = {
            let managers = budget.lock(&self.managers)?;
            managers
                .get(id)
                .map(|(registration, manager)| (registration.clone(), manager.clone()))
        };
        let manager = if let Some((expected, manager)) = existing {
            if expected.target != saved.target {
                return Err("host_registered_target_changed".into());
            }
            if self.retirement.blocks(id)? {
                return Err("host_retirement_pending".into());
            }
            let actual = attest_power_with_budget(
                &self.config,
                &expected.target,
                AttestationPower::Running,
                Some(budget),
            )?
            .0;
            if actual != expected {
                return Err("host_registered_target_changed".into());
            }
            if self.retirement.blocks(id)? {
                return Err("host_retirement_pending".into());
            }
            manager
        } else {
            budget.check()?;
            // Reopening a persisted manager may reconcile task ownership. Once
            // admitted, that recovery retains its responsibility across disconnect.
            self.manager_with_budget(&saved.target, Some(budget))?
        };
        budget.check()?;
        manager.inspection_with_budget(budget)
    }
    fn retire(&self, target: &HostLaunchTarget) -> Result<(), String> {
        self.retire_with_probe(target, || retirement::docker_absent(&self.config, target))
    }
    fn retire_with_probe(
        &self,
        target: &HostLaunchTarget,
        absent: impl FnOnce() -> Result<bool, String>,
    ) -> Result<(), String> {
        target.validate()?;
        let mut managers = self.managers.lock().map_err(|_| "host_service_failed")?;
        if managers
            .get(&target.environment_instance_id)
            .is_some_and(|(registration, _)| &registration.target != target)
        {
            return Err("host_retirement_target_mismatch".into());
        }
        self.retirement.retire(target, absent, || {
            if let Some((_, manager)) = managers.get(&target.environment_instance_id) {
                // Every accepted upload/RPC/output stream holds this exact Arc
                // until it stops touching private state. Never unlink beneath it.
                if Arc::strong_count(manager) != 1 {
                    return Err("host_retirement_busy".into());
                }
                manager.retire_after_absence()?;
            }
            Ok(())
        })?;
        managers.remove(&target.environment_instance_id);
        Ok(())
    }
    fn handle(
        &self,
        envelope: HostSupervisorEnvelope,
        budget: &probe::ProbeBudget<'_>,
    ) -> Result<HostSupervisorResult, String> {
        envelope.validate()?;
        match envelope.request {
            HostSupervisorRequest::ObserveResources {
                expected_host_id,
                targets,
            } => Ok(HostSupervisorResult::ResourceObservation {
                observation: Box::new(resource_sampler::observe(
                    &self.config,
                    &expected_host_id,
                    &targets,
                    budget,
                )?),
            }),
            HostSupervisorRequest::PauseIfIdle { target } => {
                let outcome = self.pause_if_idle(&target)?;
                Ok(HostSupervisorResult::IdlePause { target, outcome })
            }
            HostSupervisorRequest::Register { target } => {
                self.manager(&target)?;
                Ok(HostSupervisorResult::Registered { target })
            }
            HostSupervisorRequest::Retire { target } => {
                self.retire(&target)?;
                Ok(HostSupervisorResult::Retired { target })
            }
            HostSupervisorRequest::Inspect {
                environment_instance_id,
            } => Ok(HostSupervisorResult::Inspection {
                inspection: self.inspect_with_budget(&environment_instance_id, budget)?,
            }),
            HostSupervisorRequest::Task { envelope } => {
                let id = request_instance(&envelope.request);
                let manager = self.existing(id)?;
                Ok(HostSupervisorResult::Task {
                    response: EnvironmentTaskResponse::from_result(
                        manager.handle(envelope.request),
                    ),
                })
            }
            HostSupervisorRequest::Helper { .. } | HostSupervisorRequest::HelperOutput { .. } => {
                Err("host_helper_binary_transport_required".into())
            }
        }
    }
}

pub fn serve(config_path: &Path) -> Result<(), String> {
    // SAFETY: geteuid has no arguments or side effects.
    if unsafe { libc::geteuid() } != 0 {
        return Err("host_supervisor_requires_root".into());
    }
    protected(config_path, false)?;
    let config: HostSupervisorConfig = serde_json::from_value(read_json(config_path)?)
        .map_err(|_| "host_configuration_invalid")?;
    config.validate()?;
    protected(Path::new(&config.runtime_path), false)?;
    protected(Path::new(&config.docker_path), false)?;
    protected(Path::new(&config.runtime_root), true)?;
    directory(Path::new(&config.control_root))?;
    directory(Path::new(&config.output_root))?;
    let socket = Path::new(&config.socket_path);
    directory(socket.parent().ok_or("host_socket_invalid")?)?;
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(Path::new(&config.control_root).join("supervisor.lock"))
        .map_err(|_| "host_lease_unavailable")?;
    // SAFETY: the retained lease FD serializes all control/registration updates.
    if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("host_supervisor_already_running".into());
    }
    if socket.exists() {
        let meta = fs::symlink_metadata(socket).map_err(|_| "host_socket_invalid")?;
        if meta.uid() != 0 || meta.file_type().is_symlink() {
            return Err("host_socket_untrusted".into());
        }
        fs::remove_file(socket).map_err(|_| "host_socket_invalid")?;
    }
    let listener = UnixListener::bind(socket).map_err(|_| "host_socket_bind_failed")?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(|_| "host_socket_protection_failed")?;
    let helper_binary = config
        .helper_path
        .as_deref()
        .map(HelperBinary::load)
        .transpose()?
        .map(Arc::new);
    let host = Arc::new(Host {
        retirement: retirement::RetirementStore::new(&config),
        config,
        managers: Mutex::new(BTreeMap::new()),
        _lease: lease,
        helper_binary,
    });
    retirement::start_retries(&host)?;
    let admission = Arc::new(ClientAdmission::new());
    for stream in listener.incoming() {
        let stream = stream.map_err(|_| "host_socket_accept_failed")?;
        start_client(host.clone(), stream, Instant::now(), admission.clone())?;
    }
    Ok(())
}
struct ClientAdmission {
    headers: Arc<AtomicUsize>,
    executing: Arc<AtomicUsize>,
}
impl ClientAdmission {
    fn new() -> Self {
        Self {
            headers: Arc::new(AtomicUsize::new(0)),
            executing: Arc::new(AtomicUsize::new(0)),
        }
    }
}
fn start_client(
    host: Arc<Host>,
    stream: UnixStream,
    accepted_at: Instant,
    admission: Arc<ClientAdmission>,
) -> Result<Option<thread::JoinHandle<()>>, String> {
    let Some(header) = ClientPermit::acquire(&admission.headers, HEADER_LIMIT) else {
        // An unparsed peer has no proof that a valid operation was rejected.
        return Ok(None);
    };
    thread::Builder::new()
        .name("host-control".into())
        .spawn(move || {
            let _ = exchange(&host, stream, accepted_at, &admission, header);
        })
        .map(Some)
        .map_err(|_| "host_worker_unavailable".into())
}
struct ClientPermit(Arc<AtomicUsize>);
impl ClientPermit {
    fn acquire(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(counter.clone()))
    }
}
impl Drop for ClientPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn read_envelope(
    reader: &mut BufReader<UnixStream>,
    deadline: Instant,
) -> Result<HostSupervisorEnvelope, String> {
    let mut data = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("host_request_invalid".into());
        }
        reader
            .get_ref()
            .set_read_timeout(Some(remaining))
            .map_err(|_| "host_socket_failed")?;
        let bytes = reader.fill_buf().map_err(|_| "host_request_invalid")?;
        if bytes.is_empty() {
            return Err("host_request_invalid".into());
        }
        let count = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |position| position + 1);
        if data.len() + count > MAX_REQUEST {
            return Err("host_request_invalid".into());
        }
        data.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if data.last() == Some(&b'\n') {
            break;
        }
    }
    let envelope: HostSupervisorEnvelope =
        serde_json::from_slice(&data).map_err(|_| "host_request_invalid")?;
    envelope.validate()?;
    Ok(envelope)
}
fn helper_input_ready(stream: &mut UnixStream, helper: &HostHelperRequest) -> Result<(), String> {
    let response = HostSupervisorResponse::Success {
        schema: HOST_SUPERVISOR_SCHEMA.into(),
        result: HostSupervisorResult::HelperInputReady {
            operation_id: helper.operation_id.clone(),
            input_length: helper.input_length,
            input_digest: helper.input_digest.clone(),
        },
    };
    let mut bytes = serde_json::to_vec(&response).map_err(|_| "host_response_invalid")?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .map_err(|_| "host_response_unknown".into())
}
fn exchange(
    host: &Arc<Host>,
    mut stream: UnixStream,
    accepted_at: Instant,
    admission: &ClientAdmission,
    header: ClientPermit,
) -> Result<(), String> {
    let mut header = Some(header);
    let mut _execution = None;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    // SAFETY: socket FD and credential output storage are valid.
    let authenticated = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } == 0
        && cred.uid == host.config.controller_uid;
    if !authenticated {
        return Err("host_controller_unauthorized".into());
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "host_socket_failed")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "host_socket_failed")?;
    let mut response_started = false;
    let peer = stream.try_clone().map_err(|_| "host_socket_failed")?;
    let budget = probe::ProbeBudget::new(accepted_at + Duration::from_secs(30), Some(peer.as_fd()));
    let mut read_only_probe = false;
    let result = (|| {
        let mut reader = BufReader::new(stream.try_clone().map_err(|_| "host_socket_failed")?);
        let envelope = read_envelope(&mut reader, accepted_at + HEADER_TIMEOUT)?;
        _execution =
            Some(ClientPermit::acquire(&admission.executing, EXECUTION_LIMIT).ok_or("host_busy")?);
        // The execution slot now owns the full request, including response transfer.
        drop(header.take());
        read_only_probe = matches!(
            &envelope.request,
            HostSupervisorRequest::ObserveResources { .. } | HostSupervisorRequest::Inspect { .. }
        );
        // Bound full resource walks separately; task inspection does not consume
        // these permits because production cleanup also uses it.
        let _probe_permit = if matches!(
            &envelope.request,
            HostSupervisorRequest::ObserveResources { .. }
        ) {
            Some(PROBE_ADMISSION.try_acquire(&budget)?)
        } else {
            None
        };
        if let HostSupervisorRequest::Helper { helper } = envelope.request {
            helper_input_ready(&mut stream, &helper)?;
            let manager = host.existing(&helper.fence.environment_instance_id)?;
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(30))))
                .map_err(|_| "host_socket_failed")?;
            let input =
                helpers::receive_input(Path::new(&host.config.control_root), &helper, &mut reader)?;
            let mut writer = TrackedWriter {
                stream: &mut stream,
                written: 0,
            };
            let result = helpers::dispatch_helper(&manager, &helper, input, &mut writer);
            response_started = writer.written > 0;
            result?;
            Ok(None)
        } else if let HostSupervisorRequest::HelperOutput { helper, task_id } = envelope.request {
            let manager = host.existing(&helper.fence.environment_instance_id)?;
            let (task, mut stdout, mut stderr) = manager.helper_output(&helper, &task_id)?;
            stream
                .set_write_timeout(Some(Duration::from_secs(30)))
                .map_err(|_| "host_socket_failed")?;
            let mut writer = TrackedWriter {
                stream: &mut stream,
                written: 0,
            };
            let result =
                helpers::send_completed(&mut writer, task, &mut stdout, &mut stderr, helper.kind);
            response_started = writer.written > 0;
            result?;
            Ok(None)
        } else {
            host.handle(envelope, &budget).map(Some)
        }
    })();
    let response = match result {
        Ok(None) => return Ok(()),
        Ok(Some(result)) => HostSupervisorResponse::Success {
            schema: HOST_SUPERVISOR_SCHEMA.into(),
            result,
        },
        Err(error) if response_started => return Err(error),
        Err(error) => HostSupervisorResponse::Error {
            schema: HOST_SUPERVISOR_SCHEMA.into(),
            error,
        },
    };
    if read_only_probe {
        budget.check()?;
    }
    let mut data = serde_json::to_vec(&response).map_err(|_| "host_response_invalid")?;
    if data.len() >= MAX_RESPONSE {
        return Err("host_response_oversized".into());
    }
    data.push(b'\n');
    if read_only_probe {
        let mut pending = data.as_slice();
        while !pending.is_empty() {
            stream
                .set_write_timeout(Some(budget.remaining()?.min(Duration::from_secs(2))))
                .map_err(|_| "host_socket_failed")?;
            match stream.write(pending) {
                Ok(0) => return Err("host_response_unknown".into()),
                Ok(count) => pending = &pending[count..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("host_response_unknown".into()),
            }
        }
        Ok(())
    } else {
        stream
            .write_all(&data)
            .map_err(|_| "host_response_unknown".into())
    }
}
struct TrackedWriter<'a> {
    stream: &'a mut UnixStream,
    written: u64,
}
impl Write for TrackedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.stream.write(bytes)?;
        self.written += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod command_budget_tests;

#[cfg(test)]
mod admission_tests;
