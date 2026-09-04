use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::execution::ExecutionCancellationProbe;
use crate::tool::{ToolContract, ToolErrorInfo, ToolFailureKind};

use super::ToolExecutionFact;

#[derive(Clone)]
pub struct DynamicToolProviderRequest {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args_json: String,
    pub contract: ToolContract,
    pub cancellation_probe: Option<Arc<ExecutionCancellationProbe>>,
}

impl DynamicToolProviderRequest {
    pub async fn wait_for_cancellation(&self) -> Result<String, String> {
        let Some(probe) = self.cancellation_probe.as_deref() else {
            return std::future::pending().await;
        };
        loop {
            if let Some(reason) = probe()? {
                return Ok(reason);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

#[derive(Debug, Clone)]
pub struct DynamicToolProviderResponse {
    pub content: String,
    pub details: Value,
    pub is_error: bool,
    pub facts: Vec<ToolExecutionFact>,
    pub transition_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicToolPollingSpec {
    pub poll_key: String,
    #[serde(default = "default_dynamic_tool_poll_args")]
    pub poll_args: Value,
    #[serde(default)]
    pub next_poll_at_ms: Option<i64>,
    #[serde(default)]
    pub lease_ms: Option<u64>,
    #[serde(default)]
    pub max_poll_attempts: Option<u32>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DynamicToolPendingPoll {
    pub provider_id: String,
    pub tool_name: String,
    pub schema_hash: Option<String>,
    pub spec: DynamicToolPollingSpec,
}

pub trait DynamicToolProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn execute<'a>(
        &'a self,
        req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>;

    fn execute_with_error_info<'a>(
        &'a self,
        req: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, ToolErrorInfo>> + Send + 'a>>
    {
        Box::pin(async move {
            self.execute(req).await.map_err(|message| {
                let diagnostic_id = ToolErrorInfo::from_unstructured_error(message).diagnostic_id;
                let mut error = ToolErrorInfo::new(
                    ToolFailureKind::ProviderError,
                    "dynamic tool provider returned an error",
                    "Dynamic tool execution failed",
                );
                error.diagnostic_id = diagnostic_id;
                error
            })
        })
    }
}

pub fn extract_dynamic_tool_pending_poll(
    details: &Value,
) -> Option<Result<DynamicToolPendingPoll, String>> {
    if !details
        .get("dynamicTool")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let provider_id = details
        .get("providerId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let tool_name = details
        .get("toolName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let schema_hash = details
        .get("schemaHash")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let result = details.get("result")?;
    let polling_value = result.get("providerPolling")?;
    let status = polling_value
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !status.eq_ignore_ascii_case("pending") {
        return None;
    }
    let provider_id = match provider_id {
        Some(value) => value,
        None => {
            return Some(Err(
                "dynamic tool pending poll missing providerId".to_string()
            ))
        }
    };
    let tool_name = match tool_name {
        Some(value) => value,
        None => return Some(Err("dynamic tool pending poll missing toolName".to_string())),
    };
    let mut spec_value = polling_value.clone();
    if let Some(spec) = spec_value.as_object_mut() {
        spec.remove("status");
    }
    let spec = match serde_json::from_value::<DynamicToolPollingSpec>(spec_value) {
        Ok(spec) => spec,
        Err(err) => {
            return Some(Err(format!(
                "decode dynamic tool pending poll spec failed: {err}"
            )))
        }
    };
    if spec.poll_key.trim().is_empty() {
        return Some(Err(
            "dynamic tool pending poll pollKey is required".to_string()
        ));
    }
    Some(Ok(DynamicToolPendingPoll {
        provider_id,
        tool_name,
        schema_hash,
        spec,
    }))
}

fn default_dynamic_tool_poll_args() -> Value {
    Value::Object(Map::new())
}

pub(super) fn wrap_dynamic_tool_output(contract: &ToolContract, result: Value) -> Value {
    json!({
        "dynamicTool": true,
        "toolName": contract.name,
        "providerId": contract.provider_id,
        "schemaHash": contract.schema_hash,
        "scopes": contract.scopes,
        "result": result,
    })
}
