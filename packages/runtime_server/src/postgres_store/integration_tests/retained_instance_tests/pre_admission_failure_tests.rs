use super::*;

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_pre_admission_failed_producer_commits_control_receipt_without_history() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let r = run(&store, &mut db, "run-pre-admission-producer", 1);
    db.batch_execute("ALTER TABLE public.app_core_sessionevent ADD COLUMN sequence bigint")
        .unwrap();
    let mut start = crate::tests::agent_run_start();
    start.agent_run_id = r.original.agent_run_id.clone();
    start.authorization = r.original.clone();
    start.authorization_digest = r.original.digest().unwrap();
    let log = store.session_log("ws_1".into(), "sess_1".into(), "probe".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut sequence =
        centaeris_core::session::AgentRunSessionState::new("sess_1", &start.agent_run_id).unwrap();
    let mut stream = None;
    let result = crate::commit_failed_agent_run(
        &runtime,
        &log,
        &start,
        &mut sequence,
        &mut stream,
        "sandbox_prepare_rejected",
        "sandbox_prepare_failed",
        &crate::AssistantTextProjection::default(),
        &r.fence,
    )
    .unwrap();
    assert_eq!(result.terminal_state, Some("failed"));
    let event = format!("agent_run_pre_admission_failure:{}", start.agent_run_id);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.runtime_events WHERE event_id=$1",
            &[&event]
        )
        .unwrap()
        .get::<_, i64>(0),
        1,
        "terminal pre-admission failure needs an authoritative control receipt"
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM public.app_core_sessionevent", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.runtime_events WHERE event_id='agent_run_control_pending:'||$1", &[&start.agent_run_id]).unwrap().get::<_,i64>(0), 1, "terminal failure must retain durable cleanup intent in the same transaction");
    assert_eq!(
        crate::load_existing_terminal_state(&store, &start).unwrap(),
        Some("failed")
    );
}

