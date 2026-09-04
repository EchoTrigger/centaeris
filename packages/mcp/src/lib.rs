use centaeris_core::extension::{
    mcp_provider_id, McpLifecycleV1, McpServerDeclarationV1, McpToolDeclarationV1, McpTransportV1,
};
use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
};
use centaeris_core::tool::limits::{
    json_size_with_limit, ToolContractBudget, MAX_TOOL_CONTRACT_BYTES,
};
use centaeris_core::tool::DynamicToolContract;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ProtocolVersion, Tool,
};
use rmcp::service::{RunningService, ServiceError};
use rmcp::transport::common::client_side_sse::NeverRetry;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{IntoTransport, StreamableHttpClientTransport};
use rmcp::{ClientLifecycleMode, ClientServiceExt, RoleClient};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const MAX_MCP_RESULT_BYTES: usize = 256 * 1024;
const RETRYABLE_CONNECT_FAILURE_COOLDOWN: Duration = Duration::from_millis(250);
const MAX_DISCOVERY_PAGES: usize = 256;

mod transport;
pub use transport::{bounded_stdio_transport, BoundedStdioTransport};

type DynamicProvider = Arc<dyn DynamicToolProvider + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpConnectError {
    ContractMismatch(String),
    Unavailable(String),
}

impl McpConnectError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl fmt::Display for McpConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContractMismatch(message) => {
                write!(formatter, "mcp_model_contract_mismatch: {message}")
            }
            Self::Unavailable(message) => write!(formatter, "mcp_connection_failed: {message}"),
        }
    }
}

impl std::error::Error for McpConnectError {}

pub trait McpServerConnector {
    fn connect<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicProvider, McpConnectError>> + Send + 'a>>;
}

pub struct McpServerBinding {
    pub contracts: Vec<DynamicToolContract>,
    pub provider: DynamicProvider,
}

pub fn lazy_mcp_server_binding(
    plugin_name: &str,
    server: &McpServerDeclarationV1,
    connector: Arc<dyn McpServerConnector + Send + Sync>,
) -> Result<McpServerBinding, String> {
    let contracts = server.dynamic_tool_contracts(plugin_name)?;
    let provider_id = mcp_provider_id(plugin_name, server.id.as_str())?;
    Ok(McpServerBinding {
        contracts,
        provider: Arc::new(LazyMcpDynamicToolProvider {
            provider_id,
            connector,
            state: Mutex::new(LazyConnectionState::Empty),
        }),
    })
}

pub async fn connect_streamable_http_mcp_server(
    plugin_name: &str,
    server: McpServerDeclarationV1,
    bearer_token: Option<&str>,
) -> Result<DynamicProvider, McpConnectError> {
    server
        .dynamic_tool_contracts(plugin_name)
        .map_err(McpConnectError::ContractMismatch)?;
    let (url, credential_required) = match &server.transport {
        McpTransportV1::StreamableHttp {
            url,
            bearer_credential_ref,
        } => (url.as_str(), bearer_credential_ref.is_some()),
        McpTransportV1::Stdio { .. } => {
            return Err(McpConnectError::Unavailable(
                "MCP server transport is not streamable HTTP".to_string(),
            ))
        }
    };
    if credential_required != bearer_token.is_some() {
        return Err(McpConnectError::Unavailable(
            "MCP bearer credential binding does not match declaration".to_string(),
        ));
    }
    if bearer_token.is_some_and(|token| !valid_bearer_token(token)) {
        return Err(McpConnectError::Unavailable(
            "MCP bearer credential is invalid".to_string(),
        ));
    }
    let transport = bounded_http_transport(url, bearer_token)?;
    connect_mcp_server_transport(plugin_name, server, transport).await
}

pub async fn connect_mcp_server_transport<T, E, A>(
    plugin_name: &str,
    server: McpServerDeclarationV1,
    transport: T,
) -> Result<DynamicProvider, McpConnectError>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    server
        .dynamic_tool_contracts(plugin_name)
        .map_err(McpConnectError::ContractMismatch)?;
    let provider_id = mcp_provider_id(plugin_name, server.id.as_str())
        .map_err(McpConnectError::ContractMismatch)?;
    let startup_timeout = Duration::from_millis(server.startup_timeout_ms);
    let (client, protocol_version, discovered) = initialize_client_with_timeout(
        transport,
        startup_timeout,
        server.lifecycle,
        provider_id.as_str(),
    )
    .await?;
    let tools = validate_live_tools(server.tools.as_slice(), discovered.as_slice())?;
    let provider: DynamicProvider = Arc::new(McpDynamicToolProvider {
        provider_id,
        plugin_name: plugin_name.to_string(),
        server_id: server.id,
        protocol_version,
        tool_timeout: Duration::from_millis(server.tool_timeout_ms),
        tools,
        client,
    });
    Ok(provider)
}

enum LazyConnectionState {
    Empty,
    Connected(DynamicProvider),
    ContractMismatch(McpConnectError),
    RetryableFailure {
        error: McpConnectError,
        retry_after: Instant,
    },
}

struct LazyMcpDynamicToolProvider {
    provider_id: String,
    connector: Arc<dyn McpServerConnector + Send + Sync>,
    state: Mutex<LazyConnectionState>,
}

