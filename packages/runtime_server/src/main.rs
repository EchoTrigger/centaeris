mod agent_inputs;
#[cfg(test)]
mod agent_inputs_tests;
mod agent_messages;
mod agent_run_authorization;
mod agent_work;
mod agent_work_trigger;
mod api_model_client;
mod artifact_publication;
mod contract;
mod deferred_input_resolver;
mod docker_engine;
mod docker_execution_host;
mod execution_capacity;
mod execution_recovery_coordinator;
mod execution_recovery_policy;
mod execution_recovery_schedule;
mod failure_diagnostics;
mod file_mutation_commit;
mod job_protocol;
mod lifecycle_hooks;
mod mcp;
#[cfg(test)]
mod native_consumption_tests;
mod observations;
mod platform_materials;
mod postgres_store;
mod request_capacity;
mod session_delivery;
mod skill_projection;
mod transcript_capture;
mod transcript_protocol;
mod transient_stream;
mod wait_recovery;
mod workspace_tools;

use std::collections::{HashMap, HashSet};
use std::env;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use api_model_client::{ApiModelClient, ApiModelClientConfig};
use artifact_publication::WorkspaceArtifactPublicationPort;
use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, Request, Response};
use axum::Router;
#[cfg(test)]
use centaeris_core::execution::ExecutionWorkspaceGenerationV1;
use centaeris_core::execution::{
    ExecutionCancellationProbe, ExecutionHostBinding, ExecutionHostMode, ExecutionHostRunner,
    ExecutionWorkspaceGeneration, WORKSPACE_DATA_ROOT,
};
use centaeris_core::extension::skills::{SkillEntryV1, SkillIndex};
use centaeris_core::extension::{
    build_plugin_activation_snapshot, load_mcp_servers_file, ActivatedPluginPackageV1,
    PluginActivationSnapshotV1,
};
use centaeris_core::model::prompt::PromptCompactionScopeV1;
use centaeris_core::model::provider_polling::{
    parse_provider_poll_payload_ref, ProviderPollingRuntimePayload, ProviderPollingSchedulerConfig,
    ProviderPollingToolLayerResolution, ProviderPollingToolLayerResolver,
    StoreBackedProviderPollingScheduler,
};
use centaeris_core::model::EmptyModelSessionConfigStore;
use centaeris_core::model::ToolCallEnvelope;
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, EventVisibility, RecoveryWorkspaceSnapshotV1,
    RuntimeAgentRunIdentityV1, RuntimeEvent, RuntimeRecoveryCheckpointV1,
    RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1,
};
use centaeris_core::runtime::event::RuntimeEventProjection;
use centaeris_core::runtime::subagent::{
    run_due_subagent_jobs_with_worker_pool_async, subagent_work_packet_runtime_binding,
    AsyncSubagentWorkerRunner, RunDueSubagentJobsRequest, SubagentWorkerPoolPolicy,
    SubagentWorkerRunFuture, SubagentWorkerRunOutcome, SubagentWorkerRunRequest,
    SUBAGENT_RUN_JOB_KIND,
};
use centaeris_core::runtime::{
    persist_subagent_result_projection_from_scheduler_events, AgentRunInitialInput,
    AgentRunRequest, AgentRunResult, AgentRunStop, AgentRuntime, AgentRuntimeConfig,
    AgentRuntimeSubagentRunnerConfig, DurableTurnControlBinding, ModelClientSubagentRunner,
    QueryLifecycleSubagentObserver, ToolConcurrencyCoordinator, ToolSafePoint,
    ToolSafePointCommitPort, TurnStepResult, TurnUpdate,
};
use centaeris_core::session::manager::SessionManager;
use centaeris_core::session::reliability::{
    agent_run_lifecycle_job_id, ListRuntimeJobsRequest, RuntimeJobRecord, RuntimeJobStatus,
    RuntimeJobStorePort, AGENT_RUN_LIFECYCLE_JOB_KIND,
};
use centaeris_core::session::state::CompletedTurnProjectionV1;
use centaeris_core::session::store::{RuntimeStore, RuntimeStoreActor};
use centaeris_core::session::supplement::{
    CloseTurnSupplementQueueRequest, EnqueueTurnSupplementDisposition,
    EnqueueTurnSupplementRequest, TurnSupplementStoreError, TurnSupplementStorePort,
};
use centaeris_core::session::{
    parse_wire_record, restore_runtime_snapshot_from_session_records,
    session_record_projects_to_agent_run_stream, AgentRunSessionState, RewriteLastUserTailRequest,
    RuntimeJobLeaseFence, SequencedSessionRecord, SessionCommitReceipt, SessionLogPort,
    SessionRecordType, RUNTIME_JOB_LEASE_FENCE_REJECTED,
};
use centaeris_core::tool::inputs::{
    DeferredInputResolutionFailureKind, ResolvedInputManifest, ResolvedInputState,
};
use centaeris_core::tool::layer::{ToolExecutionResult, ToolLayer};
use centaeris_core::tool::{DynamicToolRegistry, ToolErrorInfo, ToolFailureKind};
use centaeris_model_catalog::model_catalog;
use contract::{
    AgentRunCancelRequest, AgentRunStart, AgentRunStartInitialInput, AgentRunStepRequest,
    AgentRunSupplementRequest, AgentRunTailAction, AgentRunTeardownRequest, HostEventInputRequest,
};
use deferred_input_resolver::{ApiDeferredInputResolver, ApiModelInputImageResolver};
use docker_execution_host::{
    resolve_workspace_image_digest, DockerExecutionHostRequest, DockerExecutionHostRunner,
    SessionWorkspaceApiError, SessionWorkspaceCommitOutcome, SessionWorkspaceLease,
    SessionWorkspaceResolution,
};
use file_mutation_commit::WorkspaceFileMutationCommitPort;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use lifecycle_hooks::{workspace_hook_catalog, workspace_lifecycle_hook_runtime};
use postgres_store::{
    hydrate_session_wire_values, PostgresConnectionLimits, PostgresRuntimeStore, PostgresSessionLog,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Semaphore;

const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;
const MAX_HTTP_CONNECTIONS: usize = 128;
const HTTP_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_BODY_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const AGENT_RUN_WAITING_TRANSITION_REASONS: &[&str] = &[
    "execution_recovery_checkpoint_committed",
    "question_wait",
    "runtime_job_wait",
    "session_record_commit_unavailable",
    "session_workspace_commit_unavailable",
    "session_workspace_resolve_unavailable",
];
use workspace_tools::{workspace_tool_contracts, WorkspaceArtifactToolProvider};

use agent_run_authorization::WorkspaceAgentRunAuthorization;
use mcp::{
    connect_mcp_servers, prepare_http_mcp_servers, workspace_mcp_catalog, McpCredentialResolver,
};
use skill_projection::{workspace_skill_catalog_config, PLUGIN_CATALOG_ROOT};
use transient_stream::{LiveTextError, TransientAgentRunStream};

type ToolLayerRegistry = Arc<Mutex<HashMap<String, HostedRuntimeContext>>>;

struct AbortOnDrop<T>(Option<tokio::task::JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    fn take(mut self) -> tokio::task::JoinHandle<T> {
        self.0.take().expect("background task handle is present")
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

const EXECUTION_CONTROL_PROBE_CACHE_MS: u64 = 250;
const HOSTED_SUBAGENT_IDLE_MS: u64 = 250;
const HOSTED_SUBAGENT_LEASE_MS: u64 = 120_000;
const HOSTED_SUBAGENT_MAX_PARALLELISM: usize = 3;
const HOSTED_SUBAGENT_SCAN_LIMIT: usize = 64;
const WORKSPACE_SKILL_CATALOG_SCHEMA: &str = "workspace.skill.catalog.v1";
const WORKSPACE_SKILL_CATALOG_RESULT_SCHEMA: &str = "workspace.skill.catalog.result.v1";
const WORKSPACE_SKILL_DETAIL_SCHEMA: &str = "workspace.skill.detail.v1";
const WORKSPACE_SKILL_DETAIL_RESULT_SCHEMA: &str = "workspace.skill.detail.result.v1";
const WORKSPACE_MCP_CATALOG_SCHEMA: &str = "workspace.mcp.catalog.v1";
const WORKSPACE_HOOK_CATALOG_SCHEMA: &str = "workspace.hook.catalog.v1";
const WORKSPACE_MODEL_CATALOG_RESULT_SCHEMA: &str = "workspace.model_catalog.result.v1";
const WORKSPACE_PLUGIN_INSPECT_SCHEMA: &str = "workspace.plugin.inspect.v1";
const WORKSPACE_PLUGIN_INSPECT_RESULT_SCHEMA: &str = "workspace.plugin.inspect.result.v1";

fn workspace_model_catalog_response() -> serde_json::Value {
    json!({
        "schema": WORKSPACE_MODEL_CATALOG_RESULT_SCHEMA,
        "catalog": model_catalog(),
    })
}

fn inspect_plugin_package_at(
    catalog_root: &Path,
    package_path: &str,
) -> Result<ActivatedPluginPackageV1, String> {
    let mut parts = package_path.split('/');
    let staging_name = parts
        .next()
        .ok_or_else(|| "Plugin inspection path is empty".to_string())?;
    let package_name = parts
        .next()
        .ok_or_else(|| "Plugin inspection path must identify a package directory".to_string())?;
    if parts.next().is_some()
        || package_name != "package"
        || package_path.contains('\\')
        || package_path.contains(':')
    {
        return Err("Plugin inspection path is not canonical staging path".to_string());
    }
    let staging_id = staging_name
        .strip_prefix(".upload-")
        .ok_or_else(|| "Plugin inspection path is not an upload staging path".to_string())?;
    if staging_id.len() != 32
        || !staging_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Plugin inspection staging identity is invalid".to_string());
    }

    let catalog_root = catalog_root
        .canonicalize()
        .map_err(|error| format!("canonicalize Plugin catalog root failed: {error}"))?;
    let staging_root = catalog_root.join(staging_name);
    let staging_metadata = std::fs::symlink_metadata(staging_root.as_path())
        .map_err(|error| format!("inspect Plugin staging root failed: {error}"))?;
    if staging_metadata.file_type().is_symlink() || !staging_metadata.is_dir() {
        return Err("Plugin staging root must be a directory, not a symlink".to_string());
    }
    let staging_root = staging_root
        .canonicalize()
        .map_err(|error| format!("canonicalize Plugin staging root failed: {error}"))?;
    if staging_root.parent() != Some(catalog_root.as_path()) {
        return Err("Plugin staging root escaped catalog".to_string());
    }

    let package_root = staging_root.join(package_name);
    let package_metadata = std::fs::symlink_metadata(package_root.as_path())
        .map_err(|error| format!("inspect Plugin package root failed: {error}"))?;
    if package_metadata.file_type().is_symlink() || !package_metadata.is_dir() {
        return Err("Plugin package root must be a directory, not a symlink".to_string());
    }
    let package_root = package_root
        .canonicalize()
        .map_err(|error| format!("canonicalize Plugin package root failed: {error}"))?;
    if package_root.parent() != Some(staging_root.as_path())
        || !package_root.starts_with(catalog_root.as_path())
    {
        return Err("Plugin package root escaped upload staging".to_string());
    }

    let mut snapshot = build_plugin_activation_snapshot(std::slice::from_ref(&package_root))?;
    if snapshot.packages.len() != 1 {
        return Err("Plugin inspection must resolve exactly one package".to_string());
    }
    let package = snapshot
        .packages
        .pop()
        .expect("validated single Plugin package");
    for resource in &package.mcp_servers {
        load_mcp_servers_file(package_root.join(resource.path.as_str()).as_path())?;
    }
    Ok(package)
}

#[derive(Clone)]
struct HostedRuntimeContext {
    agent_run_id: String,
    agent_run_identity: RuntimeAgentRunIdentityV1,
    tool_layer: ToolLayer,
    model_client: ApiModelClient,
    agent_runtime_config: AgentRuntimeConfig,
    tool_concurrency: ToolConcurrencyCoordinator,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceSkillCatalogRequest {
    schema: String,
    plugin_activation: PluginActivationSnapshotV1,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceSkillDetailRequest {
    schema: String,
    plugin_activation: PluginActivationSnapshotV1,
    skill_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceMcpCatalogRequest {
    schema: String,
    plugin_activation: PluginActivationSnapshotV1,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceHookCatalogRequest {
    schema: String,
    plugin_activation: PluginActivationSnapshotV1,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspacePluginInspectRequest {
    schema: String,
    package_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspacePluginInspectResponse {
    schema: &'static str,
    package: ActivatedPluginPackageV1,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSkillSummary {
    skill_id: String,
    name: String,
    description: String,
    enabled: bool,
    allow_implicit_invocation: bool,
    allowed_tools: Vec<String>,
}

impl From<&SkillEntryV1> for WorkspaceSkillSummary {
    fn from(value: &SkillEntryV1) -> Self {
        Self {
            skill_id: value.skill_id.clone(),
            name: value.name.clone(),
            description: value.description.clone(),
            enabled: value.enabled,
            allow_implicit_invocation: value.allow_implicit_invocation,
            allowed_tools: value.capability_metadata.allowed_tools.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSkillCatalogResult {
    schema: &'static str,
    skills: Vec<WorkspaceSkillSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSkillDetailResult {
    schema: &'static str,
    skill: WorkspaceSkillSummary,
    content: String,
}

fn cached_execution_control_reason(
    cache: &Mutex<Option<(Instant, Option<String>)>>,
    load: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    let mut cache = cache.lock().map_err(|_| {
        "run_execution_control_probe_failed:execution control cache lock poisoned".to_string()
    })?;
    if let Some((checked_at, reason)) = cache.as_ref() {
        if checked_at.elapsed() < Duration::from_millis(EXECUTION_CONTROL_PROBE_CACHE_MS) {
            return Ok(reason.clone());
        }
    }
    let reason = load()?;
    *cache = Some((Instant::now(), reason.clone()));
    Ok(reason)
}

fn main() -> Result<(), String> {
    DockerExecutionHostRunner::validate_host()?;
    let execution_profile = Arc::new(RuntimeExecutionProfile {
        schema: RUNTIME_EXECUTION_PROFILE_SCHEMA,
        image_capability: "workspace_general_v1",
        image_digest: resolve_workspace_image_digest()?,
    });
    let bind_address = env::var("RUNTIME_BIND_ADDRESS")
        .map_err(|_| "RUNTIME_BIND_ADDRESS is required".to_string())?;
    let port = env::var("RUNTIME_PORT").unwrap_or_else(|_| "9000".to_string());
    let socket_address = parse_runtime_socket_address(bind_address.as_str(), port.as_str())?;
    let runtime = Arc::new(
        tokio::runtime::Runtime::new()
            .map_err(|error| format!("create Tokio runtime failed: {error}"))?,
    );
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind(socket_address))
        .map_err(|error| format!("bind runtime server failed: {error}"))?;
    eprintln!("runtime listening on {socket_address}");
    let (store, job_store) = {
        let _guard = runtime.enter();
        open_store()?
    };
    let store = Arc::new(store);
    let job_store = Arc::new(job_store);
    let tool_layers = Arc::new(Mutex::new(HashMap::<String, HostedRuntimeContext>::new()));
    let resolver_layers = tool_layers.clone();
    let resolver_jobs = job_store.clone();
    let resolver: ProviderPollingToolLayerResolver = Arc::new(move |job| {
        resolve_provider_polling_tool_layer(job, resolver_layers.as_ref(), resolver_jobs.as_ref())
    });
    let provider_polling = StoreBackedProviderPollingScheduler::new_with_tool_layer_resolver(
        (*job_store).clone(),
        resolver,
        ProviderPollingSchedulerConfig::default(),
    );
    provider_polling.start()?;
    runtime.spawn(run_hosted_subagent_worker(
        store.clone(),
        job_store.clone(),
        tool_layers.clone(),
    ));
    execution_capacity::ExecutionCapacity::from_env()?;
    docker_engine::create_limit_from_env()?;
    let state = HttpServerState {
        runtime: runtime.clone(),
        store,
        job_store,
        tool_layers,
        execution_profile,
        request_capacity: request_capacity::RequestCapacity::from_env()?,
    };
    runtime.block_on(serve_http(listener, state))
}

fn resolve_provider_polling_tool_layer(
    job: &RuntimeJobRecord,
    tool_layers: &Mutex<HashMap<String, HostedRuntimeContext>>,
    jobs: &dyn RuntimeJobStorePort,
) -> ProviderPollingToolLayerResolution {
    let payload = match parse_provider_poll_payload_ref(job.payload_ref.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("decode provider poll binding failed: {error}");
            return provider_polling_resolution_failure(
                ToolFailureKind::ProviderError,
                false,
                "provider poll binding is invalid",
                "provider_poll_binding_invalid",
            );
        }
    };
    let lifecycle_job_id = match agent_run_lifecycle_job_id(payload.source_agent_run_id.as_str()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("derive provider poll lifecycle identity failed: {error}");
            return provider_polling_resolution_failure(
                ToolFailureKind::ProviderError,
                false,
                "provider poll AgentRun binding is invalid",
                "provider_poll_agent_run_invalid",
            );
        }
    };
    let lifecycle = match jobs.get_runtime_job(lifecycle_job_id.as_str()) {
        Ok(Some(value)) => value,
        Ok(None) => {
            return provider_polling_resolution_failure(
                ToolFailureKind::ProviderError,
                false,
                "provider poll AgentRun lifecycle is missing",
                "provider_poll_lifecycle_missing",
            );
        }
        Err(error) => {
            eprintln!("load provider poll AgentRun lifecycle failed: {error}");
            return provider_polling_resolution_failure(
                ToolFailureKind::HostUnavailable,
                true,
                "provider poll AgentRun lifecycle is temporarily unavailable",
                "provider_poll_lifecycle_unavailable",
            );
        }
    };
    match resolve_provider_polling_tool_layer_records(job, &payload, &lifecycle, None) {
        ProviderPollingToolLayerResolution::Stopped { reason } => {
            return ProviderPollingToolLayerResolution::Stopped { reason };
        }
        ProviderPollingToolLayerResolution::Failed(error) if !error.retryable => {
            return ProviderPollingToolLayerResolution::Failed(error);
        }
        _ => {}
    }
    let context = match tool_layers.lock() {
        Ok(contexts) => job
            .session_id
            .as_deref()
            .and_then(|session_id| contexts.get(session_id))
            .cloned(),
        Err(_) => {
            return provider_polling_resolution_failure(
                ToolFailureKind::HostUnavailable,
                true,
                "provider poll tool layer registry is temporarily unavailable",
                "provider_poll_registry_unavailable",
            );
        }
    };
    resolve_provider_polling_tool_layer_records(job, &payload, &lifecycle, context.as_ref())
}

fn resolve_provider_polling_tool_layer_records(
    job: &RuntimeJobRecord,
    payload: &ProviderPollingRuntimePayload,
    lifecycle: &RuntimeJobRecord,
    context: Option<&HostedRuntimeContext>,
) -> ProviderPollingToolLayerResolution {
    let expected_lifecycle_job_id =
        match agent_run_lifecycle_job_id(payload.source_agent_run_id.as_str()) {
            Ok(value) => value,
            Err(_) => {
                return provider_polling_resolution_failure(
                    ToolFailureKind::ProviderError,
                    false,
                    "provider poll AgentRun binding is invalid",
                    "provider_poll_agent_run_invalid",
                );
            }
        };
    let session_id = match job.session_id.as_deref() {
        Some(value) if !value.trim().is_empty() => value,
        _ => {
            return provider_polling_resolution_failure(
                ToolFailureKind::ProviderError,
                false,
                "provider poll session binding is missing",
                "provider_poll_session_missing",
            );
        }
    };
    if job.job_kind != centaeris_core::model::provider_polling::PROVIDER_POLL_RUNTIME_JOB_KIND
        || lifecycle.job_kind != AGENT_RUN_LIFECYCLE_JOB_KIND
        || lifecycle.job_id != expected_lifecycle_job_id
        || lifecycle.session_id.as_deref() != Some(session_id)
        || lifecycle.payload_ref.as_deref()
            != Some(format!("record:agent_run:{}", payload.source_agent_run_id).as_str())
    {
        return provider_polling_resolution_failure(
            ToolFailureKind::ProviderError,
            false,
            "provider poll AgentRun lifecycle binding is invalid",
            "provider_poll_lifecycle_binding_invalid",
        );
    }
    if lifecycle.status.is_terminal() {
        return ProviderPollingToolLayerResolution::Stopped {
            reason: "source_agent_run_terminal".to_string(),
        };
    }
    match context {
        Some(context) if context.agent_run_id == payload.source_agent_run_id => {
            ProviderPollingToolLayerResolution::Ready(Box::new(context.tool_layer.clone()))
        }
        Some(_) => provider_polling_resolution_failure(
            ToolFailureKind::ProviderError,
            false,
            "provider poll tool layer belongs to a different AgentRun",
            "provider_poll_agent_run_context_mismatch",
        ),
        None => provider_polling_resolution_failure(
            ToolFailureKind::HostUnavailable,
            true,
            "provider poll tool layer is not active during recovery",
            "provider_poll_tool_layer_recovering",
        ),
    }
}

fn provider_polling_resolution_failure(
    kind: ToolFailureKind,
    retryable: bool,
    message: &str,
    diagnostic: &str,
) -> ProviderPollingToolLayerResolution {
    let user_message = if retryable {
        "Knowledge service unavailable"
    } else {
        "Knowledge request failed"
    };
    ProviderPollingToolLayerResolution::Failed(
        ToolErrorInfo::new(kind, message, user_message)
            .with_diagnostic(diagnostic)
            .with_retryable(retryable),
    )
}

fn parse_runtime_socket_address(bind_address: &str, port: &str) -> Result<SocketAddr, String> {
    let bind_address = bind_address
        .parse::<IpAddr>()
        .map_err(|_| "RUNTIME_BIND_ADDRESS must be an exact IP address".to_string())?;
    let port = port
        .parse::<u16>()
        .map_err(|_| "RUNTIME_PORT must be an integer from 0 to 65535".to_string())?;
    Ok(SocketAddr::new(bind_address, port))
}

fn open_store() -> Result<(RuntimeStoreActor, PostgresRuntimeStore), String> {
    let database_url =
        env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required".to_string())?;
    let state_root =
        env::var("RUNTIME_STATE_ROOT").map_err(|_| "RUNTIME_STATE_ROOT is required".to_string())?;
    let defaults = PostgresConnectionLimits::default();
    let store = PostgresRuntimeStore::new_with_limits(
        database_url.as_str(),
        PostgresConnectionLimits {
            ordinary: positive_usize_env("RUNTIME_POSTGRES_POOL_SIZE", defaults.ordinary)?,
            execution_control: positive_usize_env(
                "RUNTIME_POSTGRES_CONTROL_POOL_SIZE",
                defaults.execution_control,
            )?,
            listeners: positive_usize_env("RUNTIME_POSTGRES_LISTENER_LIMIT", defaults.listeners)?,
            checkout_timeout: Duration::from_millis(positive_u64_env(
                "RUNTIME_POSTGRES_CHECKOUT_TIMEOUT_MS",
                defaults.checkout_timeout.as_millis() as u64,
            )?),
            connect_timeout: Duration::from_millis(positive_u64_env(
                "RUNTIME_POSTGRES_CONNECT_TIMEOUT_MS",
                defaults.connect_timeout.as_millis() as u64,
            )?),
        },
    )
    .map_err(|error| format!("open Postgres runtime store failed: {error}"))?;
    std::fs::create_dir_all(state_root.as_str())
        .map_err(|error| format!("create runtime state root failed: {error}"))?;
    Ok((
        RuntimeStoreActor::start(store.clone()).map_err(|error| error.to_string())?,
        store,
    ))
}

fn positive_usize_env(name: &str, default: usize) -> Result<usize, String> {
    match env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} must be a positive integer")),
    }
}

fn positive_u64_env(name: &str, default: u64) -> Result<u64, String> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| format!("{name} must be a positive integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} must be a positive integer")),
    }
}

async fn run_hosted_subagent_worker(
    store: Arc<RuntimeStoreActor>,
    job_store: Arc<PostgresRuntimeStore>,
    contexts: ToolLayerRegistry,
) {
    loop {
        if let Err(error) =
            run_next_hosted_subagent_batch(store.as_ref(), job_store.clone(), contexts.clone())
                .await
        {
            eprintln!("hosted subagent worker failed: {error}");
        }
        tokio::time::sleep(Duration::from_millis(HOSTED_SUBAGENT_IDLE_MS)).await;
    }
}

async fn run_next_hosted_subagent_batch(
    store: &RuntimeStoreActor,
    job_store: Arc<PostgresRuntimeStore>,
    contexts: ToolLayerRegistry,
) -> Result<(), String> {
    let now = now_ms()?;
    store.reclaim_expired_runtime_job_leases(now).await?;
    let jobs = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: None,
            branch_id: None,
            limit: HOSTED_SUBAGENT_SCAN_LIMIT,
            offset: 0,
        })
        .await?;
    let batches = hosted_subagent_batches(&jobs, &contexts, now)?;
    run_hosted_subagent_batches(batches, |batch| {
        run_hosted_subagent_batch(store, job_store.clone(), batch, now)
    })
    .await
}

struct HostedSubagentBatch {
    session_id: String,
    context: HostedRuntimeContext,
    limit: usize,
}

async fn run_hosted_subagent_batches<T, Run, Execution>(
    batches: Vec<T>,
    run: Run,
) -> Result<(), String>
where
    Run: Fn(T) -> Execution,
    Execution: Future<Output = Result<(), String>>,
{
    // Keep every scoped pool alive through completion, including after a sibling
    // fails. This is a bounded cross-Session batch, not rolling job admission.
    let results = futures::future::join_all(batches.into_iter().map(run)).await;
    for result in results {
        result?;
    }
    Ok(())
}

async fn run_hosted_subagent_batch(
    store: &RuntimeStoreActor,
    job_store: Arc<PostgresRuntimeStore>,
    batch: HostedSubagentBatch,
    now: i64,
) -> Result<(), String> {
    let HostedSubagentBatch {
        session_id,
        context,
        limit,
    } = batch;
    let worker_id = format!("hosted-subagent-worker-{}", std::process::id());
    let runner = HostedSubagentRunner {
        store: store.clone(),
        job_store,
        context: context.clone(),
    };
    let observer_runtime = AgentRuntime::new(
        store.clone(),
        context.tool_layer.clone(),
        context.agent_runtime_config.clone(),
        context.tool_concurrency.clone(),
    );
    let observer = QueryLifecycleSubagentObserver::new(&observer_runtime);
    let result = run_due_subagent_jobs_with_worker_pool_async(
        store,
        &runner,
        &observer,
        RunDueSubagentJobsRequest {
            now_ms: now,
            worker_id,
            session_id: Some(session_id.clone()),
            limit,
            lease_ms: HOSTED_SUBAGENT_LEASE_MS,
            started_at_ms: now,
            finished_at_ms: now_ms()?,
        },
        SubagentWorkerPoolPolicy {
            max_parallelism: limit,
        },
    )
    .await?;
    persist_subagent_result_projection_from_scheduler_events(
        store,
        session_id.as_str(),
        result.events.as_slice(),
    )?;
    Ok(())
}

fn hosted_subagent_batches(
    jobs: &[RuntimeJobRecord],
    contexts: &ToolLayerRegistry,
    now: i64,
) -> Result<Vec<HostedSubagentBatch>, String> {
    let contexts = contexts
        .lock()
        .map_err(|_| "hosted subagent context registry lock poisoned".to_string())?;
    let mut batches: Vec<HostedSubagentBatch> = Vec::new();
    let mut queued_counts = Vec::new();
    for job in jobs {
        if job.job_kind != SUBAGENT_RUN_JOB_KIND || job.status != RuntimeJobStatus::Queued {
            return Err(format!(
                "hosted subagent scan returned unsupported job: {}",
                job.job_id
            ));
        }
        let session_id = job
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("hosted subagent job session missing: {}", job.job_id))?;
        if job.run_at_ms > now {
            continue;
        }
        if let Some(context) = contexts.get(session_id) {
            if let Some(index) = batches
                .iter()
                .position(|batch| batch.session_id == session_id)
            {
                queued_counts[index] += 1;
            } else if batches.len() < HOSTED_SUBAGENT_MAX_PARALLELISM {
                batches.push(HostedSubagentBatch {
                    session_id: session_id.to_string(),
                    context: context.clone(),
                    limit: 1,
                });
                queued_counts.push(1);
            }
        }
    }
    // Give each selected Session one slot before expanding a scoped pool. The
    // sum of claim limits bounds this process, while Core owns actual leases
    // and same-Session resource conflicts.
    let mut remaining = HOSTED_SUBAGENT_MAX_PARALLELISM - batches.len();
    while remaining > 0 {
        let mut assigned = false;
        for (batch, queued) in batches.iter_mut().zip(&queued_counts) {
            if batch.limit < *queued {
                batch.limit += 1;
                remaining -= 1;
                assigned = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !assigned {
            break;
        }
    }
    Ok(batches)
}

struct HostedSubagentRunner {
    store: RuntimeStoreActor,
    job_store: Arc<PostgresRuntimeStore>,
    context: HostedRuntimeContext,
}

impl AsyncSubagentWorkerRunner for HostedSubagentRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async move {
            match self.run(req).await {
                Ok(outcome) => outcome,
                Err(error) => SubagentWorkerRunOutcome::Failed { error, retry: None },
            }
        })
    }
}

impl HostedSubagentRunner {
    async fn run(&self, req: SubagentWorkerRunRequest) -> Result<SubagentWorkerRunOutcome, String> {
        let binding = subagent_work_packet_runtime_binding(&req.work_packet, &req.job)?;
        if binding.parent_agent_run_id != self.context.agent_run_id
            || req.job.session_id.as_deref() != Some(req.lifecycle.session_id.as_str())
        {
            return Err(format!(
                "hosted subagent runtime binding mismatch: {}",
                req.job.job_id
            ));
        }
        let job_id = req.job.job_id.clone();
        let lease_owner = req
            .job
            .lease_owner
            .clone()
            .ok_or_else(|| format!("hosted subagent lease owner missing: {job_id}"))?;
        let cancellation_store = self.job_store.clone();
        let cancellation_job_id = job_id.clone();
        let cancellation_lease_owner = lease_owner.clone();
        let cancellation_probe: Arc<ExecutionCancellationProbe> = Arc::new(move || {
            let job = cancellation_store
                .get_runtime_job_execution_control(cancellation_job_id.as_str())?
                .ok_or_else(|| "hosted_subagent_job_missing".to_string())?;
            if job.status == RuntimeJobStatus::Cancelled {
                return Ok(Some(
                    job.last_error
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("subagent_cancelled")
                        .to_string(),
                ));
            }
            let now = now_ms()?;
            if job.status != RuntimeJobStatus::Running
                || job.lease_owner.as_deref() != Some(cancellation_lease_owner.as_str())
                || job.lease_expires_at_ms.is_none_or(|expires| expires <= now)
            {
                return Ok(Some("subagent_lease_lost".to_string()));
            }
            Ok(None)
        });
        let tool_layer = self
            .context
            .tool_layer
            .clone()
            .with_execution_owner(job_id.clone())
            .with_session_id(binding.child_session_id.clone())
            .with_execution_cancellation_probe(cancellation_probe)
            .with_file_mutation_commit_port(Arc::new(WorkspaceFileMutationCommitPort::new(
                Arc::new(self.store.clone()),
                binding.child_session_id.clone(),
                job_id,
            )?));
        let mut config = self.context.agent_runtime_config.clone();
        config.allowed_tools = Some(binding.allowed_tools.clone());
        let runtime = AgentRuntime::new(
            self.store.clone(),
            tool_layer,
            config,
            self.context.tool_concurrency.clone(),
        );
        runtime.validate_subagent_tool_contracts(&binding)?;
        let agent_run_identity = RuntimeAgentRunIdentityV1 {
            agent_run_id: req.job.job_id.clone(),
            execution_id: self.context.agent_run_identity.execution_id.clone(),
            authorization_digest: self.context.agent_run_identity.authorization_digest.clone(),
        };
        agent_run_identity.validate()?;
        let safe_point_store = self.store.clone();
        let safe_point_session_id = binding.child_session_id.clone();
        let safe_point_agent_run_id = req.job.job_id.clone();
        let tool_safe_point = Arc::new(move |safe_point| {
            persist_hosted_subagent_tool_safe_point(
                &safe_point_store,
                safe_point_session_id.as_str(),
                safe_point_agent_run_id.as_str(),
                safe_point,
            )
        }) as ToolSafePointCommitPort;
        let model_config_store = EmptyModelSessionConfigStore::new();
        let runner = ModelClientSubagentRunner::new(
            &runtime,
            &self.context.model_client,
            &model_config_store,
            AgentRuntimeSubagentRunnerConfig {
                auto_continue_after_resume_wait: Some(false),
                agent_run_identity: Some(agent_run_identity),
            },
        )
        .with_tool_safe_point(tool_safe_point);
        Ok(runner.run_async(req).await)
    }
}

fn persist_hosted_subagent_tool_safe_point(
    store: &RuntimeStoreActor,
    expected_session_id: &str,
    expected_agent_run_id: &str,
    safe_point: ToolSafePoint,
) -> Result<(), String> {
    let event = match safe_point {
        ToolSafePoint::DurableToolCall {
            session_id,
            turn_id,
            agent_run_id,
            call,
            provider_id,
            tool_contract_digest,
            recorded_at_ms,
        } => {
            if session_id != expected_session_id || agent_run_id != expected_agent_run_id {
                return Err("hosted subagent tool safe point identity mismatch".to_string());
            }
            RuntimeEvent {
                event_id: format!(
                    "subagent_tool_call_commit:{expected_agent_run_id}:{}",
                    call.id
                ),
                session_id,
                task_id: Some(call.id.clone()),
                event_type: "subagent_tool_call_committed".to_string(),
                at_ms: recorded_at_ms,
                visibility: EventVisibility::Internal,
                payload_json: serde_json::to_string(&json!({
                    "schema": "subagent_tool_call_commit.v1",
                    "turnId": turn_id,
                    "agentRunId": agent_run_id,
                    "call": call,
                    "providerId": provider_id,
                    "toolContractDigest": tool_contract_digest,
                }))
                .map_err(|error| {
                    format!("encode hosted subagent tool call commit failed: {error}")
                })?,
            }
        }
        ToolSafePoint::DurableReceipt {
            session_id,
            turn_id,
            agent_run_id,
            call,
            result,
        } => {
            if session_id != expected_session_id || agent_run_id != expected_agent_run_id {
                return Err("hosted subagent tool safe point identity mismatch".to_string());
            }
            RuntimeEvent {
                event_id: format!(
                    "subagent_tool_receipt_commit:{expected_agent_run_id}:{}",
                    call.id
                ),
                session_id,
                task_id: Some(call.id.clone()),
                event_type: "subagent_tool_receipt_committed".to_string(),
                at_ms: result.completed_at_ms,
                visibility: EventVisibility::Internal,
                payload_json: serde_json::to_string(&json!({
                    "schema": "subagent_tool_receipt_commit.v1",
                    "turnId": turn_id,
                    "agentRunId": agent_run_id,
                    "call": call,
                    "result": result,
                }))
                .map_err(|error| {
                    format!("encode hosted subagent tool receipt commit failed: {error}")
                })?,
            }
        }
        ToolSafePoint::ModelRequestStarted(_)
        | ToolSafePoint::ReasoningCompleted { .. }
        | ToolSafePoint::ProviderUsage { .. }
        | ToolSafePoint::CompletedTurn(_) => return Ok(()),
    };
    <RuntimeStoreActor as RuntimeStore>::append_event_idempotent(store, event)
        .map_err(|error| format!("persist hosted subagent tool safe point failed: {error}"))
}

#[derive(Clone)]
struct HttpServerState {
    runtime: Arc<tokio::runtime::Runtime>,
    store: Arc<RuntimeStoreActor>,
    job_store: Arc<PostgresRuntimeStore>,
    tool_layers: ToolLayerRegistry,
    execution_profile: Arc<RuntimeExecutionProfile>,
    request_capacity: request_capacity::RequestCapacity,
}

const RUNTIME_EXECUTION_PROFILE_SCHEMA: &str = "runtime.execution_profile.v1";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeExecutionProfile {
    schema: &'static str,
    image_capability: &'static str,
    image_digest: String,
}

async fn serve_http(
    listener: tokio::net::TcpListener,
    state: HttpServerState,
) -> Result<(), String> {
    let router = Router::new()
        .fallback(dispatch_http_request)
        .with_state(state);
    let connection_slots = Arc::new(Semaphore::new(MAX_HTTP_CONNECTIONS));
    loop {
        let connection_slot = connection_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "runtime HTTP connection limiter closed".to_string())?;
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("accept runtime HTTP connection failed: {error}"))?;
        let service = TowerToHyperService::new(router.clone());
        tokio::spawn(async move {
            let io = TokioIo::new(WriteTimeoutIo::new(stream, HTTP_WRITE_TIMEOUT));
            let builder = runtime_http1_builder(HTTP_HEADER_TIMEOUT);
            if let Err(error) = builder.serve_connection(io, service).await {
                eprintln!("runtime HTTP connection failed: {error}");
            }
            drop(connection_slot);
        });
    }
}

