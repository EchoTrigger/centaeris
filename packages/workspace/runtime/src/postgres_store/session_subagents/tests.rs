use super::*;
use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};
use centaeris_core::session::external_context::{ExternalContextObject, ExternalContextStorePort};
use centaeris_core::session::reliability::{RuntimeJobStorePort, ScheduleRuntimeJobRequest};
use centaeris_core::session::state::ChatMessage;
use centaeris_core::session::store::AgentRuntimeSnapshotStorePort;
use serde_json::json;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_subagent_presentation_source_is_atomic_and_status_only() {
    use crate::postgres_store::integration_tests::{retained_instance_tests, TEST_LOCK};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = retained_instance_tests::setup();
    let (job, _) = job_and_metadata();
    let dirty = |db: &mut postgres::Client| -> i64 {
        db.query_one("SELECT count(*) FROM public.app_core_presentationchangeoutbox WHERE key='session:sess_1:subagents'", &[])
            .unwrap().get(0)
    };
    let mut other = job.clone();
    other.job_id = "other-work".into();
    other.job_kind = "worker.noop".into();
    other.idempotency_key = "other-work".into();
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: other })
        .unwrap();
    assert_eq!(dirty(&mut db), 0);
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: job.clone() })
        .unwrap();
    assert_eq!(dirty(&mut db), 1);
    db.execute("UPDATE runtime.runtime_jobs SET heartbeat_at_ms=2,lease_expires_at_ms=100,updated_at_ms=2 WHERE job_id=$1", &[&job.job_id]).unwrap();
    db.execute(
        "UPDATE runtime.runtime_jobs SET status=status WHERE job_id=$1",
        &[&job.job_id],
    )
    .unwrap();
    assert_eq!(dirty(&mut db), 1);
    let mut tx = db.transaction().unwrap();
    tx.execute(
        "UPDATE runtime.runtime_jobs SET status='leased' WHERE job_id=$1",
        &[&job.job_id],
    )
    .unwrap();
    tx.execute(
        "UPDATE runtime.runtime_jobs SET status='running' WHERE job_id=$1",
        &[&job.job_id],
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(
        dirty(&mut db),
        2,
        "one dirty row for one source transaction"
    );
    let mut tx = db.transaction().unwrap();
    tx.execute(
        "UPDATE runtime.runtime_jobs SET status='succeeded' WHERE job_id=$1",
        &[&job.job_id],
    )
    .unwrap();
    tx.rollback().unwrap();
    assert_eq!(dirty(&mut db), 2);
    assert_eq!(
        store.get_runtime_job(&job.job_id).unwrap().unwrap().status,
        RuntimeJobStatus::Running
    );
    db.execute(
        "DELETE FROM runtime.runtime_jobs WHERE job_id=$1",
        &[&job.job_id],
    )
    .unwrap();
    assert_eq!(dirty(&mut db), 3);
    db.batch_execute("DROP FUNCTION public.workspace_presentation_bump_v1(text)")
        .unwrap();
    assert!(store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: job.clone() })
        .is_err());
    assert!(
        store.get_runtime_job(&job.job_id).unwrap().is_none(),
        "missing source ledger cannot silently commit a Job"
    );
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_subagent_presentation_migrates_nineteen_forward_and_rejects_trigger_drift() {
    use crate::postgres_store::integration_tests::{retained_instance_tests, TEST_LOCK};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = retained_instance_tests::setup();
    let (job, _) = job_and_metadata();
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: job.clone() })
        .unwrap();
    let before = db.query("SELECT version,applied_at_ms FROM runtime.schema_migrations WHERE version<=19 ORDER BY version", &[])
        .unwrap().into_iter().map(|row|(row.get::<_,i64>(0),row.get::<_,i64>(1))).collect::<Vec<_>>();
    db.batch_execute("DROP TRIGGER runtime_subagent_presentation_v1 ON runtime.runtime_jobs; DROP FUNCTION runtime.notify_subagent_presentation_v1(); DELETE FROM runtime.schema_migrations WHERE version=20").unwrap();
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    let after = db
        .query(
            "SELECT version,applied_at_ms FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .unwrap()
        .into_iter()
        .map(|row| (row.get::<_, i64>(0), row.get::<_, i64>(1)))
        .collect::<Vec<_>>();
    assert_eq!(&after[..19], before.as_slice());
    assert_eq!(after.len(), 20);
    assert_eq!(after[19].0, 20);
    assert_eq!(reopened.get_runtime_job(&job.job_id).unwrap().unwrap(), job);
    db.batch_execute(
        "ALTER TABLE runtime.runtime_jobs DISABLE TRIGGER runtime_subagent_presentation_v1",
    )
    .unwrap();
    assert!(
        PostgresRuntimeStore::new(&url).is_err(),
        "disabled source trigger must fail strict schema validation"
    );
}

