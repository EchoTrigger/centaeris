pub(crate) mod canonical_json;
mod checkpoint;
mod config;
pub(crate) mod context_window;
pub mod contracts;
mod driver;
mod engine_state;
pub mod event;
mod events;
mod external_context_objects;
mod generate_request;
pub mod keys;
mod lifecycle_hooks_runtime;
mod loop_runtime;
pub(crate) mod message_handler;
pub mod projection;
mod prompt_compaction_metadata;
mod prompt_compaction_runtime;
mod prompt_projection;
mod provider_polling;
pub mod query_loop;
mod recovery;
mod status_events;
pub mod subagent;
pub(crate) mod subagent_contracts;
mod subagent_projection;
mod subagent_runner;
mod text_preview;
mod tool_batch_executor;
mod tool_context_writer;
mod tool_execution;
mod tool_observability;
mod tool_projection;
mod turn_pipeline;
mod turn_processor;
mod turn_state;

pub const CORE_PROTOCOL_VERSION: &str = "1.0.0";

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub use self::checkpoint::*;
pub use self::config::AgentRuntimeConfig;
pub use self::driver::{
    AgentRunRequest, AgentRunResult, AgentRunResumeIntent, AgentRunStop,
    AnswerNowEnqueueDisposition, AsyncGenerateDriver, ContextTokenBreakdownV1,
    ContextToolTokenEstimateV1, DurableTurnControlBinding, GenerateDriverError,
    GenerateDriverFuture, GenerateDriverOutcome, GenerateDriverPromptCompactionFuture,
    GenerateDriverPromptCompactionOutcome, GenerateDriverRequest, ModelObservationV1,
    ModelRequestPurposeV1, ModelRequestStartedV1, ProcessTurnRequest, RejectedToolCallIdentity,
    ToolSafePoint, TurnControl, TurnInput, TurnStepResult, TurnUpdate,
};
use self::engine_state::*;
use self::external_context_objects::*;
pub use self::lifecycle_hooks_runtime::{
    QueryLifecycleHookOutcome, QueryLifecycleHookRuntime, QueryLifecycleHookStartTargetV1,
    QueryLifecycleHookStopTargetV1,
};
pub use self::loop_runtime::persist_answer_now_requested_fact;
use self::prompt_compaction_metadata::*;
use self::provider_polling::*;
pub use self::subagent_projection::persist_subagent_result_projection_from_scheduler_events;
pub use self::subagent_runner::{
    build_subagent_scheduler_runtime_event, AgentRuntimeSubagentRunnerConfig,
    ModelClientSubagentRunner, QueryLifecycleSubagentObserver, ToolSafePointCommitPort,
};
use self::text_preview::*;
pub use self::tool_observability::project_tool_operations_json;
use self::tool_observability::*;

use self::events::{
    build_runtime_event_agent_run_intervention_changed, build_runtime_event_final_event,
    build_runtime_event_prompt_compaction_event, build_runtime_event_question_required_event,
    build_runtime_event_runtime_wait_changed, build_runtime_event_status_event,
    build_runtime_event_subagent_event_from_scheduler_event,
    build_runtime_event_subagent_spawned_from_tool_result,
    build_runtime_event_subagent_tool_group_events_from_tool_results,
    build_runtime_event_tool_call_events, build_runtime_event_tool_progress_event,
    build_runtime_event_tool_result_events,
};
use self::recovery::merge_recovery_traces;
use self::status_events::{
    continuation_event_status, model_process_summary_message, should_emit_model_process_summary,
    StatusStage,
};
use self::tool_batch_executor::ToolBatchExecutor;
use self::turn_pipeline::{
    continuation_run_stop, drive_prepared_turn_async, drive_prepared_turn_with_sink_async,
    emit_runtime_events_to_stream, emit_runtime_events_to_stream_excluding,
    push_runtime_event_with_optional_stream, track_streamed_runtime_event_id,
    ModelClientGenerateDriver, PreparedTurnGeneration,
};
use crate::model::prompt::{
    run_one_turn_model_compaction_and_pre_hook, run_one_turn_model_compaction_async_and_pre_hook,
    AsyncModelCompactionSummaryCandidateProducer, ModelCompactionSummaryCandidateProducer,
    ModelCompactionSummaryCandidateRequest, PromptCompactionCommit, PromptCompactionConfig,
    PromptCompactionError, PromptCompactionOutcome, PromptCompactionPlanV1,
    PromptCompactionPreCompactHookDecision, PromptCompactionScopeV1,
};
use crate::model::provider_polling::{
    build_provider_poll_payload_ref, build_provider_poll_runtime_job_id,
    parse_provider_poll_payload_ref, ProviderPollingRuntimePayload, PROVIDER_POLL_RUNTIME_JOB_KIND,
};
use crate::model::{
    GenerateResult, ModelClient, ModelSessionConfigStore, ToolCallEnvelope,
    DEFAULT_MODEL_CONTEXT_TOKENS, DEFAULT_MODEL_OUTPUT_TOKENS, PROMPT_COMPACTION_MAX_OUTPUT_TOKENS,
    PROMPT_COMPACTION_TRIGGER_HEADROOM_TOKENS, PROMPT_COMPACTION_USER_REPLAY_TOKENS,
};
use crate::runtime::context_window::{
    refresh_session_context_window, LIFECYCLE_HOOK_CONTEXT_META_KEY,
};
use crate::runtime::contracts::{
    new_turn_id, AgentRunInterventionChangedV1, AgentRunInterventionStatusV1,
    AgentRunInterventionV1, CheckpointRecord, EventVisibility, JsonMap, ProviderTokenUsageV1,
    RuntimeAgentRunIdentityV1, RuntimeAwaitJobCheckpointV1, RuntimeEvent, RuntimeJobWaitV1,
    RuntimeProcessState, RuntimeWaitChangedV1, RuntimeWaitStatusV1,
};
use crate::runtime::event::RuntimeEventProjection;
use crate::runtime::keys::external_context as runtime_external_context_keys;
use crate::runtime::keys::metadata as runtime_metadata_keys;
use crate::runtime::message_handler::{MessageHandler, MessageHandlerConfig};
use crate::runtime::query_loop::{AgentRunResourceUsageV1, AgentStateSnapshot};
use crate::runtime::subagent::{
    build_subagent_run_job, load_subagent_work_packet, subagent_work_packet_runtime_binding,
    AsyncSubagentLifecycleObserver, AsyncSubagentWorkerRunner, SubagentLifecycleHookEvent,
    SubagentLifecycleObserverFuture, SubagentRunJobRequest, SubagentSchedulerEvent,
    SubagentWorkPacketRuntimeBindingV1, SubagentWorkerRunFuture, SubagentWorkerRunOutcome,
    SubagentWorkerRunRequest, SUBAGENT_RUN_JOB_KIND,
};
use crate::runtime::subagent_contracts::{
    AgentRunContext, ContextTransferMode, DelegatedToolContractV1, HotView, OutputContract,
    ResultEnvelope, SubAgentWorkPacket, TaskBrief,
};
use crate::session::external_context::{
    ExternalContextObject, ExternalContextObjectLink, ExternalContextStorePort,
    EXTERNAL_CONTEXT_SCHEMA_VERSION,
};
use crate::session::manager::SessionManager;
#[cfg(test)]
use crate::session::reliability::ListRuntimeJobsRequest;
use crate::session::reliability::{
    RuntimeBackoffPolicy, RuntimeJobRecord, RuntimeJobStatus, RuntimeJobStorePort,
    ScheduleRuntimeJobRequest,
};
use crate::session::state::{
    ChatMessage, CompletedTurnProjectionV1, MessageRole, ModelMessageSemanticsV1,
    ModelToolCallStateV1, SessionStateSnapshot,
};
use crate::session::store::{
    AgentRuntimeSnapshotStorePort, RuntimeStore, RuntimeStoreTransactionPort,
    UpsertExternalContextAndScheduleJobRequest,
};
use crate::session::{
    canonical_session_record, AgentRunSessionState, SequencedSessionRecord, SessionLogRecord,
    SessionRecordType,
};
pub use crate::tool::concurrency::{
    normalize_tool_parallelism, ToolConcurrencyCoordinator, DEFAULT_TOOL_PARALLELISM,
    MAX_TOOL_PARALLELISM,
};
use crate::tool::layer::{
    extract_dynamic_tool_pending_poll, DynamicToolPendingPoll, ToolExecutionFact,
    ToolExecutionResult, ToolInvocationRequest, ToolLayer,
};
use crate::tool::permission::{evaluate_tool_action, PermissionDecision, ToolPermissionRequest};
use crate::tool::{
    canonicalize_tool_name, DynamicToolRegistry, ModelToolChoice, ModelToolDefinition,
    ToolContract, ToolErrorInfo, ToolFailureKind, ToolTurnBehavior,
};

