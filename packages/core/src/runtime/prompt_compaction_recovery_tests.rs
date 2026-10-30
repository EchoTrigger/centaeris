use super::*;
use crate::session::{restore_runtime_snapshot_from_session_records, SessionLogRecord};

fn recovery_tool_facts(session: &SessionStateSnapshot, run: &str) -> Vec<SessionLogRecord> {
    let mut records = Vec::new();
    for (index, message) in session.messages.iter().enumerate() {
        let Some(ModelMessageSemanticsV1::ToolResult {
            tool_call_id,
            tool_name,
            ..
        }) = session.model_semantics.get(&message.message_id)
        else {
            continue;
        };
        let call = ToolCallEnvelope {
            id: tool_call_id.clone(),
            name: tool_name.clone(),
            args_json: "{}".into(),
        };
        let turn = format!("accepted-tool-{index}");
        records.push(
            canonical_tool_call_record(
                &session.session_id,
                &turn,
                run,
                &call,
                "centaeris.builtin",
                &format!("sha256:{}", "0".repeat(64)),
                "fixture",
                index as i64 + 2,
            )
            .unwrap(),
        );
        let result = ToolExecutionResult {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            status: "ok".into(),
            content: message.content.clone(),
            details: json!({}),
            facts: vec![],
            error: None,
            started_at_ms: 2,
            completed_at_ms: 3,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: None,
        };
        records.push(
            canonical_tool_result_record(
                &session.session_id,
                &turn,
                run,
                &call,
                &result,
                index as i64 + 2,
            )
            .unwrap(),
        );
    }
    records
}

