use super::*;
use crate::host_supervisor::linux::commands;
use std::{
    ffi::CString,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::PathBuf,
    process::{ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
#[cfg(test)]
use std::{
    io::{BufRead, BufReader},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    process::Child,
};

fn verify_domain(path: &Path) -> Result<(), String> {
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| "task_cgroup_path_invalid")?;
    let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: statfs receives a NUL-terminated path and a correctly sized output buffer.
    if unsafe { libc::statfs(name.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err("task_tree_supervision_unavailable".into());
    }
    // SAFETY: successful statfs initialized stats.
    if unsafe { stats.assume_init() }.f_type as u64 != 0x6367_7270
        || fs::read_to_string(path.join("cgroup.type"))
            .map_err(|_| "task_tree_supervision_unavailable")?
            .trim()
            != "domain"
        || OpenOptions::new()
            .write(true)
            .open(path.join("cgroup.kill"))
            .is_err()
    {
        return Err("task_tree_supervision_unavailable".into());
    }
    Ok(())
}

/// Called before exec using only async-signal-safe syscalls and stack storage.
#[cfg(test)]
fn assign_self(fd: i32) -> std::io::Result<()> {
    let mut bytes = [0u8; 64];
    // SAFETY: fd belongs to the pre-opened task cgroup.procs, and bytes is a valid buffer.
    let count = unsafe {
        if libc::write(fd, b"0".as_ptr().cast(), 1) != 1 || libc::lseek(fd, 0, libc::SEEK_SET) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len())
    };
    if count <= 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EIO));
    }
    // SAFETY: getpid takes no arguments and has no side effects.
    let own_pid = unsafe { libc::getpid() } as u32;
    let mut pid = 0u32;
    for byte in &bytes[..count as usize] {
        if byte.is_ascii_digit() {
            pid = pid.saturating_mul(10).saturating_add((byte - b'0') as u32);
        } else if *byte == b'\n' {
            if pid == own_pid {
                return Ok(());
            }
            pid = 0;
        } else {
            return Err(std::io::Error::from_raw_os_error(libc::EIO));
        }
    }
    Err(std::io::Error::from_raw_os_error(libc::EIO))
}

const OUTPUT_BUDGET_BYTES: u64 = (ENVIRONMENT_TASK_MAX_RECORDS
    * (ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK)
    * (HEADER + CHUNK)) as u64;
#[cfg(test)]
const MAX_RESPONSE: usize = 512 * 1024;
mod domains;
pub(crate) mod launcher;
mod runsc;
mod trusted;
use launcher::{OciTarget, Spawned, TaskChild};
pub(crate) use trusted::HelperBinary;
use trusted::{HelperCapture, ReservedInput, TrustedLaunch};
struct Live {
    child: TaskChild,
    input: Option<ChildStdin>,
    cgroup: PathBuf,
    supervisor: thread::JoinHandle<()>,
    input_worker: Option<thread::JoinHandle<bool>>,
}
struct ControlLease {
    _file: File,
}
impl ControlLease {
    fn acquire(root: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(root.join("service.lock"))
            .map_err(|_| "task_control_lock_unavailable")?;
        // SAFETY: file owns a valid fd; nonblocking flock never waits for another service.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK) {
                    "task_service_already_running"
                } else {
                    "task_control_lock_unavailable"
                }
                .into(),
            );
        }
        Ok(Self { _file: file })
    }
}
#[cfg(test)]
struct ServiceControl {
    lease: ControlLease,
    listener: UnixListener,
    journal: Journal,
}
#[cfg(test)]
fn prepare_control(root: &Path, socket: &Path, instance: &str) -> Result<ServiceControl, String> {
    let lease = ControlLease::acquire(root)?;
    let listener = UnixListener::bind(socket).map_err(|_| "task_socket_bind_failed")?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(|_| "task_socket_protection_failed")?;
    let journal = Journal::open(root, instance)?;
    Ok(ServiceControl {
        lease,
        listener,
        journal,
    })
}
struct Manager {
    _control_lease: ControlLease,
    storage: TaskStorage,
    cgroups: PathBuf,
    launcher: Option<Arc<OciTarget>>,
    helper_root: Option<PathBuf>,
    journal: Journal,
    live: BTreeMap<String, Live>,
    logs: BTreeMap<String, Arc<Mutex<OutputLog>>>,
    container_absent: bool,
    stop_reaper: bool,
    drain_workers: Arc<AtomicUsize>,
    #[cfg(test)]
    fail_running_commit: bool,
    #[cfg(test)]
    fail_mutation_commit: bool,
    #[cfg(test)]
    fail_worker_reservation: Option<usize>,
    #[cfg(test)]
    fail_worker_delivery: bool,
    #[cfg(test)]
    defer_failed_child_reap: bool,
}

