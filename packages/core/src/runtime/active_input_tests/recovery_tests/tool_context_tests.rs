use super::*;

struct ToolThenFailModel {
    requests: Mutex<Vec<ModelClientRequest>>,
}

impl ModelClient for ToolThenFailModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let mut requests = self.requests.lock().unwrap();
            requests.push(request.clone());
            if requests.len() > 1 {
                return Err(ModelClientError::new(
                    ModelClientErrorKind::Provider,
                    "injected_tool_continuation_provider_failure",
                    false,
                ));
            }
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: "execute the accepted tool batch".into(),
                    tool_calls: vec![crate::model::ToolCallEnvelope {
                        id: "accepted-tool".into(),
                        name: "stream_boundary_test_tool".into(),
                        args_json: "{\"value\":\"accepted\"}".into(),
                    }],
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
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
async fn query_loop_current_tool_context_with_empty_uptake_recovers_without_reexecution() {
    let queue = Arc::new(InputQueue::default());
    queue.add(
        TurnInputPayload::UserSupplement {
            supplement_id: "initial-user".into(),
            message: "  accepted objective\n".into(),

            attachments: Vec::new(),
        },
        1,
    );
    let journal = Arc::new(RecoveryJournal::default());
    journal.records.lock().unwrap().extend(
        crate::session::started_agent_run_records(
            "active-input",
            "input-turn",
            "input-run",
            "  accepted objective\n",
            1,
        )
        .unwrap(),
    );
    let store = AgentRuntimeTestStore::new();
    SessionManager::new(store.clone())
        .save_session(
            &crate::session::restore_runtime_snapshot_from_session_records(
                "active-input",
                &journal.records.lock().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(executions.clone()),
        AgentRuntimeConfig::default(),
    );
    let captured = journal.clone();
    let control = TurnControl::new_durable_inputs(
        queue.clone(),
        input_binding("before-tool-failure"),
        Arc::new(move |turn, inputs| captured.materialize(turn, inputs, Some("initial-user"))),
    )
    .unwrap();
    let mut request = input_run("active-input");
    request.agent_run_identity = Some(input_identity());
    request.initial_input = AgentRunInitialInput::UserInput {
        input_id: "initial-user".into(),
        message: "  accepted objective\n".into(),

        attachments: Vec::new(),
    };
    let model = ToolThenFailModel {
        requests: Mutex::new(vec![]),
    };
    let error = engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            request.clone(),
            &model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &control,
            &mut |point| {
                let mut records = journal.records.lock().unwrap();
                match point {
                    ToolSafePoint::ModelRequestStarted(started) => records.extend(
                        canonical_model_request_started_records("input-run", &started, 48)?,
                    ),
                    ToolSafePoint::DurableToolCall {
                        session_id,
                        turn_id,
                        agent_run_id,
                        call,
                        provider_id,
                        tool_contract_digest,
                        recorded_at_ms,
                    } => records.push(canonical_tool_call_record(
                        &session_id,
                        &turn_id,
                        &agent_run_id,
                        &call,
                        &provider_id,
                        &tool_contract_digest,
                        &call.name,
                        recorded_at_ms,
                    )?),
                    ToolSafePoint::DurableReceipt {
                        session_id,
                        turn_id,
                        agent_run_id,
                        call,
                        result,
                    } => records.push(canonical_tool_result_record(
                        &session_id,
                        &turn_id,
                        &agent_run_id,
                        &call,
                        &result,
                        result.completed_at_ms,
                    )?),
                    _ => {}
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("injected_tool_continuation_provider_failure"));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let facts = journal.read_facts();
    assert_eq!(facts.len(), 2);
    assert_eq!(input_ids(&facts[0]), ["initial-user"]);
    assert!(input_ids(&facts[1]).is_empty());
    let current_context = compacted_context_tests::observed_context(&facts[1]);
    assert_eq!(
        current_context.last().unwrap().role,
        crate::model::prepared_prompt::ModelMessageRoleV1::Tool
    );
    let records = journal.records.lock().unwrap().clone();
    let rebuilt =
        crate::session::restore_runtime_snapshot_from_session_records("active-input", &records)
            .unwrap();
    let recovered_store = AgentRuntimeTestStore::new();
    SessionManager::new(recovered_store.clone())
        .save_session(&rebuilt)
        .unwrap();
    queue.add(
        TurnInputPayload::UserSupplement {
            supplement_id: "after-tool-failure".into(),
            message: "  deferred update\n".into(),

            attachments: Vec::new(),
        },
        2,
    );
    let captured = journal.clone();
    let recovered_control = TurnControl::new_durable_inputs(
        queue.clone(),
        input_binding("after-tool-failure"),
        Arc::new(move |turn, inputs| captured.materialize(turn, inputs, Some("initial-user"))),
    )
    .unwrap();
    let recovered_model = IntakeModelClient {
        requests: Mutex::new(vec![]),
        control: None,
    };
    let recovered_engine = AgentRuntime::new_for_test_with_tools(
        recovered_store.clone(),
        stream_execution_boundary_tool_layer(executions.clone()),
        AgentRuntimeConfig::default(),
    );
    let result = recovered_engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            request,
            &recovered_model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &recovered_control,
            &mut |point| {
                if let ToolSafePoint::ModelRequestStarted(started) = point {
                    journal.commit_main(&started, &recovered_store)?;
                }
                Ok(())
            },
        )
        .await
        .unwrap();
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "recovery keeps the completed tool batch and does not execute it again"
    );
    let requests = recovered_model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].turn_id,
        facts[1].turn_id.as_ref().unwrap().as_str()
    );
    assert_eq!(requests[0].prepared_prompt.messages, current_context);
    assert!(requests[1]
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content == "  deferred update\n"));
    let facts = journal.read_facts();
    assert_eq!(
        facts
            .iter()
            .flat_map(input_ids)
            .filter(|id| id == "initial-user")
            .count(),
        1
    );
    assert_eq!(input_ids(facts.last().unwrap()), ["after-tool-failure"]);
}
