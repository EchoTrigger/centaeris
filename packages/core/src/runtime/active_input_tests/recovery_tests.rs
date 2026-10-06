use super::*;

mod compacted_context_tests;
mod runtime_context_tests;
mod tool_context_tests;

#[derive(Debug)]
pub(super) struct AckOnceFailQueue {
    pub(super) queue: Arc<InputQueue>,
    pub(super) fail_next_ack: AtomicBool,
    pub(super) ack_attempts: Mutex<Vec<AcknowledgeTurnInputsRequest>>,
}

impl TurnInputStorePort for AckOnceFailQueue {
    fn claim_turn_inputs(
        &self,
        request: ClaimTurnInputsRequest,
    ) -> Result<Vec<DurableTurnInput>, String> {
        self.queue.claim_turn_inputs(request)
    }

    fn acknowledge_turn_inputs(&self, request: AcknowledgeTurnInputsRequest) -> Result<(), String> {
        self.ack_attempts.lock().unwrap().push(request.clone());
        if self.fail_next_ack.swap(false, Ordering::SeqCst) {
            return Err("injected_input_ack_failure".into());
        }
        self.queue.acknowledge_turn_inputs(request)
    }

    fn close_turn_input_queue(&self, request: CloseTurnInputQueueRequest) -> Result<(), String> {
        self.queue.close_turn_input_queue(request)
    }
}

#[derive(Default)]
pub(super) struct RecoveryJournal {
    records: Mutex<Vec<crate::session::SessionLogRecord>>,
}

impl RecoveryJournal {
    pub(super) fn materialize(
        &self,
        turn_id: &str,
        inputs: &[DurableTurnInput],
        initial_user_id: Option<&str>,
    ) -> Result<(), String> {
        let mut records = self.records.lock().unwrap();
        for input in inputs {
            // An identified initial user reuses its admission, not a supplement record.
            if Some(input.payload.input_id()) == initial_user_id {
                continue;
            }
            let record = match &input.payload {
                TurnInputPayload::UserSupplement {
                    supplement_id,
                    message,
                    attachments,
                } => crate::session::turn_supplement_record_with_attachments(
                    "active-input",
                    turn_id,
                    "input-run",
                    supplement_id,
                    message,
                    attachments,
                    input.created_at_ms,
                )?,
                TurnInputPayload::HostEvent(input) => {
                    crate::session::host_event_input::host_event_input_record(
                        "active-input",
                        turn_id,
                        "input-run",
                        input,
                        2,
                    )?
                }
            };
            if let Some(existing) = records
                .iter()
                .find(|existing| existing.event_id == record.event_id)
            {
                if serde_json::to_value(existing).unwrap() != serde_json::to_value(&record).unwrap()
                {
                    return Err("recovery_materialization_identity_conflict".into());
                }
            } else {
                records.push(record);
            }
        }
        Ok(())
    }

    pub(super) fn commit_main(
        &self,
        started: &ModelRequestStartedV1,
        store: &AgentRuntimeTestStore,
    ) -> Result<(), String> {
        let mut records = self.records.lock().unwrap();
        records.extend(canonical_model_request_started_records(
            "input-run",
            started,
            48,
        )?);
        // Recovery imports the authoritative committed facts through Core's reducer.
        // The fixture does not invent a consumed-ID cache from claim or ACK state.
        let restored = crate::session::restore_runtime_snapshot_from_session_records(
            "active-input",
            &records,
        )?;
        SessionManager::new(store.clone()).save_session(&restored)
    }

    pub(super) fn read_facts(&self) -> Vec<crate::session::SessionLogRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|record| {
                record.event_type == crate::session::SessionRecordType::ModelRequestStarted
                    && record.payload["purpose"] == "main"
            })
            .cloned()
            .collect()
    }
}

