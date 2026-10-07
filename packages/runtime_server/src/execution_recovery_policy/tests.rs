use super::*;

fn eligible() -> RecoveryEvidence {
    RecoveryEvidence {
        failure: HostFailureEvidence::PreDispatchProcessNotStarted,
        receipt_reported_executed: Some(false),
        durable_receipt_sequence: 8,
        previous_checkpoint_sequence: 5,
        next_model_request_started: true,
        open_tool_calls: 0,
        parallel_tool_calls: 0,
        tool_calls_since_checkpoint: 1,
        receipts_since_checkpoint: 1,
        all_suffix_tools_are_bash: true,
        external_or_mcp_calls_since_checkpoint: 0,
        successful_receipts_since_checkpoint: 0,
        facts_since_checkpoint: 0,
        workspace: WorkspaceReuseEvidence {
            prior_checkpoint_snapshot_valid: true,
            generation_known: true,
            snapshot_was_quiesced: true,
            snapshot_activity_epoch: 7,
            current_activity_epoch: 7,
            snapshot_host_instance: 11,
            current_host_instance: 11,
        },
    }
}

#[test]
fn advances_checkpoint_past_durable_unexecuted_receipt() {
    let decision = recovery_eligibility(&eligible()).expect("eligible first process loss");
    assert_eq!(decision.covered_through_sequence, 8);
    assert_eq!(decision.source_checkpoint_sequence, 5);
}

#[test]
fn rejects_dispatch_started_indeterminate_io_unknown_and_unsafe_ledger() {
    let mutations: [fn(&mut RecoveryEvidence); 8] = [
        |e: &mut RecoveryEvidence| e.failure = HostFailureEvidence::DispatchStarted,
        |e: &mut RecoveryEvidence| e.failure = HostFailureEvidence::CancellationIndeterminate,
        |e: &mut RecoveryEvidence| e.failure = HostFailureEvidence::PostDispatchIo,
        |e: &mut RecoveryEvidence| e.failure = HostFailureEvidence::Unknown,
        |e: &mut RecoveryEvidence| e.open_tool_calls = 1,
        |e: &mut RecoveryEvidence| e.parallel_tool_calls = 1,
        |e: &mut RecoveryEvidence| e.successful_receipts_since_checkpoint = 1,
        |e: &mut RecoveryEvidence| e.facts_since_checkpoint = 1,
    ];
    for mutate in mutations {
        let mut evidence = eligible();
        mutate(&mut evidence);
        assert!(recovery_eligibility(&evidence).is_err());
    }
}

#[test]
fn executed_false_does_not_override_indeterminate_or_post_dispatch_phase() {
    for failure in [
        HostFailureEvidence::CancellationIndeterminate,
        HostFailureEvidence::PostDispatchIo,
    ] {
        let mut evidence = eligible();
        evidence.failure = failure;
        assert_eq!(evidence.receipt_reported_executed, Some(false));
        assert!(recovery_eligibility(&evidence).is_err());
    }
}

#[test]
fn rejects_stale_or_untrusted_workspace_evidence() {
    let mutations: [fn(&mut RecoveryEvidence); 7] = [
        |e: &mut RecoveryEvidence| e.workspace.prior_checkpoint_snapshot_valid = false,
        |e: &mut RecoveryEvidence| e.workspace.generation_known = false,
        |e: &mut RecoveryEvidence| e.workspace.snapshot_was_quiesced = false,
        |e: &mut RecoveryEvidence| e.workspace.current_activity_epoch += 1,
        |e: &mut RecoveryEvidence| e.workspace.current_host_instance += 1,
        |e: &mut RecoveryEvidence| e.durable_receipt_sequence = e.previous_checkpoint_sequence,
        |e: &mut RecoveryEvidence| e.next_model_request_started = false,
    ];
    for mutate in mutations {
        let mut evidence = eligible();
        mutate(&mut evidence);
        assert!(recovery_eligibility(&evidence).is_err());
    }
}

#[test]
fn rejects_missing_or_true_executed_receipt_flag() {
    for executed in [None, Some(true)] {
        let mut evidence = eligible();
        evidence.receipt_reported_executed = executed;
        assert!(recovery_eligibility(&evidence).is_err());
    }
}

