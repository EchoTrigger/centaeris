use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
static SEQ: AtomicU64 = AtomicU64::new(0);
fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "centaeris-task-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
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
    let storage =
        TaskStorage::new(control.clone(), output.clone(), 2 * (HEADER + CHUNK) as u64).unwrap();
    let mut journal = Journal::open(&storage.control_root, "i").unwrap();
    let fence = EnvironmentTaskFence {
        environment_instance_id: "i".into(),
        controller_id: "r".into(),
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    };
    journal.takeover_persist(fence.clone(), &control).unwrap();
    journal.tasks.insert(
        "task".into(),
        Record {
            digest: "digest".into(),
            snapshot: EnvironmentTaskSnapshot {
                task_id: "task".into(),
                operation_id: "operation".into(),
                scope: EnvironmentTaskScope::Environment,
                state: EnvironmentTaskState::Exited,
                exit_code: Some(0),
                cleanup_complete: true,
                output_complete: true,
            },
        },
    );
    journal.persist(&control).unwrap();
    let before = fs::read(control.join("journal.json")).unwrap();
    let mut log = storage.open_output("task", 2).unwrap();
    for _ in 0..10 {
        log.append(EnvironmentTaskStream::Stdout, &[42; CHUNK])
            .unwrap();
    }
    assert!(storage.open_output("other", 1).is_err());
    drop(log);
    fs::remove_dir_all(&output).unwrap();
    assert_eq!(fs::read(control.join("journal.json")).unwrap(), before);
    let mut journal = Journal::open(&control, "i").unwrap();
    assert!(journal.check(&fence).is_ok());
    assert_eq!(
        journal
            .existing("task", "digest")
            .unwrap()
            .unwrap()
            .exit_code,
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
    let before = fs::read(p.join("journal.json")).unwrap();
    journal.instance = "x".repeat(CONTROL_JOURNAL_BYTES + 1);
    assert!(journal.save(&p).is_err());
    assert_eq!(fs::read(p.join("journal.json")).unwrap(), before);
    assert!(!p.join("journal.pending").exists());
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
    let snapshot = EnvironmentTaskSnapshot {
        task_id: "task".into(),
        operation_id: "operation".into(),
        scope: EnvironmentTaskScope::Environment,
        state: EnvironmentTaskState::Accepted,
        exit_code: None,
        cleanup_complete: false,
        output_complete: false,
    };
    journal.tasks.insert(
        "task".into(),
        Record {
            digest: "digest".into(),
            snapshot,
        },
    );
    journal.save(&p).unwrap();
    let journal = Journal::open(&p, "instance").unwrap();
    assert_eq!(
        journal.tasks["task"].snapshot.state,
        EnvironmentTaskState::Unknown
    );
    assert_eq!(
        journal.existing("task", "digest").unwrap().unwrap().state,
        EnvironmentTaskState::Unknown
    );
    assert!(journal.existing("task", "changed").is_err());
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
    fs::create_dir(p.join("journal.pending")).unwrap();
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
