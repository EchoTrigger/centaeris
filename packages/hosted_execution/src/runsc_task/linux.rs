use super::*;
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{fd::FromRawFd, unix::process::CommandExt},
    process::{Command, Stdio},
};

const ROOT: &str = "/sys/fs/cgroup/centaeris-tasks";
const ALLOWED_CAPS: [(&str, u32); 4] = [
    ("CAP_CHOWN", 0),
    ("CAP_DAC_OVERRIDE", 1),
    ("CAP_FOWNER", 3),
    ("CAP_KILL", 5),
];

fn syscall_result(value: i32) -> std::io::Result<()> {
    if value < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

struct Identity {
    uid: u32,
    gid: u32,
    groups: Vec<u32>,
    permitted: u64,
    effective: u64,
    inheritable: u64,
    bounding: u64,
    ambient: u64,
}
impl Identity {
    fn parse(process: &serde_json::Value) -> Result<Self, String> {
        let number = |v: &serde_json::Value| {
            v.as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| "runsc_runner_identity_invalid".to_owned())
        };
        let groups = process["user"]
            .get("additionalGids")
            .map(|v| {
                v.as_array()
                    .filter(|v| v.len() <= 32)
                    .ok_or_else(|| "runsc_runner_identity_invalid".to_owned())?
                    .iter()
                    .map(number)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let caps = |name: &str| -> Result<u64, String> {
            let Some(values) = process["capabilities"].get(name) else {
                return Ok(0);
            };
            values
                .as_array()
                .ok_or("runsc_runner_capabilities_invalid")?
                .iter()
                .try_fold(0, |set, v| {
                    let (_, bit) = ALLOWED_CAPS
                        .iter()
                        .find(|(name, _)| v.as_str() == Some(*name))
                        .ok_or("runsc_runner_capabilities_invalid")?;
                    Ok(set | (1u64 << bit))
                })
        };
        Ok(Self {
            uid: number(&process["user"]["uid"])?,
            gid: number(&process["user"]["gid"])?,
            groups,
            permitted: caps("permitted")?,
            effective: caps("effective")?,
            inheritable: caps("inheritable")?,
            bounding: caps("bounding")?,
            ambient: caps("ambient")?,
        })
    }
    /// Called in the forked child, before any untrusted executable is entered.
    unsafe fn restore(&self) -> std::io::Result<()> {
        #[repr(C)]
        struct Header {
            version: u32,
            pid: i32,
        }
        #[repr(C)]
        struct Data {
            effective: u32,
            permitted: u32,
            inheritable: u32,
        }
        // SAFETY: all pointers reference preallocated, valid data; only this child changes credentials.
        unsafe {
            syscall_result(libc::prctl(libc::PR_SET_KEEPCAPS, 1, 0, 0, 0))?;
            syscall_result(libc::setgroups(self.groups.len(), self.groups.as_ptr()))?;
            syscall_result(libc::setresgid(self.gid, self.gid, self.gid))?;
            syscall_result(libc::setresuid(self.uid, self.uid, self.uid))?;
            syscall_result(libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_CLEAR_ALL,
                0,
                0,
                0,
            ))?;
            for bit in 0..64 {
                if self.bounding & (1u64 << bit) == 0
                    && libc::prctl(libc::PR_CAPBSET_DROP, bit, 0, 0, 0) < 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL)
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            let header = Header {
                version: 0x20080522,
                pid: 0,
            };
            let data = [
                Data {
                    effective: self.effective as u32,
                    permitted: self.permitted as u32,
                    inheritable: self.inheritable as u32,
                },
                Data {
                    effective: (self.effective >> 32) as u32,
                    permitted: (self.permitted >> 32) as u32,
                    inheritable: (self.inheritable >> 32) as u32,
                },
            ];
            syscall_result(libc::syscall(libc::SYS_capset, &header, data.as_ptr()) as i32)?;
            for bit in 0..64 {
                if self.ambient & (1u64 << bit) != 0 {
                    syscall_result(libc::prctl(
                        libc::PR_CAP_AMBIENT,
                        libc::PR_CAP_AMBIENT_RAISE,
                        bit,
                        0,
                        0,
                    ))?;
                }
            }
            syscall_result(libc::prctl(libc::PR_SET_KEEPCAPS, 0, 0, 0, 0))?;
            syscall_result(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
        }
    }
}

fn private_mount() -> Result<(), String> {
    // SAFETY: fixed mount names; only this trusted process receives CAP_SYS_ADMIN.
    unsafe {
        syscall_result(libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0))
            .map_err(|_| "runsc_control_isolation_unavailable")?;
        syscall_result(libc::unshare(libc::CLONE_NEWNS))
            .map_err(|_| "runsc_control_isolation_unavailable")?;
        syscall_result(libc::mount(
            std::ptr::null(),
            c"/sys/fs/cgroup".as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            std::ptr::null(),
        ))
        .map_err(|_| "runsc_cgroup_v2_required")?;
    }
    fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")
        .map_err(|_| "runsc_cgroup_v2_required")?;
    Ok(())
}

fn event(file: &mut File, event: &RunnerEvent) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(event).map_err(|_| "runsc_runner_event_failed")?;
    bytes.push(b'\n');
    file.write_all(&bytes)
        .map_err(|_| "runsc_runner_event_failed".into())
}

