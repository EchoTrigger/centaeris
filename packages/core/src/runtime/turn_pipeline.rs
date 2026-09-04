use std::collections::HashSet;

use crate::model::prompt::{ModelCompactionSummaryCandidateRequest, PromptCompactionError};
use crate::model::GenerateResult;
use crate::model::{
    ModelClient, ModelClientRequest, ModelClientStreamEvent, ModelSessionConfig,
    ModelSessionConfigStore, DEFAULT_MODEL_OUTPUT_TOKENS,
};
use crate::runtime::contracts::{new_turn_id, RuntimeAgentRunIdentityV1, RuntimeProcessState};
use crate::runtime::event::{RuntimeEventProjection, RuntimeEventVisibility};
use crate::runtime::query_loop::AgentRunResourceUsageV1;
use crate::runtime::{AgentRunStop, QueryContinuation};
use crate::tool::ModelToolChoice;

use super::{
    AsyncGenerateDriver, GenerateDriverError, GenerateDriverOutcome, GenerateDriverRequest,
    ModelObservationV1, ModelRequestPurposeV1, ModelRequestStartedV1, ProcessTurnRequest,
    ToolSafePoint, TurnInput, TurnUpdate,
};

const PROMPT_COMPACTION_SUMMARY_MIN_TIMEOUT_MS: u64 = 300_000;

pub(super) struct ModelClientGenerateDriver<'a, M: ModelClient, S: ModelSessionConfigStore> {
    model_client: &'a M,
    session_config_store: &'a S,
    tool_safe_point: Option<&'a super::ToolSafePointDispatcher<'a>>,
    composition_environment:
        Option<&'a crate::extension::composition::AgentCompositionEnvironmentV1>,
}

impl<'a, M: ModelClient, S: ModelSessionConfigStore> ModelClientGenerateDriver<'a, M, S> {
    pub(super) fn new(model_client: &'a M, session_config_store: &'a S) -> Self {
        Self {
            model_client,
            session_config_store,
            tool_safe_point: None,
            composition_environment: None,
        }
    }

    pub(super) fn new_with_tool_safe_point(
        model_client: &'a M,
        session_config_store: &'a S,
        tool_safe_point: &'a super::ToolSafePointDispatcher<'a>,
        composition_environment: &'a crate::extension::composition::AgentCompositionEnvironmentV1,
    ) -> Self {
        Self {
            model_client,
            session_config_store,
            tool_safe_point: Some(tool_safe_point),
            composition_environment: Some(composition_environment),
        }
    }

    fn commit_model_request_started(
        &self,
        purpose: ModelRequestPurposeV1,
        request: ModelClientRequest,
        observations: Vec<ModelObservationV1>,
    ) -> Result<(), String> {
        let Some(sink) = self.tool_safe_point else {
            return Ok(());
        };
        let composition = self
            .composition_environment
            .ok_or_else(|| "model request durability requires agent composition".to_string())?
            .resolve_request(&request)?;
        sink.commit(ToolSafePoint::ModelRequestStarted(
            ModelRequestStartedV1::from_request(purpose, &request, observations, composition)?,
        ))
    }
}

