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
        db.execute(&format!("UPDATE runtime.runtime_events SET {mutation} WHERE event_id=$1"), &[&event_id]).unwrap();
        assert!(has_pre_admission_failure(&mut db, "ws_1", "sess_1", &r.original.agent_run_id, &digest).is_err(), "{mutation}");
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
