use super::*;
use std::{
    ffi::CString,
    io::{BufRead, BufReader},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
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

const SOCKET: &str = "/run/centaeris/tasks.sock";
const CONTROL_ROOT: &str = "/run/centaeris/task-control";
const OUTPUT_ROOT: &str = "/run/centaeris/task-output";
const OUTPUT_BUDGET_BYTES: u64 = (ENVIRONMENT_TASK_MAX_RECORDS
    * (ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK)
    * (HEADER + CHUNK)) as u64;
const MAX_REQUEST: usize = 128 * 1024;
const MAX_RESPONSE: usize = 512 * 1024;
struct Live {
    child: Child,
    input: Option<ChildStdin>,
    cgroup: PathBuf,
    supervisor: thread::JoinHandle<()>,
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
struct ServiceControl {
    lease: ControlLease,
    listener: UnixListener,
    journal: Journal,
}
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
    journal: Journal,
    live: BTreeMap<String, Live>,
    logs: BTreeMap<String, Arc<Mutex<OutputLog>>>,
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
            journal,
            live: BTreeMap::new(),
            logs,
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
            let events = fs::read_to_string(self.cgroups.join(id).join("cgroup.events"))
                .map_err(|_| "task_tree_observation_failed")?;
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
            .map(|f| f.controller_id.clone())
            .ok_or("task_controller_fenced")?;
        let mut after = String::new();
        while let Some((id, _)) = store::next_closed(&self.storage.control_root, &after, &current)?
        {
            after = id.clone();
            if self.live.contains_key(&id) {
                continue;
            }
            self.logs.remove(&id);
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
                    r.can_retire(epoch, &f.controller_id, self.live.contains_key(*id))
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
                let events = fs::read_to_string(domain.join("cgroup.events"))
                    .map_err(|_| "task_tree_observation_failed")?;
                if !events.lines().any(|line| line == "populated 0") {
                    continue;
                }
                fs::remove_dir(&domain).map_err(|_| "task_history_retirement_pending")?;
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
        request.validate_authority()?;
        let mut m = manager.lock().map_err(|_| "task_service_failed")?;
        match request {
            EnvironmentTaskRequest::Takeover {
                environment_instance_id,
                controller_id,
                controller_epoch,
                controller_purpose,
            } => {
                let fence = EnvironmentTaskFence {
                    environment_instance_id,
                    controller_id,
                    controller_epoch,
                    controller_purpose,
                };
                m.journal.takeover(fence.clone())?;
                let previous_run_tasks: Vec<_> = m.journal.tasks.iter().filter(|(_, r)| !r.snapshot.cleanup_complete && matches!(&r.snapshot.scope, EnvironmentTaskScope::Run { run_id } if run_id != &fence.controller_id)).map(|(id, _)| id.clone()).collect();
                m.journal.takeover_pending = !previous_run_tasks.is_empty();
                m.persist()?;
                let mut clean = true;
                for id in previous_run_tasks {
                    let empty = cgroup_kill(&m.cgroups.join(&id)).unwrap_or(false);
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
                if let EnvironmentTaskScope::Run { run_id } = &scope {
                    valid_operation(run_id)?;
                    if run_id != &fence.controller_id {
                        return Err("task_run_scope_denied".into());
                    }
                }
                let (_, opaque) = operation_id
                    .split_once(':')
                    .ok_or("task_invalid_operation_id")?;
                let d = digest(&(
                    &fence.environment_instance_id,
                    &fence.controller_id,
                    &fence.controller_purpose,
                    opaque,
                    &scope,
                    &program,
                    &args,
                    &cwd,
                    timeout_ms,
                ))?;
                let id = task_identity(&fence, &operation_id)?;
                if m.journal.existing(&id, &d)?.is_some() {
                    return m.query(&id, 0, ENVIRONMENT_TASK_READ_BYTES);
                }
                let cwd = fs::canonicalize(cwd).map_err(|_| "task_invalid_cwd")?;
                if !cwd.starts_with("/mnt/data") && !cwd.starts_with("/home/agent") {
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
                    creator_controller_id: fence.controller_id.clone(),
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
                let cgroup = m.cgroups.join(&id);
                verify_domain(&m.cgroups)?;
                fs::create_dir(&cgroup).map_err(|_| "task_cgroup_create_failed")?;
                // Task groups provide exact tree ownership. Runtime verifies the enclosing
                // Docker instance's aggregate memory, CPU and PID limits before activation.
                verify_domain(&cgroup)?;
                let procs = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(cgroup.join("cgroup.procs"))
                    .map_err(|_| "task_cgroup_assignment_unavailable")?;
                let log = match m
                    .storage
                    .open_output(&id, ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK)
                {
                    Ok(log) => Arc::new(Mutex::new(log)),
                    Err(error) => {
                        drop(procs);
                        let _ = fs::remove_dir(&cgroup);
                        return Err(error);
                    }
                };
                m.logs.insert(id.clone(), log.clone());
                m.journal.tasks.insert(id.clone(), record);
                if let Err(error) = m.persist() {
                    if error == "task_index_capacity" {
                        // SQLITE_FULL rolls back the pre-spawn intent. Keep other
                        // tasks supervised and discard only this unaccepted task.
                        m.journal.tasks.remove(&id);
                        m.logs.remove(&id);
                        drop(log);
                        drop(procs);
                        let _ = fs::remove_file(m.storage.output_root.join(format!("{id}.output")));
                        let _ = fs::remove_dir(&cgroup);
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
                let fd = procs.as_raw_fd();
                let mut command = Command::new(program);
                command
                    .args(args)
                    .current_dir(cwd)
                    .env_clear()
                    .env(
                        "PATH",
                        "/opt/centaeris/bin:/opt/centaeris/venv/bin:/usr/local/bin:/usr/bin:/bin",
                    )
                    .env("HOME", "/home/agent")
                    .env("LANG", "C.UTF-8")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                // SAFETY: pre_exec only calls async-signal-safe syscalls. Parent retains the cgroup fd through spawn.
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
                match command.spawn() {
                    Ok(mut child) => {
                        let stdout = child.stdout.take().expect("piped stdout");
                        let stderr = child.stderr.take().expect("piped stderr");
                        let input = child.stdin.take();
                        let mut input_failed = false;
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
                                },
                                DrainWork {
                                    reader: Box::new(stderr),
                                    log,
                                    stream: EnvironmentTaskStream::Stderr,
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
                            let _ = cgroup_kill(&cgroup);
                            m.persist()?;
                            return Err("task_worker_delivery_failed".into());
                        }
                        #[cfg(test)]
                        if m.fail_running_commit {
                            fs::create_dir(m.storage.control_root.join("history.sqlite-journal"))
                                .map_err(|_| "fault_injection_failed")?;
                        }
                        if input_failed {
                            let _ = cgroup_kill(&cgroup);
                            m.journal.poisoned = true;
                            return Err("task_input_configuration_failed".into());
                        }
                        m.persist()?;
                    }
                    Err(_) => {
                        let clean = cgroup_kill(&cgroup).unwrap_or(false);
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
                if m.journal.closed_run.as_ref() == Some(&fence.controller_id)
                    || store::closed(&m.storage.control_root, &fence.controller_id)?
                {
                    return Err("task_run_closed".into());
                }
                let key = input_identity(&fence, &task_id, &operation_id)?;
                let d = digest(&(&data, close))?;
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
                    cgroup_kill(&m.cgroups.join(&task_id))?;
                }
                let result = m.mutation(&task_id)?;
                m.finish(key, result)
            }
            EnvironmentTaskRequest::EndRun {
                fence,
                run_id,
                operation_id,
            } => {
                m.journal.check(&fence)?;
                valid_operation(&operation_id)?;
                valid_operation(&run_id)?;
                if run_id != fence.controller_id {
                    return Err("task_run_scope_denied".into());
                }
                let key = format!("endRun:{run_id}");
                let d = digest(&run_id)?;
                if let Some(r) = m.receipt(&key, &d)? {
                    return Ok(r);
                }
                m.journal.closed_run = Some(run_id.clone());
                m.persist()?;
                let task_ids: Vec<_> = m
                    .journal
                    .tasks
                    .iter()
                    .filter(|(_, r)| {
                        r.snapshot.scope
                            == (EnvironmentTaskScope::Run {
                                run_id: run_id.clone(),
                            })
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                let mut cleanup_complete = true;
                for id in &task_ids {
                    let clean = m.journal.tasks[id].snapshot.cleanup_complete
                        || cgroup_kill(&m.cgroups.join(id))?;
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
                    EnvironmentTaskResult::RunEnded {
                        task_ids,
                        cleanup_complete,
                    },
                )
            }
        }
    }
}
struct DrainWork {
    reader: Box<dyn Read + Send>,
    log: Arc<Mutex<OutputLog>>,
    stream: EnvironmentTaskStream,
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
    } = work;
    let mut buf = [0; CHUNK];
    let mut okay = true;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return okay,
            Ok(n) => {
                if log.lock().map_or(true, |mut log| {
                    log.append(stream.clone(), &buf[..n]).is_err()
                }) {
                    okay = false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}
fn supervise(
    manager: Arc<Mutex<Manager>>,
    id: String,
    timeout_ms: u64,
    out: thread::JoinHandle<bool>,
    err: thread::JoinHandle<bool>,
) {
    let began = Instant::now();
    loop {
        let finished = {
            let mut m = manager.lock().expect("task manager");
            let poisoned = m.journal.poisoned;
            let unknown = m
                .journal
                .tasks
                .get(&id)
                .is_some_and(|record| record.snapshot.state == EnvironmentTaskState::Unknown);
            let Some(live) = m.live.get_mut(&id) else {
                break;
            };
            if poisoned
                || unknown
                || (timeout_ms > 0 && began.elapsed() >= Duration::from_millis(timeout_ms))
            {
                let _ = cgroup_kill(&live.cgroup);
            }
            match live.child.try_wait() {
                Ok(Some(status)) => {
                    let clean = cgroup_kill(&live.cgroup).unwrap_or(false);
                    let snapshot = &mut m.journal.tasks.get_mut(&id).expect("task record").snapshot;
                    if !unknown {
                        snapshot.state = EnvironmentTaskState::Exited;
                    }
                    snapshot.exit_code = status.code();
                    snapshot.cleanup_complete = clean;
                    let _ = m.persist();
                    true
                }
                Ok(None) => false,
                Err(_) => {
                    let _ = cgroup_kill(&live.cgroup);
                    m.journal
                        .tasks
                        .get_mut(&id)
                        .expect("task record")
                        .snapshot
                        .state = EnvironmentTaskState::Unknown;
                    false
                }
            }
        };
        if finished {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let output_complete = out.join().unwrap_or(false) & err.join().unwrap_or(false);
    let mut m = manager.lock().expect("task manager");
    if let Some(live) = m.live.remove(&id) {
        let clean = cgroup_kill(&live.cgroup).unwrap_or(false);
        if let Some(r) = m.journal.tasks.get_mut(&id) {
            r.snapshot.cleanup_complete = clean;
            r.snapshot.output_complete = output_complete;
        }
    }
    let _ = m.persist();
}

pub fn serve(instance: &str, cgroups: &Path) -> Result<(), String> {
    // SAFETY: geteuid has no arguments or side effects.
    if unsafe { libc::geteuid() } != 0 {
        return Err("task_service_requires_root".into());
    }
    if instance.is_empty() || instance.len() > 128 {
        return Err("task_tree_supervision_unavailable".into());
    }
    verify_domain(cgroups)?;
    let control_root = PathBuf::from(CONTROL_ROOT);
    fs::create_dir_all(&control_root).map_err(|_| "task_state_create_failed")?;
    fs::set_permissions(&control_root, fs::Permissions::from_mode(0o700))
        .map_err(|_| "task_state_protection_failed")?;
    let control = prepare_control(&control_root, Path::new(SOCKET), instance)?;
    let output_root = PathBuf::from(OUTPUT_ROOT);
    let _ = fs::create_dir_all(&output_root);
    if output_root.exists() {
        let _ = fs::set_permissions(&output_root, fs::Permissions::from_mode(0o700));
    }
    let storage = TaskStorage::new(control_root, output_root, OUTPUT_BUDGET_BYTES)?;
    let manager = Arc::new(Mutex::new(Manager::from_journal(
        storage,
        cgroups.into(),
        control.journal,
        control.lease,
    )?));
    start_reaper(&manager)?;
    let listener = control.listener;
    for stream in listener.incoming() {
        let stream = stream.map_err(|_| "task_socket_accept_failed")?;
        let result = request(
            &manager,
            stream.try_clone().map_err(|_| "task_socket_clone_failed")?,
        );
        let response = EnvironmentTaskResponse::from_result(result);
        let mut stream = stream;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| "task_socket_timeout_failed")?;
        let _ = serde_json::to_writer(&mut stream, &response);
        let _ = stream.write_all(b"\n");
    }
    Ok(())
}

/// Docker starts this trusted supervisor as PID 1. It owns the instance, while
/// Runtime connections may come and go. Never adopt a second initialization.
pub fn serve_instance(instance: &str) -> Result<(), String> {
    // SAFETY: identity syscalls have no arguments or side effects.
    if unsafe { libc::geteuid() } != 0 || unsafe { libc::getpid() } != 1 {
        return Err("task_instance_requires_root_init".into());
    }
    let id = instance
        .strip_prefix("instance_")
        .ok_or("task_instance_identity_invalid")?;
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("task_instance_identity_invalid".into());
    }
    let tasks = super::instance::task_domain(
        Path::new("/run/centaeris/instance-cgroups"),
        1,
        verify_domain,
    )?;
    thread::Builder::new()
        .name("workspace-watch".into())
        .spawn(|| {
            if let Err(error) = crate::agent::run_workspace_watch() {
                eprintln!("workspace watcher unavailable; generation is Unknown: {error}");
            }
        })
        .map_err(|_| "workspace_watcher_start_failed")?;
    serve(instance, &tasks)
}

fn start_reaper(manager: &Arc<Mutex<Manager>>) -> Result<(), String> {
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
                        .filter(|(_, live)| live.supervisor.is_finished())
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in failed {
                        #[cfg(test)]
                        let defer_reap = m.defer_failed_child_reap;
                        #[cfg(not(test))]
                        let defer_reap = false;
                        let live = m.live.get_mut(&id).expect("owned child");
                        let clean = cgroup_kill(&live.cgroup).unwrap_or(false);
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
                    let direct: std::collections::BTreeSet<_> =
                        m.live.values().map(|live| live.child.id()).collect();
                    if let Ok(entries) = fs::read_dir("/proc") {
                        for entry in entries.flatten() {
                            let Some(pid) = entry
                                .file_name()
                                .to_str()
                                .and_then(|s| s.parse::<u32>().ok())
                            else {
                                continue;
                            };
                            if direct.contains(&pid) {
                                continue;
                            }
                            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                                continue;
                            };
                            let Some((_, fields)) = stat.rsplit_once(')') else {
                                continue;
                            };
                            if fields
                                .split_whitespace()
                                .nth(1)
                                .and_then(|s| s.parse::<u32>().ok())
                                == Some(std::process::id())
                            {
                                // SAFETY: only reap our adopted children; direct Child handles retain exclusive wait ownership.
                                unsafe {
                                    libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG);
                                }
                            }
                        }
                    }
                }
                drop(manager);
                thread::sleep(Duration::from_millis(100));
            }
        })
        .map_err(|_| "task_reaper_start_failed")?;
    Ok(())
}
fn request(
    manager: &Arc<Mutex<Manager>>,
    stream: UnixStream,
) -> Result<EnvironmentTaskResult, String> {
    // SO_PEERCRED is the actual transport authority, never request-provided identity.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: valid socket fd and correctly sized output buffer.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || cred.uid != 0
    {
        return Err("task_controller_unauthorized".into());
    }
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "task_socket_timeout_failed")?;
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take((MAX_REQUEST + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "task_request_read_failed")?;
    if bytes.len() > MAX_REQUEST || bytes.last() != Some(&b'\n') {
        return Err("task_request_too_large".into());
    }
    let envelope: EnvironmentTaskEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| "task_request_invalid")?;
    envelope.validate()?;
    Manager::handle(manager, envelope.request)
}
pub fn relay_once() -> Result<(), String> {
    let metadata = fs::metadata(SOCKET).map_err(|_| "task_service_unavailable")?;
    if metadata.uid() != 0 || metadata.mode() & 0o777 != 0o600 {
        return Err("task_socket_untrusted".into());
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take((MAX_REQUEST + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "task_relay_input_failed")?;
    if bytes.len() > MAX_REQUEST {
        return Err("task_request_too_large".into());
    }
    let envelope: EnvironmentTaskEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| "task_request_invalid")?;
    envelope.validate()?;
    let mut stream = UnixStream::connect(SOCKET).map_err(|_| "task_service_unavailable")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "task_relay_timeout_failed")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "task_relay_timeout_failed")?;
    let output = relay_exchange(&mut stream, &bytes)?;
    std::io::stdout()
        .write_all(&output)
        .map_err(|_| "task_relay_output_unknown".into())
}

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
