//! Plugin-independent MCP HTTP connection for host-owned service registrations.

use super::*;

pub struct HttpMcpClient {
    client: RunningService<RoleClient, ()>,
    protocol_version: String,
    tools: Vec<Tool>,
}

impl HttpMcpClient {
    /// Headers are trusted host configuration, never model-controlled arguments.
    /// A connection owns its headers; concurrent invocations should use separate
    /// connections when they require different call-scoped headers.
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
        let mut configured = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| McpConnectError::Unavailable("invalid MCP host header".into()))?;
            if !name.as_str().starts_with("x-") || configured.contains_key(&name) {
                return Err(McpConnectError::Unavailable(
                    "invalid MCP host header".into(),
                ));
            }
            let mut value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| McpConnectError::Unavailable("invalid MCP host header".into()))?;
            value.set_sensitive(true);
            configured.insert(name, value);
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(configured)
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
        if !self.tools.iter().any(|tool| tool.name == name) {
            return Err("MCP tool is not discovered".into());
        }
        let response = tokio::time::timeout(
            timeout,
            self.client.call_tool_once(
                CallToolRequestParams::new(name.to_string()).with_arguments(arguments),
            ),
        )
        .await
        .map_err(|_| "MCP tool call timed out")?
        .map_err(|_| "MCP tool call failed")?;
        let CallToolResponse::Complete(result) = response else {
            return Err("MCP non-complete result is not supported".into());
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
