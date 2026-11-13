//! Stop, capture and publish a per-Run original workspace before its cleanup.
use super::*;
use crate::postgres_store::cleanup_preservation::{CleanupRecovery, CleanupRecoveryEvidence};
use crate::postgres_store::cleanup_receipts::{CleanupTarget, CleanupWorkspaceProtection};
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use centaeris_core::session::RuntimeJobLeaseFence;

const ARCHIVE_METADATA_BUDGET: u64 = 32 * 1024 * 1024;

enum RecoveryContent {
    Captured(Box<ImmutableWorkspaceSnapshot>),
    Absent,
    PreAdmissionFailed,
}

pub(crate) struct PreparedStoppedRecovery {
    allocation: RetainedInstanceAllocation,
    content: RecoveryContent,
}

/// Called only inside the current cleanup target's authorization pin.
pub(crate) fn prepare(target: &CleanupTarget) -> Result<Option<PreparedStoppedRecovery>, String> {
    let CleanupTarget::Instance {
        allocation,
        origin_authorization,
        workspace_protection,
        ..
    } = target
    else {
        return Ok(None);
    };
    prepare_instance_with(
        allocation,
        &origin_authorization.environment_profile,
        workspace_protection,
        runsc_power::stop_for_preservation,
        stopped_workspace::capture_stopped_workspace,
    )
}

fn prepare_instance_with(
    allocation: &RetainedInstanceAllocation,
    profile: &EnvironmentProfile,
    workspace_protection: &CleanupWorkspaceProtection,
    observe: impl FnOnce(
        &RetainedInstanceAllocation,
        &EnvironmentProfile,
    ) -> Result<Option<runsc_power::RunscPowerObservation>, String>,
    capture: impl FnOnce(
        &RetainedInstanceAllocation,
        &EnvironmentProfile,
        u64,
    ) -> Result<ImmutableWorkspaceSnapshot, String>,
) -> Result<Option<PreparedStoppedRecovery>, String> {
    let content = match observe(allocation, profile)? {
        Some(
            runsc_power::RunscPowerObservation::Running
            | runsc_power::RunscPowerObservation::Paused,
        ) => return Err("cleanup_stop_unconfirmed".into()),
        Some(runsc_power::RunscPowerObservation::Stopped) => {
            if matches!(
                workspace_protection,
                CleanupWorkspaceProtection::PreAdmissionFailed
            ) {
                return Ok(Some(PreparedStoppedRecovery {
                    allocation: allocation.clone(),
                    content: RecoveryContent::PreAdmissionFailed,
                }));
            }
            let limit = profile
                .resources
                .workspace_bytes
                .checked_add(ARCHIVE_METADATA_BUDGET)
                .ok_or("stopped_workspace_archive_budget_overflow")?;
            RecoveryContent::Captured(Box::new(capture(allocation, profile, limit)?))
        }
        None => RecoveryContent::Absent,
    };
    Ok(Some(PreparedStoppedRecovery {
        allocation: allocation.clone(),
        content,
    }))
}

