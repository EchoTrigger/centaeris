use super::*;
use centaeris_core::session::reliability::*;
use serde_json::Value;

mod owned_execution;
pub(super) use owned_execution::{
    cleanup_owned_physical_execution, owned_host_context, owned_inventory, prepare_owned_allocation,
};

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
        job_store: backend.clone(),
        tool_layers: Arc::new(Mutex::new(HashMap::new())),
        execution_profile: Arc::new(RuntimeExecutionProfile {
            schema: RUNTIME_EXECUTION_PROFILE_SCHEMA,
            image_digest: format!("sha256:{}", "a".repeat(64)),
        }),
        request_capacity: request_capacity::RequestCapacity::from_env().unwrap(),
    };
    let server = runtime.spawn(serve_http(listener, state));
    let mut physical_pin = None;
    println!("native-consume-production-ready:http://{address}");
    io::stdout().flush().unwrap();
    for command in io::stdin().lock().lines() {
        let command = command.unwrap();
        if command == "stop" {
            break;
        }
        if let Some(wire) = command.strip_prefix("physical-pin:") {
            assert!(
                physical_pin.is_none(),
                "only one controlled physical pin at a time"
            );
            physical_pin = Some(start_physical_pin(backend.clone(), wire));
            continue;
        }
        if command == "release-physical-pin" {
            let (release, operation) = physical_pin.take().expect("physical pin missing");
            release.send(()).unwrap();
            operation.join().unwrap().unwrap();
            println!(
                "native-consume-production-physical-released:{}",
                json!({"released":true})
            );
            io::stdout().flush().unwrap();
            continue;
        }
        if let Some(run_id) = command.strip_prefix("verify-cleanup:") {
            let proof = owned_execution::verify_cleanup(backend.as_ref(), run_id).unwrap();
            println!("native-consume-production-cleanup:{proof}");
            io::stdout().flush().unwrap();
            continue;
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
    if let Some((release, operation)) = physical_pin {
        let _ = release.send(());
        operation.join().unwrap().unwrap();
    }
    server.abort();
    runtime.block_on(async {
        let _ = server.await;
    });
}

/// Hold actual authorization pins inside the production physical callback.
/// This stdin protocol exists only in the ignored cross-process test server.
fn start_physical_pin(
    backend: Arc<PostgresRuntimeStore>,
    wire: &str,
) -> (
    std::sync::mpsc::SyncSender<()>,
    std::thread::JoinHandle<Result<(), String>>,
) {
    let body: Value = serde_json::from_str(wire).unwrap();
    let key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY").unwrap();
    let start = serde_json::from_value::<AgentRunStart>(body["agentRunStart"].clone())
        .unwrap()
        .validate(key.as_bytes())
        .unwrap();
    assert!(owns_identity(
        &start.authorization.user_id,
        &start.authorization.agent_id
    ));
    let lease_owner = body["leaseOwner"].as_str().unwrap().to_owned();
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let operation = std::thread::spawn(move || {
        use crate::postgres_store::execution_instances::RetainedInstanceRequest;
        use crate::resident_capacity::{declared_usage, ResidentCapacity};
        use std::io::Write;
        let profile = &start.authorization.environment_profile;
        let profile_digest = profile.digest()?;
        let execution_id = format!("physical-pin:{}", start.agent_run_id);
        let fence = RuntimeJobLeaseFence {
            job_id: agent_run_lifecycle_job_id(&start.agent_run_id)?,
            job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
            lease_owner,
        };
        let pinned_run_id = start.agent_run_id.clone();
        backend.prepare_retained_instance(
            &RetainedInstanceRequest {
                environment_authorization: &start.environment_authorization,
                signing_key: key.as_bytes(),
                execution_id: &execution_id,
                workspace_id: &start.authorization.workspace_id,
                host_id: "native-controller-pin-test-host",
                image_digest: &profile.image_digest,
                profile_digest: &profile_digest,
                resources: declared_usage(profile.resources.sandbox_resources()),
                lease: &fence,
            },
            ResidentCapacity::from_env()?,
            || Ok(Vec::new()),
            |_, _| Ok(()),
            move |allocation| {
                println!(
                    "native-consume-production-physical-pinned:{}",
                    json!({
                    "agentRunId":pinned_run_id,"instanceId":allocation.instance_id})
                );
                io::stdout().flush().map_err(|error| error.to_string())?;
                released
                    .recv_timeout(std::time::Duration::from_secs(20))
                    .map_err(|error| format!("controlled physical pin release failed: {error}"))?;
                Ok(format!("controlled-container:{}", allocation.instance_id))
            },
        )?;
        Ok(())
    });
    (release, operation)
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