const SYSTEM_PROMPT_MANIFEST_META_KEY: &str = runtime_metadata_keys::SYSTEM_PROMPT_MANIFEST;
const SUBAGENT_RESULT_PROJECTION_META_KEY: &str = runtime_metadata_keys::SUBAGENT_RESULT_PROJECTION;
const MESSAGE_SEMANTIC_KIND_META_KEY: &str = runtime_metadata_keys::MESSAGE_SEMANTIC_KIND;
const ACTIVE_OBJECTIVE_META_KEY: &str = runtime_metadata_keys::ACTIVE_OBJECTIVE;
const MESSAGE_SEMANTIC_USER_REQUEST: &str = "user_request";
const MESSAGE_SEMANTIC_TURN_SUPPLEMENT: &str = "turn_supplement";
const MESSAGE_SEMANTIC_TOOL_CONTINUATION: &str = "tool_continuation";
const MESSAGE_SEMANTIC_OUTPUT_TOKEN_RECOVERY: &str = "output_token_recovery";
const MESSAGE_SEMANTIC_ANSWER_NOW: &str = "answer_now";
const RUNTIME_PENDING_TOOL_BATCH_META_KEY: &str = "runtime_pending_tool_batch_v1_json";
const TERMINAL_TOOL_TRANSCRIPT_COMMITTED_META_PREFIX: &str =
    "runtime_terminal_tool_transcript_committed_v1";

struct ToolSafePointDispatcher<'a> {
    sink: Mutex<&'a mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send)>,
}

impl ToolSafePointDispatcher<'_> {
    fn commit(&self, safe_point: ToolSafePoint) -> Result<(), String> {
        let mut sink = self
            .sink
            .lock()
            .map_err(|_| "tool safe-point sink lock poisoned".to_string())?;
        (*sink)(safe_point)
    }
}
const PROMPT_COMPACTION_FAILURE_META_KEY: &str = runtime_metadata_keys::PROMPT_COMPACTION_FAILURE;
const PROMPT_COMPACTION_FAILURE_COUNT_META_KEY: &str =
    runtime_metadata_keys::PROMPT_COMPACTION_FAILURE_COUNT;
const PROMPT_COMPACTION_CIRCUIT_META_KEY: &str = runtime_metadata_keys::PROMPT_COMPACTION_CIRCUIT;
const PROMPT_COMPACTION_CIRCUIT_FAILURE_THRESHOLD: u32 = 3;
const CHECKPOINT_TOOL_REPORT_PREVIEW_CHARS: usize = 1_000;
const TOOL_EVIDENCE_ROLLUP_MIN_REPORTS: usize = 8;
const TOOL_EVIDENCE_ROLLUP_BYTES: usize = 256 * 1024;
const DEFAULT_PROVIDER_PROMPT_CACHE_RETENTION: &str = "24h";

pub fn completed_turn_projection_from_result(
    session_id: &str,
    agent_run_identity: &RuntimeAgentRunIdentityV1,
    result: &AgentRunResult,
) -> Result<CompletedTurnProjectionV1, String> {
    if session_id.trim().is_empty() {
        return Err("completed_turn_projection_session_id_required".to_string());
    }
    agent_run_identity.validate()?;
    let expected_continuation = match result.stop {
        AgentRunStop::Finalized => QueryContinuation::Finalize,
        AgentRunStop::TerminalTool => QueryContinuation::CompleteTerminalTool,
        _ => return Err("completed_turn_projection_requires_terminal_result".to_string()),
    };
    let final_response = result
        .turn_responses
        .last()
        .ok_or_else(|| "completed_turn_projection_final_response_missing".to_string())?;
    if final_response.continuation != expected_continuation {
        return Err("completed_turn_projection_final_continuation_mismatch".to_string());
    }
    if final_response.session_snapshot.session_id != session_id {
        return Err("completed_turn_projection_session_id_mismatch".to_string());
    }
    let mut expected_tool_call_ids = result
        .turn_responses
        .iter()
        .flat_map(|response| response.tool_results.iter())
        .map(|result| result.tool_call_id.clone())
        .collect::<Vec<_>>();
    expected_tool_call_ids.sort_unstable();
    if expected_tool_call_ids
        .windows(2)
        .any(|pair| pair[0] == pair[1])
    {
        return Err("completed_turn_projection_duplicate_tool_call_id".to_string());
    }
    CompletedTurnProjectionV1::new(
        agent_run_identity,
        result.stop.reason().to_string(),
        final_response.turn_id.clone(),
        expected_tool_call_ids,
    )
}

pub struct AgentRuntime<
    S: RuntimeStore
        + ExternalContextStorePort
        + RuntimeJobStorePort
        + RuntimeStoreTransactionPort
        + AgentRuntimeSnapshotStorePort
        + Clone,
> {
    checkpoint_store: TurnCheckpointStore<S>,
    runtime_store: S,
    session_manager: SessionManager<S>,
    message_handler: MessageHandler,
    tools_port: ToolLayer,
    tool_concurrency: ToolConcurrencyCoordinator,
    prompt_compaction_config: PromptCompactionConfig,
    active_agent_run_sessions: Mutex<HashSet<String>>,
    lifecycle_hooks: QueryLifecycleHookRuntime,
    model_input_image_resolver:
        Option<crate::model::prepared_prompt::SharedModelInputImageResolver>,
    config: AgentRuntimeConfig,
}

struct ActiveAgentRunGuard<'a> {
    registry: &'a Mutex<HashSet<String>>,
    session_id: String,
}

impl Drop for ActiveAgentRunGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut active) = self.registry.lock() {
            active.remove(self.session_id.as_str());
        }
    }
}

