use super::*;
use crate::execution_recovery_policy::{
    HostFailureEvidence, RecoveryEvidence, WorkspaceReuseEvidence,
};
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
            snapshot_activity_epoch: 3,
            current_activity_epoch: 3,
            snapshot_host_instance: 71,
            current_host_instance: 71,
        },
    }
}

#[test]
fn missing_and_stopped_hosts_allow_only_the_five_reserved_attempts() {
    for continuity in [
        OwnedExecutionContinuity::OwnedMissing,
        OwnedExecutionContinuity::OwnedStopped,
    ] {
        for reserved in 0..5 {
            let advance =
                validate_sandbox_loss_recovery(&eligible(), continuity, reserved, 5).unwrap();
            assert_eq!(advance.covered_through_sequence, 8);
        }
        assert!(validate_sandbox_loss_recovery(&eligible(), continuity, 5, 5).is_err());
    }
}

#[test]
fn running_unowned_and_unavailable_hosts_cannot_authorize_replacement() {
    for continuity in [
        OwnedExecutionContinuity::OwnedRunning,
        OwnedExecutionContinuity::IdentityMismatch,
        OwnedExecutionContinuity::InspectUnavailable,
    ] {
        assert!(validate_sandbox_loss_recovery(&eligible(), continuity, 0, 5).is_err());
    }
}
