use super::*;

struct DeliveryModel {
    calls: AtomicUsize,
    repair: bool,
}
impl ModelClient for DeliveryModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let tool_calls = if self.repair && index == 1 {
                assert_eq!(
                    request.prepared_prompt.tool_choice,
                    ModelToolChoice::Specific {
                        name: "stream_boundary_test_tool".into()
                    }
                );
                vec![ToolCallEnvelope {
                    id: "real-delivery-call".into(),
                    name: "stream_boundary_test_tool".into(),
                    args_json: json!({"value":"public reply"}).to_string(),
                }]
            } else {
                vec![]
            };
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if tool_calls.is_empty() {
                        "Private candidate answer".into()
                    } else {
                        String::new()
                    },
                    tool_calls,
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: Some(10),
                    total_tokens: Some(20),
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
fn request() -> AgentRunRequest {
    AgentRunRequest {
        session_id: "required-delivery".into(),
        initial_turn_id: "initial-delivery-turn".into(),
        initial_input: AgentRunInitialInput::UserMessage("Hello".into()),
        agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
            agent_run_id: "delivery-run".into(),
            execution_id: "execution-delivery".into(),
            authorization_digest: format!("sha256:{}", "a".repeat(64)),
        }),
        runtime_scope: PromptCompactionScopeV1::main(),
        resume_from_turn_id: None,
        auto_continue_after_resume_wait: None,
    }
}

#[derive(Debug)]
struct EmptyDeliveryQueue;
impl crate::session::turn_input::TurnInputStorePort for EmptyDeliveryQueue {
    fn claim_turn_inputs(
        &self,
        _: crate::session::turn_input::ClaimTurnInputsRequest,
    ) -> Result<Vec<crate::session::turn_input::DurableTurnInput>, String> {
        Ok(vec![])
    }
    fn acknowledge_turn_inputs(
        &self,
        _: crate::session::turn_input::AcknowledgeTurnInputsRequest,
    ) -> Result<(), String> {
        Ok(())
    }
    fn close_turn_input_queue(
        &self,
        _: crate::session::turn_input::CloseTurnInputQueueRequest,
    ) -> Result<(), String> {
        Ok(())
    }
}
fn durable_control(req: &AgentRunRequest, token: &str) -> TurnControl {
    let identity = req.agent_run_identity.as_ref().unwrap();
    TurnControl::new_durable_inputs(
        Arc::new(EmptyDeliveryQueue),
        DurableTurnControlBinding {
            agent_run_id: identity.agent_run_id.clone(),
            session_id: req.session_id.clone(),
            lifecycle_job_id: "delivery-job".into(),
            authorization_digest: identity.authorization_digest.clone(),
            lease_owner: "delivery-owner".into(),
            claim_token: token.into(),
        },
        Arc::new(|_, _| Ok(())),
    )
    .unwrap()
}

