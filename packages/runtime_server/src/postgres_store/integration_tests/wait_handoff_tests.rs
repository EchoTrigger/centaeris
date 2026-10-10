use super::*;
use crate::wait_recovery::WaitHandoff;
use centaeris_core::execution::{ExecutionWorkspaceGeneration, ExecutionWorkspaceGenerationV1};
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, ProviderTokenUsageV1, RecoveryWorkspaceSnapshotV1, RuntimeAgentRunIdentityV1,
    RuntimeAwaitJobCheckpointV1, RuntimeJobWaitV1, RuntimeRecoveryCheckpointV1,
    RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1,
};
use centaeris_core::session::state::SessionStateSnapshot;
use centaeris_core::session::AgentRunSessionState;

struct Fixture {
    store: PostgresRuntimeStore,
    state: AgentRunSessionState,
    fence: RuntimeJobLeaseFence,
    checkpoint: CheckpointRecord,
    handoff: WaitHandoff,
}

const RUN: &str = "agent_run_wait_handoff";
const SESSION: &str = "session_wait_handoff";
const TURN: &str = "turn_pg_fenced_terminal";

fn fixture() -> Fixture {
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    store.with_client(|db| db.batch_execute(r#"
        DROP TABLE IF EXISTS public.app_core_sessionevent CASCADE;
        DROP TABLE IF EXISTS public.app_core_session CASCADE;
        CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL);
        CREATE TABLE public.app_core_sessionevent("eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
        INSERT INTO public.app_core_session VALUES('session_wait_handoff','workspace_wait_handoff');
        "#).map_err(|e|e.to_string())).unwrap();
    let now = crate::now_ms().unwrap();
    let mut lifecycle = job(&format!("agent_run.lifecycle:{RUN}"), "wait-handoff-lease");
    lifecycle.job_kind = "agent_run.lifecycle".into();
    lifecycle.session_id = Some(SESSION.into());
    lifecycle.run_at_ms = now;
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: lifecycle.clone(),
        })
        .unwrap();
    let owner = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "wait-handoff-worker".into(),
            job_id: Some(lifecycle.job_id.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 600000,
        })
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: lifecycle.job_id.clone(),
            lease_owner: owner.clone(),
            started_at_ms: now,
        })
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id: lifecycle.job_id,
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: owner,
    };
    let digest = format!("sha256:{}", "a".repeat(64));
    let composition = centaeris_core::extension::composition::resolve_agent_composition(
        centaeris_core::extension::composition::AgentCompositionInputsV1 {
            prompt_digest: digest.clone(),
            model_binding: centaeris_core::extension::composition::ResolvedModelBindingV1 {
                provider_id: "test-provider".into(),
                model_name: "test-model".into(),
                wire_protocol: "test-wire".into(),
                config_digest: digest.clone(),
            },
            skill_catalog_digest: digest.clone(),
            plugin_activation_digest: digest.clone(),
            hook_composition_digest: digest.clone(),
            execution_profile_digest: digest.clone(),
            policy_version: "test-v1".into(),
        },
        std::iter::empty(),
    )
    .unwrap();
    let mut state = AgentRunSessionState::new(SESSION, RUN).unwrap();
    let mut records = state.start(TURN, "wait once", vec![], now).unwrap();
    records.push(
        state
            .start_execution(TURN, "execution_wait_handoff", &digest, None, now)
            .unwrap(),
    );
    records.push(state.record(session_record(RUN,SESSION,4,SessionRecordType::ModelRequestStarted,serde_json::json!({
        "requestId":"request_wait_handoff","purpose":"main","loopIndex":0,"toolChoice":{"type":"none"},"maxOutputTokens":1024,"promptCacheKey":null,"promptCacheRetention":null,"preparedPromptSchema":"prepared_prompt.v1","contextTokenEstimate":0,"contextTokenBreakdown":{"systemPromptTokens":0,"systemToolTokens":0,"mcpToolTokens":0,"skillsTokens":0,"messageTokens":0,"mcpTools":[]},"agentComposition":composition,"observations":[]}),now).event).unwrap());
    let mut payload = RuntimeRecoveryCheckpointV1 {
        schema: RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1.into(),
        checkpoint_id: "checkpoint:before_wait_handoff".into(),
        session_id: SESSION.into(),
        agent_run_id: RUN.into(),
        execution_id: "execution_wait_handoff".into(),
        authorization_digest: digest.clone(),
        session_sequence: 5,
        model_request_id: "request_wait_handoff".into(),
        workspace_snapshot: RecoveryWorkspaceSnapshotV1 {
            object_ref: None,
            snapshot_sha256: String::new(),
            snapshot_size_bytes: 0,
            expanded_size_bytes: 0,
            file_count: 0,
        },
        workspace_generation: ExecutionWorkspaceGeneration::Known {
            token: ExecutionWorkspaceGenerationV1 {
                instance_epoch: "fixture-host".into(),
                generation: 1,
            },
        },
        created_at_ms: now,
    };
    let initial = CheckpointRecord {
        checkpoint_id: payload.checkpoint_id.clone(),
        kind: CheckpointKindV1::Recovery,
        session_id: SESSION.into(),
        turn_id: TURN.into(),
        status: "committed".into(),
        done_reason: None,
        updated_at_ms: now,
        payload_json: serde_json::to_string(&payload).unwrap(),
    };
    store.save_checkpoint(initial.clone()).unwrap();
    records.push(state.checkpoint_ref(&initial).unwrap());
    records.push(
        state
            .provider_usage_record(TURN, &ProviderTokenUsageV1::default(), now)
            .unwrap()
            .unwrap(),
    );
    let call = centaeris_core::model::ToolCallEnvelope {
        id: "wait-call".into(),
        name: "durable_fixture_work".into(),
        args_json: "{}".into(),
    };
    records.push(
        state
            .record_tool_call(TURN, &call, "test.poll", &digest, "durable fixture", now)
            .unwrap()
            .unwrap(),
    );
    records.extend(
        state
            .record_tool_result(
                TURN,
                &call,
                &centaeris_core::tool::layer::ToolExecutionResult {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    status: "ok".into(),
                    content: "background accepted".into(),
                    details: serde_json::json!({}),
                    facts: vec![],
                    error: None,
                    started_at_ms: now,
                    completed_at_ms: now,
                    latency_ms: 0,
                    parallel_group: None,
                    transition_reason: None,
                },
                now,
            )
            .unwrap(),
    );
    let receipt = store
        .session_log(
            "workspace_wait_handoff".into(),
            SESSION.into(),
            "wait once".into(),
        )
        .append_session_records_with_runtime_job_lease_blocking(RUN, &records, &fence)
        .unwrap();
    state.set_committed_session_sequence(receipt.records.last().unwrap().sequence);
    assert_eq!(state.committed_session_sequence(), 8);
    assert!(!state.tool_ledger_is_checkpointed());
    let mut source = job("source_wait_handoff", "source_wait_handoff");
    source.job_kind = "provider.poll".into();
    source.session_id = Some(SESSION.into());
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: source })
        .unwrap();
    let wait_payload = RuntimeAwaitJobCheckpointV1::new(
        &RuntimeAgentRunIdentityV1 {
            agent_run_id: RUN.into(),
            execution_id: payload.execution_id.clone(),
            authorization_digest: digest.clone(),
        },
        TURN,
        vec![RuntimeJobWaitV1 {
            tool_call_id: call.id,
            source_tool_name: call.name,
            tool_definition_digest: digest,
            job_id: "source_wait_handoff".into(),
            job_kind: "provider.poll".into(),
        }],
    )
    .unwrap();
    let wait = CheckpointRecord {
        checkpoint_id: format!("checkpoint:{}", wait_payload.continuation_id),
        kind: CheckpointKindV1::Wait,
        session_id: SESSION.into(),
        turn_id: TURN.into(),
        status: "waiting".into(),
        done_reason: Some("runtime_job".into()),
        updated_at_ms: now,
        payload_json: serde_json::to_string(&wait_payload).unwrap(),
    };
    store.save_checkpoint(wait.clone()).unwrap();
    let mut snapshot = SessionStateSnapshot::new(SESSION.into(), now);
    snapshot.metadata.insert(
        "opaque-core-state".into(),
        "durable pending snapshot".into(),
    );
    store
        .save_agent_runtime_snapshot(SESSION, &serde_json::to_string(&snapshot).unwrap(), now)
        .unwrap();
    payload.session_sequence = 9;
    let handoff = WaitHandoff::new(&payload, wait, &snapshot).unwrap();
    payload.checkpoint_id = handoff.checkpoint_id().unwrap();
    let checkpoint = CheckpointRecord {
        checkpoint_id: payload.checkpoint_id.clone(),
        payload_json: serde_json::to_string(&payload).unwrap(),
        ..initial
    };
    Fixture {
        store,
        state,
        fence,
        checkpoint,
        handoff,
    }
}