fn cgroup_kill(path: &Path) -> Result<bool, String> {
    verify_domain(path)?;
    fs::write(path.join("cgroup.kill"), b"1").map_err(|_| "task_tree_kill_failed")?;
    let events = fs::read_to_string(path.join("cgroup.events"))
        .map_err(|_| "task_tree_observation_failed")?;
    Ok(events.lines().any(|line| line == "populated 0"))
}
fn valid_operation(s: &str) -> Result<(), String> {
    if s.is_empty() || s.len() > 160 || s.chars().any(char::is_control) {
        Err("task_invalid_operation_id".into())
    } else {
        Ok(())
    }
}
impl Manager {
    #[cfg(test)]
    fn open(storage: TaskStorage, cgroups: PathBuf, instance: &str) -> Result<Self, String> {
        let lease = ControlLease::acquire(&storage.control_root)?;
        let journal = Journal::open(&storage.control_root, instance)?;
        Self::from_journal(storage, cgroups, journal, lease)
    }
    fn from_journal(
        storage: TaskStorage,
        cgroups: PathBuf,
        journal: Journal,
        lease: ControlLease,
    ) -> Result<Self, String> {
        let mut logs = BTreeMap::new();
        for id in journal.tasks.keys() {
            if storage.output_root.join(format!("{id}.output")).exists() {
                if let Ok(log) = storage.open_output(id, ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK) {
                    logs.insert(id.clone(), Arc::new(Mutex::new(log)));
                }
            }
        }
        Ok(Self {
            _control_lease: lease,
            storage,
            cgroups,
            launcher: None,
            helper_root: None,
            journal,
            live: BTreeMap::new(),
            logs,
            container_absent: false,
            stop_reaper: false,
            drain_workers: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            fail_running_commit: false,
            #[cfg(test)]
            fail_mutation_commit: false,
            #[cfg(test)]
            fail_worker_reservation: None,
            #[cfg(test)]
            fail_worker_delivery: false,
            #[cfg(test)]
            defer_failed_child_reap: false,
        })
    }
    fn reconcile(&mut self, id: &str) -> Result<(), String> {
        let failed_supervisor = self
            .live
            .get(id)
            .is_some_and(|live| live.supervisor.is_finished());
        let r = self.journal.tasks.get_mut(id).ok_or("task_not_found")?;
        // Unexpected supervisor exit is reconciled by the reaper, which must
        // also confirm ownership of the direct child has ended.
        if !r.snapshot.cleanup_complete && !failed_supervisor {
            let events = domains::events(&self.launcher, &self.cgroups.join(id))?;
            if events.lines().any(|line| line == "populated 0") {
                r.snapshot.cleanup_complete = true;
                self.persist()?;
            }
        }
        Ok(())
    }
    fn mutation(&self, id: &str) -> Result<EnvironmentTaskResult, String> {
        Ok(EnvironmentTaskResult::Mutated {
            task: self
                .journal
                .tasks
                .get(id)
                .ok_or("task_not_found")?
                .snapshot
                .clone(),
        })
    }
    fn persist(&mut self) -> Result<(), String> {
        self.journal.persist(&self.storage.control_root)
    }
    fn query(
        &mut self,
        id: &str,
        cursor: u64,
        max: usize,
    ) -> Result<EnvironmentTaskResult, String> {
        if !self.journal.tasks.contains_key(id) {
            let task = store::task(&self.storage.control_root, id)?
                .ok_or("task_not_found")?
                .snapshot;
            let path = self.storage.output_root.join(format!("{id}.output"));
            let mut output = OutputLog::open_retained(&path).ok();
            let incomplete = !task.output_complete;
            let mut result = task_query_result(task, output.as_mut(), cursor, max)?;
            if incomplete {
                if let EnvironmentTaskResult::Task { gap, .. } = &mut result {
                    *gap = true;
                }
            }
            return Ok(result);
        }
        self.reconcile(id)?;
        let task = self
            .journal
            .tasks
            .get(id)
            .ok_or("task_not_found")?
            .snapshot
            .clone();
        let lost = !task.output_complete
            && !self.live.contains_key(id)
            && self
                .logs
                .get(id)
                .is_none_or(|log| Arc::strong_count(log) == 1);
        let mut result = match self.logs.get(id) {
            Some(log) => match log.lock() {
                Ok(mut log) => task_query_result(task, Some(&mut log), cursor, max),
                Err(_) => task_query_result(task, None, cursor, max),
            },
            None => task_query_result(task, None, cursor, max),
        }?;
        if lost {
            if let EnvironmentTaskResult::Task { gap, .. } = &mut result {
                *gap = true;
            }
        }
        Ok(result)
    }
    fn receipt(&mut self, key: &str, d: &str) -> Result<Option<EnvironmentTaskResult>, String> {
        if let Some(result) = self.journal.reserve_receipt(key, d)? {
            return Ok(Some(result));
        }
        self.persist()?;
        Ok(None)
    }
    fn collect_closed_history(&mut self) -> Result<(), String> {
        let current = self
            .journal
            .fence
            .as_ref()
            .map(|f| f.controller_owner.clone())
            .ok_or("task_controller_fenced")?;
        let mut after = String::new();
        while let Some((id, _)) = store::next_closed(&self.storage.control_root, &after, &current)?
        {
            after = id.clone();
            if self.live.contains_key(&id) {
                continue;
            }
            self.logs.remove(&id);
            if let Some(root) = &self.helper_root {
                for suffix in ["stdout", "stderr"] {
                    let path = root.join(format!("{id}.{suffix}"));
                    if path.exists() {
                        fs::remove_file(path).map_err(|_| "task_history_retirement_pending")?;
                    }
                }
            }
            match fs::remove_file(self.storage.output_root.join(format!("{id}.output"))) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("task_history_retirement_pending".into()),
            }
            store::forget_closed(&self.storage.control_root, &id)?;
            self.journal.tasks.remove(&id);
            self.journal.receipts.retain(|key, _| {
                !key.starts_with(&format!("input:{id}:")) && key != &format!("terminate:{id}")
            });
        }
        Ok(())
    }
    fn retire_history(&mut self, epoch: u64) -> Result<(), String> {
        let retired: Vec<_> = self
            .journal
            .tasks
            .iter()
            .filter(|(id, r)| {
                self.journal.fence.as_ref().is_some_and(|f| {
                    r.can_retire(epoch, &f.controller_owner, self.live.contains_key(*id))
                }) && self
                    .logs
                    .get(*id)
                    .is_none_or(|log| Arc::strong_count(log) == 1)
            })
            .map(|(id, _)| id.clone())
            .collect();
        if retired.is_empty() {
            return Ok(());
        }
        for id in retired {
            let domain = self.cgroups.join(&id);
            if domain.exists() {
                // Confirm the exact owned tree again before dropping its observable domain.
                let events = domains::events(&self.launcher, &domain)?;
                if !events.lines().any(|line| line == "populated 0") {
                    continue;
                }
                domains::remove(&self.launcher, &domain)?;
            }
            // Flush terminal identity before retiring the hot object. Its compact
            // output remains the recoverable copy until authoritative Run closure.
            self.persist()?;
            if let Some(log) = self.logs.remove(&id) {
                log.lock()
                    .map_err(|_| "task_output_unavailable")?
                    .compact()?;
            }
            self.journal.retire_task(&id, epoch, false);
        }
        self.persist()
    }
    fn finish(
        &mut self,
        key: String,
        result: EnvironmentTaskResult,
    ) -> Result<EnvironmentTaskResult, String> {
        #[cfg(test)]
        if self.fail_mutation_commit {
            fs::create_dir(self.storage.control_root.join("history.sqlite-journal"))
                .map_err(|_| "fault_injection_failed")?;
        }
        self.journal
            .receipts
            .get_mut(&key)
            .ok_or("task_receipt_missing")?
            .result = Some(result.clone());
        self.persist()?;
        Ok(result)
    }
    fn handle(
        manager: &Arc<Mutex<Self>>,
        request: EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String> {
        Self::handle_with_helper(manager, request, None)
    }
    fn handle_with_helper(
        manager: &Arc<Mutex<Self>>,
        request: EnvironmentTaskRequest,
        trusted: Option<&TrustedLaunch>,
    ) -> Result<EnvironmentTaskResult, String> {
        request.validate_authority()?;
        let start_digest = if matches!(&request, EnvironmentTaskRequest::Start { .. }) {
            Some(task_start_digest(
                &request,
                trusted.map(TrustedLaunch::identity).transpose()?,
            )?)
        } else {
            None
        };
        let mut m = manager.lock().map_err(|_| "task_service_failed")?;
        match request {
            EnvironmentTaskRequest::Takeover {
                environment_instance_id,
                controller_owner,
                controller_epoch,
                controller_purpose,
            } => {
                let fence = EnvironmentTaskFence {
                    environment_instance_id,
                    controller_owner,
                    controller_epoch,
                    controller_purpose,
                };
                m.journal.takeover(fence.clone())?;
                let previous_run_tasks: Vec<_> = m
                    .journal
                    .tasks
                    .iter()
                    .filter(|(_, r)| {
                        !r.snapshot.cleanup_complete
                            && r.snapshot.scope != EnvironmentTaskScope::Environment
                            && r.creator_owner != fence.controller_owner
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                m.journal.takeover_pending = !previous_run_tasks.is_empty();
                m.persist()?;
                let mut clean = true;
                for id in previous_run_tasks {
                    let empty = domains::kill(&m.launcher, &m.cgroups.join(&id)).unwrap_or(false);
                    m.journal
                        .tasks
                        .get_mut(&id)
                        .expect("record")
                        .snapshot
                        .cleanup_complete = empty;
                    clean &= empty;
                }
                if !clean {
                    m.persist()?;
                    return Err("task_takeover_pending".into());
                }
                m.journal.takeover_pending = false;
                m.persist()?;
                m.retire_history(fence.controller_epoch)?;
                m.collect_closed_history()?;
                Ok(EnvironmentTaskResult::TakenOver { fence })
            }
            EnvironmentTaskRequest::Query {
                fence,
                task_id,
                cursor,
                max_bytes,
            } => {
                m.journal.check(&fence)?;
                m.query(&task_id, cursor, max_bytes as usize)
            }
            EnvironmentTaskRequest::List { fence } => {
                m.journal.check(&fence)?;
                let ids: Vec<_> = m.journal.tasks.keys().cloned().collect();
                for id in ids {
                    m.reconcile(&id)?;
                }
                Ok(EnvironmentTaskResult::Listed {
                    tasks: m
                        .journal
                        .tasks
                        .values()
                        .map(|r| r.snapshot.clone())
                        .collect(),
                })
            }
            EnvironmentTaskRequest::Start {
                fence,
                operation_id,
                scope,
                program,
                args,
                env,
                cwd,
                timeout_ms,
            } => {
                m.journal.check(&fence)?;
                valid_operation(&operation_id)?;
                if program.is_empty()
                    || program.len() > 4096
                    || args.len() > 1024
                    || args.iter().map(String::len).sum::<usize>() > 65536
                    || timeout_ms > 86_400_000
                {
                    return Err("task_invalid_start".into());
                }
                fence.authorize_start(&scope)?;
                let d = start_digest.expect("validated Start digest");
                let id = task_identity(&fence, &operation_id)?;
                if m.journal.existing(&id, &d)?.is_some() {
                    return m.query(&id, 0, ENVIRONMENT_TASK_READ_BYTES);
                }
                if !crate::host_supervisor::absolute_path(&cwd) {
                    return Err("task_invalid_cwd".into());
                }
                m.retire_history(fence.controller_epoch)?;
                m.journal.check_admission()?;
                let snapshot = EnvironmentTaskSnapshot {
                    task_id: id.clone(),
                    operation_id,
                    scope,
                    state: EnvironmentTaskState::Accepted,
                    exit_code: None,
                    cleanup_complete: false,
                    output_complete: false,
                };
                let record = Record {
                    digest: d,
                    creator_owner: fence.controller_owner.clone(),
                    identity_version: TaskIdentityVersion::OwnerV3,
                    creator_epoch: fence.controller_epoch,
                    snapshot,
                };
                let mut candidate = m.journal.clone();
                candidate.tasks.insert(id.clone(), record.clone());
                candidate.preflight(4096)?;
                // A rejected reservation has no record or user process. The same operation
                // may be tried again; an accepted/unknown record never starts twice.
                #[cfg(test)]
                let failure = m.fail_worker_reservation;
                #[cfg(not(test))]
                let failure = None;
                let mut workers = ReservedWorkers::new(failure)?;
                let mut feeder = trusted
                    .map(|launch| {
                        launch
                            .input
                            .try_clone()
                            .map_err(|_| "host_helper_input_incomplete".to_owned())
                            .and_then(|input| {
                                ReservedInput::new(
                                    input,
                                    Some(Arc::new(HelperInputLease {
                                        _input_guard: launch.input_guard.clone(),
                                        _worker: DrainLease::new(&m.drain_workers),
                                    })),
                                )
                            })
                    })
                    .transpose()?;
                let cgroup = m.cgroups.join(&id);
                // Task groups provide exact tree ownership. Runtime verifies the enclosing
                // Docker instance's aggregate memory, CPU and PID limits before activation.
                let procs = domains::create(&m.launcher, &m.cgroups, &cgroup)?;
                let log = match m
                    .storage
                    .open_output(&id, ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK)
                {
                    Ok(log) => Arc::new(Mutex::new(log)),
                    Err(error) => {
                        drop(procs);
                        let _ = domains::remove(&m.launcher, &cgroup);
                        return Err(error);
                    }
                };
                m.logs.insert(id.clone(), log.clone());
                if let Some(launch) = trusted {
                    launch
                        .capture
                        .lock()
                        .map_err(|_| "task_service_failed")?
                        .retained = true;
                }
                m.journal.tasks.insert(id.clone(), record);
                if let Err(error) = m.persist() {
                    if error == "task_index_capacity" {
                        // SQLITE_FULL rolls back the pre-spawn intent. Keep other
                        // tasks supervised and discard only this unaccepted task.
                        m.journal.tasks.remove(&id);
                        m.logs.remove(&id);
                        if let Some(launch) = trusted {
                            launch
                                .capture
                                .lock()
                                .map_err(|_| "task_service_failed")?
                                .retained = false;
                        }
                        drop(log);
                        drop(procs);
                        let _ = fs::remove_file(m.storage.output_root.join(format!("{id}.output")));
                        let _ = domains::remove(&m.launcher, &cgroup);
                        return Err("task_journal_capacity".into());
                    }
                    return Err(error);
                }
                // Establish supervision before spawning. The supervisor waits on this
                // manager lock until the child (or a failed-spawn record) is installed.
                let supervisor = match workers.arm(manager.clone(), id.clone(), timeout_ms) {
                    Ok(supervisor) => supervisor,
                    Err(error) => {
                        let s = &mut m
                            .journal
                            .tasks
                            .get_mut(&id)
                            .expect("accepted record")
                            .snapshot;
                        s.state = EnvironmentTaskState::Unknown;
                        s.cleanup_complete = true;
                        s.output_complete = true;
                        m.persist()?;
                        return Err(error);
                    }
                };
                #[cfg(test)]
                if m.fail_worker_delivery {
                    let (send, receive) = mpsc::channel();
                    drop(receive);
                    workers.err = Some(send);
                }
                let spawned = if let Some(target) = &m.launcher {
                    target.spawn(
                        &id,
                        launcher::TaskProcess {
                            program: &program,
                            args: &args,
                            cwd: &cwd,
                            env: &env,
                        },
                        &cgroup,
                        trusted,
                    )
                } else {
                    #[cfg(test)]
                    {
                        spawn_local(
                            &program,
                            &args,
                            &cwd,
                            &env,
                            procs.as_ref().expect("local task cgroup").as_raw_fd(),
                        )
                    }
                    #[cfg(not(test))]
                    {
                        Err("task_native_launcher_required".into())
                    }
                };
                match spawned {
                    Ok(Spawned {
                        child,
                        mut input,
                        stdout,
                        stderr,
                    }) => {
                        let mut input_failed = false;
                        let input_worker = match feeder.take() {
                            Some(feeder) => match input
                                .take()
                                .ok_or_else(|| "task_input_configuration_failed".to_owned())
                                .and_then(|input| feeder.deliver(input))
                            {
                                Ok(worker) => Some(worker),
                                Err(_) => {
                                    input_failed = true;
                                    None
                                }
                            },
                            None => None,
                        };
                        if let Some(stdin) = &input {
                            // SAFETY: stdin is a valid pipe fd, made nonblocking to bound controller requests.
                            unsafe {
                                let flags = libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL);
                                if flags < 0
                                    || libc::fcntl(
                                        stdin.as_raw_fd(),
                                        libc::F_SETFL,
                                        flags | libc::O_NONBLOCK,
                                    ) < 0
                                {
                                    input_failed = true;
                                }
                            }
                        }
                        m.live.insert(
                            id.clone(),
                            Live {
                                child,
                                input,
                                cgroup: cgroup.clone(),
                                supervisor,
                                input_worker,
                            },
                        );
                        m.journal
                            .tasks
                            .get_mut(&id)
                            .expect("accepted record")
                            .snapshot
                            .state = EnvironmentTaskState::Running;
                        if workers
                            .deliver(
                                DrainWork {
                                    reader: Box::new(stdout),
                                    log: log.clone(),
                                    stream: EnvironmentTaskStream::Stdout,
                                    capture: trusted.map(|launch| launch.capture.clone()),
                                    _lease: DrainLease::new(&m.drain_workers),
                                },
                                DrainWork {
                                    reader: Box::new(stderr),
                                    log,
                                    stream: EnvironmentTaskStream::Stderr,
                                    capture: trusted.map(|launch| launch.capture.clone()),
                                    _lease: DrainLease::new(&m.drain_workers),
                                },
                            )
                            .is_err()
                        {
                            // The pre-existing supervisor still owns the child and retries
                            // exact-tree cleanup until the direct child has been reaped.
                            m.journal
                                .tasks
                                .get_mut(&id)
                                .expect("accepted record")
                                .snapshot
                                .state = EnvironmentTaskState::Unknown;
                            let _ = domains::kill(&m.launcher, &cgroup);
                            m.persist()?;
                            return Err("task_worker_delivery_failed".into());
                        }
                        #[cfg(test)]
                        if m.fail_running_commit {
                            fs::create_dir(m.storage.control_root.join("history.sqlite-journal"))
                                .map_err(|_| "fault_injection_failed")?;
                        }
                        if input_failed {
                            let _ = domains::kill(&m.launcher, &cgroup);
                            m.journal.poisoned = true;
                            return Err("task_input_configuration_failed".into());
                        }
                        m.persist()?;
                    }
                    Err(_) => {
                        let clean = domains::kill(&m.launcher, &cgroup).unwrap_or(false);
                        let s = &mut m
                            .journal
                            .tasks
                            .get_mut(&id)
                            .expect("accepted record")
                            .snapshot;
                        s.state = EnvironmentTaskState::Unknown;
                        s.cleanup_complete = clean;
                        s.output_complete = clean;
                        m.persist()?;
                    }
                }
                m.query(&id, 0, ENVIRONMENT_TASK_READ_BYTES)
            }
            EnvironmentTaskRequest::Input {
                fence,
                task_id,
                operation_id,
                data,
                close,
            } => {
                m.journal.check(&fence)?;
                valid_operation(&operation_id)?;
                if data.len() > CHUNK {
                    return Err("task_input_too_large".into());
                }
                m.journal.authorize_task_input(&fence, &task_id)?;
                let key = input_identity(&fence, &task_id, &operation_id)?;
                let d = environment_task_input_digest(&data, close)?;
                if let Some(r) = m.receipt(&key, &d)? {
                    return Ok(r);
                }
                let live = m.live.get_mut(&task_id).ok_or("task_not_running")?;
                live.input
                    .as_mut()
                    .ok_or("task_input_closed")?
                    .write_all(&data)
                    .map_err(|_| "task_input_unknown")?;
                if close {
                    live.input.take();
                }
                let result = m.mutation(&task_id)?;
                m.finish(key, result)
            }
            EnvironmentTaskRequest::QueryInputReceipt {
                fence,
                task_id,
                operation_id,
                request_digest,
            } => {
                m.journal.check(&fence)?;
                m.journal.authorize_task_input(&fence, &task_id)?;
                let key = input_identity(&fence, &task_id, &operation_id)?;
                store::input_receipt(&m.storage.control_root, &key, &request_digest, &task_id)
            }
            EnvironmentTaskRequest::Terminate {
                fence,
                task_id,
                operation_id,
            } => {
                m.journal.check(&fence)?;
                valid_operation(&operation_id)?;
                if !m.journal.tasks.contains_key(&task_id) {
                    let record =
                        store::task(&m.storage.control_root, &task_id)?.ok_or("task_not_found")?;
                    fence.authorize_termination(&record.snapshot.scope)?;
                    if record.snapshot.cleanup_complete {
                        return Ok(EnvironmentTaskResult::Mutated {
                            task: record.snapshot,
                        });
                    }
                    return Err("task_operation_unknown".into());
                }
                fence.authorize_termination(&m.journal.tasks[&task_id].snapshot.scope)?;
                // Termination is intrinsically idempotent for this never-reused task identity.
                // One control slot per bounded task cannot be consumed by ordinary input.
                let key = format!("terminate:{task_id}");
                let d = digest(&task_id)?;
                if let Some(r) = m.receipt(&key, &d)? {
                    return Ok(r);
                }
                if !m.journal.tasks[&task_id].snapshot.cleanup_complete {
                    domains::kill(&m.launcher, &m.cgroups.join(&task_id))?;
                }
                let result = m.mutation(&task_id)?;
                m.finish(key, result)
            }
            EnvironmentTaskRequest::EndRun {
                fence,
                run_id: owner_id,
                operation_id,
            }
            | EnvironmentTaskRequest::EndOperation {
                fence,
                owner_operation_id: owner_id,
                operation_id,
            } => {
                m.journal.check(&fence)?;
                valid_operation(&operation_id)?;
                valid_operation(&owner_id)?;
                let scope = owner_scope(&fence.controller_owner);
                let is_run = matches!(
                    fence.controller_owner,
                    EnvironmentTaskOwner::AgentRun { .. }
                );
                let key = format!(
                    "{}:{owner_id}",
                    if is_run { "endRun" } else { "endOperation" }
                );
                let d = digest(&owner_id)?;
                if let Some(r) = m.receipt(&key, &d)? {
                    return Ok(r);
                }
                m.journal.closed_owner = Some(fence.controller_owner.clone());
                m.persist()?;
                let task_ids: Vec<_> = m
                    .journal
                    .tasks
                    .iter()
                    .filter(|(_, r)| r.snapshot.scope == scope)
                    .map(|(id, _)| id.clone())
                    .collect();
                let mut cleanup_complete = true;
                for id in &task_ids {
                    let clean = m.journal.tasks[id].snapshot.cleanup_complete
                        || domains::kill(&m.launcher, &m.cgroups.join(id))?;
                    m.journal
                        .tasks
                        .get_mut(id)
                        .expect("task record")
                        .snapshot
                        .cleanup_complete = clean;
                    cleanup_complete &= clean;
                }
                m.finish(
                    key,
                    if is_run {
                        EnvironmentTaskResult::RunEnded {
                            task_ids,
                            cleanup_complete,
                        }
                    } else {
                        EnvironmentTaskResult::OperationEnded {
                            task_ids,
                            cleanup_complete,
                        }
                    },
                )
            }
        }
    }
}
fn task_start_digest(
    request: &EnvironmentTaskRequest,
    trusted_identity: Option<String>,
) -> Result<String, String> {
    let EnvironmentTaskRequest::Start {
        fence,
        operation_id,
        scope,
        program,
        args,
        env,
        cwd,
        timeout_ms,
    } = request
    else {
        return Err("task_invalid_start".into());
    };
    let (_, opaque) = operation_id
        .split_once(':')
        .ok_or("task_invalid_operation_id")?;
    digest(&(
        &fence.environment_instance_id,
        &fence.controller_owner,
        &fence.controller_purpose,
        opaque,
        scope,
        program,
        args,
        env,
        cwd,
        timeout_ms,
        trusted_identity,
    ))
}
struct DrainWork {
    reader: Box<dyn Read + Send>,
    log: Arc<Mutex<OutputLog>>,
    stream: EnvironmentTaskStream,
    capture: Option<Arc<Mutex<HelperCapture>>>,
    _lease: DrainLease,
}
struct DrainLease(Arc<AtomicUsize>);
struct HelperInputLease {
    _input_guard: Option<Arc<dyn Send + Sync>>,
    _worker: DrainLease,
}
impl DrainLease {
    fn new(workers: &Arc<AtomicUsize>) -> Self {
        workers.fetch_add(1, Ordering::AcqRel);
        Self(workers.clone())
    }
}
impl Drop for DrainLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Supervision {
    manager: Arc<Mutex<Manager>>,
    id: String,
    timeout_ms: u64,
    out: thread::JoinHandle<bool>,
    err: thread::JoinHandle<bool>,
}
#[derive(Default)]
struct ReservedWorkers {
    out: Option<mpsc::Sender<DrainWork>>,
    err: Option<mpsc::Sender<DrainWork>>,
    control: Option<mpsc::Sender<Supervision>>,
    out_thread: Option<thread::JoinHandle<bool>>,
    err_thread: Option<thread::JoinHandle<bool>>,
    control_thread: Option<thread::JoinHandle<()>>,
}
fn reserve_worker<T: Send + 'static, R: Default + Send + 'static>(
    name: &str,
    run: impl FnOnce(T) -> R + Send + 'static,
    fail: bool,
) -> Result<(mpsc::Sender<T>, thread::JoinHandle<R>), String> {
    // Test only substitutes the OS resource error; reservation and rollback are shared.
    if fail {
        return Err("task_thread_capacity".into());
    }
    let (send, receive) = mpsc::channel();
    let worker = thread::Builder::new()
        .name(name.into())
        .spawn(move || receive.recv().map(run).unwrap_or_default())
        .map_err(|_| "task_thread_capacity")?;
    Ok((send, worker))
}
impl ReservedWorkers {
    fn new(fail_at: Option<usize>) -> Result<Self, String> {
        let mut workers = Self::default();
        let (send, worker) = reserve_worker("task-stdout", drain, fail_at == Some(1))?;
        workers.out = Some(send);
        workers.out_thread = Some(worker);
        let (send, worker) = reserve_worker("task-stderr", drain, fail_at == Some(2))?;
        workers.err = Some(send);
        workers.err_thread = Some(worker);
        let (send, worker) = reserve_worker(
            "task-supervisor",
            move |work: Supervision| {
                #[cfg(test)]
                if fail_at == Some(4) {
                    panic!("injected supervisor failure after accepting ownership");
                }
                supervise(work.manager, work.id, work.timeout_ms, work.out, work.err);
            },
            fail_at == Some(3),
        )?;
        workers.control = Some(send);
        workers.control_thread = Some(worker);
        Ok(workers)
    }
    fn arm(
        &mut self,
        manager: Arc<Mutex<Manager>>,
        id: String,
        timeout_ms: u64,
    ) -> Result<thread::JoinHandle<()>, String> {
        let work = Supervision {
            manager,
            id,
            timeout_ms,
            out: self.out_thread.take().expect("reserved output"),
            err: self.err_thread.take().expect("reserved error"),
        };
        if let Err(error) = self.control.take().expect("reserved supervisor").send(work) {
            self.out_thread = Some(error.0.out);
            self.err_thread = Some(error.0.err);
            return Err("task_worker_delivery_failed".into());
        }
        Ok(self.control_thread.take().expect("reserved supervisor"))
    }
    fn deliver(&mut self, out: DrainWork, err: DrainWork) -> Result<(), ()> {
        self.out
            .take()
            .expect("reserved output")
            .send(out)
            .map_err(|_| ())?;
        self.err
            .take()
            .expect("reserved error")
            .send(err)
            .map_err(|_| ())
    }
}
impl Drop for ReservedWorkers {
    fn drop(&mut self) {
        self.out.take();
        self.err.take();
        self.control.take();
        // Only unarmed workers remain here. Armed workers belong to supervision.
        if let Some(worker) = self.out_thread.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.err_thread.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.control_thread.take() {
            let _ = worker.join();
        }
    }
}
fn drain(work: DrainWork) -> bool {
    let DrainWork {
        mut reader,
        log,
        stream,
        capture,
        _lease,
    } = work;
    let mut buf = [0; CHUNK];
    let mut okay = true;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(capture) = &capture {
                    if capture.lock().map_or(true, |mut capture| {
                        capture.append(&stream, &buf[..n]).is_err()
                    }) {
                        okay = false;
                    }
                }
                if log.lock().map_or(true, |mut log| {
                    log.append(stream.clone(), &buf[..n]).is_err()
                }) {
                    okay = false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                okay = false;
                break;
            }
        }
    }
    if let Some(capture) = capture {
        if capture
            .lock()
            .map_or(true, |mut capture| capture.finish(&stream).is_err())
        {
            okay = false;
        }
    }
    okay
}
fn supervise(
    manager: Arc<Mutex<Manager>>,
    id: String,
    timeout_ms: u64,
    out: thread::JoinHandle<bool>,
    err: thread::JoinHandle<bool>,
) {
    let began = Instant::now();
    let mut workers = [Some(out), Some(err)];
    let mut eof = [None, None];
    loop {
        for (worker, result) in workers.iter_mut().zip(eof.iter_mut()) {
            if worker.as_ref().is_some_and(|w| w.is_finished()) {
                *result = Some(
                    worker
                        .take()
                        .expect("finished drain")
                        .join()
                        .unwrap_or(false),
                );
            }
        }
        let finished = {
            let mut m = manager.lock().expect("task manager");
            let before = m
                .journal
                .tasks
                .get(&id)
                .expect("task record")
                .snapshot
                .clone();
            let poisoned = m.journal.poisoned;
            let unknown = before.state == EnvironmentTaskState::Unknown;
            let container_absent = m.container_absent;
            let launcher = m.launcher.clone();
            let input_failed = {
                let Some(live) = m.live.get_mut(&id) else {
                    break;
                };
                if live
                    .input_worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_finished())
                {
                    !live
                        .input_worker
                        .take()
                        .expect("finished input")
                        .join()
                        .unwrap_or(false)
                } else {
                    false
                }
            };
            if input_failed {
                m.journal
                    .tasks
                    .get_mut(&id)
                    .expect("task record")
                    .snapshot
                    .state = EnvironmentTaskState::Unknown;
            }
            let Some(live) = m.live.get_mut(&id) else {
                break;
            };
            if !container_absent
                && (poisoned
                    || unknown
                    || input_failed
                    || (timeout_ms > 0 && began.elapsed() >= Duration::from_millis(timeout_ms)))
            {
                let _ = domains::kill(&launcher, &live.cgroup);
            }
            let input_pending = live.input_worker.is_some();
            let mut lost_wait = false;
            let leader = match live.child.try_wait() {
                Ok(Some(status)) => Some(status.code()),
                Ok(None) => None,
                Err(error) => {
                    // Missing wait ownership is not a synthesized exit receipt.
                    lost_wait = error.raw_os_error() == Some(libc::ECHILD);
                    if !container_absent {
                        let _ = domains::kill(&launcher, &live.cgroup);
                    }
                    m.journal
                        .tasks
                        .get_mut(&id)
                        .expect("task record")
                        .snapshot
                        .state = EnvironmentTaskState::Unknown;
                    None
                }
            };
            let empty = container_absent
                || domains::events(&m.launcher, &m.cgroups.join(&id))
                    .is_ok_and(|s| s.lines().any(|line| line == "populated 0"));
            let snapshot = &mut m.journal.tasks.get_mut(&id).expect("task record").snapshot;
            crate::host_supervisor::physical_snapshot(
                snapshot,
                leader,
                eof == [Some(true), Some(true)],
                empty,
            );
            if container_absent {
                snapshot.cleanup_complete = true;
            }
            if *snapshot != before {
                let _ = m.persist();
            }
            (leader.is_some() || (container_absent && lost_wait))
                && empty
                && !input_pending
                && eof.iter().all(Option::is_some)
        };
        if finished {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    for worker in workers.into_iter().flatten() {
        let _ = worker.join();
    }
    let mut m = manager.lock().expect("task manager");
    m.live.remove(&id);
}

/// Outcome of an idle-only physical operation under this instance's admission fence.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IdleEnvironmentOperation<T> {
    Completed(T),
    NotIdle,
    Unknown,
}

