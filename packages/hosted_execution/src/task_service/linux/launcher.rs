//! Launch into an attested OCI container, never onto the supervisor host.
use super::*;
use std::os::unix::process::{CommandExt, ExitStatusExt};

#[derive(Clone)]
pub(crate) struct OciTarget {
    pub runtime: crate::host_supervisor::HostOciRuntime,
    pub helper_binary: Option<Arc<HelperBinary>>,
    pub runtime_path: PathBuf,
    pub runtime_root: PathBuf,
    pub container_id: String,
    pub process: serde_json::Value,
    pub launch_root: PathBuf,
    pub init_pid: u32,
    pub init_start_time: u64,
    pub bundle: PathBuf,
    pub spec_digest: String,
}

pub(super) struct TaskProcess<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a str,
    pub env: &'a std::collections::BTreeMap<String, String>,
}
pub(super) enum TaskChild {
    #[cfg(test)]
    Local(Child),
    Native {
        pid: u32,
        status: Option<std::process::ExitStatus>,
    },
    Runsc(super::runsc::RunscChild),
}
impl TaskChild {
    pub(super) fn id(&self) -> u32 {
        match self {
            #[cfg(test)]
            Self::Local(child) => child.id(),
            Self::Native { pid, .. } => *pid,
            Self::Runsc(child) => child.id(),
        }
    }
    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Runsc(child) => child.try_wait(),
            #[cfg(test)]
            Self::Local(child) => child.try_wait(),
            Self::Native { pid, status } => {
                if status.is_some() {
                    return Ok(*status);
                }
                let mut raw = 0;
                // SAFETY: this supervisor is the subreaper; this PID is exclusively owned by this handle.
                let result = unsafe { libc::waitpid(*pid as i32, &mut raw, libc::WNOHANG) };
                if result < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if result > 0 {
                    *status = Some(std::process::ExitStatus::from_raw(raw));
                }
                Ok(*status)
            }
        }
    }
}

pub(super) struct Spawned {
    pub child: TaskChild,
    pub input: Option<ChildStdin>,
    pub stdout: std::process::ChildStdout,
    pub stderr: std::process::ChildStderr,
}

impl OciTarget {
    pub(super) fn verify_identity(&self) -> Result<(), String> {
        use crate::host_supervisor::linux::observations::{measure, Phase};
        measure(Phase::RuntimeIdentity, || self.verify_identity_inner())
    }