impl PreparedStoppedRecovery {
    /// The capture transaction has ended before HTTP publication starts. A lost
    /// response leaves the exact original allocation charged and retryable.
    pub(crate) fn publish(
        mut self,
        api_url: &str,
        api_token: &str,
        cleanup: &crate::contract::AgentRunCleanup,
        lease: &RuntimeJobLeaseFence,
    ) -> Result<CleanupRecovery, String> {
        let evidence = match &mut self.content {
            RecoveryContent::Captured(artifact) => {
                let proof = stopped_preservation::upload_stopped_workspace(
                    api_url,
                    api_token,
                    cleanup,
                    lease,
                    &self.allocation,
                    artifact,
                )?;
                CleanupRecoveryEvidence::Preserved {
                    receipt_id: proof.preservation_receipt_id,
                    workspace: proof.session_workspace,
                }
            }
            RecoveryContent::Absent => CleanupRecoveryEvidence::Absent,
            RecoveryContent::PreAdmissionFailed => CleanupRecoveryEvidence::PreAdmissionFailed,
        };
        Ok(CleanupRecovery {
            expected_allocation: self.allocation,
            evidence,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_admission_stops_without_capturing_a_partial_restore() {
        let (allocation, profile) = fixture();
        let prepared = prepare_instance_with(
            &allocation,
            &profile,
            &CleanupWorkspaceProtection::PreAdmissionFailed,
            |_, _| Ok(Some(runsc_power::RunscPowerObservation::Stopped)),
            |_, _, _| panic!("a partial restore must never become a published workspace"),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            prepared.content,
            RecoveryContent::PreAdmissionFailed
        ));
        assert!(prepare_instance_with(
            &allocation,
            &profile,
            &CleanupWorkspaceProtection::PreAdmissionFailed,
            |_, _| Err("stop outcome unknown".into()),
            |_, _, _| panic!("failed admission still requires a confirmed stop"),
        )
        .is_err());
    }
    use centaeris_core::execution::EnvironmentTaskOwner;

    fn fixture() -> (RetainedInstanceAllocation, EnvironmentProfile) {
        let profile = serde_json::from_value(serde_json::json!({
            "schema":"runtime.environment_profile.v1", "imageDigest":format!("sha256:{}","a".repeat(64)),
            "workspaceRoot":"/mnt/data", "user":"10001:10001", "network":"disabled",
            "resources":{"cpuMilli":2000,"memoryBytes":2147483648u64,"pidsLimit":512,
                "workspaceBytes":4294967296u64,"storageBytes":4294967296u64}
        })).unwrap();
        let allocation = RetainedInstanceAllocation {
            allocation_execution_id: "execution:origin".into(),
            allocation_agent_run_id: Some("origin".into()),
            allocation_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "origin".into(),
            },
            instance_id: format!("instance_{}", "b".repeat(64)),
            environment_id: "environment".into(),
            host_id: "host".into(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            authorization_digest: format!("sha256:{}", "c".repeat(64)),
            binding_digest: format!("sha256:{}", "d".repeat(64)),
            profile_digest: format!("sha256:{}", "e".repeat(64)),
            container_id: Some("f".repeat(64)),
            reused: true,
        };
        (allocation, profile)
    }

    #[test]
    fn unproven_old_private_layer_remains_pending_for_explicit_manual_recovery() {
        let (allocation, profile) = fixture();
        let result = prepare_instance_with(
            &allocation,
            &profile,
            &CleanupWorkspaceProtection::Required,
            |_, _| Ok(Some(runsc_power::RunscPowerObservation::Stopped)),
            |_, _, _| Err("stopped_workspace_backing_unproven".into()),
        );
        assert!(matches!(result, Err(ref error) if error == "stopped_workspace_backing_unproven"));
    }

    #[test]
    fn ordinary_capture_failures_never_turn_into_an_empty_or_older_baseline() {
        let (allocation, profile) = fixture();
        for reason in [
            "archive_timeout",
            "stopped_workspace_root_missing",
            "runsc_power_observation_unknown",
        ] {
            let result = prepare_instance_with(
                &allocation,
                &profile,
                &CleanupWorkspaceProtection::Required,
                |_, _| Ok(Some(runsc_power::RunscPowerObservation::Stopped)),
                |_, _, _| Err(reason.into()),
            );
            assert!(matches!(result,Err(ref error) if error==reason));
        }
    }

    #[test]
    fn unconfirmed_stop_and_exact_absence_do_not_read_an_unrelated_archive() {
        let (allocation, profile) = fixture();
        for state in [
            runsc_power::RunscPowerObservation::Running,
            runsc_power::RunscPowerObservation::Paused,
        ] {
            assert!(prepare_instance_with(
                &allocation,
                &profile,
                &CleanupWorkspaceProtection::Required,
                |_, _| Ok(Some(state)),
                |_, _, _| panic!("live or paused target must not be archived")
            )
            .is_err());
        }
        let absent = prepare_instance_with(
            &allocation,
            &profile,
            &CleanupWorkspaceProtection::Required,
            |_, _| Ok(None),
            |_, _, _| panic!("absence is not an empty archive"),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(absent.content, RecoveryContent::Absent));
        let captured = prepare_instance_with(
            &allocation,
            &profile,
            &CleanupWorkspaceProtection::Required,
            |_, _| Ok(Some(runsc_power::RunscPowerObservation::Stopped)),
            |_, _, _| capture_snapshot_artifact(&mut Cursor::new(Vec::new()), 1),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(captured.content, RecoveryContent::Captured(_)));
    }
}
