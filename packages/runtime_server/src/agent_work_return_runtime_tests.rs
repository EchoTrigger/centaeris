//! Real Core terminal constructors and Runtime fences on API-owned work bindings.
use crate::contract::AgentRunStart;
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::session::reliability::*;
use centaeris_core::session::{AgentRunSessionState, RuntimeJobLeaseFence};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    database: Value,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    kind: String,
    agent_run_start: AgentRunStart,
}

#[test]
#[ignore = "requires API-owned work bindings in a fresh disposable PostgreSQL database"]
fn django_committed_work_return_facts() {
    let fixture: Fixture = serde_json::from_str(
        &std::env::var("CENTAERIS_WORK_RETURN_FIXTURE").expect("dedicated fixture required"),
    )
    .unwrap();
    let mut url = reqwest::Url::parse("postgresql://localhost/").unwrap();
    url.set_host(Some(fixture.database["HOST"].as_str().unwrap()))
        .unwrap();
    url.set_port(Some(
        fixture.database["PORT"].as_str().unwrap().parse().unwrap(),
    ))
    .unwrap();
    url.set_username(fixture.database["USER"].as_str().unwrap())
        .unwrap();
    url.set_password(Some(fixture.database["PASSWORD"].as_str().unwrap()))
        .unwrap();
    let database = fixture.database["NAME"].as_str().unwrap();
    assert!(database.starts_with("test_"));
    url.set_path(database);
    let store = PostgresRuntimeStore::new(url.as_str()).unwrap();
    for case in fixture.cases {
        let start = case.agent_run_start;
        let now = crate::now_ms().unwrap();
        let run = &start.agent_run_id;
        let session = &start.authorization.session_id;
        let job = format!("agent_run.lifecycle:{run}");
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest {
                job: RuntimeJobRecord {
                    job_id: job.clone(),
                    job_kind: "agent_run.lifecycle".into(),
                    status: RuntimeJobStatus::Queued,
                    run_at_ms: now,
                    lease_owner: None,
                    lease_expires_at_ms: None,
                    heartbeat_at_ms: None,
                    retry_count: 0,
                    max_retries: 0,
                    backoff_policy: RuntimeBackoffPolicy::default(),
                    idempotency_key: format!("{job}:{}", start.authorization_digest),
                    session_id: Some(session.clone()),
                    branch_id: None,
                    checkpoint_id: None,
                    payload_ref: Some(format!("record:agent_run:{run}")),
                    output_refs: vec![],
                    last_error: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
            })
            .unwrap();
        let owner = store
            .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: now,
                worker_id: "work-return-fixture".into(),
                job_id: Some(job.clone()),
                job_kind: None,
                session_id: None,
                limit: 1,
                lease_ms: 120_000,
            })
            .unwrap()
            .remove(0)
            .lease_owner
            .unwrap();
        store
            .start_runtime_job(StartRuntimeJobRequest {
                job_id: job.clone(),
                lease_owner: owner.clone(),
                started_at_ms: now,
            })
            .unwrap();
        let fence = RuntimeJobLeaseFence {
            job_id: job.clone(),
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: owner.clone(),
        };
        let stale = RuntimeJobLeaseFence {
            lease_owner: "stale-fixture-owner".into(),
            ..fence.clone()
        };
        let log = store.session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone(),
        );
        let mut sequence = AgentRunSessionState::new(session, run).unwrap();
        match case.kind.as_str() {
            "preAdmissionCancellation" => {
                assert!(log
                    .commit_pre_admission_cancellation(run, &start.authorization_digest, &fence)
                    .is_err());
                store
                    .request_agent_run_cancellation(run, session, &start.authorization_digest, now)
                    .unwrap();
                assert!(log
                    .commit_pre_admission_cancellation(run, &start.authorization_digest, &stale)
                    .is_err());
                for _ in 0..2 {
                    log.commit_pre_admission_cancellation(run, &start.authorization_digest, &fence)
                        .unwrap();
                }
            }
            "lifecycleFailure" => {
                store
                    .fail_runtime_job(FailRuntimeJobRequest {
                        job_id: job.clone(),
                        lease_owner: owner.clone(),
                        failed_at_ms: crate::now_ms().unwrap(),
                        last_error: "synthetic lifecycle failure".into(),
                        next_run_at_ms: None,
                        disposition: RuntimeJobFailureDisposition::DeadLettered,
                    })
                    .unwrap();
            }
            kind => {
                let initial = crate::started_session_records(&start, &mut sequence, now).unwrap();
                log.append_session_records_with_runtime_job_lease_blocking(run, &initial, &fence)
                    .unwrap();
                let text = crate::AssistantTextProjection::default();
                let terminal = match kind {
                    "completed" => {
                        let mut text = crate::AssistantTextProjection::default();
                        text.begin_model_request(start.turn_id.clone(), String::new())
                            .unwrap();
                        text.push_token(&start.turn_id, "untrusted work output")
                            .unwrap();
                        text.finish_model_request(&start.turn_id).unwrap();
                        crate::completed_session_records(
                            &start,
                            None,
                            &text,
                            &crate::AgentRunResult {
                                turn_responses: vec![],
                                stop: crate::AgentRunStop::Finalized,
                            },
                            &mut sequence,
                            crate::now_ms().unwrap(),
                        )
                        .unwrap()
                    }
                    "failed" => crate::failed_session_records(
                        &start,
                        "synthetic provider failure",
                        &text,
                        &mut sequence,
                        crate::now_ms().unwrap(),
                    )
                    .unwrap(),
                    "cancelled" => crate::interrupted_session_records(
                        &start,
                        &text,
                        &mut sequence,
                        crate::now_ms().unwrap(),
                        crate::Interruption {
                            execution_outcome: "cancelled",
                            reason_type: "cancelled",
                            message: "synthetic user cancellation",
                            retryable: false,
                        },
                    )
                    .unwrap(),
                    _ => panic!("unknown return fixture kind"),
                };
                assert!(log
                    .append_session_records_with_runtime_job_lease_blocking(run, &terminal, &stale)
                    .is_err());
                log.append_session_records_with_runtime_job_lease_blocking(run, &terminal, &fence)
                    .unwrap();
            }
        }
        if case.kind != "lifecycleFailure" {
            store
                .complete_runtime_job(CompleteRuntimeJobRequest {
                    job_id: job.clone(),
                    lease_owner: owner,
                    output_refs: vec![],
                    completed_at_ms: crate::now_ms().unwrap(),
                })
                .unwrap();
        }
        let pending = store.list_pending_runtime_job_outbox(100).unwrap();
        let outbox = pending.iter().find(|row| row.job_id == job).unwrap();
        store
            .mark_runtime_job_outbox_published(
                &job,
                &outbox.event_type,
                outbox.generation,
                crate::now_ms().unwrap(),
            )
            .unwrap();
        println!("work-return-fenced-fact-ok: {}", case.kind);
    }
    assert!(store
        .list_pending_runtime_job_outbox(100)
        .unwrap()
        .is_empty());
}
