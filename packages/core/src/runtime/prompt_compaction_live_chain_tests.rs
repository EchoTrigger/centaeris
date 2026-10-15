use super::*;
use crate::runtime::context_window::validate_context_window_materialization;

fn chain_config() -> AgentRuntimeConfig {
    AgentRuntimeConfig {
        model_context_tokens: 8000,
        model_max_output_tokens: 1024,
        prompt_compaction_summary_max_tokens: 1024,
        prompt_compaction_trigger_headroom_tokens: u32::MAX,
        ..Default::default()
    }
}

async fn compacted_chain() -> SessionStateSnapshot {
    let store = AgentRuntimeTestStore::new();
    let manager = SessionManager::new(store.clone());
    let session_id = "chat-chain-reference";
    manager
        .save_session(&continuous_tool_chain(
            session_id,
            MESSAGE_SEMANTIC_USER_REQUEST,
        ))
        .unwrap();
    let engine = AgentRuntime::new_for_test(store, chain_config());
    engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "turn-chain-reference",
            &mut TurnInput::ToolContinuation {
                objective: "continue".into(),
            },
            1,
            PromptCompactionScopeV1::main(),
            None,
            None,
            &TestPromptCompactionAsyncDriver,
            None,
        )
        .await
        .unwrap();
    manager.load_or_create_session(session_id).unwrap()
}

fn continuous_tool_chain(session_id: &str, semantic_kind: &str) -> SessionStateSnapshot {
    let mut session = SessionStateSnapshot::new(session_id.into(), 1);
    let handler = MessageHandler::new(MessageHandlerConfig::default());
    let mut metadata = JsonMap::new();
    metadata.insert(MESSAGE_SEMANTIC_KIND_META_KEY.into(), semantic_kind.into());
    metadata.insert("origin_fixture".into(), "original accepted input".into());
    if semantic_kind == crate::session::host_event_input::HOST_EVENT_SEMANTIC_KIND {
        let input = crate::session::host_event_input::HostEventInput::new(
            session_id,
            "accepted-notice".into(),
            "owned-fixture".into(),
            "Continue the approved report.".into(),
        )
        .unwrap();
        session
            .messages
            .push(input.chat_message(session_id, 1).unwrap());
        session
            .model_semantics
            .insert(input.message_id, ModelMessageSemanticsV1::Plain);
    } else {
        handler.push_user_message(
            &mut session,
            "  Continue the original objective.\n",
            metadata,
        );
    }
    for call in ["closed-older-a", "closed-older-b"] {
        append_prompt_test_tool_group(&handler, &mut session, &[call]);
        session.messages.last_mut().unwrap().content =
            format!("confirmed result {call} ").repeat(200);
    }
    append_prompt_test_tool_group(&handler, &mut session, &["latest-a", "latest-b"]);
    handler.refresh_context_window(&mut session);
    session
}

#[tokio::test]
async fn query_loop_compaction_causal_reference_rejects_damage_and_survives_metadata_order() {
    use crate::runtime::context_window::CAUSAL_ANCHOR_REFERENCE;
    let saved = compacted_chain().await;
    let anchor_id = saved.messages[0].message_id.clone();
    let summary_index = saved
        .messages
        .iter()
        .position(|m| m.metadata.contains_key(CAUSAL_ANCHOR_REFERENCE))
        .unwrap();
    let mut reordered: SessionStateSnapshot =
        serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    let mut entries = reordered.messages[0].metadata.drain().collect::<Vec<_>>();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    for (key, value) in entries {
        reordered.messages[0].metadata.insert(key, value);
    }
    refresh_session_context_window(&mut reordered);
    validate_context_window_materialization(&reordered).unwrap();
    assert_eq!(
        serde_json::to_value(&reordered.context_window).unwrap(),
        serde_json::to_value(&saved.context_window).unwrap()
    );
    for case in [
        "missing",
        "duplicate",
        "body",
        "role",
        "kind",
        "origin",
        "createdAt",
        "schema",
        "unknownField",
        "boundary",
    ] {
        let mut damaged = saved.clone();
        match case {
            "missing" => {
                damaged.messages.remove(0);
            }
            "duplicate" => {
                damaged.messages.push(damaged.messages[0].clone());
            }
            "body" => damaged.messages[0].content.push_str("changed"),
            "role" => damaged.messages[0].role = MessageRole::Assistant,
            "kind" => {
                damaged.messages[0]
                    .metadata
                    .insert(MESSAGE_SEMANTIC_KIND_META_KEY.into(), "unrecognized".into());
            }
            "origin" => {
                damaged.messages[0]
                    .metadata
                    .insert("origin_fixture".into(), "changed".into());
            }
            "createdAt" => damaged.messages[0].created_at_ms += 1,
            "boundary" => {
                damaged.messages[summary_index]
                    .metadata
                    .insert("first_kept_message_id".into(), anchor_id.clone());
            }
            "schema" | "unknownField" => {
                let raw = &damaged.messages[summary_index].metadata[CAUSAL_ANCHOR_REFERENCE];
                let mut reference: Value = serde_json::from_str(raw).unwrap();
                if case == "schema" {
                    reference["schema"] = json!("unsupported");
                } else {
                    reference["unexpected"] = json!(true);
                }
                damaged.messages[summary_index]
                    .metadata
                    .insert(CAUSAL_ANCHOR_REFERENCE.into(), reference.to_string());
            }
            _ => unreachable!(),
        }
        refresh_session_context_window(&mut damaged);
        let error = validate_context_window_materialization(&damaged).expect_err(case);
        assert!(
            error.contains("context_compaction_anchor_"),
            "{case}: {error}"
        );
    }
}

