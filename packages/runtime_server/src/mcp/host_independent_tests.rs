use super::*;
use centaeris_core::extension::{mcp_model_contract_digest, McpLifecycleV1};
use serde_json::json;
use std::sync::Mutex;

struct Api {
    url: String,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Api {
    async fn new() -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let app = axum::Router::new().route(
            "/internal/mcp-connectors/authorize",
            axum::routing::post(move |body: axum::body::Bytes| {
                let recorded = recorded.clone();
                async move {
                    recorded
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        json!({"error":"fixture denied"}).to_string(),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url,
            requests,
            task,
        }
    }
    fn resolver(&self) -> Arc<McpCredentialResolver> {
        let mut resolver = McpCredentialResolver::new(
            self.url.clone(),
            "fixture-only".into(),
            "run-owned".into(),
            "authorization-owned".into(),
            format!("sha256:{}", "b".repeat(64)),
        )
        .unwrap();
        resolver.client = reqwest::Client::builder().no_proxy().build().unwrap();
        Arc::new(resolver)
    }
    fn prepared(&self, servers: Vec<McpServerDeclarationV1>) -> PreparedMcpServers {
        PreparedMcpServers {
            servers: servers
                .into_iter()
                .map(|server| (identity(&server.id), server))
                .collect(),
            credential_resolver: self.resolver(),
        }
    }
}
fn identity(id: &str) -> McpConnectorIdentity {
    McpConnectorIdentity {
        plugin_name: "banana".into(),
        server_id: id.into(),
        resource_path: "mcp/servers.json".into(),
        resource_digest: format!("sha256:{}", "c".repeat(64)),
    }
}
fn declaration(id: &str, stdio: bool) -> McpServerDeclarationV1 {
    let tools = vec![McpToolDeclarationV1 {
        source_name: "search".into(),
        name: "banana_search".into(),
        description: "Search fixture".into(),
        input_schema: json!({"type":"object"}),
        concurrency_safe: true,
        scopes: Vec::new(),
    }];
    McpServerDeclarationV1 {
        id: id.into(),
        model_contract_digest: mcp_model_contract_digest(id, &tools).unwrap(),
        transport: if stdio {
            McpTransportV1::Stdio {
                program: "bin/never-spawn".into(),
                args: Vec::new(),
            }
        } else {
            McpTransportV1::StreamableHttp {
                url: "https://fixture.invalid/mcp".into(),
                bearer_credential_ref: None,
            }
        },
        lifecycle: McpLifecycleV1::Auto,
        startup_timeout_ms: 1000,
        tool_timeout_ms: 1000,
        tools,
    }
}
#[tokio::test]
async fn empty_mcp_declarations_need_no_legacy_stdio_host_or_authority_request() {
    let api = Api::new().await;
    let (bindings, metrics) = connect_mcp_servers(api.prepared(Vec::new()), None)
        .await
        .expect("empty declarations do not need a stdio host");
    assert!(bindings.contracts.is_empty());
    assert!(bindings.providers.is_empty());
    assert_eq!(metrics.server_count, 0);
    assert_eq!(metrics.connection_count, 0);
    assert!(api.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn http_declarations_keep_lazy_contracts_without_legacy_stdio_host() {
    let api = Api::new().await;
    let server = declaration("http-source", false);
    let (bindings, metrics) = connect_mcp_servers(api.prepared(vec![server]), None)
        .await
        .expect("HTTP declarations do not need a stdio host");
    assert_eq!(bindings.contracts.len(), 1);
    assert_eq!(bindings.contracts[0].name, "banana_search");
    assert_eq!(
        bindings.providers[0].provider_id(),
        "mcp:banana:http-source"
    );
    assert_eq!(metrics.server_count, 1);
    assert_eq!(metrics.connection_count, 0);
    assert_eq!(metrics.credential_count, 0);
    assert!(
        api.requests.lock().unwrap().is_empty(),
        "lazy setup must not resolve credentials"
    );
}
#[tokio::test]
async fn mixed_stdio_declarations_without_host_fail_before_any_authority_request() {
    let api = Api::new().await;
    let result = connect_mcp_servers(
        api.prepared(vec![
            declaration("http-source", false),
            declaration("stdio-source", true),
        ]),
        None,
    )
    .await;
    let error = result.err().expect("unsupported stdio must fail setup");
    assert!(error.contains("stdio"));
    assert!(api.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn direct_stdio_connector_without_host_rejects_before_credential_dispatch() {
    let api = Api::new().await;
    let server = declaration("stdio-source", true);
    let connector = WorkspaceMcpConnector {
        plugin_name: "banana".into(),
        authorization: Arc::new(McpDispatchAuthorization {
            identity: identity(&server.id),
            resolver: api.resolver(),
            binding_digest: tokio::sync::Mutex::new(None),
        }),
        server,
        docker: None,
    };
    assert!(connector.connect().await.is_err());
    assert!(
        api.requests.lock().unwrap().is_empty(),
        "unsupported stdio must be rejected before requesting credentials"
    );
}
#[tokio::test]
async fn http_connector_without_stdio_host_still_checks_current_authority() {
    let api = Api::new().await;
    let server = declaration("http-source", false);
    let connector = WorkspaceMcpConnector {
        plugin_name: "banana".into(),
        authorization: Arc::new(McpDispatchAuthorization {
            identity: identity(&server.id),
            resolver: api.resolver(),
            binding_digest: tokio::sync::Mutex::new(None),
        }),
        server,
        docker: None,
    };
    assert!(
        connector.connect().await.is_err(),
        "fixture denies connector authorization"
    );
    let requests = api.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["serverId"], "http-source");
    assert_eq!(requests[0]["operation"], "connect");
}