/// A manager is always bound to a physically attested native target.
pub(crate) struct NativeManager {
    manager: Arc<Mutex<Manager>>,
    control: PathBuf,
    output: PathBuf,
    helper_root: PathBuf,
    helper_binary: Option<Arc<HelperBinary>>,
    workspace_root: String,
    helper_admission: Mutex<()>,
    reaper: Mutex<Option<thread::JoinHandle<()>>>,
}
impl NativeManager {
    #[cfg(test)]
    pub(crate) fn retirement_fixture(
        control: PathBuf,
        output: PathBuf,
        instance: &str,
    ) -> Result<Self, String> {
        let manager = Manager::open(
            TaskStorage::new(control.clone(), output.clone(), OUTPUT_BUDGET_BYTES)?,
            control.join("unused-cgroups"),
            instance,
        )?;
        Ok(Self {
            manager: Arc::new(Mutex::new(manager)),
            helper_root: output.clone(),
            control,
            output,
            helper_binary: None,
            workspace_root: "/".into(),
            helper_admission: Mutex::new(()),
            reaper: Mutex::new(None),
        })
    }
    pub(crate) fn open(
        control: PathBuf,
        output: PathBuf,
        cgroups: PathBuf,
        instance: &str,
        launcher: OciTarget,
        helper_root: PathBuf,
        helper_binary: Option<Arc<HelperBinary>>,
    ) -> Result<Self, String> {
        domains::initialize(&launcher, &cgroups)?;
        let lease = ControlLease::acquire(&control)?;
        let journal = Journal::open(&control, instance)?;
        let storage = TaskStorage::new(control.clone(), output.clone(), OUTPUT_BUDGET_BYTES)?;
        let mut manager = Manager::from_journal(storage, cgroups, journal, lease)?;
        let (workspace_root, _) = crate::host_supervisor::workspace_context(&launcher.process)?;
        manager.helper_root = Some(helper_root.clone());
        manager.launcher = Some(Arc::new(launcher));
        let manager = Arc::new(Mutex::new(manager));
        let reaper = start_reaper(&manager)?;
        Ok(Self {
            manager,
            control,
            output,
            helper_root,
            helper_binary,
            workspace_root,
            helper_admission: Mutex::new(()),
            reaper: Mutex::new(Some(reaper)),
        })
    }
    pub(crate) fn handle(
        &self,
        request: EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String> {
        // Trusted helpers already hold this same fence before entering Manager.
        // Query, input receipts and termination retain their independent control path.
        let _admission = if matches!(&request, EnvironmentTaskRequest::Start { .. }) {
            Some(
                self.helper_admission
                    .lock()
                    .map_err(|_| "task_service_failed")?,
            )
        } else {
            None
        };
        Manager::handle(&self.manager, request)
    }

    /// Keep every new Start/helper outside the idle check and physical callback.
    /// The caller verifies physical identity and records authoritative power facts.
    /// This gate creates no paused marker and does not change task deadlines.
    pub(crate) fn with_idle_environment_operation<T>(
        &self,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<IdleEnvironmentOperation<T>, String> {
        let _admission = match self.helper_admission.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Ok(IdleEnvironmentOperation::NotIdle)
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Ok(IdleEnvironmentOperation::Unknown)
            }
        };
        let manager = match self.manager.lock() {
            Ok(manager) => manager,
            Err(_) => return Ok(IdleEnvironmentOperation::Unknown),
        };
        if manager.journal.poisoned
            || manager.journal.takeover_pending
            || manager.container_absent
            || manager.stop_reaper
            || manager
                .journal
                .receipts
                .values()
                .any(|receipt| receipt.result.is_none())
            || manager.journal.tasks.values().any(|record| {
                record.snapshot.state == EnvironmentTaskState::Unknown
                    && !record.snapshot.cleanup_complete
            })
        {
            return Ok(IdleEnvironmentOperation::Unknown);
        }
        if !manager.live.is_empty()
            || manager.drain_workers.load(Ordering::Acquire) != 0
            || manager
                .journal
                .tasks
                .values()
                .any(|record| !record.snapshot.cleanup_complete || !record.snapshot.output_complete)
        {
            return Ok(IdleEnvironmentOperation::NotIdle);
        }
        // Preserve query/termination and reconciliation while the bounded physical
        // operation runs. Only execution admission remains fenced per instance.
        drop(manager);
        operation().map(IdleEnvironmentOperation::Completed)
    }

