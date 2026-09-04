use serde::Serialize;
use serde_json::Value;

use super::contract::{
    RuntimeRpcError, RuntimeRpcId, RuntimeRpcNotification, RuntimeRpcRequest, RuntimeRpcResponse,
    JSON_RPC_VERSION,
};

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeRpcFrame {
    Request(RuntimeRpcRequest),
    Notification(RuntimeRpcNotification),
    Response(RuntimeRpcResponse),
}

#[cfg(test)]
impl RuntimeRpcFrame {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Request(request) => request.validate(),
            Self::Notification(notification) => notification.validate(),
            Self::Response(response) => response.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRpcCodecError {
    pub response_id: RuntimeRpcId,
    pub error: RuntimeRpcError,
}

impl RuntimeRpcCodecError {
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self {
            response_id: RuntimeRpcId::Null,
            error: RuntimeRpcError::parse_error(message),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            response_id: RuntimeRpcId::Null,
            error: RuntimeRpcError::invalid_request(message),
        }
    }

    pub fn invalid_request_with_id(response_id: RuntimeRpcId, message: impl Into<String>) -> Self {
        Self {
            response_id,
            error: RuntimeRpcError::invalid_request(message),
        }
    }

    pub fn response(self) -> RuntimeRpcResponse {
        RuntimeRpcResponse::failure(self.response_id, self.error)
    }
}

pub fn decode_jsonl_frame(line: &str) -> Result<RuntimeRpcFrame, RuntimeRpcCodecError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.trim().is_empty() {
        return Err(RuntimeRpcCodecError::invalid_request(
            "runtime_rpc_jsonl_frame_must_not_be_empty",
        ));
    }

    let value = serde_json::from_str::<Value>(trimmed).map_err(|error| {
        RuntimeRpcCodecError::parse_error(format!("runtime_rpc_json_parse_failed: {error}"))
    })?;
    let object = value.as_object().ok_or_else(|| {
        RuntimeRpcCodecError::invalid_request("runtime_rpc_frame_must_be_json_object")
    })?;

    reject_non_json_rpc_sidecar_envelope(object)?;
    validate_jsonrpc_member(object)?;

    if object.contains_key("method") {
        if object.contains_key("id") {
            let request =
                serde_json::from_value::<RuntimeRpcRequest>(value.clone()).map_err(|error| {
                    invalid_request_for_object(
                        object,
                        format!("runtime_rpc_request_decode_failed: {error}"),
                    )
                })?;
            request
                .validate()
                .map_err(|error| invalid_request_for_object(object, error))?;
            return Ok(RuntimeRpcFrame::Request(request));
        }

        let notification = serde_json::from_value::<RuntimeRpcNotification>(value.clone())
            .map_err(|error| {
                invalid_request_for_object(
                    object,
                    format!("runtime_rpc_notification_decode_failed: {error}"),
                )
            })?;
        notification
            .validate()
            .map_err(|error| invalid_request_for_object(object, error))?;
        return Ok(RuntimeRpcFrame::Notification(notification));
    }

    if object.contains_key("result") || object.contains_key("error") {
        let response =
            serde_json::from_value::<RuntimeRpcResponse>(value.clone()).map_err(|error| {
                invalid_request_for_object(
                    object,
                    format!("runtime_rpc_response_decode_failed: {error}"),
                )
            })?;
        response
            .validate()
            .map_err(|error| invalid_request_for_object(object, error))?;
        return Ok(RuntimeRpcFrame::Response(response));
    }

    Err(invalid_request_for_object(
        object,
        "runtime_rpc_frame_missing_method_or_response_body",
    ))
}

#[cfg(test)]
pub fn encode_jsonl_frame(frame: &RuntimeRpcFrame) -> Result<String, String> {
    frame.validate()?;
    match frame {
        RuntimeRpcFrame::Request(request) => encode_jsonl_value(request),
        RuntimeRpcFrame::Notification(notification) => encode_jsonl_value(notification),
        RuntimeRpcFrame::Response(response) => encode_jsonl_value(response),
    }
}

pub fn encode_jsonl_value<TValue>(value: &TValue) -> Result<String, String>
where
    TValue: Serialize,
{
    let encoded = serde_json::to_string(value)
        .map_err(|error| format!("runtime_rpc_json_encode_failed: {error}"))?;
    if encoded.contains('\n') || encoded.contains('\r') {
        return Err("runtime_rpc_jsonl_encoder_emitted_multiline_frame".to_string());
    }
    Ok(format!("{encoded}\n"))
}

