use super::*;

struct OutputFixture {
    root: PathBuf,
    manager: Arc<Mutex<Manager>>,
    fence: EnvironmentTaskFence,
}
impl OutputFixture {
    fn new() -> Self {
        let root = lock_root();
        let control = root.join("control");
        let output = root.join("output");
        fs::create_dir(&control).unwrap();
        fs::create_dir(&output).unwrap();
        let mut manager = Manager::open(
            TaskStorage::new(control, output, OUTPUT_BUDGET_BYTES).unwrap(),
            root.join("unused-cgroups"),
            "i",
        )
        .unwrap();
        let fence = EnvironmentTaskFence {
            environment_instance_id: "i".into(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run".into(),
            },
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        };
        manager.journal.takeover(fence.clone()).unwrap();
        manager.persist().unwrap();
        Self {
            root,
            manager: Arc::new(Mutex::new(manager)),
            fence,
        }
    }
    fn completed(&self, name: &str, chunks: usize) -> (EnvironmentTaskRequest, String) {
        let request = EnvironmentTaskRequest::Start {
            fence: self.fence.clone(),
            operation_id: self.fence.operation_id(name),
            scope: owner_scope(&self.fence.controller_owner),
            program: "/must-not-be-spawned".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: "/app".into(),
            timeout_ms: 0,
        };
        let id = task_identity(&self.fence, &self.fence.operation_id(name)).unwrap();
        let mut manager = self.manager.lock().unwrap();
        let mut output = manager
            .storage
            .open_output(&id, ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK)
            .unwrap();
        for _ in 0..chunks {
            output
                .append(EnvironmentTaskStream::Stdout, &[42; CHUNK])
                .unwrap();
        }
        manager
            .logs
            .insert(id.clone(), Arc::new(Mutex::new(output)));
        manager.journal.tasks.insert(
            id.clone(),
            Record {
                digest: task_start_digest(&request, None).unwrap(),
                creator_owner: self.fence.controller_owner.clone(),
                identity_version: TaskIdentityVersion::OwnerV3,
                creator_epoch: 1,
                snapshot: EnvironmentTaskSnapshot {
                    task_id: id.clone(),
                    operation_id: self.fence.operation_id(name),
                    scope: owner_scope(&self.fence.controller_owner),
                    state: EnvironmentTaskState::Exited,
                    exit_code: Some(0),
                    timed_out: None,
                    cleanup_complete: true,
                    output_complete: true,
                },
            },
        );
        manager.persist().unwrap();
        (request, id)
    }
    fn consume(&self, id: &str) -> EnvironmentTaskRequest {
        EnvironmentTaskRequest::ConsumeOutput {
            fence: self.fence.clone(),
            task_id: id.into(),
            consumer_receipt_id: "durable-result-receipt".into(),
        }
    }
}
impl Drop for OutputFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn consumed_output_keeps_original_start_and_mutation_receipts_across_restart() {
    let fixture = OutputFixture::new();
    let (start, id) = fixture.completed("original", 2);
    let input_key = format!("input:{id}:1:accepted-input");
    let snapshot = fixture.manager.lock().unwrap().journal.tasks[&id]
        .snapshot
        .clone();
    {
        let mut manager = fixture.manager.lock().unwrap();
        manager.journal.receipts.insert(
            input_key.clone(),
            Receipt {
                digest: "a".repeat(64),
                result: Some(EnvironmentTaskResult::Mutated {
                    task: snapshot.clone(),
                }),
            },
        );
        manager.persist().unwrap();
    }
    let before = Manager::handle(&fixture.manager, start.clone()).unwrap();
    assert!(
        matches!(before, EnvironmentTaskResult::Task { output, gap: false, .. } if !output.is_empty())
    );
    let request = fixture.consume(&id);
    let expected = Manager::handle(&fixture.manager, request.clone()).unwrap();
    assert!(
        matches!(&expected, EnvironmentTaskResult::OutputConsumed { task, .. } if task == &snapshot)
    );
    assert!(!fixture
        .root
        .join("output")
        .join(format!("{id}.output"))
        .exists());
    assert_eq!(
        Manager::handle(&fixture.manager, request).unwrap(),
        expected,
        "lost consume ACK replays the same receipt"
    );
    assert_eq!(
        Manager::handle(&fixture.manager, start.clone()).unwrap(),
        expected,
        "original Start cannot spawn again"
    );
    {
        let mut manager = fixture.manager.lock().unwrap();
        manager.journal = Journal::open(&fixture.root.join("control"), "i").unwrap();
    }
    assert_eq!(Manager::handle(&fixture.manager, start).unwrap(), expected);
    assert!(
        matches!(store::receipt(&fixture.root.join("control"), &input_key).unwrap().unwrap().result,
        Some(EnvironmentTaskResult::Mutated { task }) if task == snapshot)
    );
}

