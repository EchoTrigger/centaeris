//! gVisor's private cgroup v2 task domains and sealed, identity-isolated runner.
use super::*;
use crate::host_supervisor::linux::commands::{self, OwnedChild};
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
    sync::atomic::AtomicU64,
};

mod timing;

const CGROUP_ROOT: &str = "/centaeris-tasks";
static SERIAL: AtomicU64 = AtomicU64::new(0);

fn control_path(container_id: &str, path: &str) -> String {
    format!("/{container_id}{}", path.trim_end_matches('/'))
}

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
    let scoped_path = control_path(&target.container_id, path);
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
        &scoped_path,
        name,
    ];
    if let Some(value) = value {
        args.push(value);
    }
    let bytes = crate::host_supervisor::linux::command_bytes_unlocked(binary, &args)?;
    String::from_utf8(bytes).map_err(|_| "runsc_cgroup_response_invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_receipt_observations_use_host_receive_clock_and_reject_duplicates() {
        use std::io::Write;
        let (host, mut guest) = UnixStream::pair().unwrap();
        guest.write_all(b"{\"kind\":\"ready\",\"pid\":42}\n{\"kind\":\"leaderExited\",\"waitStatus\":0}\n{\"kind\":\"leaderExited\",\"waitStatus\":0}\n").unwrap();
        drop(guest);
        let state = Arc::new(Mutex::new(Events::default()));
        let mut times = [11_u64, 29, 31].into_iter();
        let mut records = Vec::new();
        read_events_with_observer(
            host,
            state.clone(),
            || times.next().unwrap(),
            |phase, at| {
                assert!(
                    state.try_lock().is_ok(),
                    "observation must not hold the event state lock"
                );
                records.push((phase, at));
            },
        );
        assert_eq!(records, vec![("taskReady", 11), ("taskLeaderExited", 29)]);
        let state = state.lock().unwrap();
        assert!(state.ready && state.leader == Some(0) && state.failed);
    }

    #[test]
    fn invalid_runner_receipt_never_becomes_a_timing_record() {
        use std::io::Write;
        for receipt in [
            b"{\"kind\":\"ready\",\"pid\":1}\n".as_slice(),
            b"{\"kind\":\"leaderExited\",\"waitStatus\":0}\n",
            b"not-json\n",
        ] {
            let (host, mut guest) = UnixStream::pair().unwrap();
            guest.write_all(receipt).unwrap();
            drop(guest);
            let state = Arc::new(Mutex::new(Events::default()));
            let mut records = Vec::new();
            read_events_with_observer(
                host,
                state.clone(),
                || 17,
                |phase, at| {
                    assert!(
                        state.try_lock().is_ok(),
                        "observation must not hold the event state lock"
                    );
                    records.push((phase, at));
                },
            );
            assert!(records.is_empty());
            assert!(state.lock().unwrap().failed);
        }
    }

    #[test]
    fn task_controls_use_the_attested_containers_cgroup_namespace() {
        let first = "a".repeat(64);
        let second = "b".repeat(64);
        let task = format!("/centaeris-tasks/task-{}", "c".repeat(64));
        assert_eq!(control_path(&first, "/"), format!("/{first}"));
        assert_eq!(control_path(&first, &task), format!("/{first}{task}"));
        assert_eq!(control_path(&second, &task), format!("/{second}{task}"));
        assert_ne!(control_path(&first, &task), control_path(&second, &task));
    }

    #[test]
    fn exited_proxy_preserves_buffered_short_task_receipts() {
        use std::{io::Write, sync::Barrier};

        let (host, mut guest) = UnixStream::pair().unwrap();
        let state = Arc::new(Mutex::new(Events::default()));
        let release_reader = Arc::new(Barrier::new(2));
        let read_state = state.clone();
        let read_stream = host.try_clone().unwrap();
        let read_release = release_reader.clone();
        let reader = thread::spawn(move || {
            read_release.wait();
            read_events(read_stream, read_state);
        });
        guest
            .write_all(
                b"{\"kind\":\"ready\",\"pid\":42}\n{\"kind\":\"leaderExited\",\"waitStatus\":0}\n",
            )
            .unwrap();
        drop(guest);
        let mut proxy = OwnedChild::spawn(Command::new("/bin/sh").args(["-c", "exit 0"])).unwrap();
        assert!(proxy.wait().unwrap().success());
        let mut child = RunscChild {
            proxy: Some(proxy),
            state,
            stream: host,
            reader: Some(reader),
        };
        let mut released = false;
        let outcome = wait_until_ready(&mut child, Duration::from_secs(1), |proxy| {
            let status = proxy.try_wait()?;
            if status.is_some() && !released {
                released = true;
                release_reader.wait();
            }
            Ok(status)
        });
        assert!(
            released,
            "test must observe proxy exit before releasing receipts"
        );
        assert_eq!(outcome, Ok(()), "buffered Ready survives proxy exit");
        let began = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                began.elapsed() < Duration::from_secs(1),
                "leader receipt missing"
            );
            thread::yield_now();
        };
        assert_eq!(status.code(), Some(0));
    }

    fn exited_proxy_with_event_channel() -> (RunscChild, UnixStream) {
        let (host, guest) = UnixStream::pair().unwrap();
        let state = Arc::new(Mutex::new(Events::default()));
        let read_stream = host.try_clone().unwrap();
        let read_state = state.clone();
        let reader = thread::spawn(move || read_events(read_stream, read_state));
        let mut proxy = OwnedChild::spawn(Command::new("/bin/sh").args(["-c", "exit 0"])).unwrap();
        assert!(proxy.wait().unwrap().success());
        (
            RunscChild {
                proxy: Some(proxy),
                state,
                stream: host,
                reader: Some(reader),
            },
            guest,
        )
    }

    #[test]
    fn event_eof_before_ready_rejects_launch_without_waiting_for_timeout() {
        let (mut child, guest) = exited_proxy_with_event_channel();
        drop(guest);
        let began = Instant::now();
        assert_eq!(
            wait_until_ready(&mut child, Duration::from_secs(5), OwnedChild::try_wait),
            Err("task_launch_unknown".into())
        );
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "closed event channel waited for the launch timeout"
        );
    }

    #[test]
    fn malformed_or_out_of_order_startup_receipts_reject_launch() {
        use std::io::Write;

        let receipts: &[&[u8]] = &[
            b"not-json\n",
            b"{\"kind\":\"ready\",\"pid\":42,\"extra\":true}\n",
            b"{\"kind\":\"leaderExited\",\"waitStatus\":0}\n",
            b"{\"kind\":\"ready\",\"pid\":1}\n",
            b"{\"kind\":\"ready\",\"pid\":42}",
        ];
        for receipt in receipts {
            let (mut child, mut guest) = exited_proxy_with_event_channel();
            guest.write_all(receipt).unwrap();
            drop(guest);
            assert_eq!(
                wait_until_ready(&mut child, Duration::from_secs(1), OwnedChild::try_wait),
                Err("task_launch_unknown".into()),
                "invalid startup receipt was accepted: {receipt:?}"
            );
        }
    }

    #[test]
    fn exited_proxy_without_receipts_waits_for_the_bounded_startup_budget() {
        let (mut child, _guest) = exited_proxy_with_event_channel();
        let timeout = Duration::from_millis(30);
        let began = Instant::now();
        assert_eq!(
            wait_until_ready(&mut child, timeout, OwnedChild::try_wait),
            Err("task_launch_unknown".into())
        );
        let elapsed = began.elapsed();
        assert!(elapsed >= timeout, "proxy exit bypassed the receipt budget");
        assert!(
            elapsed < Duration::from_secs(1),
            "missing startup receipt exceeded its bounded budget"
        );
    }
}

