use super::*;
use crate::execution::{
    run_policy_scoped_execution_file_system_operation, ExecutionCancellationProbe,
    ExecutionCommandRequest, ExecutionError, ExecutionFileSystemError, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest, ExecutionHostBinding, ExecutionHostCommandOutput,
    ExecutionHostHealth, ExecutionHostKind, ExecutionHostMode, ExecutionHostRunner,
    ExecutionHostStatus, ExecutionPolicy,
};
use base64::Engine as _;

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
