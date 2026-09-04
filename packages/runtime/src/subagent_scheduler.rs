use crate::agent_runs;
use crate::agent_runtime;
use crate::http_transport::ReqwestJsonHttpTransport;
use crate::mcp;
use crate::message_log;
use crate::runtime_config;
use crate::runtime_rpc_transport::EventWriter;
use crate::sessions;
use centaeris_core::model::{
    AnthropicMessagesModelClient, OpenAiCompatibleModelClient, OpenAiResponsesModelClient, WireApi,
};
use centaeris_core::runtime::contracts::current_timestamp_ms;
use centaeris_core::runtime::subagent::{
    load_subagent_work_packet_async, run_due_subagent_jobs_with_worker_pool_async,
    subagent_work_packet_runtime_binding, AsyncSubagentWorkerRunner, RunDueSubagentJobsRequest,
    SubagentWorkPacketRuntimeBindingV1, SubagentWorkerPoolPolicy, SubagentWorkerRunFuture,
    SubagentWorkerRunOutcome, SubagentWorkerRunRequest, SUBAGENT_RUN_JOB_KIND,
};
use centaeris_core::runtime::{
    build_subagent_scheduler_runtime_event,
    persist_subagent_result_projection_from_scheduler_events, AgentRuntimeConfig,
    AgentRuntimeSubagentRunnerConfig, ModelClientSubagentRunner, QueryLifecycleSubagentObserver,
    ToolSafePoint, ToolSafePointCommitPort, TurnUpdate,
};
use centaeris_core::session::reliability::{
    ListRuntimeJobsRequest, RuntimeJobRecord, RuntimeJobStatus,
};
use centaeris_core::session::store::RuntimeStoreActor;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_SUBAGENT_SCHEDULER_LEASE_MS: u64 = 120_000;
const DEFAULT_SUBAGENT_MAX_PARALLELISM: usize = 3;
const SUBAGENT_QUEUE_SCAN_LIMIT: usize = 64;
const BACKGROUND_WORKER_IDLE_MS: u64 = 250;

pub(crate) async fn run_background_worker(event_writer: EventWriter) {
    loop {
        match next_queued_parent_session().await {
            Ok(Some(session_id)) => {
                match run_parent_batch(session_id.as_str(), event_writer.clone()).await {
                    Ok(()) => continue,
                    Err(error) => eprintln!("centaeris Agent background worker failed: {error}"),
                }
            }
            Ok(None) => {}
            Err(error) => eprintln!("centaeris Agent background worker scan failed: {error}"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(BACKGROUND_WORKER_IDLE_MS)).await;
    }
}

async fn next_queued_parent_session() -> Result<Option<String>, String> {
    let store = agent_runtime::agent_runtime_store_actor()?;
    store
        .reclaim_expired_runtime_job_leases(current_timestamp_ms())
        .await?;
    let jobs = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: None,
            branch_id: None,
            limit: SUBAGENT_QUEUE_SCAN_LIMIT,
            offset: 0,
        })
        .await?;
    for job in jobs {
        let parent_session_id = job
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("Agent runtime job session missing: {}", job.job_id))?;
        let work_packet = load_subagent_work_packet_async(&store, &job).await?;
        let binding = subagent_work_packet_runtime_binding(&work_packet, &job)?;
        if agent_session_is_ready(&job, &binding)? {
            return Ok(Some(parent_session_id.to_string()));
        }
    }
    Ok(None)
}