#[tokio::test]
async fn query_loop_required_delivery_recovers_a_prepared_repair_without_a_fake_user_message() {
    let store = AgentRuntimeTestStore::new();
    let count = Arc::new(AtomicUsize::new(0));
    let config = AgentRuntimeConfig {
        required_completion_tool: Some("stream_boundary_test_tool".into()),
        ..Default::default()
    };
    let req = request();
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(count.clone()),
        config.clone(),
    );
    let model = DeliveryModel {
        calls: AtomicUsize::new(0),
        repair: true,
    };
    let mut starts = 0;
    let failed = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            req.clone(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &durable_control(&req, "old-claim"),
            &mut |point| {
                if matches!(point, ToolSafePoint::ModelRequestStarted(_)) {
                    starts += 1;
                    if starts == 2 {
                        return Err("injected_repair_request_fence".into());
                    }
                }
                Ok(())
            },
        )
        .await;
    assert!(failed
        .unwrap_err()
        .contains("injected_repair_request_fence"));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    drop(engine);
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(count.clone()),
        config,
    );
    let recovered = DeliveryModel {
        calls: AtomicUsize::new(1),
        repair: true,
    };
    let result = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            req.clone(),
            &recovered,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &durable_control(&req, "new-claim"),
            &mut |_| Ok(()),
        )
        .await;
    assert_eq!(
        result.expect("prepared real repair resumes").stop,
        AgentRunStop::Finalized
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(recovered.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn query_loop_required_delivery_deduplicates_attached_input_after_prepared_restore() {
    let store = AgentRuntimeTestStore::new();
    let count = Arc::new(AtomicUsize::new(0));
    let config = AgentRuntimeConfig {
        required_completion_tool: Some("stream_boundary_test_tool".into()),
        ..Default::default()
    };
    let mut req = request();
    req.initial_input = AgentRunInitialInput::UserInput {
        input_id: "accepted-attachment-input".into(),
        message: "Read this evidence".into(),
        attachments: vec![
            crate::session::turn_input::attachments::UserInputAttachment {
                input_ref: "evidence-file".into(),
                display_name: "evidence.txt".into(),
                content_type: "text/plain".into(),
            },
        ],
    };
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(count.clone()),
        config.clone(),
    );
    let model = SupplementDeliveryModel {
        calls: AtomicUsize::new(0),
        initial_background: false,
    };
    let mut committed = HashSet::new();
    let lost = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            req.clone(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &durable_control(&req, "old-attachment-claim"),
            &mut |point| {
                if let ToolSafePoint::DurableReceipt { call, .. } = point {
                    committed.insert(call.id);
                    return Err("lost_attachment_reply_ack".into());
                }
                Ok(())
            },
        )
        .await;
    assert!(lost.unwrap_err().contains("lost_attachment_reply_ack"));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(engine);
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(count.clone()),
        config,
    );
    let recovered = DeliveryModel {
        calls: AtomicUsize::new(0),
        repair: false,
    };
    let result = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            req.clone(),
            &recovered,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &durable_control(&req, "new-attachment-claim"),
            &mut |point| {
                if let ToolSafePoint::DurableReceipt { call, .. } = point {
                    committed.insert(call.id);
                }
                Ok(())
            },
        )
        .await;
    assert_eq!(
        result
            .expect("same accepted input preserves its committed receipt")
            .stop,
        AgentRunStop::Finalized
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(committed.len(), 1);
    assert_eq!(recovered.calls.load(Ordering::SeqCst), 1);
}

struct TerminalDeliveryModel;

#[tokio::test]
async fn query_loop_required_delivery_survives_canonical_checkpoint_reconstruction() {
    let store = AgentRuntimeTestStore::new();
    let count = Arc::new(AtomicUsize::new(0));
    let config = AgentRuntimeConfig {
        required_completion_tool: Some("stream_boundary_test_tool".into()),
        ..Default::default()
    };
    let mut req = request();
    req.initial_input = AgentRunInitialInput::UserInput {
        input_id: "canonical-delivery-input".into(),
        message: "Read evidence".into(),
        attachments: vec![
            crate::session::turn_input::attachments::UserInputAttachment {
                input_ref: "evidence-file".into(),
                display_name: "evidence.txt".into(),
                content_type: "text/plain".into(),
            },
        ],
    };
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(count.clone()),
        config.clone(),
    );
    let model = SupplementDeliveryModel {
        calls: AtomicUsize::new(0),
        initial_background: false,
    };
    let mut journal = vec![crate::runtime::canonical_session_record("delivery-original-user", crate::session::SessionRecordType::UserMessage,
        &req.session_id, Some(req.initial_turn_id.clone()), Some("delivery-run".into()), 1,
        json!({"messageId": super::super::prompt_projection::driver_user_message_id(&req.session_id, &req.initial_turn_id),
            "text":"Read evidence", "attachments":[{"inputRef":"evidence-file", "displayName":"evidence.txt", "contentType":"text/plain"}]})).unwrap()];
    let lost = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            req.clone(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &durable_control(&req, "canonical-first"),
            &mut |point| {
                match point {
                    ToolSafePoint::ModelRequestStarted(started) => journal.extend(
                        canonical_model_request_started_records("delivery-run", &started, 100)?,
                    ),
                    ToolSafePoint::DurableReceipt { .. } => {
                        return Err("lost_canonical_delivery_ack".into())
                    }
                    _ => {}
                }
                Ok(())
            },
        )
        .await;
    assert!(lost.unwrap_err().contains("lost_canonical_delivery_ack"));
    let main = journal.last().unwrap();
    let observations = main.payload["observations"].as_array().unwrap();
    assert_eq!(
        observations.last().unwrap()["kind"],
        "input_uptake",
        "Read projection keeps uptake last"
    );
    let delivery = observations
        .iter()
        .position(|item| item["kind"] == "required_completion_delivery")
        .unwrap();
    for (field, value) in [
        ("agentRunId", json!("foreign-run")),
        ("turnIds", json!(["foreign-turn"])),
        ("toolName", json!("SendMessage")),
        ("authorizationDigest", json!("sha256:wrong")),
        ("inputKey", json!("sha256:wrong")),
        ("extra", json!(true)),
    ] {
        let mut invalid = main.clone();
        invalid.payload["observations"][delivery]["state"][field] = value;
        assert!(
            crate::session::validate_event_shape(&invalid).is_err(),
            "reject forged delivery {field}"
        );
    }
    let mut duplicate = main.clone();
    duplicate.payload["observations"]
        .as_array_mut()
        .unwrap()
        .insert(delivery, observations[delivery].clone());
    assert!(crate::session::validate_event_shape(&duplicate).is_err());
    let rebuilt =
        crate::session::restore_runtime_snapshot_from_session_records(&req.session_id, &journal)
            .unwrap();
    SessionManager::new(store.clone())
        .save_session(&rebuilt)
        .unwrap();
    drop(engine);
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(count.clone()),
        config,
    );
    let recovered = DeliveryModel {
        calls: AtomicUsize::new(0),
        repair: false,
    };
    let result = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            req.clone(),
            &recovered,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &durable_control(&req, "canonical-recovered"),
            &mut |_| Ok(()),
        )
        .await;
    assert_eq!(
        result
            .expect("canonical policy still owns the real prior receipt")
            .stop,
        AgentRunStop::Finalized
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "reconstruction must not execute a second public reply"
    );
    assert_eq!(recovered.calls.load(Ordering::SeqCst), 1);
}

