use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use centaeris_core::extension::{
    load_mcp_servers_file, McpServerDeclarationV1, McpToolDeclarationV1, McpTransportV1,
    PluginActivationSnapshotV1,
};
use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
};
use centaeris_core::tool::limits::ToolContractBudget;
use centaeris_core::tool::{DynamicToolContract, ToolErrorInfo};
use centaeris_mcp::{
    bounded_io_transport, connect_mcp_server_transport, connect_streamable_http_mcp_server,
    lazy_mcp_server_binding, valid_bearer_token, McpConnectError, McpServerConnector,
};
use serde::{Deserialize, Serialize};

use crate::docker_execution_host::DockerExecutionHostRunner;
use crate::skill_projection::{verified_plugin_package_roots_at, PLUGIN_CATALOG_ROOT};

pub(crate) struct McpBindings {
    pub contracts: Vec<DynamicToolContract>,
    pub providers: Vec<Arc<dyn DynamicToolProvider + Send + Sync>>,
}

pub(crate) struct PreparedMcpServers {
    servers: Vec<(McpConnectorIdentity, McpServerDeclarationV1)>,
    credential_resolver: Arc<McpCredentialResolver>,
}

#[derive(Clone)]
struct McpConnectorIdentity {
    plugin_name: String,
    server_id: String,
    resource_path: String,
    resource_digest: String,
}

struct McpDispatchAuthorization {
    identity: McpConnectorIdentity,
    resolver: Arc<McpCredentialResolver>,
    binding_digest: tokio::sync::Mutex<Option<String>>,
}

impl McpDispatchAuthorization {
    async fn check(
        &self,
        operation: &'static str,
    ) -> Result<McpCredentialResolvedResponse, String> {
        let mut binding_digest = self.binding_digest.lock().await;
        let authorized = self
            .resolver
            .authorize(&self.identity, operation, binding_digest.as_deref())
            .await?;
        if binding_digest
            .as_ref()
            .is_some_and(|digest| digest != &authorized.binding_digest)
        {
            return Err("MCP connector binding changed".to_string());
        }
        if binding_digest.is_none() {
            *binding_digest = Some(authorized.binding_digest.clone());
        }
        Ok(authorized)
    }

    async fn connect_token(
        &self,
        server: &McpServerDeclarationV1,
    ) -> Result<Option<String>, String> {
        let token = self.check("connect").await?.token;
        let requires_token = matches!(
            &server.transport,
            McpTransportV1::StreamableHttp {
                bearer_credential_ref: Some(_),
                ..
            }
        );
        if requires_token != token.is_some() {
            return Err("MCP connector credential mismatch".to_string());
        }
        Ok(token)
    }
}

struct AuthorizedMcpProvider {
    inner: Arc<dyn DynamicToolProvider + Send + Sync>,
    authorization: Arc<McpDispatchAuthorization>,
}

impl DynamicToolProvider for AuthorizedMcpProvider {
    fn provider_id(&self) -> &str {
        self.inner.provider_id()
    }

    fn execute<'a>(
        &'a self,
        req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            self.authorization.check("dispatch").await?;
            self.inner.execute(req).await
        })
    }

    fn execute_with_error_info<'a>(
        &'a self,
        req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, ToolErrorInfo>> + Send + 'a>>
    {
        Box::pin(async move {
            self.authorization
                .check("dispatch")
                .await
                .map_err(ToolErrorInfo::from_unstructured_error)?;
            self.inner.execute_with_error_info(req).await
        })
    }
}

