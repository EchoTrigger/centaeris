//! Completion is historical preparation evidence, never current residency.
use super::*;
use crate::e2b_runtime::{E2bProvisionOutcome, E2bRuntime, E2bRuntimeError};
use crate::postgres_store::provider_operations::ProviderCreationDispatchState;
use centaeris_core::execution::EnvironmentTaskOwner;

pub(super) fn provision(
    provider: Option<&E2bRuntime>,
    authorization: &ProvisionAuthorization,
    key: &[u8],
) -> Result<Response<Body>, String> {
    let Some(provider) = provider else {
        return json_error_response(503, "e2b_provision_not_configured");
    };
    match provider.provision(authorization, key) {
        Ok(outcome) => response(outcome),
        Err(E2bRuntimeError::Rejected(code)) => json_error_response(409, code),
        Err(E2bRuntimeError::Unavailable(code) | E2bRuntimeError::Unknown(code)) => {
            json_error_response(503, code)
        }
    }
}
fn response(outcome: E2bProvisionOutcome) -> Result<Response<Body>, String> {
    let (status, value) = match outcome {
        E2bProvisionOutcome::Completed(receipt) => (
            200,
            json!({
                "schema":"runtime.provider_environment.provision.result.v1",
                "status":"completed","operationId":receipt.operation_id,
                "environmentId":receipt.environment_id,"creationAttemptId":receipt.creation_attempt_id,
                "instanceId":receipt.instance_id,"sandboxId":receipt.sandbox_id,
                "profileDigest":receipt.profile_digest,"preparationDigest":receipt.preparation_digest,
            }),
        ),
        E2bProvisionOutcome::Pending(operation) => {
            let EnvironmentTaskOwner::MaintenanceOperation { operation_id } =
                &operation.identity.owner
            else {
                return json_error_response(503, "e2b_provision_origin_unconfirmed");
            };
            let dispatch = match operation.dispatch_state {
                ProviderCreationDispatchState::Prepared => "prepared",
                ProviderCreationDispatchState::Dispatched => "dispatched",
                ProviderCreationDispatchState::NotDispatched => "notDispatched",
            };
            (
                202,
                json!({
                    "schema":"runtime.provider_environment.provision.result.v1",
                    "status":"pending","operationId":operation_id,
                    "environmentId":operation.identity.environment_id,"creationAttemptId":operation.identity.attempt_id,
                    "instanceId":operation.identity.instance_id,"profileDigest":operation.identity.profile_digest,"dispatchState":dispatch,
                }),
            )
        }
    };
    Ok(http_response(
        status,
        "application/json",
        serde_json::to_vec(&value).map_err(|_| "e2b_provision_response_unavailable")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres_store::provider_operations::provision::ProviderProvisionCompletion;
    use crate::postgres_store::provider_operations::{
        ProviderCreationIdentity, ProviderCreationOperation,
    };
    use serde_json::Value;
    fn decoded(response: Response<Body>) -> (u16, Value) {
        let status = response.status().as_u16();
        let bytes = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(axum::body::to_bytes(response.into_body(), 65536))
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    #[test]
    fn completion_returns_preparation_evidence_without_claiming_current_residency() {
        let receipt = ProviderProvisionCompletion {
            operation_id: "op".into(),
            creation_attempt_id: "attempt".into(),
            environment_id: "environment".into(),
            session_id: "session".into(),
            workspace_id: "workspace".into(),
            instance_id: "instance".into(),
            sandbox_id: "sandbox".into(),
            profile_digest: "profile".into(),
            provider_config_digest: "config".into(),
            authorization_digest: "auth".into(),
            controller_epoch: 1,
            controller_lease_digest: "lease".into(),
            request_digest: "request".into(),
            preparation_digest: "prepared".into(),
            recorded_at_ms: 1,
        };
        let (status, value) =
            decoded(response(E2bProvisionOutcome::Completed(Box::new(receipt))).unwrap());
        assert_eq!(status, 200);
        assert_eq!(
            value,
            json!({"schema":"runtime.provider_environment.provision.result.v1","status":"completed",
            "operationId":"op","environmentId":"environment","creationAttemptId":"attempt","instanceId":"instance",
            "sandboxId":"sandbox","profileDigest":"profile","preparationDigest":"prepared"})
        );
    }
    #[test]
    fn unverified_candidate_stays_pending_and_does_not_become_an_owned_target() {
        let identity = ProviderCreationIdentity {
            attempt_id: "attempt".into(),
            execution_id: "execution".into(),
            instance_id: "instance".into(),
            environment_id: "environment".into(),
            session_id: "session".into(),
            workspace_id: "workspace".into(),
            owner: EnvironmentTaskOwner::MaintenanceOperation {
                operation_id: "op".into(),
            },
            authorization_digest: "auth".into(),
            profile_digest: "profile".into(),
            provider_config_digest: "config".into(),
            request_digest: "request".into(),
            idempotency_key: "key".into(),
            processing_job_id: None,
        };
        let op = ProviderCreationOperation {
            identity,
            dispatch_state: ProviderCreationDispatchState::Dispatched,
            reported_sandbox_id: Some("candidate".into()),
            recorded_at_ms: 1,
        };
        let (status, value) =
            decoded(response(E2bProvisionOutcome::Pending(Box::new(op))).unwrap());
        assert_eq!(status, 202);
        assert_eq!(
            value,
            json!({"schema":"runtime.provider_environment.provision.result.v1","status":"pending",
            "operationId":"op","environmentId":"environment","creationAttemptId":"attempt","instanceId":"instance","profileDigest":"profile","dispatchState":"dispatched"})
        );
    }
}
