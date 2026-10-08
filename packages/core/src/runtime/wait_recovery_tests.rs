use super::*;
use crate::runtime::subagent::{
    build_subagent_run_job, SubagentRunJobRequest, SubagentSchedulerEventKind,
};

fn wait_recovery_fixture() -> (
    AgentRuntimeTestStore,
    SessionStateSnapshot,
    RuntimeJobRecord,
) {
    let store = AgentRuntimeTestStore::new();
    let mut sealed = SessionStateSnapshot::new("wait-parent".into(), 10);
    sealed
        .metadata
        .insert("opaqueWaitState".into(), "unchanged".into());
    SessionManager::new(store.clone())
        .save_session(&sealed)
        .unwrap();
    let mut first = build_subagent_run_job(SubagentRunJobRequest {
        session_id: sealed.session_id.clone(),
        parent_turn_id: "wait-turn".into(),
        tool_call_id: "call-a".into(),
        subagent_id: "child-a".into(),
        work_packet_ref: "work-a".into(),
        checkpoint_id: None,
        run_at_ms: 1,
        created_at_ms: 1,
        max_retries: 0,
    });
    first.status = RuntimeJobStatus::Succeeded;
    first.output_refs = vec!["external_context:result-a".into()];
    first.updated_at_ms = 20;
    store
        .state()
        .unwrap()
        .jobs
        .insert(first.job_id.clone(), first.clone());
    let second = build_subagent_run_job(SubagentRunJobRequest {
        session_id: sealed.session_id.clone(),
        parent_turn_id: "wait-turn".into(),
        tool_call_id: "call-b".into(),
        subagent_id: "child-b".into(),
        work_packet_ref: "work-b".into(),
        checkpoint_id: None,
        run_at_ms: 2,
        created_at_ms: 2,
        max_retries: 0,
    });
    store
        .state()
        .unwrap()
        .jobs
        .insert(second.job_id.clone(), second);
    (store, sealed, first)
}

fn publish_wait_child(store: &AgentRuntimeTestStore, job: &RuntimeJobRecord) {
    let event = super::super::subagent::scheduler_event_from_job(
        job,
        SubagentSchedulerEventKind::Succeeded,
        SubagentLifecycleStatus::Succeeded,
        None,
        "actual bounded scheduler summary",
        job.updated_at_ms,
    )
    .unwrap();
    persist_subagent_result_projection_from_scheduler_events(store, "wait-parent", &[event])
        .unwrap();
}

#[test]
fn wait_recovery_replay_accepts_verified_child_a_while_child_b_waits() {
    let (store, sealed, first) = wait_recovery_fixture();
    publish_wait_child(&store, &first);
    let current = SessionManager::new(store.clone())
        .load_session("wait-parent")
        .unwrap()
        .unwrap();
    validate_wait_recovery_replay(&store, &sealed, &current).unwrap();
    assert_eq!(current.updated_at_ms, sealed.updated_at_ms);
}

