use super::*;
use centaeris_core::session::reliability::*;
use centaeris_core::session::turn_input::*;
use serde_json::Value;

#[test]
#[ignore = "requires the API migrated disposable database and its owned input carriers"]
fn hosted_typed_input_store() {
    let fixture: serde_json::Value =
        serde_json::from_str(&env::var("CENTAERIS_AGENT_INPUT_FIXTURE").unwrap()).unwrap();
    let start = serde_json::from_value::<AgentRunStart>(fixture["agentRunStart"].clone())
        .unwrap()
        .validate(fixture["signingKey"].as_str().unwrap().as_bytes())
        .unwrap();
    let backend =
        Arc::new(PostgresRuntimeStore::new(fixture["databaseUrl"].as_str().unwrap()).unwrap());
    let job = agent_run_lifecycle_job_id(&start.agent_run_id).unwrap();
    let now = now_ms().unwrap();
    backend
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: job.clone(),
                job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
                status: RuntimeJobStatus::Queued,
                run_at_ms: now,
                lease_owner: None,
                lease_expires_at_ms: None,
                heartbeat_at_ms: None,
                retry_count: 0,
                max_retries: 10,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: format!("{job}:{}", start.authorization_digest),
                session_id: Some(start.authorization.session_id.clone()),
                branch_id: None,
                checkpoint_id: None,
                payload_ref: Some(format!("record:agent_run:{}", start.agent_run_id)),
                output_refs: vec![],
                last_error: None,
                created_at_ms: now,
                updated_at_ms: now,
            },
        })
        .unwrap();
    let lease = |worker: &str| {
        let owner = backend
            .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: now,
                worker_id: worker.into(),
                job_id: Some(job.clone()),
                job_kind: None,
                session_id: None,
                limit: 1,
                lease_ms: 600000,
            })
            .unwrap()
            .remove(0)
            .lease_owner
            .unwrap();
        backend
            .start_runtime_job(StartRuntimeJobRequest {
                job_id: job.clone(),
                lease_owner: owner.clone(),
                started_at_ms: now,
            })
            .unwrap();
        owner
    };
    let owner = lease("typed-input-writer");
    if fixture["attachmentRecovery"] == true {
        hosted_attachment_recovery(backend, &start, &job, &owner, now, &fixture);
        return;
    }
    if fixture["modelMain"] == true {
        native_main_intake(backend, &start, &job, &owner, now, &fixture);
        return;
    }
    let store = agent_inputs::HostedAgentInputStore::new(backend.clone(), start.clone());
    let mut claim = ClaimTurnInputsRequest {
        agent_run_id: start.agent_run_id.clone(),
        lifecycle_job_id: job.clone(),
        session_id: start.authorization.session_id.clone(),
        authorization_digest: start.authorization_digest.clone(),
        lease_owner: owner.clone(),
        claim_token: "claim-first".into(),
        now_ms: now,
        close_if_empty: true,
        limit: 256,
    };
    let first = store.claim_turn_inputs(claim.clone()).unwrap();
    let describe = |items: &[DurableTurnInput]| {
        items
            .iter()
            .map(|item| match &item.payload {
                TurnInputPayload::UserSupplement {
                    supplement_id,
                    message,
                    ..
                } => serde_json::json!({"inputId": supplement_id, "body": message}),
                TurnInputPayload::HostEvent(input) => {
                    serde_json::json!({"inputId": input.input_id, "native": true})
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(serde_json::json!(describe(&first)), fixture["expected"]);
    if fixture["commitNative"] == true {
        let log = backend
            .session_log(
                start.authorization.workspace_id.clone(),
                start.authorization.session_id.clone(),
                start.prompt.clone(),
            )
            .with_agent_run_start(&start)
            .unwrap();
        let mut sequence =
            AgentRunSessionState::new(&start.authorization.session_id, &start.agent_run_id)
                .unwrap();
        let startup = crate::started_session_records(&start, &mut sequence, now).unwrap();
        let fence = centaeris_core::session::RuntimeJobLeaseFence {
            job_id: job.clone(),
            job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
            lease_owner: owner.clone(),
        };
        let receipt = log
            .append_session_records_with_runtime_job_lease_blocking(
                &start.agent_run_id,
                &startup,
                &fence,
            )
            .unwrap();
        crate::accept_session_commit(&mut sequence, &mut None, &receipt).unwrap();
        for (i, item) in first.iter().enumerate() {
            if let TurnInputPayload::HostEvent(input) = &item.payload {
                let turn = if i == 1 {
                    start.turn_id.clone()
                } else {
                    "turn-active-next".into()
                };
                let record = sequence
                    .host_event_input_record(&turn, input, now)
                    .unwrap()
                    .unwrap();
                let receipt = log
                    .append_session_records_with_runtime_job_lease_blocking(
                        &start.agent_run_id,
                        &[record],
                        &fence,
                    )
                    .unwrap();
                crate::accept_session_commit(&mut sequence, &mut None, &receipt).unwrap();
                assert!(sequence
                    .host_event_input_record(&turn, input, now)
                    .unwrap()
                    .is_none());
            }
        }
    }
    assert!(store.claim_turn_inputs(claim.clone()).unwrap().is_empty());
    let accepting = backend
        .with_client(|client| {
            Ok(client
                .query_one(
                    "SELECT accepting FROM app_core_agentinputqueue WHERE agent_run_id=$1",
                    &[&start.agent_run_id],
                )
                .unwrap()
                .get::<_, bool>(0))
        })
        .unwrap();
    assert!(
        accepting,
        "held unacknowledged inputs prevent an empty close"
    );
    let mut stale = claim.clone();
    stale.lease_owner = "stale-writer".into();
    assert!(store.claim_turn_inputs(stale).is_err());
    let mut foreign = claim.clone();
    foreign.authorization_digest = format!("sha256:{}", "b".repeat(64));
    assert!(store.claim_turn_inputs(foreign).is_err());
    backend
        .with_client(|client| {
            client
                .execute(
                    "UPDATE runtime_jobs SET lease_expires_at_ms=$1 WHERE job_id=$2",
                    &[&(now - 60_001), &job],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    assert_eq!(
        backend
            .reclaim_expired_runtime_job_leases(now - 60_000)
            .unwrap(),
        1
    );
    let recovery_owner = lease("typed-input-recovery-writer");
    claim.lease_owner = recovery_owner.clone();
    claim.claim_token = "claim-recovery".into();
    let recovered = store.claim_turn_inputs(claim.clone()).unwrap();
    assert_eq!(describe(&recovered), describe(&first));
    let mut ack = AcknowledgeTurnInputsRequest {
        agent_run_id: start.agent_run_id.clone(),
        lifecycle_job_id: job,
        session_id: start.authorization.session_id.clone(),
        authorization_digest: start.authorization_digest.clone(),
        lease_owner: recovery_owner,
        claim_token: claim.claim_token.clone(),
        input_ids: vec!["unknown-input".into()],
        acknowledged_at_ms: now,
    };
    assert!(store.acknowledge_turn_inputs(ack.clone()).is_err());
    ack.input_ids = recovered
        .iter()
        .map(|item| item.payload.input_id().to_string())
        .collect();
    store.acknowledge_turn_inputs(ack.clone()).unwrap();
    store.acknowledge_turn_inputs(ack).unwrap();
    assert!(store.claim_turn_inputs(claim).unwrap().is_empty());
    let accepting = backend
        .with_client(|client| {
            Ok(client
                .query_one(
                    "SELECT accepting FROM app_core_agentinputqueue WHERE agent_run_id=$1",
                    &[&start.agent_run_id],
                )
                .unwrap()
                .get::<_, bool>(0))
        })
        .unwrap();
    assert!(!accepting);
    println!("hosted-agent-input-store-ok");
}

fn hosted_attachment_recovery(
    backend: Arc<PostgresRuntimeStore>,
    start: &AgentRunStart,
    job: &str,
    owner: &str,
    now: i64,
    fixture: &Value,
) {
    let input_state = || {
        Arc::new(
            centaeris_core::tool::inputs::ResolvedInputState::new(
                start.agent_run_id.clone(),
                start.authorization_digest.clone(),
                start.authorization.asset_refs.clone(),
                centaeris_core::tool::inputs::ResolvedInputManifest {
                    schema: centaeris_core::tool::inputs::RESOLVED_INPUT_MANIFEST_SCHEMA.into(),
                    agent_run_id: start.agent_run_id.clone(),
                    authorization_digest: start.authorization_digest.clone(),
                    inputs: vec![],
                },
                Some(Arc::new(ApiDeferredInputResolver::new(
                    fixture["apiUrl"].as_str().unwrap().into(),
                    fixture["internalToken"].as_str().unwrap().into(),
                    start.agent_run_id.clone(),
                    start.authorization_digest.clone(),
                ))),
            )
            .unwrap(),
        )
    };
    let state = input_state();
    let _hosted = agent_inputs::HostedAgentInputStore::new(backend.clone(), start.clone())
        .with_resolved_inputs(Some(state.clone()))
        .unwrap();
    let original = serde_json::to_value(&start.authorization).unwrap();
    let expected_refs: Vec<String> =
        serde_json::from_value(fixture["expectedAttachmentRefs"].clone()).unwrap();
    let refs = |inputs: &centaeris_core::tool::inputs::ResolvedInputState| {
        let mut refs = inputs
            .declared_inputs()
            .into_iter()
            .map(|item| item.input_ref)
            .collect::<Vec<_>>();
        refs.sort();
        refs
    };
    assert_eq!(
        refs(&state),
        expected_refs,
        "all accepted captures are restored before intake"
    );
    let inputs = hosted_attachment_vision(backend.clone(), start, job, owner, &state, fixture);
    let described = inputs
        .iter()
        .map(|item| match &item.payload {
            TurnInputPayload::UserSupplement {
                supplement_id,
                message,
                attachments,
            } => json!({"inputId": supplement_id, "body": message, "attachments": attachments}),
            _ => panic!("expected only owned user input"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        json!(described),
        json!(fixture["expected"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .collect::<Vec<_>>())
    );
    let AgentRunStartInitialInput::UserInput {
        input_id, message, ..
    } = &start.initial_input
    else {
        panic!("initial user input required")
    };
    assert_eq!(
        json!({"inputId":input_id,"body":message,"attachments":contract::initial_input_attachments(start).unwrap()}),
        fixture["expected"][0]
    );
    // Startup records persist the same initial vision identity used by Core;
    // replay must have one placeholder and no invented user text.
    let mut sequence =
        AgentRunSessionState::new(&start.authorization.session_id, &start.agent_run_id).unwrap();
    let startup = crate::started_session_records(start, &mut sequence, now).unwrap();
    let mut records = startup
        .into_iter()
        .map(|item| item.event)
        .collect::<Vec<_>>();
    for input in &inputs {
        if let TurnInputPayload::UserSupplement {
            supplement_id,
            message,
            attachments,
        } = &input.payload
        {
            if let AgentRunStartInitialInput::UserInput { input_id, .. } = &start.initial_input {
                if supplement_id == input_id {
                    continue;
                }
            }
            records.push(
                centaeris_core::session::turn_supplement_record_with_attachments(
                    &start.authorization.session_id,
                    &start.turn_id,
                    &start.agent_run_id,
                    supplement_id,
                    message,
                    attachments,
                    now,
                )
                .unwrap(),
            );
        }
    }
    let restored = centaeris_core::session::restore_runtime_snapshot_from_session_records(
        &start.authorization.session_id,
        &records,
    )
    .unwrap();
    for message in &restored.messages {
        if let Some(raw) = message
            .metadata
            .get(centaeris_core::runtime::keys::metadata::MODEL_INPUT_IMAGES)
        {
            let images: Vec<centaeris_core::model::prepared_prompt::ModelInputImageRefV1> =
                serde_json::from_str(raw).unwrap();
            for image in images {
                assert_eq!(message.content.matches(&image.placeholder).count(), 1);
            }
        }
    }
    let recovered = input_state();
    agent_inputs::HostedAgentInputStore::new(backend, start.clone())
        .with_resolved_inputs(Some(recovered.clone()))
        .unwrap();
    assert_eq!(
        refs(&recovered),
        expected_refs,
        "ACKed captures remain authorized after restart"
    );
    assert_eq!(
        serde_json::to_value(&start.authorization).unwrap(),
        original
    );
    println!("hosted-agent-attachment-recovery-ok");
}

fn hosted_attachment_vision(
    backend: Arc<PostgresRuntimeStore>,
    start: &AgentRunStart,
    job: &str,
    owner: &str,
    state: &Arc<centaeris_core::tool::inputs::ResolvedInputState>,
    fixture: &Value,
) -> Vec<DurableTurnInput> {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _actor = {
        let _guard = runtime.enter();
        RuntimeStoreActor::start((*backend).clone()).unwrap()
    };
    let admitted = Arc::new(Mutex::new(Vec::new()));
    let captured = admitted.clone();
    let control = agent_inputs::turn_control(
        backend.clone(),
        start,
        DurableTurnControlBinding {
            agent_run_id: start.agent_run_id.clone(),
            lifecycle_job_id: job.into(),
            session_id: start.authorization.session_id.clone(),
            authorization_digest: start.authorization_digest.clone(),
            lease_owner: owner.into(),
            claim_token: "attachment-claim".into(),
        },
        Arc::new(move |_, items| {
            captured.lock().unwrap().extend_from_slice(items);
            Ok(())
        }),
        Some(state.clone()),
    )
    .unwrap();
    let root = std::env::temp_dir();
    let host = Arc::new(
        centaeris_core::execution::ExecutionHostBinding::new(
            centaeris_core::execution::ExecutionHostMode::Remote,
            Arc::new(InputExecutionHost),
            root.clone(),
            centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(root),
        )
        .unwrap(),
    );
    let tools = centaeris_core::tool::layer::ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        centaeris_core::extension::skills::SkillCatalogLoadConfig::default(), host).unwrap();
    let engine = AgentRuntime::new(
        (*backend).clone(),
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
        ToolConcurrencyCoordinator::new(1),
    )
    .with_model_input_image_resolver(Arc::new(
        ApiModelInputImageResolver::new(
            state.clone(),
            fixture["apiUrl"].as_str().unwrap().into(),
            fixture["internalToken"].as_str().unwrap().into(),
            start.agent_run_id.clone(),
            start.authorization_digest.clone(),
        )
        .unwrap(),
    ));
    let model = NativeIntakeModel(Mutex::new(vec![]));
    let mut uptake = Vec::new();
    let result = runtime.block_on(
        engine
            .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                AgentRunRequest {
                    session_id: start.authorization.session_id.clone(),
                    initial_turn_id: start.turn_id.clone(),
                    initial_input: crate::model_initial_input(start, state, &HashMap::new())
                        .unwrap(),
                    agent_run_identity: Some(
                        centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1 {
                            agent_run_id: start.agent_run_id.clone(),
                            execution_id: initial_execution_id(start),
                            authorization_digest: start.authorization_digest.clone(),
                        },
                    ),
                    runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(),
                    resume_from_turn_id: None,
                    auto_continue_after_resume_wait: None,
                },
                &model,
                &centaeris_core::model::EmptyModelSessionConfigStore::new(),
                &mut |_| {},
                &|| Ok(None),
                &control,
                &mut |point| {
                    if let ToolSafePoint::ModelRequestStarted(started) = point {
                        uptake.push(started.input_ids().to_vec());
                    }
                    Ok(())
                },
            ),
    );
    assert!(
        result.is_ok(),
        "Hosted image model request failed: {result:?}"
    );
    let requests = model.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        uptake,
        vec![fixture["expected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["inputId"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()]
    );
    let actual = &requests[0].prepared_prompt;
    assert_eq!(
        actual.input_images.len(),
        2,
        "initial and running attachments enter the actual main request"
    );
    for input in &actual.input_images {
        let expected = fixture["expectedImageData"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(identity, _)| input.placeholder.contains(identity.as_str()))
            .unwrap();
        assert_eq!(input.content_type, "image/png");
        assert_eq!(input.data_base64, expected.1.as_str().unwrap());
        assert_eq!(
            actual
                .messages
                .iter()
                .map(|message| message.content.matches(&input.placeholder).count())
                .sum::<usize>(),
            1
        );
    }
    println!("hosted-agent-attachment-vision-bytes-ok");
    let inputs = admitted.lock().unwrap().clone();
    inputs
}

struct NativeIntakeModel(Mutex<Vec<centaeris_core::model::ModelClientRequest>>);

impl centaeris_core::model::ModelClient for NativeIntakeModel {
    fn generate<'a>(
        &'a self,
        request: &'a centaeris_core::model::ModelClientRequest,
    ) -> centaeris_core::model::ModelClientFuture<'a, centaeris_core::model::ModelClientResponse>
    {
        Box::pin(async move {
            self.0.lock().unwrap().push(request.clone());
            Ok(centaeris_core::model::ModelClientResponse {
                generate_result: centaeris_core::model::GenerateResult {
                    content: "accepted task answer".into(),
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

fn native_main_intake(
    backend: Arc<PostgresRuntimeStore>,
    start: &AgentRunStart,
    job: &str,
    owner: &str,
    now: i64,
    fixture: &Value,
) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _actor = {
        let _guard = runtime.enter();
        RuntimeStoreActor::start((*backend).clone()).unwrap()
    };
    let log = backend
        .session_log(
            start.authorization.workspace_id.clone(),
            start.authorization.session_id.clone(),
            start.prompt.clone(),
        )
        .with_agent_run_start(start)
        .unwrap();
    let sequence = Arc::new(Mutex::new(
        AgentRunSessionState::new(&start.authorization.session_id, &start.agent_run_id).unwrap(),
    ));
    let fence = centaeris_core::session::RuntimeJobLeaseFence {
        job_id: job.into(),
        job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
        lease_owner: owner.into(),
    };
    {
        let mut guard = sequence.lock().unwrap();
        let records = crate::started_session_records(start, &mut guard, now).unwrap();
        let receipt = log
            .append_session_records_with_runtime_job_lease_blocking(
                &start.agent_run_id,
                &records,
                &fence,
            )
            .unwrap();
        crate::accept_session_commit(&mut guard, &mut None, &receipt).unwrap();
    }
    let materialized_sequence = sequence.clone();
    let materialized_log = log.clone();
    let materialized_fence = fence.clone();
    let run = start.agent_run_id.clone();
    let materialize = Arc::new(move |turn: &str, items: &[DurableTurnInput]| {
        let mut guard = materialized_sequence.lock().unwrap();
        let mut next = guard.clone();
        let mut records = Vec::new();
        for item in items {
            let record = match &item.payload {
                TurnInputPayload::UserSupplement {
                    supplement_id,
                    message,
                    ..
                } => next.supplement(turn, supplement_id, message, item.created_at_ms)?,
                TurnInputPayload::HostEvent(input) => {
                    next.host_event_input_record(turn, input, item.created_at_ms)?
                }
            };
            if let Some(record) = record {
                records.push(record);
            }
        }
        if !records.is_empty() {
            let receipt = materialized_log.append_session_records_with_runtime_job_lease_blocking(
                &run,
                &records,
                &materialized_fence,
            )?;
            crate::accept_session_commit(&mut next, &mut None, &receipt)?;
            *guard = next;
        }
        Ok(())
    });
    let control = agent_inputs::turn_control(
        backend.clone(),
        start,
        DurableTurnControlBinding {
            agent_run_id: start.agent_run_id.clone(),
            lifecycle_job_id: job.into(),
            session_id: start.authorization.session_id.clone(),
            authorization_digest: start.authorization_digest.clone(),
            lease_owner: owner.into(),
            claim_token: "native-main-claim".into(),
        },
        materialize,
        None,
    )
    .unwrap();
    let root = std::env::temp_dir();
    let host = Arc::new(
        centaeris_core::execution::ExecutionHostBinding::new(
            centaeris_core::execution::ExecutionHostMode::Remote,
            Arc::new(InputExecutionHost),
            root.clone(),
            centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(root),
        )
        .unwrap(),
    );
    let tools = centaeris_core::tool::layer::ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        centaeris_core::extension::skills::SkillCatalogLoadConfig::default(), host).unwrap();
    let engine = AgentRuntime::new(
        (*backend).clone(),
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
        ToolConcurrencyCoordinator::new(1),
    );
    let model = NativeIntakeModel(Mutex::new(Vec::new()));
    let mut requests = Vec::new();
    let result = runtime.block_on(
        engine
            .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                AgentRunRequest {
                    session_id: start.authorization.session_id.clone(),
                    initial_turn_id: start.turn_id.clone(),
                    initial_input: AgentRunInitialInput::UserMessage(start.prompt.clone()),
                    agent_run_identity: Some(
                        centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1 {
                            agent_run_id: start.agent_run_id.clone(),
                            execution_id: initial_execution_id(start),
                            authorization_digest: start.authorization_digest.clone(),
                        },
                    ),
                    runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(),
                    resume_from_turn_id: None,
                    auto_continue_after_resume_wait: None,
                },
                &model,
                &centaeris_core::model::EmptyModelSessionConfigStore::new(),
                &mut |_| {},
                &|| Ok(None),
                &control,
                &mut |point| {
                    if let ToolSafePoint::ModelRequestStarted(started) = point {
                        requests.push(started.input_ids().to_vec());
                        let mut guard = sequence.lock().unwrap();
                        let mut next = guard.clone();
                        let records =
                            next.record_model_request_started(&started, crate::now_ms()?)?;
                        let receipt = log.append_session_records_with_runtime_job_lease_blocking(
                            &start.agent_run_id,
                            &records,
                            &fence,
                        )?;
                        crate::accept_session_commit(&mut next, &mut None, &receipt)?;
                        *guard = next;
                    }
                    Ok(())
                },
            ),
    );
    assert!(result.is_ok(), "{result:?}");
    assert!(requests[0].contains(
        &fixture["expected"][0]["inputId"]
            .as_str()
            .unwrap()
            .to_owned()
    ));
    for expected in fixture["expected"].as_array().unwrap().iter().skip(1) {
        let id = expected["inputId"].as_str().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|inputs| inputs.iter().any(|input| input == id))
                .count(),
            1
        );
    }
    let flattened = requests.iter().flatten().collect::<Vec<_>>();
    let positions = fixture["expected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|expected| {
            flattened
                .iter()
                .position(|id| id.as_str() == expected["inputId"].as_str().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "user priority and native sequence reach committed uptake"
    );
    assert_eq!(model.0.lock().unwrap().len(), requests.len());
    {
        let captured = model.0.lock().unwrap();
        let last = captured.last().unwrap();
        for input in fixture["nativeInputs"].as_array().unwrap() {
            let matches = last
                .prepared_prompt
                .messages
                .iter()
                .filter(|message| message.message_id == input["messageId"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                matches.len(),
                1,
                "native message occurs once in provider context"
            );
            let (_, encoded) = matches[0].content.rsplit_once('\n').unwrap();
            assert_eq!(serde_json::from_str::<Value>(encoded).unwrap(), *input);
        }
    }
    backend.with_client(|client| {
        let acknowledged: i64 = client.query_one("SELECT COUNT(*) FROM app_core_agentworkconsumeattempt WHERE coordinator_run_id=$1 AND acknowledged_at_ms IS NOT NULL",
            &[&start.agent_run_id]).unwrap().get(0);
        assert_eq!(acknowledged, 2);
        assert!(!client.query_one("SELECT accepting FROM app_core_agentinputqueue WHERE agent_run_id=$1",
            &[&start.agent_run_id]).unwrap().get::<_, bool>(0));
        Ok(())
    }).unwrap();
    println!("hosted-native-main-intake-ok");
}

#[derive(Clone)]
struct InputApiProbe {
    client: reqwest::blocking::Client,
    url: String,
    cookie: String,
    csrf_token: String,
}

impl InputApiProbe {
    fn read(&self) -> Value {
        crate::postgres_store::run_postgres_blocking(|| {
            let response = self
                .client
                .get(&self.url)
                .header("Cookie", &self.cookie)
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            Ok(response.json::<Value>().unwrap())
        })
        .unwrap()
    }

    fn submit(&self, expected: &Value) {
        crate::postgres_store::run_postgres_blocking(|| {
            let response = self
                .client
                .post(&self.url)
                .header("Cookie", &self.cookie)
                .header("X-CSRFToken", &self.csrf_token)
                .json(
                    &json!({"schema":"agent.input.submit.v1", "attachmentRefs":[], "inputId":expected["inputId"],
                    "body":expected["body"]}),
                )
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 201);
            assert_eq!(
                response.json::<Value>().unwrap()["input"]["read"],
                Value::Null
            );
            Ok(())
        })
        .unwrap();
    }
}

struct InputModelProbe {
    api: InputApiProbe,
    expected: Vec<Value>,
    deferred_input: bool,
    requests: Mutex<Vec<centaeris_core::model::ModelClientRequest>>,
}

impl centaeris_core::model::ModelClient for InputModelProbe {
    fn generate<'a>(
        &'a self,
        request: &'a centaeris_core::model::ModelClientRequest,
    ) -> centaeris_core::model::ModelClientFuture<'a, centaeris_core::model::ModelClientResponse>
    {
        Box::pin(async move {
            let inputs = self.api.read();
            let request_index = self.requests.lock().unwrap().len();
            println!(
                "input-provider-capture {}",
                json!({
                    "requestIndex": request_index, "inputs": inputs,
                    "messages": request.prepared_prompt.messages.iter().map(|message| json!({
                        "role": format!("{:?}", message.role), "messageId": message.message_id,
                        "content": message.content,
                    })).collect::<Vec<_>>(),
                })
            );
            for (index, expected) in self.expected.iter().enumerate() {
                let row = inputs["inputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["inputId"] == expected["inputId"])
                    .unwrap();
                let deferred = self.deferred_input && request_index == 0 && index == 2;
                assert_eq!(
                    row["read"].is_null(),
                    deferred,
                    "only the committed prepared batch has Read before provider dispatch"
                );
                let body = expected["body"].as_str().unwrap();
                let occurrences = request
                    .prepared_prompt
                    .messages
                    .iter()
                    .map(|message| message.content.matches(body).count())
                    .sum::<usize>();
                assert_eq!(
                    occurrences,
                    usize::from(!deferred),
                    "an original body occurs once after recovery: inputId={} messages={:?}",
                    expected["inputId"],
                    request.prepared_prompt.messages
                );
            }
            self.requests.lock().unwrap().push(request.clone());
            Ok(centaeris_core::model::ModelClientResponse {
                generate_result: centaeris_core::model::GenerateResult {
                    content: "completed synthetic input processing".into(),
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

struct InputExecutionHost;
impl centaeris_core::execution::ExecutionHostRunner for InputExecutionHost {
    fn status(
        &self,
        _: &centaeris_core::execution::ExecutionPolicy,
    ) -> Result<
        centaeris_core::execution::ExecutionHostStatus,
        centaeris_core::execution::ExecutionError,
    > {
        Err(centaeris_core::execution::ExecutionError::HostUnavailable {
            reason: "synthetic input host".into(),
        })
    }
    fn run_file_system_operation(
        &self,
        _: centaeris_core::execution::ExecutionFileSystemRequest,
    ) -> Result<
        centaeris_core::execution::ExecutionFileSystemOutput,
        centaeris_core::execution::ExecutionFileSystemError,
    > {
        Err(centaeris_core::execution::ExecutionFileSystemError::new(
            centaeris_core::execution::ExecutionFileSystemErrorKind::NotFound,
            "no synthetic instruction file",
        ))
    }
    fn run_host_command(
        &self,
        _: Option<&str>,
        _: centaeris_core::execution::ExecutionCommandRequest,
        _: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
    ) -> Result<
        centaeris_core::execution::ExecutionHostCommandOutput,
        centaeris_core::execution::ExecutionError,
    > {
        panic!("input fixture must never execute a host command")
    }
}

#[test]
#[ignore = "requires the owned API live server and migrated disposable PostgreSQL database"]
fn hosted_input_main_request_recovery() {
    run_hosted_input_recovery(true, false);
}

#[test]
#[ignore = "requires the owned API live server and migrated disposable PostgreSQL database"]
fn hosted_input_checkpoint_recovery() {
    run_hosted_input_recovery(true, true);
}

#[test]
#[ignore = "requires the owned API live server and migrated disposable PostgreSQL database"]
fn hosted_input_ack_recovery() {
    run_hosted_input_recovery(false, false);
}

fn run_hosted_input_recovery(inject_failed_commit: bool, replace_execution: bool) {
    use centaeris_core::runtime::{
        AgentRunInitialInput, AgentRunRequest, AgentRunStop, AgentRuntime, AgentRuntimeConfig,
        DurableTurnControlBinding, ToolConcurrencyCoordinator, ToolSafePoint,
    };
    use centaeris_core::session::{AgentRunSessionState, RuntimeJobLeaseFence};
    let fixture: Value =
        serde_json::from_str(&env::var("CENTAERIS_AGENT_INPUT_FIXTURE").unwrap()).unwrap();
    let start = serde_json::from_value::<AgentRunStart>(fixture["agentRunStart"].clone())
        .unwrap()
        .validate(fixture["signingKey"].as_str().unwrap().as_bytes())
        .unwrap();
    let backend = PostgresRuntimeStore::new(fixture["databaseUrl"].as_str().unwrap()).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let actor = {
        let _guard = runtime.enter();
        RuntimeStoreActor::start(backend.clone()).unwrap()
    };
    let job = agent_run_lifecycle_job_id(&start.agent_run_id).unwrap();
    let now = now_ms().unwrap();
    backend
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: job.clone(),
                job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
                status: RuntimeJobStatus::Queued,
                run_at_ms: now,
                lease_owner: None,
                lease_expires_at_ms: None,
                heartbeat_at_ms: None,
                retry_count: 0,
                max_retries: 10,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: format!("{job}:{}", start.authorization_digest),
                session_id: Some(start.authorization.session_id.clone()),
                branch_id: None,
                checkpoint_id: None,
                payload_ref: Some(format!("record:agent_run:{}", start.agent_run_id)),
                output_refs: vec![],
                last_error: None,
                created_at_ms: now,
                updated_at_ms: now,
            },
        })
        .unwrap();
    let api = InputApiProbe {
        client: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap(),
        url: fixture["apiUrl"].as_str().unwrap().into(),
        cookie: fixture["cookie"].as_str().unwrap().into(),
        csrf_token: fixture["csrfToken"].as_str().unwrap().into(),
    };
    let model = InputModelProbe {
        api: api.clone(),
        expected: fixture["expected"].as_array().unwrap().clone(),
        deferred_input: inject_failed_commit,
        requests: Mutex::new(vec![]),
    };
    let log = backend
        .session_log(
            start.authorization.workspace_id.clone(),
            start.authorization.session_id.clone(),
            start.prompt.clone(),
        )
        .with_agent_run_start(&start)
        .unwrap();
    let sequence = Arc::new(Mutex::new(
        AgentRunSessionState::new(&start.authorization.session_id, &start.agent_run_id).unwrap(),
    ));
    let mut original_read = None;
    let mut execution_id = initial_execution_id(&start);
    let mut restored_checkpoints = Vec::new();
    if !inject_failed_commit {
        api.submit(&fixture["expected"][2]);
    }
    for attempt in 0..if inject_failed_commit { 3 } else { 2 } {
        let failed_commit = inject_failed_commit && attempt == 0;
        let failed_ack = attempt == if inject_failed_commit { 1 } else { 0 };
        let injected_failure = failed_commit || failed_ack;
        let now = now_ms().unwrap();
        if attempt != 0 {
            backend
                .with_client(|client| {
                    client
                        .execute(
                            "UPDATE runtime_jobs SET lease_expires_at_ms=$1 WHERE job_id=$2",
                            &[&(now - 60_001), &job],
                        )
                        .unwrap();
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                backend
                    .reclaim_expired_runtime_job_leases(now - 60_000)
                    .unwrap(),
                1
            );
        }
        let owner = backend
            .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: now,
                worker_id: format!("input-writer-{attempt}"),
                job_id: Some(job.clone()),
                job_kind: None,
                session_id: None,
                limit: 1,
                lease_ms: 600000,
            })
            .unwrap()
            .remove(0)
            .lease_owner
            .unwrap();
        backend
            .start_runtime_job(StartRuntimeJobRequest {
                job_id: job.clone(),
                lease_owner: owner.clone(),
                started_at_ms: now,
            })
            .unwrap();
        let fence = RuntimeJobLeaseFence {
            job_id: job.clone(),
            job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
            lease_owner: owner.clone(),
        };
        if attempt == 0 {
            let mut guard = sequence.lock().unwrap();
            let mut records = crate::started_session_records(&start, &mut guard, now).unwrap();
            records.push(
                guard
                    .start_execution(
                        &start.turn_id,
                        &execution_id,
                        &start.authorization_digest,
                        None,
                        now,
                    )
                    .unwrap(),
            );
            let receipt =
                crate::commit_agent_tool_records(&log, &start, &records, None, &fence, None)
                    .unwrap();
            crate::accept_session_commit(&mut guard, &mut None, &receipt).unwrap();
        } else {
            let restored = crate::load_existing_session_sequence(&backend, &start).unwrap();
            *sequence.lock().unwrap() = restored;
            let checkpoint = crate::select_agent_run_recovery_checkpoint(
                &backend,
                &start,
                &sequence.lock().unwrap(),
            )
            .unwrap();
            assert_eq!(checkpoint.is_some(), replace_execution && attempt == 2);
            if let Some((record, payload)) = checkpoint {
                crate::restore_runtime_state_from_recovery_checkpoint(&backend, &actor, &payload)
                    .unwrap();
                restored_checkpoints.push(payload.checkpoint_id.clone());
                execution_id = replacement_execution_id(&start, &record.checkpoint_id, 1);
                let mut guard = sequence.lock().unwrap();
                let event = guard
                    .start_execution(
                        &start.turn_id,
                        &execution_id,
                        &start.authorization_digest,
                        Some(&record.checkpoint_id),
                        now,
                    )
                    .unwrap();
                let receipt =
                    crate::commit_agent_tool_records(&log, &start, &[event], None, &fence, None)
                        .unwrap();
                crate::accept_session_commit(&mut guard, &mut None, &receipt).unwrap();
            } else {
                assert_eq!(
                    sequence.lock().unwrap().active_execution_id(),
                    Some(execution_id.as_str())
                );
            }
        }
        println!(
            "input-recovery-selection {}",
            json!({"attempt": attempt,
            "executionId": execution_id, "restoredCheckpoints": restored_checkpoints,
            "initialTurnId": start.turn_id, "agentRunId": start.agent_run_id})
        );
        let materialized_sequence = sequence.clone();
        let materialized_log = log.clone();
        let materialized_start = start.clone();
        let materialized_fence = fence.clone();
        let materialize = Arc::new(move |turn: &str, supplements: &[DurableTurnInput]| {
            let mut guard = materialized_sequence.lock().unwrap();
            let mut next = guard.clone();
            let mut records = vec![];
            for item in supplements {
                let record = match &item.payload {
                    TurnInputPayload::UserSupplement {
                        supplement_id,
                        message,
                        ..
                    } => next.supplement(turn, supplement_id, message, item.created_at_ms)?,
                    TurnInputPayload::HostEvent(input) => {
                        next.host_event_input_record(turn, input, item.created_at_ms)?
                    }
                };
                if let Some(record) = record {
                    records.push(record);
                }
            }
            if !records.is_empty() {
                let receipt = materialized_log
                    .append_session_records_with_runtime_job_lease_blocking(
                        &materialized_start.agent_run_id,
                        &records,
                        &materialized_fence,
                    )?;
                crate::accept_session_commit(&mut next, &mut None, &receipt)?;
                *guard = next;
            }
            Ok(())
        });
        let control = agent_inputs::turn_control(
            Arc::new(backend.clone()),
            &start,
            DurableTurnControlBinding {
                agent_run_id: start.agent_run_id.clone(),
                lifecycle_job_id: job.clone(),
                session_id: start.authorization.session_id.clone(),
                authorization_digest: start.authorization_digest.clone(),
                lease_owner: owner,
                claim_token: format!("input-main-claim-{attempt}"),
            },
            materialize,
            None,
        )
        .unwrap();
        if injected_failure {
            backend.with_client(|client| {
                client.batch_execute(if failed_commit {
                    "CREATE FUNCTION public.reject_input_commit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.payload->>'type'='model_request_started' THEN RAISE EXCEPTION 'injected_main_commit_failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_input_commit AFTER INSERT ON public.app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION public.reject_input_commit();"
                } else {
                    "CREATE FUNCTION public.reject_input_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.acknowledged_at_ms IS NOT NULL THEN RAISE EXCEPTION 'injected_input_ack_failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_input_ack AFTER UPDATE ON public.app_core_agentinputdelivery FOR EACH ROW EXECUTE FUNCTION public.reject_input_ack();"
                }).unwrap(); Ok(())
            }).unwrap();
        }
        let root = std::env::temp_dir();
        let host = Arc::new(
            centaeris_core::execution::ExecutionHostBinding::new(
                centaeris_core::execution::ExecutionHostMode::Remote,
                Arc::new(InputExecutionHost),
                root.clone(),
                centaeris_core::execution::ExecutionPolicy::workspace_write_no_network(root),
            )
            .unwrap(),
        );
        let tools = centaeris_core::tool::layer::ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
            centaeris_core::extension::skills::SkillCatalogLoadConfig::default(), host).unwrap();
        let engine = AgentRuntime::new(
            backend.clone(),
            tools,
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                ..Default::default()
            },
            ToolConcurrencyCoordinator::new(1),
        );
        let AgentRunStartInitialInput::UserInput {
            input_id, message, ..
        } = &start.initial_input
        else {
            panic!("identified initial required")
        };
        let mut main_attempts = 0;
        let result = runtime.block_on(
            engine.process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                AgentRunRequest { session_id: start.authorization.session_id.clone(), initial_turn_id: start.turn_id.clone(),
                    initial_input: AgentRunInitialInput::UserInput { input_id: input_id.clone(), message: message.clone(), attachments: contract::initial_input_attachments(&start).unwrap() },
                    agent_run_identity: Some(centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1 {
                        agent_run_id: start.agent_run_id.clone(), execution_id: execution_id.clone(), authorization_digest: start.authorization_digest.clone() }),
                    runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(), resume_from_turn_id: None, auto_continue_after_resume_wait: None },
                &model, &centaeris_core::model::EmptyModelSessionConfigStore::new(), &mut |_| {}, &|| Ok(None), &control,
                &mut |point| {
                    if let ToolSafePoint::ModelRequestStarted(started) = point {
                        main_attempts += 1;
                        let mut guard = sequence.lock().unwrap();
                        let mut next = guard.clone();
                        let now = crate::now_ms()?;
                        let mut records = next.record_model_request_started(&started, now)?;
                        let main = records.iter().rev().find(|record|
                            record.event.event_type == SessionRecordType::ModelRequestStarted).unwrap();
                        let request_id = main.event.payload["requestId"].as_str().unwrap();
                        let checkpoint_id = recovery_checkpoint_id(&execution_id, request_id);
                        let payload = RuntimeRecoveryCheckpointV1 {
                            schema: RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1.into(),
                            checkpoint_id: checkpoint_id.clone(), session_id: start.authorization.session_id.clone(),
                            agent_run_id: start.agent_run_id.clone(), execution_id: execution_id.clone(),
                            authorization_digest: start.authorization_digest.clone(),
                            session_sequence: next.committed_session_sequence() + records.len() as u64 + 1,
                            model_request_id: request_id.into(),
                            workspace_snapshot: recovery_snapshot_from_session_workspace(&start.authorization.session_workspace)?,
                            workspace_generation: ExecutionWorkspaceGeneration::Known {
                                token: ExecutionWorkspaceGenerationV1 { instance_epoch: "input-fixture-host".into(), generation: 1 } },
                            created_at_ms: now,
                        };
                        payload.validate()?;
                        let checkpoint = CheckpointRecord { checkpoint_id, kind: CheckpointKindV1::Recovery,
                            session_id: start.authorization.session_id.clone(), turn_id: main.event.turn_id.clone().unwrap(),
                            status: "committed".into(), done_reason: None, updated_at_ms: now,
                            payload_json: serde_json::to_string(&payload).unwrap() };
                        records.push(next.checkpoint_ref(&checkpoint)?);
                        let receipt = crate::commit_agent_tool_records(&log, &start, &records, Some(&checkpoint), &fence, None)?;
                        crate::accept_session_commit(&mut next, &mut None, &receipt)?;
                        println!("input-main-commit {}", json!({"attempt": attempt,
                            "checkpoint": payload, "records": records.iter().map(|record| json!({
                                "turnId": record.event.turn_id, "payload": record.event.payload,
                                "type": format!("{:?}", record.event.event_type) })).collect::<Vec<_>>() }));
                        *guard = next;
                    }
                    Ok(())
                }));
        if injected_failure {
            assert_eq!(
                main_attempts, 1,
                "fault injection reaches the real main commit"
            );
            eprintln!(
                "input recovery attempt {attempt}: {:?}",
                result.as_ref().err()
            );
            assert!(result.is_err());
            assert!(model.requests.lock().unwrap().is_empty());
            backend.with_client(|client| {
                client.batch_execute(if failed_commit {
                    "DROP TRIGGER reject_input_commit ON public.app_core_sessionevent; DROP FUNCTION public.reject_input_commit();"
                } else {
                    "DROP TRIGGER reject_input_ack ON public.app_core_agentinputdelivery; DROP FUNCTION public.reject_input_ack();"
                }).unwrap(); Ok(())
            }).unwrap();
            let rows = api.read();
            if failed_commit {
                assert!(rows["inputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["read"].is_null()));
                api.submit(&fixture["expected"][2]);
                assert!(crate::latest_recovery_checkpoint(
                    &backend,
                    &start,
                    &sequence.lock().unwrap(),
                    false
                )
                .unwrap()
                .is_none());
            } else {
                let references: Vec<_> = rows["inputs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["read"].clone())
                    .collect();
                assert!(references[0].is_object());
                assert!(references
                    .iter()
                    .take(if inject_failed_commit { 2 } else { 3 })
                    .all(|reference| reference == &references[0]));
                if inject_failed_commit {
                    assert!(references[2].is_null());
                }
                original_read = Some(references[0].clone());
                if replace_execution {
                    let mut guard = sequence.lock().unwrap();
                    let (checkpoint, _) =
                        crate::latest_recovery_checkpoint(&backend, &start, &guard, false)
                            .unwrap()
                            .unwrap();
                    let event = guard
                        .end_execution(
                            &start.turn_id,
                            &execution_id,
                            "lost",
                            "execution_environment_lost",
                            true,
                            Some(&checkpoint.checkpoint_id),
                            vec![],
                            now_ms().unwrap(),
                        )
                        .unwrap();
                    let receipt = crate::commit_agent_tool_records(
                        &log,
                        &start,
                        &[event],
                        None,
                        &fence,
                        None,
                    )
                    .unwrap();
                    crate::accept_session_commit(&mut guard, &mut None, &receipt).unwrap();
                }
            }
        } else {
            assert_eq!(result.unwrap().stop, AgentRunStop::Finalized);
        }
    }
    assert_eq!(
        model.requests.lock().unwrap().len(),
        if inject_failed_commit { 2 } else { 1 }
    );
    assert_eq!(restored_checkpoints.len(), usize::from(replace_execution));
    for (index, row) in api.read()["inputs"].as_array().unwrap().iter().enumerate() {
        if inject_failed_commit && index == 2 {
            assert!(row["read"].is_object());
            assert_ne!(&row["read"], original_read.as_ref().unwrap());
        } else {
            assert_eq!(&row["read"], original_read.as_ref().unwrap());
        }
    }
    backend.with_client(|client| {
        let rows = client.query("SELECT i.body,d.acknowledged_at_ms FROM app_core_agentinput i JOIN app_core_agentinputdelivery d ON d.input_id=i.id WHERE d.queue_id=$1 ORDER BY i.sequence", &[&start.agent_run_id]).unwrap();
        assert_eq!(rows.len(), 3);
        for (row, expected) in rows.iter().zip(model.expected.iter()) {
            assert_eq!(row.get::<_, String>(0), expected["body"].as_str().unwrap());
            assert!(row.get::<_, Option<i64>>(1).is_some());
        }
        Ok(())
    }).unwrap();
    println!("hosted-input-main-request-recovery-ok");
}
