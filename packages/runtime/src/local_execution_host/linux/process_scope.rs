use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::{fs::DirBuilderExt, process::CommandExt};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use centaeris_core::execution::ExecutionError;

// This is process ownership, independent of the filesystem/network policy.
// The Runtime must run in a delegated systemd unit. Its unit owns cleanup if
// the Runtime itself exits, including abrupt exits while background work runs.
pub(in crate::local_execution_host) struct ProcessScope {
    cgroup: PathBuf,
    pub(in crate::local_execution_host) scratch: PathBuf,
}

impl ProcessScope {
    pub(in crate::local_execution_host) fn new() -> Result<Self, ExecutionError> {
        let membership = fs::read_to_string("/proc/self/cgroup").map_err(unavailable)?;
        let relative = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or_else(|| unavailable("local execution requires cgroup v2"))?;
        let parent = PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(unavailable)?
            .as_nanos();
        let name = format!("centaeris-exec-{}-{nonce}", std::process::id());
        let cgroup = parent.join(&name);
        fs::create_dir(&cgroup).map_err(|error| unavailable(format!("create execution cgroup: {error}; start the Runtime in a systemd unit with Delegate=yes")))?;
        if !cgroup.join("cgroup.kill").exists() {
            let _ = fs::remove_dir(&cgroup);
            return Err(unavailable("local execution requires cgroup.kill support"));
        }
        let scratch = std::env::temp_dir().join(name);
        if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&scratch) {
            let _ = fs::remove_dir(&cgroup);
            return Err(unavailable(error));
        }
        Ok(Self { cgroup, scratch })
    }

    pub(in crate::local_execution_host) fn attach(
        &self,
        command: &mut Command,
    ) -> Result<(), ExecutionError> {
        let membership: File = OpenOptions::new()
            .write(true)
            .open(self.cgroup.join("cgroup.procs"))
            .map_err(unavailable)?;
        // SAFETY: the prepared descriptor is owned by the closure and CLOEXEC.
        // Writing zero moves only this child, before it can execute user code.
        unsafe {
            command.pre_exec(move || {
                if libc::write(membership.as_raw_fd(), b"0".as_ptr().cast(), 1) != 1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }

    fn empty(&self) -> Result<bool, std::io::Error> {
        fs::read_to_string(self.cgroup.join("cgroup.events"))
            .map(|events| events.lines().any(|line| line == "populated 0"))
    }

    pub(in crate::local_execution_host) fn terminate(&self) -> Result<(), ExecutionError> {
        fs::write(self.cgroup.join("cgroup.kill"), "1").map_err(|error| {
            ExecutionError::CancellationIndeterminate {
                reason: format!("execution cgroup termination could not be confirmed: {error}"),
            }
        })?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.empty() {
                Ok(true) => return Ok(()),
                Ok(false) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => {
                    return Err(ExecutionError::CancellationIndeterminate {
                        reason: "execution cgroup did not confirm an empty process tree"
                            .to_string(),
                    })
                }
            }
        }
    }

    pub(in crate::local_execution_host) fn reap_in_background(self, mut child: Child) {
        std::thread::spawn(move || {
            let _ = child.wait();
            // The supervisor waits for every reparented descendant. Drop also
            // terminates a remaining tree if the supervisor exits abnormally.
            drop(self);
        });
    }
}

impl Drop for ProcessScope {
    fn drop(&mut self) {
        if self.terminate().is_ok() {
            let _ = fs::remove_dir(&self.cgroup);
            let _ = fs::remove_dir_all(&self.scratch);
        }
    }
}

fn unavailable(error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::PolicyUnavailable {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_terminates_descendants_that_create_a_new_session() {
        let scope = ProcessScope::new().unwrap();
        let marker = scope.scratch.join("escape");
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-c", "import os,time,pathlib,sys\npid=os.fork()\nif pid==0:\n os.setsid(); time.sleep(1); pathlib.Path(sys.argv[1]).write_text('escaped')\nelse: time.sleep(10)"]).arg(&marker);
        scope.attach(&mut command).unwrap();
        let mut child = command.spawn().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        scope.terminate().unwrap();
        child.wait().unwrap();
        assert!(scope.empty().unwrap());
        std::thread::sleep(Duration::from_millis(1100));
        assert!(!marker.exists());
    }
}
