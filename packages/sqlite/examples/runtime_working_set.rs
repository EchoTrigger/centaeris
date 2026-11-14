//! Frozen-input acceptance through the public, SQLite-backed production AgentRuntime.
//! Receives one JSON command on stdin and emits one machine-readable result.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use centaeris_core::execution::{
    ExecutionCommandRequest, ExecutionError, ExecutionFileSystemError,
    ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
    ExecutionHostBinding, ExecutionHostCommandOutput, ExecutionHostHealth, ExecutionHostKind,
    ExecutionHostMode, ExecutionHostRunner, ExecutionHostStatus, ExecutionPolicy,
};
use centaeris_core::extension::skills::SkillCatalogLoadConfig;
use centaeris_core::model::prompt::PromptCompactionScopeV1;
use centaeris_core::model::{
    GenerateResult, ModelClient, ModelClientError, ModelClientErrorKind, ModelClientFuture,
    ModelClientRequest, ModelClientResponse, ModelProviderKind, ModelProviderRegistry,
    ModelSessionConfig, ModelSessionConfigStore, OpenAiCompatibleModelClient, ToolCallEnvelope,
};
use centaeris_core::runtime::contracts::{JsonMap, RuntimeAgentRunIdentityV1};
use centaeris_core::runtime::{
    AgentRunInitialInput, AgentRunRequest, AgentRuntime, AgentRuntimeConfig, ModelRequestPurposeV1,
    ToolConcurrencyCoordinator, ToolSafePoint, TurnControl,
};
use centaeris_core::session::manager::SessionManager;
use centaeris_core::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, SessionStateSnapshot,
};
use centaeris_core::tool::layer::ToolLayer;
use centaeris_model_http::{build_model_http_client, ReqwestJsonHttpTransport};
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use serde_json::{json, Value};

const PROVIDER: &str = "command-code-goat.default";
const MODEL: &str = "deepseek/deepseek-v4.1-flash";
const CASES: [&str; 12] = [
    "normal-answer",
    "normal-history",
    "normal-tool",
    "compact-goal",
    "compact-path",
    "compact-failure",
    "repeat-constraints",
    "repeat-progress",
    "repeat-original",
    "tool-chain",
    "steer-owner",
    "reopen-context",
];

struct ReadOnlyRunner {
    reads: Arc<AtomicUsize>,
    proof: PathBuf,
}
impl ExecutionHostRunner for ReadOnlyRunner {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::LocalProcess
    }
    fn status(&self, _: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        Ok(ExecutionHostStatus {
            kind: self.kind(),
            policy_enforced: true,
            health: ExecutionHostHealth::Ready,
            detail: None,
        })
    }
    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        let is_proof_read = Path::new(&request.model_path) == self.proof
            && matches!(
                &request.operation,
                ExecutionFileSystemOperation::ReadFile { .. }
            );
        let result =
            centaeris_core::execution::run_policy_scoped_execution_file_system_operation(request);
        if is_proof_read
            && matches!(&result, Ok(ExecutionFileSystemOutput::ReadFile(output)) if output.bytes == b"READ_OK\n")
        {
            self.reads.fetch_add(1, Ordering::SeqCst);
        }
        result
    }
    fn run_host_command(
        &self,
        _: Option<&str>,
        _: ExecutionCommandRequest,
        _: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        Err(ExecutionError::Denied {
            reason: "acceptance fixture permits read-only filesystem tools".into(),
        })
    }
}

struct Config(ModelSessionConfig);
impl ModelSessionConfigStore for Config {
    fn get_session_config(&self, _: &str) -> Result<Option<ModelSessionConfig>, String> {
        Ok(Some(self.0.clone()))
    }
}

