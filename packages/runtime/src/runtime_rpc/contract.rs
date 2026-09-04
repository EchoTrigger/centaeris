use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSON_RPC_VERSION: &str = "2.0";

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRpcMethod {
    SessionPrompt,
    CentaerisSessionSupplement,
    CentaerisSessionAnswerNow,
    CentaerisSessionAnswerQuestion,
    CentaerisSessionSubscribeUpdates,
    CentaerisRuntimeClaimJobs,
    CentaerisRuntimeReadBoundedResult,
}

#[cfg(test)]
impl RuntimeRpcMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SessionPrompt => "session/prompt",
            Self::CentaerisSessionSupplement => "_centaeris/session/supplement",
            Self::CentaerisSessionAnswerNow => "_centaeris/session/answer_now",
            Self::CentaerisSessionAnswerQuestion => "_centaeris/session/answer_question",
            Self::CentaerisSessionSubscribeUpdates => "_centaeris/session/subscribe_updates",
            Self::CentaerisRuntimeClaimJobs => "_centaeris/runtime/claim_jobs",
            Self::CentaerisRuntimeReadBoundedResult => "_centaeris/runtime/read_bounded_result",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "session/prompt" => Some(Self::SessionPrompt),
            "_centaeris/session/supplement" => Some(Self::CentaerisSessionSupplement),
            "_centaeris/session/answer_now" => Some(Self::CentaerisSessionAnswerNow),
            "_centaeris/session/answer_question" => Some(Self::CentaerisSessionAnswerQuestion),
            "_centaeris/session/subscribe_updates" => Some(Self::CentaerisSessionSubscribeUpdates),
            "_centaeris/runtime/claim_jobs" => Some(Self::CentaerisRuntimeClaimJobs),
            "_centaeris/runtime/read_bounded_result" => {
                Some(Self::CentaerisRuntimeReadBoundedResult)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum RuntimeRpcId {
    String(String),
    Number(i64),
    Null,
}

impl RuntimeRpcId {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::String(value) if value.trim().is_empty() => {
                Err("runtime_rpc_request_id_must_not_be_empty".to_string())
            }
            Self::String(_) | Self::Number(_) | Self::Null => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRpcRequest {
    pub jsonrpc: String,
    pub id: RuntimeRpcId,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RuntimeRpcRequest {
    #[cfg(test)]
    pub fn new(id: RuntimeRpcId, method: RuntimeRpcMethod, params: Value) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            id,
            method: method.as_str().to_string(),
            params,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_jsonrpc_version(self.jsonrpc.as_str())?;
        if self.id == RuntimeRpcId::Null {
            return Err("runtime_rpc_request_id_must_not_be_null".to_string());
        }
        self.id.validate()?;
        validate_method_name(self.method.as_str())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RuntimeRpcNotification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_jsonrpc_version(self.jsonrpc.as_str())?;
        validate_method_name(self.method.as_str())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRpcResponse {
    pub jsonrpc: String,
    pub id: RuntimeRpcId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RuntimeRpcError>,
}

impl RuntimeRpcResponse {
    pub fn success(id: RuntimeRpcId, result: Value) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: RuntimeRpcId, error: RuntimeRpcError) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_jsonrpc_version(self.jsonrpc.as_str())?;
        self.id.validate()?;
        match (self.result.is_some(), self.error.is_some()) {
            (true, false) | (false, true) => Ok(()),
            (true, true) => Err("runtime_rpc_response_result_and_error_are_exclusive".to_string()),
            (false, false) => Err("runtime_rpc_response_missing_result_or_error".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RuntimeRpcError {
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(message: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: message.into(),
            data: None,
        }
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

fn validate_jsonrpc_version(value: &str) -> Result<(), String> {
    if value == JSON_RPC_VERSION {
        Ok(())
    } else {
        Err(format!("runtime_rpc_invalid_jsonrpc_version: {value}"))
    }
}

fn validate_method_name(value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("runtime_rpc_method_must_not_be_empty".to_string());
    }
    if trimmed != value {
        return Err("runtime_rpc_method_must_not_have_outer_whitespace".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_uses_json_rpc_2_envelope() {
        let request = RuntimeRpcRequest::new(
            RuntimeRpcId::String("req-1".to_string()),
            RuntimeRpcMethod::SessionPrompt,
            json!({"sessionId": "chat-1"}),
        );

        request.validate().expect("valid runtime rpc request");
        let encoded = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(encoded["jsonrpc"], "2.0");
        assert_eq!(encoded["id"], "req-1");
        assert_eq!(encoded["method"], "session/prompt");
        assert!(encoded.get("command").is_none());
    }

    #[test]
    fn answer_now_has_one_canonical_rpc_method() {
        let method = RuntimeRpcMethod::parse("_centaeris/session/answer_now")
            .expect("answer-now method is registered");
        assert_eq!(method, RuntimeRpcMethod::CentaerisSessionAnswerNow);
        assert_eq!(method.as_str(), "_centaeris/session/answer_now");
        assert!(RuntimeRpcMethod::parse("answer_now").is_none());
    }

    #[test]
    fn notification_has_no_request_id() {
        let notification =
            RuntimeRpcNotification::new("runtime.progress", json!({"transient": true}));

        notification
            .validate()
            .expect("valid runtime rpc notification");
        let encoded = serde_json::to_value(&notification).expect("serialize notification");
        assert!(encoded.get("id").is_none());
        assert_eq!(encoded["jsonrpc"], "2.0");
    }

    #[test]
    fn response_requires_exactly_one_result_or_error() {
        let response =
            RuntimeRpcResponse::success(RuntimeRpcId::Number(7), json!({"accepted": true}));
        response.validate().expect("success response is valid");

        let mut invalid = response.clone();
        invalid.error = Some(RuntimeRpcError::internal_error("boom"));
        assert!(invalid
            .validate()
            .expect_err("result and error must be exclusive")
            .contains("exclusive"));

        let invalid = RuntimeRpcResponse {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            id: RuntimeRpcId::String("req-2".to_string()),
            result: None,
            error: None,
        };
        assert!(invalid
            .validate()
            .expect_err("empty response must fail")
            .contains("missing_result_or_error"));
    }

    #[test]
    fn error_response_can_use_null_id_for_decode_failures() {
        let response =
            RuntimeRpcResponse::failure(RuntimeRpcId::Null, RuntimeRpcError::parse_error("bad"));

        response
            .validate()
            .expect("null-id error response is valid");
        let encoded = serde_json::to_value(response).expect("serialize response");
        assert_eq!(encoded["id"], serde_json::Value::Null);
        assert_eq!(encoded["error"]["code"], -32700);
    }

    #[test]
    fn request_rejects_null_id() {
        let request = RuntimeRpcRequest {
            jsonrpc: JSON_RPC_VERSION.to_string(),
            id: RuntimeRpcId::Null,
            method: RuntimeRpcMethod::SessionPrompt.as_str().to_string(),
            params: json!({}),
        };

        assert!(request
            .validate()
            .expect_err("request id null must fail")
            .contains("must_not_be_null"));
    }

    #[test]
    fn request_is_exact_and_uses_camel_case() {
        let request = serde_json::from_value::<RuntimeRpcRequest>(json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "session/prompt",
            "params": {"sessionId": "chat-1"}
        }))
        .expect("canonical request");
        assert_eq!(request.params["sessionId"], "chat-1");

        assert!(serde_json::from_value::<RuntimeRpcRequest>(json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "session/prompt",
            "params": {},
            "banana": true
        }))
        .is_err());
    }
}