fn packet() -> Value {
    let contract = centaeris_core::tool::list_tool_contracts()
        .into_iter()
        .find(|contract| contract.name == "read")
        .unwrap();
    json!({
        "run_context":{"schema":"agent_run_context_v1","sessionId":"sess_1","branchId":"child-one","turnId":"child-turn","agentRunId":"child-run","agentRef":{"agentId":"child-agent","agentRunId":"child-run"},"parentAgentRef":{"agentId":"main","agentRunId":"parent-run"},"parentTurnId":"parent-turn","depth":1,"cwd":std::env::current_dir().unwrap().display().to_string(),"cancellationScopeId":"child-scope","parentCancellationScopeId":"parent-scope","cancelOnParentCancel":true,"createdAtMs":1},
        "task_brief":{"task_id":null,"objective":"Read the source","success_criteria":[],"constraints":[],"output_hint":"Inspect source"},
        "hot_view":{"summary":"","recent_message_ids":[],"state_kv":{}},"object_refs":[],
        "allowedTools":["read"],"delegatedToolContracts":[{"name":"read","providerId":contract.provider_id,"contractDigest":contract.contract_digest().unwrap(),"concurrencySafe":contract.concurrency_safe}],"writablePathPrefixes":[],
        "output_contract":{"response_mode":"summary","expected_sections":[],"require_artifact_refs":false,"max_summary_chars":null},"parent_checkpoint_id":null,"context_mode":"Borrow"
    })
}

fn job_and_metadata() -> (RuntimeJobRecord, Value) {
    let job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: "sess_1".into(),
        parent_turn_id: "parent-turn".into(),
        tool_call_id: "call-agent".into(),
        subagent_id: "child-agent".into(),
        work_packet_ref: "work-packet-one".into(),
        checkpoint_id: None,
        run_at_ms: 1,
        created_at_ms: 1,
        max_retries: 0,
    });
    let metadata = json!({"schema":"agent_tool_work_packet_v1","sessionId":"sess_1",
        "parentTurnId":"parent-turn","childTurnId":"child-turn","toolCallId":"call-agent",
        "childSessionId":"child-one","subagentId":"child-agent","runtimeJobId":job.job_id,
        "resultRef":"result-one"});
    (job, metadata)
}

fn snapshot() -> SessionStateSnapshot {
    let mut snapshot = SessionStateSnapshot::new("child-one".into(), 12);
    snapshot.messages = vec![
        ChatMessage {
            message_id: "assistant-one".into(),
            role: MessageRole::Assistant,
            content: "I will read it".into(),
            created_at_ms: 10,
            metadata: Default::default(),
        },
        ChatMessage {
            message_id: "tool-one".into(),
            role: MessageRole::Tool,
            content: "source bytes".into(),
            created_at_ms: 11,
            metadata: Default::default(),
        },
    ];
    snapshot.model_semantics.insert(
        "assistant-one".into(),
        ModelMessageSemanticsV1::Assistant {
            reasoning_content: Some("Check source".into()),
            tool_calls: vec![ModelToolCallStateV1 {
                id: "call-read".into(),
                name: "read".into(),
                args_json: "{\"path\":\"README.md\"}".into(),
            }],
        },
    );
    snapshot.model_semantics.insert(
        "tool-one".into(),
        ModelMessageSemanticsV1::ToolResult {
            tool_call_id: "call-read".into(),
            tool_name: "read".into(),
            status: "success".into(),
            result_state: "successWithOutput".into(),
            error_kind: None,
            object_refs: vec![],
            transition_reason: None,
        },
    );
    snapshot
}