impl LazyMcpDynamicToolProvider {
    async fn connected_provider(&self) -> Result<DynamicProvider, McpConnectError> {
        let queued_at = Instant::now();
        let mut state = self.state.lock().await;
        eprintln!(
            "mcp_lazy_queue_profile: providerId={}; queueWaitMs={:.3}; connectionReused={}",
            self.provider_id,
            queued_at.elapsed().as_secs_f64() * 1_000.0,
            matches!(&*state, LazyConnectionState::Connected(_)),
        );
        match &*state {
            LazyConnectionState::Connected(provider) => return Ok(provider.clone()),
            LazyConnectionState::ContractMismatch(error) => return Err(error.clone()),
            LazyConnectionState::RetryableFailure { error, retry_after }
                if Instant::now() < *retry_after =>
            {
                return Err(error.clone());
            }
            LazyConnectionState::RetryableFailure { .. } => {}
            LazyConnectionState::Empty => {}
        }
        match self.connector.connect().await {
            Ok(provider) if provider.provider_id() == self.provider_id => {
                *state = LazyConnectionState::Connected(provider.clone());
                Ok(provider)
            }
            Ok(provider) => {
                let error = McpConnectError::ContractMismatch(format!(
                    "providerId differs: declared={} live={}",
                    self.provider_id,
                    provider.provider_id()
                ));
                *state = LazyConnectionState::ContractMismatch(error.clone());
                Err(error)
            }
            Err(error) if !error.retryable() => {
                *state = LazyConnectionState::ContractMismatch(error.clone());
                Err(error)
            }
            Err(error) => {
                *state = LazyConnectionState::RetryableFailure {
                    error: error.clone(),
                    retry_after: Instant::now() + RETRYABLE_CONNECT_FAILURE_COOLDOWN,
                };
                Err(error)
            }
        }
    }
}

impl DynamicToolProvider for LazyMcpDynamicToolProvider {
    fn provider_id(&self) -> &str {
        self.provider_id.as_str()
    }

    fn execute<'a>(
        &'a self,
        request: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let provider = tokio::select! {
                biased;
                cancellation = request.wait_for_cancellation() => {
                    return Err(format!(
                        "dynamic tool execution cancelled: {}",
                        cancellation?
                    ));
                }
                provider = self.connected_provider() => provider.map_err(|error| error.to_string())?,
            };
            let result = provider.execute(request).await;
            if result
                .as_ref()
                .is_err_and(|error| error.starts_with("mcp_connection_lost:"))
            {
                let mut state = self.state.lock().await;
                if matches!(&*state, LazyConnectionState::Connected(current) if Arc::ptr_eq(current, &provider))
                {
                    *state = LazyConnectionState::Empty;
                }
            }
            result
        })
    }
}

pub fn valid_bearer_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4_096
        && value.is_ascii()
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn streamable_http_config(
    url: &str,
    bearer_token: Option<&str>,
) -> StreamableHttpClientTransportConfig {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    if let Some(token) = bearer_token {
        config = config.auth_header(token.to_string());
    }
    config.retry_config = Arc::new(NeverRetry::default());
    config.reinit_on_expired_session = false;
    config.max_sse_event_size = MAX_TOOL_CONTRACT_BYTES;
    config
}

fn bounded_http_transport(
    url: &str,
    bearer_token: Option<&str>,
) -> Result<StreamableHttpClientTransport<transport::BoundedHttpClient>, McpConnectError> {
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| McpConnectError::Unavailable("build MCP HTTP client failed".to_string()))?;
    Ok(StreamableHttpClientTransport::with_client(
        transport::BoundedHttpClient(client),
        streamable_http_config(url, bearer_token),
    ))
}

async fn initialize_client_with_timeout<T, E, A>(
    transport: T,
    startup_timeout: Duration,
    lifecycle: McpLifecycleV1,
    provider_id: &str,
) -> Result<(RunningService<RoleClient, ()>, String, Vec<Tool>), McpConnectError>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    tokio::time::timeout(
        startup_timeout,
        initialize_client(transport, lifecycle, provider_id),
    )
        .await
        .map_err(|_| {
            eprintln!(
                "mcp_lazy_phase_profile: providerId={provider_id}; phase=connectDiscovery; outcome=timeout; timeoutMs={}",
                startup_timeout.as_millis(),
            );
            McpConnectError::Unavailable("MCP startup and discovery timed out".to_string())
        })?
}

async fn initialize_client<T, E, A>(
    transport: T,
    lifecycle: McpLifecycleV1,
    provider_id: &str,
) -> Result<(RunningService<RoleClient, ()>, String, Vec<Tool>), McpConnectError>
where
    T: IntoTransport<RoleClient, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let lifecycle = match lifecycle {
        McpLifecycleV1::Auto => ClientLifecycleMode::Auto {
            preferred_versions: vec![
                ProtocolVersion::V_2026_07_28,
                ProtocolVersion::V_2025_11_25,
                ProtocolVersion::V_2025_06_18,
            ],
            legacy_version: Some(ProtocolVersion::V_2025_06_18),
        },
        McpLifecycleV1::Initialize => ClientLifecycleMode::Initialize,
    };
    let initialize_started = Instant::now();
    let client = ().serve_with_lifecycle(transport, lifecycle).await;
    eprintln!(
        "mcp_lazy_phase_profile: providerId={provider_id}; phase=initialize; elapsedMs={:.3}; succeeded={}",
        initialize_started.elapsed().as_secs_f64() * 1_000.0,
        client.is_ok(),
    );
    let client = client
        .map_err(|_| McpConnectError::Unavailable("initialize MCP server failed".to_string()))?;
    let protocol_version = client
        .peer_info()
        .map(|info| info.protocol_version.to_string())
        .ok_or_else(|| {
            McpConnectError::Unavailable(
                "MCP server did not provide negotiated protocol version".to_string(),
            )
        })?;
    if !supported_protocol_version(protocol_version.as_str()) {
        return Err(McpConnectError::Unavailable(format!(
            "MCP server negotiated unsupported protocol version: {protocol_version}"
        )));
    }
    let discovery_started = Instant::now();
    let discovered = discover_tools(&client).await;
    eprintln!(
        "mcp_lazy_phase_profile: providerId={provider_id}; phase=listTools; elapsedMs={:.3}; succeeded={}",
        discovery_started.elapsed().as_secs_f64() * 1_000.0,
        discovered.is_ok(),
    );
    let discovered = discovered?;
    Ok((client, protocol_version, discovered))
}

