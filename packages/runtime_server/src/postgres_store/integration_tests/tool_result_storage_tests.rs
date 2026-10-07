use super::*;
use centaeris_core::model::ToolCallEnvelope;
use centaeris_core::session::AgentRunSessionState;
use centaeris_core::tool::layer::ToolExecutionResult;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_new_turn_recovers_control_byte_output_from_core_receipt_without_reexecuting() {
    use centaeris_core::runtime::{AgentRuntime, AgentRuntimeConfig, ToolConcurrencyCoordinator};
    use sha2::{Digest, Sha256};

    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.batch_execute(r#"
        DROP TABLE IF EXISTS public.app_core_sessionevent CASCADE;
        DROP TABLE IF EXISTS public.app_core_session CASCADE;
        CREATE TABLE public.app_core_session(id text PRIMARY KEY, workspace_id text NOT NULL);
        CREATE TABLE public.app_core_sessionevent("eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
        INSERT INTO public.app_core_session VALUES('session_receipt','workspace_receipt');
        INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,run_at_ms,lease_owner,lease_expires_at_ms,backoff_policy_json,idempotency_key,session_id,created_at_ms,updated_at_ms)
        VALUES('receipt_job','agent_run.lifecycle','running',0,'receipt_owner',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint + 60000,'{}','receipt_key','session_receipt',1,1);
    "#).unwrap();
    let session = "session_receipt";
    let old_run = "run_old_receipt";
    let turn = "turn_old_receipt";
    let call = ToolCallEnvelope {
        id: "call_receipt".into(),
        name: "bash".into(),
        args_json: r#" { "description" : "synthetic", "command" : "synthetic" } "#.into(),
    };
    let tools = crate::tests::hosted_context().tool_layer;
    let contract = tools.tool_contract("bash").unwrap();
    let provider = contract.provider_id.clone().unwrap();
    let contract_digest = contract.contract_digest().unwrap();
    let mut old = AgentRunSessionState::new(session, old_run).unwrap();
    let mut records = old.start(old_run, "Probe", vec![], 1).unwrap();
    records.extend(
        old.record_tool_call(turn, &call, &provider, &contract_digest, "Synthetic", 2)
            .unwrap(),
    );
    records.push(
        old.record(
            centaeris_core::session::failed_agent_run_record(
                session,
                turn,
                old_run,
                "core_run_failed",
                "Commit failed",
                3,
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let rt = tokio::runtime::Runtime::new().unwrap();
    let log = store.session_log("workspace_receipt".into(), session.into(), "Probe".into());
    rt.block_on(log.append_session_records(old_run, &records))
        .unwrap();
    let result = ToolExecutionResult {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status: "ok".into(),
        content: "kernel\0 literal \\u0000 中文".into(),
        details: serde_json::json!({}),
        facts: vec![],
        error: None,
        started_at_ms: 2,
        completed_at_ms: 3,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: None,
    };
    let args_digest = format!("sha256:{:x}", Sha256::digest(call.args_json.as_bytes()));
    let identity = format!("{session}\0{turn}\0{}", call.id);
    let event_digest = format!("sha256:{:x}", Sha256::digest(identity.as_bytes()));
    let intent = serde_json::json!({"schema":"tool_execution.intent.v1","sessionId":session,"turnId":turn,
        "toolCallId":call.id,"sourceToolName":call.name,"sessionToolCallEventId":centaeris_core::runtime::canonical_tool_call_event_id(session, turn, &call.id),
        "providerId":provider,"toolContractDigest":contract_digest,"modelArgsDigest":args_digest,"argsDigest":args_digest,
        "effectiveArgsJson":call.args_json,"recordedAtMs":2});
    let receipt = serde_json::json!({"schema":"tool_execution.receipt.v1","sessionId":session,"turnId":turn,
        "toolCallId":call.id,"sourceToolName":call.name,"argsDigest":args_digest,"effectiveArgsJson":call.args_json,
        "preHookContexts":[],"runPostHook":false,"resultJson":serde_json::to_string(&result).unwrap()});
    for (kind, payload, time) in [("intent", intent, 2), ("receipt", receipt, 3)] {
        store
            .append_event_idempotent(RuntimeEvent {
                event_id: format!("tool_execution.{kind}:{event_digest}"),
                session_id: session.into(),
                task_id: Some(turn.into()),
                event_type: format!("tool_execution.{kind}.v1"),
                at_ms: time,
                visibility: EventVisibility::Internal,
                payload_json: payload.to_string(),
            })
            .unwrap();
    }
    // The real Core planner reads its own TEXT receipt. No tool or model is invoked.
    let engine = AgentRuntime::new(
        store.clone(),
        tools,
        AgentRuntimeConfig::default(),
        ToolConcurrencyCoordinator::new(1),
    );
    let source = records
        .iter()
        .map(|record| record.event.clone())
        .collect::<Vec<_>>();
    let plan = engine
        .plan_new_user_turn_closures(
            session,
            &source,
            "run_new_receipt",
            "turn_new_receipt",
            records.len() as u64,
            4,
        )
        .unwrap();
    assert_eq!(plan.closures.len(), 1);
    assert_eq!(plan.closures[0].recovery, "receipt");
    assert_eq!(plan.closures[0].result.content, result.content);
    assert_eq!(plan.closures[0].result.status, "ok");
    assert!(plan.evidence_preconditions[0].receipt_present);
    let closure = &plan.closures[0];
    let canonical = centaeris_core::runtime::canonical_tool_call_closure_record(
        session,
        turn,
        old_run,
        &closure.call,
        &closure.result,
        &closure.recovery,
        closure.call_event_id.as_deref(),
        "run_new_receipt",
        "turn_new_receipt",
        4,
    )
    .unwrap();
    let mut next = AgentRunSessionState::new(session, "run_new_receipt").unwrap();
    let new_records = next
        .start("run_new_receipt", "Continue", vec![], 5)
        .unwrap();
    let actor = rt.block_on(async {
        centaeris_core::session::store::RuntimeStoreActor::start(store.clone()).unwrap()
    });
    let manager = centaeris_core::session::manager::SessionManager::new(actor.clone());
    let stale =
        centaeris_core::session::restore_runtime_snapshot_from_session_records(session, &source)
            .unwrap();
    assert!(!stale.model_semantics.values().any(|semantic| matches!(semantic,
        centaeris_core::session::state::ModelMessageSemanticsV1::ToolResult { tool_call_id, .. } if tool_call_id == &call.id)));
    manager.save_session(&stale).unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id: "receipt_job".into(),
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: "receipt_owner".into(),
    };
    for _ in 0..2 {
        log.append_new_user_turn_admission_blocking(
            "run_new_receipt",
            &new_records,
            std::slice::from_ref(&canonical),
            &plan,
            &fence,
        )
        .unwrap();
    }
    let head: i32 = db
        .query_one(
            "SELECT MAX(sequence) FROM app_core_sessionevent WHERE session_id=$1",
            &[&session],
        )
        .unwrap()
        .get(0);
    crate::refresh_runtime_state_after_admission(&store, &actor, session, head as u64).unwrap();
    let restored = manager.load_session(session).unwrap().unwrap();
    assert!(restored.model_semantics.values().any(|semantic| matches!(semantic,
        centaeris_core::session::state::ModelMessageSemanticsV1::ToolResult { tool_call_id, .. } if tool_call_id == &call.id)),
        "admitted closure must replace the stale snapshot before Core runs the new AgentRun");
    assert!(
        restored
            .messages
            .iter()
            .any(|message| message.content.contains(&result.content)),
        "recovered tool output must be preserved byte for byte"
    );
    let raw: String = db
        .query_one(
            "SELECT payload::text FROM app_core_sessionevent WHERE \"eventId\"=$1",
            &[&canonical.event_id],
        )
        .unwrap()
        .get(0);
    let mut wire: serde_json::Value = serde_json::from_str(&raw).unwrap();
    super::super::session_payload::decode(&mut wire).unwrap();
    assert_eq!(wire["payload"]["modelContent"], result.content);
    assert_eq!(wire["payload"]["recovery"], "receipt");
    let count: i64 = db.query_one("SELECT COUNT(*) FROM runtime.runtime_events WHERE event_type='tool_execution.receipt.v1'", &[]).unwrap().get(0);
    assert_eq!(
        count, 1,
        "readmission retains the one authoritative receipt"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_tool_result_control_bytes_preserve_receipts_and_allow_run_completion() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.batch_execute(r#"
        DROP TABLE IF EXISTS public.app_core_sessionevent CASCADE;
        DROP TABLE IF EXISTS public.app_core_session CASCADE;
        CREATE TABLE public.app_core_session(id text PRIMARY KEY, workspace_id text NOT NULL);
        CREATE TABLE public.app_core_sessionevent("eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
        INSERT INTO public.app_core_session VALUES('session_control_bytes','workspace_control_bytes');
    "#).unwrap();
    let session = "session_control_bytes";
    let run = "run_control_bytes";
    let turn = "turn_control_bytes";
    let mut state = AgentRunSessionState::new(session, run).unwrap();
    let mut records = state.start(run, "Probe", vec![], 1).unwrap();
    let calls: Vec<_> = (0..3)
        .map(|i| ToolCallEnvelope {
            id: format!("call_{i}"),
            name: "bash".into(),
            args_json: r#"{"command":"synthetic"}"#.into(),
        })
        .collect();
    for call in &calls {
        records.extend(
            state
                .record_tool_call(
                    turn,
                    call,
                    "centaeris.builtin",
                    &format!("sha256:{}", "c".repeat(64)),
                    "Synthetic probe",
                    2,
                )
                .unwrap(),
        );
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let log = store.session_log(
        "workspace_control_bytes".into(),
        session.into(),
        "Probe".into(),
    );
    rt.block_on(log.append_session_records(run, &records))
        .unwrap();
    let mut results = Vec::new();
    for (i, call) in calls.iter().enumerate() {
        let result = ToolExecutionResult {
            tool_call_id: call.id.clone(),
            tool_name: "bash".into(),
            status: if i == 1 { "error" } else { "ok" }.into(),
            content: if i == 2 {
                "kernel\0 literal \\u0000"
            } else if i == 1 {
                "Permission denied"
            } else {
                "ok"
            }
            .into(),
            details: serde_json::json!({}),
            facts: vec![],
            error: None,
            started_at_ms: 3,
            completed_at_ms: 4,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: None,
        };
        results.extend(state.record_tool_result(turn, call, &result, 4).unwrap());
    }
    db.batch_execute(r#"
        DROP SEQUENCE IF EXISTS session_commit_attempts;
        CREATE SEQUENCE session_commit_attempts;
        CREATE OR REPLACE FUNCTION transient_session_commit() RETURNS trigger LANGUAGE plpgsql AS $$
        DECLARE attempt bigint;
        BEGIN
          IF NEW.payload->>'type'='tool_result' THEN
            attempt := nextval('session_commit_attempts');
            IF attempt = 1 THEN
              RAISE EXCEPTION 'synthetic storage rejection' USING ERRCODE='22000';
            ELSIF attempt <= 3 THEN
              RAISE EXCEPTION 'synthetic transient commit' USING ERRCODE='40001';
            END IF;
          END IF;
          RETURN NEW;
        END $$;
        CREATE TRIGGER transient_session_commit BEFORE INSERT ON app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION transient_session_commit();
    "#).unwrap();
    let pending = rt
        .block_on(log.append_session_records(run, &results))
        .unwrap_err();
    assert!(crate::session_delivery::pending(&pending));
    let terminal_count: i64 = db
        .query_one(
            "SELECT COUNT(*) FROM app_core_sessionevent WHERE payload->>'type'='agent_run_failed'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(
        terminal_count, 0,
        "SQL rejection is not a Core terminal fact"
    );
    crate::session_delivery::retry(
        || rt.block_on(log.append_session_records(run, &results)),
        |_| {},
    )
    .expect("NUL output and transient database rejection must not abort the Run");
    rt.block_on(log.append_session_records(run, &results))
        .expect("replaying a durable result commit must be idempotent");
    let tail = vec![
        state
            .assistant(
                turn,
                "Probe finished despite one tool failure",
                vec![],
                "done",
                5,
            )
            .unwrap()
            .unwrap(),
        state.complete(turn, "done", 6).unwrap(),
    ];
    rt.block_on(log.append_session_records(run, &tail)).unwrap();
    let count: i64 = db
        .query_one(
            "SELECT COUNT(*) FROM app_core_sessionevent WHERE payload->>'type'='tool_result'",
            &[],
        )
        .unwrap()
        .get(0);
    assert_eq!(count, 3, "each tool result is committed exactly once");
    let raw: String = db.query_one("SELECT payload::text FROM app_core_sessionevent WHERE payload->>'type'='tool_result' AND payload#>>'{payload,callId}'='call_2'", &[]).unwrap().get(0);
    assert!(!raw.contains('\0'));
    let mut wires = vec![serde_json::from_str(&raw).unwrap()];
    super::super::runtime::hydrate_session_wire_values(&mut db, &mut wires).unwrap();
    assert_eq!(
        wires[0]["payload"]["modelContent"],
        "kernel\0 literal \\u0000"
    );
    assert_eq!(
        wires[0]["payload"]["resultState"],
        results.last().unwrap().event.payload["resultState"],
        "storage must retain Core's actual tool outcome"
    );
    let failed: String = db.query_one("SELECT payload#>>'{payload,resultState}' FROM app_core_sessionevent WHERE payload#>>'{payload,callId}'='call_1' AND payload->>'type'='tool_result'", &[]).unwrap().get(0);
    assert_eq!(failed, "failed");
    store
        .catch_up_transcript_projection(session, "control_bytes_generation", 10)
        .unwrap();
}