#[test]
fn unconsumed_active_and_uncertain_tasks_keep_their_output() {
    let fixture = OutputFixture::new();
    let (_, id) = fixture.completed("unconsumed", 1);
    let output = fixture.root.join("output").join(format!("{id}.output"));
    let before = fs::read(&output).unwrap();
    fixture
        .manager
        .lock()
        .unwrap()
        .collect_consumed_output()
        .unwrap();
    assert_eq!(fs::read(&output).unwrap(), before);
    for state in [EnvironmentTaskState::Running, EnvironmentTaskState::Unknown] {
        fixture
            .manager
            .lock()
            .unwrap()
            .journal
            .tasks
            .get_mut(&id)
            .unwrap()
            .snapshot
            .state = state;
        assert_eq!(
            Manager::handle(&fixture.manager, fixture.consume(&id)).unwrap_err(),
            "task_output_not_consumable"
        );
        assert!(
            store::output_consumption(&fixture.root.join("control"), &id)
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(&output).unwrap(), before);
    }
    let key = format!("input:{id}:1:unknown-input");
    {
        let mut manager = fixture.manager.lock().unwrap();
        manager.journal.tasks.get_mut(&id).unwrap().snapshot.state = EnvironmentTaskState::Exited;
        manager.journal.receipts.insert(
            key,
            Receipt {
                digest: "b".repeat(64),
                result: None,
            },
        );
        manager.persist().unwrap();
    }
    assert_eq!(
        Manager::handle(&fixture.manager, fixture.consume(&id)).unwrap_err(),
        "task_output_not_consumable"
    );
    assert_eq!(fs::read(&output).unwrap(), before);
}

#[test]
fn consuming_with_stale_or_foreign_authority_never_releases_output() {
    let fixture = OutputFixture::new();
    let (_, id) = fixture.completed("owned", 1);
    let mut request = fixture.consume(&id);
    if let EnvironmentTaskRequest::ConsumeOutput { fence, .. } = &mut request {
        fence.controller_epoch += 1;
    }
    assert_eq!(
        Manager::handle(&fixture.manager, request).unwrap_err(),
        "task_controller_fenced"
    );
    let mut foreign = fixture.fence.clone();
    foreign.controller_owner = EnvironmentTaskOwner::AgentRun {
        run_id: "other".into(),
    };
    foreign.controller_epoch = 2;
    {
        let mut manager = fixture.manager.lock().unwrap();
        manager.journal.takeover(foreign.clone()).unwrap();
        manager.persist().unwrap();
    }
    let foreign_request = EnvironmentTaskRequest::ConsumeOutput {
        fence: foreign,
        task_id: id.clone(),
        consumer_receipt_id: "foreign-receipt".into(),
    };
    assert_eq!(
        Manager::handle(&fixture.manager, foreign_request).unwrap_err(),
        "task_output_consumer_denied"
    );
    assert!(fixture
        .root
        .join("output")
        .join(format!("{id}.output"))
        .exists());
}

#[test]
fn accepted_consumption_waits_for_the_last_output_reader_before_releasing_bytes() {
    let fixture = OutputFixture::new();
    let (_, id) = fixture.completed("reader", 1);
    let lease = Arc::new(());
    fixture
        .manager
        .lock()
        .unwrap()
        .output_readers
        .insert(id.clone(), Arc::downgrade(&lease));
    let request = fixture.consume(&id);
    assert_eq!(
        Manager::handle(&fixture.manager, request.clone()).unwrap_err(),
        "task_output_in_use"
    );
    assert!(
        store::output_consumption(&fixture.root.join("control"), &id)
            .unwrap()
            .is_some(),
        "consumption is durable before file retirement"
    );
    assert!(
        !store::output_consumption(&fixture.root.join("control"), &id)
            .unwrap()
            .unwrap()
            .reclaimed
    );
    assert_eq!(
        store::consumed_outputs(&fixture.root.join("control"), "").unwrap(),
        vec![id.clone()]
    );
    assert!(fixture
        .root
        .join("output")
        .join(format!("{id}.output"))
        .exists());
    drop(lease);
    fixture
        .manager
        .lock()
        .unwrap()
        .collect_consumed_output()
        .unwrap();
    assert!(!fixture
        .root
        .join("output")
        .join(format!("{id}.output"))
        .exists());
    assert!(matches!(
        Manager::handle(&fixture.manager, request).unwrap(),
        EnvironmentTaskResult::OutputConsumed { .. }
    ));
}