impl<
        S: RuntimeStore
            + ExternalContextStorePort
            + RuntimeJobStorePort
            + RuntimeStoreTransactionPort
            + AgentRuntimeSnapshotStorePort
            + Clone
            + Send
            + Sync
            + 'static,
    > AgentRuntime<S>
{
    pub fn new(
        store: S,
        tools_port: ToolLayer,
        config: AgentRuntimeConfig,
        tool_concurrency: ToolConcurrencyCoordinator,
    ) -> Self {
        let mut config = config;
        config.tool_parallelism = tool_concurrency.capacity();
        let message_handler = MessageHandler::new(MessageHandlerConfig {
            max_message_chars: config.max_message_chars,
        });
        let tools_port = tools_port.with_external_context_store(Arc::new(store.clone()));
        let prompt_compaction_config = PromptCompactionConfig {
            model_context_tokens: config.model_context_tokens,
            model_max_output_tokens: config.model_max_output_tokens,
            trigger_headroom_tokens: config.prompt_compaction_trigger_headroom_tokens,
            user_replay_tokens: config.prompt_compaction_user_replay_tokens,
            summary_max_tokens: config.prompt_compaction_summary_max_tokens,
        };
        Self {
            checkpoint_store: TurnCheckpointStore::new(store.clone()),
            runtime_store: store.clone(),
            session_manager: SessionManager::new(store.clone()),
            message_handler,
            tools_port,
            tool_concurrency,
            prompt_compaction_config,
            active_agent_run_sessions: Mutex::new(HashSet::new()),
            lifecycle_hooks: QueryLifecycleHookRuntime::empty(),
            model_input_image_resolver: None,
            config,
        }
    }

    #[cfg(test)]
    fn new_for_test(store: S, config: AgentRuntimeConfig) -> Self {
        let tool_concurrency = ToolConcurrencyCoordinator::new(config.tool_parallelism);
        Self::new(store, ToolLayer::new(), config, tool_concurrency)
    }

    #[cfg(test)]
    fn new_for_test_with_tools(
        store: S,
        tools_port: ToolLayer,
        config: AgentRuntimeConfig,
    ) -> Self {
        let tool_concurrency = ToolConcurrencyCoordinator::new(config.tool_parallelism);
        Self::new(store, tools_port, config, tool_concurrency)
    }

    pub fn prepare_completed_turn_projection(
        &self,
        session_id: &str,
        agent_run_identity: &RuntimeAgentRunIdentityV1,
        result: &AgentRunResult,
    ) -> Result<CompletedTurnProjectionV1, String> {
        let projection =
            completed_turn_projection_from_result(session_id, agent_run_identity, result)?;
        let mut session = self
            .session_manager
            .load_session(session_id)?
            .ok_or_else(|| "completed_turn_projection_session_snapshot_missing".to_string())?;
        if session.session_id != session_id {
            return Err("completed_turn_projection_snapshot_session_mismatch".to_string());
        }
        match &session.completed_turn {
            Some(existing) => {
                existing.validate()?;
                if existing == &projection {
                    Ok(projection)
                } else {
                    Err("completed_turn_projection_conflict".to_string())
                }
            }
            None => {
                session.completed_turn = Some(projection.clone());
                self.session_manager.save_session(&session)?;
                Ok(projection)
            }
        }
    }

    pub fn load_completed_turn_projection(
        &self,
        session_id: &str,
        agent_run_identity: &RuntimeAgentRunIdentityV1,
    ) -> Result<Option<CompletedTurnProjectionV1>, String> {
        if session_id.trim().is_empty() {
            return Err("completed_turn_projection_session_id_required".to_string());
        }
        agent_run_identity.validate()?;
        let Some(session) = self.session_manager.load_session(session_id)? else {
            return Ok(None);
        };
        if session.session_id != session_id {
            return Err("completed_turn_projection_snapshot_session_mismatch".to_string());
        }
        let Some(projection) = session.completed_turn else {
            return Ok(None);
        };
        projection.validate()?;
        if projection.agent_run_id != agent_run_identity.agent_run_id
            || projection.authorization_digest != agent_run_identity.authorization_digest
        {
            return Err("completed_turn_projection_identity_mismatch".to_string());
        }
        Ok(Some(projection))
    }

    pub fn acknowledge_completed_turn_projection(
        &self,
        session_id: &str,
        agent_run_identity: &RuntimeAgentRunIdentityV1,
    ) -> Result<(), String> {
        if session_id.trim().is_empty() {
            return Err("completed_turn_projection_session_id_required".to_string());
        }
        agent_run_identity.validate()?;
        let Some(mut session) = self.session_manager.load_session(session_id)? else {
            return Ok(());
        };
        if session.session_id != session_id {
            return Err("completed_turn_projection_snapshot_session_mismatch".to_string());
        }
        let Some(projection) = &session.completed_turn else {
            return Ok(());
        };
        projection.validate()?;
        if projection.agent_run_id != agent_run_identity.agent_run_id
            || projection.authorization_digest != agent_run_identity.authorization_digest
        {
            return Err("completed_turn_projection_identity_mismatch".to_string());
        }
        session.completed_turn = None;
        self.session_manager.save_session(&session)
    }

    fn acquire_active_agent_run(
        &self,
        session_id: &str,
    ) -> Result<ActiveAgentRunGuard<'_>, String> {
        let mut active = self
            .active_agent_run_sessions
            .lock()
            .map_err(|_| "active AgentRun registry lock poisoned".to_string())?;
        if !active.insert(session_id.to_string()) {
            return Err(format!(
                "Session already has an in-flight AgentRun: sessionId={session_id}"
            ));
        }
        Ok(ActiveAgentRunGuard {
            registry: &self.active_agent_run_sessions,
            session_id: session_id.to_string(),
        })
    }

    pub fn with_lifecycle_hooks(mut self, lifecycle_hooks: QueryLifecycleHookRuntime) -> Self {
        self.lifecycle_hooks = lifecycle_hooks;
        self
    }

    pub fn with_model_input_image_resolver(
        mut self,
        resolver: crate::model::prepared_prompt::SharedModelInputImageResolver,
    ) -> Self {
        self.model_input_image_resolver = Some(resolver);
        self
    }

    pub fn validate_subagent_tool_contracts(
        &self,
        binding: &SubagentWorkPacketRuntimeBindingV1,
    ) -> Result<(), String> {
        binding.validate_tool_contracts(&self.tools_port)
    }

    pub fn run_session_start_hook(
        &self,
        session_id: &str,
        target: QueryLifecycleHookStartTargetV1,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks
            .run_session_start(session_id, self.cwd_text(), target)
    }

    pub fn run_user_prompt_submit_hook(
        &self,
        session_id: &str,
        prompt: &str,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks
            .run_user_prompt_submit(session_id, self.cwd_text(), prompt)
    }

    pub fn run_pre_tool_use_hook(
        &self,
        session_id: &str,
        tool_name: &str,
        tool_input: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks
            .run_pre_tool_use(session_id, self.cwd_text(), tool_name, tool_input)
    }

    pub fn run_permission_request_hook(
        &self,
        session_id: &str,
        tool_name: &str,
        permission: &PermissionDecision,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks.run_permission_request(
            session_id,
            self.cwd_text(),
            tool_name,
            permission,
        )
    }

    pub fn run_post_tool_use_hook(
        &self,
        session_id: &str,
        tool_name: &str,
        tool_result: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks
            .run_post_tool_use(session_id, self.cwd_text(), tool_name, tool_result)
    }

    pub fn run_stop_hook(
        &self,
        session_id: &str,
        target: QueryLifecycleHookStopTargetV1,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks
            .run_stop(session_id, self.cwd_text(), target)
    }

    pub fn run_pre_compact_hook(
        &self,
        session_id: &str,
        payload: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks
            .run_pre_compact(session_id, self.cwd_text(), payload)
    }

    pub fn run_post_compact_hook(
        &self,
        session_id: &str,
        payload: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.lifecycle_hooks
            .run_post_compact(session_id, self.cwd_text(), payload)
    }

    fn cwd_text(&self) -> Option<String> {
        self.tools_port
            .cwd()
            .map(|path| path.to_string_lossy().to_string())
    }

    pub fn lifecycle_hook_diagnostics_projection(
        &self,
    ) -> Result<crate::extension::hooks::LifecycleHookDiagnosticsProjectionV1, String> {
        self.lifecycle_hooks.diagnostics_projection()
    }

    fn agent_composition_environment(
        &self,
    ) -> Result<crate::extension::composition::AgentCompositionEnvironmentV1, String> {
        let skill_catalog_hash = self.tools_port.skill_index().snapshot().catalog_hash;
        let skill_catalog_digest = format!("sha256:{skill_catalog_hash}");
        let plugin_activation_digest = self
            .config
            .plugin_activation_digest
            .clone()
            .map(Ok)
            .unwrap_or_else(|| {
                crate::extension::composition::empty_composition_digest("plugin_activation")
            })?;
        let execution_profile_digest = crate::runtime::canonical_json::sha256(
            "centaeris.session_runtime_execution_profile.v1",
            &serde_json::json!({
                "cwd": self.cwd_text(),
                "agentInstructionsHash": stable_text_hash(self.config.agent_instructions.as_str()),
                "maxMessageChars": self.config.max_message_chars,
                "maxRecoveryAttempts": self.config.max_recovery_attempts,
                "toolParallelism": self.config.tool_parallelism,
                "providerPromptCacheRetention": self.config.provider_prompt_cache_retention,
                "enablePromptCompaction": self.config.enable_prompt_compaction,
                "modelContextTokens": self.config.model_context_tokens,
                "modelMaxOutputTokens": self.config.model_max_output_tokens,
                "promptCompactionTriggerHeadroomTokens": self.config.prompt_compaction_trigger_headroom_tokens,
                "promptCompactionUserReplayTokens": self.config.prompt_compaction_user_replay_tokens,
                "promptCompactionSummaryMaxTokens": self.config.prompt_compaction_summary_max_tokens,
                "enableSystemPromptTemplate": self.config.enable_system_prompt_template,
                "enableToolUseSummary": self.config.enable_tool_use_summary,
                "allowedTools": self.config.allowed_tools,
            }),
        )?;
        Ok(
            crate::extension::composition::AgentCompositionEnvironmentV1 {
                tool_contracts: tool_projection::build_generate_tool_contracts(
                    self.tools_port.dynamic_tool_registry(),
                    self.config.allowed_tools.as_deref(),
                    self.tools_port.execution_host_kind(),
                )?,
                skill_catalog_digest,
                plugin_activation_digest,
                hook_composition_digest: self.lifecycle_hooks.composition_digest()?,
                execution_profile_digest,
                policy_version: "session_runtime.v1".to_string(),
                model_binding_override: self.config.resolved_model_binding.clone(),
            },
        )
    }
}

