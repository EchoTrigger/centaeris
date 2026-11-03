use super::*;

pub(super) fn question_generate_result() -> GenerateResult {
    GenerateResult {
        content: "Choose how to continue.".into(),
        tool_calls: vec![ToolCallEnvelope {
            id: "question-call".into(),
            name: question::TOOL_NAME.into(),
            args_json: json!({"question":"Which approach?","options":["First","Second"]})
                .to_string(),
        }],
        continuation_reasoning_content: Some("provider-continuation".into()),
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    }
}

fn config() -> AgentRuntimeConfig {
    AgentRuntimeConfig {
        question_input: question::QuestionInputCapability::PersistentResponses,
        ..AgentRuntimeConfig::default()
    }
}

fn identity() -> RuntimeAgentRunIdentityV1 {
    RuntimeAgentRunIdentityV1 {
        agent_run_id: "question-run".into(),
        execution_id: "question-execution".into(),
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
    }
}

struct QuestionResumeModelClient(AtomicUsize);

impl ModelClient for QuestionResumeModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                request
                    .prepared_prompt
                    .messages
                    .iter()
                    .filter(|message| message.role
                        == crate::model::prepared_prompt::ModelMessageRoleV1::Tool)
                    .count(),
                1
            );
            let mut result = question_generate_result();
            result.tool_calls.clear();
            result.content = "Continued from the original answer".into();
            Ok(ModelClientResponse {
                generate_result: result,
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
                request_timings: None,
            })
        })
    }
}

#[tokio::test]
async fn question_tool_requires_persistent_host_and_is_not_delegated() {
    let store = AgentRuntimeTestStore::new();
    let session = SessionStateSnapshot::new("question-session".into(), 0);
    let unsupported = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
    assert!(!unsupported
        .build_generate_tool_projection(&session)
        .unwrap()
        .0
        .iter()
        .any(|tool| tool.name == question::TOOL_NAME));
    assert!(unsupported
        .validate_tool_turn_behavior_preflight(&session, &question_generate_result())
        .is_err());
    let supported = AgentRuntime::new_for_test(store.clone(), config());
    assert!(supported
        .build_generate_tool_projection(&session)
        .unwrap()
        .0
        .iter()
        .any(|tool| tool.name == question::TOOL_NAME));
    let child = AgentRuntime::new_for_test(
        store,
        AgentRuntimeConfig {
            allowed_tools: Some(vec!["read".into()]),
            ..config()
        },
    );
    assert!(!child
        .build_generate_tool_projection(&session)
        .unwrap()
        .0
        .iter()
        .any(|tool| tool.name == question::TOOL_NAME));
}

