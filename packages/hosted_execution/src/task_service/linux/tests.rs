//! These tests require an explicitly delegated, disposable cgroup subtree.
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

mod capacity_history_tests;
mod idle_pause_gate_tests;
static SEQ: AtomicU64 = AtomicU64::new(0);

#[test]
fn retirement_preserves_a_confirmed_exit_while_the_supervisor_handle_is_still_present() {
    let root = lock_root();
    let control = root.join("control");
    let output = root.join("output");
    let groups = root.join("groups");
    for path in [&control, &output, &groups] {
        fs::create_dir(path).unwrap();
    }
    seeded_running_journal(&control);
    let mut manager = Manager::open(
        TaskStorage::new(control, output, OUTPUT_BUDGET_BYTES).unwrap(),
        groups.clone(),
        "i",
    )
    .unwrap();
    let id = manager.journal.tasks.keys().next().unwrap().clone();
    let snapshot = &mut manager.journal.tasks.get_mut(&id).unwrap().snapshot;
    snapshot.state = EnvironmentTaskState::Exited;
    snapshot.exit_code = Some(7);
    snapshot.output_complete = true;
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exit 7"])
        .spawn()
        .unwrap();
    child.wait().unwrap();
    let supervisor = thread::spawn(|| ());
    while !supervisor.is_finished() {
        thread::yield_now();
    }
    manager.live.insert(
        id.clone(),
        Live {
            child: TaskChild::Local(child),
            input: None,
            cgroup: groups.join(&id),
            supervisor,
            input_worker: None,
        },
    );
    let manager = Arc::new(Mutex::new(manager));
    assert!(retire_manager(&manager).unwrap());
    let snapshot = manager.lock().unwrap().journal.tasks[&id].snapshot.clone();
    assert_eq!(snapshot.state, EnvironmentTaskState::Exited);
    assert_eq!(snapshot.exit_code, Some(7));
    assert!(snapshot.output_complete);
    assert!(snapshot.cleanup_complete);
    drop(manager);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn retirement_waits_for_a_reserved_helper_input_worker() {
    let root = lock_root();
    let control = root.join("control");
    let output = root.join("output");
    for path in [&control, &output] {
        fs::create_dir(path).unwrap();
    }
    let manager = Manager::open(
        TaskStorage::new(control, output, OUTPUT_BUDGET_BYTES).unwrap(),
        root.join("unused-cgroups"),
        "i",
    )
    .unwrap();
    let worker_lease = HelperInputLease {
        _input_guard: None,
        _worker: DrainLease::new(&manager.drain_workers),
    };
    let input = File::create(root.join("input")).unwrap();
    let worker = ReservedInput::new(input, Some(Arc::new(worker_lease))).unwrap();
    let manager = Arc::new(Mutex::new(manager));
    assert!(!retire_manager(&manager).unwrap());
    drop(worker);
    assert!(retire_manager(&manager).unwrap());
    drop(manager);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn retirement_waits_for_output_writers_without_inventing_exit_or_eof_facts() {
    let root = lock_root();
    let control = root.join("control");
    let output = root.join("output");
    let groups = root.join("groups");
    for path in [&control, &output, &groups] {
        fs::create_dir(path).unwrap();
    }
    seeded_running_journal(&control);
    let mut manager = Manager::open(
        TaskStorage::new(control.clone(), output, OUTPUT_BUDGET_BYTES).unwrap(),
        groups,
        "i",
    )
    .unwrap();
    let id = manager.journal.tasks.keys().next().unwrap().clone();
    manager.journal.tasks.get_mut(&id).unwrap().snapshot.state = EnvironmentTaskState::Unknown;
    let original = manager.journal.tasks[&id].snapshot.clone();
    let writer = DrainLease::new(&manager.drain_workers);
    let manager = Arc::new(Mutex::new(manager));
    let reaper = start_reaper(&manager).unwrap();
    assert!(
        !retire_manager(&manager).unwrap(),
        "active output writers keep retirement pending"
    );
    drop(writer);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !reaper.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(reaper.is_finished(), "retirement must stop its own reaper");
    reaper.join().unwrap();
    assert!(retire_manager(&manager).unwrap());
    let snapshot = manager.lock().unwrap().journal.tasks[&id].snapshot.clone();
    assert_eq!(snapshot.state, original.state);
    assert_eq!(snapshot.exit_code, original.exit_code);
    assert_eq!(snapshot.output_complete, original.output_complete);
    assert!(
        snapshot.cleanup_complete,
        "only independently proven domain absence changes"
    );
    drop(manager);
    fs::remove_dir_all(root).unwrap();
}

// An unrelated test's fork inherits flock descriptions until exec closes them.
// Fixtures that assert immediate lock release therefore own an entire process.
fn isolated_lock_fixture(flag: &str, name: &str) -> bool {
    if std::env::var_os(flag).is_some() {
        return false;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("task_service::linux::tests::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(flag, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated lock fixture failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

struct LockProcess {
    child: Child,
    root: PathBuf,
}
impl LockProcess {
    fn spawn(root: &Path, role: &str) -> Self {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "task_service::linux::tests::control_lock_process_fixture",
                "--nocapture",
            ])
            .env("CENTAERIS_LOCK_FIXTURE_ROOT", root)
            .env("CENTAERIS_LOCK_FIXTURE_ROLE", role)
            .spawn()
            .unwrap();
        let mut process = Self {
            child,
            root: root.into(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.join("ready").exists() {
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "lock fixture child exited early"
            );
            assert!(
                Instant::now() < deadline,
                "lock fixture child did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        process
    }
}
impl Drop for LockProcess {
    fn drop(&mut self) {
        let _ = fs::write(self.root.join("release"), b"release");
        let _ = self.child.wait();
    }
}
#[test]
fn control_lock_process_fixture() {
    let Ok(root) = std::env::var("CENTAERIS_LOCK_FIXTURE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let role = std::env::var("CENTAERIS_LOCK_FIXTURE_ROLE").unwrap();
    let file = if role == "holder" {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("service.lock"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        Some(file)
    } else {
        None
    };
    fs::write(root.join("ready"), b"ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("release").exists() {
        assert!(Instant::now() < deadline, "lock fixture release timed out");
        thread::sleep(Duration::from_millis(10));
    }
    drop(file);
}
fn lock_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "task-lock-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn supervisor_commits_eof_before_domain_cleanup_without_rewriting_unchanged_facts() {
    let root = lock_root();
    let control = root.join("control");
    let output = root.join("output");
    let groups = root.join("groups");
    for path in [&control, &output, &groups] {
        fs::create_dir(path).unwrap();
    }
    seeded_running_journal(&control);
    let storage = TaskStorage::new(control.clone(), output, OUTPUT_BUDGET_BYTES).unwrap();
    let mut manager = Manager::open(storage, groups.clone(), "i").unwrap();
    let id = manager.journal.tasks.keys().next().unwrap().clone();
    manager.journal.tasks.get_mut(&id).unwrap().snapshot.state = EnvironmentTaskState::Running;
    fs::create_dir(groups.join(&id)).unwrap();
    fs::write(groups.join(&id).join("cgroup.events"), "populated 1\n").unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "exec sleep 5"])
        .spawn()
        .unwrap();
    let pid = child.id();
    let manager = Arc::new(Mutex::new(manager));
    let mut guard = manager.lock().unwrap();
    let worker_manager = manager.clone();
    let worker_id = id.clone();
    let worker = thread::spawn(move || {
        supervise(
            worker_manager,
            worker_id,
            0,
            thread::spawn(|| true),
            thread::spawn(|| true),
        )
    });
    guard.live.insert(
        id.clone(),
        Live {
            child: TaskChild::Local(child),
            input: None,
            cgroup: groups.join(&id),
            supervisor: worker,
            input_worker: None,
        },
    );
    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(1);
    let observed_eof = loop {
        if manager.lock().unwrap().journal.tasks[&id]
            .snapshot
            .output_complete
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let before = fs::read(control.join("history.sqlite")).unwrap();
    thread::sleep(Duration::from_millis(120));
    let unchanged = before == fs::read(control.join("history.sqlite")).unwrap();
    let before_cleanup = manager.lock().unwrap().journal.tasks[&id].snapshot.clone();
    // The fake observation seam never owns a real cgroup. Cleanup this exact
    // direct child before asserting so a failing test cannot leave it running.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    fs::write(groups.join(&id).join("cgroup.events"), "populated 0\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while manager.lock().unwrap().live.contains_key(&id) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        observed_eof,
        "stdout/stderr EOF must commit while the domain remains populated"
    );
    assert!(
        unchanged,
        "polling unchanged physical facts must not rewrite the durable index"
    );
    assert!(!before_cleanup.cleanup_complete);
    assert_eq!(before_cleanup.state, EnvironmentTaskState::Running);
    assert!(manager.lock().unwrap().live.is_empty());
    drop(manager);
    fs::remove_dir_all(root).unwrap();
}
fn seeded_running_journal(root: &Path) -> Vec<u8> {
    let mut journal = Journal::open(root, "i").unwrap();
    journal
        .takeover_persist(
            EnvironmentTaskFence {
                environment_instance_id: "i".into(),
                controller_owner: EnvironmentTaskOwner::AgentRun { run_id: "r".into() },
                controller_epoch: 3,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            },
            root,
        )
        .unwrap();
    let task = task_identity(journal.fence.as_ref().unwrap(), "3:operation").unwrap();
    journal.tasks.insert(
        task.clone(),
        Record {
            digest: "d".repeat(64),
            creator_owner: EnvironmentTaskOwner::AgentRun { run_id: "r".into() },
            identity_version: TaskIdentityVersion::OwnerV3,
            creator_epoch: 3,
            snapshot: EnvironmentTaskSnapshot {
                task_id: task.clone(),
                operation_id: "3:operation".into(),
                scope: EnvironmentTaskScope::Environment,
                state: EnvironmentTaskState::Running,
                exit_code: None,
                cleanup_complete: false,
                output_complete: false,
            },
        },
    );
    journal.persist(root).unwrap();
    fs::read(root.join("history.sqlite")).unwrap()
}
fn assert_control_unchanged(root: &Path, before: &[u8]) {
    let after = fs::read(root.join("history.sqlite")).unwrap();
    assert!(
        after.as_slice() == before,
        "control journal changed: before={:x}, after={:x}",
        Sha256::digest(before),
        Sha256::digest(&after)
    );
}
#[test]
fn startup_refuses_other_process_lock_before_reading_or_rewriting_control() {
    let root = lock_root();
    let before = seeded_running_journal(&root);
    let socket = root.join("service.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let holder = LockProcess::spawn(&root, "holder");
    assert!(prepare_control(&root, &socket, "i").is_err());
    assert_control_unchanged(&root, &before);
    drop(holder);
    drop(listener);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn startup_refuses_stale_socket_before_rewriting_control_and_keeps_socket() {
    let root = lock_root();
    let before = seeded_running_journal(&root);
    let socket = root.join("service.sock");
    drop(UnixListener::bind(&socket).unwrap());
    assert!(prepare_control(&root, &socket, "i").is_err());
    assert_control_unchanged(&root, &before);
    assert!(socket.exists());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn startup_lease_survives_manager_lifetime_but_not_exec_child() {
    if isolated_lock_fixture(
        "CENTAERIS_TASK_STARTUP_LEASE_FIXTURE",
        "startup_lease_survives_manager_lifetime_but_not_exec_child",
    ) {
        return;
    }
    let root = lock_root();
    let control_root = root.join("control");
    let output_root = root.join("output");
    fs::create_dir(&control_root).unwrap();
    fs::create_dir(&output_root).unwrap();
    let socket = root.join("service.sock");
    let control = prepare_control(&control_root, &socket, "i").unwrap();
    let manager = Manager::from_journal(
        TaskStorage::new(control_root.clone(), output_root, OUTPUT_BUDGET_BYTES).unwrap(),
        root.join("unused-cgroups"),
        control.journal,
        control.lease,
    )
    .unwrap();
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(control_root.join("service.lock"))
        .unwrap();
    assert_ne!(
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "startup must retain the exclusive lock"
    );
    let child = LockProcess::spawn(&root, "wait");
    drop(control.listener);
    assert_ne!(
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "closing listener must not release a live manager's control lock"
    );
    drop(manager);
    assert_eq!(
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "exec child must not inherit supervisor lock"
    );
    drop(contender);
    drop(child);
    fs::remove_dir_all(root).unwrap();
}

fn relay_response(response: Vec<u8>) -> Result<Vec<u8>, String> {
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let writer = thread::spawn(move || {
        let mut server = BufReader::new(server);
        let mut request = Vec::new();
        server.read_until(b'\n', &mut request).unwrap();
        assert_eq!(request, b"{}\n");
        let _ = server.get_mut().write_all(&response);
    });
    let result = relay_exchange(&mut client, b"{}");
    drop(client);
    writer.join().unwrap();
    result
}
fn valid_relay_response() -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&EnvironmentTaskResponse::from_result(Ok(
        EnvironmentTaskResult::Listed { tasks: vec![] },
    )))
    .unwrap();
    bytes.push(b'\n');
    bytes
}
#[test]
fn relay_preserves_complete_valid_response() {
    let response = valid_relay_response();
    assert_eq!(relay_response(response.clone()).unwrap(), response);
}
#[test]
fn relay_preserves_complete_semantic_error_response() {
    let mut response = serde_json::to_vec(&EnvironmentTaskResponse::from_result(Err(
        "task_controller_fenced".into(),
    )))
    .unwrap();
    response.push(b'\n');
    assert_eq!(relay_response(response.clone()).unwrap(), response);
}
#[test]
fn relay_marks_invalid_response_shape_unknown() {
    for response in [b"not-json\n".as_slice(), b"{}\n".as_slice(), br#"{"status":"error","schema":"environment.task.v1","error":"denied","result":{"kind":"listed","tasks":[]}}
"#.as_slice()] {
        assert_eq!(relay_response(response.to_vec()).unwrap_err(), "task_relay_response_unknown");
    }
}
#[test]
fn relay_marks_lost_response_unknown() {
    assert_eq!(
        relay_response(vec![]).unwrap_err(),
        "task_relay_response_unknown"
    );
}
#[test]
fn relay_marks_truncated_response_unknown() {
    let mut response = valid_relay_response();
    response.pop();
    assert_eq!(
        relay_response(response).unwrap_err(),
        "task_relay_response_unknown"
    );
}
#[test]
fn relay_marks_oversized_response_unknown() {
    let response = EnvironmentTaskResponse::from_result(Err("x".repeat(512 * 1024)));
    let mut bytes = serde_json::to_vec(&response).unwrap();
    bytes.push(b'\n');
    assert!(matches!(relay_response(bytes), Err(error) if error == "task_relay_response_unknown"));
}

#[test]
fn relay_preserves_response_at_exact_frame_limit() {
    let empty = EnvironmentTaskResponse::from_result(Err(String::new()));
    let overhead = serde_json::to_vec(&empty).unwrap().len() + 1;
    let response = EnvironmentTaskResponse::from_result(Err("x".repeat(MAX_RESPONSE - overhead)));
    let mut bytes = serde_json::to_vec(&response).unwrap();
    bytes.push(b'\n');
    assert_eq!(bytes.len(), MAX_RESPONSE);
    assert!(matches!(relay_response(bytes.clone()), Ok(output) if output == bytes));
}
#[test]
fn relay_marks_unsupported_response_schema_unknown() {
    let response = EnvironmentTaskResponse::Success {
        schema: "unsupported".into(),
        result: EnvironmentTaskResult::Listed { tasks: vec![] },
    };
    let mut bytes = serde_json::to_vec(&response).unwrap();
    bytes.push(b'\n');
    assert_eq!(
        relay_response(bytes).unwrap_err(),
        "task_relay_response_unknown"
    );
}

#[test]
fn corrupt_or_removed_output_does_not_prevent_control_restart_and_queries() {
    if isolated_lock_fixture(
        "CENTAERIS_TASK_STORAGE_RESTART_FIXTURE",
        "corrupt_or_removed_output_does_not_prevent_control_restart_and_queries",
    ) {
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "task-storage-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let control = root.join("control");
    let output = root.join("output");
    fs::create_dir_all(&control).unwrap();
    fs::create_dir(&output).unwrap();
    let fence = EnvironmentTaskFence {
        environment_instance_id: "i".into(),
        controller_owner: EnvironmentTaskOwner::AgentRun { run_id: "r".into() },
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    let mut journal = Journal::open(&control, "i").unwrap();
    journal.takeover_persist(fence.clone(), &control).unwrap();
    let task_id = task_identity(&fence, "1:operation").unwrap();
    let snapshot = EnvironmentTaskSnapshot {
        task_id: task_id.clone(),
        operation_id: "1:operation".into(),
        scope: EnvironmentTaskScope::Environment,
        state: EnvironmentTaskState::Exited,
        exit_code: Some(7),
        cleanup_complete: true,
        output_complete: true,
    };
    journal.tasks.insert(
        task_id.clone(),
        Record {
            digest: "d".repeat(64),
            creator_owner: EnvironmentTaskOwner::AgentRun { run_id: "r".into() },
            identity_version: TaskIdentityVersion::OwnerV3,
            creator_epoch: 1,
            snapshot: snapshot.clone(),
        },
    );
    journal.persist(&control).unwrap();
    drop(journal);
    let original = fs::read(control.join("history.sqlite")).unwrap();
    File::create(output.join(format!("{task_id}.output")))
        .unwrap()
        .set_len((ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK * (HEADER + CHUNK) + 1) as u64)
        .unwrap();
    for remove_output in [false, true] {
        if remove_output {
            fs::remove_dir_all(&output).unwrap();
        }
        let manager = Arc::new(Mutex::new(
            Manager::open(
                TaskStorage::new(control.clone(), output.clone(), OUTPUT_BUDGET_BYTES).unwrap(),
                root.join("unused-cgroups"),
                "i",
            )
            .unwrap(),
        ));
        let result = Manager::handle(
            &manager,
            EnvironmentTaskRequest::Query {
                fence: fence.clone(),
                task_id: task_id.clone(),
                cursor: 0,
                max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
            },
        )
        .unwrap();
        assert!(
            matches!(result, EnvironmentTaskResult::Task { task, gap: true, output, .. } if task == snapshot && output.is_empty())
        );
        assert_control_unchanged(&control, &original);
        drop(manager);
    }
    let manager = Arc::new(Mutex::new(
        Manager::open(
            TaskStorage::new(control.clone(), output, OUTPUT_BUDGET_BYTES).unwrap(),
            root.join("unused-cgroups"),
            "i",
        )
        .unwrap(),
    ));
    Manager::handle(
        &manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: "i".into(),
            controller_owner: EnvironmentTaskOwner::AgentRun { run_id: "r".into() },
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        },
    )
    .unwrap();
    drop(manager);
    assert_eq!(
        Journal::open(&control, "i")
            .unwrap()
            .fence
            .unwrap()
            .controller_epoch,
        2
    );
    fs::remove_dir_all(root).unwrap();
}
struct Fixture {
    manager: Arc<Mutex<Manager>>,
    cgroups: PathBuf,
    root: PathBuf,
    fence: EnvironmentTaskFence,
}
impl Fixture {
    fn new() -> Self {
        Self::with_controllers(false)
    }
    fn with_controllers(enabled: bool) -> Self {
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "fixture must run as root inside an isolated test environment"
        );
        let parent = PathBuf::from(
            std::env::var("CENTAERIS_TASK_TEST_CGROUP_ROOT")
                .expect("explicit disposable delegated cgroup root required"),
        );
        let suffix = format!(
            "fixture-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let cgroups = parent.join(&suffix);
        fs::create_dir(&cgroups).unwrap();
        if enabled {
            fs::write(cgroups.join("cgroup.subtree_control"), b"+pids +memory").unwrap();
        }
        let root = std::env::temp_dir().join(&suffix);
        fs::create_dir(&root).unwrap();
        let control_root = root.join("control");
        let output_root = root.join("output");
        fs::create_dir(&control_root).unwrap();
        fs::create_dir(&output_root).unwrap();
        fs::create_dir_all("/mnt/data").unwrap();
        let fence = EnvironmentTaskFence {
            environment_instance_id: suffix.clone(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run-one".into(),
            },
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        };
        let manager = Arc::new(Mutex::new(
            Manager::open(
                TaskStorage::new(control_root, output_root, OUTPUT_BUDGET_BYTES).unwrap(),
                cgroups.clone(),
                &suffix,
            )
            .unwrap(),
        ));
        start_reaper(&manager).unwrap();
        Manager::handle(
            &manager,
            EnvironmentTaskRequest::Takeover {
                environment_instance_id: suffix,
                controller_owner: EnvironmentTaskOwner::AgentRun {
                    run_id: fence.controller_owner.id().to_owned(),
                },
                controller_epoch: 1,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            },
        )
        .unwrap();
        Self {
            manager,
            cgroups,
            root,
            fence,
        }
    }
    fn start(
        &self,
        operation: &str,
        scope: EnvironmentTaskScope,
        script: &str,
    ) -> EnvironmentTaskRequest {
        EnvironmentTaskRequest::Start {
            fence: self.fence.clone(),
            operation_id: self.fence.operation_id(operation),
            scope,
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: BTreeMap::new(),
            cwd: "/mnt/data".into(),
            timeout_ms: 10_000,
        }
    }
    fn await_cleanup(&self, id: &str) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let m = self.manager.lock().unwrap();
            let Some(record) = m.journal.tasks.get(id) else {
                assert!(
                    !self.cgroups.join(id).exists(),
                    "retired task domain must be absent"
                );
                break;
            };
            let task = &record.snapshot;
            if task.cleanup_complete && task.output_complete && !m.live.contains_key(id) {
                break;
            }
            drop(m);
            assert!(
                Instant::now() < end,
                "task did not fully drain and clean up"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !self.cgroups.join(id).exists()
                || fs::read_to_string(self.cgroups.join(id).join("cgroup.events"))
                    .unwrap()
                    .lines()
                    .any(|line| line == "populated 0")
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.lock().unwrap().defer_failed_child_reap = false;
        for entry in fs::read_dir(&self.cgroups).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                let _ = cgroup_kill(&entry.path());
            }
        }
        let end = Instant::now() + Duration::from_secs(5);
        while !self.manager.lock().unwrap().live.is_empty() && Instant::now() < end {
            thread::sleep(Duration::from_millis(20));
        }
        for entry in fs::read_dir(&self.cgroups).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                fs::remove_dir(entry.path()).unwrap();
            }
        }
        fs::remove_dir(&self.cgroups).unwrap();
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn id(result: &EnvironmentTaskResult) -> String {
    match result {
        EnvironmentTaskResult::Task { task, .. } => task.task_id.clone(),
        _ => panic!("expected task"),
    }
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn one_hundred_runs_release_history_without_stopping_environment_task() {
    let mut f = Fixture::new();
    let mut resident = f.start("resident", EnvironmentTaskScope::Environment, "cat");
    if let EnvironmentTaskRequest::Start { timeout_ms, .. } = &mut resident {
        *timeout_ms = 0;
    }
    let background = id(&Manager::handle(&f.manager, resident).unwrap());
    let mut old_request = None;
    for epoch in 2..=101 {
        f.fence.controller_epoch = epoch;
        f.fence.controller_owner = EnvironmentTaskOwner::AgentRun {
            run_id: format!("run-{epoch}"),
        };
        Manager::handle(
            &f.manager,
            EnvironmentTaskRequest::Takeover {
                environment_instance_id: f.fence.environment_instance_id.clone(),
                controller_owner: EnvironmentTaskOwner::AgentRun {
                    run_id: f.fence.controller_owner.id().to_owned(),
                },
                controller_epoch: epoch,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            },
        )
        .unwrap();
        let request = f.start(
            "foreground",
            EnvironmentTaskScope::Run {
                run_id: f.fence.controller_owner.id().to_owned(),
            },
            "printf completed",
        );
        let task = id(&Manager::handle(&f.manager, request.clone()).unwrap());
        f.await_cleanup(&task);
        Manager::handle(
            &f.manager,
            EnvironmentTaskRequest::EndRun {
                fence: f.fence.clone(),
                run_id: f.fence.controller_owner.id().to_owned(),
                operation_id: f.fence.operation_id("end"),
            },
        )
        .unwrap();
        assert_eq!(
            Manager::handle(
                &f.manager,
                f.start("late", EnvironmentTaskScope::Environment, "true")
            )
            .unwrap_err(),
            "task_run_closed"
        );
        if let Some(mut stale) = old_request {
            if let EnvironmentTaskRequest::Start { fence, .. } = &mut stale {
                *fence = f.fence.clone();
            }
            assert_eq!(
                Manager::handle(&f.manager, stale).unwrap_err(),
                "task_operation_replay_expired"
            );
        }
        old_request = Some(request);
        let m = f.manager.lock().unwrap();
        assert!(m.live.contains_key(&background));
        assert!(m.journal.tasks.len() <= 2);
        assert!(m.logs.len() <= 2);
        assert!(
            fs::read_dir(&m.cgroups)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().unwrap().is_dir())
                .count()
                <= 2
        );
    }
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence: f.fence.clone(),
            task_id: background.clone(),
            operation_id: f.fence.operation_id("stop"),
        },
    )
    .unwrap();
    f.await_cleanup(&background);
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn same_run_history_retirement_keeps_output_and_progress_before_run_close() {
    let mut f = Fixture::new();
    let mut resident = f.start("resident", EnvironmentTaskScope::Environment, "cat");
    // This background task must survive the whole history test; the isolated
    // gate bounds its lifetime and fixture cleanup terminates its exact tree.
    if let EnvironmentTaskRequest::Start { timeout_ms, .. } = &mut resident {
        *timeout_ms = 0;
    }
    let background = id(&Manager::handle(&f.manager, resident).unwrap());
    for index in 0..1000 {
        let task = id(&Manager::handle(
            &f.manager,
            f.start(
                &format!("short-{index}"),
                EnvironmentTaskScope::Run {
                    run_id: f.fence.controller_owner.id().to_owned(),
                },
                "printf retained-output",
            ),
        )
        .unwrap());
        f.await_cleanup(&task);
        assert!(f.manager.lock().unwrap().journal.tasks.len() <= ENVIRONMENT_TASK_MAX_RECORDS);
    }
    let overflow = f.start(
        "overflow",
        EnvironmentTaskScope::Run {
            run_id: f.fence.controller_owner.id().to_owned(),
        },
        "false",
    );
    let task = id(&Manager::handle(&f.manager, overflow).unwrap());
    f.await_cleanup(&task);
    assert!(!f.manager.lock().unwrap().journal.poisoned);
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::EndRun {
            fence: f.fence.clone(),
            run_id: f.fence.controller_owner.id().to_owned(),
            operation_id: f.fence.operation_id("end"),
        },
    )
    .unwrap();
    f.fence.controller_owner = EnvironmentTaskOwner::AgentRun {
        run_id: "next-run".into(),
    };
    f.fence.controller_epoch = 2;
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: f.fence.environment_instance_id.clone(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: f.fence.controller_owner.id().to_owned(),
            },
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        },
    )
    .unwrap();
    let task = id(&Manager::handle(
        &f.manager,
        f.start(
            "after",
            EnvironmentTaskScope::Run {
                run_id: f.fence.controller_owner.id().to_owned(),
            },
            "true",
        ),
    )
    .unwrap());
    f.await_cleanup(&task);
    assert!(f.manager.lock().unwrap().live.contains_key(&background));
    assert_eq!(f.manager.lock().unwrap().journal.tasks.len(), 2);
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn same_run_reconstructed_controller_does_not_repeat_spawn_or_acknowledged_input() {
    let mut f = Fixture::new();
    let mut start = f.start(
        "cat",
        EnvironmentTaskScope::Run {
            run_id: f.fence.controller_owner.id().to_owned(),
        },
        "cat",
    );
    let task = id(&Manager::handle(&f.manager, start.clone()).unwrap());
    let mut input = EnvironmentTaskRequest::Input {
        fence: f.fence.clone(),
        task_id: task.clone(),
        operation_id: f.fence.operation_id("input"),
        data: b"once".to_vec(),
        close: false,
    };
    let acknowledged = Manager::handle(&f.manager, input.clone()).unwrap();
    f.fence.controller_epoch = 2;
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: f.fence.environment_instance_id.clone(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: f.fence.controller_owner.id().to_owned(),
            },
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        },
    )
    .unwrap();
    if let EnvironmentTaskRequest::Start {
        fence,
        operation_id,
        ..
    } = &mut start
    {
        *fence = f.fence.clone();
        *operation_id = f.fence.operation_id("cat");
    }
    assert_eq!(id(&Manager::handle(&f.manager, start).unwrap()), task);
    if let EnvironmentTaskRequest::Input {
        fence,
        operation_id,
        ..
    } = &mut input
    {
        *fence = f.fence.clone();
        *operation_id = f.fence.operation_id("input");
    }
    assert_eq!(Manager::handle(&f.manager, input).unwrap(), acknowledged);
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Input {
            fence: f.fence.clone(),
            task_id: task.clone(),
            operation_id: f.fence.operation_id("close"),
            data: vec![],
            close: true,
        },
    )
    .unwrap();
    f.await_cleanup(&task);
    let m = f.manager.lock().unwrap();
    assert_eq!(m.journal.tasks.len(), 1);
    assert_eq!(m.journal.tasks[&task].creator_epoch, 1);
    assert_eq!(
        m.journal.tasks[&task].creator_owner,
        f.fence.controller_owner
    );
    let (chunks, _, _, _, _) = m.logs[&task]
        .lock()
        .unwrap()
        .read(0, ENVIRONMENT_TASK_READ_BYTES)
        .unwrap();
    assert_eq!(
        chunks.into_iter().flat_map(|c| c.data).collect::<Vec<_>>(),
        b"once"
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn input_history_pressure_leaves_termination_and_end_run_reconcilable() {
    let f = Fixture::new();
    let mut start = f.start(
        "cat",
        EnvironmentTaskScope::Run {
            run_id: f.fence.controller_owner.id().to_owned(),
        },
        "cat",
    );
    // This case measures receipt capacity, not a command deadline. The enclosing
    // isolated gate has a hard budget and Terminate below owns the final cleanup.
    if let EnvironmentTaskRequest::Start { timeout_ms, .. } = &mut start {
        *timeout_ms = 0;
    }
    let task = id(&Manager::handle(&f.manager, start).unwrap());
    let first = EnvironmentTaskRequest::Input {
        fence: f.fence.clone(),
        task_id: task.clone(),
        operation_id: f.fence.operation_id("input-0"),
        data: b"x".to_vec(),
        close: false,
    };
    let receipt = Manager::handle(&f.manager, first.clone()).unwrap();
    for index in 1..1000 {
        Manager::handle(
            &f.manager,
            EnvironmentTaskRequest::Input {
                fence: f.fence.clone(),
                task_id: task.clone(),
                operation_id: f.fence.operation_id(&format!("input-{index}")),
                data: b"x".to_vec(),
                close: false,
            },
        )
        .unwrap();
    }
    assert_eq!(Manager::handle(&f.manager, first).unwrap(), receipt);
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence: f.fence.clone(),
            task_id: task.clone(),
            operation_id: f.fence.operation_id("stop"),
        },
    )
    .unwrap();
    f.await_cleanup(&task);
    let end = EnvironmentTaskRequest::EndRun {
        fence: f.fence.clone(),
        run_id: f.fence.controller_owner.id().to_owned(),
        operation_id: f.fence.operation_id("end"),
    };
    assert!(matches!(
        Manager::handle(&f.manager, end.clone()).unwrap(),
        EnvironmentTaskResult::RunEnded {
            cleanup_complete: true,
            ..
        }
    ));
    assert!(Manager::handle(&f.manager, end).is_ok());
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn task_deadline_exits_before_late_input_and_uncertain_input_is_not_replayed() {
    let f = Fixture::new();
    let mut start = f.start(
        "deadline-cat",
        EnvironmentTaskScope::Run {
            run_id: f.fence.controller_owner.id().to_owned(),
        },
        "exec cat",
    );
    if let EnvironmentTaskRequest::Start { timeout_ms, .. } = &mut start {
        *timeout_ms = 100;
    }
    let began = Instant::now();
    let task = id(&Manager::handle(&f.manager, start).unwrap());
    f.await_cleanup(&task);
    assert!(began.elapsed() >= Duration::from_millis(100));
    {
        let manager = f.manager.lock().unwrap();
        let snapshot = &manager.journal.tasks[&task].snapshot;
        assert_eq!(snapshot.state, EnvironmentTaskState::Exited);
        assert_eq!(
            snapshot.exit_code, None,
            "deadline kills the owned task tree"
        );
        assert!(snapshot.cleanup_complete);
        assert!(!manager.journal.poisoned);
    }
    let input = EnvironmentTaskRequest::Input {
        fence: f.fence.clone(),
        task_id: task.clone(),
        operation_id: f.fence.operation_id("late-input"),
        data: b"must-not-be-replayed".to_vec(),
        close: false,
    };
    assert_eq!(
        Manager::handle(&f.manager, input.clone()).unwrap_err(),
        "task_not_running"
    );
    assert_eq!(
        Manager::handle(&f.manager, input).unwrap_err(),
        "task_operation_unknown"
    );
    let result = Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Query {
            fence: f.fence.clone(),
            task_id: task,
            cursor: 0,
            max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
        },
    )
    .unwrap();
    assert!(
        matches!(result,EnvironmentTaskResult::Task{task,output,..} if task.state==EnvironmentTaskState::Exited && output.is_empty())
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn unavailable_worker_threads_reject_before_exec_without_poisoning_resident_tasks() {
    let f = Fixture::new();
    let resident = id(&Manager::handle(
        &f.manager,
        f.start(
            "resident",
            EnvironmentTaskScope::Environment,
            "exec sleep 30",
        ),
    )
    .unwrap());
    let marker = format!("/mnt/data/thread-capacity-{}", std::process::id());
    let attempt = f.start(
        "capacity",
        EnvironmentTaskScope::Environment,
        &format!("printf executed >> {marker}; exec sleep 30"),
    );
    let before = fs::read(
        f.manager
            .lock()
            .unwrap()
            .storage
            .control_root
            .join("history.sqlite"),
    )
    .unwrap();
    for failed_thread in 1..=3 {
        f.manager.lock().unwrap().fail_worker_reservation = Some(failed_thread);
        assert_eq!(
            Manager::handle(&f.manager, attempt.clone()).unwrap_err(),
            "task_thread_capacity"
        );
        assert!(
            !Path::new(&marker).exists(),
            "rejected start executed user code"
        );
        let mut manager = f.manager.lock().unwrap();
        assert!(!manager.journal.poisoned);
        assert_eq!(manager.journal.tasks.len(), 1);
        assert!(manager
            .live
            .get_mut(&resident)
            .unwrap()
            .child
            .try_wait()
            .unwrap()
            .is_none());
        assert_eq!(
            fs::read(manager.storage.control_root.join("history.sqlite")).unwrap(),
            before
        );
    }
    f.manager.lock().unwrap().fail_worker_reservation = None;
    let accepted = id(&Manager::handle(&f.manager, attempt.clone()).unwrap());
    assert_eq!(id(&Manager::handle(&f.manager, attempt).unwrap()), accepted);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !Path::new(&marker).exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read(&marker).unwrap(), b"executed");
    fs::remove_file(marker).unwrap();
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn panicking_supervisor_is_unknown_until_owned_tree_is_empty_and_child_reaped() {
    let f = Fixture::new();
    let resident = id(&Manager::handle(
        &f.manager,
        f.start(
            "resident",
            EnvironmentTaskScope::Environment,
            "exec sleep 30",
        ),
    )
    .unwrap());
    f.manager.lock().unwrap().fail_worker_reservation = Some(4);
    f.manager.lock().unwrap().defer_failed_child_reap = true;
    let attempt = f.start(
        "lost-supervisor",
        EnvironmentTaskScope::Environment,
        "exec sleep 30",
    );
    let failed = id(&Manager::handle(&f.manager, attempt.clone()).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let empty = fs::read_to_string(f.cgroups.join(&failed).join("cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 0");
        let manager = f.manager.lock().unwrap();
        if empty && manager.journal.tasks[&failed].snapshot.state == EnvironmentTaskState::Unknown {
            assert!(
                manager.live.contains_key(&failed),
                "unreaped child ownership was discarded"
            );
            drop(manager);
            let query = Manager::handle(
                &f.manager,
                EnvironmentTaskRequest::Query {
                    fence: f.fence.clone(),
                    task_id: failed.clone(),
                    cursor: 0,
                    max_bytes: 65536,
                },
            )
            .unwrap();
            assert!(
                matches!(query, EnvironmentTaskResult::Task { task, .. } if !task.cleanup_complete),
                "empty cgroup does not confirm the failed supervisor's direct child was reaped"
            );
            break;
        }
        drop(manager);
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    f.manager.lock().unwrap().defer_failed_child_reap = false;
    loop {
        let mut manager = f.manager.lock().unwrap();
        assert!(!manager.journal.poisoned);
        assert!(manager
            .live
            .get_mut(&resident)
            .unwrap()
            .child
            .try_wait()
            .unwrap()
            .is_none());
        let task = &manager.journal.tasks[&failed].snapshot;
        if task.cleanup_complete && !manager.live.contains_key(&failed) {
            assert_eq!(task.state, EnvironmentTaskState::Unknown);
            assert!(!task.output_complete);
            break;
        }
        drop(manager);
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    f.manager.lock().unwrap().fail_worker_reservation = None;
    let replay = Manager::handle(&f.manager, attempt).unwrap();
    assert!(matches!(replay, EnvironmentTaskResult::Task { task, .. }
        if task.task_id == failed && task.state == EnvironmentTaskState::Unknown && task.cleanup_complete));
    assert!(
        fs::read_to_string(f.cgroups.join(failed).join("cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 0")
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn worker_delivery_failure_retains_supervision_and_does_not_respawn_unknown_start() {
    let f = Fixture::new();
    let resident = id(&Manager::handle(
        &f.manager,
        f.start(
            "resident",
            EnvironmentTaskScope::Environment,
            "exec sleep 30",
        ),
    )
    .unwrap());
    f.manager.lock().unwrap().fail_worker_delivery = true;
    let attempt = f.start(
        "failed-delivery",
        EnvironmentTaskScope::Environment,
        "exec sleep 30",
    );
    assert_eq!(
        Manager::handle(&f.manager, attempt.clone()).unwrap_err(),
        "task_worker_delivery_failed"
    );
    let failed = f
        .manager
        .lock()
        .unwrap()
        .journal
        .tasks
        .iter()
        .find(|(_, record)| {
            matches!(&attempt, EnvironmentTaskRequest::Start { operation_id, .. }
            if record.snapshot.operation_id == *operation_id)
        })
        .map(|(id, _)| id.clone())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut manager = f.manager.lock().unwrap();
        assert!(!manager.journal.poisoned);
        assert!(manager
            .live
            .get_mut(&resident)
            .unwrap()
            .child
            .try_wait()
            .unwrap()
            .is_none());
        let snapshot = &manager.journal.tasks[&failed].snapshot;
        if snapshot.cleanup_complete && !manager.live.contains_key(&failed) {
            assert_eq!(snapshot.state, EnvironmentTaskState::Unknown);
            assert!(!snapshot.output_complete);
            break;
        }
        drop(manager);
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    f.manager.lock().unwrap().fail_worker_delivery = false;
    assert!(
        matches!(Manager::handle(&f.manager, attempt).unwrap(), EnvironmentTaskResult::Task { task, .. }
        if task.task_id == failed && task.state == EnvironmentTaskState::Unknown && task.cleanup_complete)
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn exec_failure_after_worker_reservation_remains_queryable_without_deadlock_or_retry_spawn() {
    let f = Fixture::new();
    let mut request = f.start(
        "missing-program",
        EnvironmentTaskScope::Environment,
        "unused",
    );
    if let EnvironmentTaskRequest::Start { program, .. } = &mut request {
        *program = "/centaeris-nonexistent-task-program".into();
    }
    let result = Manager::handle(&f.manager, request.clone()).unwrap();
    let failed = id(&result);
    assert!(matches!(result, EnvironmentTaskResult::Task { task, .. }
        if task.state == EnvironmentTaskState::Unknown && task.cleanup_complete));
    assert_eq!(id(&Manager::handle(&f.manager, request).unwrap()), failed);
    let resident = id(&Manager::handle(
        &f.manager,
        f.start(
            "after-failure",
            EnvironmentTaskScope::Environment,
            "exec sleep 30",
        ),
    )
    .unwrap());
    let mut manager = f.manager.lock().unwrap();
    assert!(!manager.journal.poisoned);
    assert!(!manager.live.contains_key(&failed));
    assert!(manager
        .live
        .get_mut(&resident)
        .unwrap()
        .child
        .try_wait()
        .unwrap()
        .is_none());
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn real_background_tree_survives_controller_takeover_and_drains_both_streams() {
    let f = Fixture::new();
    let request = f.start("start", EnvironmentTaskScope::Environment, "(head -c 2097152 /dev/zero) & (head -c 2097152 /dev/zero >&2) & wait; printf stdout-done; printf stderr-done >&2; sleep 30 & wait");
    let task_id = id(&Manager::handle(&f.manager, request.clone()).unwrap());
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let m = f.manager.lock().unwrap();
        let mut log = m.logs[&task_id].lock().unwrap();
        let (stdout_done, stderr_done, gap) = retained_drain_markers(&mut log);
        if stdout_done && stderr_done && gap {
            break;
        }
        drop(log);
        drop(m);
        assert!(
            Instant::now() < end,
            "both streams must drain through their completion markers and expose the overwritten prefix"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let mut updated = request;
    if let EnvironmentTaskRequest::Start { fence, .. } = &mut updated {
        fence.controller_epoch = 2;
    }
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: f.fence.environment_instance_id.clone(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: f.fence.controller_owner.id().to_owned(),
            },
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        },
    )
    .unwrap();
    assert_eq!(
        Manager::handle(&f.manager, updated).unwrap_err(),
        "task_operation_replay_expired"
    );
    assert!(Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence: f.fence.clone(),
            task_id: task_id.clone(),
            operation_id: f.fence.operation_id("stale")
        }
    )
    .is_err());
    let fence = EnvironmentTaskFence {
        controller_epoch: 2,
        ..f.fence.clone()
    };
    let response = Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Query {
            fence: fence.clone(),
            task_id: task_id.clone(),
            cursor: 0,
            max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
        },
    )
    .unwrap();
    assert!(matches!(
        response,
        EnvironmentTaskResult::Task { gap: true, .. }
    ));
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence,
            task_id: task_id.clone(),
            operation_id: "2:stop".into(),
        },
    )
    .unwrap();
    f.await_cleanup(&task_id);
}

fn retained_drain_markers(log: &mut OutputLog) -> (bool, bool, bool) {
    let mut cursor = 0;
    let mut gap = false;
    let mut seen = [false, false];
    let mut tails = [Vec::new(), Vec::new()];
    let markers = [b"stdout-done".as_slice(), b"stderr-done".as_slice()];
    loop {
        let (chunks, _, next, page_gap, more) =
            log.read(cursor, ENVIRONMENT_TASK_READ_BYTES).unwrap();
        gap |= page_gap;
        for chunk in chunks {
            let index = if chunk.stream == EnvironmentTaskStream::Stdout {
                0
            } else {
                1
            };
            let tail = &mut tails[index];
            tail.extend_from_slice(&chunk.data);
            seen[index] |= tail
                .windows(markers[index].len())
                .any(|bytes| bytes == markers[index]);
            let retain = markers[index].len() - 1;
            if tail.len() > retain {
                tail.drain(..tail.len() - retain);
            }
        }
        if !more {
            return (seen[0], seen[1], gap);
        }
        assert!(next > cursor, "bounded output observation must advance");
        cursor = next;
    }
}

#[test]
fn drain_marker_observation_survives_split_stderr_and_later_stdout_chunks() {
    let root = lock_root();
    let mut log = OutputLog::open(&root.join("output"), 8).unwrap();
    for _ in 0..10 {
        log.append(EnvironmentTaskStream::Stdout, &[0; CHUNK])
            .unwrap();
    }
    log.append(EnvironmentTaskStream::Stderr, b"stderr-")
        .unwrap();
    log.append(EnvironmentTaskStream::Stdout, b"stdout-")
        .unwrap();
    log.append(EnvironmentTaskStream::Stderr, b"done").unwrap();
    log.append(EnvironmentTaskStream::Stdout, b"done").unwrap();
    log.append(EnvironmentTaskStream::Stdout, &[0; CHUNK])
        .unwrap();
    log.append(EnvironmentTaskStream::Stdout, &[0; CHUNK])
        .unwrap();
    let (last_two, _, _, _, _) = log.read(log.next - 2, ENVIRONMENT_TASK_READ_BYTES).unwrap();
    assert!(
        last_two
            .iter()
            .all(|chunk| chunk.data.iter().all(|byte| *byte == 0)),
        "later stdout can completely hide completion markers from the old two-chunk window"
    );
    assert_eq!(retained_drain_markers(&mut log), (true, true, true));
    drop(log);
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn successful_input_receipt_replays_after_child_exit_without_writing_twice() {
    let f = Fixture::new();
    let task_id = id(&Manager::handle(
        &f.manager,
        f.start("cat", EnvironmentTaskScope::Environment, "cat"),
    )
    .unwrap());
    let input = EnvironmentTaskRequest::Input {
        fence: f.fence.clone(),
        task_id: task_id.clone(),
        operation_id: f.fence.operation_id("input"),
        data: b"one-input\n".to_vec(),
        close: true,
    };
    let receipt = Manager::handle(&f.manager, input.clone()).unwrap();
    f.await_cleanup(&task_id);
    assert_eq!(Manager::handle(&f.manager, input).unwrap(), receipt);
    let m = f.manager.lock().unwrap();
    let (chunks, _, _, _, _) = m.logs[&task_id]
        .lock()
        .unwrap()
        .read(0, ENVIRONMENT_TASK_READ_BYTES)
        .unwrap();
    assert_eq!(
        chunks.into_iter().flat_map(|c| c.data).collect::<Vec<_>>(),
        b"one-input\n"
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn lost_data_ack_queries_original_receipt_then_closes_once() {
    let f = Fixture::new();
    let task_id = id(&Manager::handle(
        &f.manager,
        f.start("receipt-cat", EnvironmentTaskScope::Environment, "cat"),
    )
    .unwrap());
    let data = b"one-input\n".to_vec();
    let input_operation = f.fence.operation_id("input");
    // Deliberately discard the transport acknowledgement after durable success.
    let receipt = Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Input {
            fence: f.fence.clone(),
            task_id: task_id.clone(),
            operation_id: input_operation.clone(),
            data: data.clone(),
            close: false,
        },
    )
    .unwrap();
    let lookup = EnvironmentTaskRequest::QueryInputReceipt {
        fence: f.fence.clone(),
        task_id: task_id.clone(),
        operation_id: input_operation,
        request_digest: environment_task_input_digest(&data, false).unwrap(),
    };
    assert_eq!(
        Manager::handle(&f.manager, lookup.clone()).unwrap(),
        receipt
    );
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Input {
            fence: f.fence.clone(),
            task_id: task_id.clone(),
            operation_id: f.fence.operation_id("close"),
            data: Vec::new(),
            close: true,
        },
    )
    .unwrap();
    f.await_cleanup(&task_id);
    assert_eq!(Manager::handle(&f.manager, lookup).unwrap(), receipt);
    let m = f.manager.lock().unwrap();
    let (chunks, _, _, _, _) = m.logs[&task_id]
        .lock()
        .unwrap()
        .read(0, ENVIRONMENT_TASK_READ_BYTES)
        .unwrap();
    assert_eq!(
        chunks.into_iter().flat_map(|c| c.data).collect::<Vec<_>>(),
        data
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn running_journal_failure_keeps_supervision_and_drain_ownership() {
    let f = Fixture::new();
    f.manager.lock().unwrap().fail_running_commit = true;
    let error = Manager::handle(
        &f.manager,
        f.start(
            "fail",
            EnvironmentTaskScope::Environment,
            "head -c 2097152 /dev/zero; sleep 30 & wait",
        ),
    )
    .unwrap_err();
    assert_ne!(error, "task_journal_capacity");
    assert_ne!(error, "task_history_capacity");
    let task_id = f
        .manager
        .lock()
        .unwrap()
        .journal
        .tasks
        .keys()
        .next()
        .unwrap()
        .clone();
    f.await_cleanup(&task_id);
    assert!(f.manager.lock().unwrap().journal.poisoned);
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn lost_input_receipt_never_rewrites_stdin() {
    let f = Fixture::new();
    let task_id = id(&Manager::handle(
        &f.manager,
        f.start("cat", EnvironmentTaskScope::Environment, "cat"),
    )
    .unwrap());
    f.manager.lock().unwrap().fail_mutation_commit = true;
    let input = EnvironmentTaskRequest::Input {
        fence: f.fence.clone(),
        task_id: task_id.clone(),
        operation_id: f.fence.operation_id("input"),
        data: b"one-input\n".to_vec(),
        close: false,
    };
    let error = Manager::handle(&f.manager, input.clone()).unwrap_err();
    assert_ne!(error, "task_journal_capacity");
    assert_ne!(error, "task_input_receipt_capacity");
    assert!(Manager::handle(&f.manager, input).is_err());
    f.await_cleanup(&task_id);
    let m = f.manager.lock().unwrap();
    let (chunks, _, _, _, _) = m.logs[&task_id]
        .lock()
        .unwrap()
        .read(0, ENVIRONMENT_TASK_READ_BYTES)
        .unwrap();
    assert!(
        chunks
            .into_iter()
            .flat_map(|c| c.data)
            .collect::<Vec<_>>()
            .len()
            <= b"one-input\n".len()
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn cross_run_takeover_cleans_only_old_run_tasks_and_is_retryable() {
    let f = Fixture::new();
    let run_id = id(&Manager::handle(
        &f.manager,
        f.start(
            "run",
            EnvironmentTaskScope::Run {
                run_id: f.fence.controller_owner.id().to_owned(),
            },
            "sleep 30 & wait",
        ),
    )
    .unwrap());
    let environment_id = id(&Manager::handle(
        &f.manager,
        f.start(
            "environment",
            EnvironmentTaskScope::Environment,
            "sleep 30 & wait",
        ),
    )
    .unwrap());
    let takeover = EnvironmentTaskRequest::Takeover {
        environment_instance_id: f.fence.environment_instance_id.clone(),
        controller_owner: EnvironmentTaskOwner::AgentRun {
            run_id: "run-two".into(),
        },
        controller_epoch: 2,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if Manager::handle(&f.manager, takeover.clone()).is_ok() {
            break;
        }
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(20));
    }
    f.await_cleanup(&run_id);
    assert!(f.manager.lock().unwrap().live.contains_key(&environment_id));
    let fence = EnvironmentTaskFence {
        controller_owner: EnvironmentTaskOwner::AgentRun {
            run_id: "run-two".into(),
        },
        controller_epoch: 2,
        ..f.fence.clone()
    };
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence,
            task_id: environment_id.clone(),
            operation_id: "2:stop".into(),
        },
    )
    .unwrap();
    f.await_cleanup(&environment_id);
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn terminal_cleanup_controller_cannot_resume_execution_or_kill_environment_scope() {
    let f = Fixture::new();
    let run_id = id(&Manager::handle(
        &f.manager,
        f.start(
            "run",
            EnvironmentTaskScope::Run {
                run_id: f.fence.controller_owner.id().to_owned(),
            },
            "sleep 30 & wait",
        ),
    )
    .unwrap());
    let environment_id = id(&Manager::handle(
        &f.manager,
        f.start(
            "environment",
            EnvironmentTaskScope::Environment,
            "sleep 30 & wait",
        ),
    )
    .unwrap());
    let fence = EnvironmentTaskFence {
        controller_epoch: 2,
        controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
        ..f.fence.clone()
    };
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: fence.environment_instance_id.clone(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: fence.controller_owner.id().to_owned(),
            },
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
        },
    )
    .unwrap();
    let mut start = f.start("denied", EnvironmentTaskScope::Environment, "sleep 30");
    if let EnvironmentTaskRequest::Start { fence: target, .. } = &mut start {
        *target = fence.clone();
    }
    assert!(Manager::handle(&f.manager, start).is_err());
    assert!(Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Input {
            fence: fence.clone(),
            task_id: environment_id.clone(),
            operation_id: f.fence.operation_id("denied"),
            data: vec![],
            close: true
        }
    )
    .is_err());
    assert!(Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence: fence.clone(),
            task_id: environment_id.clone(),
            operation_id: f.fence.operation_id("denied")
        }
    )
    .is_err());
    assert!(f.manager.lock().unwrap().live.contains_key(&environment_id));
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::EndRun {
            fence,
            run_id: f.fence.controller_owner.id().to_owned(),
            operation_id: "2:end".into(),
        },
    )
    .unwrap();
    f.await_cleanup(&run_id);
    assert!(f.manager.lock().unwrap().live.contains_key(&environment_id));
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn output_capacity_failure_preserves_existing_process_control_and_allows_cleanup() {
    let f = Fixture::new();
    let task_id = id(&Manager::handle(
        &f.manager,
        f.start(
            "existing",
            EnvironmentTaskScope::Environment,
            "sleep 30 & wait",
        ),
    )
    .unwrap());
    let control = f.manager.lock().unwrap().storage.control_root.clone();
    let before = fs::read(control.join("history.sqlite")).unwrap();
    {
        let mut manager = f.manager.lock().unwrap();
        let unit = fs::metadata(&manager.storage.output_root)
            .unwrap()
            .blksize()
            .max(1);
        manager.storage.output_budget_bytes = ((ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK
            * (HEADER + CHUNK)) as u64)
            .next_multiple_of(unit);
    }
    assert_eq!(
        Manager::handle(
            &f.manager,
            f.start("capacity", EnvironmentTaskScope::Environment, "sleep 30")
        )
        .unwrap_err(),
        "task_output_capacity"
    );
    assert_eq!(fs::read(control.join("history.sqlite")).unwrap(), before);
    assert_eq!(f.manager.lock().unwrap().live.len(), 1);
    assert_eq!(
        fs::read_dir(&f.cgroups)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .count(),
        1
    );
    let fence = EnvironmentTaskFence {
        controller_epoch: 2,
        ..f.fence.clone()
    };
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: fence.environment_instance_id.clone(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: fence.controller_owner.id().to_owned(),
            },
            controller_epoch: fence.controller_epoch,
            controller_purpose: fence.controller_purpose.clone(),
        },
    )
    .unwrap();
    let result = Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Query {
            fence: fence.clone(),
            task_id: task_id.clone(),
            cursor: 0,
            max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
        },
    )
    .unwrap();
    assert!(
        matches!(result, EnvironmentTaskResult::Task { task, .. } if task.state == EnvironmentTaskState::Running)
    );
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence,
            task_id: task_id.clone(),
            operation_id: "2:cleanup".into(),
        },
    )
    .unwrap();
    f.await_cleanup(&task_id);
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn kill_only_domain_cleans_setsid_descendants_without_per_task_controllers() {
    let f = Fixture::with_controllers(false);
    assert_eq!(
        fs::read_to_string(f.cgroups.join("cgroup.type"))
            .unwrap()
            .trim(),
        "domain"
    );
    assert!(fs::read_to_string(f.cgroups.join("cgroup.subtree_control"))
        .unwrap()
        .trim()
        .is_empty());
    let task_id = id(&Manager::handle(
        &f.manager,
        f.start(
            "kill-only",
            EnvironmentTaskScope::Environment,
            "setsid /bin/sh -c 'sleep 30 & wait' & sleep 30 & wait",
        ),
    )
    .expect("kill-only domain must accept task without per-task controllers"));
    let task_group = f.cgroups.join(&task_id);
    assert!(!task_group.join("pids.max").exists());
    assert!(!task_group.join("memory.max").exists());
    assert_eq!(
        fs::read_to_string(task_group.join("cgroup.type"))
            .unwrap()
            .trim(),
        "domain"
    );
    let parent_sid = unsafe { libc::getsid(0) };
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let members: Vec<i32> = fs::read_to_string(task_group.join("cgroup.procs"))
            .unwrap()
            .lines()
            .filter_map(|pid| pid.parse().ok())
            .collect();
        let leader = f.manager.lock().unwrap().live[&task_id].child.id() as i32;
        assert!(
            members.contains(&leader),
            "actual direct child must belong to task cgroup"
        );
        if members.len() >= 4
            && members
                .iter()
                .any(|pid| unsafe { libc::getsid(*pid) } != parent_sid)
        {
            break;
        }
        assert!(
            Instant::now() < end,
            "setsid descendant did not appear in task cgroup"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence: f.fence.clone(),
            task_id: task_id.clone(),
            operation_id: f.fence.operation_id("stop"),
        },
    )
    .unwrap();
    f.await_cleanup(&task_id);
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree and python3"]
fn double_fork_setsid_cleanup_preserves_another_task() {
    let f = Fixture::new();
    let marker = f.root.join("orphan-pid");
    fs::write(&marker, b"").unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o666)).unwrap();
    let other = id(&Manager::handle(
        &f.manager,
        f.start("unrelated", EnvironmentTaskScope::Environment, "cat"),
    )
    .unwrap());
    let script = format!(
        r#"python3 - <<'PY'
import os, time
if os.fork() == 0:
    os.setsid()
    if os.fork() == 0:
        with open({marker}, 'w') as output:
            output.write(str(os.getpid()))
        time.sleep(30)
    os._exit(0)
PY
sleep 30
"#,
        marker = serde_json::to_string(marker.to_str().unwrap()).unwrap()
    );
    let escaped = id(&Manager::handle(
        &f.manager,
        f.start("escaped", EnvironmentTaskScope::Environment, &script),
    )
    .unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let members: Vec<i32> = fs::read_to_string(f.cgroups.join(&escaped).join("cgroup.procs"))
            .unwrap()
            .lines()
            .filter_map(|pid| pid.parse().ok())
            .collect();
        let direct = f.manager.lock().unwrap().live[&escaped].child.id() as i32;
        if let Ok(pid) = fs::read_to_string(&marker).unwrap().parse::<i32>() {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
            let parent = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u32>()
                .unwrap();
            if members.contains(&pid)
                && pid != direct
                && unsafe { libc::getsid(pid) } != unsafe { libc::getsid(direct) }
                && parent == std::process::id()
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "double-fork descendant must remain in the owned task domain"
        );
        thread::sleep(Duration::from_millis(10));
    }
    Manager::handle(
        &f.manager,
        EnvironmentTaskRequest::Terminate {
            fence: f.fence.clone(),
            task_id: escaped.clone(),
            operation_id: f.fence.operation_id("stop-escaped"),
        },
    )
    .unwrap();
    f.await_cleanup(&escaped);
    assert!(f.manager.lock().unwrap().live.contains_key(&other));
    assert!(
        fs::read_to_string(f.cgroups.join(&other).join("cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 1")
    );
}