use crate::postgres_store::pre_admission_failure::has_pre_admission_failure;

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_pre_admission_failure_is_fenced_durable_and_blocks_history() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (url, store, mut db) = setup();
    let r = run(&store, &mut db, "run-pre-admission-failure", 1);
    let digest = r.original.digest().unwrap();
    let log = store.session_log("ws_1".into(), "sess_1".into(), "probe".into());
    let mut stale = r.fence.clone();
    stale.lease_owner = "stale-owner".into();
    assert!(log
        .commit_pre_admission_failure(&r.original.agent_run_id, &digest, &stale)
        .is_err());
    assert!(log
        .commit_pre_admission_failure(&r.original.agent_run_id, "wrong-digest", &r.fence)
        .is_err());
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.runtime_events", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    log.commit_pre_admission_failure(&r.original.agent_run_id, &digest, &r.fence)
        .unwrap();
    let event_id = format!(
        "agent_run_pre_admission_failure:{}",
        r.original.agent_run_id
    );
    let original: String = db
        .query_one(
            "SELECT row_to_json(e)::text FROM runtime.runtime_events e WHERE event_id=$1",
            &[&event_id],
        )
        .unwrap()
        .get(0);
    assert!(has_pre_admission_failure(
        &mut db,
        "ws_1",
        "sess_1",
        &r.original.agent_run_id,
        &digest
    )
    .unwrap());
    assert!(has_pre_admission_failure(
        &mut db,
        "ws_1",
        "sess_1",
        &r.original.agent_run_id,
        "wrong-digest"
    )
    .is_err());
    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner='renewed-owner' WHERE job_id=$1",
        &[&r.fence.job_id],
    )
    .unwrap();
    let mut renewed = r.fence.clone();
    renewed.lease_owner = "renewed-owner".into();
    drop(log);
    drop(store);
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let log = reopened.session_log("ws_1".into(), "sess_1".into(), "probe".into());
    log.commit_pre_admission_failure(&r.original.agent_run_id, &digest, &renewed)
        .unwrap();
    let retried: String = db
        .query_one(
            "SELECT row_to_json(e)::text FROM runtime.runtime_events e WHERE event_id=$1",
            &[&event_id],
        )
        .unwrap()
        .get(0);
    assert_eq!(
        original, retried,
        "first timestamp and writer provenance must survive retry/reopen"
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM public.app_core_sessionevent", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let record = super::super::session_record(
        &r.original.agent_run_id,
        "sess_1",
        1,
        centaeris_core::session::SessionRecordType::AgentRunFailed,
        serde_json::json!({"reasonType":"runtime_internal_error","message":"test"}),
        1,
    );
    let error = log
        .append_session_records_with_runtime_job_lease_blocking(
            &r.original.agent_run_id,
            &[record],
            &renewed,
        )
        .unwrap_err();
    assert!(error.contains("pre-admission failure"), "{error}");
    assert!(log
        .commit_pre_admission_cancellation(&r.original.agent_run_id, &digest, &renewed)
        .is_err());
    assert!(db
        .query_one(
            "SELECT \"preAdmissionCancelledAt\" IS NULL FROM public.app_core_agentrun WHERE id=$1",
            &[&r.original.agent_run_id]
        )
        .unwrap()
        .get::<_, bool>(0));
    let original_row: serde_json::Value = serde_json::from_str(&original).unwrap();
    let payload = original_row["payload_json"].as_str().unwrap();
    let at_ms = original_row["at_ms"].as_i64().unwrap();
    for mutation in [
        "at_ms=at_ms+1",
        "event_type='runtime.agent_run.other.v1'",
        "visibility='public'",
        "session_id='other-session'",
        "task_id='other-run'",
        "payload_json=(payload_json::jsonb || '{\"workspaceId\":\"other-workspace\"}'::jsonb)::text",
        "payload_json=(payload_json::jsonb || '{\"failedAtMs\":\"invalid\"}'::jsonb)::text",
    ] {
        db.execute(
            &format!("UPDATE runtime.runtime_events SET {mutation} WHERE event_id=$1"),
            &[&event_id],
        )
        .unwrap();
        assert!(
            has_pre_admission_failure(&mut db, "ws_1", "sess_1", &r.original.agent_run_id, &digest)
                .is_err(),
            "{mutation}"
        );
        db.execute("UPDATE runtime.runtime_events SET session_id='sess_1',task_id=$1,event_type=$2,at_ms=$3,visibility='internal',payload_json=$4 WHERE event_id=$5",
            &[&r.original.agent_run_id, &crate::postgres_store::pre_admission_failure::SCHEMA, &at_ms, &payload, &event_id]).unwrap();
    }
    db.execute("UPDATE runtime.runtime_events SET payload_json=(payload_json::jsonb || '{\"unexpected\":true}'::jsonb)::text WHERE event_id=$1", &[&event_id]).unwrap();
    assert!(has_pre_admission_failure(
        &mut db,
        "ws_1",
        "sess_1",
        &r.original.agent_run_id,
        &digest
    )
    .is_err());
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_pre_admission_failure_rejects_admitted_or_cancelled_runs() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let r = run(&store, &mut db, "run-pre-admission-admitted", 1);
    let digest = r.original.digest().unwrap();
    let log = store.session_log("ws_1".into(), "sess_1".into(), "probe".into());
    db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1',$1,false,'{\"type\":\"agent_run_started\"}')", &[&r.original.agent_run_id]).unwrap();
    assert!(log
        .commit_pre_admission_failure(&r.original.agent_run_id, &digest, &r.fence)
        .is_err());
    db.batch_execute("DELETE FROM public.app_core_sessionevent; UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=now()").unwrap();
    assert!(log
        .commit_pre_admission_failure(&r.original.agent_run_id, &digest, &r.fence)
        .is_err());
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.runtime_events", &[])
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