#[tokio::test]
async fn query_loop_committed_input_uptake_survives_ack_failure_and_same_run_recovery() {
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
    let native = native_input("return-a");
    queue.add(TurnInputPayload::HostEvent(native.clone()), 3);
    let failing_queue = Arc::new(AckOnceFailQueue {
        queue: queue.clone(),
        fail_next_ack: AtomicBool::new(true),
        ack_attempts: Mutex::new(vec![]),
    });
    let journal = Arc::new(RecoveryJournal::default());
    let store = AgentRuntimeTestStore::new();
    let model = IntakeModelClient {
        requests: Mutex::new(vec![]),
        control: None,
    };
    let mut original_read = None;
    for (token, injected_failure) in [("first-claim", true), ("recovered-claim", false)] {
        let captured = journal.clone();
        let control = TurnControl::new_durable_inputs(
            failing_queue.clone(),
            input_binding(token),
            Arc::new(move |turn_id, inputs| {
                captured.materialize(turn_id, inputs, Some("initial-user"))
            }),
        )
        .unwrap();
        let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
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
        if injected_failure {
            assert!(result.unwrap_err().contains("injected_input_ack_failure"));
            let facts = journal.read_facts();
            assert_eq!(
                facts.len(),
                1,
                "the main request committed before ACK failed"
            );
            assert_eq!(
                input_ids(&facts[0]),
                ["initial-user", "user-update", "return-a"]
            );
            original_read = Some(serde_json::to_value(&facts[0]).unwrap());
            assert!(queue.0.lock().unwrap().acknowledged.is_empty());
            assert!(model.requests.lock().unwrap().is_empty());
        } else {
            assert_eq!(result.unwrap().stop, AgentRunStop::Finalized);
        }
    }
    let facts = journal.read_facts();
    assert_eq!(
        serde_json::to_value(&facts[0]).unwrap(),
        original_read.unwrap()
    );
    for id in ["initial-user", "user-update", "return-a"] {
        assert_eq!(
            facts
                .iter()
                .flat_map(input_ids)
                .filter(|input_id| input_id == id)
                .count(),
            1,
            "a new fence must not establish a second Read for {id}"
        );
        assert!(queue
            .0
            .lock()
            .unwrap()
            .acknowledged
            .iter()
            .any(|acked| acked == id));
    }
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let driver_id = prompt_projection::driver_user_message_id("active-input", "input-turn");
    let original_context = facts[0].payload["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|observation| {
            observation["kind"] == "message" && observation["message"]["messageId"] == driver_id
        })
        .unwrap();
    assert!(
        requests[0]
            .prepared_prompt
            .messages
            .iter()
            .any(|message| message.message_id == driver_id
                && message.content == original_context["message"]["content"].as_str().unwrap()),
        "previously read content remains available in the recovered context"
    );
    assert_eq!(
        requests[0]
            .prepared_prompt
            .messages
            .iter()
            .filter(|message| message.message_id == native.message_id)
            .count(),
        1,
        "recovery keeps the original native message, not a duplicate"
    );
    let records = journal.records.lock().unwrap();
    assert_eq!(
        records
            .iter()
            .filter(
                |record| record.event_type == crate::session::SessionRecordType::TurnSupplement
                    && record.payload["supplementId"] == "user-update"
            )
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(
                |record| record.event_type == crate::session::SessionRecordType::HostEventInput
                    && record.payload["inputId"] == "return-a"
            )
            .count(),
        1
    );
    assert!(failing_queue
        .ack_attempts
        .lock()
        .unwrap()
        .iter()
        .any(|attempt| attempt.claim_token == "recovered-claim"));
}

#[tokio::test]
async fn query_loop_failed_main_commit_recovery_preserves_prepared_batch_before_new_input() {
    assert_prepared_batch_recovery(false).await;
}

#[tokio::test]
async fn query_loop_lost_prepared_snapshot_response_preserves_batch_before_new_input() {
    assert_prepared_batch_recovery(true).await;
}

