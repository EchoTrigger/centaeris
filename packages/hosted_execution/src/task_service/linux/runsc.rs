//! gVisor's private cgroup v2 task domains and sealed, identity-isolated runner.
use super::*;
use crate::{
    host_supervisor::HostOciRuntime,
    runsc_task::{RunnerAction, RunnerEvent, RunnerRequest},
};
use std::{
    io::{BufRead, BufReader},
    net::Shutdown,
    os::{
        fd::{FromRawFd, RawFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::Child,
    sync::atomic::AtomicU64,
};

const CGROUP_ROOT: &str = "/centaeris-tasks";
static SERIAL: AtomicU64 = AtomicU64::new(0);

fn task_id(path: &Path) -> Result<&str, String> {
    let id = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("task_cgroup_path_invalid")?;
    RunnerRequest {
        action: RunnerAction::Prepare,
        task_id: id.into(),
        process: None,
    }
    .validate()?;
    Ok(id)
}

fn control_unlocked(
    target: &OciTarget,
    path: &str,
    name: &str,
    value: Option<&str>,
) -> Result<String, String> {
    let root = target
        .runtime_root
        .to_str()
        .ok_or("host_configuration_invalid")?;
    let binary = target
        .runtime_path
        .to_str()
        .ok_or("host_configuration_invalid")?;
    let mut args = vec![
        "--root",
        root,
        if value.is_some() {
            "write-control"
        } else {
            "read-control"
        },
        &target.container_id,
        "pids",
        path,
        name,
    ];
    if let Some(value) = value {
        args.push(value);
    }
    let bytes = crate::host_supervisor::linux::command_bytes_unlocked(binary, &args)?;
    String::from_utf8(bytes).map_err(|_| "runsc_cgroup_response_invalid".into())
}

pub(super) fn events(target: &OciTarget, path: &Path) -> Result<String, String> {
    let _commands = crate::host_supervisor::linux::COMMAND_LOCK
        .lock()
        .map_err(|_| "host_command_failed")?;
    target.verify_identity()?;
    control_unlocked(
        target,
        &format!("{CGROUP_ROOT}/{}", task_id(path)?),
        "cgroup.events",
        None,
    )
}

fn kill_unlocked(target: &OciTarget, id: &str) -> Result<bool, String> {
    let path = format!("{CGROUP_ROOT}/{id}");
    control_unlocked(target, &path, "cgroup.kill", Some("1"))?;
    Ok(control_unlocked(target, &path, "cgroup.events", None)?
        .lines()
        .any(|l| l == "populated 0"))
}

pub(super) fn kill(target: &OciTarget, path: &Path) -> Result<bool, String> {
    let _commands = crate::host_supervisor::linux::COMMAND_LOCK
        .lock()
        .map_err(|_| "host_command_failed")?;
    target.verify_identity()?;
    kill_unlocked(target, task_id(path)?)
}

pub(super) fn initialize(target: &OciTarget) -> Result<(), String> {
    let _commands = crate::host_supervisor::linux::COMMAND_LOCK
        .lock()
        .map_err(|_| "host_command_failed")?;
    if target.helper_binary.is_none() {
        return Err("host_runsc_runner_required".into());
    }
    target.verify_identity()?;
    control_unlocked(target, "/", "cgroup.controllers", None)
        .map_err(|_| "runsc_cgroup_v2_required")?;
    Ok(())
}

fn duplicate(fd: RawFd) -> Result<File, String> {
    // Reserve high FDs so installing 3/4/5 cannot overwrite another import source.
    let result = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 16) };
    if result < 0 {
        return Err("runsc_runner_descriptor_failed".into());
    }
    Ok(unsafe { File::from_raw_fd(result) })
}

struct Imports {
    _files: Vec<File>,
    spec: PathBuf,
}
impl Drop for Imports {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.spec);
    }
}