#[test]
fn rejects_any_suffix_other_than_one_closed_bash_receipt() {
    let mutations: [fn(&mut RecoveryEvidence); 4] = [
        |e| e.tool_calls_since_checkpoint = 2,
        |e| e.receipts_since_checkpoint = 0,
        |e| e.all_suffix_tools_are_bash = false,
        |e| e.external_or_mcp_calls_since_checkpoint = 1,
    ];
    for mutate in mutations {
        let mut evidence = eligible();
        mutate(&mut evidence);
        assert!(recovery_eligibility(&evidence).is_err());
    }
}

#[test]
fn activity_witness_reuses_only_an_unchanged_quiesced_snapshot_epoch() {
    let witness = ExecutionActivityWitness::new(41);
    let token = witness.begin_snapshot_capture();
    let snapshot = witness
        .complete_quiesced_snapshot(token, true, true)
        .unwrap();
    assert_eq!(snapshot.snapshot_activity_epoch, 0);
    assert_eq!(witness.workspace_evidence(&snapshot), snapshot);

    assert_eq!(witness.invalidate_before_dispatch().unwrap(), 1);
    let after_dispatch = witness.workspace_evidence(&snapshot);
    assert_eq!(after_dispatch.snapshot_activity_epoch, 0);
    assert_eq!(after_dispatch.current_activity_epoch, 1);
    let mut evidence = eligible();
    evidence.workspace = after_dispatch;
    assert!(recovery_eligibility(&evidence).is_err());
}

#[test]
fn activity_witness_never_resets_when_a_process_exits() {
    let witness = ExecutionActivityWitness::new(42);
    witness.invalidate_before_dispatch().unwrap();
    witness.invalidate_before_dispatch().unwrap();
    let token = witness.begin_snapshot_capture();
    let snapshot = witness
        .complete_quiesced_snapshot(token, true, true)
        .unwrap();
    assert_eq!(snapshot.snapshot_activity_epoch, 2);
    witness.invalidate_before_dispatch().unwrap();
    assert_eq!(
        witness.workspace_evidence(&snapshot).current_activity_epoch,
        3
    );
}

#[test]
fn generation_only_snapshot_is_not_quiescence_evidence() {
    let witness = ExecutionActivityWitness::new(43);
    let snapshot = witness.capture_without_quiescence(true, true);
    assert!(!snapshot.snapshot_was_quiesced);
    let mut evidence = eligible();
    evidence.workspace = snapshot;
    assert!(recovery_eligibility(&evidence).is_err());
}

#[test]
fn equal_epoch_from_a_replacement_host_instance_is_rejected() {
    let old = ExecutionActivityWitness::new(51);
    let token = old.begin_snapshot_capture();
    let snapshot = old.complete_quiesced_snapshot(token, true, true).unwrap();
    let replacement = ExecutionActivityWitness::new(52);
    let mut evidence = eligible();
    evidence.workspace = replacement.workspace_evidence(&snapshot);
    assert_eq!(evidence.workspace.snapshot_activity_epoch, 0);
    assert_eq!(evidence.workspace.current_activity_epoch, 0);
    assert!(recovery_eligibility(&evidence).is_err());
}

#[test]
fn dispatch_during_snapshot_collection_rejects_capture() {
    use std::sync::{Arc, Barrier};
    use std::thread;

    let witness = Arc::new(ExecutionActivityWitness::new(61));
    let token = witness.begin_snapshot_capture();
    let dispatch = Arc::clone(&witness);
    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let child_started = Arc::clone(&started);
    let child_release = Arc::clone(&release);
    let child = thread::spawn(move || {
        child_started.wait();
        dispatch.invalidate_before_dispatch().unwrap();
        child_release.wait();
    });
    started.wait();
    release.wait();
    child.join().unwrap();
    assert!(witness
        .complete_quiesced_snapshot(token, true, true)
        .is_err());
}

#[test]
fn capture_rejects_foreign_host_even_at_the_same_epoch() {
    let old = ExecutionActivityWitness::new(71);
    let replacement = ExecutionActivityWitness::new(72);
    assert_eq!(
        replacement.complete_quiesced_snapshot(old.begin_snapshot_capture(), true, true),
        Err(SnapshotCaptureError::HostChanged)
    );
}
