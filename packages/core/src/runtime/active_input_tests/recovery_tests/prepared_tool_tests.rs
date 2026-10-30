use super::*;

struct TwoToolModel {
    calls: AtomicUsize,
    bash: bool,
    parallel: bool,
}
impl ModelClient for TwoToolModel {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut tool_calls=vec![crate::model::ToolCallEnvelope {
                id:format!("prepared-call-{index}"),name:if self.bash {"bash"} else {"stream_boundary_test_tool"}.into(),args_json:if self.bash {json!({"command":format!("printf 'effect-{index}\n' >> effects; printf 'result-{index}'")})} else {json!({"value":format!("effect-{index}")})}.to_string()
            }];
            if self.parallel && index == 0 {
                let mut second = tool_calls[0].clone();
                second.id = "prepared-call-1".into();
                second.args_json = json!({"value":"effect-1"}).to_string();
                tool_calls.push(second);
            }
            Ok(ModelClientResponse {
                request_timings: None,
                generate_result: GenerateResult {
                    content: String::new(),
                    tool_calls,
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
async fn query_loop_prepared_empty_input_recovers_original_receipt_before_model_or_tools() {
    assert_prepared_receipt_recovery(1, "ok").await;
}

#[tokio::test]
async fn query_loop_prepared_nonempty_input_recovers_original_receipt_before_model_or_tools() {
    assert_prepared_receipt_recovery(0, "ok").await;
}

async fn assert_prepared_receipt_recovery(pending_index: usize, mode: &str) {
    let queue = Arc::new(InputQueue::default());
    queue.add(
        TurnInputPayload::UserSupplement {
            supplement_id: "initial-user".into(),
            message: "accepted objective".into(),
            attachments: vec![],
        },
        1,
    );
    if mode == "second-crash" {
        queue.add(
            TurnInputPayload::UserSupplement {
                supplement_id: "supplement-user".into(),
                message: "accepted supplement".into(),
                attachments: vec![],
            },
            2,
        );
    }
    let journal = Arc::new(RecoveryJournal::default());
    journal.records.lock().unwrap().extend(
        crate::session::started_agent_run_records(
            "active-input",
            "input-turn",
            "input-run",
            "accepted objective",
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
    let host_mode = mode.starts_with("host-");
    let workspace = temp_dir_path("prepared-command-recovery");
    if host_mode {
        std::fs::create_dir_all(&workspace).unwrap();
    }
    let host = Arc::new(QueryingHost {
        executions: executions.clone(),
        outputs: Mutex::new(HashMap::new()),
        queries: AtomicUsize::new(0),
        unknown: mode == "host-unknown",
        output_gap: mode == "host-gap",
    });
    let layer = || {
        if host_mode {
            recovery_host_layer(&workspace, host.clone())
        } else {
            stream_execution_boundary_tool_layer(executions.clone())
        }
    };
    let mut engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        layer(),
        AgentRuntimeConfig::default(),
    );
    if matches!(
        mode,
        "closed-hooks"
            | "pending-hooks"
            | "open-pending-hooks"
            | "closed-context"
            | "missing-context"
    ) {
        engine = engine.with_lifecycle_hooks(lifecycle_hook_runtime_for_events(
            &[LifecycleHookEventNameV1::PostToolUse],
            if mode == "closed-context" || mode == "missing-context" {
                vec![(
                    LifecycleHookEventNameV1::PostToolUse,
                    json!({"additionalContext":[{"text":"original committed hook context"}]})
                        .to_string(),
                )]
            } else {
                vec![]
            },
        ));
    }
    let captured = journal.clone();
    let control = TurnControl::new_durable_inputs(
        queue.clone(),
        input_binding("original-controller"),
        Arc::new(move |turn, inputs| captured.materialize(turn, inputs, Some("initial-user"))),
    )
    .unwrap();
    let mut request = input_run("active-input");
    request.agent_run_identity = Some(input_identity());
    request.initial_input = AgentRunInitialInput::UserInput {
        input_id: "initial-user".into(),
        message: "accepted objective".into(),
        attachments: vec![],
    };
    let model = TwoToolModel {
        calls: AtomicUsize::new(0),
        bash: host_mode,
        parallel: mode == "parallel",
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
                    ToolSafePoint::ModelRequestStarted(started) => {
                        if matches!(
                            mode,
                            "closed-hooks"
                                | "pending-hooks"
                                | "open-pending-hooks"
                                | "closed-context"
                                | "missing-context"
                        ) && model.calls.load(Ordering::SeqCst) > 0
                        {
                            return Err("injected_receipt_before_canonical_result".into());
                        }
                        records.extend(canonical_model_request_started_records(
                            "input-run",
                            &started,
                            48,
                        )?);
                    }
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
                    } => {
                        if !matches!(
                            mode,
                            "closed-hooks"
                                | "pending-hooks"
                                | "open-pending-hooks"
                                | "closed-context"
                                | "missing-context"
                        ) && call.id == format!("prepared-call-{pending_index}")
                        {
                            return Err("injected_receipt_before_canonical_result".into());
                        }
                        records.push(canonical_tool_result_record(
                            &session_id,
                            &turn_id,
                            &agent_run_id,
                            &call,
                            &result,
                            result.completed_at_ms,
                        )?);
                    }
                    _ => {}
                }
                Ok(())
            },
        )
        .await
        .unwrap_err();
    assert!(error.contains("injected_receipt_before_canonical_result"));
    assert_eq!(executions.load(Ordering::SeqCst), pending_index + 1);
    let facts = journal.read_facts();
    assert_eq!(
        facts.len(),
        if mode == "parallel" {
            1
        } else {
            pending_index + 1
        }
    );
    let empty_input = pending_index == 1 && mode != "parallel";
    assert_eq!(input_ids(facts.last().unwrap()).is_empty(), empty_input);
    if matches!(
        mode,
        "closed-hooks"
            | "pending-hooks"
            | "open-pending-hooks"
            | "closed-context"
            | "missing-context"
    ) {
        assert!(
            store
                .list_events("active-input", 1000, 0)
                .unwrap()
                .iter()
                .any(|event| event.event_type == "post_tool_hook.receipt.v1"),
            "producer must actually finish its hook"
        );
    }
    let mut records = journal.records.lock().unwrap().clone();
    if mode == "open-pending-hooks" {
        records.retain(|record| record.event_type != crate::session::SessionRecordType::ToolResult);
    }
    let mut restored =
        crate::session::restore_runtime_snapshot_from_session_records("active-input", &records)
            .unwrap();
    assert_eq!(
        restored.context_window.last().unwrap().role,
        if mode == "parallel"
            || matches!(
                mode,
                "closed-hooks" | "pending-hooks" | "closed-context" | "missing-context"
            )
        {
            MessageRole::Tool
        } else {
            MessageRole::Assistant
        }
    );
    let current =
        crate::session::turn_input::recovery::current_prepared_turn(&restored, "input-run")
            .unwrap()
            .unwrap();
    assert_eq!(
        crate::session::turn_input::recovery::prepared_ids(&restored, "input-run", &current)
            .unwrap(),
        Some(if empty_input {
            vec![]
        } else if mode == "second-crash" {
            vec!["initial-user".into(), "supplement-user".into()]
        } else {
            vec!["initial-user".into()]
        })
    );
    if mode == "closed-context" {
        let native = SessionManager::new(store.clone())
            .load_session("active-input")
            .unwrap()
            .unwrap();
        let contexts = native
            .messages
            .iter()
            .filter(|message| {
                message
                    .metadata
                    .get(LIFECYCLE_HOOK_CONTEXT_META_KEY)
                    .is_some_and(|value| value == "true")
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            contexts.len(),
            1,
            "real producer must persist its hook context"
        );
        restored.messages.extend(contexts);
        refresh_session_context_window(&mut restored);
    }
    let original_user_count = restored
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .count();
    let driver_id = prompt_projection::driver_user_message_id("active-input", &current);
    assert_eq!(
        restored
            .messages
            .iter()
            .any(|message| message.message_id == driver_id),
        !empty_input
    );
    if mode == "missing-body" {
        restored
            .messages
            .retain(|message| message.message_id != driver_id);
        restored
            .context_window
            .retain(|message| message.message_id != driver_id);
    }
    let store = if host_mode
        || mode == "wrong-source"
        || mode == "pending-hooks"
        || mode == "open-pending-hooks"
    {
        // Reconstruct the durable crash view: intent/call exist, but the final
        // receipt transaction was never committed. The original fixture stays intact.
        let recovered = AgentRuntimeTestStore::new();
        for mut event in store.list_events("active-input", 10_000, 0).unwrap() {
            let is_missing_receipt = event.event_type == "tool_execution.receipt.v1"
                && serde_json::from_str::<serde_json::Value>(&event.payload_json).unwrap()
                    ["toolCallId"]
                    == format!("prepared-call-{pending_index}");
            if mode == "wrong-source" && event.event_type == "tool_execution.intent.v1" {
                let mut payload: serde_json::Value =
                    serde_json::from_str(&event.payload_json).unwrap();
                if payload["toolCallId"] == format!("prepared-call-{pending_index}") {
                    payload["agentRunIdentity"]["authorizationDigest"] =
                        json!(format!("sha256:{}", "e".repeat(64)));
                    event.payload_json = payload.to_string();
                }
            }
            if (mode == "pending-hooks" || mode == "open-pending-hooks")
                && event.event_type == "post_tool_hook.receipt.v1"
            {
                continue;
            }
            if !is_missing_receipt || !host_mode {
                recovered.append_event_idempotent(event).unwrap();
            }
        }
        recovered
    } else {
        store
    };
    SessionManager::new(store.clone())
        .save_session(&restored)
        .unwrap();
    let mut recovered_engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        layer(),
        AgentRuntimeConfig::default(),
    );
    if mode == "hooks"
        || matches!(
            mode,
            "closed-hooks" | "pending-hooks" | "closed-context" | "missing-context"
        )
    {
        recovered_engine = recovered_engine.with_lifecycle_hooks(
            lifecycle_hook_runtime_for_events(&[LifecycleHookEventNameV1::PostToolUse], vec![]),
        );
    }
    let recovered_model = IntakeModelClient {
        requests: Mutex::new(vec![]),
        control: None,
    };
    let captured = journal.clone();
    let control = TurnControl::new_durable_inputs(
        queue.clone(),
        input_binding("new-controller"),
        Arc::new(move |turn, inputs| captured.materialize(turn, inputs, Some("initial-user"))),
    )
    .unwrap();
    if mode == "second-crash" {
        let mut sink = |point| {
            if let ToolSafePoint::DurableReceipt {
                session_id,
                turn_id,
                agent_run_id,
                call,
                result,
            } = point
            {
                journal
                    .records
                    .lock()
                    .unwrap()
                    .push(canonical_tool_result_record(
                        &session_id,
                        &turn_id,
                        &agent_run_id,
                        &call,
                        &result,
                        result.completed_at_ms,
                    )?);
            }
            Ok(())
        };
        let dispatcher = ToolSafePointDispatcher {
            sink: Mutex::new(&mut sink),
        };
        assert!(recovered_engine
            .recover_prepared_tool_phase(
                "active-input",
                &input_identity(),
                &request.initial_input,
                Some(&dispatcher)
            )
            .await
            .unwrap());
        assert!(recovered_model.requests.lock().unwrap().is_empty());
        // The process dies after the helper CAS and before any following main.
        // Only the snapshot and original receipt facts cross this boundary.
        let saved = SessionManager::new(store.clone())
            .load_session("active-input")
            .unwrap()
            .unwrap();
        let continuation =
            crate::session::turn_input::recovery::current_prepared_turn(&saved, "input-run")
                .unwrap()
                .expect("a durable continuation must survive a second crash");
        assert_ne!(continuation, current);
        assert_eq!(
            crate::session::turn_input::recovery::prepared_ids(&saved, "input-run", &continuation)
                .unwrap(),
            Some(vec![])
        );
        recovered_engine = AgentRuntime::new_for_test_with_tools(
            store.clone(),
            layer(),
            AgentRuntimeConfig::default(),
        );
    }
    if mode == "closed-context" {
        let before = store
            .load_agent_runtime_snapshot("active-input")
            .unwrap()
            .unwrap();
        let mut sink =
            |_| -> Result<(), String> { panic!("closed batch cannot replay its receipts") };
        let dispatcher = ToolSafePointDispatcher {
            sink: Mutex::new(&mut sink),
        };
        assert!(
            !recovered_engine
                .recover_prepared_tool_phase(
                    "active-input",
                    &input_identity(),
                    &request.initial_input,
                    Some(&dispatcher)
                )
                .await
                .unwrap(),
            "valid existing hook context must keep its original prepared input path"
        );
        assert_eq!(
            store
                .load_agent_runtime_snapshot("active-input")
                .unwrap()
                .unwrap(),
            before
        );
    }
    let mut recovered_receipt = matches!(mode, "second-crash" | "closed-hooks" | "closed-context");
    let result = recovered_engine
        .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
            request,
            &recovered_model,
            &StaticModelSessionConfigStore {
                config: Some(ModelSessionConfig::default()),
            },
            &mut |_| {},
            &|| Ok(None),
            &control,
            &mut |point| {
                match point {
                    ToolSafePoint::DurableReceipt {
                        session_id,
                        turn_id,
                        agent_run_id,
                        call,
                        result,
                    } => {
                        assert!(
                            recovered_model.requests.lock().unwrap().is_empty(),
                            "model cannot run before pending batch receipt"
                        );
                        assert_eq!(call.id, format!("prepared-call-{pending_index}"));
                        journal
                            .records
                            .lock()
                            .unwrap()
                            .push(canonical_tool_result_record(
                                &session_id,
                                &turn_id,
                                &agent_run_id,
                                &call,
                                &result,
                                result.completed_at_ms,
                            )?);
                        if mode == "snapshot-conflict" {
                            let mut future = SessionManager::new(store.clone())
                                .load_session("active-input")?
                                .unwrap();
                            future
                                .metadata
                                .insert("future-controller-marker".into(), "keep".into());
                            SessionManager::new(store.clone()).save_session(&future)?;
                        }
                        if mode == "host-gap" {
                            assert_ne!(result.status, "ok");
                            assert!(!result.content.contains("completed successfully"));
                            assert!(result.content.contains("output"));
                            assert_eq!(result.details["exitCode"], 0);
                            assert_eq!(result.details["executed"], true);
                            assert_eq!(result.details["toolError"]["retryable"], false);
                        }
                        recovered_receipt = true;
                    }
                    ToolSafePoint::ModelRequestStarted(_) => {
                        assert!(recovered_receipt, "new main must follow original receipt")
                    }
                    _ => {}
                }
                Ok(())
            },
        )
        .await;
    if !matches!(
        mode,
        "ok" | "host-completed"
            | "parallel"
            | "host-gap"
            | "second-crash"
            | "closed-hooks"
            | "closed-context"
    ) {
        let expected = match mode {
            "missing-body" => "turn_input_prepared_message_missing",
            "hooks" => "prepared_tool_recovery_hooks_unsupported",
            "pending-hooks" => "prepared_tool_recovery_hooks_pending",
            "missing-context" => "prepared_tool_recovery_hook_context_unsupported",
            "open-pending-hooks" => "prepared_tool_recovery_hooks_unsupported",
            "snapshot-conflict" => "prepared_tool_recovery_snapshot_conflict",
            "host-unknown" => "command recovery outcome unknown",
            "wrong-source" => "prepared_tool_recovery_source_identity_mismatch",
            _ => unreachable!(),
        };
        assert!(result.unwrap_err().contains(expected));
        assert!(recovered_model.requests.lock().unwrap().is_empty());
        assert_eq!(executions.load(Ordering::SeqCst), pending_index + 1);
        if mode == "host-unknown" {
            assert_eq!(host.queries.load(Ordering::SeqCst), 1);
            assert_eq!(
                std::fs::read_to_string(workspace.join("effects")).unwrap(),
                "effect-0\neffect-1\n"
            );
            assert!(!store
                .list_events("active-input", 100, 0)
                .unwrap()
                .iter()
                .any(|event| event.event_type == "tool_execution.receipt.v1"
                    && serde_json::from_str::<serde_json::Value>(&event.payload_json).unwrap()
                        ["toolCallId"]
                        == "prepared-call-1"));
        }
        if mode == "snapshot-conflict" {
            assert_eq!(
                SessionManager::new(store.clone())
                    .load_session("active-input")
                    .unwrap()
                    .unwrap()
                    .metadata["future-controller-marker"],
                "keep"
            );
        }
        return;
    }
    let result = result.expect(
        "prepared tool phase must reconcile its durable receipt, not restore missing user input",
    );
    if mode == "parallel" {
        let saved = SessionManager::new(store.clone())
            .load_session("active-input")
            .unwrap()
            .unwrap();
        let result_ids = saved
            .context_window
            .iter()
            .filter_map(
                |message| match saved.model_semantics.get(&message.message_id) {
                    Some(ModelMessageSemanticsV1::ToolResult { tool_call_id, .. }) => {
                        Some(tool_call_id.clone())
                    }
                    _ => None,
                },
            )
            .collect::<Vec<_>>();
        assert_eq!(result_ids, vec!["prepared-call-0", "prepared-call-1"]);
    }
    if mode == "second-crash" {
        let saved = SessionManager::new(store.clone())
            .load_session("active-input")
            .unwrap()
            .unwrap();
        assert_eq!(
            saved
                .messages
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .count(),
            original_user_count,
            "original merged user body is not materialized twice"
        );
        assert!(saved
            .messages
            .iter()
            .any(|message| message.content.contains("accepted supplement")));
        assert_eq!(
            journal.read_facts().len(),
            facts.len(),
            "no duplicate input uptake"
        );
    }
    if mode == "closed-context" {
        let requests = recovered_model.requests.lock().unwrap();
        assert!(serde_json::to_string(&requests[0].prepared_prompt)
            .unwrap()
            .contains("original committed hook context"));
        assert_eq!(
            journal.read_facts().len(),
            facts.len(),
            "no repeated uptake"
        );
    }
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert!(recovered_receipt);
    assert_eq!(
        executions.load(Ordering::SeqCst),
        pending_index + 1,
        "zero repeated effects"
    );
    assert_eq!(recovered_model.requests.lock().unwrap().len(), 1);
    if host_mode {
        assert_eq!(host.queries.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read_to_string(workspace.join("effects")).unwrap(),
            "effect-0\neffect-1\n"
        );
    }
}

#[tokio::test]
async fn query_loop_prepared_tool_recovery_rejects_missing_nonempty_input_body() {
    assert_prepared_receipt_recovery(0, "missing-body").await;
}
#[tokio::test]
async fn query_loop_prepared_tool_recovery_rejects_unsupported_hooks_without_reexecution() {
    assert_prepared_receipt_recovery(1, "hooks").await;
}
#[tokio::test]
async fn query_loop_prepared_tool_recovery_preserves_future_snapshot_on_conflict() {
    assert_prepared_receipt_recovery(1, "snapshot-conflict").await;
}

struct QueryingHost {
    executions: Arc<AtomicUsize>,
    outputs: Mutex<HashMap<String, (crate::execution::ExecutionHostCommandOutput, Vec<String>)>>,
    queries: AtomicUsize,
    unknown: bool,
    output_gap: bool,
}
impl crate::execution::ExecutionHostRunner for QueryingHost {
    fn kind(&self) -> crate::execution::ExecutionHostKind {
        crate::execution::ExecutionHostKind::RemoteHost
    }
    fn status(
        &self,
        _: &crate::execution::ExecutionPolicy,
    ) -> Result<crate::execution::ExecutionHostStatus, crate::execution::ExecutionError> {
        Ok(crate::execution::ExecutionHostStatus::remote(
            true,
            crate::execution::ExecutionHostHealth::Ready,
            None,
        ))
    }
    fn run_file_system_operation(
        &self,
        request: crate::execution::ExecutionFileSystemRequest,
    ) -> Result<
        crate::execution::ExecutionFileSystemOutput,
        crate::execution::ExecutionFileSystemError,
    > {
        crate::execution::run_policy_scoped_execution_file_system_operation(request)
    }
    fn run_host_command(
        &self,
        operation_id: Option<&str>,
        req: crate::execution::ExecutionCommandRequest,
        _: Option<&crate::execution::ExecutionCancellationProbe>,
    ) -> Result<crate::execution::ExecutionHostCommandOutput, crate::execution::ExecutionError>
    {
        use crate::execution::*;
        self.executions.fetch_add(1, Ordering::SeqCst);
        let program = std::env::var_os("CENTAERIS_TEST_BASH_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    "C:/Program Files/Git/bin/bash.exe".into()
                } else {
                    "bash".into()
                }
            });
        let output = std::process::Command::new(program)
            .args(&req.args)
            .current_dir(&req.cwd)
            .envs(&req.env)
            .output()
            .map_err(|error| ExecutionError::Io(error.to_string()))?;
        assert!(
            output.status.success(),
            "private fixture Bash failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            req.cwd.join("effects").is_file(),
            "private fixture did not write effects: cwd={:?} args={:?}",
            req.cwd,
            req.args
        );
        let stdout = decode_process_output(&output.stdout);
        let stderr = decode_process_output(&output.stderr);
        let exit = output.status.code();
        let result = ExecutionHostCommandOutput {
            process: ExecutionProcessOutput {
                exit_code: exit,
                stdout: stdout.text,
                stderr: stderr.text,
                stdout_decode: stdout.summary,
                stderr_decode: stderr.summary,
                timed_out: false,
                attempt: ExecutionAttempt {
                    transition_reason: "prepared_recovery_fixture".into(),
                    policy: ExecutionPolicySummary {
                        enforced: true,
                        network: req.policy.network,
                        workspace_root: req.cwd.to_string_lossy().into(),
                        read_only_root_count: 0,
                        writable_root_count: 1,
                        denied_read_path_count: 0,
                        denied_write_path_count: 0,
                    },
                },
                runtime_diagnostics: vec![],
            },
            failure_kind: classify_execution_host_failure(exit, false, "", ""),
            input_state_changes: vec![],
        };
        self.outputs
            .lock()
            .unwrap()
            .insert(operation_id.unwrap().into(), (result.clone(), req.args));
        Ok(result)
    }
    fn recover_host_command(
        &self,
        operation_id: &str,
        req: crate::execution::ExecutionCommandRequest,
        _: Option<&crate::execution::ExecutionCancellationProbe>,
    ) -> Result<crate::execution::ExecutionHostCommandRecovery, crate::execution::ExecutionError>
    {
        self.queries.fetch_add(1, Ordering::SeqCst);
        if self.output_gap {
            return Err(crate::execution::ExecutionError::OutputUnavailable {
                reason: "fixture original task output unavailable".into(),
                exit_code: Some(0),
            });
        }
        if self.unknown {
            return Ok(crate::execution::ExecutionHostCommandRecovery::Unknown {
                reason: "fixture task unobservable".into(),
            });
        }
        let outputs = self.outputs.lock().unwrap();
        let (output, args) = outputs
            .get(operation_id)
            .expect("same original operation identity");
        assert_eq!(args, &req.args);
        Ok(crate::execution::ExecutionHostCommandRecovery::Completed(
            Box::new(output.clone()),
        ))
    }
}
fn recovery_host_layer(workspace: &Path, host: Arc<QueryingHost>) -> ToolLayer {
    let binding = Arc::new(
        crate::execution::ExecutionHostBinding::new(
            crate::execution::ExecutionHostMode::Remote,
            host,
            workspace.into(),
            crate::execution::ExecutionPolicy::workspace_write_no_network(workspace),
        )
        .unwrap(),
    );
    ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        binding,
    )
    .unwrap()
}
#[tokio::test]
async fn query_loop_prepared_tool_queries_original_completed_host_without_repeating_bash() {
    assert_prepared_receipt_recovery(1, "host-completed").await;
}
#[tokio::test]
async fn query_loop_prepared_tool_keeps_unknown_host_outcome_without_receipt_or_model_replay() {
    assert_prepared_receipt_recovery(1, "host-unknown").await;
}