async fn discover_tools(client: &rmcp::Peer<RoleClient>) -> Result<Vec<Tool>, McpConnectError> {
    let mut discovered = Vec::new();
    let mut cursor = None;
    let mut cursors = HashSet::new();
    let mut names = HashSet::new();
    let mut budget = ToolContractBudget::default();
    let mut page_bytes = 0usize;
    for _ in 0..MAX_DISCOVERY_PAGES {
        let mut params = rmcp::model::PaginatedRequestParams::default();
        params.cursor = cursor;
        let page = client
            .list_tools(Some(params))
            .await
            .map_err(|_| McpConnectError::Unavailable("discover MCP tools failed".to_string()))?;
        page_bytes += json_size_with_limit(&page, MAX_TOOL_CONTRACT_BYTES - page_bytes)
            .map_err(McpConnectError::ContractMismatch)?;
        if page.tools.is_empty() && page.next_cursor.is_some() {
            return Err(McpConnectError::ContractMismatch(
                "MCP discovery pagination made no progress".to_string(),
            ));
        }
        for tool in &page.tools {
            budget
                .add(tool)
                .map_err(McpConnectError::ContractMismatch)?;
            if !names.insert(tool.name.to_string()) {
                return Err(McpConnectError::ContractMismatch(
                    "MCP discovery repeated a tool name".to_string(),
                ));
            }
        }
        discovered.extend(page.tools);
        let Some(next) = page.next_cursor else {
            return Ok(discovered);
        };
        if !cursors.insert(next.clone()) {
            return Err(McpConnectError::ContractMismatch(
                "MCP discovery repeated a pagination cursor".to_string(),
            ));
        }
        cursor = Some(next);
    }
    Err(McpConnectError::ContractMismatch(
        "MCP discovery exceeds 256 pages".to_string(),
    ))
}

fn supported_protocol_version(value: &str) -> bool {
    matches!(value, "2026-07-28" | "2025-11-25" | "2025-06-18")
}

fn validate_live_tools(
    declared: &[McpToolDeclarationV1],
    discovered: &[Tool],
) -> Result<HashMap<String, String>, McpConnectError> {
    let declared_source_names = declared
        .iter()
        .map(|tool| tool.source_name.as_str())
        .collect::<HashSet<_>>();
    let mut declared_discovered = HashMap::with_capacity(discovered.len());
    for tool in discovered {
        if declared_discovered
            .insert(tool.name.as_ref(), tool)
            .is_some()
        {
            return Err(McpConnectError::ContractMismatch(format!(
                "duplicate live sourceName: {}",
                tool.name
            )));
        }
        if !declared_source_names.contains(tool.name.as_ref()) {
            return Err(McpConnectError::ContractMismatch(format!(
                "unknown live sourceName: {}",
                tool.name
            )));
        }
    }
    let mut tools = HashMap::with_capacity(declared.len());
    for declaration in declared {
        let tool = declared_discovered
            .get(declaration.source_name.as_str())
            .ok_or_else(|| {
                McpConnectError::ContractMismatch(format!(
                    "declared MCP tool is missing from discovery: {}",
                    declaration.source_name
                ))
            })?;
        if tool.description.as_deref() != Some(declaration.description.as_str()) {
            return Err(McpConnectError::ContractMismatch(format!(
                "description differs for sourceName={}",
                declaration.source_name
            )));
        }
        let input_schema = Value::Object(tool.input_schema.as_ref().clone());
        if input_schema != declaration.input_schema {
            return Err(McpConnectError::ContractMismatch(format!(
                "inputSchema differs for sourceName={}",
                declaration.source_name
            )));
        }
        tools.insert(declaration.name.clone(), declaration.source_name.clone());
    }
    Ok(tools)
}

struct McpDynamicToolProvider {
    provider_id: String,
    plugin_name: String,
    server_id: String,
    protocol_version: String,
    tool_timeout: Duration,
    tools: HashMap<String, String>,
    client: RunningService<RoleClient, ()>,
}

impl DynamicToolProvider for McpDynamicToolProvider {
    fn provider_id(&self) -> &str {
        self.provider_id.as_str()
    }

    fn execute<'a>(
        &'a self,
        request: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            let source_name = self
                .tools
                .get(request.tool_name.as_str())
                .ok_or_else(|| format!("MCP tool is not configured: {}", request.tool_name))?;
            let arguments = serde_json::from_str::<Value>(request.args_json.as_str())
                .map_err(|error| format!("parse MCP tool arguments failed: {error}"))?
                .as_object()
                .cloned()
                .ok_or_else(|| "MCP tool arguments must be a JSON object".to_string())?;
            let response = tokio::select! {
                biased;
                response = tokio::time::timeout(
                    self.tool_timeout,
                    self.client.call_tool_once(
                        CallToolRequestParams::new(source_name.clone()).with_arguments(arguments),
                    ),
                ) => response,
                cancellation = request.wait_for_cancellation() => {
                    return Err(format!(
                        "dynamic tool execution cancelled: {}",
                        cancellation?
                    ));
                }
            };
            let response = response
                .map_err(|_| "MCP tool call timed out".to_string())?
                .map_err(|error| match error {
                    ServiceError::TransportSend(_) | ServiceError::TransportClosed => {
                        "mcp_connection_lost: MCP transport is closed".to_string()
                    }
                    _ => "MCP tool call failed".to_string(),
                })?;
            let result = match response {
                CallToolResponse::Complete(result) => result,
                CallToolResponse::InputRequired(_) => {
                    return Err("MCP input_required results are not supported".to_string())
                }
                CallToolResponse::Task(_) => {
                    return Err("MCP task results are not supported".to_string())
                }
                _ => return Err("MCP tool result type is not supported".to_string()),
            };
            project_result(
                self.plugin_name.as_str(),
                self.server_id.as_str(),
                source_name.as_str(),
                self.protocol_version.as_str(),
                result,
            )
        })
    }
}