impl<'a, M: ModelClient, S: ModelSessionConfigStore> AsyncGenerateDriver
    for ModelClientGenerateDriver<'a, M, S>
{
    fn generate_prompt_compaction_summary_async<'b>(
        &'b self,
        request: &'b ModelCompactionSummaryCandidateRequest,
    ) -> super::GenerateDriverPromptCompactionFuture<'b> {
        Box::pin(async move {
            Some(
                generate_prompt_compaction_summary_with_model_client_async(
                    self.model_client,
                    self.session_config_store,
                    self.tool_safe_point,
                    self.composition_environment,
                    request,
                )
                .await,
            )
        })
    }

    fn generate_next_async<'b>(
        &'b self,
        req: &'b GenerateDriverRequest,
    ) -> super::GenerateDriverFuture<'b, GenerateDriverOutcome> {
        Box::pin(async move {
            let session_config = self
                .session_config_store
                .get_session_config(req.session_id.as_str())?
                .unwrap_or_else(ModelSessionConfig::default);
            let request = ModelClientRequest {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                loop_index: req.loop_index,
                provider_prompt_cache_key: req.provider_prompt_cache_key.clone(),
                provider_prompt_cache_retention: req.provider_prompt_cache_retention.clone(),
                system_prompt_manifest_json: req.system_prompt_manifest_json.clone(),
                compression_stats_json: req.compression_stats_json.clone(),
                context_token_estimate: req.context_token_estimate,
                prepared_prompt: req.prepared_prompt.clone(),
                session_config,
            };
            self.commit_model_request_started(
                ModelRequestPurposeV1::Main,
                request.clone(),
                req.observations.clone(),
            )?;
            let response = self
                .model_client
                .generate(&request)
                .await
                .map_err(|error| GenerateDriverError::from_model_client(error, String::new()))?;
            Ok(GenerateDriverOutcome {
                generate_result: response.generate_result,
                provider_attempts: response.provider_attempts,
            })
        })
    }

    fn generate_next_with_sink_async<'b>(
        &'b self,
        req: &'b GenerateDriverRequest,
        sink: &'b mut (dyn FnMut(TurnUpdate) + Send),
    ) -> super::GenerateDriverFuture<'b, GenerateDriverOutcome> {
        Box::pin(async move {
            let session_config = self
                .session_config_store
                .get_session_config(req.session_id.as_str())?
                .unwrap_or_else(ModelSessionConfig::default);
            let request = ModelClientRequest {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                loop_index: req.loop_index,
                provider_prompt_cache_key: req.provider_prompt_cache_key.clone(),
                provider_prompt_cache_retention: req.provider_prompt_cache_retention.clone(),
                system_prompt_manifest_json: req.system_prompt_manifest_json.clone(),
                compression_stats_json: req.compression_stats_json.clone(),
                context_token_estimate: req.context_token_estimate,
                prepared_prompt: req.prepared_prompt.clone(),
                session_config,
            };
            self.commit_model_request_started(
                ModelRequestPurposeV1::Main,
                request.clone(),
                req.observations.clone(),
            )?;
            sink(TurnUpdate::ModelRequestStart {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                purpose: ModelRequestPurposeV1::Main,
                context_token_estimate: req.context_token_estimate,
                message: None,
                process_state: RuntimeProcessState::Thinking,
                elapsed_ms: 0,
                initial_content: req.live_content_prefix.clone(),
            });
            sink(TurnUpdate::Status {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                message: None,
                process_state: RuntimeProcessState::Thinking,
            });
            let mut visible_stream_content = String::new();
            let response = self
                .model_client
                .generate_stream(&request, &mut |event| {
                    match &event {
                        ModelClientStreamEvent::Token { content } if !content.is_empty() => {
                            visible_stream_content.push_str(content);
                        }
                        ModelClientStreamEvent::ReplaceContent { content } => {
                            visible_stream_content.clone_from(content);
                        }
                        _ => {}
                    }
                    forward_model_client_stream_event(req, sink, event);
                })
                .await;
            let response = response.map_err(|error| {
                let output_token_limit =
                    error.provider_code.as_deref() == Some("incomplete_output_token_limit");
                if output_token_limit {
                    sink(TurnUpdate::ModelDone {
                        session_id: req.session_id.clone(),
                        turn_id: req.turn_id.clone(),
                        finish_reason: Some("incomplete_output_token_limit".to_string()),
                        process_state: RuntimeProcessState::Recovering,
                    });
                } else if !output_token_limit && !visible_stream_content.is_empty() {
                    sink(TurnUpdate::ReplaceContent {
                        session_id: req.session_id.clone(),
                        turn_id: req.turn_id.clone(),
                        content: String::new(),
                    });
                }
                let reason = error.kind.as_str().to_string();
                let retryable = error.retryable;
                let error = GenerateDriverError::from_model_client(
                    error,
                    std::mem::take(&mut visible_stream_content),
                );
                if !error.is_output_token_limit() {
                    sink(TurnUpdate::RuntimeError {
                        session_id: req.session_id.clone(),
                        turn_id: req.turn_id.clone(),
                        message: error.message.clone(),
                        process_state: RuntimeProcessState::from_provider_error_reason(
                            reason.as_str(),
                        ),
                        reason,
                        retryable,
                    });
                }
                error
            })?;
            Ok(GenerateDriverOutcome {
                generate_result: response.generate_result,
                provider_attempts: response.provider_attempts,
            })
        })
    }
}

