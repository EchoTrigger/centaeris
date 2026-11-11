//! Controlled latency fixtures; all transitions use the production store ports.
use super::*;
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, RuntimeAgentRunIdentityV1, RuntimeAwaitJobCheckpointV1,
    RuntimeJobWaitV1,
};
use centaeris_core::session::store::RuntimeStore;

pub(super) fn seed(store: &PostgresRuntimeStore, wire: &str) -> Value {
    let index: u64 = serde_json::from_str(wire).unwrap();
    let agent = format!("measurement_agent_{index}");
    let session = format!("measurement_session_{index}");
    let turn = format!("measurement_turn_{index}");
    let parent = agent_run_lifecycle_job_id(&agent).unwrap();
    let source = format!("measurement:source:{index}");
    let now = now_ms().unwrap();
    let record = |id: String, kind: &str| RuntimeJobRecord {
        job_id: id.clone(),
        job_kind: kind.into(),
        status: RuntimeJobStatus::Queued,
        run_at_ms: now + 300_000,
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries: 0,
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key: id,
        session_id: None,
        branch_id: None,
        checkpoint_id: None,
        payload_ref: None,
        output_refs: vec![],
        last_error: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    let mut parent_job = record(parent.clone(), "agent_run.lifecycle");
    parent_job.session_id = Some(session.clone());
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: parent_job })
        .unwrap();
    let mut source_job = record(source.clone(), "contract.test");
    source_job.status = RuntimeJobStatus::Running;
    source_job.lease_owner = Some("measurement-source-owner".into());
    source_job.lease_expires_at_ms = Some(now + 60_000);
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: source_job })
        .unwrap();
    let digest = format!("sha256:{}", "a".repeat(64));
    let wait = RuntimeAwaitJobCheckpointV1::new(
        &RuntimeAgentRunIdentityV1 {
            agent_run_id: agent,
            execution_id: format!("measurement_execution_{index}"),
            authorization_digest: digest.clone(),
        },
        &turn,
        vec![RuntimeJobWaitV1 {
            tool_call_id: format!("measurement_call_{index}"),
            source_tool_name: "read_result".into(),
            tool_definition_digest: digest,
            job_id: source.clone(),
            job_kind: "contract.test".into(),
        }],
    )
    .unwrap();
    store
        .save_checkpoint(CheckpointRecord {
            checkpoint_id: format!("measurement_checkpoint_{index}"),
            kind: CheckpointKindV1::Wait,
            session_id: session,
            turn_id: turn,
            status: "waiting".into(),
            done_reason: Some("runtime_job".into()),
            updated_at_ms: now,
            payload_json: serde_json::to_string(&wait).unwrap(),
        })
        .unwrap();
    json!({"parentJobId":parent,"sourceJobId":source})
}

pub(super) fn complete(store: &PostgresRuntimeStore, source: &str) {
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: source.into(),
            lease_owner: "measurement-source-owner".into(),
            output_refs: vec![],
            completed_at_ms: now_ms().unwrap(),
        })
        .unwrap();
}