pub(crate) struct McpStartupMetrics {
    pub server_count: usize,
    pub credential_count: usize,
    pub connection_count: usize,
    pub credential_resolve_ms: u128,
    pub discovery_ms: u128,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceMcpCatalogResult {
    pub schema: &'static str,
    pub plugins: Vec<WorkspaceMcpPluginSummary>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceMcpPluginSummary {
    pub plugin_name: String,
    pub servers: Vec<WorkspaceMcpServerSummary>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceMcpServerSummary {
    pub id: String,
    pub model_contract_digest: String,
    pub transport: WorkspaceMcpTransportSummary,
    pub auth: WorkspaceMcpAuthSummary,
    pub startup_timeout_ms: u64,
    pub tool_timeout_ms: u64,
    pub tools: Vec<McpToolDeclarationV1>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceMcpTransportSummary {
    pub r#type: &'static str,
    pub endpoint: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceMcpAuthSummary {
    pub r#type: &'static str,
    pub credential_ref: Option<String>,
}

pub(crate) fn workspace_mcp_catalog(
    activation: &PluginActivationSnapshotV1,
) -> Result<WorkspaceMcpCatalogResult, String> {
    workspace_mcp_catalog_at(activation, Path::new(PLUGIN_CATALOG_ROOT))
}

fn workspace_mcp_catalog_at(
    activation: &PluginActivationSnapshotV1,
    catalog_root: &Path,
) -> Result<WorkspaceMcpCatalogResult, String> {
    let package_roots = verified_plugin_package_roots_at(activation, catalog_root)?;
    let mut plugins = Vec::with_capacity(activation.packages.len());
    let mut declaration_budget = ToolContractBudget::default();
    for (package, package_root) in activation.packages.iter().zip(package_roots) {
        let mut servers = Vec::new();
        for resource in &package.mcp_servers {
            let declaration =
                load_mcp_servers_file(package_root.join(resource.path.as_str()).as_path())?;
            declaration_budget.add(&declaration)?;
            servers.extend(declaration.servers.into_iter().map(|server| {
                let (transport, auth) = match server.transport {
                    McpTransportV1::Stdio { .. } => (
                        WorkspaceMcpTransportSummary {
                            r#type: "stdio",
                            endpoint: None,
                        },
                        WorkspaceMcpAuthSummary {
                            r#type: "none",
                            credential_ref: None,
                        },
                    ),
                    McpTransportV1::StreamableHttp {
                        url,
                        bearer_credential_ref,
                    } => {
                        let auth = WorkspaceMcpAuthSummary {
                            r#type: if bearer_credential_ref.is_some() {
                                "bearer"
                            } else {
                                "none"
                            },
                            credential_ref: bearer_credential_ref,
                        };
                        (
                            WorkspaceMcpTransportSummary {
                                r#type: "streamableHttp",
                                endpoint: Some(url),
                            },
                            auth,
                        )
                    }
                };
                WorkspaceMcpServerSummary {
                    id: server.id,
                    model_contract_digest: server.model_contract_digest,
                    transport,
                    auth,
                    startup_timeout_ms: server.startup_timeout_ms,
                    tool_timeout_ms: server.tool_timeout_ms,
                    tools: server.tools,
                }
            }));
        }
        plugins.push(WorkspaceMcpPluginSummary {
            plugin_name: package.name.clone(),
            servers,
        });
    }
    Ok(WorkspaceMcpCatalogResult {
        schema: "workspace.mcp.catalog.result.v1",
        plugins,
    })
}

pub(crate) struct McpCredentialResolver {
    api_internal_url: String,
    internal_api_token: String,
    agent_run_id: String,
    authorization_ref: String,
    authorization_digest: String,
    client: reqwest::Client,
}

impl McpCredentialResolver {
    pub(crate) fn new(
        api_internal_url: String,
        internal_api_token: String,
        agent_run_id: String,
        authorization_ref: String,
        authorization_digest: String,
    ) -> Result<Self, String> {
        Ok(Self {
            api_internal_url,
            internal_api_token,
            agent_run_id,
            authorization_ref,
            authorization_digest,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|error| format!("build MCP credential client failed: {error}"))?,
        })
    }

    async fn authorize(
        &self,
        identity: &McpConnectorIdentity,
        operation: &'static str,
        binding_digest: Option<&str>,
    ) -> Result<McpCredentialResolvedResponse, String> {
        let url = format!(
            "{}/internal/mcp-connectors/authorize",
            self.api_internal_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Internal-Token", self.internal_api_token.as_str())
            .json(&McpCredentialResolveRequest {
                schema: "runtime.mcp_connector.authorization.v1",
                agent_run_id: self.agent_run_id.as_str(),
                authorization_ref: self.authorization_ref.as_str(),
                authorization_digest: self.authorization_digest.as_str(),
                plugin_name: &identity.plugin_name,
                server_id: &identity.server_id,
                resource_path: &identity.resource_path,
                resource_digest: &identity.resource_digest,
                operation,
                binding_digest,
            })
            .send()
            .await
            .map_err(|_| "authorize MCP connector request failed".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "authorize MCP connector returned {}",
                response.status().as_u16()
            ));
        }
        let resolved = response
            .json::<McpCredentialResolvedResponse>()
            .await
            .map_err(|_| "decode MCP connector authorization response failed".to_string())?;
        if resolved.schema != "runtime.mcp_connector.authorized.v1"
            || !matches!(resolved.scope.as_str(), "private" | "managed")
            || !valid_binding_digest(&resolved.binding_digest)
            || resolved
                .token
                .as_ref()
                .is_some_and(|token| !valid_bearer_token(token))
            || (operation == "dispatch" && resolved.token.is_some())
        {
            return Err("MCP connector authorization response invalid".to_string());
        }
        Ok(resolved)
    }
}

fn valid_binding_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

struct DockerMcpTransport {
    child: crate::docker_engine::ExecChild,
    transport: centaeris_mcp::BoundedIoTransport<
        crate::docker_engine::ExecReader,
        crate::docker_engine::ExecWriter,
    >,
    stderr: tokio::task::JoinHandle<()>,
}
impl DockerMcpTransport {
    fn new(mut child: crate::docker_engine::ExecChild) -> Result<Self, String> {
        let stdout = child.stdout.take().ok_or("MCP stdout missing")?;
        let stdin = child.stdin.take().ok_or("MCP stdin missing")?;
        let mut stderr = child.stderr.take().ok_or("MCP stderr missing")?;
        let stderr = tokio::spawn(async move {
            // MCP stderr was discarded by the CLI transport. Drain it without
            // buffering so diagnostic output cannot stall protocol stdout.
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        });
        Ok(Self {
            child,
            transport: bounded_io_transport(stdout, stdin),
            stderr,
        })
    }
}
impl rmcp::transport::Transport<rmcp::RoleClient> for DockerMcpTransport {
    type Error = std::io::Error;
    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<rmcp::RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.transport.send(item)
    }
    async fn receive(&mut self) -> Option<rmcp::service::RxJsonRpcMessage<rmcp::RoleClient>> {
        self.transport.receive().await
    }
    async fn close(&mut self) -> Result<(), Self::Error> {
        let result = self.transport.close().await;
        // Closing the attach stream is not a remote kill. The execution host
        // owns sandbox teardown; no MCP command is retried by this adapter.
        self.child.disconnect();
        self.stderr.abort();
        result
    }
}
impl Drop for DockerMcpTransport {
    fn drop(&mut self) {
        self.child.disconnect();
        self.stderr.abort();
    }
}

struct WorkspaceMcpConnector {
    plugin_name: String,
    server: McpServerDeclarationV1,
    authorization: Arc<McpDispatchAuthorization>,
    docker: Arc<DockerExecutionHostRunner>,
}

impl McpServerConnector for WorkspaceMcpConnector {
    fn connect<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Arc<dyn DynamicToolProvider + Send + Sync>, McpConnectError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let credential_started = Instant::now();
            let bearer_token = self
                .authorization
                .connect_token(&self.server)
                .await
                .map_err(McpConnectError::Unavailable)?;
            let credential_resolve_ms = credential_started.elapsed().as_millis();
            let discovery_started = Instant::now();
            let provider = match &self.server.transport {
                McpTransportV1::StreamableHttp { .. } => {
                    connect_streamable_http_mcp_server(
                        self.plugin_name.as_str(),
                        self.server.clone(),
                        bearer_token.as_deref(),
                    )
                    .await?
                }
                McpTransportV1::Stdio { program, args } => {
                    let request = self
                        .docker
                        .mcp_exec_request(
                            self.plugin_name.as_str(),
                            program.as_str(),
                            args.as_slice(),
                        )
                        .map_err(McpConnectError::Unavailable)?;
                    let child = request
                        .spawn_async()
                        .await
                        .map_err(McpConnectError::Unavailable)?;
                    let transport =
                        DockerMcpTransport::new(child).map_err(McpConnectError::Unavailable)?;
                    connect_mcp_server_transport(
                        self.plugin_name.as_str(),
                        self.server.clone(),
                        transport,
                    )
                    .await?
                }
            };
            eprintln!(
                "mcp_lazy_connect_profile: pluginName={}; serverId={}; credentialResolveMs={}; connectDiscoveryMs={}",
                self.plugin_name,
                self.server.id,
                credential_resolve_ms,
                discovery_started.elapsed().as_millis(),
            );
            // Core invokes this provider only after lazy initialization and its
            // connection queue. Check current authority at that dispatch point.
            Ok(Arc::new(AuthorizedMcpProvider {
                inner: provider,
                authorization: self.authorization.clone(),
            }) as Arc<dyn DynamicToolProvider + Send + Sync>)
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpCredentialResolveRequest<'a> {
    schema: &'static str,
    agent_run_id: &'a str,
    authorization_ref: &'a str,
    authorization_digest: &'a str,
    plugin_name: &'a str,
    server_id: &'a str,
    resource_path: &'a str,
    resource_digest: &'a str,
    operation: &'a str,
    binding_digest: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpCredentialResolvedResponse {
    schema: String,
    scope: String,
    binding_digest: String,
    #[serde(deserialize_with = "required_nullable_token")]
    token: Option<String>,
}

fn required_nullable_token<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

pub(crate) async fn prepare_http_mcp_servers(
    activation: PluginActivationSnapshotV1,
    credential_resolver: McpCredentialResolver,
) -> Result<PreparedMcpServers, String> {
    prepare_mcp_servers_at(
        activation,
        credential_resolver,
        Path::new(PLUGIN_CATALOG_ROOT),
    )
}

fn prepare_mcp_servers_at(
    activation: PluginActivationSnapshotV1,
    credential_resolver: McpCredentialResolver,
    catalog_root: &Path,
) -> Result<PreparedMcpServers, String> {
    let mut servers = Vec::new();
    let mut declaration_budget = ToolContractBudget::default();
    for package in &activation.packages {
        for resource in &package.mcp_servers {
            let declaration = load_mcp_servers_file(
                catalog_root
                    .join(package.name.as_str())
                    .join(resource.path.as_str())
                    .as_path(),
            )?;
            declaration_budget.add(&declaration)?;
            servers.extend(declaration.servers.into_iter().map(|server| {
                (
                    McpConnectorIdentity {
                        plugin_name: package.name.clone(),
                        server_id: server.id.clone(),
                        resource_path: resource.path.clone(),
                        resource_digest: resource.digest.clone(),
                    },
                    server,
                )
            }));
        }
    }

    Ok(PreparedMcpServers {
        servers,
        credential_resolver: Arc::new(credential_resolver),
    })
}

pub(crate) async fn connect_mcp_servers(
    prepared: PreparedMcpServers,
    docker: Arc<DockerExecutionHostRunner>,
) -> Result<(McpBindings, McpStartupMetrics), String> {
    let server_count = prepared.servers.len();
    let mut contracts = Vec::new();
    let mut providers: Vec<Arc<dyn DynamicToolProvider + Send + Sync>> = Vec::new();
    let mut provider_ids = HashSet::new();
    for (identity, server) in prepared.servers {
        let plugin_name = identity.plugin_name.clone();
        let authorization = Arc::new(McpDispatchAuthorization {
            identity,
            resolver: prepared.credential_resolver.clone(),
            binding_digest: tokio::sync::Mutex::new(None),
        });
        let binding = lazy_mcp_server_binding(
            plugin_name.as_str(),
            &server,
            Arc::new(WorkspaceMcpConnector {
                plugin_name: plugin_name.clone(),
                server: server.clone(),
                authorization: authorization.clone(),
                docker: docker.clone(),
            }),
        )?;
        if !provider_ids.insert(binding.provider.provider_id().to_string()) {
            return Err(format!(
                "duplicate MCP server identity: {}",
                binding.provider.provider_id()
            ));
        }
        contracts.extend(binding.contracts);
        providers.push(binding.provider);
    }
    Ok((
        McpBindings {
            contracts,
            providers,
        },
        McpStartupMetrics {
            server_count,
            credential_count: 0,
            connection_count: 0,
            credential_resolve_ms: 0,
            discovery_ms: 0,
        },
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::extension::{
        mcp_model_contract_digest, McpLifecycleV1, McpServersFileV1, MCP_SERVERS_SCHEMA_V1,
    };
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_identity() -> McpConnectorIdentity {
        McpConnectorIdentity {
            plugin_name: "banana".into(),
            server_id: "banana-source".into(),
            resource_path: "mcp/banana.json".into(),
            resource_digest: format!("sha256:{}", "c".repeat(64)),
        }
    }

    fn authorized_reply(digest: char, token: Option<&str>) -> serde_json::Value {
        json!({"schema":"runtime.mcp_connector.authorized.v1", "scope":"managed",
            "bindingDigest": format!("sha256:{}", digest.to_string().repeat(64)), "token":token})
    }

    fn fake_request() -> DynamicToolProviderRequest {
        DynamicToolProviderRequest {
            tool_call_id: "call".into(),
            tool_name: "banana_search".into(),
            args_json: json!({"secretRef":"model-selected-secret", "serverId":"other-server"})
                .to_string(),
            cancellation_probe: None,
            contract: centaeris_core::tool::ToolContract {
                name: "banana_search".into(),
                category: "mcp".into(),
                summary: "Synthetic fixture".into(),
                input_schema: json!({"type":"object"}),
                concurrency_safe: true,
                turn_behavior: centaeris_core::tool::ToolTurnBehavior::ContinueTurn,
                provider_id: Some("mcp:banana:banana-source".into()),
                schema_hash: None,
                scopes: vec![],
                dynamic: true,
            },
        }
    }

    struct FakeProvider {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl DynamicToolProvider for FakeProvider {
        fn provider_id(&self) -> &str {
            "mcp:banana:banana-source"
        }
        fn execute<'a>(
            &'a self,
            _req: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(DynamicToolProviderResponse {
                    content: "ok".into(),
                    details: json!({}),
                    is_error: false,
                    facts: vec![],
                    transition_reason: None,
                })
            })
        }
        fn execute_with_error_info<'a>(
            &'a self,
            _req: DynamicToolProviderRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<DynamicToolProviderResponse, ToolErrorInfo>> + Send + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(ToolErrorInfo::new(
                    centaeris_core::tool::ToolFailureKind::PermissionDenied,
                    "downstream permission",
                    "downstream permission",
                )
                .with_diagnostic("fake-diagnostic"))
            })
        }
    }

    struct FakeConnector {
        authorization: Arc<McpDispatchAuthorization>,
        server: McpServerDeclarationV1,
        provider: Arc<FakeProvider>,
        connects: Arc<std::sync::atomic::AtomicUsize>,
        initialization: Option<Arc<InitializationBarrier>>,
    }
    impl McpServerConnector for FakeConnector {
        fn connect<'a>(
            &'a self,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            Arc<dyn DynamicToolProvider + Send + Sync>,
                            McpConnectError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.authorization
                    .connect_token(&self.server)
                    .await
                    .map_err(McpConnectError::Unavailable)?;
                if let Some(initialization) = &self.initialization {
                    initialization.token_received.wait().await;
                    initialization.release.wait().await;
                }
                self.connects
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Arc::new(AuthorizedMcpProvider {
                    inner: self.provider.clone(),
                    authorization: self.authorization.clone(),
                })
                    as Arc<dyn DynamicToolProvider + Send + Sync>)
            })
        }
    }

    struct GuardFixture {
        provider: Arc<dyn DynamicToolProvider + Send + Sync>,
        authorization: Arc<McpDispatchAuthorization>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        connects: Arc<std::sync::atomic::AtomicUsize>,
        requests: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        task: tokio::task::JoinHandle<()>,
        resource_digest: String,
    }
    impl Drop for GuardFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn guard_fixture(
        replies: Vec<(axum::http::StatusCode, serde_json::Value)>,
        transport: Option<McpTransportV1>,
    ) -> GuardFixture {
        let (url, requests, task) = fake_authorization_api(replies).await;
        let (root, activation) = banana_package();
        let resource_digest = activation.packages[0].mcp_servers[0].digest.clone();
        let mut resolver = McpCredentialResolver::new(
            url,
            "internal-secret".into(),
            "run".into(),
            "authorization".into(),
            format!("sha256:{}", "b".repeat(64)),
        )
        .unwrap();
        resolver.client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut prepared = prepare_mcp_servers_at(activation, resolver, &root).unwrap();
        let (identity, mut server) = prepared.servers.pop().unwrap();
        if let Some(transport) = transport {
            server.transport = transport;
        }
        let authorization = Arc::new(McpDispatchAuthorization {
            identity,
            resolver: prepared.credential_resolver,
            binding_digest: tokio::sync::Mutex::new(None),
        });
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let inner = lazy_mcp_server_binding(
            "banana",
            &server,
            Arc::new(FakeConnector {
                authorization: authorization.clone(),
                server: server.clone(),
                provider: Arc::new(FakeProvider {
                    calls: calls.clone(),
                }),
                connects: connects.clone(),
                initialization: None,
            }),
        )
        .unwrap()
        .provider;
        fs::remove_dir_all(root).unwrap();
        GuardFixture {
            provider: inner,
            authorization,
            calls,
            connects,
            requests,
            task,
            resource_digest,
        }
    }

    struct InitializationBarrier {
        token_received: tokio::sync::Barrier,
        release: tokio::sync::Barrier,
    }

    // Observe Pending from the actual Core lazy future, after any outer check.
    // While the first connector holds initialization, this proves the second
    // call has entered Core's connection queue rather than merely been spawned.
    struct QueuedProviderProbe {
        inner: Arc<dyn DynamicToolProvider + Send + Sync>,
        queued: Arc<tokio::sync::Notify>,
    }

    impl DynamicToolProvider for QueuedProviderProbe {
        fn provider_id(&self) -> &str {
            self.inner.provider_id()
        }

        fn execute<'a>(
            &'a self,
            request: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async move {
                let mut observe = request.tool_call_id == "queued";
                let mut future = self.inner.execute(request);
                futures::future::poll_fn(|context| {
                    let result = future.as_mut().poll(context);
                    if observe && result.is_pending() {
                        self.queued.notify_one();
                        observe = false;
                    }
                    result
                })
                .await
            })
        }

        fn execute_with_error_info<'a>(
            &'a self,
            request: DynamicToolProviderRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<DynamicToolProviderResponse, ToolErrorInfo>> + Send + 'a,
            >,
        > {
            Box::pin(async move {
                let mut observe = request.tool_call_id == "queued";
                let mut future = self.inner.execute_with_error_info(request);
                futures::future::poll_fn(|context| {
                    let result = future.as_mut().poll(context);
                    if observe && result.is_pending() {
                        self.queued.notify_one();
                        observe = false;
                    }
                    result
                })
                .await
            })
        }
    }

    async fn initialization_revocation(structured_errors: bool, reason: &str, status: u16) {
        use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering::SeqCst};
        let authority_status = Arc::new(AtomicU16::new(200));
        let current_status = authority_status.clone();
        let app = axum::Router::new().route(
            "/internal/mcp-connectors/authorize",
            axum::routing::post(move |body: axum::body::Bytes| {
                let current_status = current_status.clone();
                async move {
                    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let status =
                        axum::http::StatusCode::from_u16(current_status.load(SeqCst)).unwrap();
                    let reply = if status.is_success() {
                        authorized_reply(
                            'a',
                            (request["operation"] == "connect").then_some("test-secret"),
                        )
                    } else {
                        json!({"error": "synthetic authority withdrawn"})
                    };
                    (
                        status,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        reply.to_string(),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let api_task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (root, activation) = banana_package();
        let mut resolver = McpCredentialResolver::new(
            url,
            "internal-secret".into(),
            "run".into(),
            "authorization".into(),
            format!("sha256:{}", "b".repeat(64)),
        )
        .unwrap();
        resolver.client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut prepared = prepare_mcp_servers_at(activation, resolver, &root).unwrap();
        let (identity, server) = prepared.servers.pop().unwrap();
        fs::remove_dir_all(root).unwrap();
        let authorization = Arc::new(McpDispatchAuthorization {
            identity,
            resolver: prepared.credential_resolver,
            binding_digest: tokio::sync::Mutex::new(None),
        });
        let initialization = Arc::new(InitializationBarrier {
            token_received: tokio::sync::Barrier::new(2),
            release: tokio::sync::Barrier::new(2),
        });
        let queued = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let connects = Arc::new(AtomicUsize::new(0));
        let lazy = lazy_mcp_server_binding(
            "banana",
            &server,
            Arc::new(FakeConnector {
                authorization: authorization.clone(),
                server: server.clone(),
                provider: Arc::new(FakeProvider {
                    calls: calls.clone(),
                }),
                connects: connects.clone(),
                initialization: Some(initialization.clone()),
            }),
        )
        .unwrap()
        .provider;
        let provider = QueuedProviderProbe {
            inner: lazy,
            queued: queued.clone(),
        };
        let invoke = |request| async {
            if structured_errors {
                provider
                    .execute_with_error_info(request)
                    .await
                    .map_err(|error| error.model_message)
            } else {
                provider.execute(request).await
            }
        };
        let first = invoke(fake_request());
        tokio::pin!(first);
        tokio::select! {
            _ = initialization.token_received.wait() => {},
            result = &mut first => panic!("first call finished before initialization barrier: {result:?}"),
        }
        let mut second_request = fake_request();
        second_request.tool_call_id = "queued".into();
        let second = invoke(second_request);
        tokio::pin!(second);
        tokio::select! {
            _ = queued.notified() => {},
            result = &mut first => panic!("first call escaped initialization barrier: {result:?}"),
            result = &mut second => panic!("queued call finished before release: {result:?}"),
        }
        authority_status.store(status, SeqCst);
        let (_, (first_result, second_result)) =
            tokio::join!(initialization.release.wait(), async {
                tokio::join!(first, second)
            },);
        api_task.abort();
        assert_eq!(
            calls.load(SeqCst),
            0,
            "{reason}: dispatch after initialization/queue used withdrawn authority"
        );
        assert!(
            first_result.is_err() && second_result.is_err(),
            "{reason}: both calls must reject"
        );
        assert_eq!(connects.load(SeqCst), 1);
    }

    #[tokio::test]
    async fn connector_initialization_revocation_blocks_first_and_queued_execute() {
        for (reason, status) in [
            ("approval revoked", 403),
            ("binding removed", 403),
            ("membership revoked", 403),
            ("credential rotated", 409),
        ] {
            tokio::time::timeout(
                Duration::from_secs(10),
                initialization_revocation(false, reason, status),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn connector_initialization_revocation_blocks_first_and_queued_error_info() {
        for (reason, status) in [
            ("approval revoked", 403),
            ("binding removed", 403),
            ("membership revoked", 403),
            ("credential rotated", 409),
        ] {
            tokio::time::timeout(
                Duration::from_secs(10),
                initialization_revocation(true, reason, status),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "launched by Django LiveServerTestCase with a synthetic test database"]
    async fn python_connector_interoperability() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let fixture: serde_json::Value = serde_json::from_str(
            &std::env::var("ASSISTANT_CONNECTOR_FIXTURE").expect("Django fixture required"),
        )
        .unwrap();
        let field = |name: &str| fixture[name].as_str().unwrap().to_owned();
        let database = fixture["database"].clone();
        assert!(database["NAME"].as_str().unwrap().starts_with("test_"));
        let mut resolver = McpCredentialResolver::new(
            field("liveServerUrl"),
            "test-internal-token".into(),
            field("agentRunId"),
            field("authorizationRef"),
            field("authorizationDigest"),
        )
        .unwrap();
        resolver.client = reqwest::Client::builder().no_proxy().build().unwrap();
        let authorization = Arc::new(McpDispatchAuthorization {
            identity: McpConnectorIdentity {
                plugin_name: "banana".into(),
                server_id: "ledger".into(),
                resource_path: field("resourcePath"),
                resource_digest: field("resourceDigest"),
            },
            resolver: Arc::new(resolver),
            binding_digest: tokio::sync::Mutex::new(None),
        });
        let tools = vec![McpToolDeclarationV1 {
            source_name: "search".into(),
            name: "banana_search".into(),
            description: "Synthetic ledger search.".into(),
            input_schema: json!({"type": "object"}),
            concurrency_safe: true,
            scopes: vec!["banana:read".into()],
        }];
        let server = McpServerDeclarationV1 {
            id: "ledger".into(),
            model_contract_digest: mcp_model_contract_digest("ledger", &tools).unwrap(),
            transport: McpTransportV1::StreamableHttp {
                url: "https://business.invalid/mcp".into(),
                bearer_credential_ref: Some("business-token".into()),
            },
            lifecycle: McpLifecycleV1::Auto,
            startup_timeout_ms: 10_000,
            tool_timeout_ms: 60_000,
            tools,
        };
        struct InteropProvider(Arc<AtomicUsize>);
        impl DynamicToolProvider for InteropProvider {
            fn provider_id(&self) -> &str {
                "mcp:banana:ledger"
            }
            fn execute<'a>(
                &'a self,
                request: DynamicToolProviderRequest,
            ) -> Pin<
                Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>,
            > {
                Box::pin(async move {
                    FakeProvider {
                        calls: self.0.clone(),
                    }
                    .execute(request)
                    .await
                })
            }
        }
        struct InteropConnector {
            authorization: Arc<McpDispatchAuthorization>,
            server: McpServerDeclarationV1,
            calls: Arc<AtomicUsize>,
            connects: Arc<AtomicUsize>,
        }
        impl McpServerConnector for InteropConnector {
            fn connect<'a>(
                &'a self,
            ) -> Pin<
                Box<
                    dyn Future<
                            Output = Result<
                                Arc<dyn DynamicToolProvider + Send + Sync>,
                                McpConnectError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    let token = self
                        .authorization
                        .connect_token(&self.server)
                        .await
                        .map_err(McpConnectError::Unavailable)?;
                    assert_eq!(token.as_deref(), Some("fixture-account-one"));
                    self.connects.fetch_add(1, SeqCst);
                    Ok(Arc::new(AuthorizedMcpProvider {
                        inner: Arc::new(InteropProvider(self.calls.clone())),
                        authorization: self.authorization.clone(),
                    })
                        as Arc<dyn DynamicToolProvider + Send + Sync>)
                })
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let connects = Arc::new(AtomicUsize::new(0));
        let inner = lazy_mcp_server_binding(
            "banana",
            &server,
            Arc::new(InteropConnector {
                authorization: authorization.clone(),
                server: server.clone(),
                calls: calls.clone(),
                connects: connects.clone(),
            }),
        )
        .unwrap()
        .provider;
        let provider = inner;
        let request = || {
            let mut request = fake_request();
            request.contract.provider_id = Some("mcp:banana:ledger".into());
            request
        };
        assert!(provider.execute(request()).await.is_ok());
        assert!(provider.execute(request()).await.is_ok());
        assert_eq!(calls.load(SeqCst), 2);
        assert_eq!(connects.load(SeqCst), 1);
        let approval_id = field("approvalId");
        tokio::task::spawn_blocking(move || {
            let mut config = postgres::Config::new();
            config.dbname(database["NAME"].as_str().unwrap())
                .user(database["USER"].as_str().unwrap())
                .password(database["PASSWORD"].as_str().unwrap())
                .host(database["HOST"].as_str().unwrap())
                .port(database["PORT"].as_str().unwrap().parse().unwrap());
            let mut client = config.connect(postgres::NoTls).unwrap();
            let actual: String = client.query_one("SELECT current_database()", &[]).unwrap().get(0);
            assert_eq!(actual, database["NAME"].as_str().unwrap());
            assert!(actual.starts_with("test_"));
            assert_eq!(client.execute(
                "UPDATE app_core_agentconnectorcredentialapproval SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
                &[&approval_id],
            ).unwrap(), 1);
        }).await.unwrap();
        assert!(provider.execute(request()).await.is_err());
        assert!(authorization.connect_token(&server).await.is_err());
        assert_eq!(calls.load(SeqCst), 2);
        assert_eq!(connects.load(SeqCst), 1);
        println!("assistant_connector_interoperability: cached_calls=2 revoked_calls=0");
    }

    #[tokio::test]
    async fn connector_cached_calls_recheck_and_reject_revocation() {
        use axum::http::StatusCode;
        use std::sync::atomic::Ordering::SeqCst;
        let fixture = guard_fixture(
            vec![
                (StatusCode::OK, authorized_reply('a', Some("test-secret"))),
                (StatusCode::OK, authorized_reply('a', None)),
                (StatusCode::OK, authorized_reply('a', None)),
                (
                    StatusCode::FORBIDDEN,
                    json!({"error":"revoked test-secret internal-secret"}),
                ),
            ],
            None,
        )
        .await;
        assert!(fixture.provider.execute(fake_request()).await.is_ok());
        assert!(fixture.provider.execute(fake_request()).await.is_ok());
        let error = fixture.provider.execute(fake_request()).await.unwrap_err();
        assert!(!error.contains("test-secret") && !error.contains("internal-secret"));
        assert_eq!(fixture.calls.load(SeqCst), 2);
        assert_eq!(fixture.connects.load(SeqCst), 1);
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|body| body["operation"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["connect", "dispatch", "dispatch", "dispatch"]
        );
        for body in requests.iter() {
            assert_eq!(body["pluginName"], "banana");
            assert_eq!(body["serverId"], "banana-source");
            assert_eq!(body["resourcePath"], "mcp/banana.json");
            assert_eq!(body["resourceDigest"], fixture.resource_digest);
            assert!(body.get("secretRef").is_none() && body.get("credentialRef").is_none());
        }
        assert!(requests[0]["bindingDigest"].is_null());
        assert_eq!(
            requests[1]["bindingDigest"],
            format!("sha256:{}", "a".repeat(64))
        );
    }

    #[tokio::test]
    async fn connector_cached_binding_change_does_not_call_or_reconnect() {
        use axum::http::StatusCode;
        let fixture = guard_fixture(
            vec![
                (StatusCode::OK, authorized_reply('a', Some("test-secret"))),
                (StatusCode::OK, authorized_reply('a', None)),
                (StatusCode::OK, authorized_reply('b', None)),
            ],
            None,
        )
        .await;
        assert!(fixture.provider.execute(fake_request()).await.is_ok());
        assert!(fixture
            .provider
            .execute(fake_request())
            .await
            .unwrap_err()
            .contains("binding changed"));
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            fixture.connects.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn connector_dispatch_rejects_binding_changed_after_connect() {
        use axum::http::StatusCode;
        let fixture = guard_fixture(
            vec![
                (StatusCode::OK, authorized_reply('a', Some("test-secret"))),
                (StatusCode::OK, authorized_reply('b', None)),
            ],
            None,
        )
        .await;
        assert!(fixture.provider.execute(fake_request()).await.is_err());
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            fixture.connects.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn connector_without_bearer_and_stdio_still_check_each_dispatch() {
        use axum::http::StatusCode;
        for transport in [
            McpTransportV1::StreamableHttp {
                url: "https://banana.invalid/mcp".into(),
                bearer_credential_ref: None,
            },
            McpTransportV1::Stdio {
                program: "banana".into(),
                args: vec![],
            },
        ] {
            let fixture = guard_fixture(
                vec![
                    (StatusCode::OK, authorized_reply('a', None)),
                    (StatusCode::OK, authorized_reply('a', None)),
                    (StatusCode::FORBIDDEN, json!({"error":"revoked"})),
                ],
                Some(transport),
            )
            .await;
            assert!(fixture.provider.execute(fake_request()).await.is_ok());
            assert!(fixture.provider.execute(fake_request()).await.is_err());
            assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(
                fixture.connects.load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            assert_eq!(fixture.requests.lock().unwrap().len(), 3);
        }
    }

    #[tokio::test]
    async fn connector_invalid_authorization_never_calls_provider() {
        let mut invalid = Vec::new();
        for key in ["schema", "scope", "bindingDigest", "token"] {
            let mut reply = authorized_reply('a', None);
            reply.as_object_mut().unwrap().remove(key);
            invalid.push(reply);
        }
        for (key, value) in [
            ("extra", json!(true)),
            ("schema", json!("unknown")),
            ("scope", json!("unknown")),
            ("bindingDigest", json!("sha256:invalid")),
            ("token", json!("unexpected-secret")),
        ] {
            let mut reply = authorized_reply('a', None);
            reply[key] = value;
            invalid.push(reply);
        }
        for reply in invalid {
            let fixture = guard_fixture(
                vec![
                    (
                        axum::http::StatusCode::OK,
                        authorized_reply('a', Some("test-secret")),
                    ),
                    (axum::http::StatusCode::OK, reply),
                ],
                None,
            )
            .await;
            assert!(fixture.provider.execute(fake_request()).await.is_err());
            assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            assert_eq!(
                fixture.connects.load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            assert_eq!(fixture.requests.lock().unwrap().len(), 2);
        }
    }

    #[tokio::test]
    async fn connector_api_failure_rejects_cached_provider_without_fallback() {
        use axum::http::StatusCode;
        let fixture = guard_fixture(
            vec![
                (StatusCode::OK, authorized_reply('a', Some("test-secret"))),
                (StatusCode::OK, authorized_reply('a', None)),
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error":"internal-secret test-secret"}),
                ),
            ],
            None,
        )
        .await;
        assert!(fixture.provider.execute(fake_request()).await.is_ok());
        let error = fixture
            .provider
            .execute_with_error_info(fake_request())
            .await
            .unwrap_err();
        let visible = format!("{} {}", error.model_message, error.user_message);
        assert!(!visible.contains("internal-secret") && !visible.contains("test-secret"));
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            fixture.connects.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn connector_preserves_structured_provider_error() {
        let fixture = guard_fixture(
            vec![(axum::http::StatusCode::OK, authorized_reply('a', None))],
            None,
        )
        .await;
        let provider = AuthorizedMcpProvider {
            inner: Arc::new(FakeProvider {
                calls: fixture.calls.clone(),
            }),
            authorization: fixture.authorization.clone(),
        };
        let error = provider
            .execute_with_error_info(fake_request())
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            centaeris_core::tool::ToolFailureKind::PermissionDenied
        );
        assert_eq!(error.diagnostic_id.as_deref(), Some("fake-diagnostic"));
        assert_eq!(error.model_message, "downstream permission");
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    async fn fake_authorization_api(
        replies: Vec<(axum::http::StatusCode, serde_json::Value)>,
    ) -> (
        String,
        Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let replies = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
            replies,
        )));
        let app = axum::Router::new().route(
            "/internal/mcp-connectors/authorize",
            axum::routing::post(move |body: axum::body::Bytes| {
                let recorded = recorded.clone();
                let replies = replies.clone();
                async move {
                    recorded
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    let (status, reply) = replies.lock().unwrap().pop_front().unwrap();
                    (
                        status,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        reply.to_string(),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, requests, task)
    }

    #[tokio::test]
    async fn connector_uses_current_authorization_api() {
        let (url, requests, task) = fake_authorization_api(vec![(
            axum::http::StatusCode::OK,
            json!({"schema":"runtime.mcp_connector.authorized.v1", "scope":"managed",
                "bindingDigest":format!("sha256:{}", "a".repeat(64)), "token":"test-secret"}),
        )])
        .await;
        let mut resolver = McpCredentialResolver::new(
            url,
            "internal-secret".into(),
            "run".into(),
            "authorization".into(),
            format!("sha256:{}", "b".repeat(64)),
        )
        .unwrap();
        resolver.client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = resolver.authorize(&test_identity(), "connect", None).await;
        assert!(
            result.is_ok(),
            "connector must use current authorization API: {:?}",
            result.err()
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
        task.abort();
    }

    fn banana_package() -> (PathBuf, PluginActivationSnapshotV1) {
        use centaeris_core::extension::build_plugin_activation_snapshot;

        let catalog_root = std::env::temp_dir().join(format!(
            "centaeris-banana-mcp-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let package_root = catalog_root.join("banana");
        fs::create_dir_all(package_root.join(".centaeris-plugin")).unwrap();
        fs::create_dir_all(package_root.join("mcp")).unwrap();
        fs::write(
            package_root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"banana","version":"1.0.0","paths":{"mcpServers":["mcp/banana.json"]}}"#,
        )
        .unwrap();
        let tools = vec![McpToolDeclarationV1 {
            source_name: "search".to_string(),
            name: "banana_search".to_string(),
            description: "Search bananas.".to_string(),
            input_schema: json!({"type": "object"}),
            concurrency_safe: true,
            scopes: vec!["banana:read".to_string()],
        }];
        fs::write(
            package_root.join("mcp/banana.json"),
            serde_json::to_vec_pretty(&McpServersFileV1 {
                schema: MCP_SERVERS_SCHEMA_V1.to_string(),
                servers: vec![McpServerDeclarationV1 {
                    id: "banana-source".to_string(),
                    model_contract_digest: mcp_model_contract_digest(
                        "banana-source",
                        tools.as_slice(),
                    )
                    .expect("model contract digest"),
                    transport: McpTransportV1::StreamableHttp {
                        url: "https://banana.invalid/mcp".to_string(),
                        bearer_credential_ref: Some("banana-token".to_string()),
                    },
                    lifecycle: McpLifecycleV1::Auto,
                    startup_timeout_ms: 10_000,
                    tool_timeout_ms: 60_000,
                    tools,
                }],
            })
            .expect("MCP declaration"),
        )
        .unwrap();
        let activation = build_plugin_activation_snapshot(std::slice::from_ref(&package_root))
            .expect("banana activation");
        (catalog_root, activation)
    }

    #[test]
    fn workspace_catalog_reuses_frozen_package_and_core_declaration() {
        let (catalog_root, activation) = banana_package();

        let catalog = workspace_mcp_catalog_at(&activation, catalog_root.as_path())
            .expect("Workspace MCP catalog");

        assert_eq!(catalog.plugins.len(), 1);
        assert_eq!(catalog.plugins[0].plugin_name, "banana");
        assert_eq!(catalog.plugins[0].servers.len(), 1);
        assert_eq!(catalog.plugins[0].servers[0].id, "banana-source");
        assert_eq!(
            catalog.plugins[0].servers[0].transport.endpoint.as_deref(),
            Some("https://banana.invalid/mcp")
        );
        assert_eq!(
            catalog.plugins[0].servers[0].auth.credential_ref.as_deref(),
            Some("banana-token")
        );
        assert_eq!(catalog.plugins[0].servers[0].tools[0].name, "banana_search");
    }

    #[test]
    fn credential_protocol_is_strict_and_secret_safe() {
        assert!(valid_bearer_token("test-secret"));
        assert!(!valid_bearer_token("banana token"));

        assert_eq!(
            serde_json::to_value(McpCredentialResolveRequest {
                schema: "runtime.mcp_connector.authorization.v1",
                agent_run_id: "agent-run",
                authorization_ref: "authorization",
                authorization_digest: "sha256:banana",
                plugin_name: "banana",
                server_id: "banana-source",
                resource_path: "mcp/banana.json",
                resource_digest: "sha256:resource",
                operation: "connect",
                binding_digest: None,
            })
            .expect("serialize resolver request"),
            json!({
                "schema": "runtime.mcp_connector.authorization.v1",
                "agentRunId": "agent-run",
                "authorizationRef": "authorization",
                "authorizationDigest": "sha256:banana",
                "pluginName": "banana",
                "serverId": "banana-source",
                "resourcePath": "mcp/banana.json",
                "resourceDigest": "sha256:resource",
                "operation": "connect",
                "bindingDigest": null,
            })
        );
        assert!(
            serde_json::from_value::<McpCredentialResolvedResponse>(json!({
                "schema": "runtime.mcp_bearer_credential.resolved.v1",
                "token": "test-secret",
                "banana": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn preparation_rejects_aggregate_declarations_before_connections() {
        let (catalog_root, _) = banana_package();
        let package_root = catalog_root.join("banana");
        let base = load_mcp_servers_file(&package_root.join("mcp/banana.json")).unwrap();
        let mut paths = Vec::new();
        for index in 0..2 {
            let mut server = base.servers[0].clone();
            server.id = format!("banana-source-{index}");
            server.tools = (0..48)
                .map(|tool_index| {
                    let mut tool = base.servers[0].tools[0].clone();
                    tool.source_name = format!("search_{tool_index:03}");
                    tool.name = format!("banana_{index}_search_{tool_index:03}");
                    tool.input_schema =
                        json!({"type": "object", "description": "x".repeat(48 * 1024)});
                    tool
                })
                .collect();
            server.model_contract_digest =
                mcp_model_contract_digest(&server.id, &server.tools).unwrap();
            let relative = format!("mcp/large-{index}.json");
            fs::write(
                package_root.join(&relative),
                serde_json::to_vec(&McpServersFileV1 {
                    schema: MCP_SERVERS_SCHEMA_V1.to_string(),
                    servers: vec![server],
                })
                .unwrap(),
            )
            .unwrap();
            load_mcp_servers_file(&package_root.join(&relative)).expect("individual file fits");
            paths.push(relative);
        }
        fs::write(
            package_root.join(".centaeris-plugin/plugin.json"),
            serde_json::to_vec(&json!({
                "name": "banana", "version": "1.0.0", "paths": {"mcpServers": paths},
            }))
            .unwrap(),
        )
        .unwrap();
        let activation =
            centaeris_core::extension::build_plugin_activation_snapshot(&[package_root]).unwrap();
        let resolver = McpCredentialResolver::new(
            "http://127.0.0.1:9".to_string(),
            "unused".to_string(),
            "agent-run".to_string(),
            "authorization".to_string(),
            "sha256:banana".to_string(),
        )
        .unwrap();
        assert!(workspace_mcp_catalog_at(&activation, &catalog_root)
            .unwrap_err()
            .contains("4194304 bytes"));
        let error = prepare_mcp_servers_at(activation, resolver, &catalog_root)
            .err()
            .expect("aggregate declarations must fail");
        assert!(error.contains("4194304 bytes"));
        fs::remove_dir_all(catalog_root).unwrap();
    }

    #[test]
    fn agent_run_preparation_loads_static_contract_without_credentials_or_network() {
        let (catalog_root, activation) = banana_package();
        let prepared = prepare_mcp_servers_at(
            activation,
            McpCredentialResolver::new(
                "http://127.0.0.1:9".to_string(),
                "unused".to_string(),
                "agent-run".to_string(),
                "authorization".to_string(),
                "sha256:banana".to_string(),
            )
            .expect("credential resolver"),
            catalog_root.as_path(),
        )
        .expect("static preparation");
        assert_eq!(prepared.servers.len(), 1);
        let (identity, server) = &prepared.servers[0];
        let contracts = server
            .dynamic_tool_contracts(identity.plugin_name.as_str())
            .expect("static contract");
        assert_eq!(contracts[0].provider_id, "mcp:banana:banana-source");
    }
}