struct Client {
    live: Option<OpenAiCompatibleModelClient<ReqwestJsonHttpTransport>>,
    main: AtomicUsize,
    summary: AtomicUsize,
    usage: Mutex<Value>,
    remaining_usd: Mutex<f64>,
    input_rate: f64,
    output_rate: f64,
    answer: Value,
    proof: String,
    require_read: bool,
    steer: Option<TurnControl>,
    steering_observed: AtomicUsize,
}
impl ModelClient for Client {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let is_summary = request.prepared_prompt.tool_definitions.is_empty();
            let counter = if is_summary {
                &self.summary
            } else {
                &self.main
            };
            let maximum = if is_summary { 2 } else { 6 };
            let count = counter
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |previous| {
                    (previous < maximum).then_some(previous + 1)
                })
                .map_err(|_| {
                    ModelClientError::new(
                        ModelClientErrorKind::InvalidRequest,
                        "evaluation dispatch limit reached",
                        false,
                    )
                })?
                + 1;
            if !is_summary && count == 1 {
                if let Some(control) = &self.steer {
                    control.enqueue_supplement_with("Correction: the owner is mira-corrected. Return that corrected owner in the final JSON.".into(), || Ok(()))
                        .map_err(|_| ModelClientError::new(ModelClientErrorKind::InvalidRequest, "evaluation steering admission failed", false))?;
                }
            }
            if !is_summary
                && self.steer.is_some()
                && serde_json::to_string(&request.prepared_prompt.messages)
                    .map(|value| value.contains("mira-corrected"))
                    .unwrap_or(false)
            {
                self.steering_observed.store(1, Ordering::SeqCst);
            }
            let mut reservation = 0.0;
            if self.live.is_some() {
                if !request.prepared_prompt.input_images.is_empty() {
                    return Err(ModelClientError::new(
                        ModelClientErrorKind::InvalidRequest,
                        "evaluation supports text-only billing bounds",
                        false,
                    ));
                }
                // Conservative text-token ceiling: UTF-8 bytes plus chat framing.
                let bytes = serde_json::to_vec(&request.prepared_prompt)
                    .map_err(|_| {
                        ModelClientError::new(
                            ModelClientErrorKind::InvalidRequest,
                            "evaluation request encoding failed",
                            false,
                        )
                    })?
                    .len();
                reservation = ((bytes + 256 * (request.prepared_prompt.messages.len() + 1)) as f64
                    * self.input_rate
                    + f64::from(request.prepared_prompt.max_output_tokens) * self.output_rate)
                    / 1_000_000.0;
                let mut remaining = self.remaining_usd.lock().map_err(|_| {
                    ModelClientError::new(
                        ModelClientErrorKind::InvalidRequest,
                        "evaluation budget lock failed",
                        false,
                    )
                })?;
                if reservation > *remaining {
                    return Err(ModelClientError::new(
                        ModelClientErrorKind::InvalidRequest,
                        "evaluation cost reservation exceeds remaining budget",
                        false,
                    ));
                }
                *remaining -= reservation;
            }
            let response = if let Some(live) = &self.live {
                live.generate(request).await
            } else {
                let tool_calls = if self.require_read && !is_summary && count == 1 {
                    vec![ToolCallEnvelope {
                        id: "evaluation-read".into(),
                        name: "read".into(),
                        args_json: json!({"path": self.proof}).to_string(),
                    }]
                } else {
                    Vec::new()
                };
                let mut answer = self.answer.clone();
                if self.steer.is_some() && self.steering_observed.load(Ordering::SeqCst) == 0 {
                    answer["owner"] = json!("mira");
                }
                let content = if is_summary {
                    format!("Verified facts: {}", self.answer)
                } else {
                    answer.to_string()
                };
                Ok(ModelClientResponse {
                    request_timings: None,
                    generate_result: GenerateResult {
                        content,
                        tool_calls,
                        reasoning_content: None,
                        continuation_reasoning_content: None,
                        input_tokens: Some(i64::from(request.context_token_estimate)),
                        total_tokens: Some(i64::from(request.context_token_estimate) + 24),
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    },
                    provider_request_id: None,
                    provider_latency_ms: None,
                    provider_attempts: 1,
                })
            };
            if let Ok(result) = &response {
                let input = result.generate_result.input_tokens.unwrap_or(0).max(0);
                let output = result
                    .generate_result
                    .total_tokens
                    .unwrap_or(input)
                    .saturating_sub(input)
                    .max(0);
                let cost = (input as f64 * self.input_rate + output as f64 * self.output_rate)
                    / 1_000_000.0;
                if self.live.is_some() && result.generate_result.input_tokens.is_some() {
                    if let Ok(mut remaining) = self.remaining_usd.lock() {
                        *remaining += reservation - cost;
                    }
                }
                if let Ok(mut usage) = self.usage.lock() {
                    usage["inputTokens"] =
                        json!(usage["inputTokens"].as_i64().unwrap_or(0) + input);
                    usage["outputTokens"] =
                        json!(usage["outputTokens"].as_i64().unwrap_or(0) + output);
                    if let Some(requests) = usage["requests"].as_array_mut() {
                        requests.push(json!({"turnId":request.turn_id,"summary":is_summary,
                            "estimatedInputTokens":request.context_token_estimate,
                            "actualInputTokens":result.generate_result.input_tokens,
                            "cacheHitTokens":result.generate_result.prompt_cache_hit_tokens,
                            "cacheMissTokens":result.generate_result.prompt_cache_miss_tokens,
                            "providerRequestId":result.provider_request_id,"providerAttempts":result.provider_attempts}));
                    }
                }
            } else if let Ok(mut usage) = self.usage.lock() {
                if let Some(requests) = usage["requests"].as_array_mut() {
                    requests.push(json!({"turnId":request.turn_id,"summary":is_summary,
                        "estimatedInputTokens":request.context_token_estimate,"outcome":"error",
                        "reservedCostUsd":reservation}));
                }
            }
            response
        })
    }
}

