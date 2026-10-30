//! Finish dynamic-only API fixtures through real cleanup and release authority.
use crate::contract::{AgentRunCleanup, AgentRunStart};
use crate::postgres_store::cleanup_receipts::{
    CleanupAllocation, CleanupAuthorityRequest, CleanupTarget,
};
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::session::reliability::{
    agent_run_lifecycle_job_id, CompleteRuntimeJobRequest, RuntimeJobRecord, RuntimeJobStorePort,
    StartRuntimeJobRequest, AGENT_RUN_LIFECYCLE_JOB_KIND,
};
use centaeris_core::session::RuntimeJobLeaseFence;
use serde_json::{json, Value};
use std::time::Duration;

/// Build a hosted worker lifecycle through the same scheduling and claim protocol.
pub(crate) fn start_lifecycle_job(
    store: &PostgresRuntimeStore,
    start: &AgentRunStart,
    worker_id: &str,
) -> RuntimeJobLeaseFence {
    let now = crate::now_ms().unwrap();
    let job_id = agent_run_lifecycle_job_id(&start.agent_run_id).unwrap();
    let schedule = serde_json::to_vec(&json!({
        "schema": "runtime.job.schedule.v1",
        "jobId": job_id,
        "jobKind": AGENT_RUN_LIFECYCLE_JOB_KIND,
        "runAtMs": now,
        "maxRetries": 0,
        "idempotencyKey": format!("{job_id}:{}", start.authorization_digest),
        "sessionId": start.authorization.session_id,
        "payloadRef": format!("record:agent_run:{}", start.agent_run_id),
        "workspaceId": start.authorization.workspace_id,
    }))
    .unwrap();
    let scheduled = crate::job_protocol::schedule(&schedule, store).unwrap();
    assert_eq!(scheduled["disposition"], "inserted");
    // A replay must retain both the original Job and its execution tenant.
    let replayed = crate::job_protocol::schedule(&schedule, store).unwrap();
    assert_eq!(replayed["disposition"], "existing");
    assert_eq!(replayed["job"], scheduled["job"]);
    let claimed = crate::job_protocol::claim(
        &serde_json::to_vec(&json!({
            "schema": "runtime.job.claim.v1",
            "workerId": worker_id,
            "jobId": job_id,
            "jobKind": AGENT_RUN_LIFECYCLE_JOB_KIND,
            "nowMs": now,
            "leaseMs": 120_000,
            "limit": 1,
        }))
        .unwrap(),
        store,
    )
    .unwrap();
    let mut jobs: Vec<RuntimeJobRecord> = serde_json::from_value(claimed["jobs"].clone()).unwrap();
    assert_eq!(jobs.len(), 1, "the hosted lifecycle Job must be claimable");
    let job = jobs.remove(0);
    assert_eq!(job.job_id, job_id);
    assert_eq!(job.job_kind, AGENT_RUN_LIFECYCLE_JOB_KIND);
    let fence = RuntimeJobLeaseFence {
        job_id: job.job_id,
        job_kind: job.job_kind,
        lease_owner: job.lease_owner.unwrap(),
    };
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: fence.job_id.clone(),
            lease_owner: fence.lease_owner.clone(),
            started_at_ms: now,
        })
        .unwrap();
    fence
}

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
