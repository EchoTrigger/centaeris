use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::execution::sandbox::{
    SandboxErr, SandboxPolicy, SandboxTransformRequest, SandboxType,
};
use centaeris_core::execution::{
    ExecutionFileSystemError, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
    ExecutionHostBinding, ExecutionHostCommandOutput, ExecutionHostHealth, ExecutionHostKind,
    ExecutionHostMode, ExecutionHostRunner, ExecutionHostStatus,
};
use centaeris_core::extension::skills::SkillCatalogLoadConfig;
use centaeris_core::model::prompt::PromptCompactionScopeV1;
use centaeris_core::model::{
    GenerateResult, ModelClient, ModelClientFuture, ModelClientRequest, ModelClientResponse,
    ModelSessionConfig, ModelSessionConfigStore, ToolCallEnvelope,
};
use centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1;
use centaeris_core::runtime::{
    AgentRunRequest, AgentRunStop, AgentRuntime, AgentRuntimeConfig, DurableTurnControlBinding,
    ToolConcurrencyCoordinator, TurnControl,
};
use centaeris_core::session::manager::SessionManager;
use centaeris_core::session::reliability::{
    RuntimeBackoffPolicy, RuntimeJobRecord, RuntimeJobStatus, RuntimeJobStorePort,
    ScheduleRuntimeJobRequest, AGENT_RUN_LIFECYCLE_JOB_KIND,
};
use centaeris_core::session::state::ModelMessageSemanticsV1;
use centaeris_core::session::supplement::{
    DurableTurnSupplement, EnqueueTurnSupplementRequest, TurnSupplementStorePort,
};
use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse, ToolLayer,
};
use centaeris_core::tool::{DynamicToolContract, DynamicToolRegistry, ToolTurnBehavior};
use rusqlite::params;
use serde_json::json;

use crate::SqliteRuntimeStore;

struct FaultTestRunner;

impl ExecutionHostRunner for FaultTestRunner {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::LocalProcess
    }

    fn status(&self, _policy: &SandboxPolicy) -> Result<ExecutionHostStatus, SandboxErr> {
        Ok(ExecutionHostStatus {
            kind: ExecutionHostKind::LocalProcess,
            sandbox_type: SandboxType::HostProcess,
            health: ExecutionHostHealth::Ready,
            detail: None,
        })
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        centaeris_core::execution::run_policy_scoped_execution_file_system_operation(request)
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        _request: SandboxTransformRequest,
        _cancellation_probe: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, SandboxErr> {
        unreachable!("fault tests do not execute host commands")
    }
}

struct StaticModelConfig;

impl ModelSessionConfigStore for StaticModelConfig {
    fn get_session_config(&self, _session_id: &str) -> Result<Option<ModelSessionConfig>, String> {
        Ok(Some(ModelSessionConfig::default()))
    }
}

#[derive(Debug)]
struct FinalModelClient {
    requests: AtomicUsize,
}

impl ModelClient for FinalModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(model_response("final after durable supplement", Vec::new()))
        })
    }
}

#[derive(Debug)]
struct CompleteTurnModelClient {
    requests: AtomicUsize,
}

impl ModelClient for CompleteTurnModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(if request_index == 0 {
                model_response(
                    "",
                    vec![ToolCallEnvelope {
                        id: "call-terminal-snapshot".to_string(),
                        name: "terminal_snapshot_test".to_string(),
                        args_json: "{}".to_string(),
                    }],
                )
            } else {
                model_response("repaired without replay", Vec::new())
            })
        })
    }
}

struct SnapshotFailingProvider {
    store: SqliteRuntimeStore,
    executions: Arc<AtomicUsize>,
}

impl DynamicToolProvider for SnapshotFailingProvider {
    fn provider_id(&self) -> &str {
        "test.snapshot_failure"
    }

    fn execute<'a>(
        &'a self,
        _request: DynamicToolProviderRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<DynamicToolProviderResponse, String>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.executions.fetch_add(1, Ordering::SeqCst);
            self.store.fail_next_agent_runtime_snapshot_save();
            Ok(DynamicToolProviderResponse {
                content: "accepted".to_string(),
                details: json!({"accepted": true}),
                is_error: false,
                facts: Vec::new(),
                transition_reason: Some("terminal_snapshot_test".to_string()),
            })
        })
    }
}