    pub(crate) fn inspection_with_budget(
        &self,
        budget: &crate::host_supervisor::linux::probe::ProbeBudget<'_>,
    ) -> Result<serde_json::Value, String> {
        let _guard = budget.lock(&self.manager)?;
        budget.check()?;
        let value = store::inspection(&self.control, &self.output)?;
        budget.check()?;
        Ok(value)
    }

    #[cfg(test)]
    pub(crate) fn inspection(&self) -> Result<serde_json::Value, String> {
        let _guard = self.manager.lock().map_err(|_| "task_service_failed")?;
        store::inspection(&self.control, &self.output)
    }
    /// Called only after the host independently proves this exact Docker ID absent.
    /// It never translates physical container absence into a successful task exit.
    pub(crate) fn retire_after_absence(&self) -> Result<(), String> {
        let ready = retire_manager(&self.manager)?;
        let mut reaper = self.reaper.lock().map_err(|_| "task_service_failed")?;
        if reaper.as_ref().is_some_and(|worker| worker.is_finished()) {
            reaper
                .take()
                .expect("finished reaper")
                .join()
                .map_err(|_| "host_retirement_pending")?;
        }
        if !ready || reaper.is_some() || Arc::strong_count(&self.manager) != 1 {
            return Err("host_retirement_busy".into());
        }
        Ok(())
    }
}

