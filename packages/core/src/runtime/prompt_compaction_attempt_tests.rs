use super::*;

#[tokio::test]
async fn query_loop_failed_async_compaction_is_counted_once_and_next_turn_retries() {
    let store = AgentRuntimeTestStore::new();
    let manager = SessionManager::new(store.clone());
    let session_id = "chat-compaction-single-attempt";
    manager
        .save_session(&large_context_session(session_id))
        .unwrap();
    let engine = AgentRuntime::new_for_test(store.clone(), large_context_compaction_config());
    let client = OnlinePromptCompactionModelClient::new(PromptCompactionModelBehavior::Empty);
    let config = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let driver = ModelClientGenerateDriver::new(&client, &config);

    for (index, turn_id) in ["turn-first-attempt", "turn-next-attempt"]
        .iter()
        .enumerate()
    {
        let mut input =
            TurnInput::UserMessage("Continue without replacing the real failure.".into());
        let request = engine
            .build_generate_driver_request_with_async_driver_and_runtime_scope(
                session_id,
                turn_id,
                &mut input,
                index as u32,
                PromptCompactionScopeV1::main(),
                None,
                None,
                &driver,
                None,
            )
            .await
            .unwrap();
        let before = manager.load_or_create_session(session_id).unwrap();
        let failure_before = before.metadata[PROMPT_COMPACTION_FAILURE_META_KEY].clone();
        let failure: Value = serde_json::from_str(&failure_before).unwrap();
        assert_eq!(failure["turnId"], *turn_id);
        assert_eq!(failure["failureCount"], 1);
        assert!(!before.metadata.contains_key("prompt_compaction_stats_json"));

        let generated = driver.generate_next_async(&request).await.unwrap();
        let step = engine
            .process_turn_with_stream_sink_async(
                ProcessTurnRequest {
                    session_id: session_id.into(),
                    turn_id: (*turn_id).into(),
                    input,
                    agent_run_identity: None,
                    generate_result: generated.generate_result,
                    agent_run_resource_usage: Default::default(),
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(step.continuation, QueryContinuation::Finalize);
        assert_eq!(
            step.session_snapshot.metadata[PROMPT_COMPACTION_FAILURE_META_KEY], failure_before,
            "processing the main response must preserve the original async failure exactly"
        );
        assert_eq!(
            step.session_snapshot.metadata[PROMPT_COMPACTION_FAILURE_COUNT_META_KEY],
            "1"
        );
        assert!(!prompt_compaction_circuit_is_open(&step.session_snapshot));
    }
    let requests = client.requests();
    assert_eq!(
        requests.len(),
        4,
        "both turns must issue one real compaction request and one main request"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.prepared_prompt.tool_choice == ModelToolChoice::None)
            .count(),
        2
    );
    let events = store.list_events(session_id, 100, 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "prompt.compaction.failed")
            .count(),
        2
    );
}

#[tokio::test]
async fn query_loop_stale_or_damaged_compaction_failure_cannot_suppress_a_new_attempt() {
    let turn_id = "turn-needs-attempt";
    for (index, raw) in [
        "not json".to_string(),
        json!({"schema":"unsupported_failure_v1","turnId":turn_id}).to_string(),
        json!({"schema":"prompt_compaction_failure_v1","turnId":turn_id}).to_string(),
        json!({"schema":"prompt_compaction_failure_v1","turnId":"previous-turn",
            "phase":"summary_provider","reason":"prior provider error", "failureCount":1,
            "circuitOpen":false,"recordedAtMs":1})
        .to_string(),
    ]
    .into_iter()
    .enumerate()
    {
        let store = AgentRuntimeTestStore::new();
        let manager = SessionManager::new(store.clone());
        let session_id = format!("chat-compaction-unrecognized-{index}");
        let mut session = large_context_session(&session_id);
        session
            .metadata
            .insert(PROMPT_COMPACTION_FAILURE_META_KEY.into(), raw);
        session
            .metadata
            .insert(PROMPT_COMPACTION_FAILURE_COUNT_META_KEY.into(), "1".into());
        manager.save_session(&session).unwrap();
        let engine = AgentRuntime::new_for_test(store, large_context_compaction_config());
        let step = engine
            .process_turn_with_stream_sink_async(
                ProcessTurnRequest {
                    session_id,
                    turn_id: turn_id.into(),
                    input: TurnInput::UserMessage("continue".into()),
                    agent_run_identity: None,
                    generate_result: GenerateResult {
                        content: "main response".into(),
                        tool_calls: vec![],
                        continuation_reasoning_content: None,
                        reasoning_content: None,
                        input_tokens: None,
                        total_tokens: None,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    },
                    agent_run_resource_usage: Default::default(),
                },
                None,
            )
            .await
            .unwrap();
        let failure: Value = serde_json::from_str(
            &step.session_snapshot.metadata[PROMPT_COMPACTION_FAILURE_META_KEY],
        )
        .unwrap();
        assert_eq!(failure["schema"], "prompt_compaction_failure_v1");
        assert_eq!(failure["turnId"], turn_id);
        assert_eq!(failure["failureCount"], 2);
        assert!(failure["reason"]
            .as_str()
            .unwrap()
            .contains("model-capable generate driver"));
    }
}
