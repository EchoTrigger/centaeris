use crate::model::prepared_prompt::ModelInputImageSourceRefV1;
use crate::runtime::contracts::JsonMap;
use crate::runtime::message_handler::MessageHandler;
use crate::session::state::{ModelMessageSemanticsV1, SessionStateSnapshot};
use crate::tool::layer::ToolExecutionResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolContextWriteSummary {
    pub tool_messages_written: usize,
}

pub(super) fn write_tool_results_to_context(
    message_handler: &MessageHandler,
    session: &mut SessionStateSnapshot,
    tool_results: &[ToolExecutionResult],
) -> Result<ToolContextWriteSummary, String> {
    for report in tool_results {
        let mut metadata = JsonMap::new();
        let image_sources = tool_result_model_input_image_sources(&report.details)?;
        if !image_sources.is_empty() {
            metadata.insert(
                crate::runtime::keys::metadata::MODEL_INPUT_IMAGE_SOURCES.to_string(),
                serde_json::to_string(&image_sources).map_err(|error| {
                    format!("encode tool model input image sources failed: {error}")
                })?,
            );
        }
        message_handler.push_model_tool_message(
            session,
            report.content.as_str(),
            metadata,
            build_model_tool_result_semantics(report),
        );
    }

    Ok(ToolContextWriteSummary {
        tool_messages_written: tool_results.len(),
    })
}

pub(super) fn tool_result_model_input_image_sources(
    details: &serde_json::Value,
) -> Result<Vec<ModelInputImageSourceRefV1>, String> {
    let Some(value) = details.get("modelInputImages") else {
        return Ok(Vec::new());
    };
    let sources = serde_json::from_value::<Vec<ModelInputImageSourceRefV1>>(value.clone())
        .map_err(|error| format!("decode tool model input image sources failed: {error}"))?;
    for source in &sources {
        source.validate()?;
    }
    Ok(sources)
}

pub(super) fn build_model_tool_result_semantics(
    report: &ToolExecutionResult,
) -> ModelMessageSemanticsV1 {
    ModelMessageSemanticsV1::ToolResult {
        tool_call_id: report.tool_call_id.clone(),
        tool_name: report.tool_name.clone(),
        status: report.status.clone(),
        result_state: report.result_state().as_str().to_string(),
        error_kind: report
            .error
            .as_ref()
            .map(|error| error.kind.as_str().to_string()),
        object_refs: tool_result_object_refs(&report.details),
        transition_reason: report.transition_reason.clone(),
    }
}