fn retire_manager(manager: &Arc<Mutex<Manager>>) -> Result<bool, String> {
    let mut m = manager.lock().map_err(|_| "task_service_failed")?;
    m.container_absent = true;
    m.stop_reaper = true;
    let mut finished = Vec::new();
    let mut changed = false;
    for (id, live) in &mut m.live {
        live.input.take();
        if !live.supervisor.is_finished() {
            continue;
        }
        if live
            .input_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            continue;
        }
        let leader = match live.child.try_wait() {
            Ok(None) => continue,
            Ok(Some(status)) => Some(status.code()),
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => None,
            Err(_) => continue,
        };
        finished.push((id.clone(), leader));
    }
    for (id, leader) in finished {
        let live = m.live.remove(&id).expect("finished supervisor");
        let _ = live.supervisor.join();
        if let Some(worker) = live.input_worker {
            let _ = worker.join();
        }
        let snapshot = &mut m
            .journal
            .tasks
            .get_mut(&id)
            .ok_or("task_not_found")?
            .snapshot;
        // A fully recorded exit/EOF can precede release of its final handle.
        // Preserve that confirmed outcome; incomplete supervision stays unknown.
        if snapshot.state != EnvironmentTaskState::Exited || !snapshot.output_complete {
            snapshot.state = EnvironmentTaskState::Unknown;
            if let Some(exit) = leader {
                snapshot.exit_code = exit;
            }
        }
        changed = true;
    }
    for record in m.journal.tasks.values_mut() {
        if !record.snapshot.cleanup_complete {
            record.snapshot.cleanup_complete = true;
            changed = true;
        }
    }
    if changed {
        m.persist()?;
    }
    Ok(m.live.is_empty() && m.drain_workers.load(Ordering::Acquire) == 0)
}
/// Workload PID 1 has no controller socket, state or elevated host authority.
pub fn workload_keeper() -> Result<(), String> {
    // SAFETY: getpid has no arguments or side effects.
    if unsafe { libc::getpid() } != 1 {
        return Err("workload_keeper_requires_init".into());
    }
    loop {
        // SAFETY: the workload init only reaps its own orphaned descendants.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        thread::sleep(Duration::from_millis(100));
    }
}
#[cfg(test)]
fn spawn_local(
    program: &str,
    args: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
    fd: i32,
) -> Result<Spawned, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/home/agent")
        .env("LANG", "C.UTF-8")
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: only async-signal-safe syscalls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            assign_self(fd)?;
            if libc::setpgid(0, 0) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(10001) != 0
                || libc::setuid(10001) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = commands::OwnedChild::spawn(&mut command).map_err(|_| "task_launch_unknown")?;
    let input = child.stdin.take();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    Ok(Spawned {
        child: TaskChild::Local(child),
        input,
        stdout,
        stderr,
    })
}