fn append(f: &mut Fixture) -> Result<centaeris_core::session::SessionCommitReceipt, String> {
    let mut next = f.state.clone();
    let event = next.checkpoint_ref(&f.checkpoint).unwrap();
    f.store
        .session_log(
            "workspace_wait_handoff".into(),
            SESSION.into(),
            "wait once".into(),
        )
        .append_wait_handoff(RUN, &[event], &f.checkpoint, &f.fence, &f.handoff)
}

fn count(f: &Fixture, query: &str) -> i64 {
    f.store
        .with_client(|db| {
            db.query_one(query, &[])
                .map(|r| r.get(0))
                .map_err(|e| e.to_string())
        })
        .unwrap()
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_wait_handoff_crash_before_commit_preserves_uncovered_ledger() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    f.store.with_client(|db|db.batch_execute("CREATE FUNCTION public.reject_wait_handoff() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.payload->'payload'->>'checkpointId' LIKE 'waitcp:%' THEN RAISE EXCEPTION 'injected before checkpoint commit'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_wait_handoff BEFORE INSERT ON public.app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION public.reject_wait_handoff();").map_err(|e|e.to_string())).unwrap();
    assert!(append(&mut f).is_err());
    assert!(f
        .store
        .load_recovery_checkpoint_by_id(&f.checkpoint.checkpoint_id)
        .unwrap()
        .is_none());
    assert!(f
        .store
        .load_wait_handoff(&f.checkpoint.checkpoint_id)
        .unwrap()
        .is_none());
    assert_eq!(count(&f, "SELECT COUNT(*) FROM app_core_sessionevent"), 8);
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM runtime.runtime_job_waiters"),
        1
    );
    f.store.with_client(|db|db.batch_execute("DROP TRIGGER reject_wait_handoff ON public.app_core_sessionevent; DROP FUNCTION public.reject_wait_handoff();").map_err(|e|e.to_string())).unwrap();
    append(&mut f).unwrap();
    assert_eq!(
        f.store
            .load_wait_handoff(&f.checkpoint.checkpoint_id)
            .unwrap(),
        Some(f.handoff.clone())
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_wait_handoff_lost_ack_reopens_immutable_snapshot() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    drop(append(&mut f).unwrap()); // A successful commit whose response was lost.
    f.store
        .save_agent_runtime_snapshot(
            SESSION,
            "{\"unrelatedMutableState\":true}",
            crate::now_ms().unwrap(),
        )
        .unwrap();
    let reopened = PostgresRuntimeStore::new(&test_url()).unwrap();
    let sealed = reopened
        .load_wait_handoff(&f.checkpoint.checkpoint_id)
        .unwrap()
        .expect("immutable wait state survived lost ack");
    assert_eq!(sealed, f.handoff);
    assert_eq!(append(&mut f).unwrap().records.len(), 1);
    assert_eq!(count(&f, "SELECT COUNT(*) FROM app_core_sessionevent"), 9);
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM runtime.runtime_job_waiters"),
        1
    );
    assert_eq!(count(&f,"SELECT COUNT(*) FROM app_core_sessionevent WHERE payload->>'type'='model_request_started'"),1);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_wait_handoff_expired_or_reclaimed_lease_publishes_nothing() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    f.store
        .with_client(|db| {
            db.execute(
                "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=0 WHERE job_id=$1",
                &[&f.fence.job_id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert_eq!(
        append(&mut f).unwrap_err(),
        RUNTIME_JOB_LEASE_FENCE_REJECTED
    );
    f.store.with_client(|db|db.execute("UPDATE runtime.runtime_jobs SET lease_owner='replacement-owner',lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+600000 WHERE job_id=$1",&[&f.fence.job_id]).map(|_|()).map_err(|e|e.to_string())).unwrap();
    assert_eq!(
        append(&mut f).unwrap_err(),
        RUNTIME_JOB_LEASE_FENCE_REJECTED
    );
    assert!(f
        .store
        .load_recovery_checkpoint_by_id(&f.checkpoint.checkpoint_id)
        .unwrap()
        .is_none());
    assert_eq!(count(&f, "SELECT COUNT(*) FROM app_core_sessionevent"), 8);
    assert_eq!(
        count(&f, "SELECT COUNT(*) FROM runtime.runtime_job_waiters"),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_wait_handoff_duplicate_recovery_uses_one_checkpoint() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    let receipt = append(&mut f).unwrap();
    assert_eq!(append(&mut f).unwrap(), receipt);
    assert_eq!(
        f.store
            .load_wait_handoff(&f.checkpoint.checkpoint_id)
            .unwrap(),
        Some(f.handoff.clone())
    );
    let mut reopened = AgentRunSessionState::new(SESSION, RUN).unwrap();
    let records = f.store.with_client(|db| {
        let rows = db.query("SELECT agent_run_sequence,payload::text FROM app_core_sessionevent ORDER BY sequence",&[]).map_err(|e|e.to_string())?;
        let mut wires = rows.iter().map(|row|serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(1))).collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        super::super::runtime::hydrate_session_wire_values(db,&mut wires)?;
        rows.iter().zip(wires).map(|(row,wire)|Ok(SequencedSessionRecord {sequence:row.get::<_,i32>(0) as u64,event:centaeris_core::session::parse_wire_record(&wire).map_err(|e|e.to_string())?.event})).collect::<Result<Vec<_>,String>>()
    }).unwrap();
    for record in records {
        reopened.restore(record).unwrap();
    }
    assert!(reopened.tool_ledger_is_checkpointed());
    reopened
        .end_execution(
            TURN,
            "execution_wait_handoff",
            "lost",
            "execution_environment_lost",
            true,
            Some(&f.checkpoint.checkpoint_id),
            vec![],
            crate::now_ms().unwrap(),
        )
        .unwrap();
    reopened
        .reserve_execution_recovery(
            TURN,
            &f.checkpoint.checkpoint_id,
            5,
            crate::now_ms().unwrap(),
        )
        .unwrap();
    reopened
        .start_execution(
            TURN,
            "execution_replacement",
            &format!("sha256:{}", "a".repeat(64)),
            Some(&f.checkpoint.checkpoint_id),
            crate::now_ms().unwrap(),
        )
        .unwrap();
    assert!(reopened.has_used_recovery_checkpoint(&f.checkpoint.checkpoint_id));
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM app_core_sessionevent WHERE payload->>'type'='tool_call'"
        ),
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM runtime.runtime_jobs WHERE job_kind='provider.poll'"
        ),
        1
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_wait_handoff_attachment_tampering_fails_closed() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    append(&mut f).unwrap();
    let payload: RuntimeRecoveryCheckpointV1 =
        serde_json::from_str(&f.checkpoint.payload_json).unwrap();
    let mut corrupt = f.handoff.clone();
    corrupt.snapshot_json = "{}".into();
    assert!(corrupt.validate(&payload).is_err());
    corrupt = f.handoff.clone();
    corrupt.wait_checkpoint.payload_json = corrupt
        .wait_checkpoint
        .payload_json
        .replace("source_wait_handoff", "another_source_job");
    assert!(corrupt.validate(&payload).is_err());
    let mut altered = serde_json::to_value(&payload).unwrap();
    altered["createdAtMs"] = serde_json::json!(payload.created_at_ms + 1);
    let altered = serde_json::to_string(&altered).unwrap();
    f.store
        .with_client(|db| {
            db.execute(
                "UPDATE runtime.checkpoints SET payload_json=$2 WHERE checkpoint_id=$1",
                &[&f.checkpoint.checkpoint_id, &altered],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(f
        .store
        .load_wait_handoff(&f.checkpoint.checkpoint_id)
        .unwrap_err()
        .contains("reference binding"));
    f.store
        .with_client(|db| {
            db.execute(
                "UPDATE runtime.checkpoints SET payload_json=$2 WHERE checkpoint_id=$1",
                &[&f.checkpoint.checkpoint_id, &f.checkpoint.payload_json],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .unwrap();
    f.store
        .with_client(|db| {
            db.execute(
                "UPDATE runtime.checkpoints SET wait_handoff_json=NULL WHERE checkpoint_id=$1",
                &[&f.checkpoint.checkpoint_id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(WaitHandoff::is_wait_checkpoint(&f.checkpoint.checkpoint_id));
    assert!(f
        .store
        .load_wait_handoff(&f.checkpoint.checkpoint_id)
        .unwrap()
        .is_none());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_wait_handoff_source_change_rolls_back_publication() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    f.store.with_client(|db|db.execute("UPDATE runtime.runtime_jobs SET session_id='another_session' WHERE job_id='source_wait_handoff'",&[]).map(|_|()).map_err(|e|e.to_string())).unwrap();
    assert!(append(&mut f)
        .unwrap_err()
        .contains("source job or receipt binding"));
    assert!(f
        .store
        .load_recovery_checkpoint_by_id(&f.checkpoint.checkpoint_id)
        .unwrap()
        .is_none());
    assert_eq!(count(&f, "SELECT COUNT(*) FROM app_core_sessionevent"), 8);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_wait_handoff_model_anchor_is_reobserved_before_publication() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    assert_eq!(
        f.store.latest_model_request_id(SESSION, RUN, TURN).unwrap(),
        f.handoff.model_request_id
    );
    for (session, run, turn) in [
        ("another-session", RUN, TURN),
        (SESSION, "another-run", TURN),
        (SESSION, RUN, "another-turn"),
    ] {
        assert_eq!(
            f.store
                .latest_model_request_id(session, run, turn)
                .unwrap_err(),
            "wait handoff has no real model request"
        );
    }
    for (request, purpose, error) in [
        (
            "changed-model-request",
            "main",
            "wait handoff model request changed",
        ),
        (
            "request_wait_handoff",
            "other",
            "wait handoff model request missing",
        ),
    ] {
        f.store
            .with_client(|db| {
                db.execute(
                    "UPDATE app_core_sessionevent SET payload=jsonb_set(jsonb_set(payload,'{payload,requestId}',to_jsonb($1::text)),'{payload,purpose}',to_jsonb($2::text)) WHERE session_id=$3 AND sequence=4",
                    &[&request, &purpose, &SESSION],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(append(&mut f).unwrap_err(), error);
        assert!(f
            .store
            .load_recovery_checkpoint_by_id(&f.checkpoint.checkpoint_id)
            .unwrap()
            .is_none());
        assert_eq!(count(&f, "SELECT COUNT(*) FROM app_core_sessionevent"), 8);
        assert_eq!(
            count(&f, "SELECT COUNT(*) FROM runtime.runtime_job_waiters"),
            1
        );
    }
    f.store
        .with_client(|db| {
            db.execute(
                "UPDATE app_core_sessionevent SET payload=jsonb_set(jsonb_set(payload,'{payload,requestId}',to_jsonb($1::text)),'{payload,purpose}',to_jsonb($2::text)) WHERE session_id=$3 AND sequence=4",
                &[&"request_wait_handoff", &"main", &SESSION],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        })
        .unwrap();
    append(&mut f).unwrap();
    assert_eq!(count(&f, "SELECT COUNT(*) FROM app_core_sessionevent"), 9);
}

fn observation_versions(f: &Fixture) -> Vec<i64> {
    f.store
        .with_client(|db| {
            db.query(
                "SELECT version FROM runtime.schema_migrations ORDER BY version",
                &[],
            )
            .map(|rows| rows.iter().map(|row| row.get(0)).collect())
            .map_err(|error| error.to_string())
        })
        .unwrap()
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_completion_delivery_upgrade_preserves_records_and_accepts_core_observation_kind() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    append(&mut f).unwrap();
    f.store.with_client(|db| db.batch_execute(
        "DELETE FROM runtime.schema_migrations WHERE version>=5;
         DROP TABLE runtime.resident_sandboxes;
         ALTER TABLE runtime.runtime_jobs DROP COLUMN lease_reclaim_count;
         ALTER TABLE runtime.runtime_jobs DROP COLUMN lease_reclaim_not_before_ms;
         ALTER TABLE runtime.model_observation_contents DROP CONSTRAINT model_observation_contents_kind_check;
         ALTER TABLE runtime.model_observation_contents ADD CONSTRAINT model_observation_contents_kind_check CHECK(kind IN('system_prompt','message','input_image','tool_catalog','compaction_prompt','input_uptake'));"
    ).map_err(|error| error.to_string())).unwrap();
    let before = legacy_retained_rows(&f);
    let upgraded = PostgresRuntimeStore::new(&test_url()).unwrap();
    assert_eq!(observation_versions(&f), [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(legacy_retained_rows(&f), before);
    upgraded.with_client(|db| db.execute(
        "INSERT INTO runtime.model_observation_contents VALUES($1,$2,'required_completion_delivery','{}',2,1)",
        &[&SESSION, &format!("sha256:{}", "a".repeat(64))]
    ).map(|_| ()).map_err(|error| error.to_string())).unwrap();
    let after = retained_rows(&f);
    let _reopened = PostgresRuntimeStore::new(&test_url()).unwrap();
    assert_eq!(retained_rows(&f), after);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_completion_delivery_upgrade_rejects_unknown_legacy_constraint_without_changes() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let f = fixture();
    f.store.with_client(|db| db.batch_execute(
        "DELETE FROM runtime.schema_migrations WHERE version>=5;
         DROP TABLE runtime.resident_sandboxes;
         ALTER TABLE runtime.runtime_jobs DROP COLUMN lease_reclaim_count;
         ALTER TABLE runtime.runtime_jobs DROP COLUMN lease_reclaim_not_before_ms;
         ALTER TABLE runtime.model_observation_contents DROP CONSTRAINT model_observation_contents_kind_check;
         ALTER TABLE runtime.model_observation_contents ADD CONSTRAINT model_observation_contents_kind_check CHECK(kind<>'unknown');"
    ).map_err(|error| error.to_string())).unwrap();
    let before = retained_rows(&f);
    assert!(PostgresRuntimeStore::new(&test_url()).is_err());
    assert_eq!(observation_versions(&f), [1, 2, 3, 4]);
    assert_eq!(retained_rows(&f), before);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_current_schema_reopen_retains_sealed_wait_handoff() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut f = fixture();
    append(&mut f).unwrap();
    let before = retained_rows(&f);
    let reopened = PostgresRuntimeStore::new(&test_url()).unwrap();
    assert_eq!(observation_versions(&f), [1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(retained_rows(&f), before);
    assert_eq!(
        reopened
            .load_wait_handoff(&f.checkpoint.checkpoint_id)
            .unwrap(),
        Some(f.handoff.clone())
    );
}

fn retained_rows(f: &Fixture) -> std::collections::BTreeMap<String, String> {
    f.store.with_client(|db| {
        let mut tables = db.query("SELECT tablename FROM pg_tables WHERE schemaname='runtime' AND tablename<>'schema_migrations' ORDER BY tablename", &[])
            .map_err(|error| error.to_string())?.iter()
            .map(|row| format!("runtime.{}", row.get::<_, String>(0))).collect::<Vec<_>>();
        tables.push("public.app_core_sessionevent".into());
        tables.into_iter().map(|table| {
            let rows = db.query_one(&format!("SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text FROM {table} t"), &[])
                .map_err(|error| error.to_string())?.get::<_, String>(0);
            Ok((table, rows))
        }).collect()
    }).unwrap()
}

fn legacy_retained_rows(f: &Fixture) -> std::collections::BTreeMap<String, String> {
    let mut rows = retained_rows(f);
    if let Some(residents) = rows.remove("runtime.resident_sandboxes") {
        assert_eq!(
            residents, "[]",
            "forward migration must start without invented resident facts"
        );
    }
    let jobs = rows.get_mut("runtime.runtime_jobs").unwrap();
    let mut values: Vec<serde_json::Value> = serde_json::from_str(jobs).unwrap();
    for value in &mut values {
        let value = value.as_object_mut().unwrap();
        if let Some(count) = value.remove("lease_reclaim_count") {
            assert_eq!(count, 0);
        }
        if let Some(deadline) = value.remove("lease_reclaim_not_before_ms") {
            assert!(deadline.is_null());
        }
    }
    *jobs = serde_json::to_string(&values).unwrap();
    rows
}

fn hydrated_events(f: &Fixture) -> Vec<serde_json::Value> {
    f.store
        .with_client(|db| {
            let rows = db
                .query(
                    "SELECT payload::text FROM public.app_core_sessionevent ORDER BY sequence",
                    &[],
                )
                .map_err(|error| error.to_string())?;
            let mut wires = rows
                .iter()
                .map(|row| serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(0)))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            super::super::runtime::hydrate_session_wire_values(db, &mut wires)?;
            for wire in &wires {
                centaeris_core::session::parse_wire_record(wire)
                    .map_err(|error| error.to_string())?;
            }
            Ok(wires)
        })
        .unwrap()
}

fn seed_retained_observations(f: &Fixture) {
    let template = hydrated_events(f)[3]["payload"].clone();
    let log = f.store.session_log(
        "workspace_wait_handoff".into(),
        SESSION.into(),
        "wait once".into(),
    );
    for (index, observations) in [
        serde_json::json!([
            {"kind":"system_prompt","content":"  retained system prompt\n"},
            {"kind":"message","message":{"messageId":"retained-message","role":"user","content":"  retained original body\n"}},
            {"kind":"input_image","image":{"messageId":"retained-message","source":{"sourceKind":"inputRef","inputRef":"retained-image","contentType":"image/png","placeholder":"[retained image]"}}},
            {"kind":"tool_catalog","toolDefinitions":[{"name":"retained_tool","description":"retained catalog","inputSchema":{"type":"object"}}]}
        ]),
        serde_json::json!([
            {"kind":"system_prompt","content":"  retained system prompt\n"},
            {"kind":"message","message":{"messageId":"retained-next-message","role":"user","content":"  retained next body\n"}},
            {"kind":"input_image","image":{"messageId":"retained-next-message","source":{"sourceKind":"inputRef","inputRef":"retained-image","contentType":"image/png","placeholder":"[retained image]"}}},
            {"kind":"tool_catalog","toolDefinitions":[{"name":"retained_tool","description":"retained catalog","inputSchema":{"type":"object"}}]}
        ]),
        serde_json::json!([{"kind":"compaction_prompt","message":{"messageId":"retained-compaction","role":"user","content":"  retained compaction body\n"}}]),
    ].into_iter().enumerate() {
        let mut payload = template.clone();
        payload["requestId"] = serde_json::json!(format!("retained-request-{index}"));
        payload["purpose"] = serde_json::json!(if index == 2 { "compaction" } else { "main" });
        payload["observations"] = observations;
        let record = session_record(RUN, SESSION, 9 + index as u64, SessionRecordType::ModelRequestStarted, payload, crate::now_ms().unwrap());
        log.append_session_records_with_runtime_job_lease_blocking(RUN, &[record], &f.fence).unwrap();
    }
    assert_eq!(
        count(
            f,
            "SELECT COUNT(DISTINCT kind) FROM runtime.model_observation_contents"
        ),
        5
    );
    assert!(count(f, "SELECT COUNT(*) FROM runtime.model_observation_manifests WHERE parent_digest IS NOT NULL") > 0);
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_current_schema_reopens_observations_and_checkpoint_owners() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let f = fixture();
    seed_retained_observations(&f);
    let supported_versions = observation_versions(&f);
    let before = retained_rows(&f);
    let hydrated_before = hydrated_events(&f);
    for _ in 0..3 {
        PostgresRuntimeStore::new(&test_url()).unwrap();
        assert_eq!(observation_versions(&f), supported_versions);
        assert_eq!(retained_rows(&f), before);
        assert_eq!(hydrated_events(&f), hydrated_before);
    }
    let mut payload = hydrated_before[3]["payload"].clone();
    payload["requestId"] = serde_json::json!("retained-new-uptake");
    payload["observations"] =
        serde_json::json!([{"kind":"input_uptake","inputIds":["retained-input"]}]);
    let record = session_record(
        RUN,
        SESSION,
        12,
        SessionRecordType::ModelRequestStarted,
        payload,
        crate::now_ms().unwrap(),
    );
    f.store
        .session_log(
            "workspace_wait_handoff".into(),
            SESSION.into(),
            "wait once".into(),
        )
        .append_session_records_with_runtime_job_lease_blocking(RUN, &[record], &f.fence)
        .unwrap();
    assert_eq!(
        count(
            &f,
            "SELECT COUNT(*) FROM runtime.model_observation_contents WHERE kind='input_uptake'"
        ),
        1
    );
    PostgresRuntimeStore::new(&test_url()).unwrap();
    assert_eq!(
        hydrated_events(&f).last().unwrap()["payload"]["observations"][0]["inputIds"],
        serde_json::json!(["retained-input"])
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_current_schema_rejects_unsupported_versions_and_drift_without_writes() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for (ddl, expected_error) in [
        ("DELETE FROM runtime.schema_migrations WHERE version>1", "schema version mismatch"),
        ("DELETE FROM runtime.schema_migrations WHERE version>2", "schema version mismatch"),
        ("DELETE FROM runtime.schema_migrations WHERE version>3", "schema version mismatch"),
        ("unsupported_next_version", "schema version mismatch"),
        ("DELETE FROM runtime.schema_migrations WHERE version=2", "schema version mismatch"),
        ("ALTER TABLE runtime.model_observation_contents DROP CONSTRAINT model_observation_contents_kind_check; ALTER TABLE runtime.model_observation_contents ADD CONSTRAINT model_observation_contents_kind_check CHECK(kind<>'unknown')", "model observation kind constraint mismatch"),
        ("ALTER TABLE runtime.model_observation_contents DROP CONSTRAINT model_observation_contents_kind_check", "model observation kind constraint mismatch"),
        ("CREATE TABLE runtime.unknown_observation_shape(id text)", "table set mismatch"),
        ("DROP INDEX runtime.idx_checkpoints_wait_handoff", "index set mismatch"),
        ("ALTER TABLE runtime.checkpoints DROP COLUMN wait_handoff_json", "table definition mismatch"),
    ] {
        let mut f = fixture();
        append(&mut f).unwrap();
        let ddl = if ddl == "unsupported_next_version" {
            format!("INSERT INTO runtime.schema_migrations VALUES({},0)",
                observation_versions(&f).last().copied().unwrap() + 1)
        } else { ddl.to_string() };
        f.store.with_client(|db| db.batch_execute(&ddl).map_err(|error| error.to_string())).unwrap();
        let versions = observation_versions(&f);
        let before = retained_rows(&f);
        let shape = f.store.with_client(|db| db.query("SELECT table_name,column_name,data_type,is_nullable FROM information_schema.columns WHERE table_schema='runtime' ORDER BY table_name,ordinal_position", &[])
            .map(|rows| rows.iter().map(|row| (row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, String>(3))).collect::<Vec<_>>()).map_err(|error| error.to_string())).unwrap();
        let error = PostgresRuntimeStore::new(&test_url()).expect_err("unsupported schema must reject");
        assert!(error.contains(expected_error), "{ddl}: {error}");
        assert_eq!(observation_versions(&f), versions);
        assert_eq!(retained_rows(&f), before);
        let after_shape = f.store.with_client(|db| db.query("SELECT table_name,column_name,data_type,is_nullable FROM information_schema.columns WHERE table_schema='runtime' ORDER BY table_name,ordinal_position", &[])
            .map(|rows| rows.iter().map(|row| (row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, String>(2), row.get::<_, String>(3))).collect::<Vec<_>>()).map_err(|error| error.to_string())).unwrap();
        assert_eq!(after_shape, shape);
    }
}
