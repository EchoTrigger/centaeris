use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompactedMainFailure {
    Provider,
    Acknowledgement,
    Commit,
}

struct RecoveryAttemptModel {
    client: IntakeModelClient,
    fail_after_commit: bool,
}

impl ModelClient for RecoveryAttemptModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            if self.fail_after_commit {
                self.client.requests.lock().unwrap().push(request.clone());
                return Err(ModelClientError::new(
                    ModelClientErrorKind::Provider,
                    "injected_repeated_recovery_provider_failure",
                    false,
                ));
            }
            self.client.generate(request).await
        })
    }
}

struct CompactedRecoveryModel {
    queue: Arc<InputQueue>,
    requests: Mutex<Vec<ModelClientRequest>>,
    main_calls: AtomicUsize,
    compaction_calls: AtomicUsize,
}

impl ModelClient for CompactedRecoveryModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            let is_compaction = request
                .prepared_prompt
                .messages
                .first()
                .is_some_and(|message| {
                    message
                        .content
                        .contains("Return only a concise Markdown summary")
                });
            let content = if is_compaction {
                self.compaction_calls.fetch_add(1, Ordering::SeqCst);
                test_model_compaction_summary_from_prompt(
                    &request.prepared_prompt.messages[0].content,
                )
            } else {
                match self.main_calls.fetch_add(1, Ordering::SeqCst) {
                    0 => {
                        self.queue.add(
                            TurnInputPayload::UserSupplement {
                                supplement_id: "active-anchor".into(),
                                message: "  recent accepted anchor\n".into(),

                                attachments: Vec::new(),
                            },
                            2,
                        );
                        "initial response with older details. ".repeat(120)
                    }
                    1 => {
                        self.queue.add(
                            TurnInputPayload::HostEvent(native_input("return-after-anchor")),
                            3,
                        );
                        "response after the recent anchor. ".repeat(800)
                    }
                    _ => {
                        return Err(ModelClientError::new(
                            ModelClientErrorKind::Provider,
                            "injected_provider_failure_after_compaction",
                            false,
                        ))
                    }
                }
            };
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content,
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