#[test]
fn wait_recovery_reconstructs_arrived_result_from_durable_job_without_cache() {
    let (store, sealed, _) = wait_recovery_fixture();
    let restored = reconstruct_wait_recovery_snapshot(&store, &sealed).unwrap();
    let value = serde_json::to_value(&restored).unwrap();
    let projection = value["metadata"][runtime_metadata_keys::SUBAGENT_RESULT_PROJECTION]
        .as_str()
        .unwrap();
    let projection: Value = serde_json::from_str(projection).unwrap();
    assert_eq!(projection["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        projection["items"][0]["resultRef"],
        "external_context:result-a"
    );
    assert_eq!(restored.metadata["opaqueWaitState"], "unchanged");
}

#[test]
fn wait_recovery_replay_rejects_nonterminal_foreign_or_forged_child_refs() {
    let (store, sealed, first) = wait_recovery_fixture();
    publish_wait_child(&store, &first);
    let current = SessionManager::new(store.clone())
        .load_session("wait-parent")
        .unwrap()
        .unwrap();
    let key = runtime_metadata_keys::SUBAGENT_RESULT_PROJECTION;
    for field in [
        "resultRef",
        "childSessionRef",
        "workPacketRef",
        "parentTurnId",
    ] {
        let mut forged = current.clone();
        let mut projection: Value = serde_json::from_str(&forged.metadata[key]).unwrap();
        projection["items"][0][field] = Value::String("foreign".into());
        forged
            .metadata
            .insert(key.into(), serde_json::to_string(&projection).unwrap());
        assert!(
            validate_wait_recovery_replay(&store, &sealed, &forged).is_err(),
            "{field}"
        );
    }
    store
        .state()
        .unwrap()
        .jobs
        .get_mut(&first.job_id)
        .unwrap()
        .status = RuntimeJobStatus::Queued;
    assert!(validate_wait_recovery_replay(&store, &sealed, &current).is_err());
}

#[test]
fn wait_recovery_replay_still_protects_other_core_state() {
    let (store, sealed, first) = wait_recovery_fixture();
    publish_wait_child(&store, &first);
    let mut current = SessionManager::new(store.clone())
        .load_session("wait-parent")
        .unwrap()
        .unwrap();
    current
        .metadata
        .insert("opaqueWaitState".into(), "changed".into());
    assert!(validate_wait_recovery_replay(&store, &sealed, &current).is_err());
    current
        .metadata
        .insert("opaqueWaitState".into(), "unchanged".into());
    current.updated_at_ms += 1;
    assert!(validate_wait_recovery_replay(&store, &sealed, &current).is_err());
}

fn inject_child_b_on_next_snapshot_write(
    store: &AgentRuntimeTestStore,
    sealed: &SessionStateSnapshot,
) {
    let mut second = store
        .state()
        .unwrap()
        .jobs
        .values()
        .find(|job| job.status == RuntimeJobStatus::Queued)
        .unwrap()
        .clone();
    second.status = RuntimeJobStatus::Succeeded;
    second.output_refs = vec!["external_context:result-b".into()];
    second.updated_at_ms = 30;
    store
        .state()
        .unwrap()
        .jobs
        .insert(second.job_id.clone(), second.clone());
    publish_wait_child(store, &second);
    let concurrent = store
        .load_agent_runtime_snapshot("wait-parent")
        .unwrap()
        .unwrap();
    let mut state = store.state().unwrap();
    state.jobs.get_mut(&second.job_id).unwrap().status = RuntimeJobStatus::Queued;
    state
        .snapshots
        .insert("wait-parent".into(), serde_json::to_string(sealed).unwrap());
    state.snapshot_cas_conflict = Some(("wait-parent".into(), concurrent, second));
}

fn assert_both_child_refs(snapshot: &SessionStateSnapshot) {
    let projection: Value =
        serde_json::from_str(&snapshot.metadata[runtime_metadata_keys::SUBAGENT_RESULT_PROJECTION])
            .unwrap();
    let refs: Vec<_> = projection["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["resultRef"].as_str().unwrap())
        .collect();
    assert_eq!(refs.len(), 2);
    assert!(refs.contains(&"external_context:result-a"));
    assert!(refs.contains(&"external_context:result-b"));
}

#[test]
fn wait_recovery_restore_retries_concurrent_child_arrival_without_losing_either_result() {
    let (store, sealed, _) = wait_recovery_fixture();
    inject_child_b_on_next_snapshot_write(&store, &sealed);
    let restored = restore_wait_recovery_snapshot(&store, &sealed).unwrap();
    assert_both_child_refs(&restored);
    let retained = SessionManager::new(store.clone())
        .load_session("wait-parent")
        .unwrap()
        .unwrap();
    assert_both_child_refs(&retained);
}

#[test]
fn wait_recovery_projection_writer_retries_concurrent_child_arrival() {
    let (store, sealed, first) = wait_recovery_fixture();
    inject_child_b_on_next_snapshot_write(&store, &sealed);
    publish_wait_child(&store, &first);
    let retained = SessionManager::new(store.clone())
        .load_session("wait-parent")
        .unwrap()
        .unwrap();
    assert_both_child_refs(&retained);
}

#[test]
fn wait_recovery_restore_rejects_concurrent_semantic_progress_without_overwriting_it() {
    let (store, sealed, first) = wait_recovery_fixture();
    let mut progressed = sealed.clone();
    progressed
        .metadata
        .insert("opaqueWaitState".into(), "progressed".into());
    store.state().unwrap().snapshot_cas_conflict = Some((
        "wait-parent".into(),
        serde_json::to_string(&progressed).unwrap(),
        first,
    ));
    assert!(restore_wait_recovery_snapshot(&store, &sealed).is_err());
    assert_eq!(
        store
            .load_agent_runtime_snapshot("wait-parent")
            .unwrap()
            .unwrap(),
        serde_json::to_string(&progressed).unwrap()
    );
}

#[test]
fn wait_recovery_restore_can_roll_back_initial_speculative_state() {
    let (store, sealed, _) = wait_recovery_fixture();
    let mut speculative = sealed.clone();
    speculative
        .metadata
        .insert("opaqueWaitState".into(), "speculative".into());
    SessionManager::new(store.clone())
        .save_session(&speculative)
        .unwrap();
    let restored = restore_wait_recovery_snapshot(&store, &sealed).unwrap();
    assert_eq!(restored.metadata["opaqueWaitState"], "unchanged");
    assert!(restored
        .metadata
        .contains_key(runtime_metadata_keys::SUBAGENT_RESULT_PROJECTION));
}

#[test]
fn wait_recovery_replay_rejects_unbounded_derived_display_content() {
    let (store, sealed, first) = wait_recovery_fixture();
    publish_wait_child(&store, &first);
    let mut current = SessionManager::new(store.clone())
        .load_session("wait-parent")
        .unwrap()
        .unwrap();
    let key = runtime_metadata_keys::SUBAGENT_RESULT_PROJECTION;
    let mut projection: Value = serde_json::from_str(&current.metadata[key]).unwrap();
    projection["items"][0]["boundedSummary"] = Value::String("x".repeat(1_204));
    current
        .metadata
        .insert(key.into(), serde_json::to_string(&projection).unwrap());
    assert!(validate_wait_recovery_replay(&store, &sealed, &current).is_err());
}

#[test]
fn wait_recovery_replay_accepts_the_producers_truncated_display_content() {
    let (store, sealed, first) = wait_recovery_fixture();
    let mut event = super::super::subagent::scheduler_event_from_job(
        &first,
        SubagentSchedulerEventKind::Succeeded,
        SubagentLifecycleStatus::Succeeded,
        None,
        &"summary".repeat(300),
        first.updated_at_ms,
    )
    .unwrap();
    event.description = Some("title".repeat(200));
    persist_subagent_result_projection_from_scheduler_events(&store, "wait-parent", &[event])
        .unwrap();
    let current = SessionManager::new(store.clone())
        .load_session("wait-parent")
        .unwrap()
        .unwrap();
    validate_wait_recovery_replay(&store, &sealed, &current).unwrap();
}

struct TwoRuntimeWaitClient(RuntimeJobWaitModelClient);

struct TwoPendingRuntimeJobs(PendingRuntimeJobProvider);

impl crate::tool::layer::DynamicToolProvider for TwoPendingRuntimeJobs {
    fn provider_id(&self) -> &str {
        "test.runtime_wait"
    }
    fn execute<'a>(
        &'a self,
        request: crate::tool::layer::DynamicToolProviderRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::tool::layer::DynamicToolProviderResponse, String>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let ticket = request.tool_call_id.clone();
            let mut response = self.0.execute(request).await?;
            response.details["providerPolling"]["pollKey"] = Value::String(ticket.clone());
            response.details["providerPolling"]["pollArgs"] = json!({"ticket": ticket});
            Ok(response)
        })
    }
}

