use std::sync::atomic::{AtomicUsize, Ordering};
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
    ModelSessionConfig, ModelSessionConfigStore,
};
use centaeris_core::runtime::{
    AgentRunRequest, AgentRuntime, AgentRuntimeConfig, ToolConcurrencyCoordinator,
};
use centaeris_core::session::manager::SessionManager;
use centaeris_core::tool::layer::ToolLayer;
use centaeris_runtime_sqlite::SqliteRuntimeStore;

struct IntegrationRunner;

impl ExecutionHostRunner for IntegrationRunner {
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
        unreachable!("integration model does not call tools")
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
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: "SQLite-backed runtime completed.".to_string(),
                    tool_calls: Vec::new(),
                    reasoning_content: None,
                    input_tokens: Some(4),
                    total_tokens: Some(8),
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

struct StaticModelConfig;

impl ModelSessionConfigStore for StaticModelConfig {
    fn get_session_config(&self, _session_id: &str) -> Result<Option<ModelSessionConfig>, String> {
        Ok(Some(ModelSessionConfig::default()))
    }
}

fn temp_root() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "centaeris_core_runtime_{}_{}.db",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&root).expect("create integration temp root");
    root
}

#[tokio::test]
async fn public_agent_runtime_persists_through_sqlite_adapter() {
    let temp_root = temp_root();
    let db_path = temp_root.join("runtime.db");
    let workspace_root = temp_root.join("workspace");
    std::fs::create_dir_all(&workspace_root).expect("create integration workspace");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let tool_layer = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        std::sync::Arc::new(
            ExecutionHostBinding::new(
                ExecutionHostMode::Local,
                std::sync::Arc::new(IntegrationRunner),
                workspace_root.clone(),
                SandboxPolicy::workspace_write_no_network(&workspace_root),
            )
            .expect("create execution host binding"),
        ),
    )
    .expect("create tool layer");
    let runtime = AgentRuntime::new(
        store,
        tool_layer,
        AgentRuntimeConfig::default(),
        ToolConcurrencyCoordinator::new(1),
    );
    let model_client = FinalModelClient {
        requests: AtomicUsize::new(0),
    };

    let result = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            AgentRunRequest {
                session_id: "chat-core-runtime-integration".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-core-runtime-integration".to_string(),
                user_message: "Complete one public runtime turn.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &StaticModelConfig,
            &mut |_| {},
            &|| Ok(None),
            &mut |_| Ok(()),
        )
        .await
        .expect("run public Core runtime against SQLite");

    assert_eq!(
        result.stop,
        centaeris_core::runtime::AgentRunStop::Finalized
    );
    assert_eq!(model_client.requests.load(Ordering::SeqCst), 1);
    let reopened = SqliteRuntimeStore::new(&db_path).expect("reopen sqlite runtime store");
    let session = SessionManager::new(reopened)
        .load_session("chat-core-runtime-integration")
        .expect("load persisted session")
        .expect("persisted session exists");
    assert!(session
        .messages
        .iter()
        .any(|message| message.content == "SQLite-backed runtime completed."));

    drop(runtime);
    let _ = std::fs::remove_dir_all(temp_root);
}
