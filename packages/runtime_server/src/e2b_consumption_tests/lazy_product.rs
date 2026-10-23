//! Synthetic-provider API/Worker/main acceptance. No SDK may be launched.
use super::*;
use crate::agent_run_authorization::EnvironmentProfile;
use crate::e2b_adapter::bulk::SpoolingAdapterProcess;
use crate::e2b_adapter::{AdapterFailure, AdapterResponse};
use crate::e2b_creation::{E2bCreationAdapter, E2bCreationDriver, E2bCreationRequest};
use crate::e2b_runtime::{
    E2bAdapterLane, E2bAdapterLauncher, E2bRuntime, E2bRuntimeConfig, E2bRuntimeError,
};
use crate::postgres_store::controller_authority::{ControllerAuthorityRequest, ControllerPurpose};
use crate::postgres_store::provider_operations::ProviderCreationDispatchState;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const FIXTURE_SCHEMA: &str = "runtime.e2b_lazy_product.fixture.v1";
const CONTROL_SCHEMA: &str = "runtime.e2b_lazy_product.control.v1";
const PROOF_SCHEMA: &str = "runtime.e2b_lazy_product.proof.v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Fixture {
    schema: String,
    database_url: String,
    database_name: String,
    database_owner: String,
    workspace_id: String,
    user_id: String,
    agent_id: String,
}