fn forward_model_client_stream_event(
    req: &GenerateDriverRequest,
    sink: &mut dyn FnMut(TurnUpdate),
    event: ModelClientStreamEvent,
) {
    match event {
        ModelClientStreamEvent::RequestStart {
            message,
            process_state,
            elapsed_ms,
        } => {
            let _ = elapsed_ms;
            sink(TurnUpdate::Status {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                message,
                process_state,
            });
        }
        ModelClientStreamEvent::Token { content } => {
            sink(TurnUpdate::Token {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                content,
            });
        }
        ModelClientStreamEvent::ReplaceContent { content } => {
            sink(TurnUpdate::ReplaceContent {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                content,
            });
        }
        ModelClientStreamEvent::Status {
            message,
            process_state,
        } => {
            sink(TurnUpdate::Status {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                message,
                process_state,
            });
        }
        ModelClientStreamEvent::ToolCallPreparing { name } => {
            let process_state = RuntimeProcessState::from_tool_name(name.as_str());
            sink(TurnUpdate::ToolCallPreparing {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                name,
                process_state,
            });
        }
        ModelClientStreamEvent::ToolCallReady {
            call_id,
            provider_item_id,
            name,
            args_json,
            args_preview,
        } => {
            let process_state = RuntimeProcessState::from_tool_name(name.as_str());
            sink(TurnUpdate::ToolCallReady {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                call_id,
                provider_item_id,
                name,
                process_state,
                args_json,
                args_preview,
            });
        }
        ModelClientStreamEvent::Done { finish_reason } => {
            sink(TurnUpdate::ModelDone {
                session_id: req.session_id.clone(),
                turn_id: req.turn_id.clone(),
                finish_reason,
                process_state: RuntimeProcessState::Synthesizing,
            });
        }
    }
}

async fn generate_prompt_compaction_summary_with_model_client_async<
    M: ModelClient,
    S: ModelSessionConfigStore,