fn runtime_http1_builder(header_timeout: Duration) -> http1::Builder {
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(header_timeout)
        .max_headers(64)
        .max_buf_size(64 * 1024)
        .keep_alive(false);
    builder
}

async fn dispatch_http_request(
    State(state): State<HttpServerState>,
    request: Request<Body>,
) -> Response<Body> {
    let observed_route = observations::route(request.uri().path());
    let observed_lane =
        if request.method().as_str() == "POST" && request.uri().path() == "/internal/jobs/wait" {
            2
        } else {
            0
        };
    let admission_started = Instant::now();
    let lane = state
        .request_capacity
        .lane(request.method().as_str(), request.uri().path());
    let observed_lane = if lane.control { 1 } else { observed_lane };
    let deadline = lane.deadline.map(|duration| Instant::now() + duration);
    let slot = if lane.control {
        // Absorb brief heartbeat bursts within the same control deadline.
        // Connection admission bounds these waiters; they execute no DB work.
        tokio::time::timeout_at(
            deadline.expect("control deadline").into(),
            lane.slots.acquire_owned(),
        )
        .await
        .ok()
        .and_then(Result::ok)
    } else {
        lane.slots.try_acquire_owned().ok()
    };
    let request_slot = match slot {
        Some(slot) => slot,
        None => {
            observations::Request::rejected(observed_route, observed_lane, admission_started);
            return bounded_json_error_response(503, "runtime_busy").into_axum_response();
        }
    };
    let observed_request =
        observations::Request::admitted(observed_route, observed_lane, admission_started);
    let read = read_axum_request(request);
    let read_result = if let Some(deadline) = deadline {
        match tokio::time::timeout_at(deadline.into(), read).await {
            Ok(result) => result,
            Err(_) => {
                return bounded_json_error_response(408, "request_body_timeout")
                    .into_axum_response();
            }
        }
    } else {
        read.await
    };
    let request = match read_result {
        Ok(request) => request,
        Err(response) => return response.into_axum_response(),
    };
    let task = tokio::task::spawn_blocking(move || {
        let _context = observations::Context::enter(&request.body);
        let _observed_request = observed_request;
        let _request_slot = request_slot;
        let _deadline = postgres_store::RequestDeadline::enter(deadline);
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Ok(bounded_json_error_response(
                504,
                "request_deadline_exceeded",
            ));
        }
        let job_store = if lane.control {
            Arc::new(state.job_store.execution_control_view())
        } else {
            state.job_store
        };
        handle_request(
            request,
            state.runtime,
            state.store,
            job_store,
            state.tool_layers,
            state.execution_profile,
        )
    });
    let result = if let Some(deadline) = deadline {
        match tokio::time::timeout_at(deadline.into(), task).await {
            Ok(result) => result,
            // Dropping a JoinHandle does not stop blocking work. The closure owns
            // the permit until it actually exits; timeout must not mint capacity
            // or imply that an uncertain write was rolled back.
            Err(_) => {
                return bounded_json_error_response(504, "request_deadline_exceeded")
                    .into_axum_response();
            }
        }
    } else {
        task.await
    };
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return bounded_json_error_response(504, "request_deadline_exceeded").into_axum_response();
    }
    match result {
        Ok(Ok(response)) => response.into_axum_response(),
        Ok(Err(error)) => {
            eprintln!("runtime request failed: {error}");
            bounded_json_error_response(500, "internal_error").into_axum_response()
        }
        Err(error) => {
            eprintln!("runtime request task failed: {error}");
            bounded_json_error_response(500, "internal_error").into_axum_response()
        }
    }
}

async fn read_axum_request(request: Request<Body>) -> Result<HttpRequest, RuntimeHttpResponse> {
    let (parts, body) = request.into_parts();
    let body =
        match tokio::time::timeout(HTTP_BODY_TIMEOUT, to_bytes(body, MAX_HTTP_BODY_BYTES)).await {
            Ok(Ok(body)) => body.to_vec(),
            Ok(Err(_)) => return Err(bounded_json_error_response(413, "request_body_too_large")),
            Err(_) => return Err(bounded_json_error_response(408, "request_body_timeout")),
        };
    let mut headers = HashMap::new();
    for (name, value) in &parts.headers {
        let value = value
            .to_str()
            .map_err(|_| bounded_json_error_response(400, "request_header_invalid"))?;
        if headers
            .insert(name.as_str().to_string(), value.to_string())
            .is_some()
        {
            return Err(bounded_json_error_response(400, "request_header_duplicate"));
        }
    }
    Ok(HttpRequest {
        method: parts.method.as_str().to_string(),
        path: parts
            .uri
            .path_and_query()
            .map_or_else(|| parts.uri.path(), |value| value.as_str())
            .to_string(),
        headers,
        body,
    })
}

struct WriteTimeoutIo {
    inner: tokio::net::TcpStream,
    timeout: Duration,
    timer: Pin<Box<tokio::time::Sleep>>,
    waiting: bool,
}

impl WriteTimeoutIo {
    fn new(inner: tokio::net::TcpStream, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            timer: Box::pin(tokio::time::sleep(timeout)),
            waiting: false,
        }
    }

    fn poll_timeout(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.waiting {
            self.timer
                .as_mut()
                .reset(tokio::time::Instant::now() + self.timeout);
            self.waiting = true;
        }
        match self.timer.as_mut().poll(context) {
            Poll::Ready(()) => {
                self.waiting = false;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "runtime HTTP response write timed out",
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn complete_write(&mut self) {
        self.waiting = false;
    }
}

impl AsyncRead for WriteTimeoutIo {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(context, buffer)
    }
}

impl AsyncWrite for WriteTimeoutIo {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_write(context, buffer) {
            Poll::Ready(result) => {
                this.complete_write();
                Poll::Ready(result)
            }
            Poll::Pending => match this.poll_timeout(context) {
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Ready(Ok(())) | Poll::Pending => Poll::Pending,
            },
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_flush(context) {
            Poll::Ready(result) => {
                this.complete_write();
                Poll::Ready(result)
            }
            Poll::Pending => this.poll_timeout(context),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_shutdown(context) {
            Poll::Ready(result) => {
                this.complete_write();
                Poll::Ready(result)
            }
            Poll::Pending => this.poll_timeout(context),
        }
    }
}

fn handle_request(
    request: HttpRequest,
    runtime: Arc<tokio::runtime::Runtime>,
    store: Arc<RuntimeStoreActor>,
    job_store: Arc<PostgresRuntimeStore>,
    tool_layers: ToolLayerRegistry,
    execution_profile: Arc<RuntimeExecutionProfile>,
) -> Result<RuntimeHttpResponse, String> {
    if let Some((status, response)) = job_protocol::handle(
        request.method.as_str(),
        request.path.as_str(),
        &request.headers,
        request.body.as_slice(),
        job_store.as_ref(),
    ) {
        return Ok(http_response(status, "application/json", response));
    }
    if request.method == "GET"
        && matches!(
            request.path.as_str(),
            "/internal/model-catalog" | "/internal/execution-profile"
        )
    {
        let token = env::var("INTERNAL_API_TOKEN")
            .map_err(|_| "INTERNAL_API_TOKEN is required".to_string())?;
        if request.headers.get("x-internal-token").map(String::as_str) != Some(token.as_str()) {
            return json_error_response(401, "unauthorized");
        }
        if request.path == "/internal/execution-profile" {
            let response = serde_json::to_vec(execution_profile.as_ref())
                .map_err(|error| format!("encode Runtime execution profile failed: {error}"))?;
            return Ok(http_response(200, "application/json", response));
        }
        let response = serde_json::to_vec(&workspace_model_catalog_response())
            .map_err(|error| format!("encode workspace model catalog failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    if request.method != "POST"
        || !matches!(
            request.path.as_str(),
            "/agent-runs/step"
                | "/agent-runs/cancel"
                | "/internal/agent-runs/supplement"
                | "/agent-runs/teardown"
                | "/skills/catalog"
                | "/skills/detail"
                | "/mcp/catalog"
                | "/hooks/catalog"
                | "/internal/plugins/inspect"
                | "/internal/transcript/page"
                | "/internal/transcript/patches"
                | "/internal/transcript/content"
                | "/internal/host-event-input"
        )
    {
        return Ok(http_response(404, "text/plain", b"not_found".to_vec()));
    }
    let token =
        env::var("INTERNAL_API_TOKEN").map_err(|_| "INTERNAL_API_TOKEN is required".to_string())?;
    if request.headers.get("x-internal-token").map(String::as_str) != Some(token.as_str()) {
        return json_error_response(401, "unauthorized");
    }
    if request.path == "/internal/host-event-input" {
        let input = match serde_json::from_slice::<HostEventInputRequest>(&request.body)
            .map_err(|error| error.to_string())
            .and_then(HostEventInputRequest::create)
        {
            Ok(input) => input,
            Err(_) => return json_error_response(400, "host_event_input_invalid"),
        };
        return Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&json!({
                "schema":"runtime.host_event_input.created.v1", "input":input,
            }))
            .map_err(|error| error.to_string())?,
        ));
    }
    if let Some(response) = transcript_protocol::handle(
        request.path.as_str(),
        request.body.as_slice(),
        job_store.as_ref(),
    ) {
        let (status, body) = response?;
        return Ok(http_response(status, "application/json", body));
    }
    if request.path == "/internal/plugins/inspect" {
        let request = match serde_json::from_slice::<WorkspacePluginInspectRequest>(
            request.body.as_slice(),
        ) {
            Ok(value) if value.schema == WORKSPACE_PLUGIN_INSPECT_SCHEMA => value,
            Ok(_) | Err(_) => {
                return json_error_response(400, "plugin_inspection_request_invalid");
            }
        };
        let package = match inspect_plugin_package_at(
            Path::new(PLUGIN_CATALOG_ROOT),
            request.package_path.as_str(),
        ) {
            Ok(package) => package,
            Err(error) => {
                eprintln!("workspace Plugin package inspection failed: {error}");
                return json_error_response(400, "plugin_package_invalid");
            }
        };
        let response = WorkspacePluginInspectResponse {
            schema: WORKSPACE_PLUGIN_INSPECT_RESULT_SCHEMA,
            package,
        };
        let response = serde_json::to_vec(&response)
            .map_err(|error| format!("encode workspace Plugin inspection failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    if request.path == "/skills/catalog" {
        let request =
            match serde_json::from_slice::<WorkspaceSkillCatalogRequest>(request.body.as_slice())
                .map_err(|error| format!("invalid json: {error}"))
            {
                Ok(value) if value.schema == WORKSPACE_SKILL_CATALOG_SCHEMA => value,
                Ok(_) => {
                    return json_error_response(400, "workspace_skill_catalog_schema_mismatch");
                }
                Err(error) => {
                    return json_error_response(400, error.as_str());
                }
            };
        let response = match workspace_skill_catalog(&request.plugin_activation) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("workspace skill catalog failed: {error}");
                return json_error_response(500, "workspace_skill_catalog_unavailable");
            }
        };
        let response = serde_json::to_vec(&response)
            .map_err(|error| format!("encode workspace skill catalog failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    if request.path == "/skills/detail" {
        let request =
            match serde_json::from_slice::<WorkspaceSkillDetailRequest>(request.body.as_slice())
                .map_err(|error| format!("invalid json: {error}"))
            {
                Ok(value) if value.schema == WORKSPACE_SKILL_DETAIL_SCHEMA => value,
                Ok(_) => {
                    return json_error_response(400, "workspace_skill_detail_schema_mismatch");
                }
                Err(error) => {
                    return json_error_response(400, error.as_str());
                }
            };
        let response =
            match workspace_skill_detail(&request.plugin_activation, request.skill_id.as_str()) {
                Ok(Some(value)) => value,
                Ok(None) => {
                    return json_error_response(404, "skill_not_found");
                }
                Err(error) => {
                    eprintln!("workspace skill detail failed: {error}");
                    return json_error_response(500, "workspace_skill_detail_unavailable");
                }
            };
        let response = serde_json::to_vec(&response)
            .map_err(|error| format!("encode workspace skill detail failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    if request.path == "/mcp/catalog" {
        let request =
            match serde_json::from_slice::<WorkspaceMcpCatalogRequest>(request.body.as_slice())
                .map_err(|error| format!("invalid json: {error}"))
            {
                Ok(value) if value.schema == WORKSPACE_MCP_CATALOG_SCHEMA => value,
                Ok(_) => return json_error_response(400, "workspace_mcp_catalog_schema_mismatch"),
                Err(error) => return json_error_response(400, error.as_str()),
            };
        let response = match workspace_mcp_catalog(&request.plugin_activation) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("workspace MCP catalog failed: {error}");
                return json_error_response(500, "workspace_mcp_catalog_unavailable");
            }
        };
        let response = serde_json::to_vec(&response)
            .map_err(|error| format!("encode workspace MCP catalog failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    if request.path == "/hooks/catalog" {
        let request =
            match serde_json::from_slice::<WorkspaceHookCatalogRequest>(request.body.as_slice())
                .map_err(|error| format!("invalid json: {error}"))
            {
                Ok(value) if value.schema == WORKSPACE_HOOK_CATALOG_SCHEMA => value,
                Ok(_) => return json_error_response(400, "workspace_hook_catalog_schema_mismatch"),
                Err(error) => return json_error_response(400, error.as_str()),
            };
        let response = match workspace_hook_catalog(&request.plugin_activation) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("workspace Hook catalog failed: {error}");
                return json_error_response(500, "workspace_hook_catalog_unavailable");
            }
        };
        let response = serde_json::to_vec(&response)
            .map_err(|error| format!("encode workspace Hook catalog failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    if request.path == "/agent-runs/cancel" {
        let request = match serde_json::from_slice::<AgentRunCancelRequest>(request.body.as_slice())
            .map_err(|error| format!("invalid json: {error}"))
            .and_then(|request| {
                let signing_key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
                    .map_err(|_| "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string())?;
                request.validate(signing_key.as_bytes())
            }) {
            Ok(value) => value,
            Err(error) => {
                return json_error_response(400, error.as_str());
            }
        };
        let agent_run_id = request.agent_run_start.agent_run_id.clone();
        let terminal_state =
            match load_existing_terminal_state(job_store.as_ref(), &request.agent_run_start) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("load AgentRun cancellation terminal state failed: {error}");
                    return json_error_response(500, "agent_run_cancel_state_unavailable");
                }
            };
        let (disposition, terminal_state) = if terminal_state.is_some() {
            ("terminal", terminal_state)
        } else {
            if let Err(error) = job_store.request_agent_run_cancellation(
                agent_run_id.as_str(),
                request.agent_run_start.authorization.session_id.as_str(),
                request.agent_run_start.authorization_digest.as_str(),
                now_ms()?,
            ) {
                match load_existing_terminal_state(job_store.as_ref(), &request.agent_run_start) {
                    Ok(Some(state)) => ("terminal", Some(state)),
                    Ok(None) => {
                        eprintln!("request AgentRun cancellation failed: {error}");
                        return json_error_response(409, "agent_run_cancel_rejected");
                    }
                    Err(reload_error) => {
                        eprintln!(
                            "reload AgentRun cancellation terminal state failed: {reload_error}; requestError={error}"
                        );
                        return json_error_response(500, "agent_run_cancel_state_unavailable");
                    }
                }
            } else {
                ("requested", None)
            }
        };
        let response = serde_json::to_vec(&json!({
            "schema": "runtime.agent_run.cancel.result.v1",
            "agentRunId": agent_run_id,
            "disposition": disposition,
            "terminalState": terminal_state,
        }))
        .map_err(|error| format!("encode AgentRun cancel response failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    if request.path == "/internal/agent-runs/supplement" {
        let request =
            match serde_json::from_slice::<AgentRunSupplementRequest>(request.body.as_slice())
                .map_err(|error| format!("invalid json: {error}"))
                .and_then(|request| {
                    let signing_key =
                        env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY").map_err(|_| {
                            "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string()
                        })?;
                    request.validate(signing_key.as_bytes())
                }) {
                Ok(value) => value,
                Err(error) => {
                    return json_error_response(400, error.as_str());
                }
            };
        let result = job_store.enqueue_turn_supplement(EnqueueTurnSupplementRequest {
            agent_run_id: request.agent_run_start.agent_run_id.clone(),
            lifecycle_job_id: request.job_id,
            session_id: request.agent_run_start.authorization.session_id.clone(),
            authorization_digest: request.agent_run_start.authorization_digest.clone(),
            supplement_id: request.supplement_id.clone(),
            message: request.message,
            created_at_ms: now_ms()?,
        });
        let result = match result {
            Ok(value) => value,
            Err(error) => {
                let status = turn_supplement_http_status(&error);
                if status == 500 {
                    eprintln!("AgentRun supplement admission failed: {error}");
                }
                return json_error_response(status, error.to_string().as_str());
            }
        };
        let disposition = match result.disposition {
            EnqueueTurnSupplementDisposition::Accepted => "accepted",
            EnqueueTurnSupplementDisposition::Duplicate => "duplicate",
        };
        let response = serde_json::to_vec(&json!({
            "schema": "runtime.agent_run.supplement.result.v1",
            "accepted": true,
            "disposition": disposition,
            "agentRunId": request.agent_run_start.agent_run_id,
            "sessionId": request.agent_run_start.authorization.session_id,
            "supplementId": request.supplement_id,
            "queuedCount": result.queued_count,
            "queueRevision": result.revision,
        }))
        .map_err(|error| format!("encode AgentRun supplement response failed: {error}"))?;
        return Ok(http_response(202, "application/json", response));
    }
    if request.path == "/agent-runs/teardown" {
        let teardown =
            match serde_json::from_slice::<AgentRunTeardownRequest>(request.body.as_slice())
                .map_err(|error| format!("invalid json: {error}"))
                .and_then(|request| {
                    let signing_key =
                        env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY").map_err(|_| {
                            "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string()
                        })?;
                    request.validate(signing_key.as_bytes())
                }) {
                Ok(value) => value,
                Err(error) => {
                    return json_error_response(400, error.as_str());
                }
            };
        if let Err(error) = validate_agent_run_lifecycle_job(
            teardown.job_id.as_str(),
            teardown.lease_owner.as_str(),
            &teardown.agent_run_start,
            job_store.as_ref(),
        ) {
            return json_error_response(409, error.as_str());
        }
        if load_existing_terminal_state(job_store.as_ref(), &teardown.agent_run_start)?.is_none() {
            return json_error_response(409, "agent_run_not_terminal");
        }
        if let Err(error) = job_store.close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
            agent_run_id: teardown.agent_run_start.agent_run_id.clone(),
            lifecycle_job_id: teardown.job_id.clone(),
            session_id: teardown.agent_run_start.authorization.session_id.clone(),
            authorization_digest: teardown.agent_run_start.authorization_digest.clone(),
            lease_owner: Some(teardown.lease_owner.clone()),
            reason: "agent_run_terminal".to_string(),
            closed_at_ms: now_ms()?,
        }) {
            eprintln!("turn supplement terminal cleanup failed: {error}");
            return json_error_response(500, "agent_run_supplement_cleanup_failed");
        }
        if let Err(error) =
            DockerExecutionHostRunner::teardown(teardown.agent_run_start.agent_run_id.as_str())
        {
            eprintln!("sandbox teardown failed: {error}");
            return json_error_response(500, "sandbox_teardown_failed");
        }
        let mut contexts = tool_layers
            .lock()
            .map_err(|_| "provider poll tool layer registry lock poisoned".to_string())?;
        let session_id = teardown.agent_run_start.authorization.session_id.as_str();
        if contexts
            .get(session_id)
            .is_some_and(|context| context.agent_run_id == teardown.agent_run_start.agent_run_id)
        {
            contexts.remove(session_id);
        }
        let response = serde_json::to_vec(&json!({
            "schema": "runtime.agent_run.teardown.result.v1",
            "agentRunId": teardown.agent_run_start.agent_run_id,
            "status": "removed",
        }))
        .map_err(|error| format!("encode AgentRun teardown response failed: {error}"))?;
        return Ok(http_response(200, "application/json", response));
    }
    let agent_run_step =
        match serde_json::from_slice::<AgentRunStepRequest>(request.body.as_slice())
            .map_err(|error| format!("invalid json: {error}"))
            .and_then(|request| {
                let signing_key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
                    .map_err(|_| "AGENT_RUN_AUTHORIZATION_SIGNING_KEY is required".to_string())?;
                request.validate(signing_key.as_bytes())
            }) {
            Ok(value) => value,
            Err(error) => {
                return json_error_response(400, error.as_str());
            }
        };
    if let Err(error) = validate_agent_run_lifecycle_job(
        agent_run_step.job_id.as_str(),
        agent_run_step.lease_owner.as_str(),
        &agent_run_step.agent_run_start,
        job_store.as_ref(),
    ) {
        return json_error_response(409, error.as_str());
    }
    let agent_run_id = agent_run_step.agent_run_start.agent_run_id.clone();
    let failure_agent_run_start = agent_run_step.agent_run_start.clone();
    let failure_job_id = agent_run_step.job_id.clone();
    let failure_lease_owner = agent_run_step.lease_owner.clone();
    let failure_runtime = runtime.clone();
    let failure_store = store.clone();
    let failure_job_store = job_store.clone();
    let execution = catch_agent_run_step_panic(|| {
        execute_agent_run(
            agent_run_step.agent_run_start,
            runtime,
            store,
            job_store,
            agent_run_step.job_id,
            agent_run_step.lease_owner,
            tool_layers.clone(),
        )
    });
    let outcome = match execution {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) if error == "agent_run_lifecycle_lease_lost" => {
            return json_error_response(409, error.as_str());
        }
        Ok(Err(error)) if session_delivery::pending(&error) => {
            failure_diagnostics::report(agent_run_id.as_str(), "session_record_commit", &error);
            session_delivery_waiting()
        }
        Ok(Err(error)) => match terminalize_agent_run_failure(
            failure_runtime.as_ref(),
            failure_store.as_ref(),
            failure_job_store.as_ref(),
            &failure_agent_run_start,
            failure_job_id.as_str(),
            failure_lease_owner.as_str(),
            AgentRunFailure {
                internal_error: error.as_str(),
                transition_reason: "runtime_internal_error",
            },
        ) {
            Ok(outcome) => outcome,
            Err(terminal_error) => {
                let retryable = terminal_error == "runtime_completed_projection_requires_recovery";
                eprintln!(
                    "AgentRun failure terminalization failed: {terminal_error}; rootCause={error}"
                );
                return agent_run_step_failure_response(
                    agent_run_id.as_str(),
                    if retryable {
                        "recovery_required"
                    } else {
                        "runtime_internal"
                    },
                    retryable,
                    "runtime_failure_terminalization_failed",
                );
            }
        },
        Err(error) => {
            match catch_agent_run_step_panic(|| {
                terminalize_agent_run_failure(
                    failure_runtime.as_ref(),
                    failure_store.as_ref(),
                    failure_job_store.as_ref(),
                    &failure_agent_run_start,
                    failure_job_id.as_str(),
                    failure_lease_owner.as_str(),
                    AgentRunFailure {
                        internal_error: error.as_str(),
                        transition_reason: "runtime_panic",
                    },
                )
            }) {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(terminal_error)) => {
                    let retryable =
                        terminal_error == "runtime_completed_projection_requires_recovery";
                    eprintln!(
                        "AgentRun panic terminalization failed: {terminal_error}; rootCause={error}"
                    );
                    return agent_run_step_failure_response(
                        agent_run_id.as_str(),
                        if retryable {
                            "recovery_required"
                        } else {
                            "runtime_panic"
                        },
                        retryable,
                        "runtime_failure_terminalization_failed",
                    );
                }
                Err(terminal_error) => {
                    eprintln!(
                        "AgentRun panic terminalization panicked: {terminal_error}; rootCause={error}"
                    );
                    return agent_run_step_failure_response(
                        agent_run_id.as_str(),
                        "runtime_panic",
                        false,
                        "runtime_failure_terminalization_failed",
                    );
                }
            }
        }
    };
    outcome.validate()?;
    let mut response = json!({
        "schema": "runtime.agent_run.step.result.v1",
        "agentRunId": agent_run_id,
        "disposition": outcome.disposition,
        "terminalState": outcome.terminal_state,
        "transitionReason": outcome.transition_reason,
    });
    if let Some(retry_at_ms) = outcome.retry_at_ms {
        response["retryAtMs"] = json!(retry_at_ms);
    }
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("encode AgentRun step response failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

fn turn_supplement_http_status(error: &TurnSupplementStoreError) -> u16 {
    match error {
        TurnSupplementStoreError::Validation(_) => 400,
        TurnSupplementStoreError::AgentRunNotActive
        | TurnSupplementStoreError::IdentityMismatch
        | TurnSupplementStoreError::QueueIdentityMismatch
        | TurnSupplementStoreError::AdmissionClosed
        | TurnSupplementStoreError::QueueFull
        | TurnSupplementStoreError::IdempotencyConflict => 409,
        TurnSupplementStoreError::IdentityRequired
        | TurnSupplementStoreError::JobIdMismatch
        | TurnSupplementStoreError::QueueCasConflict
        | TurnSupplementStoreError::ClaimIdentityInvalid
        | TurnSupplementStoreError::ClaimInProgress
        | TurnSupplementStoreError::QueueMissing
        | TurnSupplementStoreError::AcknowledgeIdentityInvalid
        | TurnSupplementStoreError::AcknowledgeIdentityMismatch
        | TurnSupplementStoreError::CloseReasonRequired
        | TurnSupplementStoreError::LeaseFenceRejected
        | TurnSupplementStoreError::Internal(_) => 500,
    }
}

fn workspace_skill_index(activation: &PluginActivationSnapshotV1) -> Result<SkillIndex, String> {
    SkillIndex::load(workspace_skill_catalog_config(activation)?)
}

fn workspace_skill_catalog(
    activation: &PluginActivationSnapshotV1,
) -> Result<WorkspaceSkillCatalogResult, String> {
    let index = workspace_skill_index(activation)?;
    Ok(WorkspaceSkillCatalogResult {
        schema: WORKSPACE_SKILL_CATALOG_RESULT_SCHEMA,
        skills: index
            .entries()
            .iter()
            .map(WorkspaceSkillSummary::from)
            .collect(),
    })
}

fn workspace_skill_detail(
    activation: &PluginActivationSnapshotV1,
    skill_id: &str,
) -> Result<Option<WorkspaceSkillDetailResult>, String> {
    let index = workspace_skill_index(activation)?;
    if index.find_by_id(skill_id).is_none() {
        return Ok(None);
    }
    let detail = index.detail(skill_id)?;
    Ok(Some(WorkspaceSkillDetailResult {
        schema: WORKSPACE_SKILL_DETAIL_RESULT_SCHEMA,
        skill: WorkspaceSkillSummary::from(&detail.skill),
        content: detail.content,
    }))
}

fn validate_agent_run_lifecycle_job(
    job_id: &str,
    lease_owner: &str,
    agent_run_start: &AgentRunStart,
    store: &PostgresRuntimeStore,
) -> Result<(), String> {
    let expected_job_id = agent_run_lifecycle_job_id(agent_run_start.agent_run_id.as_str())?;
    if job_id != expected_job_id {
        return Err("agent_run_lifecycle_job_id_mismatch".to_string());
    }
    let job = store
        .get_runtime_job(job_id)?
        .ok_or_else(|| "agent_run_lifecycle_job_missing".to_string())?;
    if job.job_kind != AGENT_RUN_LIFECYCLE_JOB_KIND
        || job.status != RuntimeJobStatus::Running
        || job.lease_owner.as_deref() != Some(lease_owner)
        || job
            .lease_expires_at_ms
            .is_none_or(|expires| expires <= now_ms().unwrap_or(i64::MAX))
        || job.session_id.as_deref() != Some(agent_run_start.authorization.session_id.as_str())
        || job.payload_ref.as_deref()
            != Some(format!("record:agent_run:{}", agent_run_start.agent_run_id).as_str())
        || job.idempotency_key
            != format!(
                "agent_run.lifecycle:{}:{}",
                agent_run_start.agent_run_id, agent_run_start.authorization_digest
            )
    {
        return Err("agent_run_lifecycle_job_binding_mismatch".to_string());
    }
    Ok(())
}

struct AgentRunStepOutcome {
    disposition: &'static str,
    terminal_state: Option<&'static str>,
    transition_reason: String,
    retry_at_ms: Option<i64>,
}

impl AgentRunStepOutcome {
    fn validate(&self) -> Result<(), String> {
        if (self.transition_reason == "execution_recovery_checkpoint_committed")
            != self.retry_at_ms.is_some()
            || self.retry_at_ms.is_some_and(|deadline| deadline < 0)
        {
            return Err("runtime AgentRun retry deadline is invalid".to_string());
        }
        match self.disposition {
            "waiting"
                if self.terminal_state.is_none()
                    && AGENT_RUN_WAITING_TRANSITION_REASONS
                        .contains(&self.transition_reason.as_str()) =>
            {
                Ok(())
            }
            "terminal"
                if matches!(
                    self.terminal_state,
                    Some("completed" | "failed" | "cancelled")
                ) && !self.transition_reason.trim().is_empty() =>
            {
                Ok(())
            }
            _ => Err("runtime AgentRun step outcome is invalid".to_string()),
        }
    }
}

fn requires_session_workspace_restore(has_started_fact: bool, completing_recovery: bool) -> bool {
    !has_started_fact && !completing_recovery
}

fn execution_recovery_waiting(retry_at_ms: i64) -> AgentRunStepOutcome {
    AgentRunStepOutcome {
        disposition: "waiting",
        terminal_state: None,
        transition_reason: "execution_recovery_checkpoint_committed".to_string(),
        retry_at_ms: Some(retry_at_ms),
    }
}

fn session_delivery_waiting() -> AgentRunStepOutcome {
    AgentRunStepOutcome {
        disposition: "waiting",
        terminal_state: None,
        transition_reason: "session_record_commit_unavailable".into(),
        retry_at_ms: None,
    }
}

fn recovery_retry_at_ms(
    schedule: &execution_recovery_schedule::ExecutionRecoverySchedule,
    start: &AgentRunStart,
    sequence: &AgentRunSessionState,
) -> Result<i64, String> {
    let ended_at = sequence
        .last_execution_ended_at_ms()
        .ok_or_else(|| "execution recovery requires an ended Execution".to_string())?;
    let (attempts, anchor) = sequence
        .recovery_attempt()
        .map_or((0, ended_at), |attempt| {
            (attempt.number, attempt.started_at_ms.max(ended_at))
        });
    schedule.retry_at_ms(start.agent_run_id.as_str(), attempts, anchor)
}

fn catch_agent_run_step_panic<T>(operation: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).map_err(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                panic
                    .downcast_ref::<&str>()
                    .map(|message| message.to_string())
            })
            .unwrap_or_else(|| "runtime panic without string payload".to_string())
    })
}