#[test]
fn committed_consumption_retries_physical_retirement_after_a_file_error() {
    let fixture = OutputFixture::new();
    let (_, id) = fixture.completed("pending-removal", 1);
    let output = fixture.root.join("output").join(format!("{id}.output"));
    fixture.manager.lock().unwrap().logs.remove(&id);
    fs::remove_file(&output).unwrap();
    fs::create_dir(&output).unwrap();
    assert_eq!(
        Manager::handle(&fixture.manager, fixture.consume(&id)).unwrap_err(),
        "task_output_reclamation_pending"
    );
    assert!(
        store::output_consumption(&fixture.root.join("control"), &id)
            .unwrap()
            .is_some()
    );
    fs::remove_dir(&output).unwrap();
    fixture
        .manager
        .lock()
        .unwrap()
        .collect_consumed_output()
        .unwrap();
    assert!(matches!(
        Manager::handle(&fixture.manager, fixture.consume(&id)).unwrap(),
        EnvironmentTaskResult::OutputConsumed { .. }
    ));
}

#[test]
fn same_run_consumes_more_than_the_output_budget_without_losing_old_task_identity() {
    let fixture = OutputFixture::new();
    let mut produced = 0u64;
    let mut first = None;
    for index in 0..70 {
        let (start, id) = fixture.completed(
            &format!("result-{index}"),
            ENVIRONMENT_TASK_OUTPUT_BYTES / CHUNK,
        );
        first.get_or_insert(start);
        produced += ENVIRONMENT_TASK_OUTPUT_BYTES as u64;
        Manager::handle(&fixture.manager, fixture.consume(&id)).unwrap();
        let mut manager = fixture.manager.lock().unwrap();
        manager.retire_history(1).unwrap();
        assert!(store::bytes(&manager.storage.output_root).unwrap() < OUTPUT_BUDGET_BYTES);
        assert_eq!(
            manager.journal.fence.as_ref().unwrap().controller_owner,
            fixture.fence.controller_owner
        );
    }
    assert!(produced > OUTPUT_BUDGET_BYTES);
    assert!(matches!(
        Manager::handle(&fixture.manager, first.unwrap()).unwrap(),
        EnvironmentTaskResult::OutputConsumed { .. }
    ));
    assert_eq!(store::bytes(&fixture.root.join("output")).unwrap(), 0);
}

#[test]
fn reclaimed_outputs_are_not_revisited_by_admission_or_receipt_replay() {
    let fixture = OutputFixture::new();
    let (start, id) = fixture.completed("already-reclaimed", 1);
    let request = fixture.consume(&id);
    let expected = Manager::handle(&fixture.manager, request.clone()).unwrap();
    let control = fixture.root.join("control");
    assert!(
        store::output_consumption(&control, &id)
            .unwrap()
            .unwrap()
            .reclaimed
    );
    assert!(store::consumed_outputs(&control, "").unwrap().is_empty());
    // A later admission sweep and old Start/Consume replay must not reopen or
    // sync an output directory whose consumed files have already been retired.
    let output = fixture.root.join("output");
    fs::rename(&output, fixture.root.join("retired-output-root")).unwrap();
    fixture
        .manager
        .lock()
        .unwrap()
        .collect_consumed_output()
        .unwrap();
    assert_eq!(Manager::handle(&fixture.manager, start).unwrap(), expected);
    assert_eq!(
        Manager::handle(&fixture.manager, request).unwrap(),
        expected
    );
    assert!(
        !output.exists(),
        "replay cannot recreate the retired output directory"
    );
}

