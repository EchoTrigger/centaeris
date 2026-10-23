//! Opt-in API/Worker/Core acceptance against the explicitly selected local E2B.
use super::*;
use std::io::{BufRead, Write};

#[test]
#[ignore = "requires explicit local E2B opt-in and the API's disposable live database"]
fn e2b_consumption_production_server() {
    assert_eq!(
        env::var("CENTAERIS_E2B_PRODUCT_ACCEPTANCE").as_deref(),
        Ok("1")
    );
    assert!(
        env::var_os("CENTAERIS_NATIVE_CONSUME_PRODUCTION_FIXTURE").is_none(),
        "E2B acceptance must not enable the empty native physical fixture"
    );
    let configuration = execution_backend_config::ExecutionBackendConfig::from_env()
        .unwrap_or_else(|_| panic!("E2B product configuration unavailable"));
    let execution_backend_config::ExecutionBackendConfig::E2b(configuration) = configuration else {
        panic!("E2B product acceptance requires the E2B backend");
    };
    let execution_profile = Arc::new(
        RuntimeExecutionProfile::for_e2b(configuration.profile())
            .unwrap_or_else(|_| panic!("E2B product profile unavailable")),
    );
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap_or_else(|_| panic!("E2B product runtime unavailable")),
    );
    let database_url = env::var("CENTAERIS_E2B_PRODUCT_DATABASE_URL")
        .unwrap_or_else(|_| panic!("E2B disposable database missing"));
    let owner = env::var("CENTAERIS_E2B_PRODUCT_DB_OWNER")
        .unwrap_or_else(|_| panic!("E2B disposable database owner missing"));
    assert!(!owner.is_empty(), "E2B disposable database owner missing");
    let mut owner_check = postgres::Client::connect(&database_url, postgres::NoTls)
        .unwrap_or_else(|_| panic!("E2B disposable database owner unavailable"));
    let owner_row = owner_check.query_one(
        "SELECT current_database(),shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()",
        &[],
    ).unwrap_or_else(|_| panic!("E2B disposable database owner unavailable"));
    let name: String = owner_row.get(0);
    let comment: Option<String> = owner_row.get(1);
    assert!(
        name.starts_with("test_")
            && comment.as_deref() == Some(format!("centaeris:e2b-product:{owner}").as_str()),
        "E2B disposable database owner rejected"
    );
    drop(owner_check);
    let backend = Arc::new(
        PostgresRuntimeStore::new(&database_url)
            .unwrap_or_else(|_| panic!("E2B disposable database unavailable")),
    );
    let store = Arc::new(
        runtime
            .block_on(async { RuntimeStoreActor::start((*backend).clone()) })
            .unwrap_or_else(|_| panic!("E2B product store actor unavailable")),
    );
    let e2b_runtime = Arc::new(e2b_runtime::E2bRuntime::new(
        (*backend).clone(),
        *configuration,
    ));
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap_or_else(|_| panic!("E2B product listener unavailable"));
    let address = listener.local_addr().unwrap();
    let state = HttpServerState {
        runtime: runtime.clone(),
        store,
        job_store: backend,
        tool_layers: Arc::new(Mutex::new(HashMap::new())),
        execution_profile,
        e2b_runtime: Some(e2b_runtime),
        e2b_runs: Arc::new(e2b_run_registry::E2bRunRegistry::default()),
        request_capacity: request_capacity::RequestCapacity::from_env()
            .unwrap_or_else(|_| panic!("E2B product request capacity invalid")),
    };
    let server = runtime.spawn(serve_http(listener, state));
    println!("e2b-product-ready:http://{address}");
    io::stdout().flush().unwrap();
    if let Some(command) = io::stdin().lock().lines().next() {
        assert!(
            matches!(command.as_deref(), Ok("stop")),
            "unsupported E2B product control command"
        );
    }
    server.abort();
    runtime.block_on(async {
        let _ = server.await;
    });
}

#[cfg(target_os = "linux")]
mod lazy_product;