impl Fixture {
    fn verify_database_owner(&self) {
        assert_eq!(
            self.schema, FIXTURE_SCHEMA,
            "synthetic fixture schema rejected"
        );
        let owner = env::var("CENTAERIS_E2B_LAZY_PRODUCT_DB_OWNER")
            .unwrap_or_else(|_| panic!("synthetic fixture database owner missing"));
        assert!(
            !owner.is_empty()
                && owner.len() <= 128
                && owner
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || b"-_".contains(&value)),
            "synthetic fixture database owner rejected"
        );
        assert_eq!(
            self.database_owner, owner,
            "synthetic fixture owner binding rejected"
        );
        let mut check = postgres::Client::connect(&self.database_url, postgres::NoTls)
            .unwrap_or_else(|_| panic!("synthetic fixture database unavailable"));
        let row = check.query_one(
            "SELECT current_database(),shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()", &[]
        ).unwrap_or_else(|_| panic!("synthetic fixture database owner unavailable"));
        let name: String = row.get(0);
        let comment: Option<String> = row.get(1);
        let expected = format!("centaeris:e2b-lazy-product:{owner}");
        assert!(
            name.starts_with("test_")
                && name == self.database_name
                && comment.as_deref() == Some(expected.as_str()),
            "synthetic fixture database owner rejected"
        );
        assert!(
            check
                .query_one("SELECT to_regnamespace('runtime') IS NULL", &[])
                .unwrap_or_else(|_| panic!("synthetic fixture namespace unavailable"))
                .get::<_, bool>(0),
            "synthetic fixture requires an isolated Runtime namespace"
        );
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Control {
    schema: String,
    action: String,
    step: Option<AgentRunStepRequest>,
}

#[derive(Default)]
struct NeverLaunch {
    calls: AtomicUsize,
}
impl E2bAdapterLauncher for NeverLaunch {
    fn launch(
        &self,
        _: &E2bRuntimeConfig,
        _: E2bAdapterLane,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(E2bRuntimeError::Unavailable(
            "synthetic_fixture_sdk_launch_forbidden",
        ))
    }
}

#[derive(Default)]
struct SyntheticUnknown {
    calls: AtomicUsize,
}
impl E2bCreationAdapter for SyntheticUnknown {
    fn create(
        &self,
        _: &str,
        _: serde_json::Value,
        _: std::time::Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        assert_eq!(
            self.calls.fetch_add(1, Ordering::SeqCst),
            0,
            "synthetic Unknown must not be retried"
        );
        Err(AdapterFailure::Unknown(
            "synthetic_fixture_dispatched_unknown",
        ))
    }
}

fn seed_unknown(
    fixture: &Fixture,
    provider: &E2bRuntime,
    backend: &PostgresRuntimeStore,
    profile: &EnvironmentProfile,
    adapter: &SyntheticUnknown,
    request: AgentRunStepRequest,
) -> String {
    let signing_key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
        .unwrap_or_else(|_| panic!("synthetic fixture signing key missing"));
    let request = request
        .validate(signing_key.as_bytes())
        .unwrap_or_else(|_| panic!("synthetic fixture step authority rejected"));
    let start = &request.agent_run_start;
    assert_eq!(&start.authorization.workspace_id, &fixture.workspace_id);
    assert_eq!(&start.authorization.user_id, &fixture.user_id);
    assert_eq!(&start.authorization.agent_id, &fixture.agent_id);
    assert_eq!(&start.authorization.environment_profile, profile);
    backend
        .require_execute_lease(&request.job_id, &request.lease_owner)
        .unwrap_or_else(|_| panic!("synthetic fixture execute lease rejected"));
    validate_agent_run_lifecycle_job(&request.job_id, &request.lease_owner, start, backend)
        .unwrap_or_else(|_| panic!("synthetic fixture lifecycle binding rejected"));
    let lease = RuntimeJobLeaseFence {
        job_id: request.job_id.clone(),
        job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
        lease_owner: request.lease_owner.clone(),
    };
    let authority = ControllerAuthorityRequest {
        environment_authorization: &start.environment_authorization,
        signing_key: signing_key.as_bytes(),
        workspace_id: &fixture.workspace_id,
        lease: &lease,
        purpose: ControllerPurpose::Execute,
    };
    let execution_id = initial_execution_id(start);
    let key = provider
        .attempt_key(&authority, &start.authorization, &execution_id)
        .unwrap_or_else(|_| panic!("synthetic fixture creation identity rejected"));
    assert!(
        backend
            .provider_creation_operation(&key.attempt_id)
            .unwrap_or_else(|_| panic!("synthetic fixture creation ledger unavailable"))
            .is_none(),
        "synthetic fixture cannot replace an existing creation responsibility"
    );
    let EnvironmentProfile::E2b(selected) = profile else {
        panic!("synthetic fixture profile rejected");
    };
    let operation = E2bCreationDriver::new(backend, adapter)
        .create_or_reopen(
            &authority,
            &E2bCreationRequest {
                authorization: &start.authorization,
                execution_id: &execution_id,
                attempt_id: &key.attempt_id,
                idempotency_key: &key.idempotency_key,
                configured_provider_config_digest: &selected.provider_config_digest,
                transport_timeout: std::time::Duration::from_secs(10),
            },
        )
        .unwrap_or_else(|_| panic!("synthetic fixture Unknown dispatch unavailable"));
    assert_eq!(
        operation.dispatch_state,
        ProviderCreationDispatchState::Dispatched
    );
    assert!(operation.reported_sandbox_id.is_none());
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    operation.identity.attempt_id
}

#[test]
#[ignore = "requires the API live server and its owned migrated PostgreSQL test database"]
fn e2b_lazy_consumption_server() {
    assert_eq!(
        env::var("CENTAERIS_E2B_LAZY_PRODUCT_ACCEPTANCE").as_deref(),
        Ok("1")
    );
    assert!(
        env::var_os("CENTAERIS_NATIVE_CONSUME_PRODUCTION_FIXTURE").is_none(),
        "synthetic E2B fixture cannot enable the Legacy physical host fixture"
    );
    let wire = env::var("CENTAERIS_E2B_LAZY_PRODUCT_FIXTURE")
        .unwrap_or_else(|_| panic!("synthetic fixture missing"));
    let fixture: Fixture =
        serde_json::from_str(&wire).unwrap_or_else(|_| panic!("synthetic fixture fields rejected"));
    fixture.verify_database_owner();
    let profile: EnvironmentProfile = serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/e2b_environment_profile/v1/managed-files.json"
    ))
    .unwrap();
    let EnvironmentProfile::E2b(selected) = &profile else {
        panic!("synthetic fixture requires E2B");
    };
    let configuration = E2bRuntimeConfig::new(
        profile.clone(),
        selected.provider_config_digest.clone(),
        PathBuf::from(
            env::var_os("RUNTIME_E2B_PYTHON")
                .unwrap_or_else(|| panic!("synthetic fixture Python path missing")),
        ),
        PathBuf::from(
            env::var_os("RUNTIME_E2B_ADAPTER_PACKAGE_ROOT")
                .unwrap_or_else(|| panic!("synthetic fixture package path missing")),
        ),
        std::time::Duration::from_secs(10),
    )
    .unwrap_or_else(|_| panic!("synthetic fixture configuration rejected"));
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap(),
    );
    let backend = Arc::new(
        PostgresRuntimeStore::new(&fixture.database_url)
            .unwrap_or_else(|_| panic!("synthetic fixture store unavailable")),
    );
    let store =
        Arc::new(runtime.block_on(async { RuntimeStoreActor::start((*backend).clone()).unwrap() }));
    let launcher = Arc::new(NeverLaunch::default());
    let provider = Arc::new(E2bRuntime::with_launcher(
        (*backend).clone(),
        configuration,
        launcher.clone(),
    ));
    let synthetic = SyntheticUnknown::default();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let address = listener.local_addr().unwrap();
    let state = HttpServerState {
        runtime: runtime.clone(),
        store,
        job_store: backend.clone(),
        tool_layers: Arc::new(Mutex::new(HashMap::new())),
        execution_profile: Arc::new(RuntimeExecutionProfile::for_e2b(&profile).unwrap()),
        e2b_runtime: Some(provider.clone()),
        e2b_runs: Arc::new(e2b_run_registry::E2bRunRegistry::default()),
        request_capacity: request_capacity::RequestCapacity::from_env().unwrap(),
    };
    let server = runtime.spawn(serve_http(listener, state));
    println!("e2b-lazy-product-ready:http://{address}");
    io::stdout().flush().unwrap();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        if line == "stop" {
            break;
        }
        let control: Control = serde_json::from_str(&line)
            .unwrap_or_else(|_| panic!("synthetic fixture control fields rejected"));
        assert_eq!(
            control.schema, CONTROL_SCHEMA,
            "synthetic fixture control schema rejected"
        );
        let mut proof = json!({"schema": PROOF_SCHEMA, "action": control.action,
            "sdkLaunches": launcher.calls.load(Ordering::SeqCst),
            "syntheticCreateCalls": synthetic.calls.load(Ordering::SeqCst)});
        match control.action.as_str() {
            "verify" => assert!(control.step.is_none(), "verify cannot dispatch a creation"),
            "seedUnknown" => {
                let request = control
                    .step
                    .unwrap_or_else(|| panic!("synthetic fixture step missing"));
                let attempt =
                    seed_unknown(&fixture, &provider, &backend, &profile, &synthetic, request);
                proof["attemptId"] = json!(attempt);
                proof["syntheticCreateCalls"] = json!(synthetic.calls.load(Ordering::SeqCst));
            }
            _ => panic!("synthetic fixture control action rejected"),
        }
        println!("e2b-lazy-product-proof:{proof}");
        io::stdout().flush().unwrap();
    }
    server.abort();
    runtime.block_on(async {
        let _ = server.await;
    });
    assert_eq!(
        launcher.calls.load(Ordering::SeqCst),
        0,
        "no SDK lane may be launched"
    );
}