#[tokio::test]
async fn query_loop_prepared_parallel_batch_recovers_only_unclosed_original_call_in_order() {
    assert_prepared_receipt_recovery(1, "parallel").await;
}

#[tokio::test]
async fn query_loop_prepared_tool_recovery_rejects_wrong_source_authorization_without_replay() {
    assert_prepared_receipt_recovery(1, "wrong-source").await;
}

#[tokio::test]
async fn query_loop_prepared_tool_preserves_known_exit_with_unavailable_output_without_replay() {
    assert_prepared_receipt_recovery(1, "host-gap").await;
}

#[tokio::test]
async fn query_loop_prepared_tool_second_crash_keeps_durable_continuation_and_merged_input() {
    assert_prepared_receipt_recovery(0, "second-crash").await;
}
#[tokio::test]
async fn query_loop_prepared_closed_batch_preserves_completed_hooks_without_reexecution() {
    assert_prepared_receipt_recovery(0, "closed-hooks").await;
}
#[tokio::test]
async fn query_loop_prepared_closed_batch_rejects_pending_hook_without_reexecution() {
    assert_prepared_receipt_recovery(0, "pending-hooks").await;
}

#[tokio::test]
async fn query_loop_prepared_tool_rejects_old_pending_hook_without_current_handler() {
    assert_prepared_receipt_recovery(0, "open-pending-hooks").await;
}

#[tokio::test]
async fn query_loop_prepared_closed_batch_keeps_nonempty_hook_context_and_original_input_path() {
    assert_prepared_receipt_recovery(0, "closed-context").await;
}
#[tokio::test]
async fn query_loop_prepared_closed_batch_rejects_missing_completed_hook_context() {
    assert_prepared_receipt_recovery(0, "missing-context").await;
}