fn model_response(content: &str, tool_calls: Vec<ToolCallEnvelope>) -> ModelClientResponse {
    ModelClientResponse {
        generate_result: GenerateResult {
            content: content.to_string(),
            tool_calls,
            reasoning_content: None,
            input_tokens: None,
            total_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        },
        provider_request_id: None,
        provider_latency_ms: None,
        provider_attempts: 1,
    }
}

fn temp_paths(suffix: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "centaeris_agent_runtime_fault_{suffix}_{}_{}",
        std::process::id(),
        unique
    ));
    std::fs::create_dir_all(&root).expect("create fault test root");
    (root.join("runtime.db"), root)
}

fn tool_layer(workspace_root: &std::path::Path, registry: Arc<DynamicToolRegistry>) -> ToolLayer {
    ToolLayer::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        registry,
        Arc::new(
            ExecutionHostBinding::new(
                ExecutionHostMode::Local,
                Arc::new(FaultTestRunner),
                workspace_root.to_path_buf(),
                SandboxPolicy::workspace_write_no_network(workspace_root),
            )
            .expect("create fault test execution host binding"),
        ),
    )
    .expect("create fault test tool layer")
}

fn runtime(
    store: SqliteRuntimeStore,
    workspace_root: &std::path::Path,
    registry: Arc<DynamicToolRegistry>,
) -> AgentRuntime<SqliteRuntimeStore> {
    AgentRuntime::new(
        store,
        tool_layer(workspace_root, registry),
        AgentRuntimeConfig::default(),
        ToolConcurrencyCoordinator::new(1),
    )
}

fn run_request(session_id: &str, turn_id: &str) -> AgentRunRequest {
    AgentRunRequest {
        session_id: session_id.to_string(),
        agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
            agent_run_id: format!("agent-run-{session_id}"),
            execution_id: format!("execution-{session_id}"),
            authorization_digest: format!("sha256:{}", "b".repeat(64)),
        }),
        initial_turn_id: turn_id.to_string(),
        user_message: "exercise snapshot fault boundary".to_string(),
        runtime_scope: PromptCompactionScopeV1::main(),
        resume_from_turn_id: None,
        auto_continue_after_resume_wait: None,
    }
}