impl ModelClient for TerminalDeliveryModel {
    fn generate<'a>(
        &'a self,
        _: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: String::new(),
                    tool_calls: vec![ToolCallEnvelope {
                        id: "real-terminal".into(),
                        name: "complete_turn_test_tool".into(),
                        args_json: json!({"value":"done"}).to_string(),
                    }],
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: Some(10),
                    total_tokens: Some(20),
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
#[tokio::test]
async fn query_loop_required_delivery_cannot_be_bypassed_by_a_different_terminal_tool() {
    for matching in [false, true] {
        let layer = complete_turn_test_tool_layer(Arc::new(AtomicUsize::new(0)), true);
        let mut contracts = layer.dynamic_tool_registry().list_dynamic_contracts();
        contracts.extend(
            stream_execution_boundary_tool_layer(Arc::new(AtomicUsize::new(0)))
                .dynamic_tool_registry()
                .list_dynamic_contracts(),
        );
        let mut layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(
            crate::tool::DynamicToolRegistry::from_contracts(contracts).unwrap(),
        ));
        layer
            .register_dynamic_tool_provider(Arc::new(CompleteTurnTestProvider {
                execution_count: Arc::new(AtomicUsize::new(0)),
                succeed: true,
            }))
            .unwrap();
        let engine = AgentRuntime::new_for_test_with_tools(
            AgentRuntimeTestStore::new(),
            layer,
            AgentRuntimeConfig {
                required_completion_tool: Some(
                    if matching {
                        "complete_turn_test_tool"
                    } else {
                        "stream_boundary_test_tool"
                    }
                    .into(),
                ),
                ..Default::default()
            },
        );
        let result = engine.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            request(), &TerminalDeliveryModel, &StaticModelSessionConfigStore { config: Some(ModelSessionConfig::default()) },
            &mut |_| {}, &|| Ok(None), &mut |_| Ok(())).await;
        if matching {
            assert!(result.is_ok());
        } else {
            assert_eq!(
                result.expect_err("unrelated terminal tool cannot deliver the user reply"),
                "completion_tool_delivery_required"
            );
        }
    }
}

