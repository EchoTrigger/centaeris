//! Cleanup recovery never grants another execution attempt or replaces the Job.
use super::retained_instance_tests as fixture;
use super::*;

fn claim(store: &PostgresRuntimeStore, id: &str, stage: &str) -> Vec<RuntimeJobRecord> {
    store
        .claim_control_jobs(
            ClaimDueRuntimeJobsRequest {
                now_ms: crate::now_ms().unwrap(),
                worker_id: "worker:cleanup-recovery-test".into(),
                job_id: Some(id.into()),
                job_kind: Some("agent_run.lifecycle".into()),
                session_id: None,
                limit: 1,
                lease_ms: 60_000,
            },
            stage,
        )
        .unwrap()
}

fn failed_cleanup(store: &PostgresRuntimeStore, db: &mut Client, status: &str) -> fixture::Run {
    db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL); DELETE FROM public.app_core_agentrunenvironmentrelease").unwrap();
    let run = fixture::run(store, db, "cleanup-recovery-run", 3);
    let log = store.session_log("ws_1".into(), "sess_1".into(), "cleanup-test".into());
    log.commit_pre_admission_failure(
        &run.original.agent_run_id,
        &run.original.digest().unwrap(),
        &run.fence,
    )
    .unwrap();
    super::super::lifecycle_control::mark_terminal_pending(db, &run.original.agent_run_id).unwrap();
    db.execute(
        "UPDATE public.app_core_agentrun SET status='failed' WHERE id=$1",
        &[&run.original.agent_run_id],
    )
    .unwrap();
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE controller_run_id=$1", &[&run.original.agent_run_id]).unwrap();
    db.execute("UPDATE runtime.runtime_jobs SET status=$2,retry_count=11,max_retries=10,last_error='retained_cleanup_pending',lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=$1", &[&run.fence.job_id,&status]).unwrap();
    db.execute("INSERT INTO runtime.dead_letters(dead_letter_id,original_job_id,job_kind,status,session_id,idempotency_key,failure_reason,last_error,attempts,first_failed_at_ms,last_failed_at_ms,replay_policy_json,updated_at_ms) SELECT 'cleanup-dead-letter',job_id,job_kind,'open',session_id,idempotency_key,'retry_budget_exhausted',last_error,retry_count,updated_at_ms,updated_at_ms,'{}',updated_at_ms FROM runtime.runtime_jobs WHERE job_id=$1", &[&run.fence.job_id]).unwrap();
    run
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_cleanup_recovery_claims_original_failed_job_without_execution_or_retry_reset() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for status in ["failed", "dead_lettered"] {
        let (url, store, mut db) = fixture::setup();
        let run = failed_cleanup(&store, &mut db, status);
        let history: String = db
            .query_one(
                "SELECT row_to_json(d)::text FROM runtime.dead_letters d WHERE original_job_id=$1",
                &[&run.fence.job_id],
            )
            .unwrap()
            .get(0);
        let terminal: String = db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_id='agent_run_pre_admission_failure:'||$1", &[&run.original.agent_run_id]).unwrap().get(0);
        assert!(claim(&store, &run.fence.job_id, "advance").is_empty());
        assert!(
            store.control_ready("cleanup").unwrap(),
            "outstanding cleanup must be discoverable after a terminal Job failure"
        );
        let claimed = claim(&store, &run.fence.job_id, "cleanup");
        assert_eq!(
            claimed.len(),
            1,
            "canonical terminal Job must recover exclusively into cleanup"
        );
        let job = &claimed[0];
        assert_eq!(job.job_id, run.fence.job_id);
        assert_eq!((job.retry_count, job.max_retries), (11, 10));
        assert_eq!(job.last_error.as_deref(), Some("retained_cleanup_pending"));
        let owner = job.lease_owner.as_ref().unwrap();
        assert_ne!(owner, &run.fence.lease_owner);
        assert!(store.require_execute_lease(&job.job_id, owner).is_err());
        assert!(store
            .require_execute_lease(&job.job_id, &run.fence.lease_owner)
            .is_err());
        assert!(
            claim(&store, &job.job_id, "cleanup").is_empty(),
            "fresh owner prevents duplicate recovery"
        );
        store.start_control_job(&job.job_id, owner).unwrap();
        let recovery: serde_json::Value = serde_json::from_str(&db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE task_id=$1 AND event_type='runtime.agent_run.cleanup_recovered.v1'", &[&job.job_id]).unwrap().get::<_,String>(0)).unwrap();
        assert_eq!(recovery["previousStatus"], status);
        assert_eq!(recovery["retryCount"], 11);
        assert_eq!(recovery["maxRetries"], 10);
        assert_eq!(recovery["previousError"], "retained_cleanup_pending");
        assert_eq!(
            recovery["deadLetterIds"],
            serde_json::json!(["cleanup-dead-letter"])
        );
        assert_eq!(recovery["leaseOwner"], owner.as_str());
        assert_eq!(
            db.query_one(
                "SELECT row_to_json(d)::text FROM runtime.dead_letters d WHERE original_job_id=$1",
                &[&job.job_id]
            )
            .unwrap()
            .get::<_, String>(0),
            history
        );
        assert_eq!(
            db.query_one(
                "SELECT status FROM public.app_core_agentrun WHERE id=$1",
                &[&run.original.agent_run_id]
            )
            .unwrap()
            .get::<_, String>(0),
            "failed"
        );
        assert_eq!(db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_id='agent_run_pre_admission_failure:'||$1", &[&run.original.agent_run_id]).unwrap().get::<_,String>(0), terminal);
        let at = crate::now_ms().unwrap();
        store
            .yield_runtime_job(YieldRuntimeJobRequest {
                job_id: job.job_id.clone(),
                lease_owner: owner.clone(),
                yielded_at_ms: at,
                run_at_ms: at,
                transition_reason: "retained_cleanup_pending".into(),
            })
            .unwrap();
        drop(store);
        let reopened = PostgresRuntimeStore::new(&url).unwrap();
        assert!(reopened
            .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: at,
                worker_id: "worker:ordinary-test".into(),
                job_id: Some(job.job_id.clone()),
                job_kind: Some("agent_run.lifecycle".into()),
                session_id: None,
                limit: 1,
                lease_ms: 60_000
            })
            .unwrap()
            .is_empty());
        let next = claim(&reopened, &job.job_id, "cleanup");
        assert_eq!(next.len(), 1);
        assert_eq!((next[0].retry_count, next[0].max_retries), (11, 10));
        assert_ne!(next[0].lease_owner.as_ref().unwrap(), owner);
        assert_eq!(
            db.query_one("SELECT count(*) FROM runtime.runtime_jobs", &[])
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        assert_eq!(
            db.query_one("SELECT count(*) FROM public.app_core_sessionevent", &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        let next_owner = next[0].lease_owner.as_ref().unwrap();
        reopened.start_control_job(&job.job_id, next_owner).unwrap();
        db.batch_execute("CREATE TABLE IF NOT EXISTS public.app_core_agentrunenvironmentrelease(agent_run_id text PRIMARY KEY,environment_id text NOT NULL,authorization_digest text NOT NULL,controller_epoch bigint NOT NULL)").unwrap();
        let complete = CompleteRuntimeJobRequest {
            job_id: job.job_id.clone(),
            lease_owner: next_owner.clone(),
            completed_at_ms: crate::now_ms().unwrap(),
            output_refs: vec![],
        };
        assert!(
            reopened.complete_runtime_job(complete.clone()).is_err(),
            "recovery cannot acknowledge unfinished cleanup"
        );
        db.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms) VALUES('cleanup:recovery',$1,'environment',$2,4,'neverAllocated',NULL,true,'deferred',$3)", &[&run.original.agent_run_id,&run.original.digest().unwrap(),&crate::now_ms().unwrap()]).unwrap();
        db.execute("INSERT INTO public.app_core_agentrunenvironmentrelease(agent_run_id,environment_id,authorization_digest,controller_epoch) VALUES($1,'environment',$2,4) ON CONFLICT(agent_run_id) DO UPDATE SET environment_id=EXCLUDED.environment_id,authorization_digest=EXCLUDED.authorization_digest,controller_epoch=EXCLUDED.controller_epoch", &[&run.original.agent_run_id,&run.original.digest().unwrap()]).unwrap();
        reopened.complete_runtime_job(complete).unwrap();
        let finished = reopened.get_runtime_job(&job.job_id).unwrap().unwrap();
        assert_eq!(finished.status, RuntimeJobStatus::Succeeded);
        assert_eq!((finished.retry_count, finished.max_retries), (11, 10));
        assert_eq!(
            db.query_one(
                "SELECT row_to_json(d)::text FROM runtime.dead_letters d WHERE original_job_id=$1",
                &[&job.job_id]
            )
            .unwrap()
            .get::<_, String>(0),
            history
        );
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_cleanup_recovery_rejects_foreign_or_settled_responsibility() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for invalid in [
        "UPDATE public.app_core_sessionenvironment SET controller_released=true",
        "UPDATE public.app_core_sessionenvironment SET controller_run_id='another-run'",
        "UPDATE public.app_core_sessionenvironment SET controller_purpose='maintenance'",
        "UPDATE public.app_core_sessionenvironment SET session_id='another-session'",
        "UPDATE public.app_core_agentrunauthorization SET digest='foreign-digest'",
        "UPDATE public.app_core_agentrunauthorization SET payload=jsonb_set(payload,'{agentRunId}','\"foreign-run\"')",
        "UPDATE runtime.runtime_events SET payload_json=(payload_json::jsonb||'{\"authorizationDigest\":\"foreign-digest\"}'::jsonb)::text WHERE event_type='runtime.agent_run.pre_admission_failure.v1'",
        "UPDATE runtime.runtime_jobs SET payload_ref='record:agent_run:foreign-run'",
        "UPDATE runtime.runtime_jobs SET idempotency_key=job_id||':foreign-digest'",
        "UPDATE runtime.runtime_jobs SET status='cancelled'",
        "UPDATE runtime.runtime_jobs SET status='succeeded'",
        "DELETE FROM runtime.runtime_events WHERE event_type='runtime.agent_run.pre_admission_failure.v1'",
    ] {
        let (_url, store, mut db) = fixture::setup();
        let run = failed_cleanup(&store, &mut db, "dead_lettered");
        db.batch_execute(invalid).unwrap();
        assert!(
            !store.control_ready("cleanup").unwrap(),
            "must reject {invalid}"
        );
        assert!(
            claim(&store, &run.fence.job_id, "cleanup").is_empty(),
            "must reject {invalid}"
        );
        assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_type='runtime.agent_run.cleanup_recovered.v1'",&[]).unwrap().get::<_,i64>(0),0);
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_cleanup_recovery_reconstructs_missing_legacy_pending_intent_from_terminal_authority() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = failed_cleanup(&store, &mut db, "dead_lettered");
    db.execute("DELETE FROM runtime.runtime_events WHERE event_type='runtime.agent_run.control_pending.v1'", &[]).unwrap();
    assert!(store.control_ready("cleanup").unwrap());
    let claimed = claim(&store, &run.fence.job_id, "cleanup");
    assert_eq!(claimed.len(), 1);
    let payload: serde_json::Value = serde_json::from_str(&db.query_one("SELECT payload_json FROM runtime.runtime_events WHERE event_id='agent_run_control_pending:'||$1", &[&run.original.agent_run_id]).unwrap().get::<_,String>(0)).unwrap();
    assert_eq!(
        payload["authorizationDigest"],
        run.original.digest().unwrap()
    );
    assert_eq!((claimed[0].retry_count, claimed[0].max_retries), (11, 10));
}