#[test]
fn subagent_read_uses_core_binding_and_exact_work_packet_call() {
    let (job, metadata) = job_and_metadata();
    let agent = present_agent(
        &job,
        "sess_1",
        json!({"workPacket":packet()}),
        metadata.clone(),
    )
    .unwrap();
    let value = serde_json::to_value(agent).unwrap();
    assert_eq!(value["callId"], "call-agent");
    assert_eq!(value["prompt"], "Read the source");
    assert_eq!(value["description"], "Inspect source");
    assert_eq!(value["parentAgentRunId"], "parent-run");
    assert_eq!(value["status"], "queued");
    for field in [
        "sessionId",
        "runtimeJobId",
        "childSessionId",
        "parentTurnId",
    ] {
        let mut wrong = metadata.clone();
        wrong[field] = json!("other");
        assert!(
            present_agent(&job, "sess_1", packet(), wrong).is_err(),
            "{field}"
        );
    }
    let mut no_call = metadata;
    no_call.as_object_mut().unwrap().remove("toolCallId");
    assert!(present_agent(&job, "sess_1", packet(), no_call).is_err());
}

#[test]
fn subagent_snapshot_read_preserves_typed_model_message_semantics() {
    let value = serde_json::to_value(present_messages(snapshot(), "child-one").unwrap()).unwrap();
    assert_eq!(value[0]["role"], "assistant");
    assert_eq!(value[0]["reasoningContent"], "Check source");
    assert_eq!(
        value[0]["toolCalls"][0],
        json!({"id":"call-read","name":"read","argsJson":"{\"path\":\"README.md\"}"})
    );
    assert!(value[0]["toolCallId"].is_null());
    assert_eq!(value[1]["role"], "tool");
    assert_eq!(value[1]["toolCallId"], "call-read");
    assert_eq!(value[1]["resultState"], "successWithOutput");
    assert_eq!(value[1]["toolCalls"], json!([]));
    assert!(present_messages(snapshot(), "other-child").is_err());
    let mut missing = snapshot();
    missing.model_semantics.remove("tool-one");
    assert!(present_messages(missing, "child-one").is_err());
}

