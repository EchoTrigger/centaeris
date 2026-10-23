//! Test-only lifecycle completion for dynamic-tool fixtures with no host allocation.
use crate::contract::{AgentRunCleanup, AgentRunStart};
use crate::postgres_store::cleanup_receipts::{
    CleanupAllocation, CleanupAuthorityRequest, CleanupReceiptQuery, CleanupTarget,
};
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::session::reliability::{CompleteRuntimeJobRequest, RuntimeJobStorePort};
use centaeris_core::session::RuntimeJobLeaseFence;
use serde_json::{json, Value};
use std::time::Duration;

fn completion(fence: &RuntimeJobLeaseFence, owner: &str) -> CompleteRuntimeJobRequest {
    CompleteRuntimeJobRequest {
        job_id: fence.job_id.clone(),
        lease_owner: owner.into(),
        output_refs: vec![],
        completed_at_ms: crate::now_ms().expect("fixture clock"),
    }
}

fn api_post(
    client: &reqwest::blocking::Client,
    api_url: &str,
    token: &str,
    path: &str,
    body: &Value,
) -> Result<Value, String> {
    let response = client
        .post(format!("{}{path}", api_url.trim_end_matches('/')))
        .header("X-Internal-Token", token)
        .json(body)
        .send()
        .map_err(|_| "hosted_fixture_lifecycle_transport")?;
    if !response.status().is_success() {
        return Err(format!(
            "hosted_fixture_lifecycle_http_{}",
            response.status().as_u16()
        ));
    }
    response
        .json()
        .map_err(|_| "hosted_fixture_lifecycle_response_invalid".into())
}

/// Keep Core terminal construction in each caller. This helper closes only the
/// real cleanup/release obligations; the caller still completes its own job.
pub(crate) fn confirm_unallocated_cleanup_and_release(
    store: &PostgresRuntimeStore,
    start: &AgentRunStart,
    fence: &RuntimeJobLeaseFence,
    api_url: &str,
    internal_token: &str,
    signing_key: &[u8],
) -> Result<(), String> {
    assert!(
        store
            .complete_runtime_job(completion(fence, &fence.lease_owner))
            .is_err(),
        "terminal records alone cannot complete a lifecycle job"
    );
    let stale_owner = format!("stale:{}", fence.lease_owner);
    assert!(
        store
            .complete_runtime_job(completion(fence, &stale_owner))
            .is_err(),
        "a stale lease cannot complete before cleanup"
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "hosted_fixture_lifecycle_client")?;
    let binding = json!({
        "schema":"runtime.agent_run_lifecycle.resolve.v1",
        "jobId":fence.job_id,
        "leaseOwner":fence.lease_owner,
        "agentRunId":start.agent_run_id,
        "authorizationDigest":start.authorization_digest,
        "executionMode":start.environment_authorization.payload.execution_mode,
    });
    let resolved = api_post(
        &client,
        api_url,
        internal_token,
        "/internal/agent-run-lifecycle/resolve",
        &binding,
    )?;
    if resolved["schema"] != "runtime.agent_run_lifecycle.resolved.v1"
        || resolved["disposition"] != "terminal"
    {
        return Err("hosted_fixture_terminal_cleanup_required".into());
    }
    let cleanup: AgentRunCleanup = serde_json::from_value(
        resolved
            .get("agentRunCleanup")
            .cloned()
            .ok_or("hosted_fixture_cleanup_missing")?,
    )
    .map_err(|_| "hosted_fixture_cleanup_invalid")?;
    if cleanup.agent_run_id != start.agent_run_id
        || cleanup.authorization_digest != start.authorization_digest
        || cleanup.authorization != start.authorization
    {
        return Err("hosted_fixture_cleanup_binding_rejected".into());
    }
    let receipt = store.with_cleanup_authority(
        &CleanupAuthorityRequest {
            cleanup: &cleanup,
            signing_key,
            lease: fence,
            host_id: "hosted-unallocated-fixture",
        },
        || Ok(vec![]),
        |target, _| {
            assert!(
                matches!(target, CleanupTarget::NeverAllocated),
                "dynamic-only fixtures cannot clean a physical allocation"
            );
            Ok(())
        },
    )?;
    assert_eq!(receipt.allocation, CleanupAllocation::NeverAllocated {});
    assert!(receipt.cleanup_complete);
    assert_eq!(
        store.get_cleanup_receipt(&CleanupReceiptQuery {
            schema: "runtime.agent_run.cleanup_receipt.get.v1".into(),
            agent_run_id: start.agent_run_id.clone(),
            authorization_digest: start.authorization_digest.clone(),
            cleanup_receipt_id: receipt.receipt_id.clone(),
        })?,
        Some(receipt.clone()),
        "release must consume a durable Store receipt",
    );
    assert!(
        store
            .complete_runtime_job(completion(fence, &fence.lease_owner))
            .is_err(),
        "a cleanup receipt cannot replace controller release"
    );
    let released = api_post(
        &client,
        api_url,
        internal_token,
        "/internal/agent-run-lifecycle/release",
        &json!({
            "schema":"runtime.agent_run_lifecycle.release.v1",
            "jobId":fence.job_id,
            "leaseOwner":fence.lease_owner,
            "agentRunId":start.agent_run_id,
            "authorizationDigest":start.authorization_digest,
            "controllerEpoch":receipt.controller_epoch,
            "cleanupReceiptId":receipt.receipt_id,
        }),
    )?;
    if released["schema"] != "runtime.agent_run_lifecycle.released.v1"
        || released["agentRunId"] != start.agent_run_id
        || released["releaseReceiptId"]
            .as_str()
            .is_none_or(|id| id.is_empty())
    {
        return Err("hosted_fixture_release_invalid".into());
    }
    store.verify_lifecycle_completion(&fence.job_id)?;
    // Now the lifecycle receipts are valid: this refusal must preserve the
    // worker lease fence rather than merely failing the earlier cleanup guard.
    assert!(
        store
            .complete_runtime_job(completion(fence, &stale_owner))
            .is_err(),
        "a stale lease still cannot complete after release"
    );
    Ok(())
}