pub(super) fn events(target: &OciTarget, path: &Path) -> Result<String, String> {
    let gate = commands::instance_gate(
        &target.runtime_path,
        &target.runtime_root,
        &target.container_id,
    );
    let _commands = gate.lock().map_err(|_| "host_command_failed")?;
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
    let gate = commands::instance_gate(
        &target.runtime_path,
        &target.runtime_root,
        &target.container_id,
    );
    let _commands = gate.lock().map_err(|_| "host_command_failed")?;
    target.verify_identity()?;
    kill_unlocked(target, task_id(path)?)
}

pub(super) fn initialize(target: &OciTarget) -> Result<(), String> {
    let gate = commands::instance_gate(
        &target.runtime_path,
        &target.runtime_root,
        &target.container_id,
    );
    let _commands = gate.lock().map_err(|_| "host_command_failed")?;
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
    let gate = commands::instance_gate(
        &target.runtime_path,
        &target.runtime_root,
        &target.container_id,
    );
    let _commands = gate.lock().map_err(|_| "host_command_failed")?;
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
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(diagnostic));
    let mut child = OwnedChild::spawn(&mut command).map_err(|_| "runsc_control_failed")?;
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
    proxy: Option<OwnedChild>,
    state: Arc<Mutex<Events>>,
    stream: UnixStream,
    reader: Option<thread::JoinHandle<()>>,
}
impl RunscChild {
    #[cfg(test)]
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
        }
        // Preserve the proxy grace period; the cleanup worker owns any outstanding wait.
        self.proxy.take();
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
fn read_events(stream: UnixStream, state: Arc<Mutex<Events>>) {
    read_events_with_observer(stream, state, || (), |_, ()| {});
}