>(
    model_client: &M,
    session_config_store: &S,
    tool_safe_point: Option<&super::ToolSafePointDispatcher<'_>>,
    composition_environment: Option<&crate::extension::composition::AgentCompositionEnvironmentV1>,
    request: &ModelCompactionSummaryCandidateRequest,
) -> super::GenerateDriverPromptCompactionOutcome {
    let mut session_config =
        match session_config_store.get_session_config(request.session_id.as_str()) {
            Ok(config) => config.unwrap_or_else(ModelSessionConfig::default),
            Err(err) => {
                return super::GenerateDriverPromptCompactionOutcome {
                    result: Err(PromptCompactionError::provider(format!(
                        "load model compaction session config failed: {err}"
                    ))),
                    resource_usage: AgentRunResourceUsageV1::default(),
                };
            }
        };
    let configured_max_output_tokens = session_config
        .max_output_tokens
        .unwrap_or(DEFAULT_MODEL_OUTPUT_TOKENS);
    if request.max_output_tokens == 0 || request.max_output_tokens > configured_max_output_tokens {
        return super::GenerateDriverPromptCompactionOutcome {
            result: Err(PromptCompactionError::provider(format!(
                "model compaction output limit invalid: request={} model={configured_max_output_tokens}",
                request.max_output_tokens
            ))),
            resource_usage: AgentRunResourceUsageV1::default(),
        };
    }
    if request.prompt_token_estimate > request.input_limit_tokens {
        return super::GenerateDriverPromptCompactionOutcome {
            result: Err(PromptCompactionError::provider(format!(
                "model compaction input budget exceeded: estimatedTokens={} inputLimitTokens={}",
                request.prompt_token_estimate, request.input_limit_tokens
            ))),
            resource_usage: AgentRunResourceUsageV1::default(),
        };
    }
    let summary_max_output_tokens = request.max_output_tokens;
    let prepared_prompt = match crate::model::prepared_prompt::PreparedPromptV1::new(
        None,
        vec![crate::model::prepared_prompt::ModelMessageV1 {
            message_id: format!("msg:{}:prompt_compaction", request.turn_id),
            role: crate::model::prepared_prompt::ModelMessageRoleV1::User,
            content: request.prompt.clone(),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }],
        vec![],
        ModelToolChoice::None,
        summary_max_output_tokens,
    ) {
        Ok(prepared_prompt) => prepared_prompt,
        Err(err) => {
            return super::GenerateDriverPromptCompactionOutcome {
                result: Err(PromptCompactionError::provider(err)),
                resource_usage: AgentRunResourceUsageV1::default(),
            };
        }
    };
    session_config.max_output_tokens = Some(summary_max_output_tokens);
    session_config.timeout_ms = session_config
        .timeout_ms
        .max(PROMPT_COMPACTION_SUMMARY_MIN_TIMEOUT_MS);
    let compaction_prompt = prepared_prompt.messages[0].clone();
    let client_request = ModelClientRequest {
        session_id: request.session_id.clone(),
        turn_id: new_turn_id(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate: request.prompt_token_estimate,
        prepared_prompt,
        session_config,
    };
    if let Some(sink) = tool_safe_point {
        let composition = match composition_environment
            .ok_or_else(|| "model compaction durability requires agent composition".to_string())
            .and_then(|environment| environment.resolve_request(&client_request))
        {
            Ok(composition) => composition,
            Err(error) => {
                return super::GenerateDriverPromptCompactionOutcome {
                    result: Err(PromptCompactionError::durability(error)),
                    resource_usage: AgentRunResourceUsageV1::default(),
                }
            }
        };
        let started = ModelRequestStartedV1::from_request(
            ModelRequestPurposeV1::Compaction,
            &client_request,
            vec![ModelObservationV1::CompactionPrompt {
                message: compaction_prompt,
            }],
            composition,
        );
        if let Err(error) =
            started.and_then(|started| sink.commit(ToolSafePoint::ModelRequestStarted(started)))
        {
            return super::GenerateDriverPromptCompactionOutcome {
                result: Err(PromptCompactionError::durability(error)),
                resource_usage: AgentRunResourceUsageV1::default(),
            };
        }
    }
    let response = model_client.generate(&client_request).await;
    match response {
        Ok(response) => {
            let mut resource_usage = AgentRunResourceUsageV1::default();
            resource_usage.record_completed_provider_round(
                &response.generate_result,
                request.prompt_token_estimate,
                response.provider_attempts,
            );
            super::GenerateDriverPromptCompactionOutcome {
                result: Ok(response.generate_result.content),
                resource_usage,
            }
        }
        Err(err) => {
            let mut resource_usage = AgentRunResourceUsageV1::default();
            resource_usage
                .record_provider_attempts(request.prompt_token_estimate, err.provider_attempts);
            super::GenerateDriverPromptCompactionOutcome {
                result: Err(PromptCompactionError::provider(format!(
                    "model compaction request failed(kind={},retryable={}): {}",
                    err.kind.as_str(),
                    err.retryable,
                    err.message
                ))),
                resource_usage,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct PreparedTurnGeneration {
    session_id: String,
    turn_id: String,
    input: TurnInput,
    agent_run_identity: Option<RuntimeAgentRunIdentityV1>,
    generate_req: GenerateDriverRequest,
}

impl PreparedTurnGeneration {
    pub(super) fn new(
        session_id: String,
        turn_id: String,
        input: TurnInput,
        agent_run_identity: Option<RuntimeAgentRunIdentityV1>,
        generate_req: GenerateDriverRequest,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            input,
            agent_run_identity,
            generate_req,
        }
    }
}

pub(super) async fn drive_generate_once_async<D: AsyncGenerateDriver>(
    driver: &D,
    req: &GenerateDriverRequest,
) -> Result<(GenerateResult, u32), GenerateDriverError> {
    Ok(generate_outcome_to_result(
        driver.generate_next_async(req).await?,
    ))
}

pub(super) async fn drive_generate_once_with_sink_async<D: AsyncGenerateDriver>(
    driver: &D,
    req: &GenerateDriverRequest,
    sink: &mut (dyn FnMut(TurnUpdate) + Send),
) -> Result<(GenerateResult, u32), GenerateDriverError> {
    Ok(generate_outcome_to_result(
        driver.generate_next_with_sink_async(req, sink).await?,
    ))
}

pub(super) async fn drive_prepared_turn_async<D: AsyncGenerateDriver>(
    driver: &D,
    prepared: PreparedTurnGeneration,
) -> Result<ProcessTurnRequest, GenerateDriverError> {
    let generated = drive_generate_once_async(driver, &prepared.generate_req).await?;
    Ok(prepared.into_turn_request(generated.0, generated.1))
}

pub(super) async fn drive_prepared_turn_with_sink_async<D: AsyncGenerateDriver>(
    driver: &D,
    prepared: PreparedTurnGeneration,
    sink: &mut (dyn FnMut(TurnUpdate) + Send),
) -> Result<ProcessTurnRequest, GenerateDriverError> {
    let generated =
        drive_generate_once_with_sink_async(driver, &prepared.generate_req, sink).await?;
    Ok(prepared.into_turn_request(generated.0, generated.1))
}

fn generate_outcome_to_result(outcome: GenerateDriverOutcome) -> (GenerateResult, u32) {
    (outcome.generate_result, outcome.provider_attempts)
}

impl PreparedTurnGeneration {
    fn into_turn_request(
        self,
        generate_result: GenerateResult,
        provider_attempts: u32,
    ) -> ProcessTurnRequest {
        ProcessTurnRequest {
            session_id: self.session_id,
            turn_id: self.turn_id,
            input: self.input,
            agent_run_identity: self.agent_run_identity,
            generate_result,
            agent_run_resource_usage: AgentRunResourceUsageV1 {
                provider_attempts,
                ..AgentRunResourceUsageV1::default()
            },
        }
    }
}

pub(super) fn emit_runtime_events_to_stream(
    runtime_events: &[RuntimeEventProjection],
    sink: &mut dyn FnMut(TurnUpdate),
) {
    emit_runtime_events_to_stream_excluding(runtime_events, sink, None);
}

pub(super) fn emit_runtime_events_to_stream_excluding(
    runtime_events: &[RuntimeEventProjection],
    sink: &mut dyn FnMut(TurnUpdate),
    streamed_session_event_ids: Option<&HashSet<String>>,
) {
    for event in runtime_events {
        if event.visibility != RuntimeEventVisibility::User {
            continue;
        }
        if streamed_session_event_ids.is_some_and(|ids| ids.contains(event.event_id.as_str())) {
            continue;
        }
        sink(TurnUpdate::RuntimeEvent {
            event: event.clone(),
        });
    }
}

pub(super) fn push_runtime_event_with_optional_stream<TSink>(
    runtime_events: &mut Vec<RuntimeEventProjection>,
    stream_sink: &mut Option<&mut TSink>,
    event: RuntimeEventProjection,
) where
    TSink: FnMut(TurnUpdate) + ?Sized,
{
    if let Some(sink) = stream_sink.as_deref_mut() {
        emit_runtime_event_to_stream(&event, sink);
    }
    runtime_events.push(event);
}

pub(super) fn track_streamed_runtime_event_id(
    event: &TurnUpdate,
    streamed_session_event_ids: &mut HashSet<String>,
) {
    if let TurnUpdate::RuntimeEvent { event } = event {
        streamed_session_event_ids.insert(event.event_id.clone());
    }
}

pub(super) fn continuation_run_stop(continuation: QueryContinuation) -> Option<AgentRunStop> {
    match continuation {
        QueryContinuation::AwaitQuestion => Some(AgentRunStop::QuestionWait),
        QueryContinuation::AwaitRuntimeJob => Some(AgentRunStop::RuntimeJobWait),
        QueryContinuation::CompleteTerminalTool => Some(AgentRunStop::TerminalTool),
        QueryContinuation::Finalize => Some(AgentRunStop::Finalized),
        QueryContinuation::ExecuteTools => None,
    }
}

fn emit_runtime_event_to_stream<TSink>(event: &RuntimeEventProjection, sink: &mut TSink)
where
    TSink: FnMut(TurnUpdate) + ?Sized,
{
    if event.visibility != RuntimeEventVisibility::User {
        return;
    }
    sink(TurnUpdate::RuntimeEvent {
        event: event.clone(),
    });
}