fn message(id: String, role: MessageRole, content: String) -> ChatMessage {
    let mut metadata = JsonMap::new();
    if role == MessageRole::User {
        metadata.insert("message_semantic_kind".into(), "user_request".into());
    }
    ChatMessage {
        message_id: id,
        role,
        content,
        created_at_ms: 1,
        metadata,
    }
}
fn append(state: &mut SessionStateSnapshot, value: ChatMessage) {
    let semantics = if value.role == MessageRole::Assistant {
        ModelMessageSemanticsV1::Assistant {
            reasoning_content: None,
            tool_calls: Vec::new(),
        }
    } else {
        ModelMessageSemanticsV1::Plain
    };
    state
        .model_semantics
        .insert(value.message_id.clone(), semantics);
    state.messages.push(value);
}
fn performance_seed(id: &str, history: usize) -> SessionStateSnapshot {
    let mut state = SessionStateSnapshot::new(id.into(), 1);
    for index in 0..history - 9 {
        append(
            &mut state,
            message(
                format!("cold-{index}"),
                MessageRole::Assistant,
                format!("Historical completed item {index}: retained immutable source facts."),
            ),
        );
    }
    for index in 0..8 {
        append(
            &mut state,
            message(
                format!("live-{index}"),
                if index % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                format!("Current working-set item {index}: preserve the fixed active scope."),
            ),
        );
    }
    let mut summary = message(
        "seed-summary".into(),
        MessageRole::System,
        "Completed history summarized. The current task is to read the proof file.".into(),
    );
    summary
        .metadata
        .insert("kind".into(), "context_compaction".into());
    summary
        .metadata
        .insert("compaction_id".into(), "frozen-seed".into());
    summary
        .metadata
        .insert("first_kept_message_id".into(), "live-0".into());
    append(&mut state, summary);
    state
}
fn expected(case: &str) -> Value {
    let mut facts = json!({"project": format!("project-{case}"), "owner": if case == "steer-owner" {"mira-corrected"} else {"mira"},
        "path": format!("src/{case}.rs"), "error": "storage receipt unconfirmed", "action": "query original execution; do not repeat the write"});
    match case {
        "compact-path" => facts["path"] = json!("src/路径/HTTPClient.rs"),
        "compact-failure" => {
            facts["error"] = json!("EACCES: read failed; no write was authorized");
            facts["action"] = json!("request read authorization; never retry a write");
        }
        "repeat-constraints" => {
            facts["action"] = json!("read-only analysis; no network; no file writes")
        }
        "repeat-progress" => {
            facts["error"] = json!("step 1 confirmed; step 2 failed; step 3 not run");
            facts["action"] = json!("inspect step 2 receipt before continuing");
        }
        "repeat-original" => facts["project"] = json!("preserve-the-original-user-goal"),
        _ => {}
    }
    facts
}
fn quality_seed(id: &str, case: &str) -> SessionStateSnapshot {
    let mut state = SessionStateSnapshot::new(id.into(), 1);
    append(
        &mut state,
        message(
            "task-origin".into(),
            MessageRole::User,
            "Maintain the recorded project facts and respect its execution constraint.".into(),
        ),
    );
    let mut facts = expected(case);
    if case == "steer-owner" {
        facts["owner"] = json!("mira");
    }
    if case == "repeat-progress" {
        facts["error"] = json!("step 2 still running (old observation)");
    }
    append(
        &mut state,
        message(
            "verified-facts".into(),
            MessageRole::Assistant,
            format!("These are verified task facts: {facts}"),
        ),
    );
    for index in 0..48 {
        append(&mut state, message(format!("completed-{index}"), MessageRole::Assistant, format!("Completed unrelated investigation item {index}; no change to the verified task facts.")));
    }
    append(
        &mut state,
        message(
            "checkpoint-boundary".into(),
            MessageRole::User,
            "The historical investigation is complete. Retain its facts for continuation.".into(),
        ),
    );
    state
}

