use super::*;
use centaeris_core::session::reliability::*;
use serde_json::Value;

mod owned_execution;
pub(super) use owned_execution::{
    cleanup_owned_physical_execution, owned_host_context, owned_inventory, prepare_owned_allocation,
};

fn production_fixture() -> Option<Value> {
    env::var("CENTAERIS_ENVIRONMENT_RUNTIME_FIXTURE")
        .ok()
        .map(|wire| serde_json::from_str(&wire).unwrap())
}

pub(super) fn owns_identity(user_id: &str, workspace_id: &str) -> bool {
    production_fixture().is_some_and(|value| {
        value["userId"].as_str() == Some(user_id)
            && value["workspaceId"].as_str() == Some(workspace_id)
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
fn environment_runtime_server() {
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
        tool_concurrency_registry: Default::default(),
        execution_profile: Arc::new(RuntimeExecutionProfile {
            schema: RUNTIME_EXECUTION_PROFILE_SCHEMA,
            image_digest: format!("sha256:{}", "a".repeat(64)),
        }),
        request_capacity: request_capacity::RequestCapacity::from_env().unwrap(),
    };
    let server = runtime.spawn(serve_http(listener, state));
    let mut physical_pin = None;
    println!("environment-runtime-ready:http://{address}");
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
                "environment-runtime-physical-released:{}",
                json!({"released":true})
            );
            io::stdout().flush().unwrap();
            continue;
        }
        if let Some(run_id) = command.strip_prefix("verify-cleanup:") {
            let proof = owned_execution::verify_cleanup(backend.as_ref(), run_id).unwrap();
            println!("environment-runtime-cleanup:{proof}");
            io::stdout().flush().unwrap();
            continue;
        }
        panic!("unsupported environment fixture command");
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
        &start.authorization.workspace_id
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
                host_id: "environment-controller-pin-test-host",
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
                    "environment-runtime-physical-pinned:{}",
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