    fn verify_identity_inner(&self) -> Result<(), String> {
        use crate::host_supervisor::linux::{command_json_unlocked, start_time};
        if start_time(self.init_pid)? != self.init_start_time {
            return Err("host_registered_target_changed".into());
        }
        let state = command_json_unlocked(
            self.runtime_path
                .to_str()
                .ok_or("host_configuration_invalid")?,
            &[
                "--root",
                self.runtime_root
                    .to_str()
                    .ok_or("host_configuration_invalid")?,
                "state",
                &self.container_id,
            ],
        )?;
        if state["id"] != self.container_id
            || state["pid"] != self.init_pid
            || state["status"] != "running"
            || state["bundle"].as_str() != self.bundle.to_str()
        {
            return Err("host_registered_target_changed".into());
        }
        let spec = crate::host_supervisor::linux::read_bundle_spec(&self.bundle)?;
        if format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&spec).map_err(|_| "host_bundle_unknown")?)
        ) != self.spec_digest
        {
            return Err("host_registered_target_changed".into());
        }
        Ok(())
    }
    pub(super) fn spawn(
        &self,
        id: &str,
        request: TaskProcess<'_>,
        cgroup: &Path,
        trusted: Option<&TrustedLaunch>,
    ) -> Result<Spawned, String> {
        if self.runtime == crate::host_supervisor::HostOciRuntime::Runsc {
            return super::runsc::spawn(self, id, request, cgroup, trusted);
        }
        let TaskProcess {
            program,
            args,
            cwd,
            env,
        } = request;
        let _commands = crate::host_supervisor::linux::command_lock()?;
        self.verify_identity()?;
        let process = match trusted {
            Some(launch) => launch.process(&self.process)?,
            None => crate::host_supervisor::task_process(&self.process, program, args, cwd, env)?,
        };
        let spec = self.launch_root.join(format!("{id}.process.json"));
        let pidfile = self.launch_root.join(format!("{id}.pid"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&spec)
            .map_err(|_| "task_launch_state_failed")?;
        serde_json::to_writer(&mut file, &process).map_err(|_| "task_launch_state_failed")?;
        file.sync_all().map_err(|_| "task_launch_state_failed")?;
        let result = (|| {
            let mut command = Command::new(&self.runtime_path);
            command
                .arg("--root")
                .arg(&self.runtime_root)
                .arg("exec")
                .arg("--detach")
                .arg("--process")
                .arg(&spec)
                .arg("--pid-file")
                .arg(&pidfile)
                .arg("--cgroup")
                .arg(format!("tasks/{id}"));
            if let Some(launch) = trusted {
                let fd = launch.binary.file.as_raw_fd();
                command.arg("--preserve-fds").arg("1");
                // SAFETY: dup2/fcntl are async-signal-safe. The sealed binary
                // remains owned by the host throughout spawn; no host path FD
                // or writable binary is exposed to the workload.
                unsafe {
                    command.pre_exec(move || {
                        if libc::dup2(fd, 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
            command
                .arg(&self.container_id)
                .env_clear()
                .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut launcher = command.spawn().map_err(|_| "task_launch_unknown")?;
            let input = launcher.stdin.take();
            let stdout = launcher.stdout.take().expect("piped stdout");
            let stderr = launcher.stderr.take().expect("piped stderr");
            let began = Instant::now();
            let status = loop {
                match launcher.try_wait().map_err(|_| "task_launch_unknown")? {
                    Some(status) => break status,
                    None if began.elapsed() < Duration::from_secs(5) => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    None => {
                        let _ = cgroup_kill(cgroup);
                        let _ = launcher.kill();
                        let _ = launcher.wait();
                        return Err("task_launch_unknown".into());
                    }
                }
            };
            if !status.success() {
                let _ = cgroup_kill(cgroup);
                return Err("task_launch_unknown".into());
            }
            if let Err(error) = self.verify_identity() {
                let _ = cgroup_kill(cgroup);
                return Err(error);
            }
            let pid = fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|p| *p > 1)
                .ok_or("task_launch_unknown")?;
            // PID reuse is excluded while our adopted leader is unreaped. Do not
            // turn a successful launcher exit into a workload exit receipt.
            let proc = fs::read_to_string(format!("/proc/{pid}/stat"))
                .map_err(|_| "task_launch_unknown")?;
            let parent = proc
                .rsplit_once(')')
                .and_then(|(_, s)| s.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u32>().ok());
            if parent != Some(std::process::id()) {
                let _ = cgroup_kill(cgroup);
                return Err("task_leader_ownership_unknown".into());
            }
            let membership = fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .map_err(|_| "task_leader_cgroup_unknown")?;
            let expected = cgroup
                .to_str()
                .and_then(|s| s.strip_prefix("/sys/fs/cgroup"))
                .ok_or("task_cgroup_path_invalid")?;
            let actual = membership.lines().find_map(|line| line.strip_prefix("0::"));
            // Exited, unreaped leaders may already have left their cgroup. runc
            // has confirmed exec setup; a live leader must still match exactly.
            let zombie = proc
                .rsplit_once(')')
                .and_then(|(_, s)| s.split_whitespace().next())
                == Some("Z");
            let empty = fs::read_to_string(cgroup.join("cgroup.events"))
                .is_ok_and(|s| s.lines().any(|l| l == "populated 0"));
            if actual != Some(expected) && !(zombie && empty) {
                let _ = cgroup_kill(cgroup);
                return Err("task_leader_cgroup_rejected".into());
            }
            crate::host_supervisor::linux::MANAGED_PIDS
                .lock()
                .map_err(|_| "task_service_failed")?
                .insert(pid);
            Ok(Spawned {
                child: TaskChild::Native { pid, status: None },
                input,
                stdout,
                stderr,
            })
        })();
        let _ = fs::remove_file(spec);
        let _ = fs::remove_file(pidfile);
        result
    }
}
impl Drop for TaskChild {
    fn drop(&mut self) {
        if let Ok(mut pids) = crate::host_supervisor::linux::MANAGED_PIDS.lock() {
            pids.remove(&self.id());
        }
    }
}
