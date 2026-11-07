use super::*;
use crate::execution::{
    run_policy_scoped_execution_file_system_operation, ExecutionCancellationProbe,
    ExecutionCommandRequest, ExecutionError, ExecutionFileSystemError, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest, ExecutionHostBinding, ExecutionHostCommandOutput,
    ExecutionHostHealth, ExecutionHostKind, ExecutionHostMode, ExecutionHostRunner,
    ExecutionHostStatus, ExecutionPolicy,
};

struct InstructionReadHost {
    trusted: bool,
    revision: AtomicUsize,
    reads: AtomicUsize,
}

impl ExecutionHostRunner for InstructionReadHost {
    fn status(&self, _: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        Ok(ExecutionHostStatus {
            kind: ExecutionHostKind::LocalProcess,
            policy_enforced: false,
            health: ExecutionHostHealth::Ready,
            detail: None,
        })
    }

    fn context_observation_revision(&self) -> Option<u64> {
        self.trusted
            .then(|| self.revision.load(Ordering::SeqCst) as u64)
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        if request.model_path == "AGENTS.md" {
            self.reads.fetch_add(1, Ordering::SeqCst);
        }
        run_policy_scoped_execution_file_system_operation(request)
    }

    fn run_host_command(
        &self,
        _: Option<&str>,
        _: ExecutionCommandRequest,
        _: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        panic!("request preparation must not run user commands")
    }
}

fn instruction_read_tools(cwd: &Path, host: Arc<InstructionReadHost>) -> ToolLayer {
    let binding = ExecutionHostBinding::new(
        ExecutionHostMode::Local,
        host,
        cwd.to_path_buf(),
        ExecutionPolicy::workspace_write_no_network(cwd),
    )
    .unwrap();
    ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        Arc::new(binding),
    )
    .unwrap()
}

#[tokio::test]
async fn agents_observation_reuse_is_request_local_and_requires_host_revision() {
    for trusted in [false, true] {
        let cwd = temp_dir_path("request_local_agents");
        std::fs::create_dir_all(&cwd).unwrap();
        let host = Arc::new(InstructionReadHost {
            trusted,
            revision: AtomicUsize::new(7),
            reads: AtomicUsize::new(0),
        });
        let engine = AgentRuntime::new_for_test_with_tools(
            AgentRuntimeTestStore::new(),
            instruction_read_tools(&cwd, host.clone()),
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                ..Default::default()
            },
        );
        let reads_per_request = if trusted { 1 } else { 2 };
        let mut previous_key = None;
        for (index, content) in [
            None,
            Some("First instructions"),
            Some("Updated instructions"),
        ]
        .into_iter()
        .enumerate()
        {
            if let Some(content) = content {
                std::fs::write(cwd.join("AGENTS.md"), content).unwrap();
            }
            let request = engine
                .build_generate_driver_request_with_async_driver(
                    "request-local-agents",
                    "turn-agents",
                    "inspect",
                    0,
                    &TestPromptCompactionAsyncDriver,
                )
                .await
                .unwrap();
            let observed = request
                .prepared_prompt
                .messages
                .iter()
                .find(|message| message.message_id.ends_with(":agents_context"));
            match content {
                Some(content) => assert!(observed.unwrap().content.contains(content)),
                None => assert!(observed.is_none()),
            }
            if let Some(previous) = previous_key {
                assert_ne!(previous, request.provider_prompt_cache_key);
            }
            previous_key = Some(request.provider_prompt_cache_key);
            assert_eq!(
                host.reads.load(Ordering::SeqCst),
                (index + 1) * reads_per_request
            );
        }
        std::fs::write(cwd.join("AGENTS.md"), [0xff]).unwrap();
        assert!(engine
            .build_generate_driver_request_with_async_driver(
                "request-local-agents",
                "turn-agents",
                "inspect",
                0,
                &TestPromptCompactionAsyncDriver,
            )
            .await
            .unwrap_err()
            .contains("valid UTF-8"));
        std::fs::remove_dir_all(cwd).unwrap();
    }
}

#[tokio::test]
async fn agents_observation_is_refreshed_when_intake_changes_revision() {
    let cwd = temp_dir_path("agents_intake_revision");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "Before intake").unwrap();
    let host = Arc::new(InstructionReadHost {
        trusted: true,
        revision: AtomicUsize::new(1),
        reads: AtomicUsize::new(0),
    });
    let engine = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        instruction_read_tools(&cwd, host.clone()),
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
    );
    let mut intake = |_: &mut TurnInput| {
        std::fs::write(cwd.join("AGENTS.md"), "After intake").unwrap();
        host.revision.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    };
    let request = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            "agents-intake",
            "turn-agents-intake",
            &mut TurnInput::UserMessage("inspect".into()),
            0,
            PromptCompactionScopeV1::main(),
            None,
            None,
            &TestPromptCompactionAsyncDriver,
            Some(&mut intake),
        )
        .await
        .unwrap();
    assert_eq!(host.reads.load(Ordering::SeqCst), 2);
    let observed = request
        .prepared_prompt
        .messages
        .iter()
        .find(|message| message.message_id.ends_with(":agents_context"))
        .unwrap();
    assert!(observed.content.contains("After intake"));
    assert!(!observed.content.contains("Before intake"));
    std::fs::remove_dir_all(cwd).unwrap();
}

#[tokio::test]
async fn agents_observation_is_refreshed_after_compaction() {
    let cwd = temp_dir_path("agents_compaction_revision");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "Compaction instructions").unwrap();
    let host = Arc::new(InstructionReadHost {
        trusted: true,
        revision: AtomicUsize::new(1),
        reads: AtomicUsize::new(0),
    });
    let store = AgentRuntimeTestStore::new();
    SessionManager::new(store.clone())
        .save_session(&model_prompt_compaction_session("agents-compaction"))
        .unwrap();
    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 12_000;
    config.model_max_output_tokens = 4_096;
    config.prompt_compaction_summary_max_tokens = 2_000;
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        instruction_read_tools(&cwd, host.clone()),
        config,
    );
    let request = engine
        .build_generate_driver_request_with_async_driver(
            "agents-compaction",
            "turn-agents-compaction",
            "continue",
            0,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .unwrap();
    assert!(request
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content.starts_with("# Goal")));
    assert_eq!(host.reads.load(Ordering::SeqCst), 2);
    std::fs::remove_dir_all(cwd).unwrap();
}