#[tokio::test]
async fn model_question_resumes_original_run_and_closes_one_provider_tool_result() {
    for action in ["answer", "skip"] {
        let store = AgentRuntimeTestStore::new();
        let engine = AgentRuntime::new_for_test(store.clone(), config());
        let mut original = question_generate_result();
        original.tool_calls[0].args_json =
            "{ \"options\": [\"First\", \"Second\"], \"question\": \"Which approach?\" }".into();
        let paused = engine
            .process_turn_with_stream_sink_async(
                ProcessTurnRequest {
                    session_id: "question-session".into(),
                    turn_id: "question-turn".into(),
                    input: TurnInput::UserMessage("Please ask".into()),
                    agent_run_identity: Some(identity()),
                    generate_result: original.clone(),
                    agent_run_resource_usage: AgentRunResourceUsageV1::default(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(paused.continuation, QueryContinuation::AwaitQuestion);
        let sealed = paused.session_snapshot.clone();
        let checkpoint = paused.checkpoint.unwrap();
        let wait: question::QuestionCheckpoint =
            serde_json::from_str(&checkpoint.payload_json).unwrap();
        wait.validate().unwrap();
        assert_eq!(wait.question.options, ["First", "Second"]);
        assert_eq!(wait.tool_call.id, original.tool_calls[0].id);
        assert_eq!(wait.tool_call.name, original.tool_calls[0].name);
        assert_eq!(wait.tool_call.args_json, original.tool_calls[0].args_json);
        let event = paused
            .runtime_events
            .iter()
            .find(|event| event.event_type == "QuestionRequired")
            .unwrap();
        assert_eq!(event.payload, serde_json::to_value(&wait.question).unwrap());
        // A reconstructed engine with no response must stay paused without another provider call.
        let restored = AgentRuntime::new_for_test(store.clone(), config());
        assert_eq!(
            restored
                .resume_turn_with_agent_run_identity_async(
                    "question-session",
                    "question-turn",
                    Some(&identity())
                )
                .await
                .unwrap()
                .continuation,
            QueryContinuation::AwaitQuestion
        );
        let mut foreign = identity();
        foreign.agent_run_id = "new-run".into();
        assert!(restored
            .resume_turn_with_agent_run_identity_async(
                "question-session",
                "question-turn",
                Some(&foreign)
            )
            .await
            .is_err());
        let response = question::QuestionResponse {
            schema: question::RESPONSE_SCHEMA.into(),
            agent_run_id: wait.agent_run_id.clone(),
            authorization_digest: wait.authorization_digest.clone(),
            continuation_id: wait.continuation_id,
            question_id: wait.question_id,
            operation_id: "response:one".into(),
            action: action.into(),
            answer: (action == "answer").then(|| "2 with extra detail".into()),
        };
        store
            .append_event(RuntimeEvent {
                event_id: response.event_id(),
                session_id: "question-session".into(),
                task_id: Some("question-turn".into()),
                event_type: question::RESPONSE_SCHEMA.into(),
                at_ms: now_ms(),
                visibility: EventVisibility::Internal,
                payload_json: serde_json::to_string(&response).unwrap(),
            })
            .unwrap();
        let resumed = restored
            .resume_turn_with_agent_run_identity_async(
                "question-session",
                "question-turn",
                Some(&identity()),
            )
            .await
            .unwrap();
        assert_eq!(resumed.continuation, QueryContinuation::ExecuteTools);
        assert_eq!(resumed.tool_results.len(), 1);
        assert_eq!(resumed.tool_results[0].tool_call_id, "question-call");
        assert_eq!(resumed.tool_results[0].details["action"], action);
        let session = SessionManager::new(store.clone())
            .load_session("question-session")
            .unwrap()
            .unwrap();
        assert_eq!(session.model_semantics.values().filter(|semantic| matches!(semantic,ModelMessageSemanticsV1::ToolResult{tool_call_id,..} if tool_call_id=="question-call")).count(),1);
        assert!(store
            .load_checkpoint_by_turn("question-session", "question-turn")
            .unwrap()
            .is_none());
        // The host may restore the sealed wait snapshot after consume committed but
        // before the next model checkpoint. Replay the durable closure, never re-ask.
        SessionManager::new(store.clone())
            .save_session(&sealed)
            .unwrap();
        let restarted = AgentRuntime::new_for_test(store.clone(), config());
        let model = QuestionResumeModelClient(AtomicUsize::new(0));
        let completed = restarted
            .process_turn_loop_online_with_model_client_async(
                AgentRunRequest {
                    session_id: "question-session".into(),
                    agent_run_identity: Some(identity()),
                    initial_turn_id: "question-turn".into(),
                    initial_input: AgentRunInitialInput::UserMessage("Please ask".into()),
                    runtime_scope: PromptCompactionScopeV1::main(),
                    resume_from_turn_id: None,
                    auto_continue_after_resume_wait: None,
                },
                &model,
                &StaticModelSessionConfigStore {
                    config: Some(ModelSessionConfig::default()),
                },
            )
            .await
            .unwrap();
        assert_eq!(completed.stop, AgentRunStop::Finalized);
        assert_eq!(model.0.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn question_wait_rejects_invalid_input_and_does_not_publish_after_write_failure() {
    for (case, mut generate, enabled) in [
        ("denied", question_generate_result(), false),
        ("invalid", question_generate_result(), true),
        ("write_failure", question_generate_result(), true),
    ] {
        let store = AgentRuntimeTestStore::new();
        if case == "invalid" {
            generate.tool_calls[0].args_json = r#"{"question":"","options":["One","Two"]}"#.into();
        }
        if case == "write_failure" {
            let wait =
                question::QuestionCheckpoint::new(&identity(), "question-turn", &generate).unwrap();
            store
                .append_event(RuntimeEvent {
                    event_id: format!("runtime_wait:{}:waiting", wait.continuation_id),
                    session_id: "question-session".into(),
                    task_id: Some("question-turn".into()),
                    event_type: "conflicting_event".into(),
                    at_ms: 0,
                    visibility: EventVisibility::Internal,
                    payload_json: "{}".into(),
                })
                .unwrap();
        }
        let engine = AgentRuntime::new_for_test(
            store.clone(),
            if enabled {
                config()
            } else {
                AgentRuntimeConfig::default()
            },
        );
        let mut updates = Vec::new();
        let result = engine
            .process_turn_with_stream_sink_async(
                ProcessTurnRequest {
                    session_id: "question-session".into(),
                    turn_id: "question-turn".into(),
                    input: TurnInput::UserMessage("Please ask".into()),
                    agent_run_identity: Some(identity()),
                    generate_result: generate,
                    agent_run_resource_usage: AgentRunResourceUsageV1::default(),
                },
                Some(&mut |update| updates.push(update)),
            )
            .await;
        let error = result.unwrap_err();
        match case {
            "denied" => assert!(
                error.contains("not permitted by the projected tool policy"),
                "{error}"
            ),
            "invalid" => assert_eq!(error, "question_requires_text_and_two_to_six_options"),
            _ => assert_eq!(error, "runtime event idempotency conflict"),
        }
        assert!(
            store
                .load_checkpoint_by_turn("question-session", "question-turn")
                .unwrap()
                .is_none(),
            "{case}"
        );
        assert!(!updates.iter().any(|update| matches!(update, TurnUpdate::RuntimeEvent { event } if event.event_type == "QuestionRequired")), "{case}");
    }
}

#[tokio::test]
async fn public_question_routing_retains_serialized_snapshot_and_checkpoint_contract() {
    let store = AgentRuntimeTestStore::new();
    let checkpoints = TurnCheckpointStore::new(store);
    let result = question_generate_result();
    let routed = checkpoints
        .route_after_generate_async(RouteGenerateResultRequest {
            session_id: "question-session".into(),
            turn_id: "question-turn".into(),
            state: build_agent_state(result.clone()),
        })
        .await
        .unwrap();
    assert_eq!(routed.continuation, QueryContinuation::AwaitQuestion);
    let expected = question::question_from_result(&result).unwrap().unwrap();
    let snapshot = serde_json::to_value(&routed.state).unwrap();
    let serialized_question = snapshot["pending_question_json"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<question::Question>(serialized_question).unwrap(),
        expected
    );
    let persisted = checkpoints
        .persist_query_state_async(PersistQueryStateRequest {
            session_id: "question-session".into(),
            turn_id: "question-turn".into(),
            at_ms: 1,
            state: routed.state,
            agent_run_identity: Some(identity()),
        })
        .await
        .unwrap();
    let wait: question::QuestionCheckpoint =
        serde_json::from_str(&persisted.checkpoint.payload_json).unwrap();
    wait.validate().unwrap();
    assert_eq!(wait.question, expected);
    assert_eq!(wait.tool_call.args_json, result.tool_calls[0].args_json);
}

#[tokio::test]
async fn question_wait_recovery_rejects_tampered_tool_call_binding() {
    for field in ["id", "name", "args"] {
        let store = AgentRuntimeTestStore::new();
        let engine = AgentRuntime::new_for_test(store.clone(), config());
        let paused = engine
            .process_turn_with_stream_sink_async(
                ProcessTurnRequest {
                    session_id: "question-session".into(),
                    turn_id: "question-turn".into(),
                    input: TurnInput::UserMessage("Please ask".into()),
                    agent_run_identity: Some(identity()),
                    generate_result: question_generate_result(),
                    agent_run_resource_usage: AgentRunResourceUsageV1::default(),
                },
                None,
            )
            .await
            .unwrap();
        let mut wait: question::QuestionCheckpoint =
            serde_json::from_str(&paused.checkpoint.unwrap().payload_json).unwrap();
        match field {
            "id" => wait.tool_call.id = "other".into(),
            "name" => wait.tool_call.name = "other".into(),
            _ => {
                wait.tool_call.args_json =
                    r#"{"question":"Other?","options":["First","Second"]}"#.into()
            }
        }
        store.state().unwrap().checkpoints[0].payload_json = serde_json::to_string(&wait).unwrap();
        let error = engine
            .resume_turn_with_agent_run_identity_async(
                "question-session",
                "question-turn",
                Some(&identity()),
            )
            .await
            .unwrap_err();
        assert_eq!(error, "question_tool_call_binding_mismatch", "{field}");
    }
}

#[test]
fn question_response_validates_operation_ids_full_payload_and_old_checkpoint_rejection() {
    let checkpoint =
        question::QuestionCheckpoint::new(&identity(), "turn", &question_generate_result())
            .unwrap();
    let mut response = question::QuestionResponse {
        schema: question::RESPONSE_SCHEMA.into(),
        agent_run_id: checkpoint.agent_run_id.clone(),
        authorization_digest: checkpoint.authorization_digest.clone(),
        continuation_id: checkpoint.continuation_id.clone(),
        question_id: checkpoint.question_id.clone(),
        operation_id: format!("{}:", "x".repeat(127)),
        action: "answer".into(),
        answer: Some("x".repeat(65_536)),
    };
    response.validate_binding(&checkpoint).unwrap();
    response.operation_id.push('x');
    assert!(response.validate().is_err());
    response.operation_id = "valid".into();
    response.answer.as_mut().unwrap().push('x');
    assert!(response.validate().is_err());
    let old =
        contracts::RuntimeAwaitQuestionCheckpointV1::new(&identity(), "turn", "question").unwrap();
    assert!(serde_json::from_value::<question::QuestionCheckpoint>(
        serde_json::to_value(old).unwrap()
    )
    .is_err());
    let mut mixed = question_generate_result();
    mixed.tool_calls.push(ToolCallEnvelope {
        id: "read".into(),
        name: "read".into(),
        args_json: "{}".into(),
    });
    assert!(question::question_from_result(&mixed).is_err());
}