async fn run_parent_batch(session_id: &str, event_writer: EventWriter) -> Result<(), String> {
    let started_at_ms = current_timestamp_ms();
    let cwd = PathBuf::from(sessions::cwd_for_session_id(session_id)?);
    let worker_id = format!("electron-agent-worker-{}", std::process::id());
    let store = agent_runtime::agent_runtime_store_actor()?;
    let queued_jobs =
        queued_subagent_jobs(&store, session_id, DEFAULT_SUBAGENT_MAX_PARALLELISM).await?;
    if queued_jobs.is_empty() {
        return Ok(());
    }
    let mut bindings = HashMap::new();
    for job in queued_jobs {
        let packet = load_subagent_work_packet_async(&store, &job).await?;
        let binding = subagent_work_packet_runtime_binding(&packet, &job)?;
        if !agent_session_is_ready(&job, &binding)? {
            break;
        }
        bindings.insert(job.job_id.clone(), binding);
    }
    if bindings.is_empty() {
        return Ok(());
    }
    let runtime_config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {})?;
    let observer_runtime =
        agent_runtime::build_agent_runtime(agent_runtime::AgentRuntimeBuildRequest {
            store: store.clone(),
            session_id: session_id.to_string(),
            concurrency_scope_id: session_id.to_string(),
            cwd: cwd.clone(),
            execution_owner: worker_id.clone(),
            bash_path: runtime_config.bash_path.as_deref().map(PathBuf::from),
            runtime_config: subagent_engine_config(&runtime_config, None),
            native_plugin_activation: None,
            execution_cancellation_probe: None,
            file_mutation_commit_port: None,
        })?;
    let observer = QueryLifecycleSubagentObserver::new(&observer_runtime);
    let runner = ElectronSubagentRunner {
        store: store.clone(),
        parent_session_id: session_id.to_string(),
        cwd,
        event_writer: event_writer.clone(),
    };
    let run_request = RunDueSubagentJobsRequest {
        now_ms: started_at_ms,
        worker_id: worker_id.clone(),
        session_id: Some(session_id.to_string()),
        limit: bindings.len(),
        lease_ms: DEFAULT_SUBAGENT_SCHEDULER_LEASE_MS,
        started_at_ms,
        finished_at_ms: current_timestamp_ms(),
    };
    let result = run_due_subagent_jobs_with_worker_pool_async(
        &store,
        &runner,
        &observer,
        run_request,
        SubagentWorkerPoolPolicy {
            max_parallelism: DEFAULT_SUBAGENT_MAX_PARALLELISM,
        },
    )
    .await?;
    let _projected = persist_subagent_result_projection_from_scheduler_events(
        &store,
        session_id,
        result.events.as_slice(),
    )?;
    for item in &result.results {
        let binding = bindings.get(item.job_id.as_str()).ok_or_else(|| {
            format!(
                "Agent runtime binding missing after execution: {}",
                item.job_id
            )
        })?;
        persist_agent_session_terminal_projection(
            &store,
            &event_writer,
            item.job_id.as_str(),
            binding.child_session_id.as_str(),
            binding.child_turn_id.as_str(),
        )
        .await?;
    }
    emit_parent_subagent_updates(
        &event_writer,
        session_id,
        &bindings,
        result.events.as_slice(),
    )
}

async fn persist_agent_session_terminal_projection(
    store: &centaeris_core::session::store::RuntimeStoreActor,
    event_writer: &EventWriter,
    runtime_job_id: &str,
    child_session_id: &str,
    child_turn_id: &str,
) -> Result<(), String> {
    let job = store
        .get_runtime_job(runtime_job_id)
        .await?
        .ok_or_else(|| format!("Agent runtime job missing: {runtime_job_id}"))?;
    let (status, content) = match job.status {
        RuntimeJobStatus::Succeeded => {
            let result_ref = job
                .output_refs
                .first()
                .ok_or_else(|| format!("Agent resultRef missing after success: {}", job.job_id))?;
            let object = store
                .load_external_context_object(result_ref.as_str())
                .await?
                .ok_or_else(|| format!("Agent result object missing: {result_ref}"))?;
            ("succeeded", object.content)
        }
        RuntimeJobStatus::Failed | RuntimeJobStatus::DeadLettered => (
            "failed",
            job.last_error
                .clone()
                .unwrap_or_else(|| "Agent failed without an error message.".to_string()),
        ),
        RuntimeJobStatus::Cancelled => (
            "cancelled",
            job.last_error
                .clone()
                .unwrap_or_else(|| "Agent was cancelled.".to_string()),
        ),
        _ => return Ok(()),
    };
    let at_ms = job.updated_at_ms.max(current_timestamp_ms());
    agent_runtime::close_incomplete_tool_calls_for_agent_run(
        child_session_id,
        job.job_id.as_str(),
    )?;
    message_log::append_assistant_message(
        child_session_id,
        child_turn_id,
        Some(job.job_id.as_str()),
        content.as_str(),
        if status == "succeeded" {
            "done"
        } else {
            "error"
        },
        at_ms,
    )?;
    let task = message_log::append_agent_run_terminal(
        child_session_id,
        child_turn_id,
        job.job_id.as_str(),
        status,
        job.last_error.as_deref(),
        at_ms,
    )?;
    agent_runtime::emit_agent_run_terminal_payload(event_writer, &agent_runs::into_summary(&task))
}

