//! Real local-launch coverage of NativeManager; OCI attestation is a separate gate.
use super::*;

#[derive(Default)]
struct CapacityObservation {
    peaks: BTreeMap<String, u64>,
    rejections: BTreeMap<String, u64>,
}

impl CapacityObservation {
    fn sample(&mut self, native: &NativeManager) {
        let state = native.inspection().expect("inspect actual task index");
        for key in [
            "activeTasks",
            "hotTasks",
            "indexedTasks",
            "indexedOwnerTasks",
            "controlDirectoryBytes",
            "outputDirectoryBytes",
        ] {
            self.peak(
                key,
                state[key].as_u64().expect("numeric resource observation"),
            );
        }
        let manager = native.manager.lock().unwrap();
        self.peak("liveSupervisors", manager.live.len() as u64);
        self.peak("outputLogs", manager.logs.len() as u64);
        self.peak(
            "journalJsonBytes",
            serde_json::to_vec(&manager.journal).unwrap().len() as u64,
        );
        self.peak(
            "sqliteBytes",
            fs::metadata(native.control.join("history.sqlite"))
                .unwrap()
                .len(),
        );
        self.peak(
            "indexLiveBytes",
            store::live_bytes(&native.control).unwrap(),
        );
        self.peak(
            "osThreads",
            fs::read_dir("/proc/self/task").unwrap().count() as u64,
        );
    }

    fn peak(&mut self, name: &str, value: u64) {
        let peak = self.peaks.entry(name.into()).or_default();
        *peak = (*peak).max(value);
    }

