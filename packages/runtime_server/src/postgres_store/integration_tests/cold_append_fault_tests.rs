use super::cold_append_tests::{
    assert_bounded_cold_scan, control_record, fixture, Fixture, RUN, SESSION, WORKSPACE,
};
use super::*;

pub(super) fn start() -> crate::contract::AgentRunStart {
    let mut start = crate::tests::agent_run_start();
    start.agent_run_id = RUN.into();
    start.turn_id = "turn_pg_fenced_terminal".into();
    start.prompt = "cold append".into();
    start.authorization.agent_run_id = RUN.into();
    start.authorization.session_id = SESSION.into();
    start.authorization.workspace_id = WORKSPACE.into();
    start.authorization_digest = start.authorization.digest().unwrap();
    start.authorization_signature = start
        .authorization
        .signature(b"test-run-authorization-signing-key")
        .unwrap();
    start.environment_authorization =
        crate::environment_authorization::EnvironmentAuthorization::for_test(
            &start.authorization,
            b"test-run-authorization-signing-key",
        );
    start
}

pub(super) fn lease(fixture: &Fixture) -> RuntimeJobLeaseFence {
    let now = crate::now_ms().unwrap();
    let id = format!("agent_run.lifecycle:{RUN}");
    let mut lifecycle = job(&id, "cold-terminal");
    lifecycle.job_kind = "agent_run.lifecycle".into();
    lifecycle.session_id = Some(SESSION.into());
    lifecycle.run_at_ms = now;
    lifecycle.created_at_ms = now;
    lifecycle.updated_at_ms = now;
    fixture
        .store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: lifecycle })
        .unwrap();
    let owner = fixture
        .store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "cold-terminal-owner".into(),
            job_id: Some(id.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 60_000,
        })
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    fixture
        .store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: id.clone(),
            lease_owner: owner.clone(),
            started_at_ms: now,
        })
        .unwrap();
    RuntimeJobLeaseFence {
        job_id: id,
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner,
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_fresh_logger_failure_and_cancel_handlers_rebuild_bounded_history() {
    let _guard = TEST_LOCK.lock().unwrap();
    for cancelled in [false, true] {
        let fixture = fixture(100);
        let start = start();
        let mut sequence = crate::load_existing_session_sequence(&fixture.store, &start).unwrap();
        assert!(!sequence.is_empty());
        let fence = lease(&fixture);
        let cold = fixture.fresh_log();
        assert!(!cold.has_append_cache(RUN));
        let mut stream = None;
        let assistant = crate::AssistantTextProjection::default();
        let outcome = if cancelled {
            crate::commit_cancelled_agent_run(
                &fixture.runtime,
                &cold,
                &start,
                &mut sequence,
                &mut stream,
                &assistant,
                &fence,
            )
        } else {
            crate::commit_failed_agent_run(
                &fixture.runtime,
                &cold,
                &start,
                &mut sequence,
                &mut stream,
                "turn_input_prepared_message_missing",
                "core_run_failed",
                &assistant,
                &fence,
            )
        }
        .unwrap();
        assert_eq!(
            outcome.terminal_state,
            Some(if cancelled { "cancelled" } else { "failed" })
        );
        assert_bounded_cold_scan(&fixture, &cold);
        let mut db = Client::connect(&test_url(), NoTls).unwrap();
        let rows = db.query("SELECT agent_run_sequence,payload::text FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND agent_run_sequence >= $2 ORDER BY agent_run_sequence", &[&RUN, &(fixture.next_sequence as i32)]).unwrap();
        let mut batch = Vec::new();
        for row in rows {
            let mut wire: serde_json::Value =
                serde_json::from_str(&row.get::<_, String>(1)).unwrap();
            crate::postgres_store::decode_session_payload(&mut wire).unwrap();
            let mut record = centaeris_core::session::parse_wire_record(&wire).unwrap();
            record.sequence = row.get::<_, i32>(0) as u64;
            batch.push(record);
        }
        let expected = if cancelled {
            SessionRecordType::AgentRunInterrupted
        } else {
            SessionRecordType::AgentRunFailed
        };
        assert_eq!(
            batch
                .iter()
                .filter(|record| record.event.event_type == expected)
                .count(),
            1
        );
        assert!(!batch
            .iter()
            .any(|record| record.event.event_type == SessionRecordType::ToolResult));
        // Replay exactly the committed bytes/timestamps, rather than call a
        // terminal handler a second time and manufacture another operation.
        let replay = fixture
            .runtime
            .block_on(cold.append_session_records_with_runtime_job_lease(RUN, &batch, &fence))
            .unwrap();
        assert_eq!(replay.records.len(), batch.len());
        for (committed, expected) in replay.records.iter().zip(&batch) {
            assert_eq!(committed.sequence, expected.sequence);
            assert_eq!(committed.event, expected.event);
        }
        assert_eq!(cold.cold_append_observation().loads, 1);
    }
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_cold_append_rejects_corrupt_middle_content_and_retries_without_partial_cache() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(100);
    let mut db = Client::connect(&test_url(), NoTls).unwrap();
    let row = db.query_one("SELECT content_digest,content_json,kind,content_bytes FROM runtime.model_observation_contents WHERE session_id=$1 AND content_json::jsonb #>> '{message,messageId}'=$2", &[&SESSION, &"context-70"]).unwrap();
    let digest: String = row.get(0);
    let original: String = row.get(1);
    let kind: String = row.get(2);
    let bytes: i64 = row.get(3);
    let cold = fixture.fresh_log();
    let event = control_record(fixture.next_sequence);
    for mutation in [
        "content_json=content_json || ' '",
        "kind='system_prompt'",
        "content_bytes=content_bytes+1",
    ] {
        db.execute(&format!("UPDATE runtime.model_observation_contents SET {mutation} WHERE session_id=$1 AND content_digest=$2"), &[&SESSION, &digest]).unwrap();
        assert!(fixture
            .runtime
            .block_on(cold.append_session_records(RUN, std::slice::from_ref(&event)))
            .is_err());
        assert!(!cold.has_append_cache(RUN));
        assert_eq!(db.query_one("SELECT COUNT(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND agent_run_sequence=$2", &[&RUN, &(fixture.next_sequence as i32)]).unwrap().get::<_,i64>(0), 0);
        db.execute("UPDATE runtime.model_observation_contents SET content_json=$3,kind=$4,content_bytes=$5 WHERE session_id=$1 AND content_digest=$2", &[&SESSION, &digest, &original, &kind, &bytes]).unwrap();
    }
    fixture
        .runtime
        .block_on(cold.append_session_records(RUN, &[event]))
        .unwrap();
    assert_eq!(cold.cold_append_observation().loads, 4);
    assert!(cold.has_append_cache(RUN));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_cold_append_transaction_failure_discards_rebuilt_cache_before_retry() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(100);
    let mut db = Client::connect(&test_url(), NoTls).unwrap();
    db.batch_execute("CREATE FUNCTION runtime.reject_cold_append() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'cold_append_injected_insert_failure'; END $$; CREATE TRIGGER reject_cold_append BEFORE INSERT ON public.app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION runtime.reject_cold_append();").unwrap();
    let cold = fixture.fresh_log();
    let event = control_record(fixture.next_sequence);
    assert!(fixture
        .runtime
        .block_on(cold.append_session_records(RUN, std::slice::from_ref(&event)))
        .is_err());
    assert!(!cold.has_append_cache(RUN));
    db.batch_execute("DROP TRIGGER reject_cold_append ON public.app_core_sessionevent; DROP FUNCTION runtime.reject_cold_append();").unwrap();
    let receipt = fixture
        .runtime
        .block_on(cold.append_session_records(RUN, std::slice::from_ref(&event)))
        .unwrap();
    assert_eq!(receipt.records[0].event, event.event);
    assert_eq!(cold.cold_append_observation().loads, 2);
    assert!(cold.has_append_cache(RUN));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_cold_append_validates_middle_page_ownership_sequences_and_missing_content() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(100);
    let mut db = Client::connect(&test_url(), NoTls).unwrap();
    let row = db.query_one("SELECT content_digest,kind,content_json,content_bytes,first_seen_at_ms FROM runtime.model_observation_contents WHERE session_id=$1 AND content_json::jsonb #>> '{message,messageId}'=$2", &[&SESSION, &"context-70"]).unwrap();
    let digest: String = row.get(0);
    let kind: String = row.get(1);
    let body: String = row.get(2);
    let bytes: i64 = row.get(3);
    let first_seen: i64 = row.get(4);
    // This exact middle-page event is beyond the first two 64-record pages.
    // Retain only its identity and one CAS body, never an expanded history copy.
    let event_id: String = db.query_one("SELECT \"eventId\" FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND agent_run_sequence=143", &[&RUN]).unwrap().get(0);
    for (offset, mutation) in [
        "workspace",
        "sequence_hole",
        "sequence_null",
        "missing_content",
        "foreign_content",
    ]
    .into_iter()
    .enumerate()
    {
        match mutation {
            "workspace" => {
                db.execute("UPDATE public.app_core_sessionevent SET workspace_id='foreign-workspace' WHERE \"eventId\"=$1", &[&event_id]).unwrap();
            }
            "sequence_hole" => {
                db.execute("UPDATE public.app_core_sessionevent SET agent_run_sequence=10000 WHERE \"eventId\"=$1", &[&event_id]).unwrap();
            }
            "sequence_null" => {
                db.execute("UPDATE public.app_core_sessionevent SET agent_run_sequence=NULL WHERE \"eventId\"=$1", &[&event_id]).unwrap();
            }
            "missing_content" => {
                db.execute("DELETE FROM runtime.model_observation_contents WHERE session_id=$1 AND content_digest=$2", &[&SESSION, &digest]).unwrap();
            }
            "foreign_content" => {
                db.execute("UPDATE runtime.model_observation_contents SET session_id='foreign-session' WHERE session_id=$1 AND content_digest=$2", &[&SESSION, &digest]).unwrap();
            }
            _ => unreachable!(),
        }
        let cold = fixture.fresh_log();
        let next = fixture.next_sequence + offset as u64;
        let event = control_record(next);
        assert!(
            fixture
                .runtime
                .block_on(cold.append_session_records(RUN, std::slice::from_ref(&event)))
                .is_err(),
            "accepted {mutation}"
        );
        assert!(!cold.has_append_cache(RUN), "cached partial {mutation}");
        assert_eq!(db.query_one("SELECT COUNT(*) FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND agent_run_sequence=$2", &[&RUN, &(next as i32)]).unwrap().get::<_,i64>(0), 0);
        match mutation {
            "workspace" => {
                db.execute(
                    "UPDATE public.app_core_sessionevent SET workspace_id=$2 WHERE \"eventId\"=$1",
                    &[&event_id, &WORKSPACE],
                )
                .unwrap();
            }
            "sequence_hole" | "sequence_null" => {
                db.execute("UPDATE public.app_core_sessionevent SET agent_run_sequence=143 WHERE \"eventId\"=$1", &[&event_id]).unwrap();
            }
            "missing_content" => {
                db.execute("INSERT INTO runtime.model_observation_contents(session_id,content_digest,kind,content_json,content_bytes,first_seen_at_ms) VALUES($1,$2,$3,$4,$5,$6)", &[&SESSION, &digest, &kind, &body, &bytes, &first_seen]).unwrap();
            }
            "foreign_content" => {
                db.execute("UPDATE runtime.model_observation_contents SET session_id=$1 WHERE session_id='foreign-session' AND content_digest=$2", &[&SESSION, &digest]).unwrap();
            }
            _ => unreachable!(),
        }
        let receipt = fixture
            .runtime
            .block_on(cold.append_session_records(RUN, std::slice::from_ref(&event)))
            .unwrap();
        assert_eq!(receipt.records[0].event, event.event);
        assert_eq!(
            cold.cold_append_observation().loads,
            2,
            "retry did not rebuild {mutation}"
        );
        assert!(cold.has_append_cache(RUN));
    }
}