fn emit_parent_subagent_updates(
    event_writer: &EventWriter,
    session_id: &str,
    bindings: &HashMap<String, SubagentWorkPacketRuntimeBindingV1>,
    events: &[centaeris_core::runtime::subagent::SubagentSchedulerEvent],
) -> Result<(), String> {
    for scheduler_event in events {
        let binding = bindings
            .get(scheduler_event.job_id.as_str())
            .ok_or_else(|| {
                format!(
                    "Agent runtime binding missing while emitting event: {}",
                    scheduler_event.job_id
                )
            })?;
        let turn_id = scheduler_event.parent_turn_id.as_str();
        let bridge = build_subagent_scheduler_runtime_event(session_id, turn_id, scheduler_event);
        agent_runtime::emit_background_turn_update_blocking(
            event_writer,
            binding.parent_agent_run_id.as_str(),
            session_id,
            TurnUpdate::RuntimeEvent { event: bridge },
        )?;
    }
    Ok(())
}

async fn queued_subagent_jobs(
    store: &RuntimeStoreActor,
    session_id: &str,
    limit: usize,
) -> Result<Vec<RuntimeJobRecord>, String> {
    let jobs = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some(session_id.to_string()),
            branch_id: None,
            limit,
            offset: 0,
        })
        .await?;
    Ok(jobs)
}

fn agent_session_is_ready(
    job: &RuntimeJobRecord,
    binding: &SubagentWorkPacketRuntimeBindingV1,
) -> Result<bool, String> {
    let Some((parent_session_id, runtime_job_id)) =
        sessions::find_agent_runtime_binding(binding.child_session_id.as_str())?
    else {
        return Ok(false);
    };
    if Some(parent_session_id.as_str()) != job.session_id.as_deref() || runtime_job_id != job.job_id
    {
        return Err(format!(
            "Agent session binding mismatch: sessionId={} runtimeJobId={}",
            binding.child_session_id, job.job_id
        ));
    }
    let projection = message_log::project_session_log(binding.child_session_id.as_str())?;
    Ok(projection.agent_runs.iter().any(|task| {
        task.agent_run_id == job.job_id
            && task.turn_id == binding.child_turn_id
            && matches!(task.status.as_str(), "queued" | "running")
    }))
}

struct ElectronSubagentRunner {
    store: RuntimeStoreActor,
    parent_session_id: String,
    cwd: PathBuf,
    event_writer: EventWriter,
}

impl AsyncSubagentWorkerRunner for ElectronSubagentRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            match self.run(req).await {
                Ok(outcome) => outcome,
                Err(error) => SubagentWorkerRunOutcome::Failed { error, retry: None },
            }
        })
    }
}