#[test]
fn subagent_read_requests_reject_old_or_unknown_protocol_fields() {
    let request = json!({"schema":"runtime.session.subagents.read.v1","sessionId":"sess_1","workspaceId":"ws_1","ownerId":1});
    assert!(
        serde_json::from_value::<SessionSubagentsReadRequest>(request.clone())
            .unwrap()
            .validate()
    );
    let mut unknown = request.clone();
    unknown["childSessionId"] = json!("arbitrary-child");
    assert!(serde_json::from_value::<SessionSubagentsReadRequest>(unknown).is_err());
    let mut old = request;
    old["schema"] = json!("runtime.session.subagents.read.v0");
    assert!(!serde_json::from_value::<SessionSubagentsReadRequest>(old)
        .unwrap()
        .validate());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_session_subagents_read_persists_and_enforces_parent_scope() {
    use crate::postgres_store::integration_tests::{retained_instance_tests, TEST_LOCK};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, store, mut db) = retained_instance_tests::setup();
    db.batch_execute("UPDATE public.app_core_session SET owner_id='1'; ALTER TABLE public.app_core_session ADD COLUMN status text DEFAULT 'active',ADD COLUMN purpose text DEFAULT 'interactive',ADD COLUMN \"purgedAt\" timestamptz;").unwrap();
    let (mut job, metadata) = job_and_metadata();
    job.status = RuntimeJobStatus::Succeeded;
    store
        .upsert_external_context_object(ExternalContextObject {
            schema_version: "external_context.v1".into(),
            object_id: "work-packet-one".into(),
            object_kind: "subagent_work_packet".into(),
            source_provider_id: "centaeris.core".into(),
            source_tool_name: "agent".into(),
            title: "Inspect source".into(),
            content: json!({"workPacket":packet()}).to_string(),
            metadata,
            updated_at_ms: 1,
        })
        .unwrap();
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: job.clone() })
        .unwrap();
    let query = SessionSubagentsReadRequest {
        schema: "runtime.session.subagents.read.v1".into(),
        session_id: "sess_1".into(),
        workspace_id: "ws_1".into(),
        owner_id: 1,
    };
    let detail = SessionSubagentReadRequest {
        schema: "runtime.session.subagent.read.v1".into(),
        session_id: "sess_1".into(),
        workspace_id: "ws_1".into(),
        owner_id: 1,
        runtime_job_id: job.job_id.clone(),
    };
    assert_eq!(
        store
            .read_session_subagents(&query)
            .unwrap()
            .unwrap()
            .agents
            .len(),
        1
    );
    assert!(store
        .read_session_subagent(&detail)
        .unwrap()
        .unwrap()
        .messages
        .is_empty());
    let typed = snapshot();
    store
        .save_agent_runtime_snapshot("child-one", &serde_json::to_string(&typed).unwrap(), 12)
        .unwrap();
    // A new store instance cannot depend on resident/UI-only state.
    let reopened = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        reopened
            .read_session_subagent(&detail)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        2
    );
    let wrong_owner = SessionSubagentsReadRequest {
        owner_id: 2,
        ..query
    };
    assert!(reopened
        .read_session_subagents(&wrong_owner)
        .unwrap()
        .is_none());
    let wrong_job = SessionSubagentReadRequest {
        runtime_job_id: "another-job".into(),
        ..detail
    };
    assert!(reopened
        .read_session_subagent(&wrong_job)
        .unwrap()
        .is_none());
    db.execute(
        "UPDATE runtime.runtime_jobs SET session_id='other-parent' WHERE job_id=$1",
        &[&job.job_id],
    )
    .unwrap();
    let detail = SessionSubagentReadRequest {
        runtime_job_id: job.job_id.clone(),
        ..wrong_job
    };
    assert!(reopened.read_session_subagent(&detail).unwrap().is_none());
    db.execute(
        "UPDATE runtime.runtime_jobs SET session_id='sess_1' WHERE job_id=$1",
        &[&job.job_id],
    )
    .unwrap();
    db.batch_execute("UPDATE public.app_core_session SET \"purgedAt\"=clock_timestamp()")
        .unwrap();
    assert!(reopened.read_session_subagent(&detail).unwrap().is_none());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_session_subagent_read_scope_blocks_purge_until_transaction_ends() {
    use crate::postgres_store::integration_tests::{retained_instance_tests, TEST_LOCK};
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (url, _store, mut db) = retained_instance_tests::setup();
    db.batch_execute("UPDATE public.app_core_session SET owner_id='1'; ALTER TABLE public.app_core_session ADD COLUMN status text DEFAULT 'active',ADD COLUMN purpose text DEFAULT 'interactive',ADD COLUMN \"purgedAt\" timestamptz;").unwrap();
    let mut reader = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut tx = reader.transaction().unwrap();
    assert!(lock_scope(&mut tx, "sess_1", "ws_1", 1).unwrap());
    db.batch_execute("SET lock_timeout='100ms'").unwrap();
    assert!(db
        .batch_execute(
            "UPDATE public.app_core_session SET \"purgedAt\"=clock_timestamp() WHERE id='sess_1'"
        )
        .is_err());
    tx.commit().unwrap();
    db.batch_execute(
        "UPDATE public.app_core_session SET \"purgedAt\"=clock_timestamp() WHERE id='sess_1'",
    )
    .unwrap();
}