fn canonical_task_runtime_tool_name(tool_name: &str) -> Option<&'static str> {
    if tool_name == "task_output" {
        return Some("task_output");
    }
    None
}

fn is_task_runtime_tool_name(tool_name: &str) -> bool {
    canonical_task_runtime_tool_name(tool_name).is_some()
}

fn append_lifecycle_hook_context_messages<'a>(
    message_handler: &MessageHandler,
    session: &mut SessionStateSnapshot,
    contexts: impl IntoIterator<Item = &'a str>,
) {
    for context in contexts {
        let context = context.trim();
        if context.is_empty() {
            continue;
        }
        let mut metadata = JsonMap::new();
        metadata.insert(
            LIFECYCLE_HOOK_CONTEXT_META_KEY.to_string(),
            "true".to_string(),
        );
        message_handler.push_system_message(
            session,
            format!("[Lifecycle hook context]\n{context}").as_str(),
            metadata,
        );
    }
}

fn build_agent_state(generate_result: GenerateResult) -> AgentStateSnapshot {
    let mut state = empty_agent_state();
    state.generate_result = Some(generate_result);
    state
}

fn empty_agent_state() -> AgentStateSnapshot {
    AgentStateSnapshot {
        loop_count: 1,
        done_reason: None,
        pending_question_json: None,
        generate_result: None,
        tool_reports_json: vec![],
        process_events_json: vec![],
        transition_json: None,
        agent_run_resource_usage: AgentRunResourceUsageV1::default(),
        compression_stats_json: None,
        tool_use_summary: None,
        tool_operations_json: None,
        recovery_policy_trace_json: vec![],
        metadata: std::collections::HashMap::new(),
    }
}

fn attach_system_prompt_manifest(state: &mut AgentStateSnapshot, manifest_json: Option<&str>) {
    if let Some(raw) = manifest_json {
        state
            .metadata
            .insert(SYSTEM_PROMPT_MANIFEST_META_KEY.to_string(), raw.to_string());
    }
}

fn build_provider_prompt_cache_key(
    system_prompt: Option<&str>,
    tool_definitions: &[ModelToolDefinition],
    skill_catalog_hash: Option<&str>,
    agents_context_hash: Option<&str>,
    agent_instructions_hash: Option<&str>,
    config: &AgentRuntimeConfig,
) -> Result<Option<String>, String> {
    let Some(system_prompt) = system_prompt.map(str::trim).filter(|item| !item.is_empty()) else {
        return Ok(None);
    };
    let tool_schema_json = serde_json::to_string(tool_definitions)
        .map_err(|err| format!("serialize provider prompt cache tool schema failed: {err}"))?;
    let seed = serde_json::json!({
        "schema": "provider_prompt_cache_key_seed_v1",
        "systemPromptHash": stable_text_hash(system_prompt),
        "toolSchemaHash": stable_text_hash(tool_schema_json.as_str()),
        "skillCatalogHash": skill_catalog_hash,
        "agentsContextHash": agents_context_hash,
        "agentInstructionsHash": agent_instructions_hash,
        "systemPromptTemplateEnabled": config.enable_system_prompt_template,
        "systemPromptSchema": "system_prompt_v1",
    });
    Ok(Some(format!(
        "centaeris-provider-pcache-seed-v1:{}",
        stable_text_hash(seed.to_string().as_str())
    )))
}

pub fn canonical_session_record_from_runtime_event(
    event: &RuntimeEventProjection,
    task_id: &str,
) -> Result<Option<SessionLogRecord>, String> {
    event.validate()?;
    if task_id.trim().is_empty() {
        return Err("canonical session record taskId is required".to_string());
    }
    let (event_type, payload) = match event.event_type.as_str() {
        "Status"
            if event.payload.get("stage").and_then(Value::as_str)
                == Some("model_process_summary") =>
        {
            (
                SessionRecordType::PhaseEvent,
                json!({
                    "stage": "model_process_summary",
                    "message": event
                        .payload
                        .get("message")
                        .and_then(Value::as_str)
                        .filter(|value| !value.trim().is_empty())
                        .ok_or_else(|| "model process summary message is required".to_string())?,
                }),
            )
        }
        "PromptCompaction" if event.status == "done" => {
            let compaction = event
                .payload
                .get("compaction")
                .and_then(Value::as_object)
                .ok_or_else(|| "PromptCompaction payload.compaction is required".to_string())?;
            (
                SessionRecordType::Compaction,
                json!({
                    "compactionId": compaction
                        .get("compactionId")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "PromptCompaction compactionId is required".to_string())?,
                    "summaryMessageId": compaction
                        .get("summaryMessageId")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "PromptCompaction summaryMessageId is required".to_string())?,
                    "summaryMarkdown": compaction
                        .get("summaryMarkdown")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "PromptCompaction summaryMarkdown is required".to_string())?,
                    "firstKeptMessageId": compaction
                        .get("firstKeptMessageId")
                        .cloned()
                        .unwrap_or(Value::Null),
                    "createdReason": "context_pressure_threshold_reached",
                }),
            )
        }
        "Final" => {
            if !matches!(event.status.as_str(), "done" | "error") {
                return Err(format!(
                    "Final runtime event status is unsupported: {}",
                    event.status
                ));
            }
            (
                SessionRecordType::AssistantMessage,
                json!({
                    "messageId": format!("message:{}:assistant", event.turn_id),
                    "modelMarkdown": event
                        .payload
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "Final runtime event content is required".to_string())?,
                    "artifactRefs": event
                        .payload
                        .get("artifactRefs")
                        .cloned()
                        .unwrap_or_else(|| Value::Array(Vec::new())),
                    "status": event.status,
                }),
            )
        }
        _ => return Ok(None),
    };
    canonical_session_record(
        event.event_id.clone(),
        event_type,
        event.session_id.clone(),
        Some(event.turn_id.clone()),
        Some(task_id.to_string()),
        event.at_ms,
        payload,
    )
    .map(Some)
}

