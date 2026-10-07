//! First-party platform identity and credentials; MCP owns protocol semantics.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
};
use centaeris_core::tool::{DynamicToolContract, DynamicToolRegistry, ToolTurnBehavior};
use centaeris_mcp::HttpMcpClient;
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) const PROVIDER_ID: &str = "workspace.materials";
const TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct PlatformMaterialsProvider {
    api_url: String,
    internal_token: String,
    credential_request: Value,
    http: reqwest::Client,
    pub(crate) contracts: Vec<DynamicToolContract>,
    discovered: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Credential {
    schema: String,
    access_token: String,
    expires_at: u64,
}

impl PlatformMaterialsProvider {
    pub(crate) async fn connect_current(
        api_url: String,
        internal_token: String,
        agent_run_id: String,
        authorization_digest: String,
    ) -> Result<Self, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Processor {
            schema: String,
            processing_specification: Value,
            spec_digest: String,
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .build()
            .map_err(|_| "platform processor client failed")?;
        let mut response = client
            .post(format!(
                "{}/internal/materials/processor",
                api_url.trim_end_matches('/')
            ))
            .header("X-Internal-Token", &internal_token)
            .json(&json!({"schema":"workspace.material.processor.v1"}))
            .send()
            .await
            .map_err(|_| "platform processor unavailable")?
            .error_for_status()
            .map_err(|_| "platform processor unavailable")?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "platform processor response failed")?
        {
            if bytes.len().saturating_add(chunk.len()) > 16 * 1024 {
                return Err("platform processor response oversized".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let processor: Processor =
            serde_json::from_slice(&bytes).map_err(|_| "platform processor response invalid")?;
        if processor.schema != "workspace.material.processor.result.v1" {
            return Err("platform processor response invalid".into());
        }
        Self::connect(
            api_url,
            internal_token,
            agent_run_id,
            authorization_digest,
            processor.processing_specification,
            processor.spec_digest,
        )
        .await
    }

    pub(crate) async fn connect(
        api_url: String,
        internal_token: String,
        agent_run_id: String,
        authorization_digest: String,
        specification: Value,
        spec_digest: String,
    ) -> Result<Self, String> {
        let mut provider = Self {
            api_url,
            internal_token,
            credential_request: json!({"schema":"workspace.mcp.credential.issue.v1", "agentRunId":agent_run_id,
                "authorizationDigest":authorization_digest,"processingSpecification":specification,"specDigest":spec_digest}),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(TIMEOUT)
                .build()
                .map_err(|_| "build platform credential client failed")?,
            contracts: Vec::new(),
            discovered: Value::Null,
        };
        let client = provider.client(None).await?;
        validate_tool_names(
            &client
                .tools()
                .iter()
                .map(|tool| tool.name.to_string())
                .collect::<Vec<_>>(),
        )?;
        provider.contracts = client
            .tools()
            .iter()
            .map(|tool| DynamicToolContract {
                name: tool.name.to_string(),
                category: "platform.materials".into(),
                summary: tool
                    .description
                    .as_deref()
                    .unwrap_or(tool.name.as_ref())
                    .to_string(),
                input_schema: Value::Object(tool.input_schema.as_ref().clone()),
                provider_id: PROVIDER_ID.into(),
                scopes: Vec::new(),
                concurrency_safe: false,
                turn_behavior: ToolTurnBehavior::ContinueTurn,
            })
            .collect();
        DynamicToolRegistry::from_contracts(provider.contracts.clone())?;
        provider.discovered =
            serde_json::to_value(client.tools()).map_err(|_| "invalid material catalog")?;
        Ok(provider)
    }

    async fn client(&self, call_id: Option<&str>) -> Result<HttpMcpClient, String> {
        // A new scoped credential for each invocation avoids caching a permission
        // decision or reusing an expired bearer across a long-running AgentRun.
        let mut response = self
            .http
            .post(format!(
                "{}/internal/mcp/credential",
                self.api_url.trim_end_matches('/')
            ))
            .header("X-Internal-Token", &self.internal_token)
            .json(&self.credential_request)
            .send()
            .await
            .map_err(|_| "platform credential request failed")?;
        if !response.status().is_success() {
            return Err("platform credential rejected".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "platform credential response failed")?
        {
            if bytes.len() + chunk.len() > 16 * 1024 {
                return Err("platform credential response too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let credential: Credential =
            serde_json::from_slice(&bytes).map_err(|_| "platform credential response invalid")?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_secs();
        if credential.schema != "workspace.mcp.credential.result.v1"
            || credential.expires_at <= now
            || credential.expires_at > now + 300
        {
            return Err("platform credential response invalid".into());
        }
        let headers = call_id
            .map(|id| vec![("x-workspace-tool-call-id", id)])
            .unwrap_or_default();
        HttpMcpClient::connect(
            &format!("{}/internal/mcp", self.api_url.trim_end_matches('/')),
            &credential.access_token,
            &headers,
            TIMEOUT,
        )
        .await
        .map_err(|_| "platform MCP connection failed".into())
    }

    async fn invoke(
        &self,
        request: &DynamicToolProviderRequest,
    ) -> Result<DynamicToolProviderResponse, String> {
        if request.contract.provider_id.as_deref() != Some(PROVIDER_ID)
            || !self
                .contracts
                .iter()
                .any(|contract| contract.name == request.tool_name)
            || request.tool_call_id.is_empty()
            || request.tool_call_id.len() > 160
            || !request.tool_call_id.is_ascii()
        {
            return Err("platform material invocation binding invalid".into());
        }
        let client = self.client(Some(&request.tool_call_id)).await?;
        if serde_json::to_value(client.tools()).map_err(|_| "invalid material catalog")?
            != self.discovered
        {
            return Err("platform material catalog changed during run".into());
        }
        let arguments = serde_json::from_str::<Value>(&request.args_json)
            .map_err(|_| "invalid material arguments")?
            .as_object()
            .cloned()
            .ok_or("invalid material arguments")?;
        let result = client.call(&request.tool_name, arguments, TIMEOUT).await?;
        // Do not acknowledge a successful citable result that Core would clip.
        if result.content.len() > centaeris_core::tool::layer::MODEL_TOOL_RESULT_MAX_BYTES {
            return Err("material result exceeds model budget; retry with a smaller limit".into());
        }
        Ok(result)
    }
}

impl DynamicToolProvider for PlatformMaterialsProvider {
    fn provider_id(&self) -> &str {
        PROVIDER_ID
    }
    fn execute<'a>(
        &'a self,
        request: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = request.wait_for_cancellation() => Err("platform material call cancelled".into()),
                result = tokio::time::timeout(TIMEOUT, self.invoke(&request)) =>
                    result.map_err(|_| "platform material call timed out".to_string())?,
            }
        })
    }
}

fn validate_tool_names(names: &[String]) -> Result<(), String> {
    let expected = BTreeSet::from([
        "list_materials",
        "read_material",
        "search_materials",
        "read_material_result",
        "get_operation",
        "cancel_operation",
    ]);
    if names.len() != expected.len()
        || names.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected
    {
        return Err("platform material catalog invalid".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "scripts/platform-mcp-client-gate.py owns the real API and isolated PostgreSQL fixture"]
    fn python_http_interoperability() {
        use centaeris_core::model::ToolCallEnvelope;
        use centaeris_core::runtime::{canonical_tool_call_record, canonical_tool_result_record};
        use centaeris_core::tool::layer::ToolInvocationRequest;
        use std::sync::Arc;

        let fixture: Value = serde_json::from_str(
            &std::env::var("PLATFORM_MCP_CLIENT_FIXTURE").expect("client gate fixture"),
        )
        .unwrap();
        let text = |key: &str| fixture[key].as_str().unwrap().to_string();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let provider = runtime
            .block_on(PlatformMaterialsProvider::connect(
                text("apiUrl"),
                text("token"),
                text("runId"),
                text("authorizationDigest"),
                fixture["specification"].clone(),
                text("specDigest"),
            ))
            .expect("first-party discovery");
        let registry = DynamicToolRegistry::from_contracts(provider.contracts.clone()).unwrap();
        let mut layer = crate::tests::hosted_context()
            .tool_layer
            .with_dynamic_tool_registry(Arc::new(registry));
        layer
            .register_dynamic_tool_provider(Arc::new(provider))
            .unwrap();
        let db = &fixture["database"];
        let mut config = postgres::Config::new();
        config
            .host(db["HOST"].as_str().unwrap())
            .port(db["PORT"].as_str().unwrap().parse().unwrap())
            .dbname(db["NAME"].as_str().unwrap())
            .user(db["USER"].as_str().unwrap())
            .password(db["PASSWORD"].as_str().unwrap());
        assert!(db["NAME"]
            .as_str()
            .unwrap()
            .starts_with("test_platform_mcp_"));
        let mut database = config.connect(postgres::NoTls).unwrap();
        // Gate-only persistence of Core-generated records. Production keeps its
        // existing atomic Session store; this is not a lease-fencing acceptance test.
        let mut append = |record: centaeris_core::session::SessionLogRecord, sequence: i32| {
            let event = serde_json::to_value(record).unwrap();
            database.execute("INSERT INTO app_core_sessionevent (\"eventId\",workspace_id,session_id,agent_run_id,sequence,agent_run_sequence,projects_to_agent_run_stream,payload,\"createdAtMs\",\"insertedAt\") VALUES ($1,$2,$3,$4,$5,$5,true,$6::text::jsonb,1,now())",
                &[&event["eventId"].as_str().unwrap(), &text("workspaceId"), &text("sessionId"), &text("runId"), &sequence, &event.to_string()]).unwrap();
        };
        let mut next =
            json!({"tool":"read_material", "arguments":{"input_ref":text("inputRef"),"limit":10}});
        let mut collected = String::new();
        for index in 0..100 {
            let reject = next.is_null();
            if reject {
                next = json!({"tool":"read_material", "arguments":{"input_ref":"not-authorized"}});
            }
            let call = ToolCallEnvelope {
                id: format!("material-client-{index}"),
                name: next["tool"].as_str().unwrap().into(),
                args_json: next["arguments"].to_string(),
            };
            append(
                canonical_tool_call_record(
                    &text("sessionId"),
                    &text("turnId"),
                    &text("runId"),
                    &call,
                    PROVIDER_ID,
                    &format!("sha256:{}", "a".repeat(64)),
                    "material",
                    1,
                )
                .unwrap(),
                index * 2 + 1,
            );
            let result = runtime.block_on(layer.execute_async(ToolInvocationRequest {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                args_json: call.args_json.clone(),
            }));
            assert_eq!(
                result.status,
                if reject { "error" } else { "ok" },
                "{}",
                result.content
            );
            if !reject {
                assert!(
                    result.content.len()
                        <= centaeris_core::tool::layer::MODEL_TOOL_RESULT_MAX_BYTES
                );
                let page: Value = serde_json::from_str(&result.content).unwrap();
                assert!(page["message"].as_str().unwrap().contains("Showing"));
                collected.push_str(page["items"][0]["content"].as_str().unwrap());
                next = page["continuation"].clone();
            }
            assert!(result.facts.is_empty());
            append(
                canonical_tool_result_record(
                    &text("sessionId"),
                    &text("turnId"),
                    &text("runId"),
                    &call,
                    &result,
                    2,
                )
                .unwrap(),
                index * 2 + 2,
            );
            if reject {
                use sha2::{Digest, Sha256};
                assert_eq!(
                    format!("sha256:{:x}", Sha256::digest(collected.as_bytes())),
                    text("expectedTextSha256")
                );
                assert!(index > 1, "large fixture must require continuation");
                return;
            }
        }
        panic!("material continuation did not terminate");
    }

    #[test]
    fn material_registry_requires_the_complete_first_party_surface() {
        assert!(validate_tool_names(&["read_material".to_string()]).is_err());
        let names = [
            "list_materials",
            "read_material",
            "search_materials",
            "read_material_result",
            "get_operation",
            "cancel_operation",
        ]
        .map(str::to_string);
        assert!(validate_tool_names(&names).is_ok());
        let mut foreign = names.to_vec();
        foreign.push("external_tool".to_string());
        assert!(validate_tool_names(&foreign).is_err());
    }
}