struct AgentRunFailure<'a> {
    internal_error: &'a str,
    transition_reason: &'a str,
}

fn terminalize_agent_run_failure(
    runtime: &tokio::runtime::Runtime,
    store: &RuntimeStoreActor,
    job_store: &PostgresRuntimeStore,
    agent_run_start: &AgentRunStart,
    lifecycle_job_id: &str,
    lifecycle_lease_owner: &str,
    failure: AgentRunFailure<'_>,
) -> Result<AgentRunStepOutcome, String> {
    let AgentRunFailure {
        internal_error,
        transition_reason,
    } = failure;
    failure_diagnostics::report(
        &agent_run_start.agent_run_id,
        "agent_run_lifecycle",
        internal_error,
    );
    if let Some(terminal_state) = load_existing_terminal_state(job_store, agent_run_start)? {
        return Ok(AgentRunStepOutcome {
            retry_at_ms: None,
            disposition: "terminal",
            terminal_state: Some(terminal_state),
            transition_reason: "runtime_session_terminal_committed".to_string(),
        });
    }
    if let Some(session) = SessionManager::new(store.clone())
        .load_session(agent_run_start.authorization.session_id.as_str())?
    {
        if let Some(projection) = session.completed_turn {
            projection.validate()?;
            if projection.agent_run_id != agent_run_start.agent_run_id
                || projection.authorization_digest != agent_run_start.authorization_digest
            {
                return Err("completed_turn_projection_identity_mismatch".to_string());
            }
            return Err("runtime_completed_projection_requires_recovery".to_string());
        }
    }
    let session_log = job_store
        .session_log(
            agent_run_start.authorization.workspace_id.clone(),
            agent_run_start.authorization.session_id.clone(),
            agent_run_start.prompt.clone(),
        )
        .with_agent_run_start(agent_run_start)?;
    let mut committed_sequence = load_existing_session_sequence(job_store, agent_run_start)?;
    let assistant_text = AssistantTextProjection::default();
    let events = failed_session_records(
        agent_run_start,
        transition_reason,
        &assistant_text,
        &mut committed_sequence,
        now_ms()?,
    )?;
    if !events.is_empty() {
        let receipt = append_agent_run_session_records(
            runtime,
            &session_log,
            agent_run_start,
            events.as_slice(),
            &RuntimeJobLeaseFence {
                job_id: lifecycle_job_id.to_string(),
                job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.to_string(),
                lease_owner: lifecycle_lease_owner.to_string(),
            },
        )?;
        let mut stream = None;
        accept_session_commit(&mut committed_sequence, &mut stream, &receipt)?;
    }
    Ok(AgentRunStepOutcome {
        retry_at_ms: None,
        disposition: "terminal",
        terminal_state: Some("failed"),
        transition_reason: transition_reason.to_string(),
    })
}

fn workspace_agent_tool_contracts(
    agent_run_start: &AgentRunStart,
) -> Vec<centaeris_core::tool::DynamicToolContract> {
    let mut contracts = workspace_tool_contracts();
    if agent_run_start.coordination_session_id.is_some() {
        contracts.push(agent_messages::message_tool_contract());
    }
    if agent_run_start.native_coordination_session_id.is_some() {
        contracts.push(agent_work::work_tool_contract());
        contracts.push(agent_work::work_query_tool_contract());
        contracts.push(agent_work::work_confirmation_tool_contract());
    }
    contracts
}

fn register_workspace_agent_tool_providers(
    tool_layer: &mut ToolLayer,
    agent_run_start: &AgentRunStart,
    api_url: &str,
    token: &str,
) -> Result<(), String> {
    if agent_run_start.coordination_session_id.is_some() {
        tool_layer.register_dynamic_tool_provider(Arc::new(
            agent_messages::WorkspaceAgentMessageProvider::new(
                api_url.to_owned(),
                token.to_owned(),
                &agent_run_start.authorization,
                agent_run_start.authorization_digest.clone(),
            )?,
        ))?;
    }
    if agent_run_start.native_coordination_session_id.is_some() {
        tool_layer.register_dynamic_tool_provider(Arc::new(
            agent_work::WorkspaceAgentWorkProvider::new(
                api_url.to_owned(),
                token.to_owned(),
                &agent_run_start.authorization,
                agent_run_start.authorization_digest.clone(),
            )?,
        ))?;
    }
    Ok(())
}

