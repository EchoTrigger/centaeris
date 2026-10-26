//! Pause must share the existing per-environment helper admission boundary.
use super::*;
use std::sync::mpsc;

fn native_view(f: &Fixture) -> NativeManager {
    NativeManager {
        manager: f.manager.clone(),
        control: f.root.join("control"),
        output: f.root.join("output"),
        helper_root: f.root.join("output"),
        helper_binary: None,
        workspace_root: "/mnt/data".into(),
        helper_admission: Mutex::new(()),
        reaper: Mutex::new(None),
    }
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn ordinary_start_waits_for_the_existing_environment_admission_boundary() {
    let f = Fixture::new();
    let native = Arc::new(native_view(&f));
    let request = f.start(
        "guarded-start",
        EnvironmentTaskScope::Environment,
        "printf started-once",
    );
    let guard = native.helper_admission.lock().unwrap();
    let (ready_send, ready_receive) = mpsc::channel();
    let (done_send, done_receive) = mpsc::channel();
    let worker = {
        let native = native.clone();
        thread::spawn(move || {
            ready_send.send(()).unwrap();
            done_send.send(native.handle(request)).unwrap();
        })
    };
    ready_receive.recv_timeout(Duration::from_secs(5)).unwrap();
    let before_release = done_receive.recv_timeout(Duration::from_millis(250));
    let bypassed = before_release.is_ok();
    drop(guard);
    let result = before_release
        .unwrap_or_else(|_| done_receive.recv_timeout(Duration::from_secs(5)).unwrap());
    worker.join().unwrap();
    f.await_cleanup(&id(&result.unwrap()));
    assert!(
        !bypassed,
        "ordinary Start bypassed the in-progress environment admission/idle-operation fence"
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn closed_run_does_not_make_a_live_environment_task_idle() {
    let f = Fixture::new();
    let native = native_view(&f);
    let mut start = f.start("persistent", EnvironmentTaskScope::Environment, "cat");
    if let EnvironmentTaskRequest::Start { timeout_ms, .. } = &mut start {
        *timeout_ms = 0;
    }
    let task = id(&native.handle(start).unwrap());
    native
        .handle(EnvironmentTaskRequest::EndRun {
            fence: f.fence.clone(),
            run_id: f.fence.controller_owner.id().into(),
            operation_id: f.fence.operation_id("close-run"),
        })
        .unwrap();
    let called = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(
        native
            .with_idle_environment_operation(|| {
                called.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap(),
        IdleEnvironmentOperation::NotIdle
    );
    assert!(!called.load(Ordering::SeqCst));
    native
        .handle(EnvironmentTaskRequest::Terminate {
            fence: f.fence.clone(),
            task_id: task.clone(),
            operation_id: f.fence.operation_id("stop-persistent"),
        })
        .unwrap();
    f.await_cleanup(&task);
    assert_eq!(
        native.with_idle_environment_operation(|| Ok(7)).unwrap(),
        IdleEnvironmentOperation::Completed(7)
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn unconfirmed_cleanup_and_pending_receipts_refuse_idle_operation() {
    let f = Fixture::new();
    let native = native_view(&f);
    let task = id(&native
        .handle(f.start("finished", EnvironmentTaskScope::Environment, "true"))
        .unwrap());
    f.await_cleanup(&task);
    {
        let mut manager = native.manager.lock().unwrap();
        let task = &mut manager.journal.tasks.get_mut(&task).unwrap().snapshot;
        task.state = EnvironmentTaskState::Unknown;
        task.cleanup_complete = false;
    }
    assert_eq!(
        native.with_idle_environment_operation(|| Ok(())).unwrap(),
        IdleEnvironmentOperation::Unknown
    );
    {
        let mut manager = native.manager.lock().unwrap();
        manager
            .journal
            .tasks
            .get_mut(&task)
            .unwrap()
            .snapshot
            .cleanup_complete = true;
        manager.journal.receipts.insert(
            "input:unconfirmed".into(),
            Receipt {
                digest: "synthetic-uncertain-input".into(),
                result: None,
            },
        );
    }
    assert_eq!(
        native.with_idle_environment_operation(|| Ok(())).unwrap(),
        IdleEnvironmentOperation::Unknown
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn a_reserved_helper_input_drain_keeps_the_environment_non_idle() {
    let f = Fixture::new();
    let native = native_view(&f);
    let counter = native.manager.lock().unwrap().drain_workers.clone();
    let lease = HelperInputLease {
        _input_guard: None,
        _worker: DrainLease::new(&counter),
    };
    let worker = ReservedInput::new(
        File::create(f.root.join("reserved-helper-input")).unwrap(),
        Some(Arc::new(lease)),
    )
    .unwrap();
    assert_eq!(
        native.with_idle_environment_operation(|| Ok(())).unwrap(),
        IdleEnvironmentOperation::NotIdle
    );
    drop(worker);
    assert_eq!(
        native.with_idle_environment_operation(|| Ok(())).unwrap(),
        IdleEnvironmentOperation::Completed(())
    );
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn idle_callback_atomically_fences_start_and_helper_but_not_controls_or_other_instances() {
    use crate::host_supervisor::{HostHelperRequest, TrustedHelperKind};
    let f = Fixture::new();
    let other = Fixture::new();
    let native = Arc::new(native_view(&f));
    let other_native = native_view(&other);
    let completed = id(&native
        .handle(f.start(
            "completed-for-query",
            EnvironmentTaskScope::Environment,
            "true",
        ))
        .unwrap());
    f.await_cleanup(&completed);
    let (entered_send, entered_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let pause = {
        let native = native.clone();
        thread::spawn(move || {
            native.with_idle_environment_operation(|| {
                entered_send.send(()).unwrap();
                release_receive
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|_| "fixture_idle_operation_timeout".to_string())?;
                Ok(())
            })
        })
    };
    entered_receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        native.with_idle_environment_operation(|| Ok(())).unwrap(),
        IdleEnvironmentOperation::NotIdle
    );
    let (start_send, start_receive) = mpsc::channel();
    let start_request = f.start(
        "after-idle-callback",
        EnvironmentTaskScope::Environment,
        "printf started",
    );
    let start = {
        let native = native.clone();
        thread::spawn(move || start_send.send(native.handle(start_request)).unwrap())
    };
    let input = File::create(f.root.join("helper-empty-input")).unwrap();
    let request = HostHelperRequest {
        fence: f.fence.clone(),
        operation_id: f.fence.operation_id("helper-after-idle"),
        scope: EnvironmentTaskScope::Environment,
        kind: TrustedHelperKind::PrepareWorkspace,
        input_digest: format!("sha256:{:x}", Sha256::digest([])),
        input_length: 0,
    };
    let (helper_send, helper_receive) = mpsc::channel();
    let helper = {
        let native = native.clone();
        thread::spawn(move || {
            helper_send
                .send(native.start_helper(&request, input, None))
                .unwrap()
        })
    };
    let early_start = start_receive.recv_timeout(Duration::from_millis(250));
    let early_helper = helper_receive.recv_timeout(Duration::from_millis(250));
    // Manager control remains available while only execution admission is fenced.
    let queried = native.handle(EnvironmentTaskRequest::Query {
        fence: f.fence.clone(),
        task_id: completed.clone(),
        cursor: 0,
        max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
    });
    let terminated = native.handle(EnvironmentTaskRequest::Terminate {
        fence: f.fence.clone(),
        task_id: completed,
        operation_id: f.fence.operation_id("terminate-completed"),
    });
    let other_task = id(&other_native
        .handle(other.start(
            "independent",
            EnvironmentTaskScope::Environment,
            "printf other-instance",
        ))
        .unwrap());
    other.await_cleanup(&other_task);
    release_send.send(()).unwrap();
    let pause_result = pause.join().unwrap().unwrap();
    let bypassed_start = early_start.is_ok();
    let bypassed_helper = early_helper.is_ok();
    let started = early_start
        .unwrap_or_else(|_| start_receive.recv_timeout(Duration::from_secs(5)).unwrap())
        .unwrap();
    let helper_result = early_helper
        .unwrap_or_else(|_| helper_receive.recv_timeout(Duration::from_secs(5)).unwrap());
    start.join().unwrap();
    helper.join().unwrap();
    f.await_cleanup(&id(&started));
    assert_eq!(pause_result, IdleEnvironmentOperation::Completed(()));
    assert!(
        !bypassed_start && !bypassed_helper,
        "no new execution may pass between idle check and physical operation"
    );
    assert!(
        queried.is_ok() && terminated.is_ok(),
        "query/termination must retain their control channel"
    );
    assert_eq!(helper_result.unwrap_err(),"host_helper_not_configured",
        "the real helper entry must reach its existing configuration check only after the idle operation");
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn physical_idle_operation_failure_is_not_a_completed_or_persisted_pause() {
    let f = Fixture::new();
    let native = native_view(&f);
    let before = native.inspection().unwrap();
    assert_eq!(
        native
            .with_idle_environment_operation::<()>(|| Err("physical_outcome_unknown".into()))
            .unwrap_err(),
        "physical_outcome_unknown"
    );
    assert_eq!(
        native.inspection().unwrap(),
        before,
        "the admission fence cannot invent a power or task transition"
    );
}