pub fn canonical_tool_call_event_id(session_id: &str, turn_id: &str, call_id: &str) -> String {
    events::stable_session_event_id("tool_call", &[session_id, turn_id, call_id])
}

#[expect(
    clippy::too_many_arguments,
    reason = "Session Log record constructor keeps exact durable fields explicit"
)]
pub fn canonical_tool_call_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    call: &ToolCallEnvelope,
    provider_id: &str,
    tool_contract_digest: &str,
    display_target: &str,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    let payload =
        canonical_tool_call_payload(call, provider_id, tool_contract_digest, display_target)?;
    canonical_session_record(
        canonical_tool_call_event_id(session_id, turn_id, call.id.as_str()),
        SessionRecordType::ToolCall,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        created_at_ms,
        payload,
    )
}

pub fn canonical_tool_result_record(
    session_id: &str,
    turn_id: &str,
    agent_run_id: &str,
    call: &ToolCallEnvelope,
    result: &ToolExecutionResult,
    created_at_ms: i64,
) -> Result<SessionLogRecord, String> {
    let payload = canonical_tool_result_payload(call, result)?;
    let completed_at_ms = if result.completed_at_ms > 0 {
        result.completed_at_ms
    } else {
        created_at_ms
    };
    canonical_session_record(
        events::stable_session_event_id(
            "tool_result",
            &[session_id, turn_id, result.tool_call_id.as_str()],
        ),
        SessionRecordType::ToolResult,
        session_id,
        Some(turn_id.to_string()),
        Some(agent_run_id.to_string()),
        completed_at_ms,
        payload,
    )
}

pub fn canonical_model_request_started_records(
    task_id: &str,
    started: &ModelRequestStartedV1,
    created_at_ms: i64,
) -> Result<Vec<SessionLogRecord>, String> {
    if task_id.trim().is_empty() {
        return Err("canonical model request taskId is required".to_string());
    }
    if started.session_id.trim().is_empty() || started.turn_id.trim().is_empty() {
        return Err("canonical model request identity is required".to_string());
    }
    let request_id = format!(
        "model_request:{}:{}:{}:{}:{}",
        started.session_id,
        started.turn_id,
        started.purpose.as_str(),
        started.loop_index,
        created_at_ms
    );
    Ok(vec![canonical_session_record(
        events::stable_session_event_id("model_request_started", &[request_id.as_str()]),
        SessionRecordType::ModelRequestStarted,
        started.session_id.as_str(),
        Some(started.turn_id.clone()),
        Some(task_id.to_string()),
        created_at_ms,
        json!({
            "requestId": request_id,
            "purpose": started.purpose.as_str(),
            "loopIndex": started.loop_index,
            "toolChoice": started.tool_choice,
            "maxOutputTokens": started.max_output_tokens,
            "promptCacheKey": started.provider_prompt_cache_key,
            "promptCacheRetention": started.provider_prompt_cache_retention,
            "preparedPromptSchema": started.prepared_prompt_schema,
            "contextTokenEstimate": started.context_token_estimate,
            "contextTokenBreakdown": started.context_token_breakdown,
            "agentComposition": started.agent_composition,
            "observations": started.observations,
        }),
    )?])
}

impl AgentRunSessionState {
    pub fn record_runtime_event(
        &mut self,
        event: &RuntimeEventProjection,
    ) -> Result<Option<SequencedSessionRecord>, String> {
        let Some(record) = canonical_session_record_from_runtime_event(event, self.agent_run_id())?
        else {
            return Ok(None);
        };
        if record.event_type == SessionRecordType::PhaseEvent
            && record
                .turn_id
                .as_deref()
                .is_some_and(|turn_id| self.has_phase_turn(turn_id))
        {
            return Ok(None);
        }
        if record.event_type == SessionRecordType::AssistantMessage
            && record
                .payload
                .get("messageId")
                .and_then(Value::as_str)
                .is_some_and(|message_id| self.assistant_is_final(message_id))
        {
            return Ok(None);
        }
        self.record(record).map(Some)
    }

    pub fn record_model_request_started(
        &mut self,
        started: &ModelRequestStartedV1,
        created_at_ms: i64,
    ) -> Result<Vec<SequencedSessionRecord>, String> {
        let mut candidate = self.clone();
        let records = canonical_model_request_started_records(
            candidate.agent_run_id(),
            started,
            created_at_ms,
        )?
        .into_iter()
        .map(|record| candidate.record(record))
        .collect::<Result<Vec<_>, _>>()?;
        *self = candidate;
        Ok(records)
    }

    pub fn record_tool_call(
        &mut self,
        turn_id: &str,
        call: &ToolCallEnvelope,
        provider_id: &str,
        tool_contract_digest: &str,
        display_target: &str,
        created_at_ms: i64,
    ) -> Result<Option<SequencedSessionRecord>, String> {
        if self.has_tool_call(call.id.as_str()) {
            return Ok(None);
        }
        self.record(canonical_tool_call_record(
            self.session_id(),
            turn_id,
            self.agent_run_id(),
            call,
            provider_id,
            tool_contract_digest,
            display_target,
            created_at_ms,
        )?)
        .map(Some)
    }

    pub fn record_tool_result(
        &mut self,
        turn_id: &str,
        call: &ToolCallEnvelope,
        result: &ToolExecutionResult,
        created_at_ms: i64,
    ) -> Result<Vec<SequencedSessionRecord>, String> {
        if !self.has_tool_call(call.id.as_str()) {
            return Err(format!(
                "Session tool_result has no durable tool_call: {}",
                result.tool_call_id
            ));
        }
        let mut candidate = self.clone();
        let mut records = Vec::new();
        if !candidate.has_tool_result(result.tool_call_id.as_str()) {
            records.push(candidate.record(canonical_tool_result_record(
                candidate.session_id(),
                turn_id,
                candidate.agent_run_id(),
                call,
                result,
                created_at_ms,
            )?)?);
        }
        records.extend(candidate.record_tool_facts_inner(turn_id, result, created_at_ms)?);
        *self = candidate;
        Ok(records)
    }

    pub fn record_tool_facts(
        &mut self,
        turn_id: &str,
        result: &ToolExecutionResult,
        created_at_ms: i64,
    ) -> Result<Vec<SequencedSessionRecord>, String> {
        let mut candidate = self.clone();
        let records = candidate.record_tool_facts_inner(turn_id, result, created_at_ms)?;
        *self = candidate;
        Ok(records)
    }

    fn record_tool_facts_inner(
        &mut self,
        turn_id: &str,
        result: &ToolExecutionResult,
        created_at_ms: i64,
    ) -> Result<Vec<SequencedSessionRecord>, String> {
        if result.facts.is_empty() {
            return Ok(Vec::new());
        }
        if !self.has_tool_result(result.tool_call_id.as_str()) {
            return Err(format!(
                "Session tool facts have no durable tool_result: {}",
                result.tool_call_id
            ));
        }
        if !result.result_state().is_success() {
            return Err(format!(
                "failed tool result cannot produce Session facts: {}",
                result.tool_call_id
            ));
        }
        let mut records = Vec::new();
        for fact in &result.facts {
            let (event_type, already_recorded) = match fact {
                ToolExecutionFact::ArtifactPublished(payload) => {
                    require_tool_fact_identity(payload, result, "toolCallId", false)?;
                    (
                        SessionRecordType::ArtifactPublished,
                        self.artifact_fact_matches(payload)?,
                    )
                }
                ToolExecutionFact::CitationRecorded(payload) => {
                    require_tool_fact_identity(payload, result, "sourceToolCallId", true)?;
                    (
                        SessionRecordType::CitationRecorded,
                        self.citation_fact_matches(payload)?,
                    )
                }
                ToolExecutionFact::ExternalEvidenceRef(payload) => (
                    SessionRecordType::ExternalEvidenceRef,
                    self.external_evidence_fact_matches(payload)?,
                ),
                ToolExecutionFact::FileFact(payload) => {
                    require_tool_fact_identity(payload, result, "toolCallId", true)?;
                    if !self.has_file_fact(payload)? {
                        return Err(format!(
                            "file mutation fact must be committed before tool execution: {}",
                            result.tool_call_id
                        ));
                    }
                    (SessionRecordType::FileFact, true)
                }
            };
            if !already_recorded {
                records.push(self.event_for_turn(
                    turn_id,
                    event_type,
                    fact.payload().clone(),
                    created_at_ms,
                )?);
            }
        }
        Ok(records)
    }
}