fn execute_agent_run(
    agent_run_start: AgentRunStart,
    runtime: Arc<tokio::runtime::Runtime>,
    store: Arc<RuntimeStoreActor>,
    job_store: Arc<PostgresRuntimeStore>,
    lifecycle_job_id: String,
    lifecycle_lease_owner: String,
    tool_layers: ToolLayerRegistry,
) -> Result<AgentRunStepOutcome, String> {
    let startup_started = Instant::now();
    let redis_url = env::var("REDIS_URL").map_err(|_| "REDIS_URL is required".to_string())?;
    let ttl_seconds = env::var("RUNTIME_STREAM_TTL_SECONDS")
        .map_err(|_| "RUNTIME_STREAM_TTL_SECONDS is required".to_string())?
        .parse::<i64>()
        .map_err(|_| "RUNTIME_STREAM_TTL_SECONDS must be an integer".to_string())?;
    let live_ttl_seconds = env::var("RUNTIME_LIVE_STATE_TTL_SECONDS")
        .map_err(|_| "RUNTIME_LIVE_STATE_TTL_SECONDS is required".to_string())?
        .parse::<i64>()
        .map_err(|_| "RUNTIME_LIVE_STATE_TTL_SECONDS must be an integer".to_string())?;
    let session_stream = Arc::new(Mutex::new(
        match TransientAgentRunStream::connect(
            redis_url.as_str(),
            agent_run_start.agent_run_id.as_str(),
            ttl_seconds,
            live_ttl_seconds,
        ) {
            Ok(stream) => Some(stream),
            Err(error) => {
                eprintln!(
                    "Redis transient stream unavailable at AgentRun start; durable AgentRun continues: {error}; transitionReason=redis_transient_unavailable"
                );
                None
            }
        },
    ));
    let api_url =
        env::var("API_INTERNAL_URL").map_err(|_| "API_INTERNAL_URL is required".to_string())?;
    let token =
        env::var("INTERNAL_API_TOKEN").map_err(|_| "INTERNAL_API_TOKEN is required".to_string())?;
    if let Some(terminal_state) =
        load_existing_terminal_state(job_store.as_ref(), &agent_run_start)?
    {
        acknowledge_terminal_completed_projection(store.as_ref(), &agent_run_start)?;
        return Ok(AgentRunStepOutcome {
            retry_at_ms: None,
            disposition: "terminal",
            terminal_state: Some(terminal_state),
            transition_reason: "runtime_session_terminal_committed".to_string(),
        });
    }
    let model_client = ApiModelClient::new(ApiModelClientConfig {
        api_internal_url: api_url.clone(),
        internal_api_token: token.clone(),
        agent_run_id: agent_run_start.agent_run_id.clone(),
        model_config_ref: agent_run_start.authorization.model_config_ref.clone(),
        authorization_ref: agent_run_start.authorization.id.clone(),
        authorization_digest: agent_run_start.authorization_digest.clone(),
        thinking_mode: agent_run_start.authorization.thinking_mode.clone(),
        model_max_output_tokens: agent_run_start.model_max_output_tokens,
    });
    let session_log = job_store
        .session_log(
            agent_run_start.authorization.workspace_id.clone(),
            agent_run_start.authorization.session_id.clone(),
            agent_run_start.prompt.clone(),
        )
        .with_agent_run_start(&agent_run_start)?;
    let terminal_lease_fence = RuntimeJobLeaseFence {
        job_id: lifecycle_job_id.clone(),
        job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.to_string(),
        lease_owner: lifecycle_lease_owner.clone(),
    };
    let session_record_sequence = Arc::new(Mutex::new(load_existing_session_sequence(
        job_store.as_ref(),
        &agent_run_start,
    )?));
    let has_started_fact = !session_record_sequence
        .lock()
        .map_err(|_| "session record sequence lock poisoned".to_string())?
        .is_empty();
    let active_execution = session_record_sequence
        .lock()
        .map_err(|_| "session record sequence lock poisoned".to_string())?
        .active_execution()
        .cloned();
    let recovery_checkpoint = select_agent_run_recovery_checkpoint(
        job_store.as_ref(),
        &agent_run_start,
        &*sequence_guard(&session_record_sequence)?,
    )?;
    let has_execution_fact = active_execution.is_some();
    let mut execution_id = active_execution
        .as_ref()
        .map(|execution| execution.execution_id.clone())
        .unwrap_or_else(|| initial_execution_id(&agent_run_start));
    let cancellation_job_store = job_store.clone();
    let cancellation_agent_run_id = agent_run_start.agent_run_id.clone();
    let cancellation_lifecycle_job_id = lifecycle_job_id.clone();
    let cancellation_lifecycle_lease_owner = lifecycle_lease_owner.clone();
    let cancellation_probe_cache = Mutex::new(None);
    let cancellation_probe: Arc<ExecutionCancellationProbe> = Arc::new(move || {
        cached_execution_control_reason(&cancellation_probe_cache, || {
            let now =
                now_ms().map_err(|error| format!("run_execution_control_probe_failed:{error}"))?;
            let state = cancellation_job_store
                .agent_run_execution_control_state(
                    cancellation_agent_run_id.as_str(),
                    cancellation_lifecycle_job_id.as_str(),
                    cancellation_lifecycle_lease_owner.as_str(),
                    now,
                )
                .map_err(|error| format!("run_execution_control_probe_failed:{error}"))?;
            Ok(if state.cancellation_requested {
                Some("agent_run_cancel_requested".to_string())
            } else if !state.lifecycle_lease_current {
                Some("agent_run_lifecycle_lease_lost".to_string())
            } else {
                None
            })
        })
    });
    match cancellation_probe.as_ref()()?.as_deref() {
        Some("agent_run_cancel_requested") => {
            return commit_cancelled_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
        Some("agent_run_lifecycle_lease_lost") => {
            return Err("agent_run_lifecycle_lease_lost".to_string());
        }
        _ => {}
    }
    let recovery_schedule = execution_recovery_schedule::ExecutionRecoverySchedule::from_env()?;
    if let Some((_, checkpoint)) = recovery_checkpoint.as_ref() {
        let mut sequence = sequence_guard(&session_record_sequence)?.clone();
        let attempts = sequence
            .recovery_attempt()
            .map_or(0, |attempt| attempt.number);
        if attempts >= recovery_schedule.maximum_attempts {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut sequence,
                &mut *session_stream_guard(&session_stream)?,
                "execution recovery attempt budget exhausted",
                "execution_recovery_exhausted",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
        let retry_at_ms = recovery_retry_at_ms(&recovery_schedule, &agent_run_start, &sequence)?;
        if now_ms()? < retry_at_ms {
            return Ok(execution_recovery_waiting(retry_at_ms));
        }
        let reserved = sequence.reserve_execution_recovery(
            agent_run_start.turn_id.as_str(),
            checkpoint.checkpoint_id.as_str(),
            recovery_schedule.maximum_attempts,
            now_ms()?,
        )?;
        let receipt = append_agent_run_session_records(
            runtime.as_ref(),
            &session_log,
            &agent_run_start,
            &[reserved],
            &terminal_lease_fence,
        )?;
        accept_session_commit(
            &mut sequence,
            &mut *session_stream_guard(&session_stream)?,
            &receipt,
        )?;
        execution_id = replacement_execution_id(
            &agent_run_start,
            checkpoint.checkpoint_id.as_str(),
            attempts + 1,
        );
        *sequence_guard(&session_record_sequence)? = sequence;
    }
    let workspace_skill_catalog_config =
        workspace_skill_catalog_config(&agent_run_start.authorization.plugin_activation)?;
    let mcp_credential_resolver = match McpCredentialResolver::new(
        api_url.clone(),
        token.clone(),
        agent_run_start.agent_run_id.clone(),
        agent_run_start.authorization.id.clone(),
        agent_run_start.authorization_digest.clone(),
    ) {
        Ok(resolver) => resolver,
        Err(error) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "mcp_start_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
    };
    let mcp_prepare_task = AbortOnDrop::new(runtime.spawn(prepare_http_mcp_servers(
        agent_run_start.authorization.plugin_activation.clone(),
        mcp_credential_resolver,
    )));
    let runtime_prelude_ms = startup_started.elapsed().as_millis();
    let sandbox_started = Instant::now();
    let docker_execution = DockerExecutionHostRunner::new(DockerExecutionHostRequest {
        agent_run_id: agent_run_start.agent_run_id.clone(),
        execution_id: execution_id.clone(),
        user_id: agent_run_start.authorization.user_id.clone(),
        agent_id: agent_run_start.authorization.agent_id.clone(),
        authorization_digest: agent_run_start.authorization_digest.clone(),
        image_digest: agent_run_start.authorization.image_digest.clone(),
        resources: agent_run_start.authorization.resources,
        has_execution_fact,
        api_url: api_url.clone(),
        api_token: token.clone(),
        plugin_activation: &agent_run_start.authorization.plugin_activation,
    });
    let docker_execution = match docker_execution {
        Ok(runner) => runner,
        Err(error) if has_execution_fact && error.starts_with("execution_environment_lost:") => {
            let checkpoint = latest_recovery_checkpoint(
                job_store.as_ref(),
                &agent_run_start,
                &*sequence_guard(&session_record_sequence)?,
                true,
            )?
            .ok_or_else(|| {
                "AgentRun execution environment was lost without a recovery checkpoint".to_string()
            })?;
            let mut ended_sequence = sequence_guard(&session_record_sequence)?.clone();
            let ended_execution_id = ended_sequence
                .active_execution_id()
                .ok_or_else(|| "AgentRun lost Execution identity is missing".to_string())?
                .to_string();
            let ended = ended_sequence.end_execution(
                agent_run_start.turn_id.as_str(),
                ended_execution_id.as_str(),
                "lost",
                "execution_environment_lost",
                true,
                Some(checkpoint.0.checkpoint_id.as_str()),
                ended_sequence.open_tool_call_ids(),
                now_ms()?,
            )?;
            let receipt = append_agent_run_session_records(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &[ended],
                &terminal_lease_fence,
            )?;
            accept_session_commit(
                &mut ended_sequence,
                &mut *session_stream_guard(&session_stream)?,
                &receipt,
            )?;
            *sequence_guard(&session_record_sequence)? = ended_sequence;
            let retry_at_ms = recovery_retry_at_ms(
                &recovery_schedule,
                &agent_run_start,
                &*sequence_guard(&session_record_sequence)?,
            )?;
            return Ok(execution_recovery_waiting(retry_at_ms));
        }
        Err(error) if recovery_checkpoint.is_some() && !error.contains("refusing replacement") => {
            let sequence = sequence_guard(&session_record_sequence)?;
            if sequence
                .recovery_attempt()
                .is_some_and(|attempt| attempt.number < recovery_schedule.maximum_attempts)
            {
                let retry_at_ms =
                    recovery_retry_at_ms(&recovery_schedule, &agent_run_start, &sequence)?;
                eprintln!(
                    "execution recovery preparation failed: agentRunId={}; error={error}",
                    agent_run_start.agent_run_id
                );
                return Ok(execution_recovery_waiting(retry_at_ms));
            }
            drop(sequence);
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "execution_recovery_exhausted",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
        Err(error) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "sandbox_prepare_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
    };
    let sandbox_ensure_ms = sandbox_started.elapsed().as_millis();
    let docker_execution = Arc::new(docker_execution);
    let lifecycle_hooks = match workspace_lifecycle_hook_runtime(
        &agent_run_start.authorization.plugin_activation,
        docker_execution.clone(),
        store.clone(),
        agent_run_start.authorization.session_id.clone(),
        agent_run_start.agent_run_id.clone(),
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "hook_start_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
    };
    let prepared_mcp_servers = match runtime.block_on(mcp_prepare_task.take()) {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "mcp_start_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
        Err(_) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                "MCP startup task failed",
                "mcp_start_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
    };
    let (mcp_bindings, mcp_startup_metrics) = match runtime.block_on(connect_mcp_servers(
        prepared_mcp_servers,
        docker_execution.clone(),
    )) {
        Ok(bindings) => bindings,
        Err(error) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "mcp_start_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
    };
    let artifact_publication = Arc::new(WorkspaceArtifactPublicationPort::new(
        docker_execution.clone(),
        api_url.clone(),
        token.clone(),
        agent_run_start.agent_run_id.clone(),
        agent_run_start.authorization_digest.clone(),
    )?);
    let workspace_root = PathBuf::from(WORKSPACE_DATA_ROOT);
    #[cfg(test)]
    let workspace_root = if native_consumption_tests::owns_run(&agent_run_start.agent_run_id) {
        std::env::temp_dir()
    } else {
        workspace_root
    };
    let execution_host_binding = Arc::new(ExecutionHostBinding::new(
        ExecutionHostMode::Remote,
        docker_execution.clone(),
        workspace_root.clone(),
        centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(
            workspace_root.as_path(),
        ),
    )?);
    let resolved_input_manifest = ResolvedInputManifest {
        schema: centaeris_core::tool::inputs::RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
        agent_run_id: agent_run_start.agent_run_id.clone(),
        authorization_digest: agent_run_start.authorization_digest.clone(),
        inputs: Vec::new(),
    };
    let resolved_inputs = Arc::new(ResolvedInputState::new(
        agent_run_start.agent_run_id.clone(),
        agent_run_start.authorization_digest.clone(),
        agent_run_start.authorization.asset_refs.clone(),
        resolved_input_manifest.clone(),
        Some(Arc::new(ApiDeferredInputResolver::new(
            api_url.clone(),
            token.clone(),
            agent_run_start.agent_run_id.clone(),
            agent_run_start.authorization_digest.clone(),
        ))),
    )?);
    let platform_materials = match runtime.block_on(async {
        #[cfg(test)]
        if native_consumption_tests::owns_run(&agent_run_start.agent_run_id) {
            // This owned fixture exercises native coordination with an empty
            // execution host. Material MCP discovery has its own protocol gate.
            assert!(agent_run_start.authorization.asset_refs.is_empty());
            return Ok(None);
        }
        if agent_run_start.authorization.asset_refs.is_empty()
            && agent_run_start.coordination_session_id.is_none()
        {
            return Ok(None);
        }
        platform_materials::PlatformMaterialsProvider::connect_current(
            api_url.clone(),
            token.clone(),
            agent_run_start.agent_run_id.clone(),
            agent_run_start.authorization_digest.clone(),
        )
        .await
        .map(|provider| Some(Arc::new(provider)))
    }) {
        Ok(provider) => provider,
        Err(error) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "mcp_start_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            )
        }
    };
    let mut dynamic_tool_contracts = workspace_agent_tool_contracts(&agent_run_start);
    if let Some(provider) = &platform_materials {
        dynamic_tool_contracts.extend(provider.contracts.clone());
    }
    dynamic_tool_contracts.extend(mcp_bindings.contracts);
    let dynamic_tool_registry = match DynamicToolRegistry::from_contracts(dynamic_tool_contracts) {
        Ok(registry) => Arc::new(registry),
        Err(error) if !mcp_bindings.providers.is_empty() || platform_materials.is_some() => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                error.as_str(),
                "mcp_start_failed",
                &AssistantTextProjection::default(),
                &terminal_lease_fence,
            );
        }
        Err(error) => return Err(error),
    };
    let mut tool_layer = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        workspace_skill_catalog_config,
        execution_host_binding,
    )?
    .with_dynamic_tool_registry(dynamic_tool_registry)
    .with_network_policy(centaeris_core::execution::NetworkPolicy::Disabled)
    .with_execution_cancellation_probe(cancellation_probe.clone())
    .with_session_id(agent_run_start.authorization.session_id.clone())
    .with_execution_owner(agent_run_start.agent_run_id.clone())
    .with_resource_claim_store(Arc::new((*store).clone()))
    .with_resolved_input_manifest(resolved_inputs.clone());
    tool_layer.register_dynamic_tool_provider(Arc::new(WorkspaceArtifactToolProvider::new(
        artifact_publication,
    )))?;
    register_workspace_agent_tool_providers(&mut tool_layer, &agent_run_start, &api_url, &token)?;
    for provider in mcp_bindings.providers {
        tool_layer.register_dynamic_tool_provider(provider)?;
    }
    if let Some(provider) = platform_materials {
        tool_layer.register_dynamic_tool_provider(provider)?;
    }
    let tool_layer =
        tool_layer.with_file_mutation_commit_port(Arc::new(WorkspaceFileMutationCommitPort::new(
            store.clone(),
            agent_run_start.authorization.session_id.clone(),
            agent_run_start.agent_run_id.clone(),
        )?));
    let agent_runtime_config = AgentRuntimeConfig {
        required_completion_tool: agent_run_start
            .coordination_session_id
            .is_some()
            .then(|| "send_message".to_string()),
        agent_instructions: agent_run_start.agent_instructions.clone(),
        model_context_tokens: agent_run_start.model_context_tokens,
        model_max_output_tokens: agent_run_start.model_max_output_tokens,
        ..AgentRuntimeConfig::default()
    };
    let tool_concurrency = ToolConcurrencyCoordinator::global_for_scope(
        format!("session:{}", agent_run_start.authorization.session_id),
        agent_runtime_config.tool_parallelism,
    )?;
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: agent_run_start.agent_run_id.clone(),
        execution_id: execution_id.clone(),
        authorization_digest: agent_run_start.authorization_digest.clone(),
    };
    tool_layers
        .lock()
        .map_err(|_| "provider poll tool layer registry lock poisoned".to_string())?
        .insert(
            agent_run_start.authorization.session_id.clone(),
            HostedRuntimeContext {
                agent_run_id: agent_run_start.agent_run_id.clone(),
                agent_run_identity: agent_run_identity.clone(),
                tool_layer: tool_layer.clone(),
                model_client: model_client.clone(),
                agent_runtime_config: agent_runtime_config.clone(),
                tool_concurrency: tool_concurrency.clone(),
            },
        );
    let agent_runtime = AgentRuntime::new(
        (*store).clone(),
        tool_layer,
        agent_runtime_config,
        tool_concurrency,
    )
    .with_lifecycle_hooks(lifecycle_hooks)
    .with_model_input_image_resolver(Arc::new(ApiModelInputImageResolver::new(
        resolved_inputs.clone(),
        api_url.clone(),
        token.clone(),
        agent_run_start.agent_run_id.clone(),
        agent_run_start.authorization_digest.clone(),
    )?));
    let model_config_store = EmptyModelSessionConfigStore::new();
    let assistant_text = Arc::new(Mutex::new(AssistantTextProjection::default()));
    let workspace_lease = SessionWorkspaceLease {
        job_id: lifecycle_job_id.clone(),
        lease_owner: lifecycle_lease_owner.clone(),
    };
    let workspace_input_upper_bound_bytes = workspace_input_upper_bound_bytes(&agent_run_start)?;
    if let Some((_, checkpoint)) = recovery_checkpoint.as_ref() {
        restore_runtime_state_from_recovery_checkpoint(
            job_store.as_ref(),
            store.as_ref(),
            checkpoint,
        )?;
    }
    let completed_projection = agent_runtime.load_completed_turn_projection(
        agent_run_start.authorization.session_id.as_str(),
        &agent_run_identity,
    )?;
    let completing_recovery = completed_projection.is_some();
    let workspace_restore_started = Instant::now();
    let workspace_resolution = match if let Some((_, checkpoint)) = recovery_checkpoint.as_ref() {
        if recovery_uses_session_workspace(
            &checkpoint.workspace_snapshot,
            &agent_run_start.authorization.session_workspace,
        )? {
            docker_execution.restore_session_workspace(
                &workspace_lease,
                &agent_run_start.authorization.session_workspace,
                workspace_input_upper_bound_bytes,
            )
        } else {
            docker_execution
                .restore_recovery_workspace(
                    &workspace_lease,
                    checkpoint.checkpoint_id.as_str(),
                    &checkpoint.workspace_snapshot,
                    workspace_input_upper_bound_bytes,
                )
                .map(|_| SessionWorkspaceResolution::Download)
        }
    } else if requires_session_workspace_restore(has_started_fact, completing_recovery) {
        docker_execution.restore_session_workspace(
            &workspace_lease,
            &agent_run_start.authorization.session_workspace,
            workspace_input_upper_bound_bytes,
        )
    } else {
        docker_execution.resolve_session_workspace(
            &workspace_lease,
            &agent_run_start.authorization.session_workspace,
        )
    } {
        Ok(resolution) => resolution,
        Err(SessionWorkspaceApiError::Unavailable(_)) => {
            eprintln!(
                "Session workspace resolve unavailable; transitionReason=session_workspace_resolve_unavailable"
            );
            return Ok(AgentRunStepOutcome {
                retry_at_ms: None,
                disposition: "waiting",
                terminal_state: None,
                transition_reason: "session_workspace_resolve_unavailable".to_string(),
            });
        }
        Err(SessionWorkspaceApiError::Rejected(reason)) => {
            return commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                reason.as_str(),
                if completing_recovery {
                    "session_workspace_resolve_failed"
                } else {
                    "session_workspace_restore_failed"
                },
                &*assistant_text_guard(&assistant_text)?,
                &terminal_lease_fence,
            );
        }
    };
    let workspace_restore_ms = workspace_restore_started.elapsed().as_millis();
    match completed_projection {
        Some(projection) if workspace_resolution == SessionWorkspaceResolution::Advanced => {
            let mut completed_sequence = sequence_guard(&session_record_sequence)?.clone();
            validate_completed_projection_session_log(
                job_store.as_ref(),
                &agent_run_start,
                &projection,
                &completed_sequence,
            )?;
            let mut events = Vec::new();
            if let Some(event) = end_active_execution(
                &mut completed_sequence,
                agent_run_start.agent_run_id.as_str(),
                "completed",
                "completed",
                false,
                now_ms()?,
            )? {
                events.push(event);
            }
            events.push(completed_sequence.complete(
                agent_run_start.agent_run_id.as_str(),
                projection.completion_reason.as_str(),
                now_ms()?,
            )?);
            let receipt = append_agent_run_session_records(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                events.as_slice(),
                &terminal_lease_fence,
            )?;
            accept_session_commit(
                &mut completed_sequence,
                &mut *session_stream_guard(&session_stream)?,
                &receipt,
            )?;
            *sequence_guard(&session_record_sequence)? = completed_sequence;
            agent_runtime.acknowledge_completed_turn_projection(
                agent_run_start.authorization.session_id.as_str(),
                &agent_run_identity,
            )?;
            return Ok(AgentRunStepOutcome {
                retry_at_ms: None,
                disposition: "terminal",
                terminal_state: Some("completed"),
                transition_reason: "runtime_completed_projection_recovered".to_string(),
            });
        }
        Some(_) => {
            let outcome = commit_failed_agent_run(
                runtime.as_ref(),
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &mut *session_stream_guard(&session_stream)?,
                "workspace commit was interrupted before acceptance",
                "session_workspace_commit_interrupted",
                &*assistant_text_guard(&assistant_text)?,
                &terminal_lease_fence,
            )?;
            agent_runtime.acknowledge_completed_turn_projection(
                agent_run_start.authorization.session_id.as_str(),
                &agent_run_identity,
            )?;
            return Ok(outcome);
        }
        None if workspace_resolution == SessionWorkspaceResolution::Advanced => {
            return Err("workspace_advanced_without_completed_projection".to_string());
        }
        None => {}
    }
    if !has_started_fact {
        if let Some((pending_turn_id, pending_identity)) = agent_runtime
            .pending_runtime_job_wait_identity(agent_run_start.authorization.session_id.as_str())?
        {
            if pending_identity == agent_run_identity {
                return Err("runtime_job_wait_missing_durable_start".to_string());
            }
            if !has_terminal_agent_run_identity(
                job_store.as_ref(),
                agent_run_start.authorization.session_id.as_str(),
                &pending_identity,
            )? {
                return Err("runtime_job_wait_previous_agent_run_not_terminal".to_string());
            }
            runtime.block_on(agent_runtime.abandon_pending_runtime_job_wait_async(
                agent_run_start.authorization.session_id.as_str(),
                pending_turn_id.as_str(),
                &pending_identity,
                "agent_run_terminal",
            ))?;
        }
    }
    // Recover display text only after Core verifies its durable request identity.
    // Redis remains disposable and cannot establish AgentRun lifecycle or model history.
    if has_started_fact {
        let recovered = session_stream_guard(&session_stream)?
            .as_mut()
            .map(|stream| {
                let meta = stream.read_live_meta()?;
                let reasoning = stream.read_live_reasoning()?;
                Ok::<_, String>(meta.zip(reasoning))
            })
            .transpose();
        match recovered {
            Ok(Some(Some((meta, reasoning)))) => {
                let mut sequence = sequence_guard(&session_record_sequence)?.clone();
                if let Some(event) =
                    sequence.recover_reasoning_snapshot(&meta.turn_id, &reasoning, now_ms()?)?
                {
                    let receipt = append_agent_run_session_records(
                        runtime.as_ref(),
                        &session_log,
                        &agent_run_start,
                        &[event],
                        &terminal_lease_fence,
                    )?;
                    accept_session_commit(
                        &mut sequence,
                        &mut *session_stream_guard(&session_stream)?,
                        &receipt,
                    )?;
                    *sequence_guard(&session_record_sequence)? = sequence;
                }
            }
            Err(error) => eprintln!("cached reasoning recovery unavailable: {error}"),
            _ => {}
        }
    }
    settle_existing_live_or_log(&mut *session_stream_guard(&session_stream)?);
    let message_input_states =
        preproject_message_inputs(&agent_run_start, resolved_inputs.as_ref())?;
    let mut session_start_commit_ms = 0;
    if !has_started_fact {
        let session_start_commit_started = Instant::now();
        let started_at_ms = now_ms()?;
        let mut committed_sequence = sequence_guard(&session_record_sequence)?.clone();
        let mut events =
            started_session_records(&agent_run_start, &mut committed_sequence, started_at_ms)?;
        events.push(committed_sequence.start_execution(
            agent_run_start.turn_id.as_str(),
            docker_execution.execution_id(),
            agent_run_start.authorization_digest.as_str(),
            None,
            started_at_ms,
        )?);
        let admission_result = session_delivery::retry(
            || {
                if matches!(
                    agent_run_start.tail_action,
                    AgentRunTailAction::RewriteLastUser { .. }
                ) {
                    append_agent_run_session_records(
                        runtime.as_ref(),
                        &session_log,
                        &agent_run_start,
                        events.as_slice(),
                        &terminal_lease_fence,
                    )
                } else {
                    commit_new_user_turn_admission(
                        job_store.as_ref(),
                        &session_log,
                        &agent_runtime,
                        &agent_run_start,
                        events.as_slice(),
                        &terminal_lease_fence,
                        started_at_ms,
                    )
                }
            },
            std::thread::sleep,
        );
        let receipt = session_delivery::admission(admission_result, || {
            DockerExecutionHostRunner::teardown(agent_run_start.agent_run_id.as_str())
        })?;
        accept_session_commit(
            &mut committed_sequence,
            &mut *session_stream_guard(&session_stream)?,
            &receipt,
        )?;
        refresh_runtime_state_after_admission(
            job_store.as_ref(),
            store.as_ref(),
            agent_run_start.authorization.session_id.as_str(),
            receipt.last_sequence()?,
        )?;
        *sequence_guard(&session_record_sequence)? = committed_sequence;
        session_start_commit_ms = session_start_commit_started.elapsed().as_millis();
    } else if !has_execution_fact {
        let session_start_commit_started = Instant::now();
        let checkpoint_id = recovery_checkpoint
            .as_ref()
            .map(|(record, _)| record.checkpoint_id.as_str())
            .ok_or_else(|| "replacement Execution recovery checkpoint is missing".to_string())?;
        let mut committed_sequence = sequence_guard(&session_record_sequence)?.clone();
        let event = committed_sequence.start_execution(
            agent_run_start.turn_id.as_str(),
            docker_execution.execution_id(),
            agent_run_start.authorization_digest.as_str(),
            Some(checkpoint_id),
            now_ms()?,
        )?;
        let receipt = append_agent_run_session_records(
            runtime.as_ref(),
            &session_log,
            &agent_run_start,
            &[event],
            &terminal_lease_fence,
        )?;
        accept_session_commit(
            &mut committed_sequence,
            &mut *session_stream_guard(&session_stream)?,
            &receipt,
        )?;
        *sequence_guard(&session_record_sequence)? = committed_sequence;
        session_start_commit_ms = session_start_commit_started.elapsed().as_millis();
    }
    eprintln!(
        "agent_run_startup_profile: agentRunId={}; serverCount={}; credentialCount={}; mcpConnectionCount={}; runtimePreludeMs={}; sandboxEnsureMs={}; mcpCredentialResolveMs={}; mcpDiscoveryMs={}; workspaceRestoreMs={}; sessionStartCommitMs={}; startupTotalMs={}",
        agent_run_start.agent_run_id,
        mcp_startup_metrics.server_count,
        mcp_startup_metrics.credential_count,
        mcp_startup_metrics.connection_count,
        runtime_prelude_ms,
        sandbox_ensure_ms,
        mcp_startup_metrics.credential_resolve_ms,
        mcp_startup_metrics.discovery_ms,
        workspace_restore_ms,
        session_start_commit_ms,
        startup_started.elapsed().as_millis(),
    );
    let supplement_session_log = session_log.clone();
    let supplement_agent_run_start = agent_run_start.clone();
    let supplement_sequence = session_record_sequence.clone();
    let supplement_lease_fence = terminal_lease_fence.clone();
    let supplement_stream = session_stream.clone();
    let materialize_supplements = Arc::new(
        move |turn_id: &str,
              supplements: &[centaeris_core::session::turn_input::DurableTurnInput]| {
            let mut guard = supplement_sequence
                .lock()
                .map_err(|_| "session record sequence lock poisoned".to_string())?;
            let mut committed_sequence = guard.clone();
            let mut events = Vec::new();
            for item in supplements {
                use centaeris_core::session::turn_input::TurnInputPayload;
                let event = match &item.payload {
                    TurnInputPayload::UserSupplement {
                        supplement_id,
                        message,
                        attachments,
                    } => committed_sequence.supplement_with_attachments(
                        turn_id,
                        supplement_id,
                        message,
                        attachments,
                        item.created_at_ms,
                    )?,
                    TurnInputPayload::HostEvent(input) => committed_sequence
                        .host_event_input_record(turn_id, input, item.created_at_ms)?,
                };
                if let Some(event) = event {
                    events.push(event);
                }
            }
            if events.is_empty() {
                return Ok(());
            }
            let receipt = supplement_session_log
                .append_session_records_with_runtime_job_lease_blocking(
                    supplement_agent_run_start.agent_run_id.as_str(),
                    events.as_slice(),
                    &supplement_lease_fence,
                )?;
            accept_session_commit(
                &mut committed_sequence,
                &mut *session_stream_guard(&supplement_stream)?,
                &receipt,
            )?;
            *guard = committed_sequence;
            Ok(())
        },
    );
    let input_binding = DurableTurnControlBinding {
        agent_run_id: agent_run_start.agent_run_id.clone(),
        lifecycle_job_id: lifecycle_job_id.clone(),
        session_id: agent_run_start.authorization.session_id.clone(),
        authorization_digest: agent_run_start.authorization_digest.clone(),
        lease_owner: lifecycle_lease_owner.clone(),
        claim_token: format!(
            "claim:{}:{}:{}",
            std::process::id(),
            lifecycle_lease_owner,
            now_ms()?
        ),
    };
    let turn_control = agent_inputs::turn_control(
        job_store.clone(),
        &agent_run_start,
        input_binding,
        materialize_supplements,
        Some(resolved_inputs.clone()),
    )?;
    let live_degraded = Arc::new(AtomicBool::new(false));
    let mut stream_error = None;
    let safe_point_sequence = session_record_sequence.clone();
    let safe_point_stream = session_stream.clone();
    let safe_point_live_degraded = live_degraded.clone();
    let workspace_checkpoint = match recovery_checkpoint.as_ref() {
        Some((_, checkpoint)) => Some(checkpoint.clone()),
        None if has_started_fact => latest_recovery_checkpoint(
            job_store.as_ref(),
            &agent_run_start,
            &*sequence_guard(&session_record_sequence)?,
            false,
        )?
        .map(|(_, checkpoint)| checkpoint),
        None => None,
    };
    let mut recovery_workspace_snapshot = workspace_checkpoint
        .as_ref()
        .map(|checkpoint| checkpoint.workspace_snapshot.clone())
        .unwrap_or(recovery_snapshot_from_session_workspace(
            &agent_run_start.authorization.session_workspace,
        )?);
    let mut recovery_workspace_generation = if has_execution_fact {
        workspace_checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.workspace_generation.clone())
            .unwrap_or_else(|| ExecutionWorkspaceGeneration::Unknown {
                reason: "active execution has no recovery checkpoint generation".to_string(),
            })
    } else {
        observed_workspace_generation(docker_execution.as_ref())
    };
    let recovery_boundary_committed = AtomicBool::new(false);
    let recovery_budget_exhausted = AtomicBool::new(false);
    let mut recovery_suffix = execution_recovery_policy::RecoverySuffix::default();
    let mut previous_checkpoint_sequence = workspace_checkpoint
        .as_ref()
        .map_or(0, |checkpoint| checkpoint.session_sequence);
    let work_trigger = if agent_run_start.native_coordination_session_id.is_some() {
        match agent_work_trigger::WorkCommitTrigger::new(runtime.handle().clone(), &api_url, &token)
        {
            Ok(trigger) => Some(trigger),
            Err(_) => {
                eprintln!("agent_work_trigger_deferred reason=setup");
                None
            }
        }
    } else {
        None
    };
    let mut commit_tool_safe_point = |safe_point: ToolSafePoint| {
        let stage = match &safe_point {
            ToolSafePoint::ModelRequestStarted(_) => "model_request_safe_point",
            ToolSafePoint::DurableToolCall { .. } => "tool_call_safe_point",
            ToolSafePoint::DurableReceipt { .. } => "tool_receipt_safe_point",
            _ => "other_safe_point",
        };
        failure_diagnostics::observe(&agent_run_start.agent_run_id, stage, || {
            if !safe_point_live_degraded.load(Ordering::Relaxed) {
                match flush_live_stream(&safe_point_stream) {
                    Ok(()) => {}
                    Err(error) => latch_live_error(&safe_point_live_degraded, error),
                }
            }
            let fatal_execution_reason = match &safe_point {
                ToolSafePoint::DurableReceipt { result, .. }
                    if matches!(
                        result.transition_reason.as_deref(),
                        Some("execution_cancellation_indeterminate")
                    ) =>
                {
                    result.transition_reason.clone()
                }
                _ => None,
            };
            let now = now_ms()?;
            let mut committed_sequence = safe_point_sequence
                .lock()
                .map_err(|_| "session record sequence lock poisoned".to_string())?
                .clone();
            let mut recovery_checkpoint = None;
            let mut ending_lost_execution = false;
            let events = match safe_point {
                ToolSafePoint::ReasoningCompleted {
                    session_id,
                    turn_id,
                    request_id,
                    text,
                    status,
                } => {
                    if session_id != committed_sequence.session_id() {
                        return Err("reasoning safe point Session identity mismatch".to_string());
                    }
                    committed_sequence
                        .record_reasoning_block(&turn_id, &request_id, &text, &status, now)?
                        .into_iter()
                        .collect()
                }
                ToolSafePoint::ModelRequestStarted(started) => 'checkpoint: {
                    let mut events =
                        committed_sequence.record_model_request_started(&started, now)?;
                    if !committed_sequence.open_tool_call_ids().is_empty() {
                        return Err(
                            "recovery checkpoint requires an empty in-flight tool set".to_string()
                        );
                    }
                    let model_request = events
                        .iter()
                        .rev()
                        .find(|record| {
                            record.event.event_type == SessionRecordType::ModelRequestStarted
                        })
                        .ok_or_else(|| "model request start fact is missing".to_string())?;
                    let model_request_id = model_request
                        .event
                        .payload
                        .get("requestId")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| "model request start requestId is missing".to_string())?;
                    let turn_id = model_request
                        .event
                        .turn_id
                        .as_deref()
                        .ok_or_else(|| "model request start turnId is missing".to_string())?;
                    let checkpoint_id =
                        recovery_checkpoint_id(execution_id.as_str(), model_request_id);
                    let witness = docker_execution.recovery_workspace_evidence()?;
                    // Only a typed dispatch gate can trigger replacement. Docker text,
                    // `executed: false`, and model output are never sufficient proof.
                    let pre_dispatch_loss = docker_execution.recovery_failure_evidence()
                    == execution_recovery_policy::HostFailureEvidence::PreDispatchProcessNotStarted;
                    if pre_dispatch_loss {
                        if committed_sequence
                            .recovery_attempt()
                            .map_or(0, |attempt| attempt.number)
                            >= recovery_schedule.maximum_attempts
                        {
                            recovery_budget_exhausted.store(true, Ordering::SeqCst);
                            return Err("execution_recovery_exhausted".to_string());
                        }
                        let mut workspace = witness.ok_or_else(|| {
                            "sandbox recovery has no in-process snapshot witness".to_string()
                        })?;
                        workspace.generation_known = matches!(
                            &recovery_workspace_generation,
                            ExecutionWorkspaceGeneration::Known { .. }
                        );
                        let evidence = execution_recovery_policy::RecoveryEvidence {
                            failure: docker_execution.recovery_failure_evidence(),
                            receipt_reported_executed: recovery_suffix.receipt_executed,
                            durable_receipt_sequence: committed_sequence
                                .committed_session_sequence(),
                            previous_checkpoint_sequence,
                            next_model_request_started: true,
                            open_tool_calls: committed_sequence.open_tool_call_ids().len(),
                            parallel_tool_calls: recovery_suffix.parallel_receipts,
                            tool_calls_since_checkpoint: recovery_suffix.calls,
                            receipts_since_checkpoint: recovery_suffix.receipts,
                            all_suffix_tools_are_bash: recovery_suffix.non_bash_calls == 0,
                            external_or_mcp_calls_since_checkpoint: recovery_suffix.non_bash_calls,
                            successful_receipts_since_checkpoint: recovery_suffix
                                .successful_receipts,
                            facts_since_checkpoint: recovery_suffix.facts,
                            workspace,
                        };
                        execution_recovery_coordinator::validate_sandbox_loss_recovery(
                            &evidence,
                            docker_execution.recovery_continuity(),
                            committed_sequence
                                .recovery_attempt()
                                .map_or(0, |attempt| attempt.number),
                            recovery_schedule.maximum_attempts,
                        )?;
                        ending_lost_execution = true;
                    }
                    let current_generation = if ending_lost_execution {
                        recovery_workspace_generation.clone()
                    } else {
                        observed_workspace_generation(docker_execution.as_ref())
                    };
                    let witness_stable = witness.is_some_and(|evidence| {
                        evidence.snapshot_activity_epoch == evidence.current_activity_epoch
                            && evidence.snapshot_host_instance == evidence.current_host_instance
                    });
                    let should_collect = !ending_lost_execution
                        && (!witness_stable
                            || !workspace_generations_match(
                                &recovery_workspace_generation,
                                &current_generation,
                            ));
                    let workspace_snapshot = if should_collect {
                        let Some(snapshot) = docker_execution.stage_recovery_workspace(
                            &workspace_lease,
                            checkpoint_id.as_str(),
                            &recovery_workspace_snapshot,
                            workspace_input_upper_bound_bytes,
                        )?
                        else {
                            // Commit the real model request, but neither publish an unstable
                            // checkpoint nor reset the tool suffix used by recovery policy.
                            break 'checkpoint events;
                        };
                        snapshot
                    } else {
                        recovery_workspace_snapshot.clone()
                    };
                    let checkpoint_generation = if should_collect {
                        let after_collect =
                            observed_workspace_generation(docker_execution.as_ref());
                        stable_workspace_generation(&current_generation, after_collect)
                    } else {
                        current_generation
                    };
                    let session_sequence = committed_sequence
                        .committed_session_sequence()
                        .checked_add(events.len() as u64)
                        .and_then(|value| value.checked_add(1))
                        .ok_or_else(|| {
                            "recovery checkpoint Session sequence overflow".to_string()
                        })?;
                    let payload = RuntimeRecoveryCheckpointV1 {
                        schema: RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1.to_string(),
                        checkpoint_id: checkpoint_id.clone(),
                        session_id: agent_run_start.authorization.session_id.clone(),
                        agent_run_id: agent_run_start.agent_run_id.clone(),
                        execution_id: execution_id.clone(),
                        authorization_digest: agent_run_start.authorization_digest.clone(),
                        session_sequence,
                        model_request_id: model_request_id.to_string(),
                        workspace_snapshot: workspace_snapshot.clone(),
                        workspace_generation: checkpoint_generation.clone(),
                        created_at_ms: now,
                    };
                    payload.validate()?;
                    let checkpoint = CheckpointRecord {
                        checkpoint_id,
                        kind: CheckpointKindV1::Recovery,
                        session_id: agent_run_start.authorization.session_id.clone(),
                        turn_id: turn_id.to_string(),
                        status: "committed".to_string(),
                        done_reason: None,
                        updated_at_ms: now,
                        payload_json: serde_json::to_string(&payload).map_err(|error| {
                            format!("encode recovery checkpoint failed: {error}")
                        })?,
                    };
                    events.push(committed_sequence.checkpoint_ref(&checkpoint)?);
                    if ending_lost_execution {
                        events.push(committed_sequence.end_execution(
                            checkpoint.turn_id.as_str(),
                            execution_id.as_str(),
                            "lost",
                            "execution_environment_lost",
                            true,
                            Some(checkpoint.checkpoint_id.as_str()),
                            Vec::new(),
                            now,
                        )?);
                    }
                    recovery_checkpoint =
                        Some((checkpoint, workspace_snapshot, checkpoint_generation));
                    events
                }
                ToolSafePoint::ProviderUsage {
                    turn_id,
                    usage,
                    recorded_at_ms,
                } => provider_usage_session_records(
                    &agent_run_start,
                    turn_id.as_str(),
                    &usage,
                    &mut committed_sequence,
                    recorded_at_ms,
                )?,
                ToolSafePoint::DurableToolCall {
                    session_id,
                    turn_id,
                    agent_run_id,
                    call,
                    provider_id,
                    tool_contract_digest,
                    recorded_at_ms,
                } => {
                    if !committed_sequence.has_tool_call(call.id.as_str()) {
                        recovery_suffix.record_call(call.name.as_str());
                    }
                    if session_id != agent_run_start.authorization.session_id
                        || agent_run_id != agent_run_start.agent_run_id
                    {
                        return Err("tool safe point AgentRun identity mismatch".to_string());
                    }
                    tool_call_session_records(
                        &agent_run_start,
                        Some(resolved_inputs.as_ref()),
                        turn_id.as_str(),
                        &call,
                        &mut committed_sequence,
                        ToolCallRecordContext {
                            provider_id: provider_id.as_str(),
                            tool_contract_digest: tool_contract_digest.as_str(),
                            created_at_ms: recorded_at_ms,
                        },
                    )?
                }
                ToolSafePoint::DurableReceipt {
                    session_id,
                    turn_id,
                    agent_run_id,
                    call,
                    result,
                } => {
                    if !committed_sequence.has_tool_result(result.tool_call_id.as_str()) {
                        recovery_suffix.record_receipt(&result);
                    }
                    if session_id != agent_run_start.authorization.session_id
                        || agent_run_id != agent_run_start.agent_run_id
                    {
                        return Err("tool safe point AgentRun identity mismatch".to_string());
                    }
                    tool_result_session_records(
                        &agent_run_start,
                        Some(resolved_inputs.as_ref()),
                        turn_id.as_str(),
                        &call,
                        &result,
                        &mut committed_sequence,
                        now,
                    )?
                }
                ToolSafePoint::CompletedTurn(turn) => tool_safe_point_session_records(
                    &agent_run_start,
                    Some(resolved_inputs.as_ref()),
                    &turn,
                    &mut committed_sequence,
                    now,
                )?,
            };
            if events.is_empty() {
                return fatal_execution_reason
                    .map(|reason| Err(format!("fatal_execution_outcome:{reason}")))
                    .unwrap_or(Ok(()));
            }
            let receipt = commit_agent_tool_records(
                &session_log,
                &agent_run_start,
                &events,
                recovery_checkpoint
                    .as_ref()
                    .map(|(checkpoint, _, _)| checkpoint),
                &terminal_lease_fence,
                work_trigger.as_ref(),
            )?;
            accept_session_commit(
                &mut committed_sequence,
                &mut *session_stream_guard(&safe_point_stream)?,
                &receipt,
            )?;
            *safe_point_sequence
                .lock()
                .map_err(|_| "session record sequence lock poisoned".to_string())? =
                committed_sequence;
            // Capture publication is downstream of the durable Core event and
            // must never change its successful tool outcome or trigger replay.
            if let Ok(mut capture) = docker_execution.transcript_capture.lock() {
                transcript_capture::publish_committed(
                    job_store.as_ref().clone(),
                    docker_execution.execution_id().to_string(),
                    docker_execution.transcript_publisher.clone(),
                    &mut capture,
                    &receipt,
                    usize::try_from(
                        agent_run_start
                            .authorization
                            .resources
                            .memory_bytes
                            .min(agent_run_start.authorization.resources.data_tmpfs_bytes),
                    )
                    .unwrap_or(usize::MAX),
                );
            }
            if let Some((checkpoint, workspace_snapshot, workspace_generation)) =
                recovery_checkpoint
            {
                recovery_workspace_snapshot = workspace_snapshot;
                recovery_workspace_generation = workspace_generation;
                previous_checkpoint_sequence =
                    serde_json::from_str::<RuntimeRecoveryCheckpointV1>(&checkpoint.payload_json)
                        .map_err(|error| {
                            format!("decode committed recovery checkpoint failed: {error}")
                        })?
                        .session_sequence;
                recovery_suffix = execution_recovery_policy::RecoverySuffix::default();
            }
            if ending_lost_execution {
                recovery_boundary_committed.store(true, Ordering::SeqCst);
                return Err("execution_recovery_yield".to_string());
            }
            if let Some(reason) = fatal_execution_reason {
                return Err(format!("fatal_execution_outcome:{reason}"));
            }
            Ok(())
        })
    };
    let agent_run_result = runtime.block_on(async {
        agent_runtime
            .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                AgentRunRequest {
                    session_id: agent_run_start.authorization.session_id.clone(),
                    agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
                        agent_run_id: agent_run_start.agent_run_id.clone(),
                        execution_id: execution_id.clone(),
                        authorization_digest: agent_run_start.authorization_digest.clone(),
                    }),
                    initial_turn_id: agent_run_start.turn_id.clone(),
                    initial_input: model_initial_input(
                        &agent_run_start,
                        resolved_inputs.as_ref(),
                        &message_input_states,
                    )?,
                    runtime_scope: PromptCompactionScopeV1::main(),
                    resume_from_turn_id: None,
                    auto_continue_after_resume_wait: Some(false),
                },
                &model_client,
                &model_config_store,
                &mut |event| match event {
                    TurnUpdate::Reasoning { turn_id, request_id, text, .. }
                        if !live_degraded.load(Ordering::Relaxed) => {
                            match session_stream.lock() {
                                Ok(mut guard) => if let Some(stream) = guard.as_mut() {
                                    if let Err(error) = stream.live_reasoning(&turn_id, &request_id, &text) { latch_live_error(&live_degraded, error); }
                                },
                                Err(_) => { stream_error = Some("session stream lock poisoned".to_string()); }
                            }
                    }
                    TurnUpdate::ModelRequestStart {
                        turn_id,
                        initial_content,
                        ..
                    } => {
                        if let Err(error) = assistant_text
                            .lock()
                            .map_err(|_| "assistant text lock poisoned".to_string())
                            .and_then(|mut guard| {
                                guard.begin_model_request(
                                    turn_id.clone(),
                                    initial_content.clone(),
                                )
                            })
                        {
                            if stream_error.is_none() {
                                stream_error = Some(error);
                            }
                        }
                        if !live_degraded.load(Ordering::Relaxed) {
                            match session_stream.lock() {
                                Ok(mut guard) => {
                                    if let Some(stream) = guard.as_mut() {
                                        let message_id = assistant_message_id(turn_id.as_str());
                                        match sequence_guard(&session_record_sequence)
                                            .map(|state| state.committed_session_sequence())
                                        {
                                            Ok(after_sequence) => {
                                                if let Err(error) = stream.live_open(
                                                    turn_id.as_str(),
                                                    message_id.as_str(),
                                                    after_sequence,
                                                    initial_content.as_str(),
                                                ) {
                                                    latch_live_error(
                                                        &live_degraded,
                                                        error,
                                                    );
                                                }
                                            }
                                            Err(error) => {
                                                *guard = None;
                                                eprintln!(
                                                    "Runtime committed position unavailable; durable AgentRun continues: {error}"
                                                );
                                            }
                                        }
                                    }
                                }
                                Err(_) if stream_error.is_none() => {
                                    stream_error = Some("session stream lock poisoned".to_string());
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    TurnUpdate::ToolCallReady {
                        turn_id,
                        call_id,
                        name,
                        args_json,
                        ..
                    } => {
                        flush_live_or_latch(&session_stream, &live_degraded);
                        if let Err(error) = assistant_text
                            .lock()
                            .map_err(|_| "assistant text lock poisoned".to_string())
                            .and_then(|mut guard| guard.mark_tool_call(turn_id.as_str()))
                        {
                            if stream_error.is_none() {
                                stream_error = Some(error);
                            }
                        }
                        let _ = (call_id, name, args_json);
                    }
                    TurnUpdate::Token {
                        turn_id, content, ..
                    } => {
                        let pushed = assistant_text
                            .lock()
                            .map_err(|_| "assistant text lock poisoned".to_string())
                            .and_then(|mut guard| {
                                guard.push_token(turn_id.as_str(), content.as_str())
                            });
                        match pushed {
                            Ok(()) => {
                                if !live_degraded.load(Ordering::Relaxed) {
                                    match session_stream.lock() {
                                        Ok(mut guard) => {
                                            if let Some(stream) = guard.as_mut() {
                                                let message_id =
                                                    assistant_message_id(turn_id.as_str());
                                                if let Err(error) = stream.live_append_delta(
                                                    turn_id.as_str(),
                                                    message_id.as_str(),
                                                    content.as_str(),
                                                ) {
                                                    latch_live_error(
                                                        &live_degraded,
                                                        error,
                                                    );
                                                }
                                            }
                                        }
                                        Err(_) if stream_error.is_none() => {
                                            stream_error =
                                                Some("session stream lock poisoned".to_string());
                                        }
                                        Err(_) => {}
                                    }
                                }
                            }
                            Err(error) if stream_error.is_none() => stream_error = Some(error),
                            Err(_) => {}
                        }
                    }
                    TurnUpdate::ReplaceContent {
                        turn_id, content, ..
                    } => {
                        if let Err(error) = assistant_text
                            .lock()
                            .map_err(|_| "assistant text lock poisoned".to_string())
                            .and_then(|mut guard| {
                                guard.replace_content(turn_id.as_str(), content.clone())
                            })
                        {
                            if stream_error.is_none() {
                                stream_error = Some(error);
                            }
                        }
                        if !live_degraded.load(Ordering::Relaxed) {
                            match session_stream.lock() {
                                Ok(mut guard) => {
                                    if let Some(stream) = guard.as_mut() {
                                        let message_id = assistant_message_id(turn_id.as_str());
                                        if let Err(error) = stream.live_replace(
                                            turn_id.as_str(),
                                            message_id.as_str(),
                                            content.as_str(),
                                        ) {
                                            latch_live_error(
                                                &live_degraded,
                                                error,
                                            );
                                        }
                                    }
                                }
                                Err(_) if stream_error.is_none() => {
                                    stream_error = Some("session stream lock poisoned".to_string());
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    TurnUpdate::ModelDone { turn_id, .. } => {
                        if let Err(error) = assistant_text
                            .lock()
                            .map_err(|_| "assistant text lock poisoned".to_string())
                            .and_then(|mut guard| guard.finish_model_request(turn_id.as_str()))
                        {
                            if stream_error.is_none() {
                                stream_error = Some(error);
                            }
                        }
                        flush_live_or_latch(&session_stream, &live_degraded);
                    }
                    TurnUpdate::RuntimeError { message, .. } => {
                        flush_live_or_latch(&session_stream, &live_degraded);
                        if stream_error.is_none() {
                            stream_error = Some(message);
                        }
                    }
                    TurnUpdate::RuntimeEvent { event } => {
                        flush_live_or_latch(&session_stream, &live_degraded);
                        if let Err(error) = persist_model_process_phase(
                            &session_log,
                            &agent_run_start,
                            &event,
                            &session_record_sequence,
                            &terminal_lease_fence,
                            &session_stream,
                        ) {
                            eprintln!(
                                "phase event projection degraded: agentRunId={}; eventId={}; error={error}; transitionReason=phase_event_projection_degraded",
                                agent_run_start.agent_run_id, event.event_id
                            );
                        }
                    }
                    _ => {}
                },
                cancellation_probe.as_ref(),
                &turn_control,
                &mut commit_tool_safe_point,
            )
            .await
    });
    let cancellation_reason = cancellation_probe.as_ref()()?;
    if cancellation_reason.as_deref() == Some("agent_run_lifecycle_lease_lost") {
        return Err("agent_run_lifecycle_lease_lost".to_string());
    }
    flush_live_or_latch(&session_stream, &live_degraded);
    // B' invariant check：健康路径内存是权威，Redis live text 必须与内存一致（degraded 时跳过）。
    if !live_degraded.load(Ordering::Relaxed) {
        let memory_text = assistant_text_guard(&assistant_text)?
            .responses
            .last()
            .map(|response| response.text.as_str())
            .unwrap_or_default()
            .to_string();
        let mut guard = session_stream_guard(&session_stream)?;
        if let Some(stream) = guard.as_mut() {
            match stream.read_live_text() {
                Ok(Some(live_text)) => {
                    if live_text != memory_text {
                        eprintln!(
                            "severe invariant violation: live text diverged from memory (live={} bytes, memory={} bytes); transitionReason=live_text_invariant_violation",
                            live_text.len(),
                            memory_text.len()
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!("live text invariant check unavailable: {error}");
                }
            }
        }
    }
    if cancellation_reason.as_deref() == Some("agent_run_cancel_requested") {
        if let Ok(response) = &agent_run_result {
            if matches!(
                &response.stop,
                AgentRunStop::RuntimeJobWait | AgentRunStop::QuestionWait
            ) {
                let wait_turn_id = response
                    .turn_responses
                    .last()
                    .ok_or_else(|| "agent_run_cancel_wait_response_missing".to_string())?
                    .checkpoint
                    .as_ref()
                    .ok_or_else(|| "agent_run_cancel_wait_checkpoint_missing".to_string())?
                    .turn_id
                    .clone();
                let cleanup = runtime.block_on(async {
                    agent_runtime
                        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
                            AgentRunRequest {
                                session_id: agent_run_start.authorization.session_id.clone(),
                                agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
                                    agent_run_id: agent_run_start.agent_run_id.clone(),
                                    execution_id: execution_id.clone(),
                                    authorization_digest: agent_run_start.authorization_digest.clone(),
                                }),
                                initial_turn_id: agent_run_start.turn_id.clone(),
                                initial_input: model_initial_input(
                                    &agent_run_start,
                                    resolved_inputs.as_ref(),
                                    &message_input_states,
                                )?,
                                runtime_scope: PromptCompactionScopeV1::main(),
                                resume_from_turn_id: Some(wait_turn_id),
                                auto_continue_after_resume_wait: Some(false),
                            },
                            &model_client,
                            &model_config_store,
                            &mut |_| {},
                            cancellation_probe.as_ref(),
                            &mut commit_tool_safe_point,
                        )
                        .await
                })?;
                if cleanup.stop != AgentRunStop::Cancelled("agent_run_cancel_requested".to_string())
                {
                    return Err("agent_run_cancel_wait_cleanup_incomplete".to_string());
                }
            }
        }
        return commit_cancelled_agent_run(
            &runtime,
            &session_log,
            &agent_run_start,
            &mut *sequence_guard(&session_record_sequence)?,
            &mut *session_stream_guard(&session_stream)?,
            &*assistant_text_guard(&assistant_text)?,
            &terminal_lease_fence,
        );
    }
    if recovery_boundary_committed.load(Ordering::SeqCst) {
        return Ok(execution_recovery_waiting(recovery_retry_at_ms(
            &recovery_schedule,
            &agent_run_start,
            &*sequence_guard(&session_record_sequence)?,
        )?));
    }
    if recovery_budget_exhausted.load(Ordering::SeqCst) {
        return commit_failed_agent_run(
            &runtime,
            &session_log,
            &agent_run_start,
            &mut *sequence_guard(&session_record_sequence)?,
            &mut *session_stream_guard(&session_stream)?,
            "execution recovery attempt budget exhausted",
            "execution_recovery_exhausted",
            &*assistant_text_guard(&assistant_text)?,
            &terminal_lease_fence,
        );
    }
    match (agent_run_result, stream_error) {
        (Err(error), _) if session_delivery::pending(&error) => Err(error),
        (Ok(response), _)
            if response.stop
                == AgentRunStop::Cancelled("agent_run_lifecycle_lease_lost".to_string()) =>
        {
            Err("agent_run_lifecycle_lease_lost".to_string())
        }
        (Err(error), _)
            if error.starts_with("agent_run_lifecycle_lease_probe_failed:")
                || error.starts_with("run_execution_control_probe_failed:") =>
        {
            Err(error)
        }
        (Ok(_), Some(error)) => commit_failed_agent_run(
            &runtime,
            &session_log,
            &agent_run_start,
            &mut *sequence_guard(&session_record_sequence)?,
            &mut *session_stream_guard(&session_stream)?,
            error.as_str(),
            "core_stream_failed",
            &*assistant_text_guard(&assistant_text)?,
            &terminal_lease_fence,
        ),
        (Ok(response), None)
            if matches!(
                &response.stop,
                AgentRunStop::RuntimeJobWait | AgentRunStop::QuestionWait
            ) =>
        {
            append_assistant_progress_records(
                &runtime,
                &session_log,
                &agent_run_start,
                &mut *sequence_guard(&session_record_sequence)?,
                &*assistant_text_guard(&assistant_text)?,
                &terminal_lease_fence,
                &mut *session_stream_guard(&session_stream)?,
            )?;
            if response.stop == AgentRunStop::RuntimeJobWait {
                let turn = response
                    .turn_responses
                    .last()
                    .ok_or("runtime job wait response missing")?;
                wait_recovery::WaitHandoffPublisher {
                    store: job_store.as_ref(),
                    log: &session_log,
                    host: docker_execution.as_ref(),
                    start: &agent_run_start,
                    fence: &terminal_lease_fence,
                    workspace_lease: &workspace_lease,
                    input_upper_bound_bytes: workspace_input_upper_bound_bytes,
                }
                .publish(
                    turn,
                    &mut *sequence_guard(&session_record_sequence)?,
                    &mut *session_stream_guard(&session_stream)?,
                )?;
            }
            Ok(AgentRunStepOutcome {
                retry_at_ms: None,
                disposition: "waiting",
                terminal_state: None,
                transition_reason: response.stop.reason().to_string(),
            })
        }
        (Ok(response), None) => {
            if !matches!(
                &response.stop,
                AgentRunStop::Finalized | AgentRunStop::TerminalTool
            ) {
                return commit_failed_agent_run(
                    &runtime,
                    &session_log,
                    &agent_run_start,
                    &mut *sequence_guard(&session_record_sequence)?,
                    &mut *session_stream_guard(&session_stream)?,
                    format!(
                        "unsupported AgentRun stop reason: {}",
                        response.stop.reason()
                    )
                    .as_str(),
                    "unsupported_runtime_stop_reason",
                    &*assistant_text_guard(&assistant_text)?,
                    &terminal_lease_fence,
                );
            }
            let mut completed_sequence = sequence_guard(&session_record_sequence)?.clone();
            let completed_events = match completed_session_records(
                &agent_run_start,
                Some(resolved_inputs.as_ref()),
                &*assistant_text_guard(&assistant_text)?,
                &response,
                &mut completed_sequence,
                now_ms()?,
            ) {
                Ok(events) => events,
                Err(error) => {
                    return commit_failed_agent_run(
                        &runtime,
                        &session_log,
                        &agent_run_start,
                        &mut *sequence_guard(&session_record_sequence)?,
                        &mut *session_stream_guard(&session_stream)?,
                        error.as_str(),
                        "semantic_projection_failed",
                        &*assistant_text_guard(&assistant_text)?,
                        &terminal_lease_fence,
                    );
                }
            };
            if let Err(error) = agent_runtime.prepare_completed_turn_projection(
                agent_run_start.authorization.session_id.as_str(),
                &agent_run_identity,
                &response,
            ) {
                return commit_failed_agent_run(
                    &runtime,
                    &session_log,
                    &agent_run_start,
                    &mut *sequence_guard(&session_record_sequence)?,
                    &mut *session_stream_guard(&session_stream)?,
                    error.as_str(),
                    "completed_projection_prepare_failed",
                    &*assistant_text_guard(&assistant_text)?,
                    &terminal_lease_fence,
                );
            }
            let final_workspace_generation =
                observed_workspace_generation(docker_execution.as_ref());
            if !workspace_generations_match(
                &recovery_workspace_generation,
                &final_workspace_generation,
            ) || !recovery_snapshot_matches_session_workspace(
                &recovery_workspace_snapshot,
                &agent_run_start.authorization.session_workspace,
            ) {
                match docker_execution.collect_and_commit_session_workspace(
                    &workspace_lease,
                    &agent_run_start.authorization.session_workspace,
                    workspace_input_upper_bound_bytes,
                ) {
                    Ok(
                        SessionWorkspaceCommitOutcome::Accepted
                        | SessionWorkspaceCommitOutcome::Unchanged,
                    ) => {}
                    Ok(SessionWorkspaceCommitOutcome::Pending) => {
                        return Ok(AgentRunStepOutcome {
                            retry_at_ms: None,
                            disposition: "waiting",
                            terminal_state: None,
                            transition_reason: "session_workspace_commit_unavailable".to_string(),
                        });
                    }
                    Ok(SessionWorkspaceCommitOutcome::Rejected(error)) | Err(error) => {
                        let outcome = commit_failed_agent_run(
                            &runtime,
                            &session_log,
                            &agent_run_start,
                            &mut *sequence_guard(&session_record_sequence)?,
                            &mut *session_stream_guard(&session_stream)?,
                            error.as_str(),
                            "session_workspace_commit_failed",
                            &*assistant_text_guard(&assistant_text)?,
                            &terminal_lease_fence,
                        )?;
                        agent_runtime.acknowledge_completed_turn_projection(
                            agent_run_start.authorization.session_id.as_str(),
                            &agent_run_identity,
                        )?;
                        return Ok(outcome);
                    }
                }
            }
            let receipt = append_agent_run_session_records(
                &runtime,
                &session_log,
                &agent_run_start,
                &completed_events,
                &terminal_lease_fence,
            )?;
            let mut completed_sequence = sequence_guard(&session_record_sequence)?.clone();
            accept_session_commit(
                &mut completed_sequence,
                &mut *session_stream_guard(&session_stream)?,
                &receipt,
            )?;
            *sequence_guard(&session_record_sequence)? = completed_sequence;
            agent_runtime.acknowledge_completed_turn_projection(
                agent_run_start.authorization.session_id.as_str(),
                &agent_run_identity,
            )?;
            assistant_text_guard(&assistant_text)?
                .responses
                .last()
                .ok_or_else(|| "final assistant response is missing".to_string())?;
            Ok(AgentRunStepOutcome {
                retry_at_ms: None,
                disposition: "terminal",
                terminal_state: Some("completed"),
                transition_reason: "runtime_session_terminal_committed".to_string(),
            })
        }
        (Err(error), _) => commit_failed_agent_run(
            &runtime,
            &session_log,
            &agent_run_start,
            &mut *sequence_guard(&session_record_sequence)?,
            &mut *session_stream_guard(&session_stream)?,
            error.as_str(),
            "core_run_failed",
            &*assistant_text_guard(&assistant_text)?,
            &terminal_lease_fence,
        ),
    }
}

fn assistant_message_id(turn_id: &str) -> String {
    format!("message:{turn_id}:assistant")
}

fn sequence_guard(
    sequence: &Arc<Mutex<AgentRunSessionState>>,
) -> Result<std::sync::MutexGuard<'_, AgentRunSessionState>, String> {
    sequence
        .lock()
        .map_err(|_| "session record sequence lock poisoned".to_string())
}

fn assistant_text_guard(
    assistant_text: &Arc<Mutex<AssistantTextProjection>>,
) -> Result<std::sync::MutexGuard<'_, AssistantTextProjection>, String> {
    assistant_text
        .lock()
        .map_err(|_| "assistant text lock poisoned".to_string())
}

fn session_stream_guard(
    stream: &Arc<Mutex<Option<TransientAgentRunStream>>>,
) -> Result<std::sync::MutexGuard<'_, Option<TransientAgentRunStream>>, String> {
    stream
        .lock()
        .map_err(|_| "session stream lock poisoned".to_string())
}

