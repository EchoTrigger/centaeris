use super::*;

#[tokio::test]
async fn query_loop_record_recovery_reuses_execution_context_without_duplicate_ids() {
    assert_runtime_context_recovery(false).await;
}

#[tokio::test]
async fn query_loop_record_recovery_rejects_conflicting_execution_context_identity() {
    assert_runtime_context_recovery(true).await;
}

async fn assert_runtime_context_recovery(conflicting_context: bool) {
    let queue = Arc::new(InputQueue::default());
    for (id, message, sequence) in [
        ("initial-user", "  initial body\n", 1),
        ("user-update", "  retained update\n", 2),
    ] {
        queue.add(
            TurnInputPayload::UserSupplement {
                supplement_id: id.into(),
                message: message.into(),

                attachments: Vec::new(),
            },
            sequence,
        );
    }
    let failing_queue = Arc::new(AckOnceFailQueue {
        queue: queue.clone(),
        fail_next_ack: AtomicBool::new(true),
        ack_attempts: Mutex::new(vec![]),
    });
    let journal = Arc::new(RecoveryJournal::default());
    journal.records.lock().unwrap().extend(
        crate::session::started_agent_run_records(
            "active-input",
            "input-turn",
            "input-run",
            "  initial body\n",
            1,
        )
        .unwrap(),
    );
    let store = AgentRuntimeTestStore::new();
    let workspace_root = temp_dir_path("record_recovery_execution_context");
    std::fs::create_dir_all(&workspace_root).unwrap();
    let model = IntakeModelClient {
        requests: Mutex::new(vec![]),
        control: None,
    };
    let execution_context_id = "msg:active-input:input-turn:execution_context";
    let mut original_context = None;
    for (token, first_attempt) in [("first-claim", true), ("recovered-claim", false)] {
        let records = journal.records.lock().unwrap().clone();
        let mut restored =
            crate::session::restore_runtime_snapshot_from_session_records("active-input", &records)
                .unwrap();
        if conflicting_context && !first_attempt {
            for message in &mut restored.messages {
                if message.message_id == execution_context_id {
                    message.content = "conflicting execution context".into();
                }
            }
            restored.context_window = restored.messages.clone();
        }
        SessionManager::new(store.clone())
            .save_session(&restored)
            .unwrap();
        let captured = journal.clone();
        let control = TurnControl::new_durable_inputs(
            failing_queue.clone(),
            input_binding(token),
            Arc::new(move |turn, inputs| captured.materialize(turn, inputs, Some("initial-user"))),
        )
        .unwrap();
        let tools = ToolLayer::new().with_cwd(workspace_root.clone()).unwrap();
        let engine = AgentRuntime::new(
            store.clone(),
            tools,
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                agent_instructions: "Keep the accepted input context.".into(),
                ..Default::default()
            },
            ToolConcurrencyCoordinator::new(1),
        );
        let mut request = input_run("active-input");
        request.agent_run_identity = Some(input_identity());
        request.initial_input = AgentRunInitialInput::UserInput {
            input_id: "initial-user".into(),
            message: "  initial body\n".into(),

            attachments: Vec::new(),
        };
        let result = engine
            .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                request,
                &model,
                &StaticModelSessionConfigStore {
                    config: Some(ModelSessionConfig::default()),
                },
                &mut |_| {},
                &|| Ok(None),
                &control,
                &mut |point| {
                    if let ToolSafePoint::ModelRequestStarted(started) = point {
                        journal.commit_main(&started, &store)?;
                    }
                    Ok(())
                },
            )
            .await;
        if first_attempt {
            assert!(result.unwrap_err().contains("injected_input_ack_failure"));
            let facts = journal.read_facts();
            assert_eq!(facts.len(), 1);
            assert_eq!(input_ids(&facts[0]), ["initial-user", "user-update"]);
            original_context = Some(compacted_context_tests::observed_context(&facts[0]));
            assert!(original_context
                .as_ref()
                .unwrap()
                .iter()
                .any(|message| { message.message_id == execution_context_id }));
            assert!(model.requests.lock().unwrap().is_empty());
        } else if conflicting_context {
            assert!(result.unwrap_err().contains(&format!(
                "prepared_prompt_message_id_duplicate:{execution_context_id}"
            )));
            assert!(model.requests.lock().unwrap().is_empty());
            assert_eq!(journal.read_facts().len(), 1);
        } else {
            assert_eq!(result.unwrap().stop, AgentRunStop::Finalized);
            let requests = model.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                &requests[0].prepared_prompt.messages,
                original_context.as_ref().unwrap()
            );
            assert_eq!(
                requests[0]
                    .prepared_prompt
                    .messages
                    .iter()
                    .filter(|message| { message.message_id == execution_context_id })
                    .count(),
                1
            );
            assert_eq!(
                requests[0]
                    .prepared_prompt
                    .messages
                    .iter()
                    .map(|message| { &message.message_id })
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                requests[0].prepared_prompt.messages.len()
            );
            for id in ["initial-user", "user-update"] {
                assert_eq!(
                    journal
                        .read_facts()
                        .iter()
                        .flat_map(input_ids)
                        .filter(|read| read == id)
                        .count(),
                    1
                );
            }
        }
    }
}