fn require_tool_fact_identity(
    payload: &Value,
    result: &ToolExecutionResult,
    call_id_field: &str,
    require_tool_name: bool,
) -> Result<(), String> {
    if payload.get(call_id_field).and_then(Value::as_str) != Some(result.tool_call_id.as_str())
        || require_tool_name
            && payload
                .get(if call_id_field == "sourceToolCallId" {
                    "sourceToolName"
                } else {
                    "toolName"
                })
                .and_then(Value::as_str)
                != Some(result.tool_name.as_str())
    {
        return Err(format!(
            "tool fact identity mismatch: {}",
            result.tool_call_id
        ));
    }
    Ok(())
}

pub fn canonical_tool_call_payload(
    call: &ToolCallEnvelope,
    provider_id: &str,
    tool_contract_digest: &str,
    display_target: &str,
) -> Result<Value, String> {
    let normalized_input = serde_json::from_str::<Value>(call.args_json.as_str())
        .map_err(|error| format!("semantic tool_call input is invalid JSON: {error}"))?;
    let normalized_input_object = normalized_input
        .as_object()
        .ok_or_else(|| "semantic tool_call input must be an object".to_string())?;
    let display_target = if call.name == "bash" {
        bash_tool_call_display(normalized_input_object)?.0
    } else {
        display_target.trim().to_string()
    };
    if display_target.is_empty() {
        return Err("semantic tool_call displayTarget is required".to_string());
    }
    if provider_id.trim().is_empty() {
        return Err("semantic tool_call providerId is required".to_string());
    }
    let digest = tool_contract_digest
        .strip_prefix("sha256:")
        .unwrap_or_default();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("semantic tool_call toolContractDigest is invalid".to_string());
    }
    Ok(json!({
        "callId": call.id,
        "toolName": call.name,
        "toolContractDigest": tool_contract_digest,
        "providerId": provider_id,
        "normalizedInput": normalized_input,
        "displayTarget": display_target,
    }))
}

pub(crate) fn bash_tool_call_display(
    input: &serde_json::Map<String, Value>,
) -> Result<(String, String, Option<String>), String> {
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "bash tool_call command is required".to_string())?;
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let display_target = description
        .clone()
        .unwrap_or_else(|| compact_text(command, 256));
    Ok((display_target, command.to_string(), description))
}

pub fn canonical_tool_result_payload(
    call: &ToolCallEnvelope,
    result: &ToolExecutionResult,
) -> Result<Value, String> {
    if result.tool_call_id != call.id || result.tool_name != call.name {
        return Err(format!(
            "tool receipt identity mismatch: callId={} resultCallId={} callTool={} resultTool={}",
            call.id, result.tool_call_id, call.name, result.tool_name
        ));
    }
    let operations = project_tool_operations_json(std::slice::from_ref(result))
        .ok_or_else(|| "Core tool operation projection is missing".to_string())?;
    let operations = serde_json::from_str::<Value>(operations.as_str())
        .map_err(|error| format!("Core tool operation projection is invalid: {error}"))?;
    if !operations.is_array() {
        return Err("Core tool operation projection must be an array".to_string());
    }
    let capture = crate::tool::layer::tool_result_capture(result);
    let model_input_images =
        tool_context_writer::tool_result_model_input_image_sources(&result.details)?;
    Ok(json!({
        "callId": result.tool_call_id,
        "toolName": result.tool_name,
        "resultState": result.result_state().as_str(),
        "modelContent": result.content,
        "fullOutputPath": capture.full_output_path,
        "outputStartByte": capture.output_start_byte,
        "outputByteLength": capture.output_byte_length,
        "outputComplete": capture.output_complete,
        "summary": summarize_tool_result(result),
        "operations": operations,
        "modelInputImages": model_input_images,
        "latencyMs": result.latency_ms,
    }))
}

fn push_model_assistant_semantics_message(
    message_handler: &MessageHandler,
    session: &mut SessionStateSnapshot,
    generate_result: &GenerateResult,
) {
    if generate_result.tool_calls.is_empty() && generate_result.reasoning_content.is_none() {
        return;
    }
    message_handler.push_model_assistant_message(
        session,
        generate_result.content.as_str(),
        JsonMap::new(),
        build_model_assistant_semantics(generate_result),
    );
}

fn ensure_model_assistant_semantics_message(
    message_handler: &MessageHandler,
    session: &mut SessionStateSnapshot,
    generate_result: &GenerateResult,
) -> Result<bool, String> {
    if generate_result.tool_calls.is_empty() && generate_result.reasoning_content.is_none() {
        return Ok(false);
    }
    let expected_semantics = build_model_assistant_semantics(generate_result);
    let expected_call_ids = generate_result
        .tool_calls
        .iter()
        .map(|call| call.id.as_str())
        .collect::<HashSet<_>>();
    let mut matching_message = None;
    for message in &session.messages {
        let ModelMessageSemanticsV1::Assistant { tool_calls, .. } =
            session.model_semantics_for(message.message_id.as_str())?
        else {
            continue;
        };
        if !tool_calls
            .iter()
            .any(|call| expected_call_ids.contains(call.id.as_str()))
        {
            continue;
        }
        if matching_message.is_some() {
            return Err("assistant_tool_call_batch_was_persisted_more_than_once".to_string());
        }
        matching_message = Some(message);
    }
    if let Some(message) = matching_message {
        let actual_semantics = session.model_semantics_for(message.message_id.as_str())?;
        if actual_semantics != &expected_semantics
            || message.role != MessageRole::Assistant
            || message.content != generate_result.content.trim()
        {
            return Err("assistant_tool_call_batch_persistence_conflict".to_string());
        }
        return Ok(false);
    }
    push_model_assistant_semantics_message(message_handler, session, generate_result);
    Ok(true)
}

fn terminal_tool_transcript_commit_key(tool_call_id: &str) -> String {
    format!("{TERMINAL_TOOL_TRANSCRIPT_COMMITTED_META_PREFIX}:{tool_call_id}")
}

fn mark_terminal_tool_transcript_committed(
    session: &mut SessionStateSnapshot,
    generate_result: &GenerateResult,
) -> Result<(), String> {
    let [call] = generate_result.tool_calls.as_slice() else {
        return Err("terminal_tool_transcript_commit_requires_one_call".to_string());
    };
    session.metadata.insert(
        terminal_tool_transcript_commit_key(call.id.as_str()),
        "true".to_string(),
    );
    Ok(())
}

fn find_model_assistant_semantics_tool_call(
    session: &SessionStateSnapshot,
    tool_call_id: &str,
) -> Option<crate::runtime::contracts::ToolCall> {
    for message in session.messages.iter().rev() {
        let Some(ModelMessageSemanticsV1::Assistant { tool_calls, .. }) =
            session.model_semantics.get(message.message_id.as_str())
        else {
            continue;
        };
        for call in tool_calls {
            if call.id != tool_call_id {
                continue;
            }
            return Some(crate::runtime::contracts::ToolCall {
                tool_call_id: tool_call_id.to_string(),
                tool_name: call.name.clone(),
                args_json: call.args_json.clone(),
            });
        }
    }
    None
}