fn accept_session_commit(
    state: &mut AgentRunSessionState,
    stream: &mut Option<TransientAgentRunStream>,
    receipt: &SessionCommitReceipt,
) -> Result<(), String> {
    accept_session_commit_position(state, receipt)?;
    if let Some((turn_id, message_id, committed_sequence)) =
        committed_assistant_supersession(receipt)
    {
        settle_live_before_sequence_or_log(
            stream,
            turn_id.as_str(),
            message_id.as_str(),
            committed_sequence,
        );
    }
    publish_commit_wake_or_log(stream, receipt);
    Ok(())
}

fn accept_session_commit_position(
    state: &mut AgentRunSessionState,
    receipt: &SessionCommitReceipt,
) -> Result<(), String> {
    state.set_committed_session_sequence(
        state
            .committed_session_sequence()
            .max(receipt.last_sequence()?),
    );
    Ok(())
}

fn publish_commit_wake_or_log(
    stream: &mut Option<TransientAgentRunStream>,
    receipt: &SessionCommitReceipt,
) {
    let Some(active) = stream.as_mut() else {
        return;
    };
    let high_water_sequence = projected_commit_high_water(receipt);
    let result = high_water_sequence.map_or(Ok(()), |sequence| {
        active.publish_commit_wake(sequence).map(|_| ())
    });
    if let Err(error) = result {
        *stream = None;
        eprintln!("Redis commit wake unavailable; Postgres remains authoritative: {error}");
    }
}

fn projected_commit_high_water(receipt: &SessionCommitReceipt) -> Option<u64> {
    receipt
        .records
        .iter()
        .filter(|record| session_record_projects_to_agent_run_stream(record.event.event_type))
        .map(|record| record.sequence)
        .max()
}

fn committed_assistant_supersession(
    receipt: &SessionCommitReceipt,
) -> Option<(String, String, u64)> {
    receipt
        .records
        .iter()
        .filter(|record| record.event.event_type == SessionRecordType::AssistantMessage)
        .filter_map(|record| {
            record.event.turn_id.as_ref().map(|turn_id| {
                (
                    turn_id.clone(),
                    assistant_message_id(turn_id.as_str()),
                    record.sequence,
                )
            })
        })
        .max_by_key(|(_, _, sequence)| *sequence)
}

fn settle_live_before_sequence_or_log(
    stream: &mut Option<TransientAgentRunStream>,
    turn_id: &str,
    message_id: &str,
    committed_sequence: u64,
) {
    let Some(active) = stream.as_mut() else {
        return;
    };
    log_live_supersession_result(active.live_seal_before_sequence(
        turn_id,
        message_id,
        committed_sequence,
    ));
}

fn log_live_supersession_result(result: Result<bool, LiveTextError>) {
    if let Err(error) = result {
        eprintln!(
            "committed live overlay cleanup unavailable; durable session remains authoritative: {error}"
        );
    }
}

fn settle_existing_live_or_log(stream: &mut Option<TransientAgentRunStream>) {
    let result = stream.as_mut().map(|active| {
        active
            .read_live_meta()
            .map_err(|error| format!("read live meta failed: {error}"))
            .and_then(|meta| {
                meta.map_or(Ok(()), |meta| {
                    active
                        .settle_live_meta(&meta)
                        .map_err(|error| error.to_string())
                })
            })
    });
    if let Some(Err(error)) = result {
        *stream = None;
        eprintln!("stale live overlay cleanup unavailable; durable AgentRun continues: {error}");
    }
}

fn flush_live_stream(
    stream: &Arc<Mutex<Option<TransientAgentRunStream>>>,
) -> Result<(), LiveTextError> {
    let mut guard = stream
        .lock()
        .map_err(|_| LiveTextError::Fatal("session stream lock poisoned".to_string()))?;
    if let Some(active) = guard.as_mut() {
        active.live_flush()?;
    }
    Ok(())
}

fn flush_live_or_latch(
    stream: &Arc<Mutex<Option<TransientAgentRunStream>>>,
    live_degraded: &Arc<AtomicBool>,
) {
    if live_degraded.load(Ordering::Relaxed) {
        return;
    }
    if let Err(error) = flush_live_stream(stream) {
        latch_live_error(live_degraded, error);
    }
}

#[derive(Debug, Default)]
struct AssistantTextProjection {
    responses: Vec<AssistantResponse>,
}

impl AssistantTextProjection {
    fn begin_model_request(
        &mut self,
        turn_id: String,
        initial_content: String,
    ) -> Result<(), String> {
        if self
            .responses
            .last()
            .is_some_and(|response| response.turn_id == turn_id)
        {
            return Ok(());
        }
        if self.responses.last().is_some_and(|response| !response.done) {
            return Err("model request started before the previous response completed".to_string());
        }
        self.responses.push(AssistantResponse {
            turn_id,
            text: initial_content,
            ..AssistantResponse::default()
        });
        Ok(())
    }

    fn push_token(&mut self, turn_id: &str, content: &str) -> Result<(), String> {
        let response = self.current_mut(turn_id)?;
        response.text.push_str(content);
        Ok(())
    }

    fn replace_content(&mut self, turn_id: &str, content: String) -> Result<(), String> {
        self.current_mut(turn_id)?.text = content;
        Ok(())
    }

    fn mark_tool_call(&mut self, turn_id: &str) -> Result<(), String> {
        self.current_mut(turn_id)?.has_tool_call = true;
        Ok(())
    }

    fn finish_model_request(&mut self, turn_id: &str) -> Result<(), String> {
        self.current_mut(turn_id)?.done = true;
        Ok(())
    }

    fn terminal_text(&self) -> &str {
        let Some(response) = self.responses.last() else {
            return "";
        };
        if response.has_tool_call {
            ""
        } else {
            response.text.as_str()
        }
    }

    fn current_mut(&mut self, turn_id: &str) -> Result<&mut AssistantResponse, String> {
        self.responses
            .last_mut()
            .filter(|response| response.turn_id == turn_id)
            .ok_or_else(|| format!("assistant response turn mismatch: {turn_id}"))
    }
}

#[derive(Debug, Default)]
struct AssistantResponse {
    turn_id: String,
    text: String,
    has_tool_call: bool,
    done: bool,
}

#[allow(clippy::too_many_arguments)]
fn commit_failed_agent_run(
    runtime: &tokio::runtime::Runtime,
    session_log: &PostgresSessionLog,
    agent_run_start: &AgentRunStart,
    session_record_sequence: &mut AgentRunSessionState,
    session_stream: &mut Option<TransientAgentRunStream>,
    internal_error: &str,
    transition_reason: &str,
    assistant_text: &AssistantTextProjection,
    lease_fence: &RuntimeJobLeaseFence,
) -> Result<AgentRunStepOutcome, String> {
    failure_diagnostics::report(&agent_run_start.agent_run_id, "core_turn", internal_error);
    if provider_response_was_interrupted(internal_error) {
        return commit_interrupted_agent_run(
            runtime,
            session_log,
            agent_run_start,
            session_record_sequence,
            session_stream,
            assistant_text,
            lease_fence,
            "lost",
            "provider_interrupted",
            "The model response was interrupted. Retry the request.",
            true,
            "provider_response_interrupted",
        );
    }
    if execution_environment_was_lost(internal_error) {
        return commit_interrupted_agent_run(
            runtime,
            session_log,
            agent_run_start,
            session_record_sequence,
            session_stream,
            assistant_text,
            lease_fence,
            "lost",
            "stopped",
            "The execution environment was interrupted. Retry the request.",
            true,
            "execution_environment_lost",
        );
    }
    let mut committed_sequence = session_record_sequence.clone();
    let events = failed_session_records(
        agent_run_start,
        public_agent_failure_reason(internal_error, transition_reason),
        assistant_text,
        &mut committed_sequence,
        now_ms()?,
    )?;
    if !events.is_empty() {
        let receipt = append_agent_run_session_records(
            runtime,
            session_log,
            agent_run_start,
            events.as_slice(),
            lease_fence,
        )?;
        accept_session_commit(&mut committed_sequence, session_stream, &receipt)?;
        *session_record_sequence = committed_sequence;
    }
    Ok(AgentRunStepOutcome {
        retry_at_ms: None,
        disposition: "terminal",
        terminal_state: Some("failed"),
        transition_reason: transition_reason.to_string(),
    })
}

fn commit_cancelled_agent_run(
    runtime: &tokio::runtime::Runtime,
    session_log: &PostgresSessionLog,
    agent_run_start: &AgentRunStart,
    session_record_sequence: &mut AgentRunSessionState,
    session_stream: &mut Option<TransientAgentRunStream>,
    assistant_text: &AssistantTextProjection,
    lease_fence: &RuntimeJobLeaseFence,
) -> Result<AgentRunStepOutcome, String> {
    commit_interrupted_agent_run(
        runtime,
        session_log,
        agent_run_start,
        session_record_sequence,
        session_stream,
        assistant_text,
        lease_fence,
        "cancelled",
        "cancelled",
        "AgentRun cancelled by user.",
        false,
        "agent_run_cancelled",
    )
}

#[allow(clippy::too_many_arguments)]
fn commit_interrupted_agent_run(
    runtime: &tokio::runtime::Runtime,
    session_log: &PostgresSessionLog,
    agent_run_start: &AgentRunStart,
    session_record_sequence: &mut AgentRunSessionState,
    session_stream: &mut Option<TransientAgentRunStream>,
    assistant_text: &AssistantTextProjection,
    lease_fence: &RuntimeJobLeaseFence,
    execution_outcome: &str,
    reason_type: &str,
    message: &str,
    retryable: bool,
    transition_reason: &str,
) -> Result<AgentRunStepOutcome, String> {
    let mut committed_sequence = session_record_sequence.clone();
    let events = interrupted_session_records(
        agent_run_start,
        assistant_text,
        &mut committed_sequence,
        now_ms()?,
        Interruption {
            execution_outcome,
            reason_type,
            message,
            retryable,
        },
    )?;
    if !events.is_empty() {
        let receipt = append_agent_run_session_records(
            runtime,
            session_log,
            agent_run_start,
            events.as_slice(),
            lease_fence,
        )?;
        accept_session_commit(&mut committed_sequence, session_stream, &receipt)?;
        *session_record_sequence = committed_sequence;
    }
    if events.is_empty() {
        session_log.commit_pre_admission_cancellation(
            &agent_run_start.agent_run_id,
            &agent_run_start.authorization_digest,
            lease_fence,
        )?;
    }
    Ok(AgentRunStepOutcome {
        retry_at_ms: None,
        disposition: "terminal",
        terminal_state: Some("cancelled"),
        transition_reason: transition_reason.to_string(),
    })
}

struct Interruption<'a> {
    execution_outcome: &'a str,
    reason_type: &'a str,
    message: &'a str,
    retryable: bool,
}

fn interrupted_session_records(
    agent_run_start: &AgentRunStart,
    assistant_text: &AssistantTextProjection,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
    interruption: Interruption<'_>,
) -> Result<Vec<SequencedSessionRecord>, String> {
    if sequence.is_empty() {
        // Pre-admission cancellation/interruption: keep the input in the
        // control plane and do not enter the model history.
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    if assistant_text
        .responses
        .iter()
        .any(|response| !response.text.trim().is_empty())
    {
        events.extend(assistant_session_records(
            agent_run_start,
            assistant_text,
            Some("error"),
            sequence,
            created_at_ms,
        )?);
    }
    if let Some(event) = end_active_execution(
        sequence,
        agent_run_start.agent_run_id.as_str(),
        interruption.execution_outcome,
        interruption.reason_type,
        interruption.retryable,
        created_at_ms,
    )? {
        events.push(event);
    }
    events.push(sequence.interrupt(
        agent_run_start.agent_run_id.as_str(),
        interruption.reason_type,
        interruption.message,
        interruption.retryable,
        created_at_ms,
    )?);
    Ok(events)
}

fn model_initial_input(
    start: &AgentRunStart,
    inputs: &ResolvedInputState,
    states: &HashMap<String, DeferredInputResolutionFailureKind>,
) -> Result<AgentRunInitialInput, String> {
    match &start.initial_input {
        AgentRunStartInitialInput::UserMessage {} => {
            model_user_message(start, inputs, states).map(AgentRunInitialInput::UserMessage)
        }
        AgentRunStartInitialInput::UserInput {
            input_id, message, ..
        } => Ok(AgentRunInitialInput::UserInput {
            input_id: input_id.clone(),
            message: message.clone(),
            attachments: contract::initial_input_attachments(start)?,
        }),
        AgentRunStartInitialInput::HostEvent { input, .. } => {
            Ok(AgentRunInitialInput::HostEvent(input.clone()))
        }
    }
}

fn latch_live_error(live_degraded: &Arc<AtomicBool>, error: LiveTextError) {
    live_degraded.store(true, Ordering::Relaxed);
    eprintln!(
        "live text streaming degraded to memory-only: {error}; transitionReason=live_text_degraded"
    );
}

fn persist_model_process_phase(
    session_log: &PostgresSessionLog,
    agent_run_start: &AgentRunStart,
    event: &RuntimeEventProjection,
    sequence: &Mutex<AgentRunSessionState>,
    lease_fence: &RuntimeJobLeaseFence,
    stream: &Mutex<Option<TransientAgentRunStream>>,
) -> Result<(), String> {
    let mut guard = sequence
        .lock()
        .map_err(|_| "session record sequence lock poisoned".to_string())?;
    let mut committed_sequence = guard.clone();
    let Some(record) =
        model_process_phase_session_record(agent_run_start, event, &mut committed_sequence)?
    else {
        return Ok(());
    };
    let receipt = session_log.append_session_records_with_runtime_job_lease_blocking(
        agent_run_start.agent_run_id.as_str(),
        std::slice::from_ref(&record),
        lease_fence,
    )?;
    let committed_phase_sequence = committed_phase_sequence(&receipt, &record)?;
    accept_session_commit_position(&mut committed_sequence, &receipt)?;
    match stream.lock() {
        Ok(mut active_stream) => {
            settle_live_before_sequence_or_log(
                &mut active_stream,
                event.turn_id.as_str(),
                assistant_message_id(event.turn_id.as_str()).as_str(),
                committed_phase_sequence,
            );
            publish_commit_wake_or_log(&mut active_stream, &receipt);
        }
        Err(_) => {
            eprintln!(
                "Redis phase overlay cleanup and commit wake unavailable; Postgres remains authoritative: session stream lock poisoned"
            );
        }
    }
    *guard = committed_sequence;
    Ok(())
}

fn committed_phase_sequence(
    receipt: &SessionCommitReceipt,
    expected: &SequencedSessionRecord,
) -> Result<u64, String> {
    receipt
        .records
        .iter()
        .find(|record| {
            record.event.event_type == SessionRecordType::PhaseEvent
                && record.event.event_id == expected.event.event_id
        })
        .map(|record| record.sequence)
        .ok_or_else(|| "phase commit receipt is missing the durable phase event".to_string())
}

fn model_process_phase_session_record(
    agent_run_start: &AgentRunStart,
    event: &RuntimeEventProjection,
    sequence: &mut AgentRunSessionState,
) -> Result<Option<SequencedSessionRecord>, String> {
    if event.event_type != "Status"
        || event
            .payload
            .get("stage")
            .and_then(serde_json::Value::as_str)
            != Some("model_process_summary")
    {
        return Ok(None);
    }
    if event.session_id != agent_run_start.authorization.session_id {
        return Err("model process phase session identity mismatch".to_string());
    }
    let turn_id = event.turn_id.as_str();
    if sequence.has_phase_turn(turn_id) {
        return Ok(None);
    }
    sequence.record_runtime_event(event)
}

fn commit_agent_tool_records(
    session_log: &PostgresSessionLog,
    start: &AgentRunStart,
    events: &[SequencedSessionRecord],
    checkpoint: Option<&CheckpointRecord>,
    fence: &RuntimeJobLeaseFence,
    trigger: Option<&agent_work_trigger::WorkCommitTrigger>,
) -> Result<SessionCommitReceipt, String> {
    let receipt = session_delivery::retry(
        || match checkpoint {
            Some(checkpoint) => session_log
                .append_recovery_checkpoint_with_runtime_job_lease_blocking(
                    &start.agent_run_id,
                    events,
                    checkpoint,
                    fence,
                ),
            None => session_log.append_session_records_with_runtime_job_lease_blocking(
                &start.agent_run_id,
                events,
                fence,
            ),
        },
        std::thread::sleep,
    )?;
    // The store's append guard and transaction have ended. Downstream stream or
    // capture failures must not prevent waking a successfully committed result.
    if let Some(trigger) = trigger {
        trigger.after_commit(start, &receipt);
    }
    Ok(receipt)
}

fn append_agent_run_session_records(
    runtime: &tokio::runtime::Runtime,
    session_log: &PostgresSessionLog,
    agent_run_start: &AgentRunStart,
    events: &[SequencedSessionRecord],
    lease_fence: &RuntimeJobLeaseFence,
) -> Result<SessionCommitReceipt, String> {
    if let (
        Some(SequencedSessionRecord { sequence: 2, event }),
        AgentRunTailAction::RewriteLastUser {
            target_message_id,
            expected_tail_message_id,
        },
    ) = (events.first(), &agent_run_start.tail_action)
    {
        return session_log
            .append_rewritten_session_records_with_runtime_job_lease_blocking(
                agent_run_start.agent_run_id.as_str(),
                events,
                &RewriteLastUserTailRequest {
                    target_message_id: target_message_id.clone(),
                    expected_tail_message_id: expected_tail_message_id.clone(),
                    new_turn_id: agent_run_start.turn_id.clone(),
                    new_agent_run_id: agent_run_start.agent_run_id.clone(),
                    created_at_ms: event.created_at_ms,
                },
                lease_fence,
            )
            .map_err(|error| {
                if error == RUNTIME_JOB_LEASE_FENCE_REJECTED {
                    "agent_run_lifecycle_lease_lost".to_string()
                } else {
                    error
                }
            });
    }
    runtime
        .block_on(session_log.append_session_records_with_runtime_job_lease(
            agent_run_start.agent_run_id.as_str(),
            events,
            lease_fence,
        ))
        .map_err(|error| {
            if error == RUNTIME_JOB_LEASE_FENCE_REJECTED {
                "agent_run_lifecycle_lease_lost".to_string()
            } else {
                error
            }
        })
}

fn commit_new_user_turn_admission(
    job_store: &PostgresRuntimeStore,
    session_log: &PostgresSessionLog,
    agent_runtime: &AgentRuntime<RuntimeStoreActor>,
    agent_run_start: &AgentRunStart,
    new_run_events: &[SequencedSessionRecord],
    lease_fence: &RuntimeJobLeaseFence,
    now_ms: i64,
) -> Result<SessionCommitReceipt, String> {
    let session_id = agent_run_start.authorization.session_id.as_str();
    let session_head = job_store.with_client(|client| {
        client
            .query_one(
                "SELECT COALESCE(MAX(sequence), 0) FROM app_core_sessionevent WHERE session_id=$1",
                &[&session_id],
            )
            .map_err(|error| format!("load admission session head failed: {error}"))
            .map(|row| row.get::<_, i32>(0))
    })?;
    let plan = if session_head == 0 {
        centaeris_core::runtime::contracts::NewUserTurnClosurePlanV1 {
            session_id: session_id.to_string(),
            expected_session_sequence: 0,
            trigger_agent_run_id: agent_run_start.agent_run_id.clone(),
            trigger_turn_id: agent_run_start.turn_id.clone(),
            evidence_preconditions: Vec::new(),
            closures: Vec::new(),
        }
    } else {
        let records = load_recovery_session_events(job_store, session_id, session_head)?;
        agent_runtime.plan_new_user_turn_closures(
            session_id,
            records.as_slice(),
            agent_run_start.agent_run_id.as_str(),
            agent_run_start.turn_id.as_str(),
            u64::try_from(session_head)
                .map_err(|_| "admission session head is invalid".to_string())?,
            now_ms,
        )?
    };
    let mut closure_events = Vec::with_capacity(plan.closures.len());
    for closure in &plan.closures {
        closure_events.push(centaeris_core::runtime::canonical_tool_call_closure_record(
            session_id,
            closure.turn_id.as_str(),
            closure.agent_run_id.as_str(),
            &closure.call,
            &closure.result,
            closure.recovery.as_str(),
            closure.call_event_id.as_deref(),
            closure.trigger_agent_run_id.as_str(),
            closure.trigger_turn_id.as_str(),
            now_ms,
        )?);
    }
    session_log.append_new_user_turn_admission_blocking(
        agent_run_start.agent_run_id.as_str(),
        new_run_events,
        closure_events.as_slice(),
        &plan,
        lease_fence,
    )
}

fn append_assistant_progress_records(
    runtime: &tokio::runtime::Runtime,
    session_log: &PostgresSessionLog,
    agent_run_start: &AgentRunStart,
    sequence: &mut AgentRunSessionState,
    assistant_text: &AssistantTextProjection,
    lease_fence: &RuntimeJobLeaseFence,
    stream: &mut Option<TransientAgentRunStream>,
) -> Result<(), String> {
    let mut committed_sequence = sequence.clone();
    let events = assistant_session_records(
        agent_run_start,
        assistant_text,
        None,
        &mut committed_sequence,
        now_ms()?,
    )?;
    if !events.is_empty() {
        let receipt =
            runtime.block_on(session_log.append_session_records_with_runtime_job_lease(
                agent_run_start.agent_run_id.as_str(),
                events.as_slice(),
                lease_fence,
            ))?;
        accept_session_commit(&mut committed_sequence, stream, &receipt)?;
    }
    *sequence = committed_sequence;
    Ok(())
}

fn started_session_records(
    agent_run_start: &AgentRunStart,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    if let AgentRunStartInitialInput::HostEvent { input, .. } = &agent_run_start.initial_input {
        let mut records = Vec::new();
        if sequence.is_empty() {
            let [started, _] = centaeris_core::session::started_agent_run_records(
                &agent_run_start.authorization.session_id,
                &agent_run_start.turn_id,
                &agent_run_start.agent_run_id,
                &agent_run_start.prompt,
                created_at_ms,
            )?;
            records.push(sequence.record(started)?);
        }
        if let Some(record) =
            sequence.host_event_input_record(&agent_run_start.turn_id, input, created_at_ms)?
        {
            records.push(record);
        }
        return Ok(records);
    }
    if sequence.is_empty()
        && matches!(
            agent_run_start.tail_action,
            AgentRunTailAction::RewriteLastUser { .. }
        )
    {
        sequence.reserve_rewritten_user_predecessor()?;
    }
    let (objective, attachments) = if matches!(
        agent_run_start.initial_input,
        AgentRunStartInitialInput::UserInput { .. }
    ) {
        // Persist the same model-context rendering that Core uses on the first
        // request, including image placeholders for durable checkpoint replay.
        let mut message = centaeris_core::session::state::ChatMessage {
            message_id: format!("message:{}:user", agent_run_start.turn_id),
            role: centaeris_core::session::state::MessageRole::User,
            content: agent_run_start.prompt.clone(),
            created_at_ms,
            metadata: HashMap::new(),
        };
        let captured = contract::initial_input_attachments(agent_run_start)?;
        centaeris_core::session::turn_input::attachments::apply_user_input_attachments(
            &mut message,
            &captured,
        );
        let images: Vec<centaeris_core::model::prepared_prompt::ModelInputImageRefV1> = message
            .metadata
            .get(centaeris_core::runtime::keys::metadata::MODEL_INPUT_IMAGES)
            .map(|raw| serde_json::from_str(raw))
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        let attachments = captured
            .iter()
            .map(|item| {
                let mut value = serde_json::to_value(item).expect("typed attachment serializes");
                if let Some(image) = images
                    .iter()
                    .find(|image| image.input_ref == item.input_ref)
                {
                    value["placeholder"] = json!(image.placeholder);
                }
                value
            })
            .collect();
        (message.content, attachments)
    } else {
        (
            agent_run_start.prompt.clone(),
            message_attachments(agent_run_start)?,
        )
    };
    sequence.start(
        agent_run_start.turn_id.as_str(),
        objective.as_str(),
        attachments,
        created_at_ms,
    )
}

fn model_user_message(
    agent_run_start: &AgentRunStart,
    resolved_inputs: &ResolvedInputState,
    input_states: &HashMap<String, DeferredInputResolutionFailureKind>,
) -> Result<String, String> {
    if agent_run_start.authorization.message_asset_refs.is_empty() {
        return Ok(agent_run_start.prompt.clone());
    }
    let attachments = agent_run_start
        .authorization
        .asset_refs
        .iter()
        .filter(|reference| {
            agent_run_start
                .authorization
                .message_asset_refs
                .contains(&reference.input_ref)
        })
        .map(|reference| {
            if let Some(state) = input_states.get(reference.input_ref.as_str()) {
                return Ok(format!(
                    "- {} (inputRef: {}; contentType: {}; state: {})",
                    reference.display_name,
                    reference.input_ref,
                    reference.content_type,
                    state.as_str()
                ));
            }
            resolved_inputs
                .input_by_ref(reference.input_ref.as_str())?
                .ok_or_else(|| {
                    format!(
                        "message attachment was not projected: {}",
                        reference.input_ref
                    )
                })?;
            Ok(format!(
                "- {} (inputRef: {}; contentType: {}). Use read_material(input_ref) or search_materials; if pending use get_operation. Cite only citationIds returned by a successful material read.",
                reference.display_name, reference.input_ref, reference.content_type
            ))
        })
        .collect::<Result<Vec<_>, String>>()?
        .join("\n");
    Ok(format!(
        "{}\n\nAttached session files for this message:\n{}",
        agent_run_start.prompt, attachments
    ))
}

fn preproject_message_inputs(
    agent_run_start: &AgentRunStart,
    resolved_inputs: &ResolvedInputState,
) -> Result<HashMap<String, DeferredInputResolutionFailureKind>, String> {
    let mut states = HashMap::new();
    for input_ref in &agent_run_start.authorization.message_asset_refs {
        if let Err(error) = resolved_inputs.resolve_input(input_ref) {
            if error.kind == DeferredInputResolutionFailureKind::HostUnavailable {
                return Err(format!(
                    "message attachment projection failed: inputRef={input_ref}; {}",
                    error.message
                ));
            }
            states.insert(input_ref.clone(), error.kind);
        }
    }
    Ok(states)
}

fn message_attachments(agent_run_start: &AgentRunStart) -> Result<Vec<serde_json::Value>, String> {
    agent_run_start
        .authorization
        .message_asset_refs
        .iter()
        .map(|input_ref| {
            let reference = agent_run_start
                .authorization
                .asset_refs
                .iter()
                .find(|reference| reference.input_ref == *input_ref)
                .ok_or_else(|| format!("message attachment is not authorized: {input_ref}"))?;
            Ok(json!({
                "inputRef": reference.input_ref,
                "displayName": reference.display_name,
                "contentType": reference.content_type,
            }))
        })
        .collect()
}

fn completed_session_records(
    agent_run_start: &AgentRunStart,
    resolved_inputs: Option<&ResolvedInputState>,
    assistant_text: &AssistantTextProjection,
    response: &AgentRunResult,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    if assistant_text.terminal_text().trim().is_empty() {
        return Err("final assistant text is required".to_string());
    }
    let mut events = tool_session_records(
        agent_run_start,
        resolved_inputs,
        response,
        sequence,
        created_at_ms,
    )?;
    events.extend(assistant_session_records(
        agent_run_start,
        assistant_text,
        Some("done"),
        sequence,
        created_at_ms,
    )?);
    if let Some(event) = end_active_execution(
        sequence,
        agent_run_start.agent_run_id.as_str(),
        "completed",
        "completed",
        false,
        created_at_ms,
    )? {
        events.push(event);
    }
    events.push(sequence.complete(
        agent_run_start.agent_run_id.as_str(),
        "finalized",
        created_at_ms,
    )?);
    Ok(events)
}

fn observed_workspace_generation(host: &impl ExecutionHostRunner) -> ExecutionWorkspaceGeneration {
    let generation = host.workspace_generation();
    if let Err(reason) = generation.validate() {
        eprintln!(
            "Workspace generation invalid: {reason}; transitionReason=workspace_generation_unknown; forceCollect=true"
        );
        return ExecutionWorkspaceGeneration::Unknown { reason };
    }
    if let ExecutionWorkspaceGeneration::Unknown { reason } = &generation {
        eprintln!(
            "Workspace generation unavailable: {reason}; transitionReason=workspace_generation_unknown; forceCollect=true"
        );
    }
    generation
}

fn workspace_generations_match(
    previous: &ExecutionWorkspaceGeneration,
    current: &ExecutionWorkspaceGeneration,
) -> bool {
    matches!((previous.token(), current.token()), (Some(previous), Some(current)) if previous == current)
}

fn stable_workspace_generation(
    before_collect: &ExecutionWorkspaceGeneration,
    after_collect: ExecutionWorkspaceGeneration,
) -> ExecutionWorkspaceGeneration {
    if workspace_generations_match(before_collect, &after_collect) {
        after_collect
    } else {
        let reason = "workspace generation changed while collecting a recovery snapshot";
        eprintln!(
            "Workspace generation changed while collecting a recovery snapshot; transitionReason=workspace_generation_unstable; forceCollect=true"
        );
        ExecutionWorkspaceGeneration::Unknown {
            reason: reason.to_string(),
        }
    }
}

fn recovery_snapshot_matches_session_workspace(
    snapshot: &RecoveryWorkspaceSnapshotV1,
    workspace: &agent_run_authorization::SessionWorkspace,
) -> bool {
    snapshot.snapshot_sha256 == workspace.snapshot_sha256
        && snapshot.snapshot_size_bytes == workspace.snapshot_size_bytes
        && snapshot.expanded_size_bytes == workspace.expanded_size_bytes
        && snapshot.file_count == workspace.file_count
}

fn execution_environment_was_lost(error: &str) -> bool {
    error.starts_with("execution_environment_lost:")
        || error.starts_with("AgentRun execution environment was lost")
        || error == "recovery checkpoint already started a replacement Execution"
}

fn provider_response_was_interrupted(error: &str) -> bool {
    error.contains("kind=provider_response_interrupted")
        && api_model_client::hosted_model_failure(error).is_none()
}

fn public_agent_failure_reason<'a>(internal_error: &'a str, transition_reason: &'a str) -> &'a str {
    if api_model_client::hosted_model_failure(internal_error).is_some()
        || matches!(
            internal_error,
            "completion_tool_delivery_required" | "completion_delivery_repair_prompt_too_large"
        )
    {
        internal_error
    } else {
        transition_reason
    }
}

fn failed_session_records(
    agent_run_start: &AgentRunStart,
    failure_kind: &str,
    assistant_text: &AssistantTextProjection,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    if sequence.is_empty() {
        // The new user turn was never admitted (no closure + first batch
        // committed). A pre-admission failure must not fabricate model-history
        // messages; it stays in the durable AgentRun control plane.
        return Ok(Vec::new());
    }
    let model_failure = api_model_client::hosted_model_failure(failure_kind);
    let failure_kind = model_failure
        .as_ref()
        .map(|failure| failure.reason_type.as_str())
        .unwrap_or(failure_kind);
    let failure_message = model_failure
        .as_ref()
        .map(|failure| failure.public_message());
    let mut events = Vec::new();
    if assistant_text
        .responses
        .iter()
        .any(|response| !response.text.trim().is_empty())
    {
        events.extend(assistant_session_records(
            agent_run_start,
            assistant_text,
            Some("error"),
            sequence,
            created_at_ms,
        )?);
    }
    if let Some(event) = end_active_execution(
        sequence,
        agent_run_start.agent_run_id.as_str(),
        "failed",
        failure_kind,
        false,
        created_at_ms,
    )? {
        events.push(event);
    }
    events.push(
        sequence.fail(
            agent_run_start.agent_run_id.as_str(),
            failure_kind,
            failure_message
                .as_deref()
                .unwrap_or("AgentRun did not complete. Retry the request."),
            created_at_ms,
        )?,
    );
    Ok(events)
}

fn end_active_execution(
    sequence: &mut AgentRunSessionState,
    turn_id: &str,
    outcome: &str,
    reason_code: &str,
    retryable: bool,
    created_at_ms: i64,
) -> Result<Option<SequencedSessionRecord>, String> {
    let Some(execution_id) = sequence.active_execution_id().map(str::to_string) else {
        return Ok(None);
    };
    let indeterminate_tool_call_ids = sequence.open_tool_call_ids();
    sequence
        .end_execution(
            turn_id,
            execution_id.as_str(),
            outcome,
            reason_code,
            retryable,
            None,
            indeterminate_tool_call_ids,
            created_at_ms,
        )
        .map(Some)
}

fn assistant_session_records(
    _agent_run_start: &AgentRunStart,
    assistant_text: &AssistantTextProjection,
    terminal_status: Option<&str>,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    if !matches!(terminal_status, None | Some("done") | Some("error")) {
        return Err("assistant terminal status is unsupported".to_string());
    }
    let last_index = assistant_text.responses.len().checked_sub(1);
    let artifact_refs = sequence.artifact_refs();
    let mut events = Vec::new();
    for (index, response) in assistant_text.responses.iter().enumerate() {
        if response.has_tool_call {
            continue;
        }
        let is_terminal = terminal_status.is_some() && Some(index) == last_index;
        if !response.done && !is_terminal {
            return Err(format!(
                "assistant response did not complete: {}",
                response.turn_id
            ));
        }
        if response.text.trim().is_empty() {
            continue;
        }
        let message_id = assistant_message_id(response.turn_id.as_str());
        if sequence.assistant_is_final(message_id.as_str()) {
            continue;
        }
        let text = response.text.as_str();
        if let Some(event) = sequence.assistant(
            response.turn_id.as_str(),
            text,
            artifact_refs.clone(),
            if is_terminal {
                terminal_status.unwrap()
            } else {
                "done"
            },
            created_at_ms,
        )? {
            events.push(event);
        }
    }
    Ok(events)
}

fn tool_session_records(
    agent_run_start: &AgentRunStart,
    resolved_inputs: Option<&ResolvedInputState>,
    response: &AgentRunResult,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    let mut events = Vec::new();
    for turn in &response.turn_responses {
        events.extend(tool_safe_point_session_records(
            agent_run_start,
            resolved_inputs,
            turn,
            sequence,
            created_at_ms,
        )?);
    }
    Ok(events)
}

fn provider_usage_session_records(
    _agent_run_start: &AgentRunStart,
    turn_id: &str,
    usage: &centaeris_core::runtime::contracts::ProviderTokenUsageV1,
    sequence: &mut AgentRunSessionState,
    recorded_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    Ok(sequence
        .provider_usage_record(turn_id, usage, recorded_at_ms)?
        .into_iter()
        .collect())
}

fn tool_safe_point_session_records(
    _agent_run_start: &AgentRunStart,
    resolved_inputs: Option<&ResolvedInputState>,
    turn: &TurnStepResult,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    let _ = resolved_inputs;
    let mut events = Vec::new();
    for result in &turn.tool_results {
        if !sequence.has_tool_result(result.tool_call_id.as_str()) {
            return Err(format!(
                "completed Turn has an uncommitted tool result: {}",
                result.tool_call_id
            ));
        }
        events.extend(sequence.record_tool_facts(turn.turn_id.as_str(), result, created_at_ms)?);
    }
    Ok(events)
}

struct ToolCallRecordContext<'a> {
    provider_id: &'a str,
    tool_contract_digest: &'a str,
    created_at_ms: i64,
}