impl ModelClient for TwoRuntimeWaitClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let mut response = self.0.generate(request).await?;
            if let Some(first) = response.generate_result.tool_calls.first().cloned() {
                let mut second = first;
                second.id = "call-runtime-wait-b".into();
                response.generate_result.tool_calls.push(second);
            }
            Ok(response)
        })
    }
}

#[tokio::test]
async fn query_loop_wait_recovery_two_runtime_results_progress_once_after_both_complete() {
    let store = AgentRuntimeTestStore::new();
    let executions = Arc::new(AtomicUsize::new(0));
    let registry = crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
        name: "runtime_wait_test_tool".into(), category: "test".into(), summary: "Wait for two distinct jobs".into(),
        input_schema: json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false}),
        provider_id: "test.runtime_wait".into(), scopes: Vec::new(), concurrency_safe: true,
        turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
    }]).unwrap();
    let mut tools = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
    tools
        .register_dynamic_tool_provider(Arc::new(TwoPendingRuntimeJobs(
            PendingRuntimeJobProvider {
                execution_count: executions.clone(),
            },
        )))
        .unwrap();
    let engine =
        AgentRuntime::new_for_test_with_tools(store.clone(), tools, AgentRuntimeConfig::default());
    let model = TwoRuntimeWaitClient(RuntimeJobWaitModelClient {
        request_count: AtomicUsize::new(0),
        follow_up_tool_result_count: AtomicUsize::new(0),
        expected_tool_result_fragment: "durable background evidence",
        expect_toolless_follow_up: false,
    });
    let config = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "wait-two-run".into(),
        execution_id: "wait-two-execution".into(),
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
    };
    let request = |resume| AgentRunRequest {
        session_id: "chat-runtime-wait".into(),
        agent_run_identity: Some(identity.clone()),
        initial_turn_id: "turn-runtime-wait".into(),
        initial_input: AgentRunInitialInput::UserMessage("Wait for both results".into()),
        runtime_scope: PromptCompactionScopeV1::main(),
        resume_from_turn_id: resume,
        auto_continue_after_resume_wait: None,
    };
    let first = engine
        .process_turn_loop_online_with_model_client_async(request(None), &model, &config)
        .await
        .unwrap();
    assert_eq!(first.stop, AgentRunStop::RuntimeJobWait);
    let checkpoint = first.turn_responses[0].checkpoint.as_ref().unwrap();
    let waits: RuntimeAwaitJobCheckpointV1 =
        serde_json::from_str(&checkpoint.payload_json).unwrap();
    assert_eq!(waits.waits.len(), 2);
    complete_runtime_wait_test_job(&store, waits.waits[0].job_id.clone(), "chat-runtime-wait");
    let partial = engine
        .resume_turn_with_agent_run_identity_async(
            "chat-runtime-wait",
            "turn-runtime-wait",
            Some(&identity),
        )
        .await
        .unwrap();
    assert_eq!(partial.continuation, QueryContinuation::AwaitRuntimeJob);
    assert!(partial.tool_results.is_empty());
    assert_eq!(model.0.request_count.load(Ordering::SeqCst), 1);
    complete_runtime_wait_test_job(&store, waits.waits[1].job_id.clone(), "chat-runtime-wait");
    store
        .link_external_context_object(ExternalContextObjectLink {
            session_id: "chat-runtime-wait".into(),
            turn_id: Some("turn-runtime-wait".into()),
            tool_call_id: Some("call-runtime-wait-b".into()),
            object_id: "runtime-wait-object".into(),
            source_provider_id: "test.runtime_wait".into(),
            source_tool_name: "runtime_wait_test_tool".into(),
            linked_at_ms: now_ms(),
        })
        .unwrap();
    let final_result = engine
        .process_turn_loop_online_with_model_client_async(
            request(Some("turn-runtime-wait".into())),
            &model,
            &config,
        )
        .await
        .unwrap();
    assert_eq!(final_result.stop, AgentRunStop::Finalized);
    assert_eq!(
        model.0.follow_up_tool_result_count.load(Ordering::SeqCst),
        2
    );
    assert_eq!(model.0.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert!(engine
        .resume_turn_with_agent_run_identity_async(
            "chat-runtime-wait",
            "turn-runtime-wait",
            Some(&identity)
        )
        .await
        .is_err());
    let snapshot = SessionManager::new(store)
        .load_session("chat-runtime-wait")
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .count(),
        2
    );
}
