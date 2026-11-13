//! Plugin-independent MCP HTTP connection for host-owned service registrations.

use super::*;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use rmcp::model::{CallToolRequest, ClientRequest, ServerResult};

#[derive(Clone)]
pub(crate) struct ScopedHttpRequest {
    pub(crate) headers: HeaderMap,
    pub(crate) deadline: tokio::time::Instant,
    pub(crate) invocation_finished: tokio::sync::watch::Receiver<()>,
}

fn host_headers(headers: &[(&str, &str)]) -> Result<HeaderMap, McpConnectError> {
    let invalid = || McpConnectError::Unavailable("invalid MCP host header".into());
    let mut configured = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
        if !name.as_str().starts_with("x-") || configured.contains_key(&name) {
            return Err(invalid());
        }
        let mut value = HeaderValue::from_str(value).map_err(|_| invalid())?;
        value.set_sensitive(true);
        configured.insert(name, value);
    }
    Ok(configured)
}

pub struct HttpMcpClient {
    client: RunningService<RoleClient, super::DirectoryClient>,
    protocol_version: String,
    tools: Vec<Tool>,
}

impl HttpMcpClient {
    /// Headers are trusted host configuration shared by requests on this connection.
    pub async fn connect(
        url: &str,
        bearer_token: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<Self, McpConnectError> {
        if !valid_bearer_token(bearer_token) {
            return Err(McpConnectError::Unavailable(
                "invalid MCP bearer credential".into(),
            ));
        }
        let http = reqwest::Client::builder()
            .user_agent(PRODUCT_USER_AGENT)
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(host_headers(headers)?)
            .connect_timeout(timeout)
            .build()
            .map_err(|_| McpConnectError::Unavailable("build MCP HTTP client failed".into()))?;
        let transport = StreamableHttpClientTransport::with_client(
            transport::BoundedHttpClient(http),
            streamable_http_config(url, Some(bearer_token)),
        );
        let (client, protocol_version, tools) =
            initialize_client_with_timeout(transport, timeout, McpLifecycleV1::Auto, "host.http")
                .await?;
        Ok(Self {
            client,
            protocol_version,
            tools,
        })
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    /// No transparent retry: a lost response does not prove non-execution.
    pub async fn call(
        &self,
        name: &str,
        arguments: serde_json::Map<String, Value>,
        timeout: Duration,
    ) -> Result<DynamicToolProviderResponse, String> {
        self.call_request(name, arguments, None, timeout).await
    }

    /// Bind trusted HTTP credentials to this message, including when calls overlap.
    pub async fn call_scoped(
        &self,
        name: &str,
        arguments: serde_json::Map<String, Value>,
        bearer_token: &str,
        headers: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<DynamicToolProviderResponse, String> {
        if !valid_bearer_token(bearer_token) {
            return Err("invalid MCP bearer credential".into());
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {bearer_token}"))
            .map_err(|_| "invalid MCP bearer credential")?;
        authorization.set_sensitive(true);
        let mut headers = host_headers(headers).map_err(|_| "invalid MCP host header")?;
        headers.insert(AUTHORIZATION, authorization);
        let (_invocation, invocation_finished) = tokio::sync::watch::channel(());
        self.call_request(
            name,
            arguments,
            Some(ScopedHttpRequest {
                headers,
                deadline: tokio::time::Instant::now() + timeout,
                invocation_finished,
            }),
            timeout,
        )
        .await
    }

    async fn call_request(
        &self,
        name: &str,
        arguments: serde_json::Map<String, Value>,
        scope: Option<ScopedHttpRequest>,
        timeout: Duration,
    ) -> Result<DynamicToolProviderResponse, String> {
        if !self.tools.iter().any(|tool| tool.name == name) {
            return Err("MCP tool is not discovered".into());
        }
        let mut request = CallToolRequest::new(
            CallToolRequestParams::new(name.to_string()).with_arguments(arguments),
        );
        if let Some(scope) = scope {
            request.extensions.insert(scope);
        }
        let response = tokio::time::timeout(
            timeout,
            self.client
                .send_request(ClientRequest::CallToolRequest(request)),
        )
        .await
        .map_err(|_| "MCP tool call timed out")?
        .map_err(|_| "MCP tool call failed")?;
        let result = match response {
            ServerResult::CallToolResult(result) => result,
            ServerResult::InputRequiredResult(_) | ServerResult::CreateTaskResult(_) => {
                return Err("MCP non-complete result is not supported".into());
            }
            _ => return Err("MCP tool call failed".into()),
        };
        let (content, projected, is_error) = project_content(result)?;
        Ok(DynamicToolProviderResponse {
            content,
            details: json!({"schema": "runtime.mcp_http.result.v1", "providerKind": "mcp",
                "sourceName": name, "protocolVersion": self.protocol_version, "result": projected}),
            is_error,
            facts: Vec::new(),
            transition_reason: Some("mcp_tool_exec".into()),
        })
    }
}
