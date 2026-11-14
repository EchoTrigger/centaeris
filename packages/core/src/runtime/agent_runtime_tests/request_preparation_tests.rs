use super::*;
use crate::execution::{
    run_policy_scoped_execution_file_system_operation, ExecutionCancellationProbe,
    ExecutionCommandRequest, ExecutionError, ExecutionFileSystemError, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest, ExecutionHostBinding, ExecutionHostCommandOutput,
    ExecutionHostHealth, ExecutionHostKind, ExecutionHostMode, ExecutionHostRunner,
    ExecutionHostStatus, ExecutionPolicy,
};
use base64::Engine as _;

struct ChangingToolCatalog {
    revision: AtomicUsize,
    refreshes: AtomicUsize,
    available: AtomicBool,
}

impl crate::tool::layer::DynamicToolProvider for ChangingToolCatalog {
    fn provider_id(&self) -> &str {
        "mcp:fixture:source"
    }
    fn has_dynamic_catalog(&self) -> bool {
        true
    }
    fn refresh_catalog<'a>(&'a self) -> crate::tool::layer::DynamicToolCatalogFuture<'a> {
        Box::pin(async move {
            self.refreshes.fetch_add(1, Ordering::SeqCst);
            if !self.available.load(Ordering::SeqCst) {
                return Ok(Some(Vec::new()));
            }
            Ok(Some(vec![crate::tool::DynamicToolContract {
                name: "source_search".into(),
                category: "external.mcp".into(),
                summary: format!(
                    "Search source revision {}.",
                    self.revision.load(Ordering::SeqCst)
                ),
                input_schema: json!({"type":"object"}),
                provider_id: self.provider_id().into(),
                scopes: vec!["source:read".into()],
                concurrency_safe: true,
                turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
            }]))
        })
    }
    fn execute<'a>(
        &'a self,
        request: crate::tool::layer::DynamicToolProviderRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::tool::layer::DynamicToolProviderResponse, String>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            Ok(crate::tool::layer::DynamicToolProviderResponse {
                content: request.contract.summary,
                details: json!({}),
                is_error: false,
                facts: vec![],
                transition_reason: None,
            })
        })
    }
}