fn read_events_with_observer<T>(
    stream: UnixStream,
    state: Arc<Mutex<Events>>,
    mut now: impl FnMut() -> T,
    mut observe: impl FnMut(&'static str, T),
) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut bytes = Vec::new();
        let result = Read::by_ref(&mut reader)
            .take(1025)
            .read_until(b'\n', &mut bytes);
        if matches!(result, Ok(0)) {
            break;
        }
        let received_at = now();
        let parsed = if result.is_ok() && bytes.len() <= 1024 && bytes.last() == Some(&b'\n') {
            serde_json::from_slice::<RunnerEvent>(&bytes).ok()
        } else {
            None
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        let phase = match parsed {
            Some(RunnerEvent::Ready { pid })
                if pid > 1 && !state.ready && state.leader.is_none() =>
            {
                state.ready = true;
                "taskReady"
            }
            Some(RunnerEvent::LeaderExited { wait_status })
                if state.ready && state.leader.is_none() =>
            {
                state.leader = Some(wait_status);
                "taskLeaderExited"
            }
            _ => {
                state.failed = true;
                break;
            }
        };
        drop(state);
        observe(phase, received_at);
    }
}

fn wait_until_ready(
    child: &mut RunscChild,
    timeout: Duration,
    mut poll_proxy: impl FnMut(&mut OwnedChild) -> std::io::Result<Option<std::process::ExitStatus>>,
) -> Result<(), String> {
    let began = Instant::now();
    loop {
        // Read completion before state: a finished reader has published all buffered events.
        let reader_finished = child
            .reader
            .as_ref()
            .is_some_and(|reader| reader.is_finished());
        let (ready, failed) = {
            let state = child.state.lock().map_err(|_| "task_service_failed")?;
            (state.ready, state.failed)
        };
        if ready && !failed {
            return Ok(());
        }
        if failed || reader_finished || began.elapsed() >= timeout {
            return Err("task_launch_unknown".into());
        }
        // Proxy exit does not establish that its buffered Ready receipt has been consumed.
        // Keep sole wait ownership while allowing the event reader the remaining budget.
        let _ = poll_proxy(child.proxy.as_mut().expect("owned proxy"))
            .map_err(|_| "task_launch_unknown")?;
        thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn spawn(
    target: &OciTarget,
    id: &str,
    request: launcher::TaskProcess<'_>,
    domain: &Path,
    trusted: Option<&TrustedLaunch>,
) -> Result<Spawned, String> {
    let observation = timing::TaskTiming::new(id);
    if target.runtime != HostOciRuntime::Runsc {
        return Err("runsc_backend_required".into());
    }
    let gate = commands::instance_gate(
        &target.runtime_path,
        &target.runtime_root,
        &target.container_id,
    );
    let _commands = gate.lock().map_err(|_| "host_command_failed")?;
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
        .spawn(move || {
            read_events_with_observer(
                read_stream,
                read_state,
                || observation.sample(),
                |phase, received_at| observation.record(phase, received_at),
            )
        })
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
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut proxy = OwnedChild::spawn(&mut command).map_err(|_| "task_launch_unknown")?;
    let input = proxy.stdin.take();
    let stdout = proxy.stdout.take().expect("piped stdout");
    let stderr = proxy.stderr.take().expect("piped stderr");
    child.proxy = Some(proxy);
    drop(guest);
    if let Err(error) = wait_until_ready(&mut child, Duration::from_secs(5), OwnedChild::try_wait) {
        let _ = kill_unlocked(target, task_id(domain)?);
        return Err(error);
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
