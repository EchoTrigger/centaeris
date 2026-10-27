//! Exclusive child wait ownership independent of command execution duration.
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus},
    sync::{Arc, Mutex, OnceLock, TryLockError, Weak},
    thread,
    time::Duration,
};

#[derive(Default)]
struct WaitRegistry {
    owned: BTreeSet<u32>,
    abandoned: BTreeMap<u32, Abandoned>,
    handoffs: usize,
}

enum Abandoned {
    Child(Child),
    Adopted,
}

static WAIT_REGISTRY: Mutex<WaitRegistry> = Mutex::new(WaitRegistry {
    owned: BTreeSet::new(),
    abandoned: BTreeMap::new(),
    handoffs: 0,
});
static CLEANUP: OnceLock<io::Result<()>> = OnceLock::new();
type InstanceKey = (PathBuf, PathBuf, String);
static INSTANCE_GATES: Mutex<BTreeMap<InstanceKey, Weak<Mutex<()>>>> = Mutex::new(BTreeMap::new());

/// Every operation for the same attested container shares this lifecycle gate.
pub(crate) fn instance_gate(runtime: &Path, root: &Path, container: &str) -> Arc<Mutex<()>> {
    let mut gates = INSTANCE_GATES
        .lock()
        .expect("instance gate registry poisoned");
    gates.retain(|_, gate| gate.strong_count() != 0);
    let key = (runtime.to_owned(), root.to_owned(), container.to_owned());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}

fn ensure_cleanup() -> io::Result<()> {
    let result = CLEANUP.get_or_init(|| {
        thread::Builder::new()
            .name("host-child-cleanup".into())
            .spawn(|| loop {
                reap_abandoned();
                thread::sleep(Duration::from_millis(20));
            })
            .map(|_| ())
    });
    result
        .as_ref()
        .map_err(|error| io::Error::new(error.kind(), "host child cleanup unavailable"))
        .copied()
}

fn reap_abandoned() {
    let pids: Vec<_> = WAIT_REGISTRY
        .lock()
        .expect("child wait registry poisoned")
        .abandoned
        .keys()
        .copied()
        .collect();
    for pid in pids {
        let mut registry = WAIT_REGISTRY.lock().expect("child wait registry poisoned");
        let Some(owner) = registry.abandoned.get_mut(&pid) else {
            continue;
        };
        let reaped = match owner {
            Abandoned::Child(child) => matches!(child.try_wait(), Ok(Some(_))),
            Abandoned::Adopted => {
                // SAFETY: registration transferred this exact adopted child's sole wait ownership.
                unsafe {
                    libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) == pid as i32
                }
            }
        };
        if reaped {
            registry.abandoned.remove(&pid);
            registry.owned.remove(&pid);
        }
    }
}

/// A direct child remains registered until reaped or transferred to the cleanup worker.
pub(crate) struct OwnedChild {
    child: Option<Child>,
    registered: bool,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}
impl OwnedChild {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        ensure_cleanup()?;
        let mut registry = WAIT_REGISTRY.lock().expect("child wait registry poisoned");
        Self::spawn_registered(command, &mut registry)
    }
    pub(crate) fn spawn_checked(
        command: &mut Command,
        mut check: impl FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        ensure_cleanup().map_err(|_| "host_command_failed")?;
        loop {
            check()?;
            match WAIT_REGISTRY.try_lock() {
                Ok(mut registry) => {
                    check()?;
                    return Self::spawn_registered(command, &mut registry)
                        .map_err(|_| "host_command_failed".into());
                }
                Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Poisoned(_)) => return Err("host_command_failed".into()),
            }
        }
    }
    fn spawn_registered(command: &mut Command, registry: &mut WaitRegistry) -> io::Result<Self> {
        // Spawn and registration are one short critical section with the adopted reaper.
        let mut child = command.spawn()?;
        registry.owned.insert(child.id());
        Ok(Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child: Some(child),
            registered: true,
        })
    }
    #[cfg(test)]
    pub(crate) fn id(&self) -> u32 {
        self.child
            .as_ref()
            .expect("owned child remains present until drop")
            .id()
    }
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        // The wait and removal are atomic with spawn so PID reuse cannot erase a new registration.
        let mut registry = WAIT_REGISTRY.lock().expect("child wait registry poisoned");
        let child = self
            .child
            .as_mut()
            .expect("owned child remains present until drop");
        let status = child.try_wait()?;
        if status.is_some() && self.registered {
            registry.owned.remove(&child.id());
            self.registered = false;
        }
        Ok(status)
    }
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        self.stdin.take();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub(crate) fn kill(&mut self) -> io::Result<()> {
        self.child
            .as_mut()
            .expect("owned child remains present until drop")
            .kill()
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        // Do not block in Drop: the already-running worker retains the Child even if kill fails.
        let _ = child.kill();
        WAIT_REGISTRY
            .lock()
            .expect("child wait registry poisoned")
            .abandoned
            .insert(child.id(), Abandoned::Child(child));
    }
}

/// The detached OCI leader is adopted before its launch handoff ends.
pub(crate) struct OwnedPid {
    pid: u32,
    status: Option<ExitStatus>,
}
impl OwnedPid {
    #[cfg(test)]
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        let mut registry = WAIT_REGISTRY.lock().expect("child wait registry poisoned");
        let mut raw = 0;
        // SAFETY: this handle owns the wait registration for the exact adopted child PID.
        let result = unsafe { libc::waitpid(self.pid as i32, &mut raw, libc::WNOHANG) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if result > 0 {
            self.status = Some(ExitStatus::from_raw(raw));
            registry.owned.remove(&self.pid);
        }
        Ok(self.status)
    }
}
impl Drop for OwnedPid {
    fn drop(&mut self) {
        if self.status.is_none() {
            WAIT_REGISTRY
                .lock()
                .expect("child wait registry poisoned")
                .abandoned
                .insert(self.pid, Abandoned::Adopted);
        }
    }
}

/// Unknown adopted children cannot be reaped while runc is handing over a leader PID.
pub(crate) struct AdoptedHandoff {
    _private: (),
}
impl AdoptedHandoff {
    pub(crate) fn begin() -> io::Result<Self> {
        ensure_cleanup()?;
        WAIT_REGISTRY
            .lock()
            .expect("child wait registry poisoned")
            .handoffs += 1;
        Ok(Self { _private: () })
    }
    /// Transfer an already-verified child PID while this handoff still prevents adopted reaping.
    pub(crate) fn adopt(&self, pid: u32) -> io::Result<OwnedPid> {
        let mut registry = WAIT_REGISTRY.lock().expect("child wait registry poisoned");
        if !registry.owned.insert(pid) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "child wait ownership already registered",
            ));
        }
        Ok(OwnedPid { pid, status: None })
    }
}
impl Drop for AdoptedHandoff {
    fn drop(&mut self) {
        WAIT_REGISTRY
            .lock()
            .expect("child wait registry poisoned")
            .handoffs -= 1;
    }
}

/// Reap only unregistered adopted children, checking ownership at each wait syscall.
pub(crate) fn reap_adopted() {
    let Ok(entries) = fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u32>().ok());
        if parent != Some(std::process::id()) {
            continue;
        }
        reap_adopted_pid(pid);
    }
}
fn reap_adopted_pid(pid: u32) {
    let registry = WAIT_REGISTRY.lock().expect("child wait registry poisoned");
    if registry.handoffs != 0 || registry.owned.contains(&pid) {
        return;
    }
    // SAFETY: the shared spawn/registration lock excludes every Child handle and pending adopted handoff.
    unsafe {
        libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG);
    }
}

#[cfg(test)]
mod tests;