#[tokio::test]
async fn durable_turn_supplement_is_not_acked_when_prompt_snapshot_fails() {
    let (db_path, root) = temp_paths("durable_supplement");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite store");
    let agent_run_id = "agent-run-durable-supplement";
    let session_id = "chat-durable-supplement";
    let authorization_digest = format!("sha256:{}", "a".repeat(64));
    let lifecycle_job_id = format!("agent_run.lifecycle:{agent_run_id}");
    let lease_owner = "worker-durable-supplement";
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: lifecycle_job_id.clone(),
                job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.to_string(),
                status: RuntimeJobStatus::Running,
                run_at_ms: 1,
                lease_owner: Some(lease_owner.to_string()),
                lease_expires_at_ms: Some(i64::MAX),
                heartbeat_at_ms: Some(1),
                retry_count: 0,
                max_retries: 1,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: format!(
                    "agent_run.lifecycle:{agent_run_id}:{authorization_digest}"
                ),
                session_id: Some(session_id.to_string()),
                branch_id: None,
                checkpoint_id: None,
                payload_ref: Some(format!("record:agent_run:{agent_run_id}")),
                output_refs: Vec::new(),
                last_error: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        })
        .expect("schedule lifecycle job");
    store
        .enqueue_turn_supplement(EnqueueTurnSupplementRequest {
            agent_run_id: agent_run_id.to_string(),
            lifecycle_job_id: lifecycle_job_id.clone(),
            session_id: session_id.to_string(),
            authorization_digest: authorization_digest.clone(),
            supplement_id: "supplement-snapshot-failure".to_string(),
            message: "Preserve this after a snapshot failure.".to_string(),
            created_at_ms: 2,
        })
        .expect("enqueue durable supplement");
    let failure_store = store.clone();
    let materialized_turn_id = Arc::new(Mutex::new(None));
    let captured_turn_id = materialized_turn_id.clone();
    let turn_control = TurnControl::new_durable(
        Arc::new(store.clone()),
        DurableTurnControlBinding {
            agent_run_id: agent_run_id.to_string(),
            lifecycle_job_id,
            session_id: session_id.to_string(),
            authorization_digest: authorization_digest.clone(),
            lease_owner: lease_owner.to_string(),
            claim_token: "claim-before-snapshot-failure".to_string(),
        },
        Arc::new(move |turn_id, _| {
            *captured_turn_id.lock().expect("capture materialized turn") =
                Some(turn_id.to_string());
            failure_store.fail_next_agent_runtime_snapshot_save();
            Ok(())
        }),
    )
    .expect("create durable turn control");
    let runtime = runtime(store.clone(), &root, Arc::new(DynamicToolRegistry::empty()));
    let model_client = FinalModelClient {
        requests: AtomicUsize::new(0),
    };
    let error = runtime
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            AgentRunRequest {
                session_id: session_id.to_string(),
                agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
                    agent_run_id: agent_run_id.to_string(),
                    execution_id: "execution-durable-supplement".to_string(),
                    authorization_digest,
                }),
                initial_turn_id: "turn-durable-supplement".to_string(),
                user_message: "Start the durable run.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &StaticModelConfig,
            &mut |_| {},
            &|| Ok(None),
            &turn_control,
            &mut |_| Ok(()),
        )
        .await
        .expect_err("snapshot failure must stop before provider retry");
    assert!(
        error.contains("injected one-shot session runtime snapshot save failure"),
        "unexpected durable supplement snapshot failure: {error}"
    );
    assert!(materialized_turn_id
        .lock()
        .expect("read materialized turn")
        .as_deref()
        .is_some_and(|turn_id| turn_id.starts_with("turn-")));
    store
        .with_conn(|connection| {
            let (accepting, entries_json) = connection
                .query_row(
                    "SELECT accepting,entries_json FROM runtime_turn_supplement_queues WHERE agent_run_id=?1",
                    params![agent_run_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(accepting, 1);
            assert_eq!(
                serde_json::from_str::<Vec<DurableTurnSupplement>>(&entries_json)
                    .map_err(|error| error.to_string())?
                    .len(),
                1
            );
            Ok(())
        })
        .expect("supplement remains durable after snapshot failure");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn terminal_snapshot_failure_retry_repairs_transcript_without_replaying_side_effects() {
    let (db_path, root) = temp_paths("terminal_repair");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite store");
    let executions = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(
        DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "terminal_snapshot_test".to_string(),
            category: "test".to_string(),
            summary: "Complete after injecting a snapshot failure.".to_string(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            provider_id: "test.snapshot_failure".to_string(),
            scopes: Vec::new(),
            concurrency_safe: false,
            turn_behavior: ToolTurnBehavior::CompleteTurnOnSuccess,
        }])
        .expect("create dynamic registry"),
    );
    let mut tool_layer = tool_layer(&root, registry);
    tool_layer
        .register_dynamic_tool_provider(Arc::new(SnapshotFailingProvider {
            store: store.clone(),
            executions: executions.clone(),
        }))
        .expect("register snapshot failing provider");
    let runtime = AgentRuntime::new(
        store.clone(),
        tool_layer,
        AgentRuntimeConfig::default(),
        ToolConcurrencyCoordinator::new(1),
    );
    let model_client = CompleteTurnModelClient {
        requests: AtomicUsize::new(0),
    };

    let error = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            run_request("chat-terminal-repair", "turn-terminal-repair"),
            &model_client,
            &StaticModelConfig,
            &mut |_| {},
            &|| Ok(None),
            &mut |_| Ok(()),
        )
        .await
        .expect_err("terminal snapshot save must fail once");
    assert!(
        error.contains("injected one-shot session runtime snapshot save failure"),
        "unexpected terminal snapshot failure: {error}"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 1);

    let recovery_request = run_request("chat-terminal-repair", "turn-terminal-repair");
    let recovered = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            recovery_request,
            &model_client,
            &StaticModelConfig,
            &mut |_| {},
            &|| Ok(None),
            &mut |_| Ok(()),
        )
        .await
        .expect("repair persisted tool receipt");
    assert_eq!(recovered.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.requests.load(Ordering::SeqCst), 2);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let session = SessionManager::new(store)
        .load_or_create_session("chat-terminal-repair")
        .expect("load repaired session");
    let assistant_indices = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            matches!(
                session.model_semantics.get(message.message_id.as_str()),
                Some(ModelMessageSemanticsV1::Assistant { tool_calls, .. })
                    if tool_calls.iter().any(|call| call.id == "call-terminal-snapshot")
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let tool_indices = session
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            matches!(
                session.model_semantics.get(message.message_id.as_str()),
                Some(ModelMessageSemanticsV1::ToolResult { tool_call_id, .. })
                    if tool_call_id == "call-terminal-snapshot"
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(assistant_indices.len(), 1);
    assert_eq!(tool_indices, vec![assistant_indices[0] + 1]);
    let _ = std::fs::remove_dir_all(root);
}