fn generate_result_from_persisted_tool_batch(
    session: &SessionStateSnapshot,
    required_tool_call_ids: &[String],
) -> Result<GenerateResult, String> {
    if required_tool_call_ids.is_empty() {
        return Err("runtime wait requires tool call identities".to_string());
    }
    let required = required_tool_call_ids.iter().collect::<HashSet<_>>();
    if required.len() != required_tool_call_ids.len() {
        return Err("runtime wait has duplicate tool call identity".to_string());
    }
    let mut found = None;
    for message in &session.messages {
        let Some(ModelMessageSemanticsV1::Assistant {
            reasoning_content,
            tool_calls,
        }) = session.model_semantics.get(message.message_id.as_str())
        else {
            continue;
        };
        if !required
            .iter()
            .all(|id| tool_calls.iter().any(|call| &call.id == *id))
        {
            continue;
        }
        if found.is_some() {
            return Err("runtime wait tool batch identity is ambiguous".to_string());
        }
        found = Some(GenerateResult {
            content: message.content.clone(),
            tool_calls: tool_calls
                .iter()
                .map(|call| ToolCallEnvelope {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    args_json: call.args_json.clone(),
                })
                .collect(),
            reasoning_content: reasoning_content.clone(),
            input_tokens: None,
            total_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        });
    }
    found.ok_or_else(|| "runtime wait tool batch is missing from Session transcript".to_string())
}

fn build_model_assistant_semantics(generate_result: &GenerateResult) -> ModelMessageSemanticsV1 {
    ModelMessageSemanticsV1::Assistant {
        reasoning_content: generate_result.reasoning_content.clone(),
        tool_calls: generate_result
            .tool_calls
            .iter()
            .map(|call| ModelToolCallStateV1 {
                id: call.id.clone(),
                name: call.name.clone(),
                args_json: call.args_json.clone(),
            })
            .collect(),
    }
}

fn parse_json_text(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(trimmed).ok()
}

fn event_status_from_tool_status(status: &str) -> &'static str {
    match status {
        "ok" | "skipped" | "blocked" | "cancelled" | "discarded" => "done",
        _ => normalize_session_event_status(status),
    }
}

fn normalize_session_event_status(status: &str) -> &'static str {
    let lowered = status.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "done" | "ok" | "completed" | "finalized" | "success" | "skipped" | "cancelled"
        | "discarded" => "done",
        "error" | "failed" | "timeout" | "rejected" | "denied" => "error",
        _ => "running",
    }
}

fn model_input_already_persisted(
    session: &SessionStateSnapshot,
    session_id: &str,
    turn_id: &str,
) -> bool {
    let driver_user_message_id = prompt_projection::driver_user_message_id(session_id, turn_id);
    session
        .messages
        .iter()
        .any(|message| message.message_id == driver_user_message_id)
}

fn read_active_objective_state(session: &SessionStateSnapshot) -> Option<ActiveObjectiveState> {
    session
        .metadata
        .get(ACTIVE_OBJECTIVE_META_KEY)
        .and_then(|raw| serde_json::from_str::<ActiveObjectiveState>(raw).ok())
        .filter(|state| !state.objective.trim().is_empty())
}

fn write_active_objective_state(
    session: &mut SessionStateSnapshot,
    state: &ActiveObjectiveState,
) -> Result<(), String> {
    let encoded = serde_json::to_string(state)
        .map_err(|err| format!("serialize active objective failed: {err}"))?;
    session
        .metadata
        .insert(ACTIVE_OBJECTIVE_META_KEY.to_string(), encoded);
    Ok(())
}

fn active_objective_id(session_id: &str, turn_id: &str) -> String {
    format!("active_objective:{session_id}:{turn_id}")
}

fn update_active_objective_for_message(
    session: &mut SessionStateSnapshot,
    semantic_kind: &str,
    turn_id: &str,
    user_message: &str,
) -> Result<String, String> {
    let trimmed = user_message.trim();
    if semantic_kind == MESSAGE_SEMANTIC_USER_REQUEST {
        let now = now_ms();
        let state = ActiveObjectiveState {
            schema: "active_objective_v1".to_string(),
            objective_id: active_objective_id(session.session_id.as_str(), turn_id),
            source_turn_id: turn_id.to_string(),
            objective: compact_text(trimmed, 2_000),
            root_user_message: compact_text(trimmed, 2_000),
            supplements: vec![],
            updated_at_ms: now,
        };
        write_active_objective_state(session, &state)?;
        return Ok(trimmed.to_string());
    }

    let now = now_ms();
    let mut state = read_active_objective_state(session).unwrap_or_else(|| ActiveObjectiveState {
        schema: "active_objective_v1".to_string(),
        objective_id: active_objective_id(session.session_id.as_str(), turn_id),
        source_turn_id: turn_id.to_string(),
        objective: compact_text(trimmed, 2_000),
        root_user_message: compact_text(trimmed, 2_000),
        supplements: vec![],
        updated_at_ms: now,
    });
    if semantic_kind == MESSAGE_SEMANTIC_TURN_SUPPLEMENT && !trimmed.is_empty() {
        state.supplements.push(ActiveObjectiveSupplement {
            content: compact_text(trimmed, 1_200),
            at_ms: now_ms(),
        });
        if state.supplements.len() > 8 {
            let extra = state.supplements.len().saturating_sub(8);
            state.supplements.drain(0..extra);
        }
        state.updated_at_ms = now_ms();
        write_active_objective_state(session, &state)?;
    }
    Ok(effective_user_message_from_active_objective(
        semantic_kind,
        trimmed,
        &state,
    ))
}

fn effective_user_message_from_active_objective(
    semantic_kind: &str,
    user_message: &str,
    state: &ActiveObjectiveState,
) -> String {
    let objective = state.objective.trim();
    if semantic_kind == MESSAGE_SEMANTIC_TURN_SUPPLEMENT {
        return compact_text(
            format!(
                "原始任务：{}\n\n本轮补充要求：{}",
                objective,
                user_message.trim()
            )
            .as_str(),
            3_000,
        );
    }
    if semantic_kind == MESSAGE_SEMANTIC_TOOL_CONTINUATION {
        return compact_text(objective, 2_000);
    }
    if semantic_kind == MESSAGE_SEMANTIC_OUTPUT_TOKEN_RECOVERY {
        return user_message.trim().to_string();
    }
    user_message.trim().to_string()
}

fn attach_runtime_metadata_to_state(
    state: &mut AgentStateSnapshot,
    session: &SessionStateSnapshot,
    _config: &AgentRuntimeConfig,
) {
    for key in [
        PROMPT_COMPACTION_FAILURE_META_KEY,
        PROMPT_COMPACTION_CIRCUIT_META_KEY,
    ] {
        if let Some(value) = session.metadata.get(key) {
            state.metadata.insert(key.to_string(), value.clone());
        }
    }
}

fn build_subagent_query_loop_request(
    req: &SubagentWorkerRunRequest,
    config: &AgentRuntimeSubagentRunnerConfig,
) -> Result<AgentRunRequest, String> {
    let work_packet = decode_subagent_work_packet(&req.work_packet.content_json)?;
    let user_message = build_subagent_user_message(&work_packet, req);
    let runtime_scope = build_subagent_query_loop_runtime_scope(req);
    Ok(AgentRunRequest {
        session_id: work_packet.run_context.branch_id.clone(),
        initial_turn_id: work_packet.run_context.turn_id.clone(),
        user_message,
        agent_run_identity: config.agent_run_identity.clone(),
        runtime_scope,
        resume_from_turn_id: None,
        auto_continue_after_resume_wait: config.auto_continue_after_resume_wait,
    })
}