fn project_result(
    plugin_name: &str,
    server_id: &str,
    source_name: &str,
    protocol_version: &str,
    result: CallToolResult,
) -> Result<DynamicToolProviderResponse, String> {
    let is_error = result.is_error.unwrap_or(false);
    let mut text = Vec::new();
    for content in result.content {
        match content {
            ContentBlock::Text(item) => text.push(item.text),
            ContentBlock::Image(_) => return Err("MCP image content is not supported".to_string()),
            ContentBlock::Audio(_) => return Err("MCP audio content is not supported".to_string()),
            ContentBlock::Resource(_) | ContentBlock::ResourceLink(_) => {
                return Err("MCP resource content is not supported".to_string())
            }
            _ => return Err("MCP content type is not supported".to_string()),
        }
    }
    let projected = json!({
        "text": text,
        "structuredContent": result.structured_content,
    });
    let encoded = serde_json::to_vec(&projected)
        .map_err(|error| format!("serialize MCP tool result failed: {error}"))?;
    if encoded.len() > MAX_MCP_RESULT_BYTES {
        return Err("MCP tool result exceeded 262144 bytes".to_string());
    }
    let content = match (
        projected.get("text").and_then(Value::as_array),
        projected.get("structuredContent"),
    ) {
        (Some(items), Some(Value::Null)) if items.len() == 1 => {
            items[0].as_str().expect("MCP text projection").to_string()
        }
        (_, Some(value)) if !value.is_null() && projected["text"] == json!([]) => {
            serde_json::to_string(value)
                .map_err(|error| format!("serialize MCP structured result failed: {error}"))?
        }
        _ => String::from_utf8(encoded).expect("JSON is UTF-8"),
    };
    Ok(DynamicToolProviderResponse {
        content,
        details: json!({
            "schema": "runtime.mcp_tool.result.v1",
            "providerKind": "mcp",
            "pluginName": plugin_name,
            "serverId": server_id,
            "sourceName": source_name,
            "protocolVersion": protocol_version,
            "result": projected,
        }),
        is_error,
        facts: Vec::new(),
        transition_reason: Some("mcp_tool_exec".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use centaeris_core::extension::mcp_model_contract_digest;
    use rmcp::{ServerHandler, ServiceExt};
    use serde_json::Map;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    #[derive(Debug, Clone)]
    struct TestMcpServer;

    #[rmcp::tool_router]
    impl TestMcpServer {
        #[rmcp::tool(description = "Search laws.")]
        fn search_laws(&self) -> String {
            "statute".to_string()
        }
    }

    #[rmcp::tool_handler]
    impl ServerHandler for TestMcpServer {}

    fn declared_tool() -> McpToolDeclarationV1 {
        let input_schema = Value::Object(
            TestMcpServer::tool_router().list_all()[0]
                .input_schema
                .as_ref()
                .clone(),
        );
        McpToolDeclarationV1 {
            source_name: "search_laws".to_string(),
            name: "banana_search".to_string(),
            description: "Search laws.".to_string(),
            input_schema,
            concurrency_safe: false,
            scopes: vec!["banana:read".to_string()],
        }
    }

    fn declaration(transport: McpTransportV1, lifecycle: McpLifecycleV1) -> McpServerDeclarationV1 {
        declaration_with_id("banana-source", transport, lifecycle)
    }

    fn declaration_with_id(
        server_id: &str,
        transport: McpTransportV1,
        lifecycle: McpLifecycleV1,
    ) -> McpServerDeclarationV1 {
        let tools = vec![declared_tool()];
        McpServerDeclarationV1 {
            id: server_id.to_string(),
            model_contract_digest: mcp_model_contract_digest(server_id, tools.as_slice())
                .expect("model contract digest"),
            transport,
            lifecycle,
            startup_timeout_ms: 10_000,
            tool_timeout_ms: 10_000,
            tools,
        }
    }

    async fn require_test_bearer(
        request: axum::extract::Request,
        next: axum::middleware::Next,
    ) -> axum::response::Response {
        if request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some("Bearer test-secret")
        {
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
        next.run(request).await
    }

    fn matching_tool() -> Tool {
        Tool::new(
            "search_laws",
            "Search laws.",
            declared_tool()
                .input_schema
                .as_object()
                .expect("object schema")
                .clone(),
        )
    }

    struct FakeProvider {
        provider_id: String,
        calls: AtomicUsize,
        disconnect_once: AtomicBool,
        business_error: bool,
    }

    impl DynamicToolProvider for FakeProvider {
        fn provider_id(&self) -> &str {
            self.provider_id.as_str()
        }

        fn execute<'a>(
            &'a self,
            _request: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.disconnect_once.swap(false, Ordering::SeqCst) {
                    return Err("mcp_connection_lost: fake transport closed".to_string());
                }
                if self.business_error {
                    return Err("MCP tool call failed".to_string());
                }
                Ok(DynamicToolProviderResponse {
                    content: "ok".to_string(),
                    details: json!({}),
                    is_error: false,
                    facts: Vec::new(),
                    transition_reason: Some("fake_mcp_call".to_string()),
                })
            })
        }
    }

    struct FakeConnector {
        connects: AtomicUsize,
        declared: Vec<McpToolDeclarationV1>,
        discoveries: StdMutex<VecDeque<Vec<Tool>>>,
        provider: DynamicProvider,
    }

    struct FlakyConnector {
        connects: AtomicUsize,
        discoveries: AtomicUsize,
        outcomes: StdMutex<VecDeque<Result<(), McpConnectError>>>,
        provider: DynamicProvider,
    }

    struct PendingOnceConnector {
        connects: AtomicUsize,
        provider: DynamicProvider,
    }

    impl McpServerConnector for PendingOnceConnector {
        fn connect<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicProvider, McpConnectError>> + Send + 'a>>
        {
            Box::pin(async move {
                if self.connects.fetch_add(1, Ordering::SeqCst) == 0 {
                    std::future::pending::<()>().await;
                }
                Ok(self.provider.clone())
            })
        }
    }

    impl McpServerConnector for FlakyConnector {
        fn connect<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicProvider, McpConnectError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.connects.fetch_add(1, Ordering::SeqCst);
                self.discoveries.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                self.outcomes
                    .lock()
                    .expect("fake outcomes")
                    .pop_front()
                    .unwrap_or(Ok(()))?;
                Ok(self.provider.clone())
            })
        }
    }

    impl McpServerConnector for FakeConnector {
        fn connect<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicProvider, McpConnectError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.connects.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                let discovered = self
                    .discoveries
                    .lock()
                    .expect("fake discoveries")
                    .pop_front()
                    .unwrap_or_else(|| vec![matching_tool()]);
                validate_live_tools(self.declared.as_slice(), discovered.as_slice())?;
                Ok(self.provider.clone())
            })
        }
    }

    fn fake_connector(
        provider_id: String,
        discoveries: Vec<Vec<Tool>>,
        disconnect_once: bool,
        business_error: bool,
    ) -> (Arc<FakeConnector>, Arc<FakeProvider>) {
        let provider = Arc::new(FakeProvider {
            provider_id,
            calls: AtomicUsize::new(0),
            disconnect_once: AtomicBool::new(disconnect_once),
            business_error,
        });
        let connector = Arc::new(FakeConnector {
            connects: AtomicUsize::new(0),
            declared: vec![declared_tool()],
            discoveries: StdMutex::new(discoveries.into()),
            provider: provider.clone(),
        });
        (connector, provider)
    }

    fn fake_request(contract: &DynamicToolContract, index: usize) -> DynamicToolProviderRequest {
        DynamicToolProviderRequest {
            tool_call_id: format!("call-{index}"),
            tool_name: contract.name.clone(),
            args_json: "{}".to_string(),
            contract: centaeris_core::tool::DynamicToolRegistry::from_contracts(vec![
                contract.clone()
            ])
            .expect("registry")
            .find_contract(contract.name.as_str())
            .expect("contract"),
            cancellation_probe: None,
        }
    }

    #[test]
    fn official_sdk_projects_declared_contract_and_call() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let (server_transport, client_transport) = tokio::io::duplex(4_096);
                let server_task = tokio::spawn(async move {
                    TestMcpServer
                        .serve(server_transport)
                        .await
                        .expect("serve")
                        .waiting()
                        .await
                        .expect("wait");
                });
                let declaration = declaration(
                    McpTransportV1::Stdio {
                        program: "bin/banana-mcp".to_string(),
                        args: Vec::new(),
                    },
                    McpLifecycleV1::Initialize,
                );
                let contracts = declaration
                    .dynamic_tool_contracts("legal")
                    .expect("static contracts");
                let provider = connect_mcp_server_transport("legal", declaration, client_transport)
                    .await
                    .expect("provider");
                assert_eq!(provider.provider_id(), "mcp:legal:banana-source");
                assert_eq!(contracts[0].name, "banana_search");
                assert!(!contracts[0].concurrency_safe);
                let response = provider
                    .execute(DynamicToolProviderRequest {
                        tool_call_id: "call-1".to_string(),
                        tool_name: "banana_search".to_string(),
                        args_json: "{}".to_string(),
                        contract: centaeris_core::tool::DynamicToolRegistry::from_contracts(
                            contracts.clone(),
                        )
                        .expect("registry")
                        .find_contract("banana_search")
                        .expect("contract"),
                        cancellation_probe: None,
                    })
                    .await
                    .expect("call");
                assert_eq!(response.content, "statute");
                drop(provider);
                server_task.abort();
                let _ = server_task.await;
            });
    }

    #[test]
    fn lazy_connect_cancellation_releases_singleflight_without_calling_the_tool() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let server = declaration(
                    McpTransportV1::Stdio {
                        program: "bin/banana-mcp".to_string(),
                        args: Vec::new(),
                    },
                    McpLifecycleV1::Initialize,
                );
                for cancel_waiter in [false, true] {
                    let (_, live) = fake_connector(
                        "mcp:legal:banana-source".to_string(),
                        Vec::new(),
                        false,
                        false,
                    );
                    let connector = Arc::new(PendingOnceConnector {
                        connects: AtomicUsize::new(0),
                        provider: live.clone(),
                    });
                    let binding = lazy_mcp_server_binding("legal", &server, connector.clone())
                        .expect("lazy binding");
                    let first = if cancel_waiter {
                        let provider = binding.provider.clone();
                        let request = fake_request(&binding.contracts[0], 0);
                        let first = tokio::spawn(async move { provider.execute(request).await });
                        tokio::time::timeout(Duration::from_secs(1), async {
                            while connector.connects.load(Ordering::SeqCst) == 0 {
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("first connection started");
                        Some(first)
                    } else {
                        None
                    };
                    let mut request = fake_request(&binding.contracts[0], 1);
                    let cancellation_connector = connector.clone();
                    request.cancellation_probe = Some(Arc::new(move || {
                        Ok((cancellation_connector.connects.load(Ordering::SeqCst) > 0)
                            .then(|| "user_cancelled".to_string()))
                    }));
                    let error = tokio::time::timeout(
                        Duration::from_secs(1),
                        binding.provider.execute(request),
                    )
                    .await
                    .expect("cancellation must not wait for startup timeout")
                    .expect_err("cancelled lazy connection");
                    assert_eq!(error, "dynamic tool execution cancelled: user_cancelled");
                    assert_eq!(connector.connects.load(Ordering::SeqCst), 1);
                    assert_eq!(live.calls.load(Ordering::SeqCst), 0);
                    if let Some(first) = first {
                        first.abort();
                        assert!(first.await.expect_err("aborted connection").is_cancelled());
                    }
                    tokio::time::timeout(
                        Duration::from_secs(1),
                        binding
                            .provider
                            .execute(fake_request(&binding.contracts[0], 2)),
                    )
                    .await
                    .expect("singleflight lock released")
                    .expect("later call can initialize");
                    assert_eq!(connector.connects.load(Ordering::SeqCst), 2);
                    assert_eq!(live.calls.load(Ordering::SeqCst), 1);
                }
            });
    }

    #[test]
    fn nine_lazy_servers_start_offline_and_first_concurrent_call_connects_once() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let mut bindings = Vec::new();
                let mut connectors = Vec::new();
                for index in 0..9 {
                    let server_id = format!("banana-{index}");
                    let server = declaration_with_id(
                        server_id.as_str(),
                        McpTransportV1::StreamableHttp {
                            url: format!("https://banana.invalid/{index}"),
                            bearer_credential_ref: Some("banana-token".to_string()),
                        },
                        McpLifecycleV1::Initialize,
                    );
                    let (connector, _) = fake_connector(
                        format!("mcp:legal:{server_id}"),
                        vec![vec![matching_tool()]],
                        false,
                        false,
                    );
                    bindings.push(
                        lazy_mcp_server_binding("legal", &server, connector.clone())
                            .expect("lazy binding"),
                    );
                    connectors.push(connector);
                }
                assert_eq!(
                    connectors
                        .iter()
                        .map(|connector| connector.connects.load(Ordering::SeqCst))
                        .sum::<usize>(),
                    0
                );

                let provider = bindings[0].provider.clone();
                let contract = bindings[0].contracts[0].clone();
                let mut tasks = Vec::new();
                for index in 0..8 {
                    let provider = provider.clone();
                    let request = fake_request(&contract, index);
                    tasks.push(tokio::spawn(async move { provider.execute(request).await }));
                }
                for task in tasks {
                    assert_eq!(task.await.expect("task").expect("call").content, "ok");
                }
                assert_eq!(connectors[0].connects.load(Ordering::SeqCst), 1);
                assert_eq!(
                    connectors
                        .iter()
                        .skip(1)
                        .map(|connector| connector.connects.load(Ordering::SeqCst))
                        .sum::<usize>(),
                    0
                );
            });
    }

    #[test]
    fn concurrent_retryable_failure_is_shared_until_cooldown_expires() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let server = declaration(
                    McpTransportV1::StreamableHttp {
                        url: "https://banana.invalid/mcp".to_string(),
                        bearer_credential_ref: None,
                    },
                    McpLifecycleV1::Initialize,
                );
                let provider: DynamicProvider = Arc::new(FakeProvider {
                    provider_id: "mcp:legal:banana-source".to_string(),
                    calls: AtomicUsize::new(0),
                    disconnect_once: AtomicBool::new(false),
                    business_error: false,
                });
                let connector = Arc::new(FlakyConnector {
                    connects: AtomicUsize::new(0),
                    discoveries: AtomicUsize::new(0),
                    outcomes: StdMutex::new(
                        vec![
                            Err(McpConnectError::Unavailable("temporary".to_string())),
                            Ok(()),
                        ]
                        .into(),
                    ),
                    provider,
                });
                let binding = lazy_mcp_server_binding("legal", &server, connector.clone())
                    .expect("lazy binding");
                let mut tasks = Vec::new();
                for index in 0..32 {
                    let provider = binding.provider.clone();
                    let request = fake_request(&binding.contracts[0], index);
                    tasks.push(tokio::spawn(async move { provider.execute(request).await }));
                }
                for task in tasks {
                    assert_eq!(
                        task.await.expect("task").expect_err("temporary failure"),
                        "mcp_connection_failed: temporary"
                    );
                }
                assert_eq!(connector.connects.load(Ordering::SeqCst), 1);
                assert_eq!(connector.discoveries.load(Ordering::SeqCst), 1);

                tokio::time::sleep(RETRYABLE_CONNECT_FAILURE_COOLDOWN + Duration::from_millis(25))
                    .await;
                assert_eq!(
                    binding
                        .provider
                        .execute(fake_request(&binding.contracts[0], 33))
                        .await
                        .expect("retry after cooldown")
                        .content,
                    "ok"
                );
                assert_eq!(connector.connects.load(Ordering::SeqCst), 2);
                assert_eq!(connector.discoveries.load(Ordering::SeqCst), 2);
            });
    }

    #[test]
    fn proven_disconnect_reconnects_and_revalidates_before_call() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let server = declaration(
                    McpTransportV1::StreamableHttp {
                        url: "https://banana.invalid/mcp".to_string(),
                        bearer_credential_ref: None,
                    },
                    McpLifecycleV1::Initialize,
                );
                let changed = Tool::new(
                    "search_laws",
                    "Changed live description.",
                    Map::from_iter([("type".to_string(), json!("object"))]),
                );
                let (connector, live) = fake_connector(
                    "mcp:legal:banana-source".to_string(),
                    vec![vec![matching_tool()], vec![changed]],
                    true,
                    false,
                );
                let binding = lazy_mcp_server_binding("legal", &server, connector.clone())
                    .expect("lazy binding");
                assert!(binding
                    .provider
                    .execute(fake_request(&binding.contracts[0], 1))
                    .await
                    .expect_err("disconnect")
                    .starts_with("mcp_connection_lost:"));
                assert!(binding
                    .provider
                    .execute(fake_request(&binding.contracts[0], 2))
                    .await
                    .expect_err("contract mismatch")
                    .starts_with("mcp_model_contract_mismatch:"));
                assert_eq!(connector.connects.load(Ordering::SeqCst), 2);
                assert_eq!(live.calls.load(Ordering::SeqCst), 1);

                assert!(binding
                    .provider
                    .execute(fake_request(&binding.contracts[0], 3))
                    .await
                    .is_err());
                assert_eq!(connector.connects.load(Ordering::SeqCst), 2);
            });
    }

    #[test]
    fn business_error_does_not_trigger_rediscovery() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                let server = declaration(
                    McpTransportV1::StreamableHttp {
                        url: "https://banana.invalid/mcp".to_string(),
                        bearer_credential_ref: None,
                    },
                    McpLifecycleV1::Initialize,
                );
                let (connector, live) = fake_connector(
                    "mcp:legal:banana-source".to_string(),
                    vec![vec![matching_tool()]],
                    false,
                    true,
                );
                let binding = lazy_mcp_server_binding("legal", &server, connector.clone())
                    .expect("lazy binding");
                for index in 0..2 {
                    assert!(binding
                        .provider
                        .execute(fake_request(&binding.contracts[0], index))
                        .await
                        .is_err());
                }
                assert_eq!(connector.connects.load(Ordering::SeqCst), 1);
                assert_eq!(live.calls.load(Ordering::SeqCst), 2);
            });
    }

    #[test]
    fn discovery_must_match_frozen_model_contract() {
        let declaration = declared_tool();
        let matching = matching_tool();
        let extra = Tool::new(
            "extra_live_tool",
            "Ignored.",
            Map::from_iter([("type".to_string(), json!("object"))]),
        );
        assert!(matches!(
            validate_live_tools(std::slice::from_ref(&declaration), &[matching.clone(), extra]),
            Err(McpConnectError::ContractMismatch(message)) if message.contains("unknown")
        ));
        assert_eq!(
            validate_live_tools(
                std::slice::from_ref(&declaration),
                std::slice::from_ref(&matching),
            )
            .expect("exact live contract")["banana_search"],
            "search_laws"
        );

        let padded = Tool::new(
            "search_laws",
            "  Search laws. \n",
            declared_tool()
                .input_schema
                .as_object()
                .expect("object schema")
                .clone(),
        );
        let mut padded_declaration = declaration.clone();
        padded_declaration.description = "  Search laws. \n".to_string();
        assert!(validate_live_tools(&[padded_declaration], std::slice::from_ref(&padded),).is_ok());
        assert!(matches!(
            validate_live_tools(std::slice::from_ref(&declaration), &[padded]),
            Err(McpConnectError::ContractMismatch(message)) if message.contains("description differs")
        ));

        let mut changed_schema_value = declared_tool().input_schema;
        changed_schema_value["banana"] = json!(true);
        let changed_schema = Tool::new(
            "search_laws",
            "Search laws.",
            changed_schema_value
                .as_object()
                .expect("changed object schema")
                .clone(),
        );
        assert!(matches!(
            validate_live_tools(std::slice::from_ref(&declaration), &[changed_schema]),
            Err(McpConnectError::ContractMismatch(message)) if message.contains("inputSchema differs")
        ));
        assert!(matches!(
            validate_live_tools(std::slice::from_ref(&declaration), &[]),
            Err(McpConnectError::ContractMismatch(message)) if message.contains("missing")
        ));
        assert!(matches!(
            validate_live_tools(&[declaration], &[matching.clone(), matching]),
            Err(McpConnectError::ContractMismatch(message)) if message.contains("duplicate")
        ));
    }

    #[tokio::test]
    async fn discovery_rejects_unbounded_pagination_and_aggregate_bytes() {
        #[derive(Clone)]
        struct Pages {
            mode: &'static str,
            calls: Arc<AtomicUsize>,
        }
        impl ServerHandler for Pages {
            async fn list_tools(
                &self,
                _: Option<rmcp::model::PaginatedRequestParams>,
                _: rmcp::service::RequestContext<rmcp::RoleServer>,
            ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
                let index = self.calls.fetch_add(1, Ordering::SeqCst);
                let mut page = rmcp::model::ListToolsResult::default();
                let mut tool = matching_tool();
                tool.name = if self.mode == "duplicate" {
                    "same".into()
                } else {
                    format!("tool_{index}").into()
                };
                if self.mode == "bytes" {
                    tool.description = Some("x".repeat(2 * 1024 * 1024).into());
                }
                if self.mode != "empty" {
                    page.tools.push(tool);
                }
                page.next_cursor = Some(if self.mode == "cursor" {
                    "same".to_string()
                } else {
                    index.to_string()
                });
                Ok(page)
            }
        }
        for (mode, expected_calls, expected_error) in [
            ("empty", 1, "no progress"),
            ("cursor", 2, "repeated"),
            ("duplicate", 2, "repeated"),
            ("pages", MAX_DISCOVERY_PAGES, "256 pages"),
            ("bytes", 2, "bytes"),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let (server_io, client_io) = tokio::io::duplex(8192);
            let server = Pages {
                mode,
                calls: calls.clone(),
            };
            let task = tokio::spawn(async move {
                let service = server.serve(server_io).await.expect("serve");
                let _ = service.waiting().await;
            });
            let client = ().serve(client_io).await.expect("client");
            let error = discover_tools(&client)
                .await
                .expect_err("bounded discovery");
            assert!(
                matches!(&error, McpConnectError::ContractMismatch(message) if message.contains(expected_error)),
                "{mode}: {error}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), expected_calls, "{mode}");
            drop(client);
            task.abort();

            let connector = Arc::new(FlakyConnector {
                connects: AtomicUsize::new(0),
                discoveries: AtomicUsize::new(0),
                outcomes: StdMutex::new(vec![Err(error.clone())].into()),
                provider: Arc::new(FakeProvider {
                    provider_id: "mcp:legal:banana-source".to_string(),
                    calls: AtomicUsize::new(0),
                    disconnect_once: AtomicBool::new(false),
                    business_error: false,
                }),
            });
            let server = declaration(
                McpTransportV1::Stdio {
                    program: "bin/fixture".to_string(),
                    args: Vec::new(),
                },
                McpLifecycleV1::Initialize,
            );
            let binding =
                lazy_mcp_server_binding("legal", &server, connector.clone()).expect("lazy binding");
            for index in 0..2 {
                assert_eq!(
                    binding
                        .provider
                        .execute(fake_request(&binding.contracts[0], index))
                        .await
                        .expect_err("sticky malformed discovery"),
                    error.to_string()
                );
                if index == 0 {
                    tokio::time::sleep(
                        RETRYABLE_CONNECT_FAILURE_COOLDOWN + Duration::from_millis(25),
                    )
                    .await;
                }
            }
            assert_eq!(
                connector.connects.load(Ordering::SeqCst),
                1,
                "{mode}: contract failures must remain sticky after cooldown"
            );
        }
    }

    #[test]
    fn official_sdk_streamable_http_uses_bearer_and_rejects_invalid_responses() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async {
                use rmcp::transport::streamable_http_server::{
                    session::local::LocalSessionManager, StreamableHttpServerConfig,
                    StreamableHttpService,
                };

                let service: StreamableHttpService<TestMcpServer, LocalSessionManager> =
                    StreamableHttpService::new(
                        || Ok(TestMcpServer),
                        Default::default(),
                        StreamableHttpServerConfig::default().with_json_response(true),
                    );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("listener");
                let address = listener.local_addr().expect("listener address");
                let router = axum::Router::new()
                    .merge(
                        axum::Router::new()
                            .nest_service("/mcp", service)
                            .layer(axum::middleware::from_fn(require_test_bearer)),
                    )
                    .route(
                        "/unauthorized",
                        axum::routing::any(|| async { axum::http::StatusCode::UNAUTHORIZED }),
                    )
                    .route(
                        "/forbidden",
                        axum::routing::any(|| async { axum::http::StatusCode::FORBIDDEN }),
                    )
                    .route(
                        "/redirect",
                        axum::routing::any(|| async {
                            axum::response::Redirect::temporary("/mcp")
                        }),
                    );
                let server_task = tokio::spawn(async move {
                    axum::serve(listener, router).await.expect("serve");
                });
                let declaration = |credential_ref: Option<&str>| {
                    declaration(
                        McpTransportV1::StreamableHttp {
                            url: "https://banana.invalid/mcp".to_string(),
                            bearer_credential_ref: credential_ref.map(str::to_string),
                        },
                        McpLifecycleV1::Auto,
                    )
                };
                let declared = declaration(Some("test-token"));
                let contracts = declared
                    .dynamic_tool_contracts("legal")
                    .expect("static contracts");
                let provider = connect_mcp_server_transport(
                    "legal",
                    declared,
                    bounded_http_transport(
                        format!("http://{address}/mcp").as_str(),
                        Some("test-secret"),
                    )
                    .expect("bounded transport"),
                )
                .await
                .expect("HTTP provider");
                assert_eq!(contracts[0].name, "banana_search");
                assert_eq!(
                    provider
                        .execute(DynamicToolProviderRequest {
                            tool_call_id: "call-1".to_string(),
                            tool_name: "banana_search".to_string(),
                            args_json: "{}".to_string(),
                            contract: centaeris_core::tool::DynamicToolRegistry::from_contracts(
                                contracts.clone(),
                            )
                            .expect("registry")
                            .find_contract("banana_search")
                            .expect("contract"),
                            cancellation_probe: None,
                        })
                        .await
                        .expect("HTTP call")
                        .content,
                    "statute"
                );
                drop(provider);

                for path in ["unauthorized", "forbidden", "redirect"] {
                    let transport =
                        bounded_http_transport(format!("http://{address}/{path}").as_str(), None)
                            .expect("bounded transport");
                    assert!(
                        connect_mcp_server_transport("legal", declaration(None), transport)
                            .await
                            .is_err(),
                        "accepted {path} response"
                    );
                }
                server_task.abort();
                let _ = server_task.await;
            });
    }

    #[test]
    fn projects_text_and_rejects_binary_content() {
        let projected = project_result(
            "legal",
            "source",
            "banana-search",
            "2025-11-25",
            CallToolResult::success(vec![ContentBlock::text("result")]),
        )
        .expect("text result");
        assert_eq!(projected.content, "result");
        assert!(!projected.is_error);

        let projected_error = project_result(
            "legal",
            "source",
            "banana-search",
            "2025-11-25",
            CallToolResult::error(vec![ContentBlock::text("not found")]),
        )
        .expect("error result");
        assert_eq!(projected_error.content, "not found");
        assert!(projected_error.is_error);
        assert!(project_result(
            "legal",
            "source",
            "banana-search",
            "2025-11-25",
            CallToolResult::success(vec![ContentBlock::image("banana", "image/png")]),
        )
        .is_err());
    }

    #[test]
    fn bearer_binding_is_exact_and_secret_safe() {
        let mut server = declaration(
            McpTransportV1::StreamableHttp {
                url: "https://banana.invalid/mcp".to_string(),
                bearer_credential_ref: Some("legal-token".to_string()),
            },
            McpLifecycleV1::Auto,
        );
        server.startup_timeout_ms = 1;
        server.tool_timeout_ms = 1;
        let error = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(connect_streamable_http_mcp_server("legal", server, None))
            .err()
            .expect("missing credential rejected");
        assert_eq!(
            error.to_string(),
            "mcp_connection_failed: MCP bearer credential binding does not match declaration"
        );
        assert!(valid_bearer_token("banana"));
        assert!(!valid_bearer_token("banana token"));
        assert!(supported_protocol_version("2025-06-18"));
        assert!(!supported_protocol_version("banana"));
    }
}