#[tokio::test]
async fn query_loop_failed_chain_compaction_keeps_original_projection_and_pending_pairs() {
    let store = AgentRuntimeTestStore::new();
    let manager = SessionManager::new(store.clone());
    let session_id = "chat-chain-failure";
    let original = continuous_tool_chain(session_id, MESSAGE_SEMANTIC_USER_REQUEST);
    manager.save_session(&original).unwrap();
    let engine = AgentRuntime::new_for_test(store.clone(), chain_config());
    let client =
        PromptCompactionModelClient::new(PromptCompactionModelClientBehavior::ProviderError);
    let configs = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let driver = ModelClientGenerateDriver::new(&client, &configs);
    engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "turn-provider-failure",
            &mut TurnInput::ToolContinuation {
                objective: "continue".into(),
            },
            1,
            PromptCompactionScopeV1::main(),
            None,
            None,
            &driver,
            None,
        )
        .await
        .unwrap();
    let failed = manager.load_or_create_session(session_id).unwrap();
    assert_eq!(
        serde_json::to_value(&failed.messages).unwrap(),
        serde_json::to_value(&original.messages).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&failed.context_window).unwrap(),
        serde_json::to_value(&original.context_window).unwrap()
    );
    assert!(failed
        .metadata
        .contains_key(PROMPT_COMPACTION_FAILURE_META_KEY));

    let mut pending = original.clone();
    pending.messages.pop();
    refresh_session_context_window(&mut pending);
    manager.save_session(&pending).unwrap();
    let error = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "turn-pending-pair",
            &mut TurnInput::ToolContinuation {
                objective: "continue".into(),
            },
            1,
            PromptCompactionScopeV1::main(),
            None,
            None,
            &TestPromptCompactionAsyncDriver,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.contains("tool_pairing"), "{error}");
    let unchanged = manager.load_or_create_session(session_id).unwrap();
    assert_eq!(
        serde_json::to_value(&unchanged.messages).unwrap(),
        serde_json::to_value(&pending.messages).unwrap()
    );
}