fn invalid_request_for_object(
    object: &serde_json::Map<String, Value>,
    message: impl Into<String>,
) -> RuntimeRpcCodecError {
    RuntimeRpcCodecError::invalid_request_with_id(response_id_from_object(object), message)
}

fn response_id_from_object(object: &serde_json::Map<String, Value>) -> RuntimeRpcId {
    object
        .get("id")
        .cloned()
        .and_then(|value| serde_json::from_value::<RuntimeRpcId>(value).ok())
        .unwrap_or(RuntimeRpcId::Null)
}

fn validate_jsonrpc_member(
    object: &serde_json::Map<String, Value>,
) -> Result<(), RuntimeRpcCodecError> {
    match object.get("jsonrpc").and_then(Value::as_str) {
        Some(JSON_RPC_VERSION) => Ok(()),
        Some(value) => Err(invalid_request_for_object(
            object,
            format!("runtime_rpc_invalid_jsonrpc_version: {value}"),
        )),
        None => Err(invalid_request_for_object(
            object,
            "runtime_rpc_jsonrpc_member_is_required",
        )),
    }
}

fn reject_non_json_rpc_sidecar_envelope(
    object: &serde_json::Map<String, Value>,
) -> Result<(), RuntimeRpcCodecError> {
    if object.contains_key("command") {
        return Err(invalid_request_for_object(
            object,
            "runtime_rpc_non_json_rpc_command_member_rejected",
        ));
    }
    if object.contains_key("kind") {
        return Err(invalid_request_for_object(
            object,
            "runtime_rpc_non_json_rpc_kind_member_rejected",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jsonl_codec_decodes_standard_request() {
        let frame = decode_jsonl_frame(
            r#"{"jsonrpc":"2.0","id":"req-1","method":"session/prompt","params":{"sessionId":"chat-1"}}"#,
        )
        .expect("decode request");

        match frame {
            RuntimeRpcFrame::Request(request) => {
                assert_eq!(request.id, RuntimeRpcId::String("req-1".to_string()));
                assert_eq!(request.method, "session/prompt");
                assert_eq!(request.params["sessionId"], "chat-1");
            }
            other => panic!("expected request frame, got {other:?}"),
        }
    }

    #[test]
    fn jsonl_codec_decodes_standard_notification() {
        let frame = decode_jsonl_frame(
            r#"{"jsonrpc":"2.0","method":"runtime.progress","params":{"agentRunId":"agent-run-1"}}"#,
        )
        .expect("decode notification");

        match frame {
            RuntimeRpcFrame::Notification(notification) => {
                assert_eq!(notification.method, "runtime.progress");
                assert_eq!(notification.params["agentRunId"], "agent-run-1");
            }
            other => panic!("expected notification frame, got {other:?}"),
        }
    }

    #[test]
    fn jsonl_codec_encodes_single_line_response() {
        let response = RuntimeRpcResponse::success(
            RuntimeRpcId::String("req-1".to_string()),
            json!({"text": "first line\nsecond line"}),
        );
        let encoded =
            encode_jsonl_frame(&RuntimeRpcFrame::Response(response)).expect("encode response");

        assert!(encoded.ends_with('\n'));
        assert_eq!(encoded.matches('\n').count(), 1);
        assert!(encoded.contains(r#""jsonrpc":"2.0""#));
        assert!(encoded.contains(r#"first line\nsecond line"#));
    }

    #[test]
    fn jsonl_codec_accepts_only_json_rpc_envelope() {
        let cases = [
            (
                r#"{"id":"req-1","command":"session/prompt","payload":{"request":{"message":"hi"}}}"#,
                RuntimeRpcId::String("req-1".to_string()),
                "non_json_rpc_command_member",
            ),
            (
                r#"{"kind":"event","eventName":"session/update","payload":{"taskId":"task-1"}}"#,
                RuntimeRpcId::Null,
                "non_json_rpc_kind_member",
            ),
        ];

        for (raw, response_id, expected_message) in cases {
            let error = decode_jsonl_frame(raw).expect_err("non JSON-RPC envelope must fail");

            assert_eq!(error.response_id, response_id);
            assert_eq!(error.error.code, -32600);
            assert!(error.error.message.contains(expected_message));
        }
    }

    #[test]
    fn jsonl_codec_preserves_request_id_for_invalid_request_error() {
        let error =
            decode_jsonl_frame(r#"{"jsonrpc":"2.0","id":"req-2","method":" session/prompt"}"#)
                .expect_err("invalid method whitespace must fail");

        assert_eq!(error.response_id, RuntimeRpcId::String("req-2".to_string()));
        assert!(error.error.message.contains("outer_whitespace"));
    }
}