pub(super) fn observed_context(
    record: &crate::session::SessionLogRecord,
) -> Vec<crate::model::prepared_prompt::ModelMessageV1> {
    serde_json::from_value::<Vec<ModelObservationV1>>(record.payload["observations"].clone())
        .unwrap()
        .into_iter()
        .filter_map(|observation| match observation {
            ModelObservationV1::ContextMessage { message } => Some(message),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn query_loop_same_run_recovery_uses_committed_compacted_context_before_new_input() {
    assert_compacted_context_recovery(CompactedMainFailure::Provider, false).await;
}

#[tokio::test]
async fn query_loop_repeated_compacted_context_recovery_preserves_membership_with_empty_uptake() {
    assert_compacted_context_recovery(CompactedMainFailure::Provider, true).await;
}

#[tokio::test]
async fn query_loop_compacted_context_ack_failure_reconciles_before_deferred_input() {
    assert_compacted_context_recovery(CompactedMainFailure::Acknowledgement, false).await;
}

#[tokio::test]
async fn query_loop_compacted_context_commit_failure_preserves_uncommitted_batch_before_deferred_input(
) {
    assert_compacted_context_recovery(CompactedMainFailure::Commit, false).await;
}

async fn assert_compacted_context_recovery(
    failure: CompactedMainFailure,
    fail_recovery_again: bool,
) {
    let initial_body = "  accepted original objective\n".to_string();
    let queue = Arc::new(InputQueue::default());
    queue.add(
        TurnInputPayload::UserSupplement {
            supplement_id: "initial-user".into(),
            message: initial_body.clone(),

            attachments: Vec::new(),
        },
        1,
    );
    let journal = Arc::new(RecoveryJournal::default());
    let queue_store = Arc::new(AckOnceFailQueue {
        queue: queue.clone(),
        fail_next_ack: AtomicBool::new(false),
        ack_attempts: Mutex::new(vec![]),
    });
    let prepared_main = Mutex::new(None);
    let store = AgentRuntimeTestStore::new();
    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 12_000;
    config.prompt_compaction_summary_max_tokens = 2_000;
    let engine = AgentRuntime::new_for_test(store.clone(), config.clone());
    let captured = journal.clone();
    let control = TurnControl::new_durable_inputs(
        queue_store.clone(),
        input_binding("before-compaction-failure"),
        Arc::new(move |turn, inputs| captured.materialize(turn, inputs, Some("initial-user"))),
    )
    .unwrap();
    let mut request = input_run("active-input");
    request.agent_run_identity = Some(input_identity());
    request.initial_input = AgentRunInitialInput::UserInput {
        input_id: "initial-user".into(),
        message: initial_body.clone(),

        attachments: Vec::new(),
    };
    let model = CompactedRecoveryModel {
        queue: queue.clone(),
        requests: Mutex::new(vec![]),
        main_calls: AtomicUsize::new(0),
        compaction_calls: AtomicUsize::new(0),
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
                if let ToolSafePoint::ModelRequestStarted(started) = point {
                    if started.purpose == ModelRequestPurposeV1::Main
                        && journal.read_facts().len() == 2
                    {
                        *prepared_main.lock().unwrap() = Some(started.clone());
                        match failure {
                            CompactedMainFailure::Acknowledgement => {
                                queue_store.fail_next_ack.store(true, Ordering::SeqCst)
                            }
                            CompactedMainFailure::Commit => {
                                return Err("injected_compacted_main_commit_failure".into())
                            }
                            CompactedMainFailure::Provider => {}
                        }
                    }
                    journal.records.lock().unwrap().extend(
                        canonical_model_request_started_records("input-run", &started, 48)?,
                    );
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains(match failure {
        CompactedMainFailure::Provider => "injected_provider_failure_after_compaction",
        CompactedMainFailure::Acknowledgement => "injected_input_ack_failure",
        CompactedMainFailure::Commit => "injected_compacted_main_commit_failure",
    }));
    assert!(
        model.compaction_calls.load(Ordering::SeqCst) > 0,
        "the test must exercise real Core compaction"
    );
    let original_facts = journal.read_facts();
    assert_eq!(
        original_facts.len(),
        if failure == CompactedMainFailure::Commit {
            2
        } else {
            3
        }
    );
    let prepared_main = prepared_main.into_inner().unwrap().unwrap();
    let prepared_record = canonical_model_request_started_records("input-run", &prepared_main, 48)
        .unwrap()
        .pop()
        .unwrap();
    let original_driver_id =
        prompt_projection::driver_user_message_id("active-input", "input-turn");
    let compacted_context = observed_context(&prepared_record);
    assert!(
        compacted_context
            .iter()
            .all(|message| message.message_id != original_driver_id),
        "the old initial input was removed by compaction"
    );
    assert!(
        compacted_context
            .iter()
            .any(|message| message.content == "  recent accepted anchor\n"
                || (message.content == initial_body && message.message_id != original_driver_id)),
        "Core retains the relevant user context while replacing the old initial identity"
    );
    assert_eq!(input_ids(&original_facts[0]), ["initial-user"]);
    assert_eq!(input_ids(&original_facts[1]), ["active-anchor"]);
    if failure != CompactedMainFailure::Commit {
        assert_eq!(input_ids(&original_facts[2]), ["return-after-anchor"]);
    } else {
        assert!(
            !queue
                .0
                .lock()
                .unwrap()
                .acknowledged
                .iter()
                .any(|id| id == "return-after-anchor"),
            "a failed main commit cannot acknowledge or establish Read"
        );
    }

    // The restart rebuilds through Core's authoritative record reducer, without
    // retaining an old in-memory snapshot or resurrecting compacted messages.
    let rebuilt = if failure == CompactedMainFailure::Commit {
        // Without a committed current request, the saved preparation remains
        // operational authority for its immutable body and membership.
        SessionManager::new(store.clone())
            .load_session("active-input")
            .unwrap()
            .unwrap()
    } else {
        let records = journal.records.lock().unwrap().clone();
        crate::session::restore_runtime_snapshot_from_session_records("active-input", &records)
            .unwrap()
    };
    assert!(rebuilt
        .context_window
        .iter()
        .all(|message| message.message_id != original_driver_id));
    queue.add(
        TurnInputPayload::UserSupplement {
            supplement_id: "after-provider-failure".into(),
            message: "  later arrival\n".into(),

            attachments: Vec::new(),
        },
        4,
    );
    let mut recovery_snapshot = rebuilt;
    for retry in 0..=usize::from(fail_recovery_again) {
        let recovered_store = AgentRuntimeTestStore::new();
        SessionManager::new(recovered_store.clone())
            .save_session(&recovery_snapshot)
            .unwrap();
        let captured = journal.clone();
        let recovered_control = TurnControl::new_durable_inputs(
            queue_store.clone(),
            input_binding(if retry == 0 {
                "first-recovery"
            } else {
                "second-recovery"
            }),
            Arc::new(move |turn, inputs| captured.materialize(turn, inputs, Some("initial-user"))),
        )
        .unwrap();
        let fail_after_commit = fail_recovery_again && retry == 0;
        let recovered_model = RecoveryAttemptModel {
            client: IntakeModelClient {
                requests: Mutex::new(vec![]),
                control: None,
            },
            fail_after_commit,
        };
        let recovered_engine = AgentRuntime::new_for_test(recovered_store.clone(), config.clone());
        let result = recovered_engine
            .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                request.clone(),
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
            .await;
        let requests = recovered_model.client.requests.lock().unwrap();
        if fail_after_commit {
            assert!(result
                .unwrap_err()
                .contains("injected_repeated_recovery_provider_failure"));
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].prepared_prompt.messages, compacted_context);
            let facts = journal.read_facts();
            assert!(
                input_ids(facts.last().unwrap()).is_empty(),
                "current request replay does not establish another Read"
            );
            assert!(!queue.0.lock().unwrap().closed);
            let records = journal.records.lock().unwrap().clone();
            assert!(
                !records
                    .iter()
                    .any(|record| record.payload["supplementId"] == "after-provider-failure"),
                "a deferred arrival has not been assigned to an uncommitted subsequent turn"
            );
            recovery_snapshot = crate::session::restore_runtime_snapshot_from_session_records(
                "active-input",
                &records,
            )
            .unwrap();
            continue;
        }
        assert_eq!(result.unwrap().stop, AgentRunStop::Finalized);
        assert_eq!(
            requests.len(),
            2,
            "new input follows the restored request at the next legal boundary"
        );
        assert_eq!(
            requests[0].prepared_prompt.messages, compacted_context,
            "recovery reuses the current committed context and does not restore an obsolete batch"
        );
        assert_eq!(
            requests[0].turn_id, prepared_main.turn_id,
            "recovery uses the current request turn, retaining the initial Run identity"
        );
        assert!(requests[1]
            .prepared_prompt
            .messages
            .iter()
            .any(|message| message.content == "  later arrival\n"));
    }
    let facts = journal.read_facts();
    for id in [
        "initial-user",
        "active-anchor",
        "return-after-anchor",
        "after-provider-failure",
    ] {
        assert_eq!(
            facts
                .iter()
                .flat_map(input_ids)
                .filter(|seen| seen == id)
                .count(),
            1,
            "each identity has one committed Read across the restart"
        );
    }
    assert_eq!(
        input_ids(&facts[original_facts.len()]),
        if failure == CompactedMainFailure::Commit {
            vec!["return-after-anchor".to_string()]
        } else {
            vec![]
        }
    );
    assert_eq!(input_ids(facts.last().unwrap()), ["after-provider-failure"]);
    for id in [
        "active-anchor",
        "return-after-anchor",
        "after-provider-failure",
    ] {
        let records = journal.records.lock().unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| record.payload["supplementId"] == id
                    || record.payload["inputId"] == id)
                .count(),
            1,
            "recovery must not duplicate canonical materialization"
        );
    }
    if failure == CompactedMainFailure::Acknowledgement {
        assert!(
            queue_store
                .ack_attempts
                .lock()
                .unwrap()
                .iter()
                .any(|attempt| attempt.claim_token == "first-recovery"
                    && attempt
                        .input_ids
                        .iter()
                        .any(|id| id == "return-after-anchor")),
            "a committed input reconciles its lost ACK under the new fence"
        );
    }
    assert!(queue.0.lock().unwrap().closed);
}
