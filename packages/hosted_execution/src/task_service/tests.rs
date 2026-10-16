use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
static SEQ: AtomicU64 = AtomicU64::new(0);

#[test]
fn stable_task_address_matches_existing_index_across_owner_renewal() {
    let original = fixture_fence("instance", "run", 3);
    let renewed = fixture_fence("instance", "run", 4);
    let old_id = task_identity(&original, &original.operation_id("command-original")).unwrap();
    assert_eq!(
        old_id,
        renewed
            .task_id(&renewed.operation_id("command-original"))
            .unwrap()
    );
    assert_ne!(
        old_id,
        renewed
            .task_id(&renewed.operation_id("command-other"))
            .unwrap()
    );
    let foreign = fixture_fence("instance", "other-run", 4);
    assert_ne!(
        old_id,
        foreign
            .task_id(&foreign.operation_id("command-original"))
            .unwrap()
    );
    assert!(renewed
        .task_id(&original.operation_id("command-original"))
        .is_err());
}
pub(super) fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "centaeris-task-test-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn shared_core_query_budget_reads_real_output_log_and_small_budget_is_rejected() {
    let p = root();
    let mut log = OutputLog::open(&p.join("output"), 2).unwrap();
    log.append(EnvironmentTaskStream::Stdout, &[42; CHUNK])
        .unwrap();
    assert!(matches!(log.read(0,1),Err(code) if code=="task_invalid_output_cursor"));
    let (chunks, _, _, _, _) = log.read(0, ENVIRONMENT_TASK_READ_BYTES).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].data, vec![42; CHUNK]);
    drop(log);
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn output_budget_exhaustion_and_cleanup_preserve_control_receipts_and_fence() {
    let p = root();
    let control = p.join("control");
    let output = p.join("output");
    fs::create_dir_all(&control).unwrap();
    fs::create_dir_all(&output).unwrap();
    let storage = TaskStorage::new(control.clone(), output.clone(), 12 * 1024).unwrap();
    let mut journal = Journal::open(&control, "i").unwrap();
    let fence = fixture_fence("i", "r", 1);
    journal.takeover_persist(fence.clone(), &control).unwrap();
    let record = fixture_record("i", "r", 1, "task", false);
    let id = record.snapshot.task_id.clone();
    let d = record.digest.clone();
    journal.tasks.insert(id.clone(), record);
    journal.persist(&control).unwrap();
    let before = fs::read(control.join("history.sqlite")).unwrap();
    let mut log = storage.open_output(&id, 2).unwrap();
    for _ in 0..10 {
        log.append(EnvironmentTaskStream::Stdout, &[42; CHUNK])
            .unwrap();
    }
    assert!(storage.open_output("other", 1).is_err());
    drop(log);
    fs::remove_dir_all(&output).unwrap();
    assert_eq!(fs::read(control.join("history.sqlite")).unwrap(), before);
    let mut journal = Journal::open(&control, "i").unwrap();
    assert!(journal.check(&fence).is_ok());
    assert_eq!(
        journal.existing(&id, &d).unwrap().unwrap().exit_code,
        Some(0)
    );
    journal
        .takeover_persist(
            EnvironmentTaskFence {
                controller_epoch: 2,
                ..fence
            },
            &control,
        )
        .unwrap();
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn control_serialized_budget_refuses_oversize_without_overwriting_committed_record() {
    let p = root();
    let mut journal = Journal::open(&p, "i").unwrap();
    let before = fs::read(p.join("history.sqlite")).unwrap();
    journal.instance = "x".repeat(CONTROL_JOURNAL_BYTES + 1);
    assert!(journal.save(&p).is_err());
    assert_eq!(fs::read(p.join("history.sqlite")).unwrap(), before);
    assert!(!p.join("history.sqlite-journal").exists());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn unavailable_output_reports_gap_without_losing_queryable_control_snapshot() {
    let snapshot = EnvironmentTaskSnapshot {
        task_id: "task".into(),
        operation_id: "operation".into(),
        scope: EnvironmentTaskScope::Environment,
        state: EnvironmentTaskState::Exited,
        exit_code: Some(7),
        cleanup_complete: true,
        output_complete: true,
    };
    let result = task_query_result(snapshot.clone(), None, 5, ENVIRONMENT_TASK_READ_BYTES).unwrap();
    assert!(
        matches!(result, EnvironmentTaskResult::Task { task, output, next_cursor: 5, gap: true, has_more: false, .. } if task == snapshot && output.is_empty())
    );
}

#[test]
fn reopened_fully_corrupt_output_explicitly_reports_loss() {
    let p = root();
    let path = p.join("output");
    let mut log = OutputLog::open(&path, 1).unwrap();
    log.append(EnvironmentTaskStream::Stdout, b"data").unwrap();
    drop(log);
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(HEADER as u64)).unwrap();
    file.write_all(b"lost").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let mut log = OutputLog::open(&path, 1).unwrap();
    let (chunks, _, _, gap, _) = log.read(0, ENVIRONMENT_TASK_READ_BYTES).unwrap();
    assert!(gap);
    assert!(chunks.is_empty());
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn output_retains_bounded_tail_and_reports_cursor_gap_after_reopen() {
    let p = root();
    let mut log = OutputLog::open(&p.join("output"), 2).unwrap();
    log.append(EnvironmentTaskStream::Stdout, b"first").unwrap();
    log.append(EnvironmentTaskStream::Stderr, b"second")
        .unwrap();
    log.append(EnvironmentTaskStream::Stdout, b"third").unwrap();
    drop(log);
    let mut log = OutputLog::open(&p.join("output"), 2).unwrap();
    let (chunks, earliest, next, gap, more) = log.read(0, 65536).unwrap();
    assert_eq!((earliest, next, gap, more), (1, 3, true, false));
    assert_eq!(
        chunks.iter().map(|c| c.data.clone()).collect::<Vec<_>>(),
        vec![b"second".to_vec(), b"third".to_vec()]
    );
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn accepted_receipt_recovers_unknown_and_same_operation_cannot_respawn() {
    let p = root();
    let mut journal = Journal::open(&p, "instance").unwrap();
    journal.takeover(fixture_fence("instance", "r", 1)).unwrap();
    let mut record = fixture_record("instance", "r", 1, "task", true);
    record.snapshot.state = EnvironmentTaskState::Accepted;
    let id = record.snapshot.task_id.clone();
    let d = record.digest.clone();
    journal.tasks.insert(id.clone(), record);
    journal.save(&p).unwrap();
    let journal = Journal::open(&p, "instance").unwrap();
    assert_eq!(
        journal.tasks[&id].snapshot.state,
        EnvironmentTaskState::Unknown
    );
    assert_eq!(
        journal.existing(&id, &d).unwrap().unwrap().state,
        EnvironmentTaskState::Unknown
    );
    assert!(journal.existing(&id, &digest(&"changed").unwrap()).is_err());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn takeover_fences_stale_controller_and_rejects_instance_mismatch() {
    let p = root();
    let mut j = Journal::open(&p, "instance").unwrap();
    let old = EnvironmentTaskFence {
        environment_instance_id: "instance".into(),
        controller_id: "a".into(),
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    j.takeover(old.clone()).unwrap();
    let new = EnvironmentTaskFence {
        controller_id: "b".into(),
        controller_epoch: 2,
        ..old.clone()
    };
    j.takeover(new.clone()).unwrap();
    assert!(j.check(&old).is_err());
    assert!(j.check(&new).is_ok());
    assert!(j.takeover(old).is_err());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn failed_fence_persistence_refuses_all_subsequent_controller_commands() {
    let p = root();
    let mut j = Journal::open(&p, "instance").unwrap();
    let old = EnvironmentTaskFence {
        environment_instance_id: "instance".into(),
        controller_id: "a".into(),
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    j.takeover_persist(old.clone(), &p).unwrap();
    fs::create_dir(p.join("history.sqlite-journal")).unwrap();
    let new = EnvironmentTaskFence {
        controller_id: "b".into(),
        controller_epoch: 2,
        ..old.clone()
    };
    assert!(j.takeover_persist(new.clone(), &p).is_err());
    assert!(j.check(&new).is_err());
    assert!(j.check(&old).is_err());
    assert!(j.takeover(old).is_err());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn corrupted_output_record_is_a_cursor_gap_and_never_returns_unverified_bytes() {
    let p = root();
    let path = p.join("output");
    let mut log = OutputLog::open(&path, 2).unwrap();
    log.append(EnvironmentTaskStream::Stdout, b"first").unwrap();
    log.append(EnvironmentTaskStream::Stderr, b"second")
        .unwrap();
    let mut file = OpenOptions::new().write(true).open(&path).unwrap();
    file.seek(SeekFrom::Start(HEADER as u64)).unwrap();
    file.write_all(b"other").unwrap();
    file.sync_all().unwrap();
    let (chunks, earliest, next, gap, more) = log.read(0, ENVIRONMENT_TASK_READ_BYTES).unwrap();
    assert_eq!((earliest, next, gap, more), (0, 2, true, false));
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].data, b"second");
    assert!(log.read(3, ENVIRONMENT_TASK_READ_BYTES).is_err());
    assert!(log.read(0, ENVIRONMENT_TASK_READ_BYTES + 1).is_err());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn same_epoch_cannot_upgrade_cleanup_to_execution_and_pending_takeover_refuses_mutation() {
    let p = root();
    let mut journal = Journal::open(&p, "i").unwrap();
    let fence = EnvironmentTaskFence {
        environment_instance_id: "i".into(),
        controller_id: "r".into(),
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
    };
    journal.takeover_persist(fence.clone(), &p).unwrap();
    assert!(journal
        .takeover(EnvironmentTaskFence {
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            ..fence.clone()
        })
        .is_err());
    journal.takeover_pending = true;
    journal.persist(&p).unwrap();
    assert!(journal.check(&fence).is_err());
    assert!(Journal::open(&p, "i").unwrap().check(&fence).is_err());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn admission_capacity_failure_does_not_poison_existing_work() {
    let p = root();
    let mut journal = Journal {
        poisoned: false,
        index_root: None,
        schema: ENVIRONMENT_TASK_SCHEMA.into(),
        instance: "i".into(),
        fence: None,
        takeover_pending: false,
        closed_run: None,
        tasks: BTreeMap::new(),
        receipts: BTreeMap::new(),
    };
    journal.instance = "x".repeat(CONTROL_JOURNAL_BYTES + 1);
    assert_eq!(journal.persist(&p).unwrap_err(), "task_journal_too_large");
    assert!(
        !journal.poisoned,
        "capacity refusal must leave the service usable"
    );
    journal.instance = "i".into();
    fs::remove_dir_all(p).unwrap();
}

fn fixture_fence(instance: &str, controller: &str, epoch: u64) -> EnvironmentTaskFence {
    EnvironmentTaskFence {
        environment_instance_id: instance.into(),
        controller_id: controller.into(),
        controller_epoch: epoch,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    }
}
fn fixture_record(instance: &str, controller: &str, epoch: u64, alias: &str, live: bool) -> Record {
    let fence = fixture_fence(instance, controller, epoch);
    let operation_id = fence.operation_id(alias);
    Record {
        digest: digest(&(instance, controller, alias)).unwrap(),
        creator_controller_id: controller.into(),
        creator_epoch: epoch,
        snapshot: EnvironmentTaskSnapshot {
            task_id: task_identity(&fence, &operation_id).unwrap(),
            operation_id,
            scope: EnvironmentTaskScope::Environment,
            state: if live {
                EnvironmentTaskState::Running
            } else {
                EnvironmentTaskState::Exited
            },
            exit_code: (!live).then_some(0),
            cleanup_complete: !live,
            output_complete: !live,
        },
    }
}
fn history_record(alias: &str, epoch: u64, live: bool) -> Record {
    fixture_record("i", &format!("r{epoch}"), epoch, alias, live)
}

#[test]
fn same_run_controller_reconstruction_keeps_start_and_input_operation_identity() {
    let original = EnvironmentTaskFence {
        environment_instance_id: "i".into(),
        controller_id: "r".into(),
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    let current = EnvironmentTaskFence {
        controller_epoch: 2,
        ..original.clone()
    };
    assert_eq!(
        task_identity(&original, "1:tool-call").unwrap(),
        task_identity(&current, "2:tool-call").unwrap(),
        "reconstructed Run must not spawn again"
    );
    assert_eq!(
        input_identity(&original, "task", "1:input"),
        input_identity(&current, "task", "2:input"),
        "reconstructed Run must find original input receipt"
    );
}

#[test]
fn same_run_takeover_preserves_uncertain_input_and_terminal_start_history() {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    let original = fixture_fence("i", "r", 1);
    j.takeover(original.clone()).unwrap();
    let record = fixture_record("i", "r", 1, "task", false);
    let id = record.snapshot.task_id.clone();
    let d = record.digest.clone();
    j.tasks.insert(id.clone(), record);
    let key = input_identity(&original, &id, "1:input").unwrap();
    let data_digest = digest(&"input").unwrap();
    j.reserve_receipt(&key, &data_digest).unwrap();
    j.persist(&p).unwrap();
    let current = EnvironmentTaskFence {
        controller_epoch: 2,
        ..original
    };
    j.takeover(current.clone()).unwrap();
    assert!(j.retire_task(&id, 2, false));
    assert!(j.existing(&id, &d).unwrap().is_some());
    assert_eq!(
        j.reserve_receipt(
            &input_identity(&current, &id, "2:input").unwrap(),
            &data_digest
        )
        .unwrap_err(),
        "task_operation_unknown"
    );
    j.persist(&p).unwrap();
    j = Journal::open(&p, "i").unwrap();
    assert!(j.existing(&id, &d).unwrap().is_some());
    assert_eq!(
        j.reserve_receipt(&key, &data_digest).unwrap_err(),
        "task_operation_unknown"
    );
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn input_exhaustion_keeps_task_termination_and_run_closure_available() {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    j.takeover(fixture_fence("i", "r", 1)).unwrap();
    let input_digest = digest(&"input").unwrap();
    let stop_digest = digest(&"stop").unwrap();
    let close_digest = digest(&"close").unwrap();
    for index in 0..1000 {
        let key = format!("input:task:1:{index}");
        j.reserve_receipt(&key, &input_digest).unwrap();
    }
    assert!(j.receipts.len() <= ENVIRONMENT_TASK_MAX_RECORDS);
    assert!(j
        .reserve_receipt("terminate:task", &stop_digest)
        .unwrap()
        .is_none());
    assert!(j
        .reserve_receipt("endRun:r", &close_digest)
        .unwrap()
        .is_none());
    // Lost acknowledgement of an idempotent control remains reconcilable; an
    // unknown input is never delivered again.
    assert!(j
        .reserve_receipt("terminate:task", &stop_digest)
        .unwrap()
        .is_none());
    assert_eq!(
        j.reserve_receipt("input:task:1:0", &input_digest)
            .unwrap_err(),
        "task_operation_unknown"
    );
    assert!(j.receipts.len() <= ENVIRONMENT_TASK_MAX_RECORDS);
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn one_hundred_controller_windows_retire_history_and_keep_environment_task() {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    let mut fence = fixture_fence("i", "r1", 1);
    let background = history_record("background", 1, true);
    let background_id = background.snapshot.task_id.clone();
    j.tasks.insert(background_id.clone(), background);
    for epoch in 1..=100 {
        fence.controller_epoch = epoch;
        fence.controller_id = format!("r{epoch}");
        j.takeover(fence.clone()).unwrap();
        let ids: Vec<_> = j.tasks.keys().cloned().collect();
        for id in ids {
            j.retire_task(&id, epoch, id == background_id);
        }
        assert_eq!(j.tasks.len(), 1);
        // Model closed-Run collection before admitting the next window. The real
        // Manager also verifies physical retirement before calling this store API.
        while let Some((id, _)) = store::next_closed(&p, "", &fence.controller_id).unwrap() {
            store::forget_closed(&p, &id).unwrap();
        }
        let records = if epoch == 1 {
            ENVIRONMENT_TASK_MAX_RECORDS - 1
        } else {
            2
        };
        for index in 0..records {
            j.check_admission().unwrap();
            let mut record = history_record(&format!("history-{index}"), epoch, false);
            record.snapshot.scope = EnvironmentTaskScope::Run {
                run_id: fence.controller_id.clone(),
            };
            j.tasks.insert(record.snapshot.task_id.clone(), record);
        }
        if epoch == 1 {
            assert_eq!(j.check_admission().unwrap_err(), "task_history_capacity");
        } else {
            j.check_admission().unwrap();
        }
        assert!(!j.poisoned);
        j.closed_run = Some(fence.controller_id.clone());
        assert_eq!(j.check_admission().unwrap_err(), "task_run_closed");
        assert!(!j.retire_task(&background_id, epoch + 1, true));
        j.persist(&p).unwrap();
        j = Journal::open(&p, "i").unwrap();
        assert_eq!(j.check_admission().unwrap_err(), "task_run_closed");
        assert_eq!(
            fence.validate_operation_id("1:old-start").is_ok(),
            epoch == 1
        );
    }
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn admission_preflight_preserves_committed_work_and_completion_space() {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    j.takeover(fixture_fence("i", "r", 1)).unwrap();
    let background = fixture_record("i", "r", 1, "background", true);
    let id = background.snapshot.task_id.clone();
    j.tasks.insert(id.clone(), background);
    j.persist(&p).unwrap();
    let committed = fs::read(p.join("history.sqlite")).unwrap();
    let occupied = store::bytes(&p).unwrap() as usize;
    let pressure = p.join("capacity-pressure");
    fs::write(
        &pressure,
        vec![0; CONTROL_ROOT_BYTES - CONTROL_COMPLETION_RESERVE_BYTES - occupied + 1],
    )
    .unwrap();
    let data = digest(&"input").unwrap();
    let input = format!("input:{id}:large");
    assert_eq!(
        j.reserve_receipt(&input, &data).unwrap_err(),
        "task_journal_capacity"
    );
    assert!(j.receipts.is_empty());
    assert!(!j.poisoned);
    assert_eq!(fs::read(p.join("history.sqlite")).unwrap(), committed);
    assert_eq!(j.tasks[&id].snapshot.state, EnvironmentTaskState::Running);
    // Control receipts remain available while ordinary work is refused.
    assert!(j
        .reserve_receipt(&format!("terminate:{id}"), &digest(&id).unwrap())
        .is_ok());
    assert!(j
        .reserve_receipt("endRun:r", &digest(&"r").unwrap())
        .is_ok());
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn one_thousand_same_run_terminal_records_keep_original_start_identity() {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    j.takeover(fixture_fence("i", "r1", 1)).unwrap();
    for n in 0..1000 {
        let record = history_record(&format!("finished-{n}"), 1, false);
        let id = record.snapshot.task_id.clone();
        let d = record.digest.clone();
        j.check_admission().unwrap();
        j.tasks.insert(id.clone(), record);
        j.persist(&p).unwrap();
        assert!(j.retire_task(&id, 1, false));
        assert!(j.existing(&id, &d).unwrap().is_some());
    }
    j = Journal::open(&p, "i").unwrap();
    let first = history_record("finished-0", 1, false);
    let last = history_record("finished-999", 1, false);
    assert!(j
        .existing(&first.snapshot.task_id, &first.digest)
        .unwrap()
        .is_some());
    assert!(j
        .existing(&last.snapshot.task_id, &digest(&"changed").unwrap())
        .is_err());
    assert!(j.tasks.len() <= ENVIRONMENT_TASK_MAX_RECORDS);
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn compact_cold_output_preserves_results_before_consumer_commit_and_ring_gaps() {
    for slots in [1, 8] {
        let p = root();
        let path = p.join("out");
        let mut output = OutputLog::open(&path, slots).unwrap();
        output
            .append(EnvironmentTaskStream::Stdout, b"first")
            .unwrap();
        output
            .append(EnvironmentTaskStream::Stderr, b"second")
            .unwrap();
        let before = output.read(0, ENVIRONMENT_TASK_READ_BYTES).unwrap();
        output.compact().unwrap();
        drop(output);
        let size = fs::metadata(&path).unwrap().len();
        let mut output = OutputLog::open_retained(&path).unwrap();
        assert_eq!(output.read(0, ENVIRONMENT_TASK_READ_BYTES).unwrap(), before);
        assert_eq!(fs::metadata(&path).unwrap().len(), size);
        drop(output);
        fs::remove_dir_all(p).unwrap();
    }
}

#[test]
fn closed_run_cannot_reopen_after_intervening_controller_and_store_reopen() {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    let mut fence = EnvironmentTaskFence {
        environment_instance_id: "i".into(),
        controller_id: "r".into(),
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    j.takeover(fence.clone()).unwrap();
    j.closed_run = Some("r".into());
    j.persist(&p).unwrap();
    fence.controller_id = "other".into();
    fence.controller_epoch = 2;
    j.takeover(fence.clone()).unwrap();
    j.persist(&p).unwrap();
    j = Journal::open(&p, "i").unwrap();
    fence.controller_id = "r".into();
    fence.controller_epoch = 3;
    j.takeover(fence).unwrap();
    assert_eq!(j.check_admission().unwrap_err(), "task_run_closed");
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn unsupported_index_version_and_missing_table_are_not_repaired() {
    for sql in ["PRAGMA user_version=99", "DROP TABLE operations"] {
        let p = root();
        Journal::open(&p, "i").unwrap();
        let db = rusqlite::Connection::open(p.join("history.sqlite")).unwrap();
        db.execute_batch(sql).unwrap();
        drop(db);
        let before = fs::read(p.join("history.sqlite")).unwrap();
        assert!(Journal::open(&p, "i").is_err());
        assert_eq!(fs::read(p.join("history.sqlite")).unwrap(), before);
        fs::remove_dir_all(p).unwrap();
    }
}

#[test]
fn index_counts_and_memory_stay_bounded_with_three_thousand_realistic_task_ids() {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    j.takeover(EnvironmentTaskFence {
        environment_instance_id: "i".into(),
        controller_id: "agent_run_00000000000000000000000000000000".into(),
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    })
    .unwrap();
    for n in 0..3000 {
        let operation = format!("1:command_{}", digest(&("command", n)).unwrap());
        let id = task_identity(j.fence.as_ref().unwrap(), &operation).unwrap();
        let mut record = history_record(&id, 1, false);
        record.creator_controller_id = j.fence.as_ref().unwrap().controller_id.clone();
        record.digest = "d".repeat(64);
        record.snapshot.operation_id = operation;
        record.snapshot.task_id = id.clone();
        record.snapshot.scope = EnvironmentTaskScope::Run {
            run_id: record.creator_controller_id.clone(),
        };
        for input in 0..if n < 2000 { 2 } else { 1 } {
            let key = input_identity(
                j.fence.as_ref().unwrap(),
                &id,
                &format!("1:input_{}", digest(&(n, input)).unwrap()),
            )
            .unwrap();
            let mut accepted = record.snapshot.clone();
            accepted.state = EnvironmentTaskState::Running;
            accepted.exit_code = None;
            accepted.cleanup_complete = false;
            accepted.output_complete = false;
            j.receipts.insert(
                key,
                Receipt {
                    digest: "e".repeat(64),
                    result: Some(EnvironmentTaskResult::Mutated { task: accepted }),
                },
            );
        }
        j.check_admission().unwrap();
        j.tasks.insert(id.clone(), record);
        j.preflight(4096).unwrap();
        j.persist(&p).unwrap();
        assert!(j.retire_task(&id, 1, false));
        j.receipts.clear();
    }
    let inspection = store::inspection(&p, &p).unwrap();
    assert_eq!(inspection["indexedTasks"], 3000);
    assert_eq!(inspection["hotTasks"], 0);
    assert!(store::bytes(&p).unwrap() < (INDEX_BYTES - CONTROL_COMPLETION_RESERVE_BYTES) as u64);
    let first_operation = format!("1:command_{}", digest(&("command", 0)).unwrap());
    let first_id = task_identity(j.fence.as_ref().unwrap(), &first_operation).unwrap();
    let first_key = input_identity(
        j.fence.as_ref().unwrap(),
        &first_id,
        &format!("1:input_{}", digest(&(0, 0)).unwrap()),
    )
    .unwrap();
    assert!(
        matches!(store::receipt(&p,&first_key).unwrap().unwrap().result,Some(EnvironmentTaskResult::Mutated{task}) if task.state==EnvironmentTaskState::Running && task.exit_code.is_none() && !task.cleanup_complete)
    );
    assert_eq!(
        store::task(&p, &first_id).unwrap().unwrap().snapshot.state,
        EnvironmentTaskState::Exited
    );
    eprintln!(
        "private index: tasks=3000 inputReceipts=5000 hot=0 bytes={}",
        store::bytes(&p).unwrap()
    );
    fs::remove_dir_all(p).unwrap();
}

#[test]
fn strict_forward_import_preserves_legacy_journal_and_rejects_invalid_identity() {
    for valid in [true, false] {
        let p = root();
        let mut j = Journal::open(&p, "i").unwrap();
        let fence = EnvironmentTaskFence {
            environment_instance_id: "i".into(),
            controller_id: "r1".into(),
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        };
        j.takeover(fence.clone()).unwrap();
        let id = task_identity(&fence, "1:start").unwrap();
        let mut record = fixture_record("i", "r1", 1, "start", false);
        record.digest = "d".repeat(64);
        record.snapshot.operation_id = "1:start".into();
        if !valid {
            record.snapshot.task_id = "wrong".into();
        }
        j.tasks.insert(id.clone(), record);
        let legacy = serde_json::to_vec(&j).unwrap();
        fs::remove_file(p.join("history.sqlite")).unwrap();
        fs::write(p.join("journal.json"), &legacy).unwrap();
        let result = Journal::open(&p, "i");
        assert_eq!(result.is_ok(), valid);
        if valid {
            assert_eq!(fs::read(p.join("journal.legacy.json")).unwrap(), legacy);
            assert!(
                serde_json::from_slice::<Journal>(&fs::read(p.join("journal.json")).unwrap())
                    .is_err()
            );
        } else {
            assert_eq!(fs::read(p.join("journal.json")).unwrap(), legacy);
        }
        if let Ok(mut reopened) = result {
            assert!(reopened.existing(&id, &"d".repeat(64)).unwrap().is_some());
            reopened.persist(&p).unwrap();
            assert_eq!(fs::read(p.join("journal.legacy.json")).unwrap(), legacy);
        }
        fs::remove_dir_all(p).unwrap();
    }
}

#[test]
fn closed_history_collection_preserves_environment_and_uncertain_input_and_does_not_resurrect_receipts(
) {
    let p = root();
    let mut j = Journal::open(&p, "i").unwrap();
    let fence = fixture_fence("i", "r1", 1);
    j.takeover(fence.clone()).unwrap();
    let mut ids = std::collections::BTreeMap::new();
    for alias in ["complete", "uncertain", "environment"] {
        let mut record = history_record(alias, 1, false);
        if alias != "environment" {
            record.snapshot.scope = EnvironmentTaskScope::Run {
                run_id: "r1".into(),
            };
        }
        let id = record.snapshot.task_id.clone();
        ids.insert(alias, id.clone());
        j.tasks.insert(id, record);
    }
    let key = input_identity(&fence, &ids["uncertain"], "1:input").unwrap();
    j.reserve_receipt(&key, &digest(&"input").unwrap()).unwrap();
    j.closed_run = Some("r1".into());
    j.persist(&p).unwrap();
    for id in ids.values() {
        assert!(j.retire_task(id, 1, false));
    }
    let (id, _) = store::next_closed(&p, "", "r2").unwrap().unwrap();
    assert_eq!(id, ids["complete"]);
    store::forget_closed(&p, &id).unwrap();
    j.persist(&p).unwrap();
    assert!(store::task(&p, &ids["complete"]).unwrap().is_none());
    assert!(store::task(&p, &ids["uncertain"]).unwrap().is_some());
    assert!(store::task(&p, &ids["environment"]).unwrap().is_some());
    assert!(store::next_closed(&p, "", "r2").unwrap().is_none());
    fs::remove_dir_all(p).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn allocated_output_blocks_reject_admission_before_oversubscribing_output_budget() {
    use std::os::fd::AsRawFd;
    let p = root();
    let control = p.join("control");
    let output = p.join("output");
    fs::create_dir(&control).unwrap();
    fs::create_dir(&output).unwrap();
    let path = output.join("previous.output");
    let mut file = File::create(&path).unwrap();
    file.write_all(b"x").unwrap();
    // SAFETY: the descriptor belongs to this test's isolated regular file.
    assert_eq!(
        unsafe { libc::fallocate(file.as_raw_fd(), libc::FALLOC_FL_KEEP_SIZE, 0, 65536) },
        0
    );
    file.sync_all().unwrap();
    assert_eq!(file.metadata().unwrap().len(), 1);
    assert!(charged_bytes(&file.metadata().unwrap()) >= 65536);
    let storage = TaskStorage::new(control, output, 32768).unwrap();
    assert!(matches!(storage.open_output("new",1),Err(error) if error=="task_output_capacity"));
    assert_eq!(fs::read(&path).unwrap(), b"x");
    drop(file);
    fs::remove_dir_all(p).unwrap();
}