fn tool_call_session_records(
    agent_run_start: &AgentRunStart,
    resolved_inputs: Option<&ResolvedInputState>,
    turn_id: &str,
    call: &ToolCallEnvelope,
    sequence: &mut AgentRunSessionState,
    context: ToolCallRecordContext<'_>,
) -> Result<Vec<SequencedSessionRecord>, String> {
    if sequence.has_tool_call(call.id.as_str()) {
        return Ok(Vec::new());
    }
    let normalized_input = serde_json::from_str::<serde_json::Value>(call.args_json.as_str())
        .map_err(|error| format!("semantic tool_call input is invalid JSON: {error}"))?;
    let normalized_input_object = normalized_input
        .as_object()
        .ok_or_else(|| "semantic tool_call input must be an object".to_string())?;
    let display_target = tool_display_target(
        &agent_run_start.authorization,
        resolved_inputs,
        call.name.as_str(),
        normalized_input_object,
    )?;
    Ok(sequence
        .record_tool_call(
            turn_id,
            call,
            context.provider_id,
            context.tool_contract_digest,
            display_target.as_str(),
            context.created_at_ms,
        )?
        .into_iter()
        .collect())
}

fn tool_result_session_records(
    _agent_run_start: &AgentRunStart,
    resolved_inputs: Option<&ResolvedInputState>,
    turn_id: &str,
    call: &ToolCallEnvelope,
    result: &ToolExecutionResult,
    sequence: &mut AgentRunSessionState,
    created_at_ms: i64,
) -> Result<Vec<SequencedSessionRecord>, String> {
    let _ = resolved_inputs;
    let created_at_ms = if result.completed_at_ms > 0 {
        result.completed_at_ms
    } else {
        created_at_ms
    };
    sequence.record_tool_result(turn_id, call, result, created_at_ms)
}

fn tool_display_target(
    authorization: &WorkspaceAgentRunAuthorization,
    resolved_inputs: Option<&ResolvedInputState>,
    tool_name: &str,
    input: &serde_json::Map<String, serde_json::Value>,
) -> Result<String, String> {
    let target = match tool_name {
        "read" => {
            if let Some(input_ref) = input.get("input_ref").and_then(serde_json::Value::as_str) {
                if let Some(display_name) = authorization
                    .asset_refs
                    .iter()
                    .find(|reference| reference.input_ref == input_ref)
                    .map(|reference| reference.display_name.clone())
                {
                    display_name
                } else if let Some(display_name) = resolved_inputs
                    .map(|state| state.display_name_by_ref(input_ref))
                    .transpose()?
                    .flatten()
                {
                    display_name
                } else {
                    "授权资料".to_string()
                }
            } else if input.get("input_refs").is_some() {
                "多份授权资料".to_string()
            } else {
                required_tool_input_string(input, "path", tool_name)?.to_string()
            }
        }
        "bash" => tool_name.to_string(),
        "write" | "edit" => required_tool_input_string(input, "path", tool_name)?.to_string(),
        "web_search" => required_tool_input_string(input, "query", tool_name)?.to_string(),
        "agent" => required_tool_input_string(input, "description", tool_name)?.to_string(),
        _ => tool_name.to_string(),
    };
    Ok(target.chars().take(256).collect())
}

fn required_tool_input_string<'a>(
    input: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    tool_name: &str,
) -> Result<&'a str, String> {
    input
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{tool_name} display target requires {field}"))
}