#[test]
#[ignore = "explicit diagnostic of 2500 synthetic closed tasks on durable storage"]
fn closed_run_takeover_reports_large_cold_history_collection() {
    const TASKS: usize = 2500;
    let fixture = OutputFixture::new();
    let control = fixture.root.join("control");
    let output = fixture.root.join("output");
    let observe = || {
        let manager = fixture.manager.lock().unwrap();
        let mut state = store::inspection(&control, &output).unwrap();
        let connection = rusqlite::Connection::open_with_flags(
            control.join("history.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let (operations, barriers): (u64, u64) = connection
            .query_row(
                "SELECT (SELECT count(*) FROM operations),(SELECT count(*) FROM owner_barriers)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        state["operationRows"] = serde_json::json!(operations);
        state["ownerBarrierRows"] = serde_json::json!(barriers);
        state["memoryHotTasks"] = serde_json::json!(manager.journal.tasks.len());
        state["outputFiles"] = serde_json::json!(fs::read_dir(&output).unwrap().count());
        state
    };
    let seed_started = Instant::now();
    let mut peak_hot_tasks = 0;
    // Reuse completed synthetic records; no Start is dispatched and no child or
    // cgroup is created. Cooling retains each original Start identity on disk.
    for index in 0..TASKS {
        fixture.completed(&format!("synthetic-history-{index}"), 0);
        let mut manager = fixture.manager.lock().unwrap();
        peak_hot_tasks = peak_hot_tasks.max(manager.journal.tasks.len());
        manager.retire_history(1).unwrap();
    }
    let seed_wall_ms = seed_started.elapsed().as_secs_f64() * 1000.0;
    let cleanup = EnvironmentTaskFence {
        controller_epoch: 2,
        controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
        ..fixture.fence.clone()
    };
    Manager::handle(
        &fixture.manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: cleanup.environment_instance_id.clone(),
            controller_owner: cleanup.controller_owner.clone(),
            controller_epoch: cleanup.controller_epoch,
            controller_purpose: cleanup.controller_purpose.clone(),
        },
    )
    .unwrap();
    let ended = Manager::handle(
        &fixture.manager,
        EnvironmentTaskRequest::EndRun {
            fence: cleanup.clone(),
            run_id: cleanup.controller_owner.id().into(),
            operation_id: cleanup.operation_id("close-run"),
        },
    )
    .unwrap();
    assert!(matches!(
        &ended,
        EnvironmentTaskResult::RunEnded {
            cleanup_complete: true,
            ..
        }
    ));
    assert!(store::next_closed(&control, "", &cleanup.controller_owner)
        .unwrap()
        .is_none());
    let next = EnvironmentTaskFence {
        controller_owner: EnvironmentTaskOwner::AgentRun {
            run_id: "next-run".into(),
        },
        controller_epoch: 3,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        ..fixture.fence.clone()
    };
    let mut candidates = 0;
    let mut after_id = String::new();
    while let Some((id, _)) =
        store::next_closed(&control, &after_id, &next.controller_owner).unwrap()
    {
        candidates += 1;
        after_id = id;
    }
    let before = observe();
    let started = Instant::now();
    let result = Manager::handle(
        &fixture.manager,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: next.environment_instance_id.clone(),
            controller_owner: next.controller_owner.clone(),
            controller_epoch: next.controller_epoch,
            controller_purpose: next.controller_purpose.clone(),
        },
    );
    let takeover_wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after = observe();
    eprintln!(
        "TASK_CLOSED_HISTORY_OBSERVATION {}",
        serde_json::json!({
            "syntheticFixture":true,"seededTasks":TASKS,"eligibleCandidates":candidates,
            "seedWallMs":seed_wall_ms,"takeoverWallMs":takeover_wall_ms,
            "measuredPath":"Manager::handle(Takeover), including synchronous GC and persistence",
            "peakMemoryHotTasks":peak_hot_tasks,"before":before,"after":after,
            "takeoverError":result.as_ref().err(),
        })
    );
    assert!(matches!(result, Ok(EnvironmentTaskResult::TakenOver { fence }) if fence == next));
    assert_eq!(candidates, TASKS);
    assert_eq!(before["indexedTasks"], TASKS);
    assert_eq!(before["hotTasks"], 0);
    assert_eq!(before["memoryHotTasks"], 0);
    assert_eq!(before["outputFiles"], TASKS);
    assert_eq!(after["indexedTasks"], 0);
    assert_eq!(after["hotTasks"], 0);
    assert_eq!(after["memoryHotTasks"], 0);
    assert_eq!(after["outputFiles"], 0);
    assert_eq!(before["ownerBarrierRows"], 1);
    assert_eq!(after["ownerBarrierRows"], 1);
    assert_eq!(before["operationRows"], 1);
    assert_eq!(after["operationRows"], 1);
    assert!(store::closed(&control, &cleanup.controller_owner).unwrap());
    assert_eq!(
        store::check_execution(&control, &cleanup.controller_owner).unwrap_err(),
        "task_run_closed"
    );
    assert!(store::check_execution(&control, &next.controller_owner).is_ok());
    assert_eq!(
        store::receipt(
            &control,
            &format!("endRun:{}", cleanup.controller_owner.id())
        )
        .unwrap()
        .unwrap()
        .result,
        Some(ended),
        "GC preserves the permanent Run closure acknowledgement"
    );
    let reopened = Journal::open(&control, "i").unwrap();
    assert_eq!(reopened.fence.as_ref(), Some(&next));
    assert!(reopened.tasks.is_empty());
}