#[tokio::test]
async fn dynamic_catalog_refreshes_requests_and_preserves_delegated_contracts() {
    let source = Arc::new(ChangingToolCatalog {
        revision: AtomicUsize::new(0),
        refreshes: AtomicUsize::new(0),
        available: AtomicBool::new(true),
    });
    let mut tools = ToolLayer::new().with_cwd(std::env::temp_dir()).unwrap();
    tools
        .register_dynamic_tool_provider(source.clone())
        .unwrap();
    let engine = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
    );
    let initial = engine
        .build_generate_driver_request_with_async_driver(
            "catalog-parent",
            "catalog-turn",
            "search",
            0,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .unwrap();
    let contract = engine.tools_port.tool_contract("source_search").unwrap();
    let old_environment = engine.agent_composition_environment().unwrap();
    let binding = crate::runtime::subagent::SubagentWorkPacketRuntimeBindingV1 {
        child_session_id: "catalog-child".into(),
        child_turn_id: "catalog-child-turn".into(),
        subagent_id: "catalog-agent".into(),
        parent_agent_run_id: "parent-run".into(),
        parent_turn_id: "catalog-turn".into(),
        cancel_on_parent_cancel: true,
        description: "Search delegated source".into(),
        allowed_tools: vec!["source_search".into()],
        delegated_tool_contracts: vec![
            crate::runtime::subagent_contracts::DelegatedToolContractV1 {
                name: contract.name.clone(),
                provider_id: contract.provider_id.clone().unwrap(),
                contract_digest: contract.contract_digest().unwrap(),
                concurrency_safe: contract.concurrency_safe,
            },
        ],
    };
    let child = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        engine.tools_port.clone(),
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            allowed_tools: Some(binding.allowed_tools.clone()),
            ..Default::default()
        },
    );
    child
        .prepare_subagent_tool_contracts(&binding)
        .await
        .unwrap();
    let refreshes = source.refreshes.load(Ordering::SeqCst);
    source.revision.store(1, Ordering::SeqCst);
    let frozen_child = child
        .build_generate_driver_request_with_async_driver(
            "catalog-child",
            "catalog-child-turn",
            "search",
            0,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .unwrap();
    assert_eq!(
        source.refreshes.load(Ordering::SeqCst),
        refreshes,
        "validated delegate must not refresh its contract"
    );
    assert_eq!(
        frozen_child.prepared_prompt.tool_definitions[0].description,
        "Search source revision 0."
    );
    let current = engine
        .build_generate_driver_request_with_async_driver(
            "catalog-parent",
            "catalog-turn",
            "search",
            1,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .unwrap();
    assert_eq!(
        initial
            .tool_contracts
            .iter()
            .find(|tool| tool.name == "source_search")
            .unwrap()
            .summary,
        "Search source revision 0."
    );
    assert_eq!(
        current
            .tool_contracts
            .iter()
            .find(|tool| tool.name == "source_search")
            .unwrap()
            .summary,
        "Search source revision 1."
    );
    let session = engine
        .session_manager
        .load_or_create_session("catalog-parent")
        .unwrap();
    let batch = engine
        .execute_tool_calls_with_safe_point_async(
            "catalog-parent",
            "catalog-execution",
            &session,
            GenerateResult {
                content: String::new(),
                tool_calls: vec![crate::model::ToolCallEnvelope {
                    id: "frozen-call".into(),
                    name: "source_search".into(),
                    args_json: "{}".into(),
                }],
                reasoning_content: None,
                continuation_reasoning_content: None,
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            None,
            None,
            None,
            Some(&initial.execution_tools),
        )
        .await
        .unwrap();
    assert_eq!(
        batch.tool_results[0].content, "Search source revision 0.",
        "a later Session refresh cannot change the earlier request execution contract"
    );
    let stale_child = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        engine.tools_port.clone(),
        AgentRuntimeConfig::default(),
    );
    assert!(stale_child
        .prepare_subagent_tool_contracts(&binding)
        .await
        .unwrap_err()
        .contains("delegated tool contract drift"));

    let model = CompleteTurnTestModelClient {
        request_count: AtomicUsize::new(0),
        include_sibling: false,
        sibling_first: false,
    };
    let config = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut committed = Vec::new();
    let mut sink = |point| {
        if let ToolSafePoint::ModelRequestStarted(started) = point {
            committed.push(started);
        }
        Ok(())
    };
    let dispatcher = ToolSafePointDispatcher {
        sink: Mutex::new(&mut sink),
    };
    let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
        &model,
        &config,
        &dispatcher,
        &old_environment,
        Some("catalog-run"),
    );
    driver.generate_next_async(&current).await.unwrap();
    assert_eq!(
        committed[0]
            .agent_composition
            .tool_contracts
            .iter()
            .find(|tool| tool.name == "source_search")
            .unwrap()
            .summary,
        "Search source revision 1."
    );
}