fn runsc_paused_cancel_cleanup(r: &mut Run, db: &mut Client) -> crate::contract::AgentRunCleanup {
    let mut binding = r.grant.payload.clone();
    binding.controller.as_mut().unwrap().controller_purpose = "cleanup".into();
    r.grant = sign(binding);
    db.execute("UPDATE public.app_core_sessionenvironment SET controller_purpose='cleanup' WHERE controller_run_id=$1",&[&r.original.agent_run_id]).unwrap();
    db.execute(
        "UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=now() WHERE id=$1",
        &[&r.original.agent_run_id],
    )
    .unwrap();
    crate::contract::AgentRunCleanup {
        schema: "workspace.agent_run.cleanup.v1".into(),
        agent_run_id: r.original.agent_run_id.clone(),
        authorization_digest: r.original.digest().unwrap(),
        authorization_signature: r.original.signature(KEY).unwrap(),
        authorization: r.original.clone(),
        environment_authorization: r.grant.clone(),
    }
}
fn runsc_mark_paused(db: &mut Client, state: &str) {
    db.execute("INSERT INTO runtime.runsc_environment_power(environment_id,host_id,allocation_execution_id,instance_id,container_id,state,controller_epoch,idle_since_ms,updated_at_ms) SELECT environment_id,host_id,execution_id,instance_id,container_id,$1,1,0,0 FROM runtime.resident_sandboxes ON CONFLICT(environment_id) DO UPDATE SET state=$1",&[&state]).unwrap();
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_runsc_queued_cancel_preserves_original_paused_environment_without_task_rpc() {
    use crate::postgres_store::cleanup_receipts::{
        CleanupAllocation, CleanupAuthorityRequest, CleanupTarget,
    };
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let origin = run(&store, &mut db, "runsc-cancel-origin", 1);
    let allocation = prepare(&store, &origin, "runsc-cancel-allocation", &physical).unwrap();
    runsc_mark_paused(&mut db, "paused");
    let mut cancelled = run(&store, &mut db, "runsc-cancel-queued", 2);
    let cleanup = runsc_paused_cancel_cleanup(&mut cancelled, &mut db);
    // Observe after the new controller fixture is bound, before cleanup dispatch.
    // Host inventory refreshes its observation timestamp; every identity, state
    // and resource debit must remain unchanged by an unstarted Run cleanup.
    let resident = db
        .query_one(
            "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    let power = db
        .query_one(
            "SELECT row_to_json(p)::text FROM runtime.runsc_environment_power p",
            &[],
        )
        .unwrap()
        .get::<_, String>(0);
    let receipt = store
        .with_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &cancelled.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |target, _| {
                assert!(
                    matches!(target, CleanupTarget::NeverAllocated),
                    "unstarted Run must not issue Takeover, EndRun or resume to the paused original"
                );
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(receipt.allocation, CleanupAllocation::NeverAllocated {});
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.execution_instances WHERE agent_run_id=$1",
            &[&cancelled.original.agent_run_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one(
            "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        resident
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(p)::text FROM runtime.runsc_environment_power p",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        power
    );
    assert_eq!(
        allocation.container_id,
        physical
            .lock()
            .unwrap()
            .first()
            .map(|p| p.container_id.clone())
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_runsc_queued_cancel_rejects_missing_association_with_execution_history() {
    use crate::postgres_store::cleanup_receipts::CleanupAuthorityRequest;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let origin = run(&store, &mut db, "runsc-history-origin", 1);
    prepare(&store, &origin, "runsc-history-allocation", &physical).unwrap();
    runsc_mark_paused(&mut db, "paused");
    let mut cancelled = run(&store, &mut db, "runsc-history-cancelled", 2);
    let cleanup = runsc_paused_cancel_cleanup(&mut cancelled, &mut db);
    db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1',$1,false,'{\"type\":\"agent_run_execution_started\"}')",&[&cancelled.original.agent_run_id]).unwrap();
    let error = store
        .with_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &cancelled.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |_, _| -> Result<(), String> {
                panic!("execution evidence without association cannot be discarded")
            },
        )
        .unwrap_err();
    assert!(
        error.contains("cleanup execution association missing"),
        "{error}"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_runsc_queued_cancel_keeps_unknown_or_associated_effects_supervised() {
    use crate::postgres_store::cleanup_receipts::{CleanupAuthorityRequest, CleanupTarget};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for associated in [false, true] {
        let (_url, store, mut db) = setup();
        let physical = Arc::new(Mutex::new(Vec::new()));
        let origin = run(&store, &mut db, "runsc-effects-origin", 1);
        prepare(&store, &origin, "runsc-effects-allocation", &physical).unwrap();
        let mut cancelled = run(&store, &mut db, "runsc-effects-cancelled", 2);
        if associated {
            prepare(&store, &cancelled, "runsc-effects-associated", &physical).unwrap();
        }
        runsc_mark_paused(
            &mut db,
            if associated {
                "paused"
            } else {
                "pauseDispatched"
            },
        );
        let cleanup = runsc_paused_cancel_cleanup(&mut cancelled, &mut db);
        let result = store.with_cleanup_authority(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &cancelled.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |target, _| -> Result<(), String> {
                assert!(
                    matches!(target, CleanupTarget::Instance { .. }),
                    "unknown pause or own allocation still requires physical cleanup"
                );
                Err("physical cleanup remains supervised".into())
            },
        );
        assert_eq!(result.unwrap_err(), "physical cleanup remains supervised");
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
            0
        );
    }
}

// Reproduce the committed association + running reservation left behind when
// the before-create callback fails, before any physical resume/takeover call.
fn runsc_reused_before_create_failure(
    store: &PostgresRuntimeStore,
    db: &mut Client,
    physical: &Arc<Mutex<Vec<ObservedResidentSandbox>>>,
) -> (Run, RetainedInstanceAllocation) {
    let origin = run(store, db, "runsc-before-create-origin", 1);
    let allocation = prepare(store, &origin, "runsc-before-create-allocation", physical).unwrap();
    runsc_mark_paused(db, "paused");
    let cancelled = run(store, db, "runsc-before-create-cancelled", 2);
    let digest = cancelled.original.environment_profile.digest().unwrap();
    let error = store
        .prepare_retained_instance(
            &cancelled.request("runsc-before-create-associated", &digest),
            limits(&cancelled),
            || Ok(physical.lock().unwrap().clone()),
            |client, reused| {
                assert!(reused.reused);
                // Exercise the same durable reservation boundary without process-wide
                // environment changes; the allocation association is already committed.
                let mut tx = client.transaction().map_err(|e| e.to_string())?;
                crate::postgres_store::runsc_rotation::reserve_running(
                    &mut tx,
                    &crate::postgres_store::runsc_rotation::RunscBinding::from(reused),
                    1,
                )?;
                tx.commit().map_err(|e| e.to_string())?;
                Err("before_create_failed_without_resume".into())
            },
            |_| panic!("failed callback must prevent resume, takeover and task dispatch"),
        )
        .unwrap_err();
    assert_eq!(error, "before_create_failed_without_resume");
    assert_eq!(
        db.query_one("SELECT state FROM runtime.runsc_environment_power", &[])
            .unwrap()
            .get::<_, String>(0),
        "resumePrepared"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.execution_instances WHERE agent_run_id=$1",
            &[&cancelled.original.agent_run_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    (cancelled, allocation)
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_runsc_reused_paused_before_create_failure_closes_instance_without_task_rpc() {
    use crate::postgres_store::cleanup_receipts::{CleanupAllocation, CleanupAuthorityRequest};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = setup();
    let physical = Arc::new(Mutex::new(Vec::new()));
    let (mut cancelled, allocation) =
        runsc_reused_before_create_failure(&store, &mut db, &physical);
    let cleanup = runsc_paused_cancel_cleanup(&mut cancelled, &mut db);
    let resident: String = db
        .query_one(
            "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
            &[],
        )
        .unwrap()
        .get(0);
    let power: String = db
        .query_one(
            "SELECT row_to_json(p)::text FROM runtime.runsc_environment_power p",
            &[],
        )
        .unwrap()
        .get(0);
    let receipt = store
        .with_cleanup_authority_and_power_observation(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &cancelled.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |observed, profile| {
                assert_eq!(
                    observed.allocation_execution_id,
                    allocation.allocation_execution_id
                );
                assert_eq!(observed.container_id, allocation.container_id);
                assert_eq!(profile.digest().unwrap(), allocation.profile_digest);
                Ok(true)
            },
            |_, _| -> Result<(), String> { Err("unexpected_takeover_or_end_run".into()) },
        )
        .unwrap();
    assert_eq!(
        receipt.allocation,
        CleanupAllocation::Instance {
            instance_id: allocation.instance_id
        }
    );
    assert_eq!(db.query_one("SELECT allocation_kind,physical_removed FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1", &[&cancelled.original.agent_run_id]).unwrap().get::<_,String>(0), "instance");
    assert!(!db
        .query_one(
            "SELECT physical_removed FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1",
            &[&cancelled.original.agent_run_id]
        )
        .unwrap()
        .get::<_, bool>(0));
    assert_eq!(
        db.query_one(
            "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        resident
    );
    assert_eq!(
        db.query_one(
            "SELECT row_to_json(p)::text FROM runtime.runsc_environment_power p",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        power
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1",
            &[&cancelled.original.agent_run_id]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_runsc_reused_paused_cleanup_keeps_execution_or_unknown_power_supervised() {
    use crate::postgres_store::cleanup_receipts::CleanupAuthorityRequest;
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for evidence in ["execution", "runtime", "unknown", "running"] {
        let (_url, store, mut db) = setup();
        let physical = Arc::new(Mutex::new(Vec::new()));
        let (mut cancelled, _) = runsc_reused_before_create_failure(&store, &mut db, &physical);
        let cleanup = runsc_paused_cancel_cleanup(&mut cancelled, &mut db);
        if evidence == "execution" {
            db.execute("INSERT INTO public.app_core_sessionevent VALUES('ws_1','sess_1',$1,false,'{\"type\":\"agent_run_execution_started\"}')", &[&cancelled.original.agent_run_id]).unwrap();
        }
        if evidence == "runtime" {
            db.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES('uncertain-runtime-effect','sess_1',$1,'runtime.unknown_execution.v1',1,'internal','{}')", &[&cancelled.original.agent_run_id]).unwrap();
        }
        let result = store.with_cleanup_authority_and_power_observation(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &cancelled.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |_, _| {
                if evidence == "unknown" {
                    Err("physical_pause_unconfirmed".into())
                } else {
                    Ok(evidence != "running")
                }
            },
            |_, _| -> Result<(), String> { Err("physical_cleanup_still_required".into()) },
        );
        assert!(
            result.is_err(),
            "{evidence} cannot create a cleanup receipt"
        );
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            db.query_one("SELECT count(*) FROM runtime.resident_sandboxes", &[])
                .unwrap()
                .get::<_, i64>(0),
            1
        );
        assert_eq!(
            db.query_one("SELECT state FROM runtime.runsc_environment_power", &[])
                .unwrap()
                .get::<_, String>(0),
            "resumePrepared"
        );
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL cleanup database"]
fn postgres_runsc_reused_paused_failed_run_accepts_only_bound_failure_receipt() {
    use crate::postgres_store::cleanup_receipts::{CleanupAllocation, CleanupAuthorityRequest};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for evidence in ["valid", "corrupt", "foreign"] {
        let (_url, store, mut db) = setup();
        let physical = Arc::new(Mutex::new(Vec::new()));
        let (mut failed, allocation) =
            runsc_reused_before_create_failure(&store, &mut db, &physical);
        let log = store.session_log("ws_1".into(), "sess_1".into(), "failed admission".into());
        log.commit_pre_admission_failure(
            &failed.original.agent_run_id,
            &failed.original.digest().unwrap(),
            &failed.fence,
        )
        .unwrap();
        let cleanup = runsc_paused_cancel_cleanup(&mut failed, &mut db);
        db.execute("UPDATE public.app_core_agentrun SET \"preAdmissionCancelledAt\"=NULL,status='failed' WHERE id=$1", &[&failed.original.agent_run_id]).unwrap();
        if evidence == "corrupt" {
            db.execute(
                "UPDATE runtime.runtime_events SET payload_json='{' WHERE task_id=$1",
                &[&failed.original.agent_run_id],
            )
            .unwrap();
        } else if evidence == "foreign" {
            db.execute("UPDATE runtime.runtime_events SET payload_json=(payload_json::jsonb || '{\"workspaceId\":\"foreign-workspace\"}'::jsonb)::text WHERE task_id=$1", &[&failed.original.agent_run_id]).unwrap();
        }
        let resident: String = db
            .query_one(
                "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
                &[],
            )
            .unwrap()
            .get(0);
        let power: String = db
            .query_one(
                "SELECT row_to_json(p)::text FROM runtime.runsc_environment_power p",
                &[],
            )
            .unwrap()
            .get(0);
        let receipt = store.with_cleanup_authority_and_power_observation(
            &CleanupAuthorityRequest {
                cleanup: &cleanup,
                signing_key: KEY,
                lease: &failed.fence,
                host_id: "host",
            },
            || Ok(physical.lock().unwrap().clone()),
            |observed, _| {
                assert_eq!(
                    observed.allocation_execution_id,
                    allocation.allocation_execution_id
                );
                Ok(true)
            },
            |_, _| -> Result<(), String> {
                Err("unexpected_task_rpc_for_failed_unstarted_run".into())
            },
        );
        if evidence == "valid" {
            assert_eq!(
                receipt.unwrap().allocation,
                CleanupAllocation::Instance {
                    instance_id: allocation.instance_id
                }
            );
        } else {
            assert!(receipt.is_err(), "{evidence} receipt cannot settle a Run");
            assert_eq!(
                db.query_one(
                    "SELECT count(*) FROM runtime.agent_run_cleanup_receipts",
                    &[]
                )
                .unwrap()
                .get::<_, i64>(0),
                0
            );
        }
        assert_eq!(
            db.query_one(
                "SELECT (to_jsonb(s)-'updated_at_ms')::text FROM runtime.resident_sandboxes s",
                &[]
            )
            .unwrap()
            .get::<_, String>(0),
            resident
        );
        assert_eq!(
            db.query_one(
                "SELECT row_to_json(p)::text FROM runtime.runsc_environment_power p",
                &[]
            )
            .unwrap()
            .get::<_, String>(0),
            power
        );
    }
}