fn tools(root: &Path, reads: Arc<AtomicUsize>) -> Result<ToolLayer, String> {
    let binding = ExecutionHostBinding::new(
        ExecutionHostMode::Local,
        Arc::new(ReadOnlyRunner {
            reads,
            proof: root.join("proof.txt"),
        }),
        root.to_path_buf(),
        ExecutionPolicy::read_only_no_network(root),
    )?;
    ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig {
            cwd: Some(root.to_path_buf()),
            ..Default::default()
        },
        Arc::new(binding),
    )
}
fn runtime(
    store: SqliteRuntimeStore,
    layer: ToolLayer,
    timings: Arc<Mutex<Vec<Value>>>,
) -> Result<AgentRuntime<SqliteRuntimeStore>, String> {
    let config = AgentRuntimeConfig {
        allowed_tools: Some(vec!["read".into()]),
        ..Default::default()
    };
    Ok(AgentRuntime::new(
        Arc::new(store.job_store(129).map_err(|error| error.to_string())?),
        store,
        layer,
        config,
        ToolConcurrencyCoordinator::new(1),
    )
    .with_request_preparation_observer(Arc::new(move |timing| {
        if let Ok(mut values) = timings.lock() {
            values.push(
                json!({"stage":format!("{:?}", timing.stage), "turnId":timing.turn_id,
                "elapsedMs":timing.elapsed.as_secs_f64()*1000.0,"completed":timing.completed}),
            );
        }
    })))
}
fn live_client(
    session_id: &str,
) -> Result<OpenAiCompatibleModelClient<ReqwestJsonHttpTransport>, String> {
    if std::env::var("COMMAND_CODE_API_KEY")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err("COMMAND_CODE_API_KEY is required for paid evaluation".into());
    }
    let mut registry = ModelProviderRegistry::new();
    let mut provider = registry
        .get(PROVIDER)
        .ok_or("evaluation provider missing")?
        .clone();
    // Retries are owned by the bounded evaluation; no hidden transport dispatches.
    provider.default_max_retries = Some(0);
    registry.insert_user_defined(provider);
    Ok(OpenAiCompatibleModelClient::new(
        registry,
        ReqwestJsonHttpTransport::for_session(session_id, PROVIDER, build_model_http_client)?,
    ))
}
fn model_config(real: bool) -> Config {
    Config(ModelSessionConfig {
        provider_kind: if real {
            ModelProviderKind::CommandCode
        } else {
            ModelProviderKind::Local
        },
        provider_id: if real {
            PROVIDER.into()
        } else {
            "local.default".into()
        },
        model: if real {
            MODEL.into()
        } else {
            "working-set-fixed".into()
        },
        api_base: real.then(|| "https://api.commandcode.ai/provider/v1".into()),
        timeout_ms: 180_000,
        max_retries: 0,
        max_output_tokens: Some(32_768),
        thinking_mode: real.then(|| "high".into()),
        ..Default::default()
    })
}
fn integer(request: &Value, field: &str, default: usize) -> usize {
    request[field]
        .as_u64()
        .map(|value| value as usize)
        .unwrap_or(default)
}