#[tokio::test]
async fn query_loop_compacts_closed_batches_inside_one_user_chain_and_reopens_exact_anchor() {
    for kind in [
        MESSAGE_SEMANTIC_USER_REQUEST,
        MESSAGE_SEMANTIC_ANSWER_NOW,
        crate::session::host_event_input::HOST_EVENT_SEMANTIC_KIND,
    ] {
        let store = AgentRuntimeTestStore::new();
        let manager = SessionManager::new(store.clone());
        let session_id = format!("chat-continuous-chain-{kind}");
        let original = continuous_tool_chain(&session_id, kind);
        let anchor = original.messages[0].clone();
        manager.save_session(&original).unwrap();
        let engine = AgentRuntime::new_for_test(
            store.clone(),
            AgentRuntimeConfig {
                model_context_tokens: 8000,
                model_max_output_tokens: 1024,
                prompt_compaction_summary_max_tokens: 1024,
                prompt_compaction_trigger_headroom_tokens: u32::MAX,
                ..Default::default()
            },
        );
        let request = engine
            .build_generate_driver_request_with_async_driver_and_runtime_scope(
                &session_id,
                "turn-compacts-closed-chain",
                &mut TurnInput::ToolContinuation {
                    objective: "continue".into(),
                },
                1,
                PromptCompactionScopeV1::main(),
                None,
                None,
                &TestPromptCompactionAsyncDriver,
                None,
            )
            .await
            .unwrap();
        let stats: Value =
            serde_json::from_str(request.compression_stats_json.as_deref().unwrap()).unwrap();
        let pressure = &stats["decision"]["pressure"];
        assert!(
            pressure["estimatedInputTokens"].as_u64().unwrap()
                >= pressure["triggerInputTokens"].as_u64().unwrap(),
            "fixture must cross the actual compaction trigger: {stats}"
        );
        let saved = manager.load_or_create_session(&session_id).unwrap();
        assert!(
            saved.context_window.iter().any(|message| message
                .metadata
                .get("kind")
                .map(String::as_str)
                == Some("context_compaction")),
            "closed earlier batches inside a single user chain must be compactable"
        );
        let projected_anchor = saved
            .context_window
            .iter()
            .find(|message| message.message_id == anchor.message_id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(projected_anchor).unwrap(),
            serde_json::to_value(&anchor).unwrap()
        );
        assert_eq!(
            saved
                .context_window
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .count(),
            1
        );
        assert!(request
            .prepared_prompt
            .messages
            .iter()
            .all(|message| !message.content.contains("confirmed result closed-older")));
        assert_eq!(
            request
                .prepared_prompt
                .messages
                .iter()
                .filter_map(|message| message.tool_call_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["latest-a", "latest-b"]
        );
        assert_eq!(
            serde_json::to_value(&saved.messages[..original.messages.len()]).unwrap(),
            serde_json::to_value(&original.messages).unwrap()
        );
        assert!(original
            .model_semantics
            .iter()
            .all(|(id, value)| saved.model_semantics.get(id) == Some(value)));
        let mut restored: SessionStateSnapshot =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        refresh_session_context_window(&mut restored);
        validate_context_window_materialization(&restored).unwrap();
        assert_eq!(
            serde_json::to_value(&restored.context_window).unwrap(),
            serde_json::to_value(&saved.context_window).unwrap()
        );
        let restored_manager = SessionManager::new(store.clone());
        restored_manager.save_session(&restored).unwrap();
        let reopened = AgentRuntime::new_for_test(
            store,
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                ..Default::default()
            },
        );
        let continued = reopened
            .build_generate_driver_request_with_runtime_scope(
                &session_id,
                "turn-after-reopen",
                &TurnInput::ToolContinuation {
                    objective: "continue".into(),
                },
                2,
                PromptCompactionScopeV1::main(),
            )
            .unwrap();
        assert!(continued
            .prepared_prompt
            .messages
            .iter()
            .any(|message| message.message_id == anchor.message_id
                && message.content == anchor.content));
    }
}

#[tokio::test]
async fn query_loop_repeated_chain_compaction_keeps_one_original_anchor() {
    let mut saved = compacted_chain().await;
    let original_anchor = saved.messages[0].clone();
    let original_history = saved.messages.clone();
    let session_id = saved.session_id.clone();
    let handler = MessageHandler::new(MessageHandlerConfig::default());
    // Two newly closed batches provide real pressure after the first compaction.
    for call in ["second-old-a", "second-old-b"] {
        append_prompt_test_tool_group(&handler, &mut saved, &[call]);
        saved.messages.last_mut().unwrap().content =
            format!("confirmed result {call} ").repeat(200);
    }
    append_prompt_test_tool_group(
        &handler,
        &mut saved,
        &["second-latest-a", "second-latest-b"],
    );
    handler.refresh_context_window(&mut saved);
    let store = AgentRuntimeTestStore::new();
    let manager = SessionManager::new(store.clone());
    manager.save_session(&saved).unwrap();
    let engine = AgentRuntime::new_for_test(store, chain_config());
    let request = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            &session_id,
            "turn-chain-second-compaction",
            &mut TurnInput::ToolContinuation {
                objective: "continue".into(),
            },
            2,
            PromptCompactionScopeV1::main(),
            None,
            None,
            &TestPromptCompactionAsyncDriver,
            None,
        )
        .await
        .unwrap();
    let stats: Value =
        serde_json::from_str(request.compression_stats_json.as_deref().unwrap()).unwrap();
    let pressure = &stats["decision"]["pressure"];
    assert!(
        pressure["estimatedInputTokens"].as_u64().unwrap()
            >= pressure["triggerInputTokens"].as_u64().unwrap(),
        "{stats}"
    );
    assert_eq!(stats["triggered"], true, "{stats}");
    let saved = manager.load_or_create_session(&session_id).unwrap();
    assert_eq!(
        saved
            .context_window
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count(),
        1
    );
    assert_eq!(
        serde_json::to_value(
            saved
                .context_window
                .iter()
                .find(|m| m.message_id == original_anchor.message_id)
                .unwrap()
        )
        .unwrap(),
        serde_json::to_value(&original_anchor).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&saved.messages[..original_history.len()]).unwrap(),
        serde_json::to_value(&original_history).unwrap()
    );
    assert_eq!(
        request
            .prepared_prompt
            .messages
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .collect::<Vec<_>>(),
        vec!["second-latest-a", "second-latest-b"]
    );
    let mut restored: SessionStateSnapshot =
        serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
    refresh_session_context_window(&mut restored);
    validate_context_window_materialization(&restored).unwrap();
    assert_eq!(
        serde_json::to_value(&restored.context_window).unwrap(),
        serde_json::to_value(&saved.context_window).unwrap()
    );
}