fn load_existing_session_sequence(
    store: &PostgresRuntimeStore,
    agent_run_start: &AgentRunStart,
) -> Result<AgentRunSessionState, String> {
    store.with_client(|client| {
    let rows = client
        .query(
            "SELECT agent_run_sequence, \"eventId\", payload->>'type', session_id, payload::text FROM app_core_sessionevent WHERE agent_run_id = $1 AND session_level = false ORDER BY agent_run_sequence",
            &[&agent_run_start.agent_run_id],
        )
        .map_err(|error| format!("query existing session sequence failed: {error}"))?;
    let mut sequence = AgentRunSessionState::new(
        agent_run_start.authorization.session_id.clone(),
        agent_run_start.agent_run_id.clone(),
    )?;
    let mut wires = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row.get::<_, String>(4).as_str())
                .map_err(|error| format!("decode existing session wire failed: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    hydrate_session_wire_values(client, wires.as_mut_slice())?;
    for ((index, row), wire) in rows.iter().enumerate().zip(wires) {
        let stored_sequence = row.get::<_, i32>(0);
        let expected = i32::try_from(index + 1)
            .map_err(|_| "existing session sequence overflow".to_string())?;
        if stored_sequence != expected
            || row.get::<_, String>(3) != agent_run_start.authorization.session_id
        {
            return Err("existing session sequence identity mismatch".to_string());
        }
        let event = parse_wire_record(&wire)
            .map_err(|error| format!("decode existing session record failed: {error}"))?
            .event;
        if row.get::<_, String>(1) != event.event_id {
            return Err("existing session event identity mismatch".to_string());
        }
        sequence.restore(SequencedSessionRecord {
            sequence: expected as u64,
            event,
        })?;
    }
    if !rows.is_empty() {
        let input_record_type = match agent_run_start.initial_input {
            AgentRunStartInitialInput::UserMessage {} | AgentRunStartInitialInput::UserInput { .. } => "user_message",
            AgentRunStartInitialInput::HostEvent { .. } => "host_event_input",
        };
        let types = rows
            .iter()
            .map(|row| row.get::<_, Option<String>>(2))
            .collect::<Vec<_>>();
        if types.first().and_then(Option::as_deref) != Some("agent_run_started")
            || types.get(1).and_then(Option::as_deref) != Some(input_record_type)
            || types.iter().flatten().any(|value| {
                matches!(
                    value.as_str(),
                    "agent_run_completed" | "agent_run_failed" | "agent_run_interrupted"
                )
            })
        {
            return Err("existing session sequence is not restartable".to_string());
        }
    }
    let committed_session_sequence = client
        .query_one(
            "SELECT COALESCE(MAX(sequence), 0) FROM app_core_sessionevent WHERE session_id=$1",
            &[&agent_run_start.authorization.session_id],
        )
        .map_err(|error| format!("query committed session position failed: {error}"))?
        .get::<_, i32>(0);
    sequence.set_committed_session_sequence(
        u64::try_from(committed_session_sequence)
            .map_err(|_| "committed session position is invalid".to_string())?,
    );
    Ok(sequence)
    })
}

fn select_agent_run_recovery_checkpoint(
    store: &PostgresRuntimeStore,
    start: &AgentRunStart,
    sequence: &AgentRunSessionState,
) -> Result<Option<(CheckpointRecord, RuntimeRecoveryCheckpointV1)>, String> {
    if sequence
        .active_execution()
        .is_some_and(|execution| execution.authorization_digest != start.authorization_digest)
    {
        return Err("AgentRun Execution authorization identity mismatch".to_string());
    }
    if sequence.is_empty() || sequence.active_execution().is_some() {
        return Ok(None);
    }
    latest_recovery_checkpoint(store, start, sequence, true)?
        .map(Some)
        .ok_or_else(|| {
            "AgentRun execution environment was lost without a recovery checkpoint".to_string()
        })
}

fn latest_recovery_checkpoint(
    store: &PostgresRuntimeStore,
    agent_run_start: &AgentRunStart,
    sequence: &AgentRunSessionState,
    require_recoverable: bool,
) -> Result<Option<(CheckpointRecord, RuntimeRecoveryCheckpointV1)>, String> {
    let Some(checkpoint_id) = sequence.latest_recovery_checkpoint_id() else {
        return Ok(None);
    };
    let record = store
        .load_recovery_checkpoint_by_id(checkpoint_id)?
        .ok_or_else(|| "committed recovery checkpoint is missing".to_string())?;
    let payload = serde_json::from_str::<RuntimeRecoveryCheckpointV1>(&record.payload_json)
        .map_err(|error| format!("decode recovery checkpoint failed: {error}"))?;
    payload.validate()?;
    if record.kind != CheckpointKindV1::Recovery
        || record.checkpoint_id != payload.checkpoint_id
        || record.session_id != payload.session_id
        || record.status != "committed"
        || record.done_reason.is_some()
        || payload.agent_run_id != agent_run_start.agent_run_id
        || payload.session_id != agent_run_start.authorization.session_id
        || payload.authorization_digest != agent_run_start.authorization_digest
        || payload.session_sequence > sequence.committed_session_sequence()
        || !sequence.has_checkpoint(payload.checkpoint_id.as_str())
    {
        return Err("recovery checkpoint binding mismatch".to_string());
    }
    if require_recoverable && sequence.has_used_recovery_checkpoint(&payload.checkpoint_id) {
        return Err("recovery checkpoint already started a replacement Execution".to_string());
    }
    if require_recoverable && !sequence.tool_ledger_is_checkpointed() {
        return Err("recovery checkpoint does not cover the tool ledger".to_string());
    }
    if wait_recovery::WaitHandoff::is_wait_checkpoint(&payload.checkpoint_id) {
        store
            .load_wait_handoff(&payload.checkpoint_id)?
            .ok_or("wait recovery attachment missing")?
            .validate(&payload)?;
    }
    Ok(Some((record, payload)))
}

fn refresh_runtime_state_after_admission(
    job_store: &PostgresRuntimeStore,
    store: &RuntimeStoreActor,
    session_id: &str,
    session_sequence: u64,
) -> Result<(), String> {
    let session_sequence = i32::try_from(session_sequence)
        .map_err(|_| "admitted Session sequence overflow".to_string())?;
    // Admission may close tools from an earlier AgentRun. The cached snapshot
    // predates that commit; replay Core's authoritative Session records before
    // starting the new turn, rather than weakening safe-point identity checks.
    with_recovery_session_events(job_store, session_id, session_sequence, |events| {
        let snapshot = restore_runtime_snapshot_from_session_records(session_id, &events)?;
        SessionManager::new(store.clone()).save_session(&snapshot)
    })
}

fn restore_runtime_state_from_recovery_checkpoint(
    job_store: &PostgresRuntimeStore,
    store: &RuntimeStoreActor,
    checkpoint: &RuntimeRecoveryCheckpointV1,
) -> Result<(), String> {
    if wait_recovery::WaitHandoff::is_wait_checkpoint(&checkpoint.checkpoint_id) {
        let snapshot = job_store
            .load_wait_handoff(&checkpoint.checkpoint_id)?
            .ok_or("wait recovery attachment missing")?
            .validate(checkpoint)?;
        return SessionManager::new(store.clone()).save_session(&snapshot);
    }
    let session_sequence = i32::try_from(checkpoint.session_sequence)
        .map_err(|_| "recovery checkpoint Session sequence overflow".to_string())?;
    with_recovery_session_events(
        job_store,
        checkpoint.session_id.as_str(),
        session_sequence,
        |events| {
            let snapshot = restore_runtime_snapshot_from_session_records(
                checkpoint.session_id.as_str(),
                &events,
            )?;
            SessionManager::new(store.clone()).save_session(&snapshot)
        },
    )
}

fn with_recovery_session_events<T>(
    store: &PostgresRuntimeStore,
    session_id: &str,
    session_sequence: i32,
    followup: impl FnOnce(Vec<centaeris_core::session::SessionLogRecord>) -> Result<T, String>,
) -> Result<T, String> {
    let events = load_recovery_session_events(store, session_id, session_sequence)?;
    // Loading owns the decoded records, so its database lease ends before the
    // follow-up may call RuntimeStoreActor through the same capacity-one pool.
    followup(events)
}

fn load_recovery_session_events(
    store: &PostgresRuntimeStore,
    session_id: &str,
    session_sequence: i32,
) -> Result<Vec<centaeris_core::session::SessionLogRecord>, String> {
    store.with_client(|client| {
    let rows = client
        .query(
            "SELECT sequence,payload::text FROM app_core_sessionevent WHERE session_id=$1 AND sequence<=$2 ORDER BY sequence",
            &[&session_id, &session_sequence],
        )
        .map_err(|error| format!("load recovery Session replay failed: {error}"))?;
    if rows.last().map(|row| row.get::<_, i32>(0)) != Some(session_sequence) {
        return Err("recovery checkpoint Session high-water mark is missing".to_string());
    }
    let mut wires = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row.get::<_, String>(1).as_str())
                .map_err(|error| format!("decode recovery Session wire failed: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    hydrate_session_wire_values(client, wires.as_mut_slice())?;
    wires
        .iter()
        .map(|wire| {
            parse_wire_record(wire)
                .map(|record| record.event)
                .map_err(|error| format!("decode recovery Session record failed: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()
    })
}

fn load_existing_terminal_state(
    store: &PostgresRuntimeStore,
    agent_run_start: &AgentRunStart,
) -> Result<Option<&'static str>, String> {
    let terminal = store.with_client(|client| {
    let rows = client
        .query(
            "SELECT payload->>'type', session_id FROM app_core_sessionevent WHERE agent_run_id=$1 AND payload->>'type' IN ('agent_run_completed','agent_run_failed','agent_run_interrupted') ORDER BY sequence",
            &[&agent_run_start.agent_run_id],
        )
        .map_err(|error| format!("query existing terminal state failed: {error}"))?;
    if rows.len() > 1
        || rows
            .iter()
            .any(|row| row.get::<_, String>(1) != agent_run_start.authorization.session_id)
    {
        return Err("existing terminal state identity conflict".to_string());
    }
    Ok(rows
        .first()
        .map(|row| match row.get::<_, String>(0).as_str() {
            "agent_run_completed" => "completed",
            "agent_run_failed" => "failed",
            "agent_run_interrupted" => "cancelled",
            _ => unreachable!("terminal query only returns supported types"),
        }))
    })?;
    if terminal.is_some() {
        store.repair_terminal_runtime_job_waits(
            &agent_run_start.authorization.session_id,
            &agent_run_start.agent_run_id,
        )?;
    } else {
        let cancelled = store.with_client(|client| {
            client.query_opt(
                "SELECT 1 FROM app_core_agentrun WHERE id=$1 AND session_id=$2 AND \"preAdmissionCancelledAt\" IS NOT NULL",
                &[&agent_run_start.agent_run_id, &agent_run_start.authorization.session_id],
            ).map(|row| row.is_some()).map_err(|e| e.to_string())
        })?;
        if cancelled {
            return Ok(Some("cancelled"));
        }
    }
    Ok(terminal)
}

fn has_terminal_agent_run_identity(
    store: &PostgresRuntimeStore,
    session_id: &str,
    identity: &RuntimeAgentRunIdentityV1,
) -> Result<bool, String> {
    identity.validate()?;
    store.with_client(|client| {
    let rows = client
        .query(
            concat!(
                "SELECT event.payload->>'type' FROM app_core_agentrunauthorization auth ",
                "JOIN app_core_sessionevent event ON event.agent_run_id=auth.agent_run_id ",
                "WHERE auth.agent_run_id=$1 AND auth.digest=$2 AND event.session_id=$3 ",
                "AND event.payload->>'type' IN ('agent_run_completed','agent_run_failed','agent_run_interrupted')",
            ),
            &[
                &identity.agent_run_id,
                &identity.authorization_digest,
                &session_id,
            ],
        )
        .map_err(|error| format!("query terminal AgentRun identity failed: {error}"))?;
    if rows.len() > 1 {
        return Err("terminal AgentRun identity conflict".to_string());
    }
    Ok(rows.len() == 1)
    })
}

fn workspace_input_upper_bound_bytes(agent_run_start: &AgentRunStart) -> Result<u64, String> {
    agent_run_start
        .authorization
        .asset_refs
        .iter()
        .try_fold(0_u64, |total, input| {
            total
                .checked_add(input.size_bytes)
                .ok_or_else(|| "workspace input size overflow".to_string())
        })
}

fn acknowledge_terminal_completed_projection(
    store: &RuntimeStoreActor,
    agent_run_start: &AgentRunStart,
) -> Result<(), String> {
    let manager = SessionManager::new(store.clone());
    let Some(mut session) =
        manager.load_session(agent_run_start.authorization.session_id.as_str())?
    else {
        return Ok(());
    };
    if session.session_id != agent_run_start.authorization.session_id {
        return Err("completed_turn_projection_snapshot_session_mismatch".to_string());
    }
    let Some(projection) = &session.completed_turn else {
        return Ok(());
    };
    projection.validate()?;
    if projection.agent_run_id != agent_run_start.agent_run_id
        || projection.authorization_digest != agent_run_start.authorization_digest
    {
        return Err("completed_turn_projection_identity_mismatch".to_string());
    }
    session.completed_turn = None;
    manager.save_session(&session)
}

fn validate_completed_projection_session_log(
    store: &PostgresRuntimeStore,
    agent_run_start: &AgentRunStart,
    projection: &CompletedTurnProjectionV1,
    sequence: &AgentRunSessionState,
) -> Result<(), String> {
    projection.validate()?;
    if projection.agent_run_id != agent_run_start.agent_run_id
        || projection.authorization_digest != agent_run_start.authorization_digest
    {
        return Err("completed_turn_projection_identity_mismatch".to_string());
    }
    let expected_tool_calls = projection
        .expected_tool_call_ids
        .iter()
        .collect::<HashSet<_>>();
    let committed_tool_calls = sequence.tool_result_ids().iter().collect::<HashSet<_>>();
    if expected_tool_calls != committed_tool_calls {
        return Err("completed_turn_projection_tool_receipts_mismatch".to_string());
    }
    store.with_client(|client| {
        let rows = client
            .query(
                concat!(
                "SELECT payload->>'modelMarkdown' FROM app_core_sessionevent ",
                "WHERE agent_run_id=$1 AND session_id=$2 AND payload->>'type'='assistant_message' ",
                "AND payload->>'turnId'=$3 AND payload->>'messageId'=$4 ",
                "AND payload->>'status'='done'",
            ),
                &[
                    &agent_run_start.agent_run_id,
                    &agent_run_start.authorization.session_id,
                    &projection.final_turn_id,
                    &assistant_message_id(projection.final_turn_id.as_str()),
                ],
            )
            .map_err(|error| format!("query completed projection assistant failed: {error}"))?;
        if rows.len() != 1
            || rows[0]
                .get::<_, Option<String>>(0)
                .is_none_or(|text| text.trim().is_empty())
        {
            return Err("completed_turn_projection_final_assistant_missing".to_string());
        }
        Ok(())
    })
}

fn initial_execution_id(agent_run_start: &AgentRunStart) -> String {
    format!(
        "execution:{:x}",
        Sha256::digest(
            format!(
                "workspace_agent_execution_v1:{}:{}",
                agent_run_start.agent_run_id, agent_run_start.authorization_digest
            )
            .as_bytes()
        )
    )
}

fn replacement_execution_id(
    agent_run_start: &AgentRunStart,
    checkpoint_id: &str,
    attempt: u64,
) -> String {
    format!(
        "execution:{:x}",
        Sha256::digest(
            format!(
                "workspace_agent_replacement_execution_v1:{}:{}:{checkpoint_id}:{attempt}",
                agent_run_start.agent_run_id, agent_run_start.authorization_digest
            )
            .as_bytes()
        )
    )
}

fn recovery_checkpoint_id(execution_id: &str, model_request_id: &str) -> String {
    format!(
        "checkpoint:{:x}",
        Sha256::digest(format!("runtime_recovery_v1:{execution_id}:{model_request_id}").as_bytes())
    )
}

fn recovery_snapshot_from_session_workspace(
    workspace: &agent_run_authorization::SessionWorkspace,
) -> Result<RecoveryWorkspaceSnapshotV1, String> {
    workspace.validate()?;
    Ok(RecoveryWorkspaceSnapshotV1 {
        object_ref: (workspace.snapshot_size_bytes != 0)
            .then(|| format!("session-workspace:generation:{}", workspace.generation)),
        snapshot_sha256: workspace.snapshot_sha256.clone(),
        snapshot_size_bytes: workspace.snapshot_size_bytes,
        expanded_size_bytes: workspace.expanded_size_bytes,
        file_count: workspace.file_count,
    })
}

fn recovery_uses_session_workspace(
    snapshot: &RecoveryWorkspaceSnapshotV1,
    workspace: &agent_run_authorization::SessionWorkspace,
) -> Result<bool, String> {
    if !snapshot
        .object_ref
        .as_deref()
        .is_some_and(|value| value.starts_with("session-workspace:generation:"))
    {
        return Ok(false);
    }
    if snapshot != &recovery_snapshot_from_session_workspace(workspace)? {
        return Err("recovery checkpoint Session workspace binding mismatch".to_string());
    }
    Ok(true)
}

fn now_ms() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before unix epoch: {error}"))?
        .as_millis();
    i64::try_from(millis).map_err(|_| "current timestamp exceeds i64".to_string())
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug)]
struct RuntimeHttpResponse {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl RuntimeHttpResponse {
    fn into_axum_response(self) -> Response<Body> {
        let mut builder = Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, self.content_type)
            .header(header::CONNECTION, "close");
        if self.status == 429 || self.status == 503 {
            builder = builder.header(header::RETRY_AFTER, "5");
        }
        match builder.body(Body::from(self.body)) {
            Ok(response) => response,
            Err(error) => {
                eprintln!("build runtime HTTP response failed: {error}");
                Response::new(Body::from("internal_error"))
            }
        }
    }
}

fn http_response(status: u16, content_type: &'static str, body: Vec<u8>) -> RuntimeHttpResponse {
    RuntimeHttpResponse {
        status,
        content_type,
        body,
    }
}

fn json_error_response(status: u16, error: &str) -> Result<RuntimeHttpResponse, String> {
    let body = serde_json::to_vec(&json!({"error": error}))
        .map_err(|encode_error| format!("encode error response failed: {encode_error}"))?;
    Ok(http_response(status, "application/json", body))
}

fn bounded_json_error_response(status: u16, error: &str) -> RuntimeHttpResponse {
    match json_error_response(status, error) {
        Ok(response) => response,
        Err(encode_error) => {
            eprintln!("encode runtime HTTP error failed: {encode_error}");
            http_response(500, "text/plain", b"internal_error".to_vec())
        }
    }
}

fn agent_run_step_failure_response(
    agent_run_id: &str,
    failure_class: &str,
    retryable: bool,
    transition_reason: &str,
) -> Result<RuntimeHttpResponse, String> {
    let body = serde_json::to_vec(&json!({
        "schema": "runtime.agent_run.step.failure.v1",
        "agentRunId": agent_run_id,
        "failureClass": failure_class,
        "retryable": retryable,
        "transitionReason": transition_reason,
        "error": "runtime_step_failed",
    }))
    .map_err(|error| format!("encode AgentRun step failure failed: {error}"))?;
    Ok(http_response(500, "application/json", body))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_result_delivery_is_recoverable_without_claiming_tool_or_run_failure() {
        let outcome = session_delivery_waiting();
        outcome.validate().unwrap();
        assert_eq!(outcome.disposition, "waiting");
        assert_eq!(outcome.terminal_state, None);
        assert_eq!(
            outcome.transition_reason,
            "session_record_commit_unavailable"
        );
    }
    use centaeris_core::session::supplement::TurnSupplementValidationError;
    use centaeris_core::session::SessionRecordType;
    use centaeris_core::tool::layer::ToolExecutionFact;
    use hyper::service::service_fn;
    use std::convert::Infallible;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn execution_profile_response_uses_exact_runtime_v1_contract() {
        let profile = RuntimeExecutionProfile {
            schema: RUNTIME_EXECUTION_PROFILE_SCHEMA,
            image_capability: "workspace_general_v1",
            image_digest: format!("sha256:{}", "a".repeat(64)),
        };
        assert_eq!(
            serde_json::to_value(profile).expect("execution profile"),
            json!({
                "schema": "runtime.execution_profile.v1",
                "imageCapability": "workspace_general_v1",
                "imageDigest": format!("sha256:{}", "a".repeat(64)),
            })
        );
    }

    #[test]
    fn internal_model_catalog_response_uses_exact_v1_envelope() {
        let response = workspace_model_catalog_response();
        assert_eq!(response["schema"], "workspace.model_catalog.result.v1");
        assert_eq!(response["catalog"]["schema"], "centaeris.model_catalog.v1");
        assert!(response["catalog"]["providers"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        assert_eq!(response.as_object().expect("response object").len(), 2);
    }

    #[test]
    fn plugin_inspection_uses_core_snapshot_and_loads_mcp_contracts() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-plugin-inspection-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("test clock")
                .as_nanos()
        ));
        let staging_name = ".upload-0123456789abcdef0123456789abcdef";
        let package = root.join(staging_name).join("package");
        std::fs::create_dir_all(package.join(".centaeris-plugin")).expect("create Plugin package");
        std::fs::write(
            package.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"example","version":"1.0.0"}"#,
        )
        .expect("write Plugin manifest");

        let inspected =
            inspect_plugin_package_at(root.as_path(), format!("{staging_name}/package").as_str())
                .expect("inspect Plugin package");
        assert_eq!(inspected.name, "example");
        assert_eq!(inspected.version, "1.0.0");

        std::fs::write(
            package.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"example","version":"1.0.0","paths":{"mcpServers":["mcp.json"]}}"#,
        )
        .expect("write MCP Plugin manifest");
        std::fs::write(package.join("mcp.json"), r#"{"schema":"mcp_servers_v1"}"#)
            .expect("write invalid MCP declaration");
        assert!(inspect_plugin_package_at(
            root.as_path(),
            format!("{staging_name}/package").as_str(),
        )
        .expect_err("invalid MCP declaration")
        .contains("parse MCP server declaration"));

        std::fs::remove_dir_all(root).expect("remove Plugin inspection fixture");
    }

    #[test]
    fn plugin_inspection_accepts_only_exact_upload_staging_paths() {
        let missing_root = Path::new("unused");
        for path in [
            "example/package",
            ".upload-0123456789abcdef0123456789abcdeg/package",
            ".upload-0123456789ABCDEF0123456789ABCDEF/package",
            ".upload-0123456789abcdef0123456789abcdef",
            ".upload-0123456789abcdef0123456789abcdef/other",
            ".upload-0123456789abcdef0123456789abcdef/package/extra",
            "../.upload-0123456789abcdef0123456789abcdef/package",
        ] {
            assert!(
                inspect_plugin_package_at(missing_root, path).is_err(),
                "accepted invalid path: {path}"
            );
        }
    }

    #[test]
    fn plugin_inspection_request_rejects_unknown_fields() {
        let request = json!({
            "schema": WORKSPACE_PLUGIN_INSPECT_SCHEMA,
            "packagePath": ".upload-0123456789abcdef0123456789abcdef/package",
            "extra": true,
        });
        assert!(serde_json::from_value::<WorkspacePluginInspectRequest>(request).is_err());
    }

    #[test]
    fn agent_run_step_outcome_accepts_only_canonical_waiting_reasons() {
        for transition_reason in AGENT_RUN_WAITING_TRANSITION_REASONS {
            AgentRunStepOutcome {
                retry_at_ms: (*transition_reason == "execution_recovery_checkpoint_committed")
                    .then_some(1),
                disposition: "waiting",
                terminal_state: None,
                transition_reason: (*transition_reason).to_string(),
            }
            .validate()
            .expect("canonical waiting transition");
        }
        for transition_reason in [
            "execution_recovery_prepare_unavailable",
            "sandbox_prepare_unavailable",
            "sandbox_prepare_unavailable:detail",
        ] {
            assert!(AgentRunStepOutcome {
                retry_at_ms: None,
                disposition: "waiting",
                terminal_state: None,
                transition_reason: transition_reason.to_string(),
            }
            .validate()
            .is_err());
        }
    }

    #[test]
    fn runtime_bind_address_is_explicit_and_typed() {
        assert_eq!(
            parse_runtime_socket_address("127.0.0.1", "9000").expect("socket address"),
            "127.0.0.1:9000"
                .parse::<SocketAddr>()
                .expect("expected address")
        );
        assert!(parse_runtime_socket_address("banana", "9000").is_err());
        assert!(parse_runtime_socket_address("127.0.0.1", "banana").is_err());
    }

    #[test]
    fn one_commit_wake_uses_the_maximum_projected_sequence() {
        let committed = |sequence, event_type| centaeris_core::session::CommittedSessionRecord {
            sequence,
            event: centaeris_core::session::SessionLogRecord {
                schema_version: centaeris_core::session::SESSION_EVENT_SCHEMA_VERSION.to_string(),
                event_version: centaeris_core::session::SESSION_EVENT_VERSION,
                event_type,
                event_id: format!("event_{sequence}"),
                session_id: "session_1".to_string(),
                turn_id: Some("turn_1".to_string()),
                agent_run_id: Some("agent_run_1".to_string()),
                created_at_ms: sequence as i64,
                payload: json!({}),
            },
        };
        let receipt = SessionCommitReceipt {
            records: vec![
                committed(10, SessionRecordType::ModelRequestStarted),
                committed(11, SessionRecordType::PhaseEvent),
                committed(12, SessionRecordType::CheckpointRef),
                committed(13, SessionRecordType::ToolCall),
            ],
        };
        assert_eq!(projected_commit_high_water(&receipt), Some(13));
        assert_eq!(committed_assistant_supersession(&receipt), None);
        let expected_phase = SequencedSessionRecord {
            sequence: 11,
            event: receipt.records[1].event.clone(),
        };
        assert_eq!(committed_phase_sequence(&receipt, &expected_phase), Ok(11));
        let tool_record = SequencedSessionRecord {
            sequence: 13,
            event: receipt.records[3].event.clone(),
        };
        assert!(committed_phase_sequence(&receipt, &tool_record).is_err());
        let assistant_receipt = SessionCommitReceipt {
            records: vec![
                committed(20, SessionRecordType::ToolCall),
                committed(21, SessionRecordType::AssistantMessage),
            ],
        };
        assert_eq!(
            committed_assistant_supersession(&assistant_receipt),
            Some((
                "turn_1".to_string(),
                "message:turn_1:assistant".to_string(),
                21,
            ))
        );
        assert_eq!(
            projected_commit_high_water(&SessionCommitReceipt {
                records: vec![committed(14, SessionRecordType::ProviderUsage)],
            }),
            None
        );
    }

    #[test]
    fn runtime_http_rejects_oversized_body() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let response = runtime.block_on(async {
            read_axum_request(
                Request::builder()
                    .method("POST")
                    .uri("/banana")
                    .body(Body::from(vec![0; MAX_HTTP_BODY_BYTES + 1]))
                    .expect("request"),
            )
            .await
            .expect_err("oversized body must fail")
        });
        assert_eq!(response.status, 413);
    }

    #[test]
    fn runtime_http_times_out_incomplete_headers() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listener");
            let address = listener.local_addr().expect("listener address");
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("accept client");
                let service = service_fn(|_| async {
                    Ok::<_, Infallible>(Response::new(Body::from("unexpected")))
                });
                runtime_http1_builder(Duration::from_millis(50))
                    .serve_connection(TokioIo::new(stream), service)
                    .await
            });
            let mut client = tokio::net::TcpStream::connect(address)
                .await
                .expect("connect client");
            client
                .write_all(b"POST /banana HTTP/1.1\r\nHost:")
                .await
                .expect("write incomplete header");
            let error = server
                .await
                .expect("server task")
                .expect_err("incomplete headers must time out");
            assert!(error.is_timeout());
        });
    }

    #[test]
    fn turn_supplement_http_status_is_typed() {
        assert_eq!(
            turn_supplement_http_status(&TurnSupplementStoreError::Validation(
                TurnSupplementValidationError::IdInvalid,
            )),
            400
        );
        assert_eq!(
            turn_supplement_http_status(&TurnSupplementStoreError::QueueFull),
            409
        );
        assert_eq!(
            turn_supplement_http_status(&TurnSupplementStoreError::Internal("banana".to_string(),)),
            500
        );
    }

    #[test]
    fn agent_run_step_boundary_catches_panic_without_continuing() {
        let error = catch_agent_run_step_panic(|| panic!("banana runtime invariant"))
            .expect_err("panic must cross the boundary as an error");
        assert_eq!(error, "banana runtime invariant");
    }

    #[test]
    fn session_workspace_restore_only_precedes_durable_start() {
        assert!(requires_session_workspace_restore(false, false));
        assert!(!requires_session_workspace_restore(true, false));
        assert!(!requires_session_workspace_restore(false, true));
        assert!(!requires_session_workspace_restore(true, true));
    }

    #[test]
    fn live_overlay_errors_only_degrade_projection() {
        for error in [
            LiveTextError::Indeterminate("banana".to_string()),
            LiveTextError::Fatal("banana".to_string()),
        ] {
            let degraded = Arc::new(AtomicBool::new(false));
            latch_live_error(&degraded, error);
            assert!(degraded.load(Ordering::Relaxed));
        }
    }

    #[test]
    fn committed_live_cleanup_failure_does_not_propagate_past_the_durable_commit() {
        log_live_supersession_result(Err(LiveTextError::Indeterminate(
            "simulated Redis I/O failure".to_string(),
        )));
        log_live_supersession_result(Err(LiveTextError::Fatal(
            "simulated Redis command failure".to_string(),
        )));
    }

    fn agent_run_start() -> AgentRunStart {
        let authorization = serde_json::json!({
            "schema": "workspace.agent_run_authorization.v1",
            "id": "authorization_1",
            "organizationId": "org_1",
            "workspaceId": "ws_1",
            "userId": "user_1",
            "agentId": "centaeris",
            "sessionId": "sess_1",
            "agentRunId": "agent_run_1",
            "sessionWorkspace": {
                "generation": 0,
                "snapshotSha256": "",
                "snapshotSizeBytes": 0,
                "expandedSizeBytes": 0,
                "fileCount": 0
            },
            "modelConfigRef": "model_1",
            "thinkingMode": null,
            "artifactScopeRef": "artifact_scope_1",
            "assetRefs": [],
            "messageAssetRefs": [],
            "imageCapability": "workspace_general_v1",
            "imageDigest": format!("sha256:{}", "a".repeat(64)),
            "pluginActivation": centaeris_core::extension::build_plugin_activation_snapshot(&[])
                .expect("empty plugin activation"),
            "resources": {
                "memoryBytes": 2147483648_u64,
                "cpuMilli": 2000,
                "pidsLimit": 512,
                "dataTmpfsBytes": 4294967296_u64
            }
        });
        let digest = serde_json::from_value::<
            crate::agent_run_authorization::WorkspaceAgentRunAuthorization,
        >(authorization.clone())
        .expect("authorization")
        .digest()
        .expect("authorization digest");
        serde_json::from_value::<AgentRunStart>(serde_json::json!({
            "schema": "workspace.agent_run.start.v2",
            "initialInput": {"type":"userMessage"},
            "agentRunId": "agent_run_1",
            "turnId": "turn_1",
            "prompt": "hello",
            "agentInstructions": "",
            "modelContextTokens": 200000,
            "modelMaxOutputTokens": 32768,
            "authorizationDigest": digest,
            "authorizationSignature": "hmac-sha256:test-only-not-validated",
            "authorization": authorization,
            "tailAction": {"type": "append"}
        }))
        .expect("run start")
    }

    fn agent_run_session_state(agent_run_start: &AgentRunStart) -> AgentRunSessionState {
        AgentRunSessionState::new(
            agent_run_start.authorization.session_id.clone(),
            agent_run_start.agent_run_id.clone(),
        )
        .expect("AgentRun Session state")
    }

    fn assistant_projection(turn_id: &str, text: &str) -> AssistantTextProjection {
        let mut projection = AssistantTextProjection::default();
        projection
            .begin_model_request(turn_id.to_string(), String::new())
            .expect("begin model request");
        projection
            .push_token(turn_id, text)
            .expect("append assistant text");
        projection
            .finish_model_request(turn_id)
            .expect("finish model request");
        projection
    }

    struct UnavailableTestExecutionHost;

    impl centaeris_core::execution::ExecutionHostRunner for UnavailableTestExecutionHost {
        fn status(
            &self,
            _policy: &centaeris_core::execution::ExecutionPolicy,
        ) -> Result<
            centaeris_core::execution::ExecutionHostStatus,
            centaeris_core::execution::ExecutionError,
        > {
            Err(centaeris_core::execution::ExecutionError::HostUnavailable {
                reason: "test execution host is unavailable".to_string(),
            })
        }

        fn run_file_system_operation(
            &self,
            _request: centaeris_core::execution::ExecutionFileSystemRequest,
        ) -> Result<
            centaeris_core::execution::ExecutionFileSystemOutput,
            centaeris_core::execution::ExecutionFileSystemError,
        > {
            panic!("test does not execute filesystem operations")
        }

        fn run_host_command(
            &self,
            _operation_id: Option<&str>,
            _request: centaeris_core::execution::ExecutionCommandRequest,
            _cancellation_probe: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
        ) -> Result<
            centaeris_core::execution::ExecutionHostCommandOutput,
            centaeris_core::execution::ExecutionError,
        > {
            Err(centaeris_core::execution::ExecutionError::HostUnavailable {
                reason: "test execution host is unavailable".to_string(),
            })
        }
    }

    #[test]
    fn memory_discovery_reaches_the_model_request_without_plugins_or_eager_reads() {
        use centaeris_core::model::{
            ModelClient, ModelClientError, ModelClientErrorKind, ModelClientFuture,
            ModelClientRequest, ModelClientResponse,
        };
        use centaeris_runtime_sqlite::SqliteRuntimeStore;

        struct DiscoveryHost;
        impl centaeris_core::execution::ExecutionHostRunner for DiscoveryHost {
            fn status(
                &self,
                policy: &centaeris_core::execution::ExecutionPolicy,
            ) -> Result<
                centaeris_core::execution::ExecutionHostStatus,
                centaeris_core::execution::ExecutionError,
            > {
                UnavailableTestExecutionHost.status(policy)
            }
            fn run_file_system_operation(
                &self,
                request: centaeris_core::execution::ExecutionFileSystemRequest,
            ) -> Result<
                centaeris_core::execution::ExecutionFileSystemOutput,
                centaeris_core::execution::ExecutionFileSystemError,
            > {
                assert_eq!(
                    request.model_path, "AGENTS.md",
                    "discovery must not read Memory eagerly"
                );
                Err(centaeris_core::execution::ExecutionFileSystemError::new(
                    centaeris_core::execution::ExecutionFileSystemErrorKind::NotFound,
                    "no fixture AGENTS.md",
                ))
            }
            fn run_host_command(
                &self,
                _id: Option<&str>,
                _request: centaeris_core::execution::ExecutionCommandRequest,
                _probe: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
            ) -> Result<
                centaeris_core::execution::ExecutionHostCommandOutput,
                centaeris_core::execution::ExecutionError,
            > {
                panic!("discovery must not run commands")
            }
        }
        #[derive(Default)]
        struct Capture(Mutex<Vec<ModelClientRequest>>);
        impl ModelClient for Capture {
            fn generate<'a>(
                &'a self,
                request: &'a ModelClientRequest,
            ) -> ModelClientFuture<'a, ModelClientResponse> {
                self.0.lock().expect("capture").push(request.clone());
                Box::pin(async {
                    Err(ModelClientError::new(
                        ModelClientErrorKind::Provider,
                        "offline capture complete".to_string(),
                        false,
                    ))
                })
            }
        }
        let root = std::env::temp_dir().join(format!(
            "centaeris-memory-discovery-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        std::fs::create_dir(&root).expect("isolated fixture");
        {
            let binding = Arc::new(
                ExecutionHostBinding::new(
                    ExecutionHostMode::Remote,
                    Arc::new(DiscoveryHost),
                    root.clone(),
                    centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(&root),
                )
                .expect("host binding"),
            );
            let activation = centaeris_core::extension::build_plugin_activation_snapshot(&[])
                .expect("no plugins");
            let layer = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
                workspace_skill_catalog_config(&activation).expect("installed system skills"),
                binding,
            )
            .expect("tools");
            let location = layer
                .skill_index()
                .entries()
                .iter()
                .find(|skill| skill.name == "memory")
                .expect("built-in memory")
                .skill_md_path
                .clone();
            let store = SqliteRuntimeStore::new(root.join("runtime.sqlite")).expect("store");
            let engine = AgentRuntime::new(
                store,
                layer,
                AgentRuntimeConfig {
                    enable_prompt_compaction: false,
                    ..Default::default()
                },
                ToolConcurrencyCoordinator::new(1),
            );
            let capture = Capture::default();
            let result = tokio::runtime::Runtime::new().expect("runtime").block_on(
                engine.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
                    AgentRunRequest {
                        session_id: "memory-discovery".to_string(), initial_turn_id: "turn-memory".to_string(),
                        initial_input: AgentRunInitialInput::UserMessage(
                            "Remember my synthetic preference: use green headings.".to_string(),
                        ),
                        agent_run_identity: None,
                        runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(),
                        resume_from_turn_id: None, auto_continue_after_resume_wait: None,
                    }, &capture, &EmptyModelSessionConfigStore::new(), &mut |_| {},
                    &|| Ok(None), &mut |_| Ok(()),
                )
            );
            assert!(result.is_err(), "offline provider stops after capture");
            let requests = capture.0.lock().expect("requests");
            assert_eq!(
                requests.len(),
                1,
                "exactly one offline model request: {result:?}"
            );
            let prompt = &requests[0].prepared_prompt;
            let context = prompt
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(context.contains("<name>memory</name>"));
            assert!(context.contains("remember, update, correct, or forget"));
            assert!(context.contains(location.as_str()));
            assert!(!context.contains("Keep `plastic-memories://self/MEMORY.md` as a short index"));
            for name in ["read", "edit", "write"] {
                assert!(
                    prompt.tool_definitions.iter().any(|tool| tool.name == name),
                    "{name}"
                );
            }
            // DiscoveryHost allows only the ordinary AGENTS.md probe; private
            // Memory and skill bodies are not read during prompt construction.
        }
        std::fs::remove_dir_all(root).expect("remove isolated fixture");
    }

    pub(super) fn hosted_context() -> HostedRuntimeContext {
        let workspace_root = std::env::current_dir().expect("test workspace root");
        let execution_host_binding = Arc::new(
            ExecutionHostBinding::new(
                ExecutionHostMode::Remote,
                Arc::new(UnavailableTestExecutionHost),
                workspace_root.clone(),
                centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(
                    workspace_root,
                ),
            )
            .expect("test execution host binding"),
        );
        HostedRuntimeContext {
            agent_run_id: "agent_run_1".to_string(),
            agent_run_identity: RuntimeAgentRunIdentityV1 {
                agent_run_id: "agent_run_1".to_string(),
                execution_id: "execution_1".to_string(),
                authorization_digest: format!("sha256:{}", "a".repeat(64)),
            },
            tool_layer: ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
                centaeris_core::extension::skills::SkillCatalogLoadConfig::default(),
                execution_host_binding,
            )
            .expect("test tool layer"),
            model_client: ApiModelClient::new(ApiModelClientConfig {
                api_internal_url: "http://api.invalid".to_string(),
                internal_api_token: "test-token".to_string(),
                agent_run_id: "agent_run_1".to_string(),
                model_config_ref: "model_1".to_string(),
                authorization_ref: "authorization_1".to_string(),
                authorization_digest: format!("sha256:{}", "a".repeat(64)),
                thinking_mode: None,
                model_max_output_tokens: 32,
            }),
            agent_runtime_config: AgentRuntimeConfig::default(),
            tool_concurrency: ToolConcurrencyCoordinator::new(2),
        }
    }

    fn queued_job(job_kind: &str) -> RuntimeJobRecord {
        RuntimeJobRecord {
            job_id: format!("{job_kind}:job-1"),
            job_kind: job_kind.to_string(),
            status: RuntimeJobStatus::Queued,
            run_at_ms: 1,
            lease_owner: None,
            lease_expires_at_ms: None,
            heartbeat_at_ms: None,
            retry_count: 0,
            max_retries: 0,
            backoff_policy: centaeris_core::session::reliability::RuntimeBackoffPolicy::default(),
            idempotency_key: "job-1".to_string(),
            session_id: Some("sess_1".to_string()),
            branch_id: Some("turn_1".to_string()),
            checkpoint_id: None,
            payload_ref: Some("external_context:packet-1".to_string()),
            output_refs: Vec::new(),
            last_error: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn hosted_subagent_sessions_progress_while_other_session_is_blocked() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime")
            .block_on(async {
                let first_context = hosted_context();
                let mut second_context = hosted_context();
                second_context.agent_run_id = "agent_run_2".to_string();
                second_context.agent_run_identity.agent_run_id = "agent_run_2".to_string();
                let contexts = Arc::new(Mutex::new(HashMap::from([
                    ("sess_1".to_string(), first_context),
                    ("sess_2".to_string(), second_context),
                ])));
                let first_job = queued_job(SUBAGENT_RUN_JOB_KIND);
                let mut second_job = queued_job(SUBAGENT_RUN_JOB_KIND);
                second_job.job_id = "subagent_2".to_string();
                second_job.session_id = Some("sess_2".to_string());
                let batches = hosted_subagent_batches(&[first_job, second_job], &contexts, 1)
                    .expect("plan both ready Sessions");
                let release_first = Arc::new(tokio::sync::Semaphore::new(0));
                let release = release_first.clone();
                let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
                let execution = tokio::spawn(run_hosted_subagent_batches(batches, move |batch| {
                    let started = started.clone();
                    let release = release.clone();
                    async move {
                        started
                            .send((batch.session_id.clone(), batch.context.agent_run_id))
                            .expect("observe dispatched context");
                        if batch.session_id == "sess_1" {
                            let _permit = release.acquire().await.expect("release slow session");
                        }
                        Ok(())
                    }
                }));
                assert_eq!(
                    observed.recv().await,
                    Some(("sess_1".to_string(), "agent_run_1".to_string()))
                );
                let second_before_release =
                    tokio::time::timeout(Duration::from_millis(100), observed.recv()).await;
                release_first.add_permits(1);
                execution
                    .await
                    .expect("scheduler task")
                    .expect("both batches");
                assert_eq!(
                    second_before_release.ok().flatten(),
                    Some(("sess_2".to_string(), "agent_run_2".to_string())),
                    "a ready second Session must progress before the slow Session completes"
                );
            });
    }

    #[test]
    fn hosted_subagent_claim_budgets_preserve_one_pool_per_session_and_total_three() {
        let contexts = Arc::new(Mutex::new(HashMap::from_iter(
            (1..=4).map(|index| (format!("sess_{index}"), hosted_context())),
        )));
        for (sessions, expected) in [
            (vec![1, 1, 1], vec![("sess_1", 3)]),
            (vec![1, 1, 1, 2], vec![("sess_1", 2), ("sess_2", 1)]),
            (
                vec![1, 2, 3, 4],
                vec![("sess_1", 1), ("sess_2", 1), ("sess_3", 1)],
            ),
            (vec![5, 1, 1], vec![("sess_1", 2)]),
        ] {
            let jobs: Vec<_> = sessions
                .iter()
                .enumerate()
                .map(|(index, session)| {
                    let mut job = queued_job(SUBAGENT_RUN_JOB_KIND);
                    job.job_id = format!("subagent_{index}");
                    job.session_id = Some(format!("sess_{session}"));
                    job
                })
                .collect();
            let batches = hosted_subagent_batches(&jobs, &contexts, 1).expect("bounded claims");
            let actual: Vec<_> = batches
                .iter()
                .map(|batch| (batch.session_id.as_str(), batch.limit))
                .collect();
            assert_eq!(actual, expected, "queued Sessions: {sessions:?}");
            assert!(batches.iter().map(|batch| batch.limit).sum::<usize>() <= 3);
            let sessions: HashSet<_> = batches.iter().map(|batch| &batch.session_id).collect();
            assert_eq!(sessions.len(), batches.len());
        }
    }

    #[test]
    fn hosted_subagent_future_retry_does_not_take_a_due_sessions_claim_budget() {
        let contexts = Arc::new(Mutex::new(HashMap::from([
            ("sess_1".to_string(), hosted_context()),
            ("sess_2".to_string(), hosted_context()),
        ])));
        let mut jobs: Vec<_> = (0..3)
            .map(|index| {
                let mut job = queued_job(SUBAGENT_RUN_JOB_KIND);
                job.job_id = format!("due_{index}");
                job
            })
            .collect();
        let mut future = queued_job(SUBAGENT_RUN_JOB_KIND);
        future.job_id = "future_retry".to_string();
        future.session_id = Some("sess_2".to_string());
        future.run_at_ms = i64::MAX;
        jobs.push(future);
        let batches = hosted_subagent_batches(&jobs, &contexts, 1).expect("plan due jobs");
        assert_eq!(
            batches.len(),
            1,
            "future retry cannot reserve a scoped pool"
        );
        assert_eq!(batches[0].session_id, "sess_1");
        assert_eq!(batches[0].limit, 3);
    }

    #[test]
    fn hosted_subagent_fast_failure_keeps_slow_sibling_alive_until_completion() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime")
            .block_on(async {
                let release_slow = Arc::new(tokio::sync::Semaphore::new(0));
                let release = release_slow.clone();
                let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
                let finished_slow = Arc::new(AtomicBool::new(false));
                let finished = finished_slow.clone();
                let execution = tokio::spawn(run_hosted_subagent_batches(
                    vec!["fast-error", "slow-success"],
                    move |session| {
                        let release = release.clone();
                        let started = started.clone();
                        let finished = finished.clone();
                        async move {
                            if session == "fast-error" {
                                return Err("scoped batch failure".to_string());
                            }
                            started.send(()).expect("observe slow sibling");
                            let _permit = release.acquire().await.expect("release slow sibling");
                            finished.store(true, Ordering::SeqCst);
                            Ok(())
                        }
                    },
                ));
                let slow_started =
                    tokio::time::timeout(Duration::from_millis(100), observed.recv())
                        .await
                        .expect("a failed sibling must not suppress another scoped pool");
                assert_eq!(slow_started, Some(()));
                assert!(
                    !execution.is_finished(),
                    "the current batch still awaits its slow pool"
                );
                release_slow.add_permits(1);
                assert_eq!(
                    execution.await.expect("scheduler task"),
                    Err("scoped batch failure".to_string())
                );
                assert!(finished_slow.load(Ordering::SeqCst));
            });
    }

    #[test]
    fn hosted_worker_selects_only_exact_subagent_jobs_with_active_context() {
        let contexts = Arc::new(Mutex::new(HashMap::from([(
            "sess_1".to_string(),
            hosted_context(),
        )])));
        let selected = hosted_subagent_batches(&[queued_job(SUBAGENT_RUN_JOB_KIND)], &contexts, 1)
            .expect("select hosted subagent");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].session_id, "sess_1");
        assert_eq!(selected[0].context.agent_run_id, "agent_run_1");
        assert_eq!(selected[0].limit, 1);
        match hosted_subagent_batches(&[queued_job("provider.poll")], &contexts, 1) {
            Err(error) => assert!(error.contains("unsupported job")),
            Ok(_) => panic!("provider jobs belong to the Core provider scheduler"),
        }
    }

    #[test]
    fn provider_poll_resolver_is_fenced_to_its_source_agent_run() {
        let payload = ProviderPollingRuntimePayload {
            provider_id: "centaeris.knowledge".to_string(),
            tool_name: "search_knowledge".to_string(),
            poll_key: "poll_1".to_string(),
            poll_args: json!({}),
            source_agent_run_id: "agent_run_1".to_string(),
            source_turn_id: "turn_1".to_string(),
            source_tool_call_id: "call_1".to_string(),
            lease_ms: 30_000,
        };
        let mut poll =
            queued_job(centaeris_core::model::provider_polling::PROVIDER_POLL_RUNTIME_JOB_KIND);
        poll.session_id = Some("sess_1".to_string());
        poll.payload_ref = Some(
            centaeris_core::model::provider_polling::build_provider_poll_payload_ref(&payload)
                .expect("poll payload"),
        );
        let mut lifecycle = queued_job(AGENT_RUN_LIFECYCLE_JOB_KIND);
        lifecycle.job_id = "agent_run.lifecycle:agent_run_1".to_string();
        lifecycle.session_id = Some("sess_1".to_string());
        lifecycle.payload_ref = Some("record:agent_run:agent_run_1".to_string());
        let context = hosted_context();

        assert!(matches!(
            resolve_provider_polling_tool_layer_records(
                &poll,
                &payload,
                &lifecycle,
                Some(&context)
            ),
            ProviderPollingToolLayerResolution::Ready(_)
        ));

        lifecycle.status = RuntimeJobStatus::Succeeded;
        assert!(matches!(
            resolve_provider_polling_tool_layer_records(
                &poll,
                &payload,
                &lifecycle,
                None
            ),
            ProviderPollingToolLayerResolution::Stopped { ref reason }
                if reason == "source_agent_run_terminal"
        ));

        lifecycle.status = RuntimeJobStatus::Queued;
        let mut other_context = context.clone();
        other_context.agent_run_id = "agent_run_2".to_string();
        match resolve_provider_polling_tool_layer_records(
            &poll,
            &payload,
            &lifecycle,
            Some(&other_context),
        ) {
            ProviderPollingToolLayerResolution::Failed(error) => {
                assert_eq!(error.kind, ToolFailureKind::ProviderError);
                assert!(!error.retryable);
                assert_eq!(
                    error.diagnostic_id.as_deref(),
                    Some("provider_poll_agent_run_context_mismatch")
                );
            }
            _ => panic!("a new AgentRun context must not inherit an older poll"),
        }

        match resolve_provider_polling_tool_layer_records(&poll, &payload, &lifecycle, None) {
            ProviderPollingToolLayerResolution::Failed(error) => {
                assert_eq!(error.kind, ToolFailureKind::HostUnavailable);
                assert!(error.retryable);
                assert_eq!(
                    error.diagnostic_id.as_deref(),
                    Some("provider_poll_tool_layer_recovering")
                );
            }
            _ => panic!("an active AgentRun without its context must stay recoverable"),
        }
    }

    #[test]
    fn workspace_skill_summary_is_direct_and_hides_runtime_paths() {
        let summary = WorkspaceSkillSummary::from(&SkillEntryV1 {
            skill_id: "plugin-banana-0:banana".to_string(),
            source_id: "plugin-banana-0".to_string(),
            scope: centaeris_core::extension::skills::SkillSourceScopeV1::Plugin,
            name: "banana".to_string(),
            description: "Synthetic extension fixture.".to_string(),
            enabled: true,
            allow_implicit_invocation: true,
            capability_metadata: centaeris_core::extension::skills::SkillCapabilityMetadata {
                allowed_tools: vec!["read".to_string(), "bash".to_string()],
            },
            skill_md_path: "/opt/centaeris/plugins/banana/skills/banana/SKILL.md".to_string(),
            root_path: "/opt/centaeris/plugins/banana/skills/banana".to_string(),
            content_hash: "sha256:content".to_string(),
            shadowed_by: None,
            errors: Vec::new(),
        });

        assert_eq!(
            serde_json::to_value(summary).expect("serialize skill summary"),
            json!({
                "skillId": "plugin-banana-0:banana",
                "name": "banana",
                "description": "Synthetic extension fixture.",
                "enabled": true,
                "allowImplicitInvocation": true,
                "allowedTools": ["read", "bash"],
            })
        );
    }

    #[test]
    fn execution_control_probe_reuses_the_agent_run_scoped_cache() {
        let cache = Mutex::new(None);
        let loads = std::sync::atomic::AtomicUsize::new(0);
        let load = || {
            loads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(None)
        };
        assert_eq!(cached_execution_control_reason(&cache, load), Ok(None));
        assert_eq!(cached_execution_control_reason(&cache, load), Ok(None));
        assert_eq!(loads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn assistant_text_projection_keeps_each_model_response() {
        let mut projection = AssistantTextProjection::default();
        projection
            .begin_model_request("turn_1".to_string(), String::new())
            .expect("first request");
        projection
            .push_token("turn_1", "阶段一：检查工作区")
            .expect("first response");
        projection.mark_tool_call("turn_1").expect("tool call");
        projection
            .finish_model_request("turn_1")
            .expect("first done");
        assert_eq!(projection.terminal_text(), "");

        projection
            .begin_model_request("turn_1:2".to_string(), String::new())
            .expect("second request");
        projection
            .push_token("turn_1:2", "检查完成，已生成结果。")
            .expect("second response");
        projection
            .finish_model_request("turn_1:2")
            .expect("second done");
        assert_eq!(projection.terminal_text(), "检查完成，已生成结果。");
        assert_eq!(projection.responses.len(), 2);
        assert_eq!(projection.responses[0].text, "阶段一：检查工作区");
    }

    #[test]
    fn assistant_text_projection_carries_output_limit_prefix_into_recovery_request() {
        let mut projection = AssistantTextProjection::default();
        projection
            .begin_model_request("turn_1".to_string(), String::new())
            .expect("capped request");
        projection.push_token("turn_1", "partial").expect("partial");
        projection
            .finish_model_request("turn_1")
            .expect("capped boundary");
        projection
            .begin_model_request("turn_1:2".to_string(), "partial".to_string())
            .expect("recovery request");
        projection
            .push_token("turn_1:2", " complete")
            .expect("recovery suffix");
        projection
            .finish_model_request("turn_1:2")
            .expect("recovery done");

        assert_eq!(projection.terminal_text(), "partial complete");
    }

    #[test]
    fn model_process_phase_is_durable_once_and_tool_round_is_not_an_assistant_message() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        let phase = serde_json::from_value::<RuntimeEventProjection>(json!({
            "id": "event-phase-1",
            "version": "v1",
            "type": "Status",
            "sessionId": agent_run_start.authorization.session_id,
            "turnId": "turn_1",
            "taskId": "model_process_status:turn_1",
            "parentTaskId": "turn_1",
            "status": "running",
            "visibility": "user",
            "at": 2,
            "payload": {
                "stage": "model_process_summary",
                "message": "### 读取资料\n\n我会先核对用户提供的资料。"
            },
            "meta": {}
        }))
        .expect("phase event");
        let record = model_process_phase_session_record(&agent_run_start, &phase, &mut sequence)
            .expect("phase record")
            .expect("model phase");
        assert_eq!(record.event.event_type, SessionRecordType::PhaseEvent);
        assert_eq!(record.event.payload["stage"], "model_process_summary");
        assert_eq!(
            model_process_phase_session_record(&agent_run_start, &phase, &mut sequence),
            Ok(None)
        );

        let mut assistant_text = AssistantTextProjection::default();
        assistant_text
            .begin_model_request("turn_1".to_string(), String::new())
            .expect("tool request");
        assistant_text
            .push_token("turn_1", "### 读取资料\n\n我会先核对用户提供的资料。")
            .expect("stage text");
        assistant_text.mark_tool_call("turn_1").expect("tool call");
        assistant_text
            .finish_model_request("turn_1")
            .expect("tool response");
        assistant_text
            .begin_model_request("turn_1:2".to_string(), String::new())
            .expect("final request");
        assistant_text
            .push_token("turn_1:2", "资料核对完成。")
            .expect("final text");
        assistant_text
            .finish_model_request("turn_1:2")
            .expect("final response");
        let records = assistant_session_records(
            &agent_run_start,
            &assistant_text,
            Some("done"),
            &mut sequence,
            3,
        )
        .expect("assistant records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event.turn_id.as_deref(), Some("turn_1:2"));
        assert_eq!(records[0].event.payload["modelMarkdown"], "资料核对完成。");
    }

    #[test]
    fn tool_receipt_keeps_bash_command_on_paired_tool_call() {
        let agent_run_start = agent_run_start();
        let call = ToolCallEnvelope {
            id: "call_bash".to_string(),
            name: "bash".to_string(),
            args_json: json!({
                "command": "python -c \"print('ok')\"",
                "description": "Check Python output"
            })
            .to_string(),
        };
        let result = ToolExecutionResult {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status: "ok".to_string(),
            content: "ok".to_string(),
            details: json!({"exitCode": 0, "stdout": "ok"}),
            facts: Vec::new(),
            error: None,
            started_at_ms: 2,
            completed_at_ms: 3,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: Some("test".to_string()),
        };
        let mut sequence = agent_run_session_state(&agent_run_start);
        let mut records = tool_call_session_records(
            &agent_run_start,
            None,
            "turn_1",
            &call,
            &mut sequence,
            ToolCallRecordContext {
                provider_id: "centaeris.builtin",
                tool_contract_digest:
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                created_at_ms: 2,
            },
        )
        .expect("tool call");
        records.extend(
            tool_result_session_records(
                &agent_run_start,
                None,
                "turn_1",
                &call,
                &result,
                &mut sequence,
                3,
            )
            .expect("tool result"),
        );
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].event.payload["normalizedInput"]["command"],
            "python -c \"print('ok')\""
        );
        assert_eq!(
            records[0].event.payload["displayTarget"],
            "Check Python output"
        );
        let operation = &records[1].event.payload["operations"][0];
        assert_eq!(operation["callId"], "call_bash");
        assert_eq!(operation["toolName"], "bash");
        assert!(operation.get("commandPreview").is_none());
        assert_eq!(operation["exitCode"], 0);
        assert_eq!(records[1].event.payload["latencyMs"], 1);
    }

    #[test]
    fn provider_usage_safe_point_is_durable_and_idempotent() {
        let agent_run_start = agent_run_start();
        let usage = centaeris_core::runtime::contracts::ProviderTokenUsageV1 {
            input_tokens: Some(10),
            output_tokens: Some(2),
            total_tokens: Some(12),
            prompt_cache_hit_tokens: Some(4),
            prompt_cache_miss_tokens: Some(6),
        };
        let mut sequence = agent_run_session_state(&agent_run_start);
        let records =
            provider_usage_session_records(&agent_run_start, "turn_1", &usage, &mut sequence, 2)
                .expect("usage record");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].event.event_type,
            SessionRecordType::ProviderUsage
        );
        assert_eq!(records[0].event.payload["totalTokens"], 12);
        assert!(provider_usage_session_records(
            &agent_run_start,
            "turn_1",
            &usage,
            &mut sequence,
            3,
        )
        .expect("idempotent usage")
        .is_empty());
    }

    #[test]
    fn assistant_text_projection_replaces_current_model_response() {
        let mut projection = AssistantTextProjection::default();
        projection
            .begin_model_request("turn_1".to_string(), String::new())
            .expect("request");
        projection.push_token("turn_1", "partial").expect("partial");
        projection
            .replace_content("turn_1", "完整结果".to_string())
            .expect("replace");
        assert_eq!(projection.terminal_text(), "完整结果");
    }

    #[test]
    fn assistant_message_state_machine_is_sealed_only() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        let message_id = assistant_message_id("turn_1");
        sequence
            .event_for_turn(
                "turn_1",
                SessionRecordType::AssistantMessage,
                json!({
                    "messageId": message_id,
                    "modelMarkdown": "第一段",
                    "artifactRefs": [],
                    "status": "done",
                }),
                1,
            )
            .expect("sealed assistant");
        assert!(sequence.assistant_is_final(message_id.as_str()));
        let error = sequence
            .event_for_turn(
                "turn_1",
                SessionRecordType::AssistantMessage,
                json!({
                    "messageId": message_id,
                    "modelMarkdown": "late",
                    "artifactRefs": [],
                    "status": "done",
                }),
                2,
            )
            .expect_err("second sealed write must fail");
        assert!(error.contains("written after final"));
        let error = sequence
            .event_for_turn(
                "turn_1",
                SessionRecordType::AssistantMessage,
                json!({
                    "messageId": "message:turn_1:assistant",
                    "modelMarkdown": "text",
                    "artifactRefs": [],
                    "status": "running",
                }),
                3,
            )
            .expect_err("running snapshot must be rejected");
        assert!(error.contains("status is unsupported"));
    }

    #[test]
    fn assistant_message_state_machine_rejects_cross_turn_identity() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        sequence
            .event_for_turn(
                "turn_1",
                SessionRecordType::AssistantMessage,
                json!({
                    "messageId": "message:turn_1:assistant",
                    "modelMarkdown": "text",
                    "artifactRefs": [],
                    "status": "done",
                }),
                1,
            )
            .expect("sealed assistant");
        let error = sequence
            .event_for_turn(
                "turn_1:other",
                SessionRecordType::AssistantMessage,
                json!({
                    "messageId": "message:turn_1:assistant",
                    "modelMarkdown": "text",
                    "artifactRefs": [],
                    "status": "error",
                }),
                2,
            )
            .expect_err("cross-turn assistant message must fail");
        assert!(error.contains("messageId does not match turnId"));
    }

    #[test]
    fn tool_call_only_response_has_no_assistant_record() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        let mut assistant_text = AssistantTextProjection::default();
        assistant_text
            .begin_model_request("turn_1".to_string(), String::new())
            .expect("tool request");
        assistant_text.mark_tool_call("turn_1").expect("tool call");
        assistant_text
            .finish_model_request("turn_1")
            .expect("tool response");

        assert!(assistant_session_records(
            &agent_run_start,
            &assistant_text,
            None,
            &mut sequence,
            1,
        )
        .expect("assistant records")
        .is_empty());
    }

    #[test]
    fn runtime_user_admission_uses_authoritative_initial_turn() {
        let start = agent_run_start();
        assert_ne!(start.agent_run_id, start.turn_id);
        let mut sequence = agent_run_session_state(&start);
        let records = started_session_records(&start, &mut sequence, 1).unwrap();
        assert_eq!(records.len(), 2);
        for record in &records {
            assert_eq!(
                record.event.turn_id.as_deref(),
                Some(start.turn_id.as_str())
            );
            assert_eq!(
                record.event.agent_run_id.as_deref(),
                Some(start.agent_run_id.as_str())
            );
        }
        assert_eq!(
            records[1].event.payload["messageId"],
            format!("message:{}:user", start.turn_id)
        );
        assert_eq!(records[1].event.payload["text"], start.prompt);
    }

    #[test]
    fn runtime_builds_direct_session_start_and_terminal_records() {
        let agent_run_start = agent_run_start();
        let response = AgentRunResult {
            turn_responses: vec![],
            stop: AgentRunStop::Finalized,
        };
        let mut sequence = agent_run_session_state(&agent_run_start);
        let started =
            started_session_records(&agent_run_start, &mut sequence, 1).expect("started events");
        let assistant_text = assistant_projection("turn_1", "done");
        let completed = completed_session_records(
            &agent_run_start,
            None,
            &assistant_text,
            &response,
            &mut sequence,
            2,
        )
        .expect("completed events");
        assert_eq!(
            started[0].event.event_type,
            SessionRecordType::AgentRunStarted
        );
        assert_eq!(started[1].event.event_type, SessionRecordType::UserMessage);
        assert!(!started[0].event.event_id.is_empty());
        assert_ne!(started[0].event.event_id, started[1].event.event_id);
        assert_eq!(
            completed[0].event.event_type,
            SessionRecordType::AssistantMessage
        );
        assert_eq!(
            completed[1].event.event_type,
            SessionRecordType::AgentRunCompleted
        );
        assert!(completed_session_records(
            &agent_run_start,
            None,
            &AssistantTextProjection::default(),
            &response,
            &mut sequence,
            2,
        )
        .is_err());
    }

    #[test]
    fn pre_admission_startup_failure_writes_no_session_records() {
        let agent_run_start = agent_run_start();
        let failed = failed_session_records(
            &agent_run_start,
            "sandbox unavailable",
            &AssistantTextProjection::default(),
            &mut agent_run_session_state(&agent_run_start),
            1,
        )
        .expect("startup failure records");
        // The new user turn was never admitted, so nothing enters the model
        // history; the failure stays in the AgentRun control plane.
        assert!(failed.is_empty());
    }

    #[test]
    fn failed_agent_run_without_model_text_emits_no_assistant_placeholder() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        started_session_records(&agent_run_start, &mut sequence, 1).expect("started events");
        let failed = failed_session_records(
            &agent_run_start,
            "provider unavailable",
            &AssistantTextProjection::default(),
            &mut sequence,
            2,
        )
        .expect("failed events");
        assert_eq!(
            failed
                .iter()
                .map(|item| item.event.event_type)
                .collect::<Vec<_>>(),
            vec![SessionRecordType::AgentRunFailed]
        );
    }

    #[test]
    fn completed_agent_run_skips_whitespace_tool_response() {
        let agent_run_start = agent_run_start();
        let mut assistant_text = AssistantTextProjection::default();
        assistant_text
            .begin_model_request("turn_1".to_string(), String::new())
            .expect("tool request");
        assistant_text
            .push_token("turn_1", "\n\n")
            .expect("tool preamble");
        assistant_text.mark_tool_call("turn_1").expect("tool call");
        assistant_text
            .finish_model_request("turn_1")
            .expect("tool response");
        assistant_text
            .begin_model_request("turn_1:2".to_string(), String::new())
            .expect("final request");
        assistant_text
            .push_token("turn_1:2", "done")
            .expect("final text");
        assistant_text
            .finish_model_request("turn_1:2")
            .expect("final response");

        let events = completed_session_records(
            &agent_run_start,
            None,
            &assistant_text,
            &AgentRunResult {
                turn_responses: Vec::new(),
                stop: AgentRunStop::Finalized,
            },
            &mut agent_run_session_state(&agent_run_start),
            1,
        )
        .expect("completed records");

        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].event.event_type,
            SessionRecordType::AssistantMessage
        );
        assert_eq!(events[0].event.turn_id.as_deref(), Some("turn_1:2"));
        assert_eq!(events[0].event.payload["modelMarkdown"], "done");
        assert_eq!(
            events[1].event.event_type,
            SessionRecordType::AgentRunCompleted
        );
    }

    #[test]
    fn workspace_finalization_failure_keeps_sealed_assistant_without_duplicate() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        started_session_records(&agent_run_start, &mut sequence, 1).expect("started events");
        let assistant_text = assistant_projection("turn_1", "completed text");
        let sealed = assistant_session_records(
            &agent_run_start,
            &assistant_text,
            Some("done"),
            &mut sequence,
            2,
        )
        .expect("sealed assistant");
        let failed = failed_session_records(
            &agent_run_start,
            "snapshot collect failed",
            &assistant_text,
            &mut sequence,
            3,
        )
        .expect("workspace failure records");

        assert_eq!(sealed.len(), 1);
        assert_eq!(sealed[0].event.payload["status"], "done");
        assert_eq!(
            failed
                .iter()
                .map(|item| item.event.event_type)
                .collect::<Vec<_>>(),
            vec![SessionRecordType::AgentRunFailed]
        );
        assert_eq!(
            failed[0].event.payload["message"],
            "AgentRun did not complete. Retry the request."
        );
    }

    #[test]
    fn nonempty_recovery_workspace_uses_exact_session_baseline_without_stage() {
        let workspace = agent_run_authorization::SessionWorkspace {
            generation: 3,
            snapshot_sha256: format!("sha256:{}", "a".repeat(64)),
            snapshot_size_bytes: 256,
            expanded_size_bytes: 10,
            file_count: 1,
        };
        let initial = recovery_snapshot_from_session_workspace(&workspace)
            .expect("initial recovery snapshot reuses the nonempty frozen baseline");
        assert_eq!(
            initial.object_ref.as_deref(),
            Some("session-workspace:generation:3")
        );
        // The first checkpoint can reuse this logical ref: the restore dispatcher
        // selects session-workspace/download, never execution-workspace/download.
        assert!(recovery_uses_session_workspace(&initial, &workspace).expect("session route"));
        let mut generation_mismatch = initial.clone();
        generation_mismatch.object_ref = Some("session-workspace:generation:4".to_string());
        let mut noncanonical_generation = initial.clone();
        noncanonical_generation.object_ref = Some("session-workspace:generation:03".to_string());
        let mut sha_mismatch = initial.clone();
        sha_mismatch.snapshot_sha256 = format!("sha256:{}", "b".repeat(64));
        let mut size_mismatch = initial.clone();
        size_mismatch.snapshot_size_bytes += 1;
        let mut expanded_mismatch = initial.clone();
        expanded_mismatch.expanded_size_bytes += 1;
        let mut count_mismatch = initial.clone();
        count_mismatch.file_count += 1;
        for invalid in [
            generation_mismatch,
            noncanonical_generation,
            sha_mismatch,
            size_mismatch,
            expanded_mismatch,
            count_mismatch,
        ] {
            assert!(recovery_uses_session_workspace(&invalid, &workspace)
                .expect_err("mismatched baseline must never reach either download")
                .contains("Session workspace binding mismatch"));
        }
        let staged = RecoveryWorkspaceSnapshotV1 {
            object_ref: Some("workspaces/test/execution-checkpoints/staged.snapshot".to_string()),
            ..initial
        };
        assert!(!recovery_uses_session_workspace(&staged, &workspace).expect("checkpoint route"));
    }

    #[test]
    fn trusted_workspace_generation_skips_42_readonly_bash_snapshots() {
        let known = |generation| ExecutionWorkspaceGeneration::Known {
            token: ExecutionWorkspaceGenerationV1 {
                instance_epoch: "watcher-1".to_string(),
                generation,
            },
        };
        let mut baseline = known(7);
        let mut model_requests = 0;
        let mut collect_helper_calls = 0;
        let mut stage_api_calls = 0;

        for round in 0..20 {
            for _ in 0..if round < 2 { 3 } else { 2 } {
                model_requests += 1;
                let current = baseline.clone(); // read-only bash
                if !workspace_generations_match(&baseline, &current) {
                    collect_helper_calls += 2;
                    stage_api_calls += 1;
                    baseline = current;
                }
            }
        }
        assert_eq!(model_requests, 42);
        let legacy_tool_name_fixture = (model_requests, model_requests);
        assert_eq!(legacy_tool_name_fixture, (42, 42));
        assert_eq!((collect_helper_calls, stage_api_calls), (0, 0));

        let written = known(baseline.token().expect("known baseline").generation + 1);
        if !workspace_generations_match(&baseline, &written) {
            // A non-empty new snapshot is inspected once and streamed once to one stage API.
            collect_helper_calls += 2;
            stage_api_calls += 1;
            baseline = written;
        }
        assert_eq!((collect_helper_calls, stage_api_calls), (2, 1));

        for _ in 0..20 {
            let current = baseline.clone();
            assert!(workspace_generations_match(&baseline, &current));
        }
        assert_eq!((collect_helper_calls, stage_api_calls), (2, 1));

        let reverted = known(baseline.token().expect("known baseline").generation + 2);
        assert!(!workspace_generations_match(&baseline, &reverted));
        // write -> revert still collects once, but the matching descriptor skips the stage API.
        collect_helper_calls += 1;
        baseline = reverted;
        assert_eq!((collect_helper_calls, stage_api_calls), (3, 1));
        assert_eq!(baseline.token().expect("known baseline").generation, 10);
    }

    #[test]
    fn unknown_or_unstable_workspace_generation_forces_another_collect() {
        let baseline = ExecutionWorkspaceGeneration::Known {
            token: ExecutionWorkspaceGenerationV1 {
                instance_epoch: "watcher-1".to_string(),
                generation: 7,
            },
        };
        let unavailable = observed_workspace_generation(&UnavailableTestExecutionHost);
        assert!(unavailable.token().is_none());
        assert!(!workspace_generations_match(&baseline, &unavailable));

        let restarted = ExecutionWorkspaceGeneration::Known {
            token: ExecutionWorkspaceGenerationV1 {
                instance_epoch: "watcher-2".to_string(),
                generation: 7,
            },
        };
        assert!(!workspace_generations_match(&baseline, &restarted));
        assert!(stable_workspace_generation(&baseline, restarted)
            .token()
            .is_none());
    }

    #[test]
    fn duplicate_publications_keep_one_final_attachment_in_first_occurrence_order() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        let mut events =
            started_session_records(&agent_run_start, &mut sequence, 1).expect("start");
        for (index, hash) in ['1', '2'].into_iter().enumerate() {
            let call_id = format!("call_publish_{index}");
            events.push(
                sequence
                    .event_for_turn(
                        agent_run_start.agent_run_id.as_str(),
                        SessionRecordType::ToolCall,
                        json!({
                            "callId": call_id,
                            "toolName": "publish_artifact",
                            "toolContractDigest": format!("sha256:{}", hash.to_string().repeat(64)),
                            "providerId": "centaeris.builtin",
                            "normalizedInput": {"path": "/mnt/data/report.docx"},
                            "displayTarget": "report.docx",
                        }),
                        2,
                    )
                    .expect("tool call"),
            );
            events.push(
                sequence
                    .event_for_turn(
                        agent_run_start.agent_run_id.as_str(),
                        SessionRecordType::ToolResult,
                        json!({
                            "callId": call_id,
                            "toolName": "publish_artifact",
                            "resultState": "successWithOutput",
                            "modelContent": "published",
                            "fullOutputPath": null,
                            "outputStartByte": null,
                            "outputByteLength": 9,
                            "outputComplete": true,
                            "summary": "published",
                            "operations": [],
                            "modelInputImages": [],
                            "latencyMs": 1,
                        }),
                        2,
                    )
                    .expect("tool result"),
            );
            events.push(
                sequence
                    .event_for_turn(
                        agent_run_start.agent_run_id.as_str(),
                        SessionRecordType::ArtifactPublished,
                        json!({
                            "publicationId": format!("pub_{}", hash.to_string().repeat(64)),
                            "artifactRef": "artifact:artifact_1",
                            "toolCallId": call_id,
                            "filename": "report.docx",
                            "sizeBytes": 4,
                            "sha256": format!("sha256:{}", "b".repeat(64)),
                        }),
                        2,
                    )
                    .expect("artifact publication"),
            );
        }
        let terminal = completed_session_records(
            &agent_run_start,
            None,
            &assistant_projection("turn_1", "done"),
            &AgentRunResult {
                turn_responses: vec![],
                stop: AgentRunStop::Finalized,
            },
            &mut sequence,
            3,
        )
        .expect("terminal records");
        assert_eq!(
            terminal[0].event.payload["artifactRefs"],
            json!(["artifact:artifact_1"])
        );
        events.extend(terminal);
        let projection = centaeris_core::session::reduce_events(
            agent_run_start.authorization.session_id.as_str(),
            events.iter().map(|item| &item.event),
        )
        .expect("reduce duplicate publication log");
        assert_eq!(projection.artifact_order, vec!["artifact:artifact_1"]);
    }

    #[test]
    fn failed_agent_run_persists_partial_answer_without_internal_error() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        started_session_records(&agent_run_start, &mut sequence, 1).expect("started events");
        let assistant_text = assistant_projection("turn_1", "已经生成的正文");
        let failed = failed_session_records(
            &agent_run_start,
            "banana",
            &assistant_text,
            &mut sequence,
            2,
        )
        .expect("failed events");
        assert_eq!(failed[0].event.payload["modelMarkdown"], "已经生成的正文");
        assert_eq!(failed[1].event.payload["reasonType"], "banana");
        assert_eq!(
            failed[1].event.payload["message"],
            "AgentRun did not complete. Retry the request."
        );
    }

    #[test]
    fn hosted_model_configuration_failure_retains_its_code_in_session_history() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        started_session_records(&agent_run_start, &mut sequence, 1).unwrap();
        let failure = failed_session_records(
            &agent_run_start,
            r#"model_client_error(kind=invalid_request,retryable=false,providerCode=model_quota_domain_required,providerAttempts=1): hosted_model_error: {"schema":"hosted.model.failure.v1","reasonType":"model_quota_domain_required","httpStatus":null,"providerAttempts":1,"retryExhausted":false}"#,
            &AssistantTextProjection::default(), &mut sequence, 2,
        ).unwrap();
        let payload = &failure.last().unwrap().event.payload;
        assert_eq!(payload["reasonType"], "model_quota_domain_required");
        assert_eq!(payload["message"], "model_quota_domain_required");
    }

    #[test]
    fn hosted_model_terminal_detail_survives_into_failure_history() {
        let start = agent_run_start();
        let mut state = agent_run_session_state(&start);
        started_session_records(&start, &mut state, 1).unwrap();
        let detail = r#"hosted_model_error: {"schema":"hosted.model.failure.v1","reasonType":"provider_rate_limited","httpStatus":429,"providerAttempts":3,"retryExhausted":true}"#;
        let events = failed_session_records(
            &start,
            public_agent_failure_reason(detail, "core_run_failed"),
            &AssistantTextProjection::default(),
            &mut state,
            2,
        )
        .unwrap();
        let payload = &events.last().unwrap().event.payload;
        assert_eq!(payload["reasonType"], "provider_rate_limited");
        assert_eq!(payload["message"], "exceeded retry limit after 3 attempts, last status: 429 Too Many Requests · provider_rate_limited");
        assert!(!provider_response_was_interrupted(&format!(
            "model_client_error(kind=provider_response_interrupted,retryable=false): {detail}"
        )));
    }

    #[test]
    fn exhausted_stream_failure_stays_terminal_after_core_error_conversion() {
        let start = agent_run_start();
        let mut state = agent_run_session_state(&start);
        started_session_records(&start, &mut state, 1).unwrap();
        let error = centaeris_core::model::ModelClientError::new(
            centaeris_core::model::ModelClientErrorKind::ProviderResponseInterrupted,
            r#"hosted_model_error: {"schema":"hosted.model.failure.v1","reasonType":"provider_stream_interrupted","httpStatus":null,"providerAttempts":2,"retryExhausted":true}"#,
            false,
        ).with_provider_attempts(2);
        let error: String =
            centaeris_core::runtime::GenerateDriverError::from_model_client(error, String::new())
                .into();
        assert!(!provider_response_was_interrupted(&error));
        let records = failed_session_records(
            &start,
            public_agent_failure_reason(&error, "core_run_failed"),
            &AssistantTextProjection::default(),
            &mut state,
            2,
        )
        .unwrap();
        let failed = &records.last().unwrap().event;
        assert_eq!(failed.payload["reasonType"], "provider_stream_interrupted");
        assert_eq!(
            failed.payload["message"],
            "exceeded retry limit after 2 attempts · provider_stream_interrupted"
        );
        assert!(records
            .iter()
            .all(|record| record.event.payload.get("retryable") != Some(&serde_json::json!(true))));
    }

    #[test]
    fn completion_delivery_failures_retain_explicit_code_in_public_failed_event() {
        for code in [
            "completion_tool_delivery_required",
            "completion_delivery_repair_prompt_too_large",
        ] {
            let start = agent_run_start();
            let mut state = agent_run_session_state(&start);
            started_session_records(&start, &mut state, 1).unwrap();
            let events = failed_session_records(
                &start,
                public_agent_failure_reason(code, "core_run_failed"),
                &AssistantTextProjection::default(),
                &mut state,
                2,
            )
            .unwrap();
            assert_eq!(events.last().unwrap().event.payload["reasonType"], code);
        }
        assert_eq!(
            public_agent_failure_reason("private arbitrary internal text", "core_run_failed"),
            "core_run_failed"
        );
    }

    #[test]
    fn retryable_execution_interruptions_are_classified_before_terminalization() {
        assert!(provider_response_was_interrupted(
            "model_client_error(kind=provider_response_interrupted,retryable=true)"
        ));
        assert!(execution_environment_was_lost(
            "execution_environment_lost:banana"
        ));
        assert!(!provider_response_was_interrupted("banana"));
        assert!(!execution_environment_was_lost("banana"));
    }

    #[test]
    fn cancelled_agent_run_persists_partial_answer_before_interruption() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        started_session_records(&agent_run_start, &mut sequence, 1).expect("started events");
        let cancelled = interrupted_session_records(
            &agent_run_start,
            &assistant_projection("turn_1", "已经生成的部分正文"),
            &mut sequence,
            2,
            Interruption {
                execution_outcome: "cancelled",
                reason_type: "cancelled",
                message: "AgentRun cancelled by user.",
                retryable: false,
            },
        )
        .expect("cancelled events");

        assert_eq!(cancelled.len(), 2);
        assert_eq!(
            cancelled[0].event.event_type,
            SessionRecordType::AssistantMessage
        );
        assert_eq!(
            cancelled[0].event.payload["modelMarkdown"],
            "已经生成的部分正文"
        );
        assert_eq!(cancelled[0].event.payload["status"], "error");
        assert_eq!(
            cancelled[1].event.event_type,
            SessionRecordType::AgentRunInterrupted
        );
    }

    #[test]
    fn cancelled_agent_run_before_admission_writes_no_session_records() {
        let agent_run_start = agent_run_start();
        let mut sequence = agent_run_session_state(&agent_run_start);
        let cancelled = interrupted_session_records(
            &agent_run_start,
            &AssistantTextProjection::default(),
            &mut sequence,
            1,
            Interruption {
                execution_outcome: "cancelled",
                reason_type: "cancelled",
                message: "AgentRun cancelled by user.",
                retryable: false,
            },
        )
        .expect("cancelled events");

        // Pre-admission cancellation must not enter the model history.
        assert!(cancelled.is_empty());
    }

    #[test]
    fn attached_files_are_projected_into_the_model_user_message_not_system_events() {
        let mut agent_run_start = agent_run_start();
        agent_run_start.authorization.asset_refs.push(
            centaeris_core::tool::inputs::DeclaredInput {
                schema: centaeris_core::tool::inputs::DECLARED_INPUT_SCHEMA.to_string(),
                input_ref: "input_1".to_string(),
                display_name: "notice.md".to_string(),
                content_type: "text/markdown".to_string(),
                input_identity: centaeris_core::tool::inputs::InputIdentityV1 {
                    owner_kind: "userLibraryObject".to_string(),
                    owner_id: "object_1".to_string(),
                    generation: 1,
                    sha256: format!("sha256:{}", "a".repeat(64)),
                },
                size_bytes: 1,
            },
        );
        agent_run_start.authorization.message_asset_refs = vec!["input_1".to_string()];
        let resolved_inputs = ResolvedInputState::new(
            agent_run_start.agent_run_id.clone(),
            agent_run_start.authorization_digest.clone(),
            agent_run_start.authorization.asset_refs.clone(),
            ResolvedInputManifest {
                schema: centaeris_core::tool::inputs::RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
                agent_run_id: agent_run_start.agent_run_id.clone(),
                authorization_digest: agent_run_start.authorization_digest.clone(),
                inputs: vec![centaeris_core::tool::inputs::ResolvedInput {
                    schema: centaeris_core::tool::inputs::RESOLVED_INPUT_SCHEMA.to_string(),
                    input_ref: "input_1".to_string(),
                    object_ref: "object_1".to_string(),
                    owner_kind: "userLibraryObject".to_string(),
                    virtual_path: "notice.md".to_string(),
                    display_name: "notice.md".to_string(),
                    content_type: "text/markdown".to_string(),
                    size_bytes: 1,
                    sha256: format!("sha256:{}", "a".repeat(64)),
                    source_version: "1".to_string(),
                    evidence_kind: "userProvided".to_string(),
                    citation_allowed: true,
                }],
            },
            None,
        )
        .expect("resolved message input");
        let projected = model_user_message(&agent_run_start, &resolved_inputs, &HashMap::new())
            .expect("project model message");
        assert!(projected.contains("Attached session files for this message"));
        assert!(projected.contains("input_1"));
        assert!(projected.contains("Use read_material(input_ref)"));
        assert!(!projected.contains("search_knowledge"));
        assert!(!projected.contains("/mnt/data"));
        let events = started_session_records(
            &agent_run_start,
            &mut agent_run_session_state(&agent_run_start),
            1,
        )
        .expect("started events");
        assert_eq!(events[0].event.payload, json!({"userObjective": "hello"}));
        assert_eq!(events[1].event.payload["text"], "hello");
        assert_eq!(
            events[1].event.payload["attachments"][0]["displayName"],
            "notice.md"
        );
    }

    #[test]
    fn unavailable_message_input_is_projected_as_controlled_model_context() {
        let mut agent_run_start = agent_run_start();
        agent_run_start.authorization.asset_refs.push(
            centaeris_core::tool::inputs::DeclaredInput {
                schema: centaeris_core::tool::inputs::DECLARED_INPUT_SCHEMA.to_string(),
                input_ref: "input_1".to_string(),
                display_name: "removed.md".to_string(),
                content_type: "text/markdown".to_string(),
                input_identity: centaeris_core::tool::inputs::InputIdentityV1 {
                    owner_kind: "userLibraryObject".to_string(),
                    owner_id: "object_1".to_string(),
                    generation: 1,
                    sha256: format!("sha256:{}", "a".repeat(64)),
                },
                size_bytes: 1,
            },
        );
        agent_run_start.authorization.message_asset_refs = vec!["input_1".to_string()];
        let resolved_inputs = ResolvedInputState::new(
            agent_run_start.agent_run_id.clone(),
            agent_run_start.authorization_digest.clone(),
            agent_run_start.authorization.asset_refs.clone(),
            ResolvedInputManifest {
                schema: centaeris_core::tool::inputs::RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
                agent_run_id: agent_run_start.agent_run_id.clone(),
                authorization_digest: agent_run_start.authorization_digest.clone(),
                inputs: Vec::new(),
            },
            None,
        )
        .expect("empty input state");
        let states = HashMap::from([(
            "input_1".to_string(),
            DeferredInputResolutionFailureKind::AssetRemoved,
        )]);
        let projected = model_user_message(&agent_run_start, &resolved_inputs, &states)
            .expect("project unavailable input");
        assert!(projected.contains("state: asset_removed"));
        assert!(!projected.contains("path: /mnt/data"));
    }

    #[test]
    fn resumed_knowledge_read_projects_citation_after_pending_receipt() {
        let agent_run_start = agent_run_start();
        let call = ToolCallEnvelope {
            id: "call_read".to_string(),
            name: "read".to_string(),
            args_json: r#"{"input_ref":"input_1"}"#.to_string(),
        };
        let pending = ToolExecutionResult {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status: "ok".to_string(),
            content: "processing".to_string(),
            details: json!({
                "schema": "knowledge.pending.v1",
                "dynamicTool": true,
                "providerId": "workspace_knowledge",
                "toolName": "read",
            }),
            facts: Vec::new(),
            error: None,
            started_at_ms: 1,
            completed_at_ms: 2,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: None,
        };
        let mut sequence = agent_run_session_state(&agent_run_start);
        let mut pending_events = tool_call_session_records(
            &agent_run_start,
            None,
            "turn_1",
            &call,
            &mut sequence,
            ToolCallRecordContext {
                provider_id: "workspace_knowledge",
                tool_contract_digest:
                    "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                created_at_ms: 1,
            },
        )
        .expect("pending tool call");
        pending_events.extend(
            tool_result_session_records(
                &agent_run_start,
                None,
                "turn_1",
                &call,
                &pending,
                &mut sequence,
                2,
            )
            .expect("pending receipt records"),
        );
        assert_eq!(pending_events.len(), 2);

        let completed = ToolExecutionResult {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status: "ok".to_string(),
            content: "matched evidence".to_string(),
            details: json!({
                "schema": "runtime_job_tool_result.v1",
                "externalObjects": [{
                    "metadata": {
                        "knowledgeCitations": [{
                            "citationId": format!("citation:{}", "b".repeat(64)),
                            "inputRef": "input_1",
                            "ownerRef": "library_object_1",
                            "ownerKind": "userLibraryObject",
                            "displayName": "policy.pdf",
                            "evidenceKind": "userProvided",
                            "ownerSha256": format!("sha256:{}", "a".repeat(64)),
                            "ownerGeneration": 1,
                            "representationId": format!("representation:sha256:{}", "c".repeat(64)),
                            "specDigest": format!("sha256:{}", "d".repeat(64)),
                            "evidenceSha256": format!("sha256:{}", "e".repeat(64)),
                            "sourceToolName": "read",
                            "locator": {
                                "kind": "textSpan",
                                "pageStart": 10,
                                "pageEnd": 10,
                                "startByte": 10,
                                "endByte": 20,
                                "startLine": 4,
                                "endLine": 4
                            }
                        }]
                    }
                }]
            }),
            facts: vec![ToolExecutionFact::CitationRecorded(json!({
                "citationId": format!("citation:{}", "b".repeat(64)),
                "inputRef": "input_1",
                "ownerRef": "library_object_1",
                "ownerKind": "userLibraryObject",
                "displayName": "policy.pdf",
                "evidenceKind": "userProvided",
                "ownerSha256": format!("sha256:{}", "a".repeat(64)),
                "ownerGeneration": 1,
                "representationId": format!("representation:sha256:{}", "c".repeat(64)),
                "specDigest": format!("sha256:{}", "d".repeat(64)),
                "evidenceSha256": format!("sha256:{}", "e".repeat(64)),
                "sourceToolName": "read",
                "sourceToolCallId": "call_read",
                "locator": {
                    "kind": "textSpan",
                    "pageStart": 10,
                    "pageEnd": 10,
                    "startByte": 10,
                    "endByte": 20,
                    "startLine": 4,
                    "endLine": 4
                }
            }))],
            error: None,
            started_at_ms: 1,
            completed_at_ms: 3,
            latency_ms: 2,
            parallel_group: None,
            transition_reason: Some("runtime_job_succeeded".to_string()),
        };
        let turn = TurnStepResult {
            turn_id: "turn_1".to_string(),
            continuation: centaeris_core::runtime::QueryContinuation::ExecuteTools,
            checkpoint: None,
            provider_tool_calls: vec![call],
            tool_results: vec![completed],
            tool_use_summary: None,
            tool_operations_json: None,
            agent_run_resource_usage:
                centaeris_core::runtime::query_loop::AgentRunResourceUsageV1::default(),
            runtime_events: vec![],
            session_snapshot: centaeris_core::session::state::SessionStateSnapshot::new(
                "sess_1".to_string(),
                3,
            ),
        };

        let resumed_events =
            tool_safe_point_session_records(&agent_run_start, None, &turn, &mut sequence, 3)
                .expect("resumed records");
        assert_eq!(resumed_events.len(), 1);
        assert_eq!(
            resumed_events[0].event.event_type,
            SessionRecordType::CitationRecorded
        );
        assert_eq!(resumed_events[0].event.payload["locator"]["pageStart"], 10);
        assert_eq!(sequence.citation_products().len(), 1);
        assert_eq!(
            sequence.citation_products()[0].0,
            resumed_events[0].sequence
        );
        assert_eq!(
            sequence.citation_products()[0].1["citationId"],
            resumed_events[0].event.payload["citationId"]
        );
    }

    #[test]
    fn workspace_uses_core_committed_turn_projection() {
        let terminals = [
            (
                "completed",
                "turn-success",
                "agent-run-success",
                "AgentRunCompleted",
                "done",
            ),
            (
                "failed",
                "turn-failed",
                "agent-run-failed",
                "AgentRunFailed",
                "error",
            ),
            (
                "interrupted",
                "turn-cancelled",
                "agent-run-cancelled",
                "AgentRunInterrupted",
                "done",
            ),
        ];
        for (terminal_kind, turn_id, agent_run_id, expected_type, expected_status) in terminals {
            let mut state = AgentRunSessionState::new("chat-1", agent_run_id).expect("state");
            let mut records = state
                .start(turn_id, "objective", Vec::new(), 1)
                .expect("start records")
                .into_iter()
                .map(|record| record.event)
                .collect::<Vec<_>>();
            records.push(
                state
                    .assistant(
                        turn_id,
                        "answer",
                        Vec::new(),
                        if expected_type == "AgentRunCompleted" {
                            "done"
                        } else {
                            "error"
                        },
                        2,
                    )
                    .expect("assistant record")
                    .expect("assistant event")
                    .event,
            );
            let terminal = match terminal_kind {
                "completed" => state.complete(turn_id, "finalized", 3),
                "failed" => state.fail(turn_id, "runtime_error", "failed", 3),
                "interrupted" => state.interrupt(turn_id, "cancelled", "cancelled", false, 3),
                _ => unreachable!(),
            }
            .expect("terminal record");
            records.push(terminal.event);
            centaeris_core::session::reduce_events("chat-1", records.iter()).expect("Core reducer");
            let assistant_projection = serde_json::to_value(
                centaeris_core::session::project_committed_session_record(&records[2], 2)
                    .expect("assistant projection"),
            )
            .expect("serialize assistant projection");
            let terminal_projection = serde_json::to_value(
                centaeris_core::session::project_committed_session_record(&records[3], 3)
                    .expect("terminal projection"),
            )
            .expect("serialize terminal projection");
            assert_eq!(assistant_projection["type"], "session_event");
            assert_eq!(assistant_projection["event"]["type"], "Final");
            assert_eq!(terminal_projection["type"], "session_event");
            assert_eq!(terminal_projection["event"]["type"], expected_type);
            assert_eq!(terminal_projection["event"]["status"], expected_status);
        }
    }
}
