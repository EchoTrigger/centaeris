//! Typed creation provenance shares Core ownership without inventing a Job.
pub(crate) mod schema;
use centaeris_core::execution::EnvironmentTaskOwner;
use std::collections::BTreeMap;

pub(crate) fn owner_metadata(
    owner: &EnvironmentTaskOwner,
    job: Option<&str>,
) -> Result<BTreeMap<String, String>, String> {
    super::validate_identity(owner.id())?;
    if let Some(job) = job {
        super::validate_identity(job)?;
    }
    match (owner, job) {
        (EnvironmentTaskOwner::AgentRun { run_id }, Some(job))
            if !run_id.is_empty() && !job.is_empty() =>
        {
            Ok(BTreeMap::from([
                ("agentRunId".into(), run_id.clone()),
                ("processingJobId".into(), job.into()),
            ]))
        }
        (EnvironmentTaskOwner::MaintenanceOperation { operation_id }, None)
            if !operation_id.is_empty() =>
        {
            Ok(BTreeMap::from([
                ("creationOwnerKind".into(), "maintenanceOperation".into()),
                ("maintenanceOperationId".into(), operation_id.clone()),
            ]))
        }
        _ => Err("provider_creation_owner_rejected".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_creation_origin_preserves_agent_run_metadata_bytes() {
        let metadata = owner_metadata(
            &EnvironmentTaskOwner::AgentRun {
                run_id: "run-original".into(),
            },
            Some("agent_run.lifecycle:run-original"),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&metadata).unwrap(),
            r#"{"agentRunId":"run-original","processingJobId":"agent_run.lifecycle:run-original"}"#
        );
    }
    #[test]
    fn provider_creation_origin_maintenance_has_real_operation_without_run_or_job() {
        let metadata = owner_metadata(
            &EnvironmentTaskOwner::MaintenanceOperation {
                operation_id: "provision-original".into(),
            },
            None,
        )
        .expect("independent provision must not require a synthetic Run or Job");
        assert_eq!(
            serde_json::to_string(&metadata).unwrap(),
            r#"{"creationOwnerKind":"maintenanceOperation","maintenanceOperationId":"provision-original"}"#
        );
    }
    #[test]
    fn provider_creation_origin_rejects_mixed_or_missing_job_responsibility() {
        assert!(owner_metadata(
            &EnvironmentTaskOwner::AgentRun {
                run_id: "run-original".into()
            },
            None
        )
        .is_err());
        assert!(owner_metadata(
            &EnvironmentTaskOwner::MaintenanceOperation {
                operation_id: "provision-original".into()
            },
            Some("invented-job")
        )
        .is_err());
    }
}

#[cfg(test)]
pub(super) fn legacy_test_identity() -> super::ProviderCreationIdentity {
    super::ProviderCreationIdentity {
        attempt_id: "attempt".into(),
        execution_id: "execution".into(),
        instance_id: "instance".into(),
        environment_id: "environment".into(),
        session_id: "session".into(),
        workspace_id: "workspace".into(),
        owner: EnvironmentTaskOwner::AgentRun {
            run_id: "run-original".into(),
        },
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
        profile_digest: format!("sha256:{}", "b".repeat(64)),
        provider_config_digest: format!("sha256:{}", "c".repeat(64)),
        request_digest: format!("sha256:{}", "d".repeat(64)),
        idempotency_key: "key".into(),
        processing_job_id: Some("agent_run.lifecycle:run-original".into()),
    }
}
#[cfg(test)]
#[test]
fn provider_creation_origin_preserves_complete_legacy_metadata_and_verification_digest() {
    let identity = legacy_test_identity();
    assert_eq!(
        serde_json::to_string(&super::cleanup::expected_metadata(&identity)).unwrap(),
        r#"{"agentRunId":"run-original","authorizationDigest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","centaerisAttemptId":"attempt","environmentId":"environment","executionId":"execution","instanceId":"instance","processingJobId":"agent_run.lifecycle:run-original","profileDigest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","sessionId":"session","workspaceId":"workspace"}"#
    );
    assert_eq!(
        super::cleanup::target_verification_digest(
            &identity,
            "template",
            &identity.provider_config_digest,
            "sandbox"
        )
        .unwrap(),
        "sha256:afae5c30e32925a0877778177aa876364f4a118390abe78ccd9bbeb3ca7d70fb"
    );
}
