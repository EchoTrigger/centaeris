use crate::execution_recovery_policy::{recovery_eligibility, RecoveryEvidence};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnedExecutionContinuity {
    OwnedRunning,
    OwnedMissing,
    OwnedStopped,
    IdentityMismatch,
    InspectUnavailable,
}

pub(crate) fn validate_sandbox_loss_recovery(
    evidence: &RecoveryEvidence,
    continuity: OwnedExecutionContinuity,
    reserved_attempts: u64,
    maximum_attempts: u64,
) -> Result<crate::execution_recovery_policy::CheckpointAdvance, String> {
    if !matches!(
        continuity,
        OwnedExecutionContinuity::OwnedMissing | OwnedExecutionContinuity::OwnedStopped
    ) {
        return Err("old Execution environment is not proven lost".to_string());
    }
    if reserved_attempts >= maximum_attempts {
        return Err("sandbox replacement recovery budget is exhausted".to_string());
    }
    recovery_eligibility(evidence).map_err(str::to_string)
}

#[cfg(test)]
mod tests;