pub fn run() -> Result<(), String> {
    // These descriptors are provided only by the host through runsc's explicit FD import.
    let mut input = unsafe { File::from_raw_fd(4) };
    let mut bytes = Vec::new();
    Read::by_ref(&mut input)
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "runsc_runner_input_failed")?;
    if bytes.len() > 128 * 1024 {
        return Err("runsc_runner_input_oversized".into());
    }
    let request: RunnerRequest =
        serde_json::from_slice(&bytes).map_err(|_| "runsc_runner_request_invalid")?;
    request.validate()?;
    drop(input);
    private_mount()?;
    let path = format!("{ROOT}/{}", request.task_id);
    match request.action {
        RunnerAction::Prepare => {
            for directory in [ROOT, &path] {
                match fs::create_dir(directory) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err("runsc_task_domain_unavailable".into()),
                }
            }
            if fs::read_to_string(format!("{path}/cgroup.type"))
                .map_err(|_| "runsc_task_domain_unavailable")?
                .trim()
                != "domain"
            {
                return Err("runsc_task_domain_unavailable".into());
            }
            File::options()
                .write(true)
                .open(format!("{path}/cgroup.kill"))
                .map_err(|_| "runsc_task_domain_unavailable")?;
            if !fs::read_to_string(format!("{path}/cgroup.events"))
                .map_err(|_| "runsc_task_domain_unavailable")?
                .lines()
                .any(|l| l == "populated 0")
            {
                return Err("runsc_task_domain_not_empty".into());
            }
            Ok(())
        }
        RunnerAction::Remove => {
            fs::remove_dir(path).map_err(|_| "runsc_task_domain_retirement_pending".into())
        }
        RunnerAction::Start => start(request.process.expect("validated process"), &path),
    }
}

fn start(process: serde_json::Value, path: &str) -> Result<(), String> {
    use std::os::unix::process::ExitStatusExt;
    let identity = Identity::parse(&process)?;
    // The control process must never share the workload identity, even during exec startup.
    if unsafe { libc::geteuid() } == identity.uid {
        return Err("runsc_control_identity_rejected".into());
    }
    let args = process["args"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or("runsc_runner_process_invalid")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("runsc_runner_process_invalid")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .env_clear()
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for value in process["env"]
        .as_array()
        .ok_or("runsc_runner_process_invalid")?
    {
        let (key, value) = value
            .as_str()
            .and_then(|s| s.split_once('='))
            .ok_or("runsc_runner_process_invalid")?;
        command.env(key, value);
    }
    let cwd = std::ffi::CString::new(
        process["cwd"]
            .as_str()
            .ok_or("runsc_runner_process_invalid")?,
    )
    .map_err(|_| "runsc_runner_process_invalid")?;
    let procs = File::options()
        .write(true)
        .open(format!("{path}/cgroup.procs"))
        .map_err(|_| "runsc_task_domain_unavailable")?;
    use std::os::fd::AsRawFd;
    let procs_fd = procs.as_raw_fd();
    // Only the forked workload joins the task domain. Keeping its subreaper outside
    // that domain prevents concurrent tree termination from killing the sandbox init.
    unsafe {
        syscall_result(libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0))
            .map_err(|_| "runsc_subreaper_unavailable")?;
        command.pre_exec(move || {
            if libc::write(procs_fd, b"0".as_ptr().cast(), 1) != 1 {
                return Err(std::io::Error::last_os_error());
            }
            syscall_result(libc::unshare(libc::CLONE_NEWNS))?;
            syscall_result(libc::mount(
                std::ptr::null(),
                c"/sys/fs/cgroup".as_ptr(),
                std::ptr::null(),
                libc::MS_REMOUNT
                    | libc::MS_RDONLY
                    | libc::MS_NOSUID
                    | libc::MS_NODEV
                    | libc::MS_NOEXEC,
                std::ptr::null(),
            ))?;
            libc::close(5);
            // No descriptor granting access to trusted input or control reaches a command.
            syscall_result(libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC))?;
            identity.restore()?;
            syscall_result(libc::chdir(cwd.as_ptr()))
        });
    }
    let mut child = command.spawn().map_err(|_| "runsc_task_spawn_failed")?;
    let mut control = unsafe { File::from_raw_fd(5) };
    let _ = event(&mut control, &RunnerEvent::Ready { pid: child.id() });
    // The controller owns no copies of task stdin/stdout/stderr after fork.
    unsafe {
        libc::close(0);
        libc::close(1);
        libc::close(2);
    }
    let status = child.wait().map_err(|_| "runsc_task_wait_unknown")?;
    let _ = event(
        &mut control,
        &RunnerEvent::LeaderExited {
            wait_status: status.into_raw(),
        },
    );
    loop {
        let result = unsafe { libc::waitpid(-1, std::ptr::null_mut(), 0) };
        if result >= 0 {
            continue;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::ECHILD) => break,
            _ => return Err("runsc_task_wait_unknown".into()),
        }
    }
    Ok(())
}