impl ElectronSubagentRunner {
    async fn run(&self, req: SubagentWorkerRunRequest) -> Result<SubagentWorkerRunOutcome, String> {
        let binding = subagent_work_packet_runtime_binding(&req.work_packet, &req.job)?;
        if req.job.session_id.as_deref() != Some(self.parent_session_id.as_str()) {
            return Err(format!(
                "Agent runtime job parent mismatch: jobId={} expected={}",
                req.job.job_id, self.parent_session_id
            ));
        }
        let persisted_binding = sessions::agent_runtime_binding(binding.child_session_id.as_str())?
            .ok_or_else(|| {
                format!(
                    "Agent child session is not a subagent session: {}",
                    binding.child_session_id
                )
            })?;
        if persisted_binding != (self.parent_session_id.clone(), req.job.job_id.clone()) {
            return Err(format!(
                "Agent child session binding mismatch: {}",
                binding.child_session_id
            ));
        }
        message_log::append_agent_turn_running(
            binding.child_session_id.as_str(),
            req.job.job_id.as_str(),
        )?;

        let runtime_config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {})?;
        let auto_continue_after_resume_wait = runtime_config.auto_continue_after_resume_wait;
        let agent_run_identity = agent_runtime::native_agent_run_identity(
            req.job.job_id.as_str(),
            binding.child_session_id.as_str(),
            self.cwd.as_path(),
        )?;
        let native_plugin_activation = mcp::connect_enabled_plugins().await?;
        let mut engine_config =
            subagent_engine_config(&runtime_config, Some(binding.allowed_tools.clone()));
        engine_config.plugin_activation_digest = Some(native_plugin_activation.digest.clone());
        let runtime =
            agent_runtime::build_agent_runtime(agent_runtime::AgentRuntimeBuildRequest {
                store: self.store.clone(),
                session_id: binding.child_session_id.clone(),
                concurrency_scope_id: self.parent_session_id.clone(),
                cwd: self.cwd.clone(),
                execution_owner: req.job.job_id.clone(),
                bash_path: runtime_config.bash_path.as_deref().map(PathBuf::from),
                runtime_config: engine_config,
                native_plugin_activation: Some(native_plugin_activation),
                execution_cancellation_probe: None,
                file_mutation_commit_port: Some(agent_runtime::file_mutation_commit_port(
                    binding.child_session_id.as_str(),
                    binding.child_turn_id.as_str(),
                    req.job.job_id.as_str(),
                )),
            })?;
        runtime.validate_subagent_tool_contracts(&binding)?;
        let (model_config, registry) =
            agent_runtime::model_session_config_and_registry(&runtime_config)?;
        let wire_api = agent_runtime::model_config_wire_api(&registry, &model_config)?;
        let config_store = agent_runtime::SingleModelSessionConfigStore {
            session_id: binding.child_session_id.clone(),
            config: model_config,
        };
        let runner_config = AgentRuntimeSubagentRunnerConfig {
            auto_continue_after_resume_wait: Some(auto_continue_after_resume_wait),
            agent_run_identity: Some(agent_run_identity),
        };
        let transport = ReqwestJsonHttpTransport::new()?;
        let stream_sink = {
            let event_writer = self.event_writer.clone();
            let runtime_job_id = req.job.job_id.clone();
            let child_session_id = binding.child_session_id.clone();
            Arc::new(move |event| {
                if let Err(error) = agent_runtime::emit_background_turn_update_blocking(
                    &event_writer,
                    runtime_job_id.as_str(),
                    child_session_id.as_str(),
                    event,
                ) {
                    eprintln!("centaeris Agent stream emit failed: {error}");
                }
            }) as Arc<dyn Fn(TurnUpdate) + Send + Sync>
        };
        let tool_safe_point = {
            let event_writer = self.event_writer.clone();
            let agent_run_id = req.job.job_id.clone();
            let child_session_id = binding.child_session_id.clone();
            Arc::new(move |safe_point: ToolSafePoint| {
                agent_runtime::persist_agent_tool_safe_point(
                    &event_writer,
                    child_session_id.as_str(),
                    agent_run_id.as_str(),
                    safe_point,
                )
            }) as ToolSafePointCommitPort
        };
        let outcome = match wire_api {
            WireApi::AnthropicMessages => {
                let client = AnthropicMessagesModelClient::new(registry, transport);
                let runner =
                    ModelClientSubagentRunner::new(&runtime, &client, &config_store, runner_config)
                        .with_stream_sink(stream_sink)
                        .with_tool_safe_point(tool_safe_point);
                runner.run_async(req).await
            }
            WireApi::OpenAiResponses => {
                let client = OpenAiResponsesModelClient::new(registry, transport);
                let runner =
                    ModelClientSubagentRunner::new(&runtime, &client, &config_store, runner_config)
                        .with_stream_sink(stream_sink)
                        .with_tool_safe_point(tool_safe_point);
                runner.run_async(req).await
            }
            WireApi::OpenAiChatCompletions => {
                let client = OpenAiCompatibleModelClient::new(registry, transport);
                let runner =
                    ModelClientSubagentRunner::new(&runtime, &client, &config_store, runner_config)
                        .with_stream_sink(stream_sink)
                        .with_tool_safe_point(tool_safe_point);
                runner.run_async(req).await
            }
            unsupported => {
                return Err(format!(
                    "model wire API {:?} is not supported by Electron subagent scheduler",
                    unsupported
                ));
            }
        };
        Ok(outcome)
    }
}

fn subagent_engine_config(
    runtime_config: &runtime_config::AgentRuntimeConfigResponse,
    allowed_tools: Option<Vec<String>>,
) -> AgentRuntimeConfig {
    let mut engine_config = AgentRuntimeConfig::default();
    engine_config.auto_continue_after_resume_wait = runtime_config.auto_continue_after_resume_wait;
    engine_config.model_context_tokens = runtime_config
        .model_context_tokens
        .unwrap_or(engine_config.model_context_tokens);
    engine_config.model_max_output_tokens = runtime_config
        .model_max_output_tokens
        .unwrap_or(engine_config.model_max_output_tokens);
    engine_config.tool_parallelism = runtime_config
        .tool_parallelism
        .unwrap_or(engine_config.tool_parallelism);
    engine_config.allowed_tools = allowed_tools;
    engine_config
}