struct LargePrivateDraftModel(AtomicUsize);
impl ModelClient for LargePrivateDraftModel {
    fn generate<'a>(
        &'a self,
        _: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: "长".repeat(10000),
                    tool_calls: vec![],
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: Some(10),
                    total_tokens: Some(20),
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
#[tokio::test]
async fn query_loop_required_delivery_bounds_private_draft_and_checks_the_actual_repair_budget() {
    let store = AgentRuntimeTestStore::new();
    let mut config = AgentRuntimeConfig {
        required_completion_tool: Some("stream_boundary_test_tool".into()),
        max_message_chars: 4096,
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let req = request();
    let baseline = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(Arc::new(AtomicUsize::new(0))),
        config.clone(),
    )
    .build_generate_driver_request(&req.session_id, &req.initial_turn_id, "Hello", 0)
    .unwrap();
    config.model_context_tokens =
        baseline.context_token_estimate + config.model_max_output_tokens + 500;
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(Arc::new(AtomicUsize::new(0))),
        config,
    );
    let model = LargePrivateDraftModel(AtomicUsize::new(0));
    let result = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            req.clone(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &mut |_| Ok(()),
        )
        .await;
    assert_eq!(
        result.expect_err("oversized repair cannot reach the provider"),
        "completion_delivery_repair_prompt_too_large"
    );
    assert_eq!(model.0.load(Ordering::SeqCst), 1);
    let session = SessionManager::new(store)
        .load_or_create_session(&req.session_id)
        .unwrap();
    let state: serde_json::Value =
        serde_json::from_str(&session.metadata["required_completion_delivery_v1_json"]).unwrap();
    assert_eq!(state["draft"].as_str().unwrap().chars().count(), 4096);
}

#[tokio::test]
async fn query_loop_requires_real_committed_delivery_before_final() {
    let store = AgentRuntimeTestStore::new();
    let count = Arc::new(AtomicUsize::new(0));
    let runtime = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(count.clone()),
        AgentRuntimeConfig {
            required_completion_tool: Some("stream_boundary_test_tool".into()),
            ..Default::default()
        },
    );
    let model = DeliveryModel {
        calls: AtomicUsize::new(0),
        repair: true,
    };
    let mut committed = HashSet::new();
    let result = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            request(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &mut |point| {
                if let ToolSafePoint::ModelRequestStarted(ref started) = point {
                    canonical_model_request_started_records("delivery-run", started, 100)?;
                }
                if let ToolSafePoint::DurableReceipt { call, result, .. } = point {
                    assert!(result.error.is_none());
                    committed.insert(call.id);
                }
                Ok(())
            },
        )
        .await
        .expect("repair posts real tool then Final");
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(model.calls.load(Ordering::SeqCst), 3);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(committed.len(), 1);
}

#[tokio::test]
async fn query_loop_provider_ignoring_required_delivery_fails_explicitly() {
    let runtime = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        stream_execution_boundary_tool_layer(Arc::new(AtomicUsize::new(0))),
        AgentRuntimeConfig {
            required_completion_tool: Some("stream_boundary_test_tool".into()),
            ..Default::default()
        },
    );
    let model = DeliveryModel {
        calls: AtomicUsize::new(0),
        repair: false,
    };
    let result = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            request(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &mut |_| Ok(()),
        )
        .await;
    assert_eq!(
        result.expect_err("undelivered Final cannot succeed"),
        "completion_tool_delivery_required"
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn query_loop_delivery_recovers_a_lost_commit_ack_without_reexecuting_tool() {
    let store = AgentRuntimeTestStore::new();
    let count = Arc::new(AtomicUsize::new(0));
    let config = AgentRuntimeConfig {
        required_completion_tool: Some("stream_boundary_test_tool".into()),
        ..Default::default()
    };
    let runtime = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(count.clone()),
        config.clone(),
    );
    let model = DeliveryModel {
        calls: AtomicUsize::new(0),
        repair: true,
    };
    let mut committed = HashSet::new();
    let lost = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            request(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &mut |point| {
                if let ToolSafePoint::DurableReceipt { call, .. } = point {
                    committed.insert(call.id);
                    return Err("lost_commit_ack".into());
                }
                Ok(())
            },
        )
        .await;
    assert!(lost.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(runtime);
    let runtime = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(count.clone()),
        config,
    );
    let model = DeliveryModel {
        calls: AtomicUsize::new(0),
        repair: false,
    };
    let result = runtime
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            request(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &mut |point| {
                if let ToolSafePoint::DurableReceipt { call, .. } = point {
                    committed.insert(call.id);
                }
                Ok(())
            },
        )
        .await
        .expect("durable real receipt recovers delivery");
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(committed.len(), 1);
}