fn start_reaper(manager: &Arc<Mutex<Manager>>) -> Result<thread::JoinHandle<()>, String> {
    // SAFETY: enabling subreaping only adopts orphaned descendants of this service.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err("task_subreaper_unavailable".into());
    }
    let weak = Arc::downgrade(manager);
    thread::Builder::new()
        .name("task-reaper".into())
        .spawn(move || {
            while let Some(manager) = weak.upgrade() {
                if let Ok(mut m) = manager.lock() {
                    // A worker panic cannot turn an owned live child into a successful
                    // completion. Keep its handle until exact-tree cleanup and wait agree.
                    let failed: Vec<_> = m
                        .live
                        .iter()
                        .filter(|(_, live)| !m.stop_reaper && live.supervisor.is_finished())
                        .map(|(id, _)| id.clone())
                        .collect();
                    let launcher = m.launcher.clone();
                    for id in failed {
                        #[cfg(test)]
                        let defer_reap = m.defer_failed_child_reap;
                        #[cfg(not(test))]
                        let defer_reap = false;
                        let live = m.live.get_mut(&id).expect("owned child");
                        let clean = domains::kill(&launcher, &live.cgroup).unwrap_or(false);
                        let reaped = !defer_reap && matches!(live.child.try_wait(), Ok(Some(_)));
                        let snapshot =
                            &mut m.journal.tasks.get_mut(&id).expect("task record").snapshot;
                        let changed =
                            snapshot.state != EnvironmentTaskState::Unknown || (clean && reaped);
                        snapshot.state = EnvironmentTaskState::Unknown;
                        snapshot.cleanup_complete = clean && reaped;
                        snapshot.output_complete = false;
                        if clean && reaped {
                            m.live.remove(&id);
                        }
                        if changed {
                            let _ = m.persist();
                        }
                    }
                    commands::reap_adopted();
                    // Retiring managers make one final adopted-child pass after leaders and I/O workers finish.
                    if m.stop_reaper
                        && m.live.is_empty()
                        && m.drain_workers.load(Ordering::Acquire) == 0
                    {
                        break;
                    }
                }
                drop(manager);
                thread::sleep(Duration::from_millis(100));
            }
        })
        .map_err(|_| "task_reaper_start_failed".into())
}
#[cfg(test)]
fn relay_exchange(stream: &mut (impl Read + Write), bytes: &[u8]) -> Result<Vec<u8>, String> {
    stream
        .write_all(bytes)
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|_| "task_relay_write_unknown")?;
    let mut output = Vec::new();
    BufReader::new(stream)
        .take((MAX_RESPONSE + 1) as u64)
        .read_until(b'\n', &mut output)
        .map_err(|_| "task_relay_response_unknown")?;
    if output.len() > MAX_RESPONSE || output.last() != Some(&b'\n') {
        return Err("task_relay_response_unknown".into());
    }
    let response: EnvironmentTaskResponse =
        serde_json::from_slice(&output).map_err(|_| "task_relay_response_unknown")?;
    response
        .validate()
        .map_err(|_| "task_relay_response_unknown")?;
    Ok(output)
}
#[cfg(test)]
mod tests;