fn tool_result_object_refs(details: &serde_json::Value) -> Vec<String> {
    let mut refs = Vec::new();
    for key in ["evidenceObjectId", "evidenceRollupObjectId", "objectId"] {
        if let Some(value) = details.get(key).and_then(serde_json::Value::as_str) {
            if !value.trim().is_empty() {
                refs.push(value.to_string());
            }
        }
    }
    if let Some(object_ref) = details.get("objectRef") {
        if let Some(object_id) = object_ref
            .get("objectId")
            .and_then(serde_json::Value::as_str)
        {
            if !object_id.trim().is_empty() {
                refs.push(object_id.to_string());
            }
        }
    }
    refs.sort();
    refs.dedup();
    refs
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use crate::model::prepared_prompt::{
        project_session_messages_to_model_messages, PreparedPromptV1,
    };
    use crate::model::{
        JsonHttpFuture, JsonHttpRequest, JsonHttpResponse, JsonHttpTransport, ModelClient,
        ModelClientRequest, ModelProviderKind, ModelProviderRegistry, ModelSessionConfig,
        OpenAiCompatibleModelClient,
    };
    use crate::runtime::message_handler::{MessageHandler, MessageHandlerConfig};
    use crate::session::state::{
        MessageRole, ModelMessageSemanticsV1, ModelToolCallStateV1, SessionStateSnapshot,
    };
    use crate::tool::layer::{ToolExecutionResult, ToolInvocationRequest, ToolLayer};
    use crate::tool::{ModelToolChoice, ModelToolDefinition};

    use super::*;

    #[test]
    fn writer_copies_executor_content_verbatim() {
        let handler = MessageHandler::new(MessageHandlerConfig {
            max_message_chars: 8_000,
        });
        let mut session = SessionStateSnapshot::new("chat-tool-context".to_string(), 0);
        let report = ToolExecutionResult {
            tool_call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            status: "error".to_string(),
            content: "Command failed with exit code 127.\nstderr (truncated=false):\nbash: python: command not found".to_string(),
            details: serde_json::json!({
                "runtimeDiagnostics": [{
                    "source": "networkProxy",
                    "message": "host-only diagnostic",
                    "details": {
                        "targetHost": "localhost",
                        "networkPolicyMode": "publicInternet"
                    }
                }],
                "objectId": "external_context:test",
            }),
            facts: Vec::new(),
            error: None,
            started_at_ms: 1,
            completed_at_ms: 2,
            latency_ms: 1,
            parallel_group: Some("serial".to_string()),
            transition_reason: Some("local_tool_exec_error".to_string()),
        };

        write_tool_results_to_context(&handler, &mut session, &[report])
            .expect("write tool context");

        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].role, MessageRole::Tool);
        assert_eq!(
            session.messages[0].content,
            "Command failed with exit code 127.\nstderr (truncated=false):\nbash: python: command not found"
        );
        assert!(!session.messages[0].content.contains("host-only diagnostic"));
        assert!(!session.messages[0].content.contains("localhost"));
        assert!(!session.messages[0].content.contains("publicInternet"));
        assert!(matches!(
            session.model_semantics_for(session.messages[0].message_id.as_str()),
            Ok(ModelMessageSemanticsV1::ToolResult { object_refs, .. })
                if object_refs == &["external_context:test".to_string()]
        ));
    }

    #[derive(Clone, Default)]
    struct RecordingJsonTransport {
        requests: Arc<Mutex<Vec<JsonHttpRequest>>>,
    }

    impl JsonHttpTransport for RecordingJsonTransport {
        fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
            Box::pin(async move {
                self.requests
                    .lock()
                    .expect("record HTTP request")
                    .push(request.clone());
                Ok(JsonHttpResponse {
                    status_code: 200,
                    headers: HashMap::new(),
                    body_json:
                        "{\"id\":\"response-after-tool\",\"choices\":[{\"message\":{\"content\":\"done\"}}]}"
                            .to_string(),
                })
            })
        }
    }

    fn remove_isolated_test_workspace(path: &std::path::Path) {
        let mut last_error = None;
        for _ in 0..40 {
            match std::fs::remove_dir_all(path) {
                Ok(()) => return,
                Err(_) if !path.exists() => return,
                Err(error) => {
                    last_error = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
        }
        panic!(
            "cleanup isolated test workspace failed: path={}, error={}",
            path.display(),
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }

    #[tokio::test]
    async fn bash_exit_127_stderr_reaches_openai_request_without_host_details() {
        let test_root = std::env::temp_dir().join(format!(
            "centaeris-tool-result-boundary-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        std::fs::create_dir_all(test_root.as_path()).expect("create isolated test workspace");
        let workspace_root = test_root
            .canonicalize()
            .expect("canonicalize isolated test workspace");
        let tool_layer = ToolLayer::new()
            .with_cwd(workspace_root.clone())
            .expect("configure isolated tool workspace");
        let tool_call_id = "call-bash-exit-127";
        let missing_command = "centaeris_command_that_does_not_exist_127";
        let report = tool_layer.execute(ToolInvocationRequest {
            tool_call_id: tool_call_id.to_string(),
            tool_name: "bash".to_string(),
            args_json: serde_json::json!({
                "command": missing_command,
                "timeout_ms": 30_000,
            })
            .to_string(),
        });

        assert_eq!(report.status, "error");
        assert_eq!(report.details["exitCode"], 127, "report={report:#?}");
        assert!(report.details.get("executionHost").is_some());
        assert!(report.content.contains("exit code 127"));
        assert!(report.content.contains(missing_command));
        assert!(report.content.contains("command not found"));
        assert!(!report.content.contains("executionHost"));
        assert!(!report.content.contains("runtimeDiagnostics"));

        let message_handler = MessageHandler::new(MessageHandlerConfig {
            max_message_chars: 64_000,
        });
        let mut session = SessionStateSnapshot::new("chat-tool-boundary".to_string(), 0);
        message_handler.push_user_message(
            &mut session,
            "Run the requested command.",
            HashMap::new(),
        );
        message_handler.push_model_assistant_message(
            &mut session,
            "",
            HashMap::new(),
            ModelMessageSemanticsV1::Assistant {
                reasoning_content: None,
                tool_calls: vec![ModelToolCallStateV1 {
                    id: tool_call_id.to_string(),
                    name: "bash".to_string(),
                    args_json: serde_json::json!({ "command": missing_command }).to_string(),
                }],
            },
        );
        write_tool_results_to_context(
            &message_handler,
            &mut session,
            std::slice::from_ref(&report),
        )
        .expect("write canonical tool result");

        let prepared_prompt = PreparedPromptV1::new(
            Some("You are Centaeris.".to_string()),
            project_session_messages_to_model_messages(&session, session.messages.as_slice())
                .expect("project session messages"),
            vec![ModelToolDefinition {
                name: "bash".to_string(),
                description: "Run a foreground command.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"],
                    "additionalProperties": false,
                }),
            }],
            ModelToolChoice::Auto,
            512,
        )
        .expect("build prepared prompt");
        let transport = RecordingJsonTransport::default();
        let recorded_requests = Arc::clone(&transport.requests);
        let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
        client
            .generate(&ModelClientRequest {
                session_id: session.session_id.clone(),
                turn_id: "turn-after-bash-error".to_string(),
                loop_index: 1,
                provider_prompt_cache_key: None,
                provider_prompt_cache_retention: None,
                system_prompt_manifest_json: None,
                compression_stats_json: None,
                context_token_estimate: 256,
                prepared_prompt,
                session_config: ModelSessionConfig {
                    provider_kind: ModelProviderKind::Custom,
                    provider_id: "custom.openai_compatible".to_string(),
                    model: "test-model".to_string(),
                    api_base: Some("http://localhost:4000/v1".to_string()),
                    timeout_ms: 5_000,
                    max_retries: 0,
                    retry_backoff_ms: 0,
                    max_output_tokens: Some(512),
                    thinking_mode: None,
                    metadata: HashMap::new(),
                },
            })
            .await
            .expect("serialize next OpenAI-compatible request");

        let requests = recorded_requests.lock().expect("read HTTP requests");
        assert_eq!(requests.len(), 1);
        let request_body =
            serde_json::from_str::<serde_json::Value>(requests[0].body_json.as_str())
                .expect("decode OpenAI-compatible request");
        let tool_message = request_body["messages"]
            .as_array()
            .expect("request messages")
            .iter()
            .find(|message| message["role"] == "tool")
            .expect("native tool result message");
        assert_eq!(tool_message["tool_call_id"], tool_call_id);
        assert_eq!(tool_message["content"], report.content);
        assert!(requests[0].body_json.contains(missing_command));
        assert!(requests[0].body_json.contains("command not found"));
        assert!(requests[0].body_json.contains("exit code 127"));
        assert!(!requests[0].body_json.contains("executionHost"));
        assert!(!requests[0].body_json.contains("runtimeDiagnostics"));

        drop(requests);
        drop(client);
        drop(tool_layer);
        remove_isolated_test_workspace(test_root.as_path());
    }

    #[tokio::test]
    async fn file_io_failure_reaches_openai_request_with_actionable_logical_path() {
        let test_root = std::env::temp_dir().join(format!(
            "centaeris-file-error-boundary-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        std::fs::create_dir_all(test_root.as_path()).expect("create isolated test workspace");
        let workspace_root = test_root
            .canonicalize()
            .expect("canonicalize isolated test workspace");
        let tool_layer = ToolLayer::new()
            .with_cwd(workspace_root.clone())
            .expect("configure isolated tool workspace");
        let tool_call_id = "call-read-missing-file";
        let logical_path = "missing/report.txt";
        let report = tool_layer.execute(ToolInvocationRequest {
            tool_call_id: tool_call_id.to_string(),
            tool_name: "read".to_string(),
            args_json: serde_json::json!({ "path": logical_path }).to_string(),
        });

        assert_eq!(report.status, "error", "report={report:#?}");
        assert!(report.content.contains("missing/report.txt"));
        assert!(report.content.contains("does not exist"));
        assert!(!report.content.contains("/workspace/"));
        assert!(!report
            .content
            .contains(workspace_root.to_string_lossy().as_ref()));
        assert!(!report.content.contains("AppData"));

        let message_handler = MessageHandler::new(MessageHandlerConfig {
            max_message_chars: 64_000,
        });
        let mut session = SessionStateSnapshot::new("chat-file-error-boundary".to_string(), 0);
        message_handler.push_user_message(
            &mut session,
            "Read the requested project file.",
            HashMap::new(),
        );
        message_handler.push_model_assistant_message(
            &mut session,
            "",
            HashMap::new(),
            ModelMessageSemanticsV1::Assistant {
                reasoning_content: None,
                tool_calls: vec![ModelToolCallStateV1 {
                    id: tool_call_id.to_string(),
                    name: "read".to_string(),
                    args_json: serde_json::json!({ "path": logical_path }).to_string(),
                }],
            },
        );
        write_tool_results_to_context(
            &message_handler,
            &mut session,
            std::slice::from_ref(&report),
        )
        .expect("write canonical file error");

        let prepared_prompt = PreparedPromptV1::new(
            Some("You are Centaeris.".to_string()),
            project_session_messages_to_model_messages(&session, session.messages.as_slice())
                .expect("project session messages"),
            vec![ModelToolDefinition {
                name: "read".to_string(),
                description: "Read a project file.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"],
                    "additionalProperties": false,
                }),
            }],
            ModelToolChoice::Auto,
            512,
        )
        .expect("build prepared prompt");
        let transport = RecordingJsonTransport::default();
        let recorded_requests = Arc::clone(&transport.requests);
        let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
        client
            .generate(&ModelClientRequest {
                session_id: session.session_id.clone(),
                turn_id: "turn-after-file-error".to_string(),
                loop_index: 1,
                provider_prompt_cache_key: None,
                provider_prompt_cache_retention: None,
                system_prompt_manifest_json: None,
                compression_stats_json: None,
                context_token_estimate: 256,
                prepared_prompt,
                session_config: ModelSessionConfig {
                    provider_kind: ModelProviderKind::Custom,
                    provider_id: "custom.openai_compatible".to_string(),
                    model: "test-model".to_string(),
                    api_base: Some("http://localhost:4000/v1".to_string()),
                    timeout_ms: 5_000,
                    max_retries: 0,
                    retry_backoff_ms: 0,
                    max_output_tokens: Some(512),
                    thinking_mode: None,
                    metadata: HashMap::new(),
                },
            })
            .await
            .expect("serialize next OpenAI-compatible request");

        let requests = recorded_requests.lock().expect("read HTTP requests");
        assert_eq!(requests.len(), 1);
        let request_body =
            serde_json::from_str::<serde_json::Value>(requests[0].body_json.as_str())
                .expect("decode OpenAI-compatible request");
        let tool_message = request_body["messages"]
            .as_array()
            .expect("request messages")
            .iter()
            .find(|message| message["role"] == "tool")
            .expect("native tool result message");
        assert_eq!(tool_message["tool_call_id"], tool_call_id);
        assert_eq!(tool_message["content"], report.content);
        assert!(requests[0].body_json.contains("missing/report.txt"));
        assert!(requests[0].body_json.contains("does not exist"));
        assert!(!requests[0].body_json.contains("/workspace/"));
        assert!(!requests[0]
            .body_json
            .contains(workspace_root.to_string_lossy().as_ref()));

        drop(requests);
        drop(client);
        drop(tool_layer);
        remove_isolated_test_workspace(test_root.as_path());
    }
}
