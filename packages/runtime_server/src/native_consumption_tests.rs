use super::*;
use centaeris_core::session::reliability::*;
use serde_json::Value;

fn production_fixture() -> Option<Value> {
    env::var("CENTAERIS_NATIVE_CONSUME_PRODUCTION_FIXTURE")
        .ok()
        .map(|wire| serde_json::from_str(&wire).unwrap())
}

pub(super) fn owns_identity(user_id: &str, agent_id: &str) -> bool {
    production_fixture().is_some_and(|value| {
        value["userId"].as_str() == Some(user_id) && value["agentId"].as_str() == Some(agent_id)
    })
}

fn owned_runs() -> &'static Mutex<HashSet<String>> {
    static RUNS: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashSet::new()))
}

pub(super) fn register_owned_run(run: &str) {
    assert!(owned_runs().lock().unwrap().insert(run.to_string()));
}

pub(super) fn owns_run(run: &str) -> bool {
    owned_runs().lock().unwrap().contains(run)
}

/// Serve the actual Runtime routes, store actor and runner. The empty owned
/// execution fixture omits material discovery; its protocol has a separate gate.
/// Django owns admission and deterministic model responses.
#[test]
#[ignore = "requires the API live server and its migrated disposable database"]
fn native_consumption_production_server() {
    use std::io::{BufRead, Write};
    let value = production_fixture().unwrap();
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let backend =
        Arc::new(PostgresRuntimeStore::new(value["databaseUrl"].as_str().unwrap()).unwrap());
    let store =
        Arc::new(runtime.block_on(async { RuntimeStoreActor::start((*backend).clone()).unwrap() }));
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let address = listener.local_addr().unwrap();
    let state = HttpServerState {
        runtime: runtime.clone(),
        store: store.clone(),
        job_store: backend,
        tool_layers: Arc::new(Mutex::new(HashMap::new())),
        execution_profile: Arc::new(RuntimeExecutionProfile {
            schema: RUNTIME_EXECUTION_PROFILE_SCHEMA,
            image_capability: "workspace_general_v1",
            image_digest: format!("sha256:{}", "a".repeat(64)),
        }),
        request_capacity: request_capacity::RequestCapacity::from_env().unwrap(),
    };
    let server = runtime.spawn(serve_http(listener, state));
    println!("native-consume-production-ready:http://{address}");
    io::stdout().flush().unwrap();
    for command in io::stdin().lock().lines() {
        let command = command.unwrap();
        if command == "stop" {
            break;
        }
        let start = serde_json::from_str::<AgentRunStart>(&command)
            .unwrap()
            .validate(
                env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        assert!(owns_run(&start.agent_run_id));
        let AgentRunStartInitialInput::HostEvent { input, .. } = &start.initial_input else {
            panic!("native input required")
        };
        let snapshot = SessionManager::new(store.as_ref().clone())
            .load_session(&start.authorization.session_id)
            .unwrap()
            .unwrap();
        let origins: Vec<_> = snapshot
            .messages
            .iter()
            .filter_map(|message| {
                centaeris_core::session::host_event_input::host_event_origin(
                    &snapshot.session_id,
                    message,
                )
                .unwrap()
            })
            .collect();
        assert_eq!(origins, vec![input.clone()]);
        println!(
            "native-consume-production-origin:{}",
            serde_json::to_string(input).unwrap()
        );
        io::stdout().flush().unwrap();
    }
    server.abort();
    runtime.block_on(async {
        let _ = server.await;
    });
}

#[test]
#[ignore = "invoked by the API native consume gate"]
fn create_native_input() {
    let body = env::var("CENTAERIS_NATIVE_INPUT_REQUEST").unwrap();
    let input = serde_json::from_str::<HostEventInputRequest>(&body)
        .unwrap()
        .create()
        .unwrap();
    println!(
        "core-native-input:{}",
        json!({"schema":"runtime.host_event_input.created.v1", "input":input})
    );
}

#[test]
#[ignore = "requires the API's migrated disposable database and committed admission"]
fn native_consumption_fenced_record() {
    let value: Value =
        serde_json::from_str(&env::var("CENTAERIS_NATIVE_CONSUME_FIXTURE").unwrap()).unwrap();
    let start = serde_json::from_value::<AgentRunStart>(value["agentRunStart"].clone())
        .unwrap()
        .validate(value["signingKey"].as_str().unwrap().as_bytes())
        .unwrap();
    let backend = PostgresRuntimeStore::new(value["databaseUrl"].as_str().unwrap()).unwrap();
    let now = now_ms().unwrap();
    let run = &start.agent_run_id;
    let session = &start.authorization.session_id;
    let lifecycle = agent_run_lifecycle_job_id(run).unwrap();
    backend
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: lifecycle.clone(),
                job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
                status: RuntimeJobStatus::Queued,
                run_at_ms: now,
                lease_owner: None,
                lease_expires_at_ms: None,
                heartbeat_at_ms: None,
                retry_count: 0,
                max_retries: 10,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: format!("{lifecycle}:{}", start.authorization_digest),
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
    let owner = backend
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "native-consume-writer".into(),
            job_id: Some(lifecycle.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 600000,
        })
        .unwrap()
        .remove(0)
        .lease_owner
        .unwrap();
    backend
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: lifecycle.clone(),
            lease_owner: owner.clone(),
            started_at_ms: now,
        })
        .unwrap();
    let fence = RuntimeJobLeaseFence {
        job_id: lifecycle,
        job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
        lease_owner: owner,
    };
    let log = backend
        .session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone(),
        )
        .with_agent_run_start(&start)
        .unwrap();
    let mut sequence = AgentRunSessionState::new(session, run).unwrap();
    let records = started_session_records(&start, &mut sequence, now).unwrap();
    assert_eq!(
        records[1].event.event_type,
        SessionRecordType::HostEventInput
    );
    let mut changed = records.clone();
    changed[1].event.payload["source"] = json!("another source");
    assert!(log
        .append_session_records_with_runtime_job_lease_blocking(run, &changed, &fence)
        .is_err());
    let committed = log
        .append_session_records_with_runtime_job_lease_blocking(run, &records, &fence)
        .unwrap();
    assert_eq!(committed.records.len(), 2);
    assert_eq!(
        log.append_session_records_with_runtime_job_lease_blocking(run, &records, &fence)
            .unwrap(),
        committed
    );
    let reopened = backend
        .session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone(),
        )
        .with_agent_run_start(&start)
        .unwrap();
    assert_eq!(
        reopened
            .append_session_records_with_runtime_job_lease_blocking(run, &records, &fence)
            .unwrap(),
        committed
    );
    let mut downgraded = start.clone();
    downgraded.initial_input = AgentRunStartInitialInput::UserMessage {};
    assert!(backend
        .session_log(
            start.authorization.workspace_id.clone(),
            session.clone(),
            start.prompt.clone()
        )
        .with_agent_run_start(&downgraded)
        .is_err());
    let restored = load_existing_session_sequence(&backend, &start).unwrap();
    assert_eq!(restored.next_sequence(), sequence.next_sequence());
    let crate::contract::AgentRunStartInitialInput::HostEvent { input, .. } = &start.initial_input
    else {
        panic!("native input required")
    };
    let snapshot = restore_runtime_snapshot_from_session_records(
        session,
        &records
            .iter()
            .map(|record| record.event.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(
        centaeris_core::session::host_event_input::host_event_origin(
            session,
            &snapshot.messages[0]
        )
        .unwrap(),
        Some(input.clone())
    );
    assert_ne!(run, &start.turn_id);
    println!(
        "native-consume-fenced-record-ok:{}",
        json!({"agentRunId":run,"turnId":start.turn_id,"inputId":input.input_id,"messageId":input.message_id})
    );
}