fn build_subagent_query_loop_runtime_scope(
    req: &SubagentWorkerRunRequest,
) -> PromptCompactionScopeV1 {
    PromptCompactionScopeV1 {
        agent_scope: "subagent".to_string(),
        parent_session_id: Some(req.lifecycle.session_id.clone()),
        runtime_job_id: Some(req.job.job_id.clone()),
    }
}

fn poll_loop_cancellation<F>(cancellation_probe: Option<&F>) -> Result<Option<String>, String>
where
    F: Fn() -> Result<Option<String>, String> + ?Sized,
{
    let Some(probe) = cancellation_probe else {
        return Ok(None);
    };
    probe().map(|reason| {
        reason.map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                "cancelled".to_string()
            } else {
                trimmed.to_string()
            }
        })
    })
}

fn decode_subagent_work_packet(value: &Value) -> Result<SubAgentWorkPacket, String> {
    let candidate = value.get("workPacket").unwrap_or(value).clone();
    let packet = serde_json::from_value::<SubAgentWorkPacket>(candidate)
        .map_err(|error| format!("invalid subagent work packet: {error}"))?;
    packet.validate_for_agent_runtime()?;
    Ok(packet)
}

fn build_subagent_user_message(
    work_packet: &SubAgentWorkPacket,
    req: &SubagentWorkerRunRequest,
) -> String {
    let mut sections = vec![
        "You are running as a Centaeris subagent worker. Complete only the delegated work packet and return a concise structured result for the parent agent.".to_string(),
        format!("Subagent id: {}", req.lifecycle.subagent_id),
        format!("Parent turn id: {}", req.lifecycle.parent_turn_id),
        format!("Agent depth: {}", work_packet.run_context.depth),
        format!("Objective: {}", work_packet.task_brief.objective),
    ];

    if !work_packet.task_brief.success_criteria.is_empty() {
        sections.push(format!(
            "Success criteria:\n{}",
            bullet_lines(&work_packet.task_brief.success_criteria)
        ));
    }
    if !work_packet.task_brief.constraints.is_empty() {
        sections.push(format!(
            "Constraints:\n{}",
            bullet_lines(&work_packet.task_brief.constraints)
        ));
    }
    if let Some(output_hint) = work_packet.task_brief.output_hint.as_deref() {
        if !output_hint.trim().is_empty() {
            sections.push(format!("Output hint: {}", output_hint.trim()));
        }
    }
    if !work_packet.hot_view.summary.trim().is_empty() {
        sections.push(format!(
            "Parent context summary:\n{}",
            work_packet.hot_view.summary.trim()
        ));
    }
    if !work_packet.object_refs.is_empty() {
        let refs = work_packet
            .object_refs
            .iter()
            .map(|item| {
                format!(
                    "- {} ({:?}){}",
                    item.ref_id,
                    item.kind,
                    item.summary
                        .as_deref()
                        .filter(|summary| !summary.trim().is_empty())
                        .map(|summary| format!(": {}", summary.trim()))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("Context refs:\n{refs}"));
    }
    sections.push(
        "Return your final answer directly. Do not claim parent-level completion unless the delegated objective is complete."
            .to_string(),
    );
    sections.join("\n\n")
}

fn bullet_lines(items: &[String]) -> String {
    items
        .iter()
        .filter_map(|item| {
            let trimmed = item.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(format!("- {trimmed}"))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn subagent_worker_outcome_from_query_loop_response(
    response: AgentRunResult,
) -> SubagentWorkerRunOutcome {
    if let AgentRunStop::Cancelled(reason) = &response.stop {
        return SubagentWorkerRunOutcome::Cancelled {
            reason: if reason.trim().is_empty() {
                "subagent_cancelled".to_string()
            } else {
                reason.clone()
            },
        };
    }
    let output_refs = response
        .turn_responses
        .iter()
        .map(|item| format!("turn:{}", item.turn_id))
        .collect::<Vec<_>>();
    let final_summary = response
        .turn_responses
        .iter()
        .rev()
        .find_map(|item| latest_assistant_message(item.session_snapshot.messages.as_slice()));

    match final_summary {
        Some(summary) => SubagentWorkerRunOutcome::Succeeded {
            summary,
            output_refs,
        },
        None => SubagentWorkerRunOutcome::Failed {
            error: format!(
                "subagent query loop ended without assistant final text: {}",
                response.stop.reason()
            ),
            retry: None,
        },
    }
}

fn latest_assistant_message(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, MessageRole::Assistant))
        .map(|message| message.content.trim().to_string())
        .filter(|content| !content.is_empty())
}

fn collect_recovery_trace(traces: &mut Vec<String>, report: &ToolExecutionResult, stage: &str) {
    if report.status != "error" {
        return;
    }
    let Some(error_text) = report.error.as_ref().map(|e| e.model_message.as_str()) else {
        return;
    };
    let Some((policy, priority)) = classify_recovery_policy(error_text) else {
        return;
    };
    traces.push(
        json!({
            "policy": policy,
            "priority": priority,
            "stage": stage,
            "action": "fallback_finalize",
            "meta": {
                "tool": report.tool_name,
                "error": error_text,
            },
            "timestamp": now_ms(),
        })
        .to_string(),
    );
}

fn classify_recovery_policy(error_text: &str) -> Option<(&'static str, i32)> {
    let normalized = error_text.to_ascii_lowercase();
    if normalized.contains("prompt too long")
        || normalized.contains("context length")
        || normalized.contains("413")
    {
        return Some(("prompt_too_long", 100));
    }
    if normalized.contains("max_output_tokens") || normalized.contains("max output tokens") {
        return Some(("max_output_tokens", 90));
    }
    if normalized.contains("rate limit")
        || normalized.contains("429")
        || normalized.contains("overloaded")
    {
        return Some(("rate_limit", 80));
    }
    if normalized.contains("model") && normalized.contains("unavailable") {
        return Some(("model_unavailable", 70));
    }
    None
}

fn now_ms() -> i64 {
    crate::runtime::contracts::current_timestamp_ms()
}

fn runtime_waiting_transition(
    session_id: &str,
    turn_id: &str,
    wait_checkpoint: &RuntimeAwaitJobCheckpointV1,
    at_ms: i64,
) -> Result<(CheckpointRecord, RuntimeEvent, RuntimeWaitChangedV1), String> {
    wait_checkpoint.validate()?;
    let changed = RuntimeWaitChangedV1 {
        schema: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
        continuation_id: wait_checkpoint.continuation_id.clone(),
        agent_run_id: wait_checkpoint.agent_run_id.clone(),
        status: RuntimeWaitStatusV1::Waiting,
        transition_reason: "runtime_jobs_pending".to_string(),
        at_ms,
    };
    changed.validate()?;
    let checkpoint = CheckpointRecord {
        checkpoint_id: format!("checkpoint:{}", wait_checkpoint.continuation_id),
        kind: crate::runtime::contracts::CheckpointKindV1::Wait,
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
        status: "waiting".to_string(),
        done_reason: Some("runtime_job".to_string()),
        updated_at_ms: at_ms,
        payload_json: serde_json::to_string(wait_checkpoint)
            .map_err(|error| format!("serialize runtime wait checkpoint failed: {error}"))?,
    };
    let event = RuntimeEvent {
        event_id: format!("runtime_wait:{}:waiting", wait_checkpoint.continuation_id),
        session_id: session_id.to_string(),
        task_id: Some(turn_id.to_string()),
        event_type: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
        at_ms,
        visibility: EventVisibility::User,
        payload_json: serde_json::to_string(&changed)
            .map_err(|error| format!("serialize runtime wait event failed: {error}"))?,
    };
    Ok((checkpoint, event, changed))
}

#[cfg(test)]
#[path = "runtime/agent_runtime_tests.rs"]
mod tests;