    fn request(
        &mut self,
        native: &NativeManager,
        request: EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String> {
        let result = native.handle(request);
        if let Err(reason) = &result {
            if environment_task_capacity_rejection(reason) {
                *self.rejections.entry(reason.clone()).or_default() += 1;
            }
        }
        self.sample(native);
        if let Err(reason) = &result {
            if !environment_task_capacity_rejection(reason) {
                self.report("unexpected-error", Some(reason));
            }
        }
        result
    }

    fn report(&self, status: &str, reason: Option<&str>) {
        eprintln!(
            "TASK_CAPACITY_OBSERVATION {}",
            serde_json::json!({
                "status":status,"shortCommands":256,"sameOwner":true,
                "highWater":self.peaks,"capacityRejections":self.rejections,"failureReason":reason,
            })
        );
    }
}

fn view(fixture: &Fixture) -> NativeManager {
    NativeManager {
        manager: fixture.manager.clone(),
        control: fixture.root.join("control"),
        output: fixture.root.join("output"),
        helper_root: fixture.root.join("output"),
        helper_binary: None,
        workspace_root: "/mnt/data".into(),
        helper_admission: Mutex::new(()),
        reaper: Mutex::new(None),
    }
}

fn writable_witness(f: &Fixture, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    // Local launch deliberately uses the same unprivileged UID as native execution.
    // Permit traversal only; control/output directories retain their private modes.
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o711)).unwrap();
    let path = f.root.join(name);
    fs::write(&path, []).unwrap();
    std::os::unix::fs::chown(&path, Some(10001), Some(10001)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

fn query(native: &NativeManager, f: &Fixture, task_id: &str) -> EnvironmentTaskResult {
    native
        .handle(EnvironmentTaskRequest::Query {
            fence: f.fence.clone(),
            task_id: task_id.into(),
            cursor: 0,
            max_bytes: ENVIRONMENT_TASK_READ_BYTES as u32,
        })
        .expect("query remains available independently of ordinary admission")
}

fn long_start(f: &Fixture, operation: &str, scope: EnvironmentTaskScope) -> EnvironmentTaskRequest {
    let mut request = f.start(operation, scope, "cat");
    if let EnvironmentTaskRequest::Start { timeout_ms, .. } = &mut request {
        // The owned systemd fixture bounds lifetime; explicit controls below own cleanup.
        *timeout_ms = 0;
    }
    request
}

#[test]
#[ignore = "requires explicitly delegated disposable Linux cgroup v2 subtree"]
fn native_short_command_history_and_live_capacity_preserve_replay_and_controls() {
    let f = Fixture::new();
    let native = view(&f);
    let mut observation = CapacityObservation::default();
    let scope = EnvironmentTaskScope::Run {
        run_id: f.fence.controller_owner.id().into(),
    };
    let background = id(&observation
        .request(
            &native,
            long_start(
                &f,
                "environment-background",
                EnvironmentTaskScope::Environment,
            ),
        )
        .unwrap());
    let run_request = long_start(&f, "run-background", scope.clone());
    let run_background = id(&observation.request(&native, run_request.clone()).unwrap());
    let witness = writable_witness(&f, "short-start-witness");
    let mut first = None;
    for index in 0..256 {
        let script = if index == 0 {
            "set -eu; printf x >> \"$CAPACITY_WITNESS\"; printf short-stdout; printf short-stderr >&2"
        } else {
            "printf short-stdout; printf short-stderr >&2"
        };
        let mut request = f.start(&format!("short-{index}"), scope.clone(), script);
        if let EnvironmentTaskRequest::Start { env, .. } = &mut request {
            env.insert(
                "CAPACITY_WITNESS".into(),
                witness.to_string_lossy().into_owned(),
            );
        }
        let result = observation
            .request(&native, request.clone())
            .unwrap_or_else(|reason| {
                observation.report("ordinary-short-command-rejected", Some(&reason));
                panic!("short command {index} rejected: {reason}");
            });
        let task = id(&result);
        f.await_cleanup(&task);
        let result = query(&native, &f, &task);
        assert!(matches!(result,EnvironmentTaskResult::Task { task, .. }
            if task.state==EnvironmentTaskState::Exited && task.exit_code==Some(0)
                && task.output_complete && task.cleanup_complete));
        if index == 0 {
            first = Some((request, task));
        }
        observation.sample(&native);
    }
    assert!(
        native.inspection().unwrap()["indexedTasks"]
            .as_u64()
            .unwrap()
            > ENVIRONMENT_TASK_MAX_RECORDS as u64
    );
    let (first_request, first_id) = first.unwrap();
    assert_eq!(
        id(&observation.request(&native, first_request).unwrap()),
        first_id
    );
    assert_eq!(
        fs::read(&witness).unwrap(),
        b"x",
        "cold same-ID replay must never spawn again"
    );
    let EnvironmentTaskResult::Task { output, .. } = query(&native, &f, &first_id) else {
        panic!("task output response");
    };
    for (stream, expected) in [
        (EnvironmentTaskStream::Stdout, b"short-stdout".as_slice()),
        (EnvironmentTaskStream::Stderr, b"short-stderr".as_slice()),
    ] {
        let bytes: Vec<u8> = output
            .iter()
            .filter(|part| part.stream == stream)
            .flat_map(|part| part.data.iter().copied())
            .collect();
        assert_eq!(bytes, expected, "cold task output remains recoverable");
    }

    let mut busy = Vec::new();
    for index in 2..ENVIRONMENT_TASK_MAX_ACTIVE {
        busy.push(id(&observation
            .request(
                &native,
                long_start(&f, &format!("busy-{index}"), scope.clone()),
            )
            .unwrap()));
    }
    assert_eq!(
        native.inspection().unwrap()["activeTasks"],
        ENVIRONMENT_TASK_MAX_ACTIVE
    );
    let rejected_witness = writable_witness(&f, "rejected-start-witness");
    let mut retry = f.start(
        "capacity-rejected",
        scope,
        "set -eu; printf x >> \"$CAPACITY_WITNESS\"; printf admitted-once",
    );
    if let EnvironmentTaskRequest::Start { env, .. } = &mut retry {
        env.insert(
            "CAPACITY_WITNESS".into(),
            rejected_witness.to_string_lossy().into_owned(),
        );
    }
    assert_eq!(
        observation.request(&native, retry.clone()).unwrap_err(),
        "task_capacity"
    );
    assert!(
        fs::read(&rejected_witness).unwrap().is_empty(),
        "capacity rejection proves this attempt did not start"
    );
    assert_eq!(
        id(&observation.request(&native, run_request).unwrap()),
        run_background,
        "original-ID replay must remain available at full live capacity"
    );
    assert!(
        matches!(query(&native,&f,&background),EnvironmentTaskResult::Task { task, .. } if !task.cleanup_complete)
    );
    let terminate = EnvironmentTaskRequest::Terminate {
        fence: f.fence.clone(),
        task_id: busy[0].clone(),
        operation_id: f.fence.operation_id("terminate-busy"),
    };
    observation
        .request(&native, terminate.clone())
        .expect("termination retains its control channel at capacity");
    f.await_cleanup(&busy[0]);
    observation
        .request(&native, terminate)
        .expect("termination replay remains available");
    // Retry only the exact attempt explicitly proven not accepted, never an unknown Start.
    let retried = id(&observation.request(&native, retry.clone()).unwrap());
    f.await_cleanup(&retried);
    assert_eq!(id(&observation.request(&native, retry).unwrap()), retried);
    assert_eq!(fs::read(&rejected_witness).unwrap(), b"x");
    let end = EnvironmentTaskRequest::EndRun {
        fence: f.fence.clone(),
        run_id: f.fence.controller_owner.id().into(),
        operation_id: f.fence.operation_id("end-busy-run"),
    };
    assert!(matches!(
        observation.request(&native, end.clone()).unwrap(),
        EnvironmentTaskResult::RunEnded { .. }
    ));
    for task in busy.iter().chain(std::iter::once(&run_background)) {
        f.await_cleanup(task);
    }
    observation
        .request(&native, end)
        .expect("EndRun replay remains available");
    assert!(
        matches!(query(&native,&f,&background),EnvironmentTaskResult::Task { task, .. } if !task.cleanup_complete),
        "Run closure must preserve the unrelated environment task"
    );
    observation
        .request(
            &native,
            EnvironmentTaskRequest::Terminate {
                fence: f.fence.clone(),
                task_id: background.clone(),
                operation_id: f.fence.operation_id("terminate-environment"),
            },
        )
        .unwrap();
    f.await_cleanup(&background);
    observation.sample(&native);
    assert_eq!(native.inspection().unwrap()["activeTasks"], 0);
    assert_eq!(observation.rejections.get("task_capacity"), Some(&1));
    assert_eq!(
        observation.rejections.values().sum::<u64>(),
        1,
        "ordinary 256-command progress must not exhaust another task resource"
    );
    assert!(observation.peaks["hotTasks"] <= ENVIRONMENT_TASK_MAX_RECORDS as u64);
    observation.report("passed", None);
}