async fn assert_prepared_batch_recovery(lose_snapshot_response: bool) {
    let queue = Arc::new(InputQueue::default());
    queue.add(
        TurnInputPayload::UserSupplement {
            supplement_id: "user-update".into(),
            message: "  retained update\n".into(),

            attachments: Vec::new(),
        },
        1,
    );
    queue.add(TurnInputPayload::HostEvent(native_input("return-a")), 2);
    let journal = Arc::new(RecoveryJournal::default());
    let store = AgentRuntimeTestStore::new();
    if lose_snapshot_response {
        store.state().unwrap().fail_after_input_snapshot = Some(
            prompt_projection::driver_user_message_id("active-input", "input-turn"),
        );
    }
    let model = IntakeModelClient {
        requests: Mutex::new(vec![]),
        control: None,
    };
    let mut original_prepared_message = None;
    for (token, reject_main) in [("first-claim", true), ("recovered-claim", false)] {
        if !reject_main {
            // This is the missing arrival window in the original commit-failure test.
            queue.add(
                TurnInputPayload::UserSupplement {
                    supplement_id: "user-after-failure".into(),
                    message: "  newer update\n".into(),

                    attachments: Vec::new(),
                },
                3,
            );
        }
        let captured = journal.clone();
        let control = TurnControl::new_durable_inputs(
            queue.clone(),
            input_binding(token),
            Arc::new(move |turn_id, inputs| captured.materialize(turn_id, inputs, None)),
        )
        .unwrap();
        let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
        let mut request = input_run("active-input");
        request.agent_run_identity = Some(input_identity());
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
                        if reject_main {
                            return Err("injected_main_commit_failure".into());
                        }
                        journal.commit_main(&started, &store)?;
                    }
                    Ok(())
                },
            )
            .await;
        if reject_main {
            let injected_failure = if lose_snapshot_response {
                "injected_prepared_snapshot_response_failure"
            } else {
                "injected_main_commit_failure"
            };
            assert!(result.unwrap_err().contains(injected_failure));
            assert!(journal.read_facts().is_empty());
            assert!(queue.0.lock().unwrap().acknowledged.is_empty());
            assert!(model.requests.lock().unwrap().is_empty());
            let snapshot = SessionManager::new(store.clone())
                .load_session("active-input")
                .unwrap()
                .unwrap();
            let driver_id = prompt_projection::driver_user_message_id("active-input", "input-turn");
            original_prepared_message = Some(
                snapshot
                    .messages
                    .iter()
                    .find(|message| message.message_id == driver_id)
                    .unwrap()
                    .clone(),
            );
        } else {
            assert_eq!(result.unwrap().stop, AgentRunStop::Finalized);
        }
    }
    let requests = model.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "the new arrival follows the recovered batch at the next legal request boundary"
    );
    let original = original_prepared_message.unwrap();
    let recovered = requests[0]
        .prepared_prompt
        .messages
        .iter()
        .find(|message| message.message_id == original.message_id)
        .unwrap();
    assert_eq!(
        recovered.content, original.content,
        "the prepared message identity keeps its original body"
    );
    assert!(requests[0]
        .prepared_prompt
        .messages
        .iter()
        .all(|message| !message.content.contains("newer update")));
    assert!(requests[1]
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content == "  newer update\n"));
    let facts = journal.read_facts();
    assert_eq!(facts.len(), 2);
    assert_eq!(input_ids(&facts[0]), ["user-update", "return-a"]);
    assert_eq!(input_ids(&facts[1]), ["user-after-failure"]);
    let records = journal.records.lock().unwrap();
    for (id, body, owning_turn) in [
        (
            "user-update",
            "  retained update\n",
            facts[0].turn_id.as_deref(),
        ),
        (
            "user-after-failure",
            "  newer update\n",
            facts[1].turn_id.as_deref(),
        ),
    ] {
        let materializations = records
            .iter()
            .filter(|record| {
                record.event_type == crate::session::SessionRecordType::TurnSupplement
                    && record.payload["supplementId"] == id
            })
            .collect::<Vec<_>>();
        assert_eq!(
            materializations.len(),
            1,
            "a recovered input has one canonical materialization"
        );
        assert_eq!(materializations[0].payload["message"], body);
        assert_eq!(
            materializations[0].turn_id.as_deref(),
            owning_turn,
            "an arrival after failure belongs to its subsequent request boundary"
        );
        assert!(queue
            .0
            .lock()
            .unwrap()
            .acknowledged
            .iter()
            .any(|acked| acked == id));
    }
}
