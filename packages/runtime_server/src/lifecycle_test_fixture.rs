//! Finish dynamic-only API fixtures through real cleanup and release authority.
use crate::contract::{AgentRunCleanup, AgentRunStart};
use crate::postgres_store::cleanup_receipts::{
    CleanupAllocation, CleanupAuthorityRequest, CleanupTarget,
};
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::session::reliability::{CompleteRuntimeJobRequest, RuntimeJobStorePort};
use centaeris_core::session::RuntimeJobLeaseFence;
use serde_json::{json, Value};
use std::time::Duration;

pub(crate) fn cleanup_and_release_unallocated_run(
    store: &PostgresRuntimeStore,
    start: &AgentRunStart,
    fence: &RuntimeJobLeaseFence,
    api_url: &str,
    internal_token: &str,
) {
    assert_eq!(
        store
            .complete_runtime_job(CompleteRuntimeJobRequest {
                job_id: fence.job_id.clone(),
                lease_owner: fence.lease_owner.clone(),
                output_refs: vec![],
                completed_at_ms: crate::now_ms().unwrap(),
            })
            .unwrap_err(),
        "lifecycle cleanup and release receipts required",
        "Core terminal records alone must not complete the lifecycle Job"
    );

    // These fixtures run dynamic tools and never allocate an OCI environment.
    // Fail before inventory reconciliation if this Session gains any physical
    // responsibility, including an allocation owned by another Run.
    store
        .with_client(|db| {
            let row = db
                .query_one(
                    "SELECT (SELECT count(*) FROM runtime.execution_instances WHERE session_id=$1 OR environment_id=$2) + (SELECT count(*) FROM runtime.resident_sandboxes WHERE environment_id=$2)",
                    &[&start.authorization.session_id, &start.environment_authorization.payload.environment_id],
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(row.get::<_, i64>(0), 0, "fixture must remain physically unallocated");
            Ok(())
        })
        .unwrap();

    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let post = |path: &str, body: &Value, expected_status: u16| {
        let response = client
            .post(format!("{}{path}", api_url.trim_end_matches('/')))
            .header("X-Internal-Token", internal_token)
            .json(body)
            .send()
            .unwrap();
        let status = response.status().as_u16();
        let result: Value = response.json().unwrap();
        assert_eq!(status, expected_status, "{path}: {result}");
        result
    };
    let resolved = post(
        "/internal/agent-run-lifecycle/resolve",
        &json!({
            "schema": "runtime.agent_run_lifecycle.resolve.v1",
            "jobId": fence.job_id,
            "leaseOwner": fence.lease_owner,
            "agentRunId": start.agent_run_id,
            "authorizationDigest": start.authorization_digest,
            "executionMode": start.environment_authorization.payload.execution_mode,
        }),
        200,
    );
    assert_eq!(resolved["disposition"], "terminal");
    let cleanup: AgentRunCleanup =
        serde_json::from_value(resolved["agentRunCleanup"].clone()).unwrap();
    let epoch = cleanup
        .environment_authorization
        .payload
        .controller
        .as_ref()
        .unwrap()
        .controller_epoch;
    let mut release = json!({
        "schema": "runtime.agent_run_lifecycle.release.v1",
        "jobId": fence.job_id,
        "leaseOwner": fence.lease_owner,
        "agentRunId": start.agent_run_id,
        "authorizationDigest": start.authorization_digest,
        "controllerEpoch": epoch,
        "cleanupReceiptId": "missing-fixture-cleanup-receipt",
    });
    let rejected = post("/internal/agent-run-lifecycle/release", &release, 503);
    assert_eq!(rejected["error"], "agent_run_cleanup_receipt_unavailable");

    let signing_key = std::env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY").unwrap();
    let host_id = format!("dynamic-only-fixture:{}", start.agent_run_id);
    let receipt = store
        .with_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: signing_key.as_bytes(),
                lease: fence,
                host_id: &host_id,
            },
            || Ok(vec![]),
            |target, _| {
                assert!(
                    matches!(target, CleanupTarget::NeverAllocated),
                    "dynamic-only fixtures must never clean a physical instance"
                );
                Ok::<(), String>(())
            },
        )
        .unwrap();
    assert_eq!(receipt.allocation, CleanupAllocation::NeverAllocated {});
    assert!(receipt.cleanup_complete);
    // A persisted receipt for this Run does not authorize another receipt ID.
    let rejected = post("/internal/agent-run-lifecycle/release", &release, 503);
    assert_eq!(rejected["error"], "agent_run_cleanup_receipt_unavailable");
    release["cleanupReceiptId"] = json!(receipt.receipt_id);
    let released = post("/internal/agent-run-lifecycle/release", &release, 200);
    assert_eq!(released["agentRunId"], start.agent_run_id);
    assert!(released["releaseReceiptId"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    store.verify_lifecycle_completion(&fence.job_id).unwrap();
}