async fn execute(request: &Value, index: usize) -> Result<Value, String> {
    let mode = request["mode"].as_str().ok_or("mode required")?;
    let real = request["real"].as_bool().unwrap_or(false);
    let case = request["case"].as_str().unwrap_or("normal-tool");
    if !["perf", "quality"].contains(&mode) || (mode == "perf" && real) || !CASES.contains(&case) {
        return Err("invalid evaluation mode or case".into());
    }
    let root = PathBuf::from(request["root"].as_str().ok_or("root required")?);
    let workspace = request["workspaceRoot"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("workspace"));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    std::fs::write(workspace.join("proof.txt"), "READ_OK\n").map_err(|error| error.to_string())?;
    let id = format!("working-set-{mode}-{case}-{index}");
    let path = root.join(format!("{id}.sqlite"));
    if path.exists() {
        return Err("evaluation database already exists; use a fresh sample root".into());
    }
    let store = SqliteRuntimeStore::new(&path)?;
    let manager = SessionManager::new(store.clone());
    let seed = if mode == "perf" {
        let history = integer(request, "history", 1000);
        if ![1000, 10000].contains(&history) {
            return Err("performance history must be 1000 or 10000".into());
        }
        performance_seed(&id, history)
    } else {
        quality_seed(&id, case)
    };
    manager.save_session(&seed)?;
    let readings = Arc::new(AtomicUsize::new(0));
    let layer = tools(&workspace, readings.clone())?;
    let timings = Arc::new(Mutex::new(Vec::new()));
    let mut engine = runtime(store.clone(), layer.clone(), timings.clone())?;
    let control = TurnControl::new();
    let budget = request["budgetUsd"].as_f64().unwrap_or(0.0);
    let input_rate = request["inputUsdPerMillion"].as_f64().unwrap_or(0.0);
    let output_rate = request["outputUsdPerMillion"].as_f64().unwrap_or(0.0);
    if real
        && (![budget, input_rate, output_rate]
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
            || budget > 20.0)
    {
        return Err("positive verified billing rates and budget <=20 required".into());
    }
    let answer = if mode == "perf" {
        json!({"proof":"READ_OK"})
    } else {
        expected(case)
    };
    let require_read =
        mode == "perf" || ["normal-tool", "tool-chain", "steer-owner"].contains(&case);
    let client = Client {
        live: if real { Some(live_client(&id)?) } else { None },
        main: AtomicUsize::new(0),
        summary: AtomicUsize::new(0),
        usage: Mutex::new(json!({"inputTokens":0,"outputTokens":0,"requests":[]})),
        remaining_usd: Mutex::new(budget),
        input_rate,
        output_rate,
        answer: answer.clone(),
        proof: workspace.join("proof.txt").to_string_lossy().into_owned(),
        require_read,
        steer: (case == "steer-owner").then(|| control.clone()),
        steering_observed: AtomicUsize::new(0),
    };
    let config = model_config(real);
    let rounds = if case.starts_with("repeat-") {
        2
    } else if case.starts_with("compact-") || case == "reopen-context" {
        1
    } else {
        0
    };
    let mut compactions = Vec::new();
    for round in 0..rounds {
        if round > 0 {
            let mut current = manager
                .load_session(&id)?
                .ok_or("state missing after compaction")?;
            for item in 0..8 {
                append(
                    &mut current,
                    message(
                        format!("more-{item}"),
                        MessageRole::Assistant,
                        format!("Additional completed work {item}; original facts unchanged."),
                    ),
                );
            }
            if case == "repeat-progress" {
                append(
                    &mut current,
                    message(
                        "progress-update".into(),
                        MessageRole::Assistant,
                        format!(
                            "Latest confirmed progress supersedes only the old error field: {}",
                            answer["error"]
                        ),
                    ),
                );
            }
            append(
                &mut current,
                message(
                    "next-checkpoint".into(),
                    MessageRole::User,
                    "Retain the original task facts again.".into(),
                ),
            );
            manager.save_session(&current)?;
        }
        compactions.push(
            engine
                .compact_session_online_with_model_client_and_tool_safe_point_async(
                    &id,
                    &format!("checkpoint-{round}"),
                    &client,
                    &config,
                    &mut |_| Ok(()),
                )
                .await?,
        );
    }
    if case == "reopen-context" {
        drop(engine);
        engine = runtime(SqliteRuntimeStore::new(&path)?, layer, timings.clone())?;
    }
    timings.lock().map_err(|_| "timing lock failed")?.clear();
    let prompt = if mode == "perf" {
        format!(
            "Read {} exactly once using read, then return JSON {{\"proof\":\"READ_OK\"}} only.",
            client.proof
        )
    } else {
        format!("Using the verified historical task facts and any later user correction, return ONLY a JSON object with string fields project, owner, path, error, action. Preserve the exact historical values. {}", if require_read {format!("First read {} exactly once; do not modify any files.",client.proof)} else {"Do not call tools.".into()})
    };
    let started = Instant::now();
    let mut first_request_prepared_ms = None;
    let mut safe_point = |point| {
        if let ToolSafePoint::ModelRequestStarted(request) = point {
            if request.purpose() == ModelRequestPurposeV1::Main
                && first_request_prepared_ms.is_none()
            {
                first_request_prepared_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
        Ok(())
    };
    let result = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            AgentRunRequest {
                session_id: id.clone(),
                initial_turn_id: format!("turn-{id}"),
                initial_input: AgentRunInitialInput::UserMessage(prompt),
                agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
                    agent_run_id: format!("evaluation-agent-{id}"),
                    execution_id: format!("evaluation-execution-{id}"),
                    authorization_digest: format!("sha256:{}", "a".repeat(64)),
                }),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &client,
            &config,
            &mut |_| {},
            &|| Ok(None),
            &control,
            &mut safe_point,
        )
        .await;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let (reply, stop, error) = match result {
        Ok(result) => (
            result.final_reply.unwrap_or_default(),
            result.stop.reason().to_string(),
            None,
        ),
        Err(error) => (String::new(), "error".into(), Some(error)),
    };
    let parsed = serde_json::from_str::<Value>(&reply).ok();
    let matched = answer
        .as_object()
        .ok_or("expected object")?
        .iter()
        .all(|(key, value)| parsed.as_ref().and_then(|result| result.get(key)) == Some(value));
    let proof_unchanged = std::fs::read_to_string(workspace.join("proof.txt"))
        .map_err(|error| error.to_string())?
        == "READ_OK\n";
    let read_count = readings.load(Ordering::SeqCst);
    let stored = manager
        .load_session(&id)?
        .ok_or("final working state missing")?;
    let stages = timings.lock().map_err(|_| "timing lock failed")?.clone();
    let prepare_ms: f64 = stages
        .iter()
        .map(|stage| stage["elapsedMs"].as_f64().unwrap_or(0.0))
        .sum();
    Ok(
        json!({"schema":"runtime.working_set.sample.v1","mode":mode,"case":case,"index":index,"real":real,
        "providerId":if real {PROVIDER}else{"local.default"},"model":if real{MODEL}else{"fixed"},"stop":stop,
        "error":error,"reply":reply,"expected":answer,"passed":matched && proof_unchanged && (!require_read || read_count==1) && compactions.iter().all(|value|*value),
        "elapsedMs":elapsed_ms,"firstRequestPreparedMs":first_request_prepared_ms,
        "preparationStageWorkSumMs":prepare_ms,"stages":stages,"compactions":compactions,
        "mainCalls":client.main.load(Ordering::SeqCst),"summaryCalls":client.summary.load(Ordering::SeqCst),"proofReads":read_count,
        "steerInputObserved":client.steering_observed.load(Ordering::SeqCst)==1,
        "activeMessages":stored.context_window.len(),"retainedSourceMessages":stored.messages.len(),
        "activeContextBytes":serde_json::to_vec(&stored.context_window).map_err(|error|error.to_string())?.len(),
        "retainedSnapshotBytes":serde_json::to_vec(&stored).map_err(|error|error.to_string())?.len(),
        "retainedSourceBytes":serde_json::to_vec(&stored.messages).map_err(|error|error.to_string())?.len(),
        "usage":client.usage.lock().map_err(|_|"usage lock failed")?.clone(),
        "costUpperUsd":budget-*client.remaining_usd.lock().map_err(|_|"budget lock failed")?,
        "budgets":{"contextTokens":200000,"maxOutputTokens":32768,"summaryMaxTokens":16384,"headroomTokens":32768,"userReplayTokens":20000}}),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut raw = String::new();
    let result = match std::io::stdin().read_to_string(&mut raw) {
        Ok(_) => match serde_json::from_str::<Value>(&raw) {
            Ok(request) => {
                let warmup = integer(&request, "warmup", 0);
                let samples = integer(&request, "samples", 1);
                if warmup > 20
                    || samples == 0
                    || samples > 100
                    || (request["real"] == true && (warmup != 0 || samples != 1))
                {
                    Err("evaluation sample bounds invalid".to_string())
                } else {
                    let mut values = Vec::new();
                    let mut error = None;
                    for index in 0..warmup + samples {
                        match execute(&request, index).await {
                            Ok(mut value) => {
                                value["warmup"] = json!(index < warmup);
                                values.push(value);
                            }
                            Err(reason) => {
                                error = Some(reason);
                                break;
                            }
                        }
                    }
                    Ok(
                        json!({"schema":"runtime.working_set.result.v1","samples":values,"error":error}),
                    )
                }
            }
            Err(_) => Err("invalid evaluation command JSON".into()),
        },
        Err(error) => Err(error.to_string()),
    };
    let mut output = match result {
        Ok(value) => value,
        Err(error) => json!({"schema":"runtime.working_set.result.v1","error":error,"samples":[]}),
    }
    .to_string();
    if let Ok(key) = std::env::var("COMMAND_CODE_API_KEY") {
        if !key.is_empty() {
            output = output.replace(&key, "[REDACTED]");
        }
    }
    println!("{output}");
}