#[tokio::test]
async fn query_loop_canonical_main_recovery_keeps_compacted_chain_anchor_and_closed_batch() {
    for host_event in [false, true] {
        let session_id = if host_event {
            "recover-compacted-host"
        } else {
            "recover-compacted-user"
        };
        let run = "accepted-chain-run";
        let host = crate::session::host_event_input::HostEventInput::new(
            session_id,
            "accepted-event".into(),
            "opaque://approved".into(),
            "Continue the accepted objective.".into(),
        )
        .unwrap();
        let mut records = if host_event {
            vec![crate::session::host_event_input::host_event_input_record(
                session_id, "initial", run, &host, 1,
            )
            .unwrap()]
        } else {
            crate::session::started_agent_run_records(
                session_id,
                "initial",
                run,
                "  Continue the accepted objective.\n",
                1,
            )
            .unwrap()
            .to_vec()
        };
        // Use canonical accepted input facts to create the anchor, not test-invented metadata.
        let mut session =
            restore_runtime_snapshot_from_session_records(session_id, &records).unwrap();
        let anchor = session.messages[0].clone();
        assert!(crate::runtime::context_window::is_reliable_tool_chain_user_anchor(&anchor));
        let handler = MessageHandler::new(MessageHandlerConfig::default());
        for call in ["completed-old-a", "completed-old-b"] {
            append_prompt_test_tool_group(&handler, &mut session, &[call]);
            session.messages.last_mut().unwrap().content =
                "confirmed closed historical evidence ".repeat(700);
        }
        append_prompt_test_tool_group(
            &handler,
            &mut session,
            &["latest-parallel-a", "latest-parallel-b"],
        );
        records.extend(recovery_tool_facts(&session, run));
        handler.refresh_context_window(&mut session);
        let store = AgentRuntimeTestStore::new();
        let manager = SessionManager::new(store.clone());
        manager.save_session(&session).unwrap();
        let engine = AgentRuntime::new_for_test(store.clone(), large_context_compaction_config());
        let prepared = engine
            .build_generate_driver_request_with_async_driver_and_runtime_scope(
                session_id,
                "main-after-compaction",
                &mut TurnInput::ToolContinuation {
                    objective: "continue".into(),
                },
                10,
                PromptCompactionScopeV1::main(),
                None,
                None,
                &TestPromptCompactionAsyncDriver,
                None,
            )
            .await
            .unwrap();
        let saved = manager.load_or_create_session(session_id).unwrap();
        let stats: Value =
            serde_json::from_str(&saved.metadata["prompt_compaction_stats_json"]).unwrap();
        assert!(
            stats["decision"]["pressure"]["estimatedInputTokens"]
                .as_u64()
                .unwrap()
                >= stats["trigger_input_tokens"].as_u64().unwrap(),
            "fixture must reach real pressure"
        );
        let summary = saved
            .context_window
            .iter()
            .find(|message| {
                message.metadata.get("kind").map(String::as_str) == Some("context_compaction")
            })
            .expect("closed batches in the same accepted input must compact before recovery")
            .clone();
        assert!(prepared
            .prepared_prompt
            .messages
            .iter()
            .any(|message| message.message_id == anchor.message_id
                && message.content == anchor.content));
        let request = ModelClientRequest {
            session_id: prepared.session_id.clone(),
            turn_id: prepared.turn_id.clone(),
            loop_index: prepared.loop_index,
            provider_prompt_cache_key: prepared.provider_prompt_cache_key.clone(),
            provider_prompt_cache_retention: prepared.provider_prompt_cache_retention.clone(),
            system_prompt_manifest_json: prepared.system_prompt_manifest_json.clone(),
            compression_stats_json: prepared.compression_stats_json.clone(),
            context_token_estimate: prepared.context_token_estimate,
            prepared_prompt: prepared.prepared_prompt.clone(),
            session_config: ModelSessionConfig::default(),
        };
        let started = ModelRequestStartedV1::from_request(
            ModelRequestPurposeV1::Main,
            &request,
            prepared.observations.clone(),
            engine
                .agent_composition_environment()
                .unwrap()
                .resolve_request(&request)
                .unwrap(),
        )
        .unwrap();
        records.extend(canonical_model_request_started_records(run, &started, 100).unwrap());
        let committed_before = serde_json::to_vec(&records).unwrap();
        // This is the actual server/local canonical-main reducer, not a snapshot clone.
        let restored = restore_runtime_snapshot_from_session_records(session_id, &records).unwrap();
        let restored_anchor = restored
            .messages
            .iter()
            .find(|message| message.message_id == anchor.message_id)
            .unwrap();
        assert_eq!(restored_anchor.content, anchor.content);
        assert!(
            crate::runtime::context_window::is_reliable_tool_chain_user_anchor(restored_anchor)
        );
        if host_event {
            assert_eq!(
                crate::session::host_event_input::host_event_origin(session_id, restored_anchor)
                    .unwrap(),
                Some(host.clone())
            );
        } else {
            assert_eq!(
                restored_anchor
                    .metadata
                    .get(MESSAGE_SEMANTIC_KIND_META_KEY)
                    .map(String::as_str),
                Some(MESSAGE_SEMANTIC_USER_REQUEST)
            );
        }
        assert!(restored
            .messages
            .iter()
            .any(|message| message.message_id == summary.message_id
                && message.content == summary.content));
        assert!(restored.messages.iter().all(|message| !message
            .content
            .contains("confirmed closed historical evidence")));
        let reopened_store = AgentRuntimeTestStore::new();
        SessionManager::new(reopened_store.clone())
            .save_session(&restored)
            .unwrap();
        // With no compaction driver available, successful continuation proves no regenerated summary is required.
        let reopened = AgentRuntime::new_for_test(reopened_store, AgentRuntimeConfig::default());
        let continued = reopened
            .build_generate_driver_request_with_runtime_scope(
                session_id,
                "main-after-recovery",
                &TurnInput::ToolContinuation {
                    objective: "continue".into(),
                },
                11,
                PromptCompactionScopeV1::main(),
            )
            .unwrap();
        assert!(continued
            .prepared_prompt
            .messages
            .iter()
            .any(|message| message.message_id == summary.message_id
                && message.content == summary.content));
        assert_eq!(
            continued
                .prepared_prompt
                .messages
                .iter()
                .filter(|message| message.message_id == anchor.message_id)
                .count(),
            1
        );
        assert_eq!(
            continued
                .prepared_prompt
                .messages
                .iter()
                .filter_map(|message| message.tool_call_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["latest-parallel-a", "latest-parallel-b"]
        );
        assert_eq!(
            serde_json::to_vec(&records).unwrap(),
            committed_before,
            "recovery must not append/replay accepted tool effects or regenerate compaction"
        );
    }
}

#[derive(Debug)]
struct RecoveryFinalModel {
    requests: AtomicUsize,
}
impl ModelClient for RecoveryFinalModel {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(ModelClientResponse {
                transport_timings: None,
                generate_result: GenerateResult {
                    content: "continuation accepted".into(),
                    tool_calls: vec![],
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
async fn query_loop_async_compaction_and_main_share_one_durable_boundary_before_provider_call() {
    let session_id = "async-compaction-canonical-boundary";
    let store = AgentRuntimeTestStore::new();
    let manager = SessionManager::new(store.clone());
    manager
        .save_session(&model_prompt_compaction_session(session_id))
        .unwrap();
    let engine_config = AgentRuntimeConfig {
        model_context_tokens: 11000,
        model_max_output_tokens: 1024,
        prompt_compaction_trigger_headroom_tokens: u32::MAX,
        prompt_compaction_user_replay_tokens: 30,
        prompt_compaction_summary_max_tokens: 1024,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test(store.clone(), engine_config);
    let model = RecoveryFinalModel {
        requests: AtomicUsize::new(0),
    };
    let config = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let composition = engine.agent_composition_environment().unwrap();
    let mut sequence = AgentRunSessionState::new(session_id, "accepted-run").unwrap();
    let durable = Mutex::new(Vec::new());
    let lost_ack = AtomicBool::new(true);
    let conflict_ack = AtomicBool::new(true);
    let observed_starts = Mutex::new(Vec::new());
    let mut commit = |point| {
        if let ToolSafePoint::ModelRequestStarted(started) = point {
            observed_starts.lock().unwrap().push(started.clone());
            let expected = engine.compaction_ack_snapshot(
                started.prompt_compaction.as_deref(),
                started.request_id(),
            )?;
            let mut candidate = sequence.clone();
            let records = candidate.record_model_request_started(&started, 100)?;
            if started.purpose() == ModelRequestPurposeV1::Main {
                assert_eq!(
                    model.requests.load(Ordering::SeqCst),
                    1,
                    "only summary provider call may precede main durable acknowledgment"
                );
                let types = records
                    .iter()
                    .map(|record| record.event.event_type)
                    .collect::<Vec<_>>();
                if lost_ack.load(Ordering::SeqCst) {
                    assert_eq!(
                        types,
                        vec![
                            SessionRecordType::Compaction,
                            SessionRecordType::ModelRequestStarted
                        ],
                        "async summary and observing main must commit together"
                    );
                    assert_eq!(
                        records[0].event.payload["summaryMarkdown"],
                        "continuation accepted"
                    );
                } else {
                    assert_eq!(
                        types,
                        vec![SessionRecordType::ModelRequestStarted],
                        "ack loss retry must not duplicate committed compaction"
                    );
                }
            }
            durable.lock().unwrap().extend(records);
            sequence = candidate;
            if started.purpose() == ModelRequestPurposeV1::Main
                && lost_ack.swap(false, Ordering::SeqCst)
            {
                return Err("canonical_batch_commit_ack_lost".into());
            }
            if started.purpose() == ModelRequestPurposeV1::Main
                && conflict_ack.swap(false, Ordering::SeqCst)
            {
                let mut future = manager.load_or_create_session(session_id)?;
                future
                    .metadata
                    .insert("future_controller_fact".into(), "preserve".into());
                manager.save_session(&future)?;
            }
            engine.acknowledge_compaction_after_safe_point(
                started.prompt_compaction.as_deref(),
                expected.as_ref(),
            )?;
        }
        Ok(())
    };
    let dispatcher = ToolSafePointDispatcher {
        sink: Mutex::new(&mut commit),
    };
    let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
        &model,
        &config,
        &dispatcher,
        &composition,
        Some("accepted-run"),
    );
    let prepare = || TurnInput::UserMessage("Continue the approved task.".into());
    let request = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "main-with-summary",
            &mut prepare(),
            0,
            PromptCompactionScopeV1::main(),
            None,
            Some(&dispatcher),
            &driver,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        model.requests.load(Ordering::SeqCst),
        1,
        "summary must be generated by actual ModelClientGenerateDriver"
    );
    let pending = manager.load_or_create_session(session_id).unwrap();
    let pending_receipt = super::super::prompt_compaction_commit::active_commit(&pending)
        .unwrap()
        .expect("successful summary has a durable source identity");
    assert!(!pending_receipt.acknowledged);
    let pending_value = serde_json::to_value(&pending).unwrap();
    let blocked = engine
        .compact_session_online_with_model_client_and_tool_safe_point_async(
            session_id,
            "manual-cannot-hide-pending",
            &model,
            &config,
            &mut |_| Err("must not dispatch".into()),
        )
        .await
        .unwrap_err();
    assert!(
        blocked.contains("manual compaction cannot replace an unacknowledged automatic summary")
    );
    assert!(
        serde_json::to_value(manager.load_or_create_session(session_id).unwrap()).unwrap()
            == pending_value,
        "refused manual compaction must preserve the snapshot"
    );
    assert_eq!(model.requests.load(Ordering::SeqCst), 1);
    {
        let mut fenced = |_: ToolSafePoint| Err("task_controller_fenced".into());
        let fence_dispatcher = ToolSafePointDispatcher {
            sink: Mutex::new(&mut fenced),
        };
        let fence_driver = ModelClientGenerateDriver::new_with_tool_safe_point(
            &model,
            &config,
            &fence_dispatcher,
            &composition,
            Some("accepted-run"),
        );
        assert!(fence_driver
            .generate_next_async(&request)
            .await
            .unwrap_err()
            .message
            .contains("task_controller_fenced"));
        assert_eq!(model.requests.load(Ordering::SeqCst), 1);
        assert!(
            !super::super::prompt_compaction_commit::active_commit(
                &manager.load_or_create_session(session_id).unwrap()
            )
            .unwrap()
            .unwrap()
            .acknowledged
        );
        let foreign_driver = ModelClientGenerateDriver::new_with_tool_safe_point(
            &model,
            &config,
            &fence_dispatcher,
            &composition,
            Some("foreign-run"),
        );
        let error = engine
            .build_generate_driver_request_with_async_driver_and_runtime_scope(
                session_id,
                "foreign-run-main",
                &mut TurnInput::UserMessage("New Run cannot hide pending summary.".into()),
                0,
                PromptCompactionScopeV1::main(),
                None,
                Some(&fence_dispatcher),
                &foreign_driver,
                None,
            )
            .await
            .unwrap_err();
        assert!(error.contains("unacknowledged compaction receipt belongs to another AgentRun"));
        assert_eq!(model.requests.load(Ordering::SeqCst), 1);
    }
    let error = driver.generate_next_async(&request).await.unwrap_err();
    assert!(error.message.contains("canonical_batch_commit_ack_lost"));
    assert_eq!(
        model.requests.load(Ordering::SeqCst),
        1,
        "no main provider call after unacknowledged batch"
    );
    let retry = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "main-with-summary",
            &mut prepare(),
            0,
            PromptCompactionScopeV1::main(),
            None,
            Some(&dispatcher),
            &driver,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        model.requests.load(Ordering::SeqCst),
        1,
        "repeated preparation must reuse the persisted summary"
    );
    let retry_receipt = retry.prompt_compaction.as_ref().unwrap();
    assert_eq!(retry_receipt.event.event_id, pending_receipt.event.event_id);
    assert_eq!(retry_receipt.event.at_ms, pending_receipt.event.at_ms);
    assert_eq!(
        retry_receipt.source.request_id,
        pending_receipt.source.request_id
    );
    let conflict = driver.generate_next_async(&retry).await.unwrap_err();
    assert!(conflict
        .message
        .contains("snapshot changed after safe point"));
    assert_eq!(
        model.requests.load(Ordering::SeqCst),
        1,
        "CAS conflict must block the main provider"
    );
    assert_eq!(
        manager.load_or_create_session(session_id).unwrap().metadata["future_controller_fact"],
        "preserve"
    );
    driver.generate_next_async(&retry).await.unwrap();
    assert_eq!(model.requests.load(Ordering::SeqCst), 2);
    let acknowledged = manager.load_or_create_session(session_id).unwrap();
    assert!(
        super::super::prompt_compaction_commit::active_commit(&acknowledged)
            .unwrap()
            .unwrap()
            .acknowledged
    );
    {
        let records = durable.lock().unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| record.event.event_type == SessionRecordType::Compaction)
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(
                    |record| record.event.event_type == SessionRecordType::ModelRequestStarted
                        && record.event.payload["purpose"] == "compaction"
                )
                .count(),
            1
        );
        let mut replayed = AgentRunSessionState::new(session_id, "accepted-run").unwrap();
        for record in records.iter() {
            replayed.restore(record.clone()).unwrap();
        }
        let starts = observed_starts.lock().unwrap();
        let started = starts.last().unwrap();
        let canonical_records = records
            .iter()
            .map(|record| record.event.clone())
            .collect::<Vec<_>>();
        let mut reconstructed =
            restore_runtime_snapshot_from_session_records(session_id, &canonical_records).unwrap();
        let receipt = started.prompt_compaction.as_ref().unwrap();
        assert!(
            super::super::prompt_compaction_commit::active_commit(&reconstructed)
                .unwrap()
                .is_none()
        );
        assert!(
            super::super::prompt_compaction_commit::matches_reconstructed_main_proof(
                &mut reconstructed,
                started.request_id(),
                receipt
            )
            .unwrap(),
            "nonadjacent idempotent receipt must be proven by canonical source and current main"
        );
        assert!(
            !super::super::prompt_compaction_commit::matches_reconstructed_main_proof(
                &mut reconstructed,
                "another-main-request",
                receipt
            )
            .unwrap()
        );
        reconstructed.metadata.insert(
            "future_controller_fact".into(),
            "same-body-new-state".into(),
        );
        assert!(
        !super::super::prompt_compaction_commit::matches_reconstructed_main_proof(
            &mut reconstructed,
            started.request_id(),
            receipt
        )
        .unwrap(),
        "same messages with future controller metadata must never satisfy the reconstruction proof"
    );
        assert_eq!(
            replayed
                .record_model_request_started(started, 201)
                .unwrap()
                .iter()
                .map(|record| record.event.event_type)
                .collect::<Vec<_>>(),
            vec![SessionRecordType::ModelRequestStarted],
            "replayed authority must reconcile the identical compaction receipt"
        );
        for field in ["request", "turn", "prompt"] {
            let mut invalid = started.clone();
            let source = &mut invalid.prompt_compaction.as_mut().unwrap().source;
            match field {
                "request" => source.request_id.push_str("wrong"),
                "turn" => source.request_turn_id.push_str("wrong"),
                _ => source.prompt_sha256.push_str("wrong"),
            }
            assert!(replayed
                .record_model_request_started(&invalid, 202)
                .unwrap_err()
                .contains("not durably bound"));
        }
    }
    let other_model = RecoveryFinalModel {
        requests: AtomicUsize::new(0),
    };
    let mut accept_new = |point| {
        if let ToolSafePoint::ModelRequestStarted(started) = point {
            assert!(
                started.prompt_compaction.is_none(),
                "new Run observes acknowledged old summary without changing its attribution"
            );
            let mut new_run = AgentRunSessionState::new(session_id, "next-run").unwrap();
            new_run.record_model_request_started(&started, 200)?;
        }
        Ok(())
    };
    let new_dispatcher = ToolSafePointDispatcher {
        sink: Mutex::new(&mut accept_new),
    };
    let new_driver = ModelClientGenerateDriver::new_with_tool_safe_point(
        &other_model,
        &config,
        &new_dispatcher,
        &composition,
        Some("next-run"),
    );
    let error = new_driver.generate_next_async(&request).await.unwrap_err();
    assert!(error
        .message
        .contains("unacknowledged compaction receipt belongs to another AgentRun"));
    assert_eq!(other_model.requests.load(Ordering::SeqCst), 0);
    let accepted = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "next-run-main",
            &mut TurnInput::UserMessage("Next accepted objective.".into()),
            0,
            PromptCompactionScopeV1::main(),
            None,
            Some(&new_dispatcher),
            &new_driver,
            None,
        )
        .await
        .unwrap();
    new_driver.generate_next_async(&accepted).await.unwrap();
    assert_eq!(other_model.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn query_loop_manual_compaction_preserves_its_bool_contract_without_automatic_pending_receipt(
) {
    let session_id = "manual-summary-followed-by-new-run";
    let store = AgentRuntimeTestStore::new();
    let manager = SessionManager::new(store.clone());
    manager
        .save_session(&model_prompt_compaction_session(session_id))
        .unwrap();
    let engine = AgentRuntime::new_for_test(
        store,
        AgentRuntimeConfig {
            model_context_tokens: 11000,
            model_max_output_tokens: 1024,
            prompt_compaction_summary_max_tokens: 1024,
            ..Default::default()
        },
    );
    let model = RecoveryFinalModel {
        requests: AtomicUsize::new(0),
    };
    let config = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut old_run = AgentRunSessionState::new(session_id, "existing-finished-run").unwrap();
    let compacted = engine
        .compact_session_online_with_model_client_and_tool_safe_point_async(
            session_id,
            "manual-request",
            &model,
            &config,
            &mut |point| {
                let ToolSafePoint::ModelRequestStarted(started) = point else {
                    return Err("unexpected manual safe point".into());
                };
                assert_eq!(started.purpose(), ModelRequestPurposeV1::Compaction);
                old_run.record_model_request_started(&started, 100)?;
                Ok(())
            },
        )
        .await
        .unwrap();
    assert!(compacted);
    assert_eq!(model.requests.load(Ordering::SeqCst), 1);
    let saved = manager.load_or_create_session(session_id).unwrap();
    assert!(saved.context_window.iter().any(|message| message
        .metadata
        .get("kind")
        .map(String::as_str)
        == Some("context_compaction")));
    assert!(
        super::super::prompt_compaction_commit::active_commit(&saved)
            .unwrap()
            .is_none(),
        "manual does not claim the automatic durable receipt protocol"
    );
    let mut new_run = AgentRunSessionState::new(session_id, "next-accepted-run").unwrap();
    let mut commit = |point| {
        if let ToolSafePoint::ModelRequestStarted(started) = point {
            assert!(started.prompt_compaction.is_none());
            new_run.record_model_request_started(&started, 101)?;
        }
        Ok(())
    };
    let dispatcher = ToolSafePointDispatcher {
        sink: Mutex::new(&mut commit),
    };
    let composition = engine.agent_composition_environment().unwrap();
    let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
        &model,
        &config,
        &dispatcher,
        &composition,
        Some("next-accepted-run"),
    );
    let request = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "next-accepted-turn",
            &mut TurnInput::UserMessage("Continue after manual compaction.".into()),
            0,
            PromptCompactionScopeV1::main(),
            None,
            Some(&dispatcher),
            &driver,
            None,
        )
        .await
        .unwrap();
    driver.generate_next_async(&request).await.unwrap();
    assert_eq!(model.requests.load(Ordering::SeqCst), 2);
}