#[tokio::test]
async fn query_loop_internal_worker_and_background_wakeup_can_finish_silently() {
    for child in [true, false] {
        let runtime = AgentRuntime::new_for_test_with_tools(
            AgentRuntimeTestStore::new(),
            stream_execution_boundary_tool_layer(Arc::new(AtomicUsize::new(0))),
            AgentRuntimeConfig {
                required_completion_tool: Some("stream_boundary_test_tool".into()),
                ..Default::default()
            },
        );
        let mut req = request();
        if child {
            req.runtime_scope.agent_scope = "subagent".into();
        } else {
            req.initial_input = AgentRunInitialInput::HostEvent(
                crate::session::host_event_input::HostEventInput::new(
                    &req.session_id,
                    "background-input".into(),
                    "opaque://background".into(),
                    "Background update".into(),
                )
                .unwrap(),
            );
        }
        let model = DeliveryModel {
            calls: AtomicUsize::new(0),
            repair: false,
        };
        let result = runtime.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            req, &model, &StaticModelSessionConfigStore { config: Some(ModelSessionConfig::default()) },
            &mut |_| {}, &|| Ok(None), &mut |_| Ok(())).await.unwrap();
        assert_eq!(result.stop, AgentRunStop::Finalized);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    }
}

struct SupplementDeliveryModel {
    calls: AtomicUsize,
    initial_background: bool,
}
impl ModelClient for SupplementDeliveryModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            if index == 2 {
                assert_eq!(
                    request.prepared_prompt.tool_choice,
                    ModelToolChoice::Specific {
                        name: "stream_boundary_test_tool".into()
                    }
                );
            }
            let tool_round = index.is_multiple_of(2) && !(index == 0 && self.initial_background);
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if !tool_round {
                        "Private Final".into()
                    } else {
                        String::new()
                    },
                    tool_calls: if tool_round {
                        vec![ToolCallEnvelope {
                            id: format!("reply-{index}"),
                            name: "stream_boundary_test_tool".into(),
                            args_json: json!({"value":format!("answer {index}")}).to_string(),
                        }]
                    } else {
                        vec![]
                    },
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: Some(10),
                    total_tokens: Some(20),
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

#[tokio::test]
async fn query_loop_later_user_input_requires_a_new_delivery_after_the_first_reply() {
    let count = Arc::new(AtomicUsize::new(0));
    let runtime = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        stream_execution_boundary_tool_layer(count.clone()),
        AgentRuntimeConfig {
            required_completion_tool: Some("stream_boundary_test_tool".into()),
            ..Default::default()
        },
    );
    let control = TurnControl::new();
    let model = SupplementDeliveryModel {
        calls: AtomicUsize::new(0),
        initial_background: false,
    };
    let mut added = false;
    let result = runtime
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            request(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &control,
            &mut |point| {
                if let ToolSafePoint::DurableReceipt { call, .. } = point {
                    if call.id == "reply-0" && !added {
                        control.enqueue_supplement_with("New user question".into(), || Ok(()))?;
                        added = true;
                    }
                }
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(model.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn query_loop_user_input_arriving_during_background_wakeup_requires_delivery() {
    let count = Arc::new(AtomicUsize::new(0));
    let runtime = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        stream_execution_boundary_tool_layer(count.clone()),
        AgentRuntimeConfig {
            required_completion_tool: Some("stream_boundary_test_tool".into()),
            ..Default::default()
        },
    );
    let mut req = request();
    req.initial_input = AgentRunInitialInput::HostEvent(
        crate::session::host_event_input::HostEventInput::new(
            &req.session_id,
            "background-input".into(),
            "opaque://background".into(),
            "Background update".into(),
        )
        .unwrap(),
    );
    let control = TurnControl::new();
    let model = SupplementDeliveryModel {
        calls: AtomicUsize::new(0),
        initial_background: true,
    };
    let mut added = false;
    let result = runtime
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            req,
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &control,
            &mut |point| {
                if matches!(point, ToolSafePoint::ModelRequestStarted(_)) && !added {
                    control.enqueue_supplement_with("New real user question".into(), || Ok(()))?;
                    added = true;
                }
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 4);
}