#[tokio::test]
async fn excluded_dynamic_directory_keeps_unrelated_model_tools_available() {
    let source = Arc::new(ChangingToolCatalog {
        revision: AtomicUsize::new(0),
        refreshes: AtomicUsize::new(0),
        available: AtomicBool::new(false),
    });
    let mut tools = ToolLayer::new().with_cwd(std::env::temp_dir()).unwrap();
    tools
        .register_dynamic_tool_provider(source.clone())
        .unwrap();
    let engine = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
    );
    let request = engine
        .build_generate_driver_request_with_async_driver(
            "catalog-isolation",
            "isolation-turn",
            "write a document",
            0,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .unwrap();
    assert!(request
        .prepared_prompt
        .tool_definitions
        .iter()
        .any(|tool| tool.name == "write"));
    assert!(!request
        .prepared_prompt
        .tool_definitions
        .iter()
        .any(|tool| tool.name == "source_search"));
    source.available.store(true, Ordering::SeqCst);
    let request = engine
        .build_generate_driver_request_with_async_driver(
            "catalog-isolation",
            "isolation-turn",
            "write a document",
            1,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .unwrap();
    assert!(request
        .prepared_prompt
        .tool_definitions
        .iter()
        .any(|tool| tool.name == "source_search"));
}

struct InstructionReadHost {
    trusted: bool,
    revision: AtomicUsize,
    reads: AtomicUsize,
    image_reads: AtomicUsize,
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
        if request.model_path == "page.png" {
            self.image_reads.fetch_add(1, Ordering::SeqCst);
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
            image_reads: AtomicUsize::new(0),
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
        image_reads: AtomicUsize::new(0),
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
        image_reads: AtomicUsize::new(0),
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

#[tokio::test]
async fn image_estimation_skips_host_reads_and_history_survives_runtime_recreation() {
    let root = temp_dir_path("retained_image_preparation");
    std::fs::create_dir_all(&root).unwrap();
    let (pixels, reference) = consumption_image_reference(&root);
    let host = Arc::new(InstructionReadHost {
        trusted: true,
        revision: AtomicUsize::new(1),
        reads: AtomicUsize::new(0),
        image_reads: AtomicUsize::new(0),
    });
    let store = AgentRuntimeTestStore::new();
    let mut session = SessionStateSnapshot::new("retained-image".into(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    let mut metadata = JsonMap::new();
    metadata.insert(
        MESSAGE_SEMANTIC_KIND_META_KEY.into(),
        MESSAGE_SEMANTIC_USER_REQUEST.into(),
    );
    metadata.insert(
        crate::runtime::keys::metadata::MODEL_INPUT_IMAGE_SOURCES.into(),
        serde_json::to_string(&vec![
            crate::model::prepared_prompt::ModelInputImageSourceRefV1::ExecutionFile {
                image: reference.clone(),
            },
        ])
        .unwrap(),
    );
    handler.push_user_message(&mut session, "Describe [Previously read image]", metadata);
    SessionManager::new(store.clone())
        .save_session(&session)
        .unwrap();
    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        instruction_read_tools(&root, host.clone()),
        config.clone(),
    );
    let mut intake = |_: &mut TurnInput| {
        assert_eq!(
            host.image_reads.load(Ordering::SeqCst),
            0,
            "estimation must not enter the image Host path"
        );
        Ok(None)
    };
    let before = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            "retained-image",
            "image-first",
            &mut TurnInput::UserMessage("Describe the image".into()),
            1,
            PromptCompactionScopeV1::main(),
            None,
            None,
            &TestPromptCompactionAsyncDriver,
            Some(&mut intake),
        )
        .await
        .unwrap();
    assert_eq!(
        host.image_reads.load(Ordering::SeqCst),
        1,
        "legacy references hydrate only in the formal request"
    );
    assert_eq!(before.prepared_prompt.input_images.len(), 1);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&before.prepared_prompt.input_images[0].data_base64)
            .unwrap(),
        pixels
    );
    assert!(store
        .load_model_input_image("other-session", &reference.sha256)
        .unwrap()
        .is_none());
    drop(engine);
    std::fs::remove_file(root.join("page.png")).unwrap();
    let resumed = AgentRuntime::new_for_test_with_tools(
        store,
        instruction_read_tools(&root, host.clone()),
        config,
    );
    let after = resumed
        .build_generate_driver_request_with_async_driver(
            "retained-image",
            "image-resumed",
            "continue",
            2,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .unwrap();
    assert_eq!(
        before.prepared_prompt.input_images,
        after.prepared_prompt.input_images
    );
    assert_eq!(
        host.image_reads.load(Ordering::SeqCst),
        1,
        "recreated runtimes use immutable history, not the deleted source"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn image_tool_read_retains_pixels_before_the_source_changes() {
    let root = temp_dir_path("image_read_ingestion");
    std::fs::create_dir_all(&root).unwrap();
    let (pixels, reference) = consumption_image_reference(&root);
    let store = AgentRuntimeTestStore::new();
    let tools = ToolLayer::new().with_cwd(root.clone()).unwrap();
    let engine =
        AgentRuntime::new_for_test_with_tools(store.clone(), tools, AgentRuntimeConfig::default());
    let session = SessionStateSnapshot::new("read-ingestion".into(), 0);
    let batch = engine
        .execute_tool_calls_async(
            "read-ingestion",
            "read-turn",
            &session,
            read_generate_result("image-read", "page.png"),
            None,
        )
        .await
        .unwrap();
    let report = &batch.tool_results[0];
    assert_eq!(report.status, "ok", "{report:?}");
    std::fs::write(root.join("page.png"), "changed").unwrap();
    assert_eq!(
        store
            .load_model_input_image("read-ingestion", &reference.sha256)
            .unwrap()
            .unwrap(),
        pixels
    );
    std::fs::remove_dir_all(root).unwrap();
}