fn command(
    target: &OciTarget,
    request: &RunnerRequest,
    control: Option<RawFd>,
) -> Result<(Command, Imports), String> {
    request.validate()?;
    let binary = target
        .helper_binary
        .as_ref()
        .ok_or("host_runsc_runner_required")?;
    let bytes = serde_json::to_vec(request).map_err(|_| "runsc_runner_input_failed")?;
    if bytes.len() > 128 * 1024 {
        return Err("runsc_runner_input_oversized".into());
    }
    let input = trusted::sealed_file(&bytes)?;
    let mut files = vec![
        duplicate(binary.file.as_raw_fd())?,
        duplicate(input.as_raw_fd())?,
    ];
    if let Some(fd) = control {
        files.push(duplicate(fd)?);
    }
    let descriptors = files.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>();
    let uid = target.process["user"]["uid"]
        .as_u64()
        .ok_or("host_process_invalid")?;
    let control_uid = if uid == 2_147_483_647 {
        2_147_483_646u32
    } else {
        2_147_483_647u32
    };
    let mut process = target.process.clone();
    process["args"] = serde_json::json!(["execution_agent", "runsc-control"]);
    process["cwd"] = serde_json::json!("/");
    process["user"] = serde_json::json!({"uid":control_uid,"gid":control_uid,"additionalGids":[]});
    process["env"] = serde_json::json!([
        "PATH=/usr/sbin:/usr/bin:/sbin:/bin",
        "HOME=/",
        "LANG=C.UTF-8"
    ]);
    process["terminal"] = serde_json::json!(false);
    let caps = serde_json::json!([
        "CAP_SYS_ADMIN",
        "CAP_SETPCAP",
        "CAP_SETUID",
        "CAP_SETGID",
        "CAP_DAC_OVERRIDE",
        "CAP_CHOWN",
        "CAP_FOWNER",
        "CAP_KILL"
    ]);
    process["capabilities"] = serde_json::json!({"bounding":caps,"permitted":caps,"effective":caps,"inheritable":caps,"ambient":caps});
    let spec = target.launch_root.join(format!(
        "{}.{}.{}.runsc.json",
        request.task_id,
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&spec)
        .map_err(|_| "task_launch_state_failed")?;
    let imports = Imports {
        _files: files,
        spec,
    };
    serde_json::to_writer(&mut file, &process).map_err(|_| "task_launch_state_failed")?;
    file.sync_all().map_err(|_| "task_launch_state_failed")?;
    let mut command = Command::new(&target.runtime_path);
    command
        .arg("--root")
        .arg(&target.runtime_root)
        .arg("exec")
        .arg("--process")
        .arg(&imports.spec)
        .arg("--exec-fd")
        .arg("3")
        .arg("--pass-fd")
        .arg("3:3")
        .arg("--pass-fd")
        .arg("4:4");
    if control.is_some() {
        command.arg("--pass-fd").arg("5:5");
    }
    command
        .arg(&target.container_id)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    unsafe {
        command.pre_exec(move || {
            for (index, fd) in descriptors.iter().enumerate() {
                let destination = 3 + index as i32;
                if libc::dup2(*fd, destination) < 0
                    || libc::fcntl(destination, libc::F_SETFD, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok((command, imports))
}

pub(super) fn administer(
    target: &OciTarget,
    path: &Path,
    action: RunnerAction,
) -> Result<(), String> {
    let _commands = crate::host_supervisor::linux::COMMAND_LOCK
        .lock()
        .map_err(|_| "host_command_failed")?;
    target.verify_identity()?;
    let request = RunnerRequest {
        action,
        task_id: task_id(path)?.into(),
        process: None,
    };
    let (mut command, _imports) = command(target, &request, None)?;
    let diagnostic_path = target.launch_root.join("runsc-control.last-error");
    let diagnostic = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&diagnostic_path)
        .map_err(|_| "runsc_control_failed")?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(diagnostic))
        .spawn()
        .map_err(|_| "runsc_control_failed")?;
    let began = Instant::now();
    loop {
        match child.try_wait().map_err(|_| "runsc_control_failed")? {
            Some(status) => {
                return if status.success() {
                    let _ = fs::remove_file(&diagnostic_path);
                    Ok(())
                } else {
                    let mut detail = String::new();
                    if let Ok(file) = File::open(&diagnostic_path) {
                        let _ = file.take(4096).read_to_string(&mut detail);
                    }
                    eprintln!("runsc administrative helper failed: {detail}");
                    Err("runsc_control_failed".into())
                }
            }
            None if began.elapsed() < Duration::from_secs(5) => {
                thread::sleep(Duration::from_millis(10))
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("runsc_control_unknown".into());
            }
        }
    }
}

#[derive(Default)]
struct Events {
    ready: bool,
    leader: Option<i32>,
    failed: bool,
}

pub(super) struct RunscChild {
    proxy: Option<Child>,
    state: Arc<Mutex<Events>>,
    stream: UnixStream,
    reader: Option<thread::JoinHandle<()>>,
}
impl RunscChild {
    pub(super) fn id(&self) -> u32 {
        self.proxy.as_ref().expect("launched proxy").id()
    }
    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let proxy = self.proxy.as_mut().expect("launched proxy").try_wait()?;
        let events = self
            .state
            .lock()
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EIO))?;
        if events.failed {
            return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
        }
        if let Some(raw) = events.leader {
            return Ok(Some(std::process::ExitStatus::from_raw(raw)));
        }
        if proxy.is_some() && self.reader.as_ref().is_some_and(|w| w.is_finished()) {
            return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
        }
        Ok(None)
    }
}
impl Drop for RunscChild {
    fn drop(&mut self) {
        if let Some(proxy) = &mut self.proxy {
            let began = Instant::now();
            while matches!(proxy.try_wait(), Ok(None)) && began.elapsed() < Duration::from_secs(1) {
                thread::sleep(Duration::from_millis(5));
            }
            if matches!(proxy.try_wait(), Ok(None)) {
                let _ = proxy.kill();
            }
            let _ = proxy.wait();
            if let Ok(mut pids) = crate::host_supervisor::linux::MANAGED_PIDS.lock() {
                pids.remove(&proxy.id());
            }
        }
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_events(stream: UnixStream, state: Arc<Mutex<Events>>) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut bytes = Vec::new();
        let result = Read::by_ref(&mut reader)
            .take(1025)
            .read_until(b'\n', &mut bytes);
        if matches!(result, Ok(0)) {
            break;
        }
        let parsed = if result.is_ok() && bytes.len() <= 1024 && bytes.last() == Some(&b'\n') {
            serde_json::from_slice::<RunnerEvent>(&bytes).ok()
        } else {
            None
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        match parsed {
            Some(RunnerEvent::Ready { pid })
                if pid > 1 && !state.ready && state.leader.is_none() =>
            {
                state.ready = true
            }
            Some(RunnerEvent::LeaderExited { wait_status })
                if state.ready && state.leader.is_none() =>
            {
                state.leader = Some(wait_status)
            }
            _ => {
                state.failed = true;
                break;
            }
        }
    }
}

pub(super) fn spawn(
    target: &OciTarget,
    id: &str,
    request: launcher::TaskProcess<'_>,
    domain: &Path,
    trusted: Option<&TrustedLaunch>,
) -> Result<Spawned, String> {
    if target.runtime != HostOciRuntime::Runsc {
        return Err("runsc_backend_required".into());
    }
    let _commands = crate::host_supervisor::linux::COMMAND_LOCK
        .lock()
        .map_err(|_| "host_command_failed")?;
    target.verify_identity()?;
    let process = match trusted {
        Some(launch) => launch.process(&target.process)?,
        None => crate::host_supervisor::task_process(
            &target.process,
            request.program,
            request.args,
            request.cwd,
            request.env,
        )?,
    };
    let (host, guest) = UnixStream::pair().map_err(|_| "runsc_control_channel_failed")?;
    let state = Arc::new(Mutex::new(Events::default()));
    let read_stream = host
        .try_clone()
        .map_err(|_| "runsc_control_channel_failed")?;
    let read_state = state.clone();
    let reader = thread::Builder::new()
        .name("runsc-task-events".into())
        .spawn(move || read_events(read_stream, read_state))
        .map_err(|_| "task_thread_capacity")?;
    let mut child = RunscChild {
        proxy: None,
        state,
        stream: host,
        reader: Some(reader),
    };
    let input = RunnerRequest {
        action: RunnerAction::Start,
        task_id: id.into(),
        process: Some(process),
    };
    let (mut command, _imports) = command(target, &input, Some(guest.as_raw_fd()))?;
    let mut proxy = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "task_launch_unknown")?;
    let input = proxy.stdin.take();
    let stdout = proxy.stdout.take().expect("piped stdout");
    let stderr = proxy.stderr.take().expect("piped stderr");
    crate::host_supervisor::linux::MANAGED_PIDS
        .lock()
        .map_err(|_| "task_service_failed")?
        .insert(proxy.id());
    child.proxy = Some(proxy);
    drop(guest);
    let began = Instant::now();
    loop {
        let (ready, failed) = {
            let state = child.state.lock().map_err(|_| "task_service_failed")?;
            (state.ready, state.failed)
        };
        if ready && !failed {
            break;
        }
        if failed
            || began.elapsed() >= Duration::from_secs(5)
            || child
                .proxy
                .as_mut()
                .expect("owned proxy")
                .try_wait()
                .map_err(|_| "task_launch_unknown")?
                .is_some()
        {
            let _ = kill_unlocked(target, task_id(domain)?);
            return Err("task_launch_unknown".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    if let Err(error) = target.verify_identity() {
        let _ = kill_unlocked(target, id);
        return Err(error);
    }
    Ok(Spawned {
        child: TaskChild::Runsc(child),
        input,
        stdout,
        stderr,
    })
}
