use super::*;

impl<
        S: RuntimeStore
            + ExternalContextStorePort
            + RuntimeJobStorePort
            + RuntimeStoreTransactionPort
            + AgentRuntimeSnapshotStorePort
            + Clone
            + Send
            + Sync
            + 'static,
    > AgentRuntime<S>
{
    #[cfg(test)]
    pub(super) async fn process_turn_with_stream_sink_async(
        &self,
        req: ProcessTurnRequest,
        stream_sink: Option<&mut (dyn FnMut(TurnUpdate) + Send + '_)>,
    ) -> Result<TurnStepResult, String> {
        self.process_turn_with_stream_sink_and_tool_safe_point_async(req, stream_sink, None)
            .await
    }

    pub(super) async fn process_turn_with_stream_sink_and_tool_safe_point_async(
        &self,
        req: ProcessTurnRequest,
        mut stream_sink: Option<&mut (dyn FnMut(TurnUpdate) + Send + '_)>,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<TurnStepResult, String> {
        let mut session = self
            .session_manager
            .load_or_create_session(req.session_id.as_str())?;
        let mut prompt_compaction_runtime_events = Vec::new();
        let mut recovery_policy_trace_json = vec![];
        self.close_unpaired_tool_calls_for_new_turn(
            &mut session,
            req.session_id.as_str(),
            req.turn_id.as_str(),
            tool_safe_point,
        )
        .await?;

        let submitted_user_message = req.input.user_message();
        let message_semantic_kind = req.input.semantic_kind();
        let effective_user_message = update_active_objective_for_message(
            &mut session,
            message_semantic_kind,
            req.turn_id.as_str(),
            req.input.objective(),
        )?;
        if message_semantic_kind == MESSAGE_SEMANTIC_USER_REQUEST
            && !model_input_already_persisted(
                &session,
                req.session_id.as_str(),
                req.turn_id.as_str(),
            )
        {
            let user_prompt_hook = self.run_user_prompt_submit_hook(
                req.session_id.as_str(),
                submitted_user_message.expect("user request has submitted message"),
            )?;
            if user_prompt_hook.blocked {
                return Err(format!(
                    "UserPromptSubmit hook blocked turn: {}",
                    user_prompt_hook
                        .block_reason
                        .unwrap_or_else(|| "blocked by lifecycle hook".to_string())
                ));
            }
            append_lifecycle_hook_context_messages(
                &self.message_handler,
                &mut session,
                user_prompt_hook
                    .additional_context
                    .iter()
                    .map(|item| item.text.as_str()),
            );
        }
        let model_input_persisted =
            model_input_already_persisted(&session, req.session_id.as_str(), req.turn_id.as_str());
        if message_semantic_kind == MESSAGE_SEMANTIC_USER_REQUEST && !model_input_persisted {
            let mut metadata = JsonMap::new();
            metadata.insert(
                MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
                MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
            );
            self.message_handler.push_user_message(
                &mut session,
                submitted_user_message.expect("user request has submitted message"),
                metadata,
            );
        }
        refresh_session_context_window(&mut session);
        let prompt_compaction_result =
            if prompt_compaction_stats_match_turn(&session, req.turn_id.as_str()) {
                PromptCompactionApplyResult {
                    stats_json: session
                        .metadata
                        .get("prompt_compaction_stats_json")
                        .cloned(),
                    runtime_events: vec![],
                }
            } else {
                self.apply_prompt_compaction(&mut session, req.turn_id.as_str())?
            };
        let compression_stats_json = prompt_compaction_result.stats_json.clone();
        prompt_compaction_runtime_events.extend(prompt_compaction_result.runtime_events);

        let effective_generate_result = req.generate_result.clone();
        if effective_generate_result.content.trim().is_empty()
            && effective_generate_result.tool_calls.is_empty()
        {
            return Err("provider returned an empty final response without tool calls".to_string());
        }
        self.validate_tool_turn_behavior_preflight(&session, &effective_generate_result)?;
        let system_prompt_manifest_json = self.apply_system_prompt_template(
            &mut session,
            &req,
            effective_user_message.as_str(),
            compression_stats_json.as_deref(),
        )?;

        let tool_permission_preview = self.build_tool_permission_preview(
            &session,
            effective_generate_result.tool_calls.as_slice(),
        );
        let mut route_state = turn_state::build_route_state(
            effective_generate_result.clone(),
            system_prompt_manifest_json.as_deref(),
            compression_stats_json.clone(),
            &req.agent_run_resource_usage,
        );

        attach_runtime_metadata_to_state(&mut route_state, &session, &self.config);
        let routed = self
            .route_after_generate_with_retry(
                RouteGenerateResultRequest {
                    session_id: req.session_id.clone(),
                    turn_id: req.turn_id.clone(),
                    state: route_state,
                },
                "route_after_response",
                &mut recovery_policy_trace_json,
            )
            .await?;

        let mut runtime_events = prompt_compaction_runtime_events;
        let mut checkpoint = None;
        let mut continuation = routed.continuation;
        let mut tool_results = vec![];
        let mut tool_use_summary = None;
        let mut tool_operations_json = None;
        let model_process_summary = if should_emit_model_process_summary(&routed.continuation) {
            model_process_summary_message(&effective_generate_result)
        } else {
            None
        };
        if matches!(routed.continuation, QueryContinuation::ExecuteTools)
            && ensure_model_assistant_semantics_message(
                &self.message_handler,
                &mut session,
                &effective_generate_result,
            )?
        {
            self.session_manager.save_session(&session)?;
        }
        if matches!(routed.continuation, QueryContinuation::AwaitQuestion) {
            let persisted = self
                .persist_query_state_with_retry(
                    PersistQueryStateRequest {
                        session_id: req.session_id.clone(),
                        turn_id: req.turn_id.clone(),
                        at_ms: now_ms(),
                        state: routed.state.clone(),
                        agent_run_identity: req.agent_run_identity.clone(),
                    },
                    "persist_runtime_wait",
                    &mut recovery_policy_trace_json,
                )
                .await?;
            checkpoint = Some(persisted.checkpoint);
        }
        let should_emit_tool_call_events =
            matches!(routed.continuation, QueryContinuation::ExecuteTools);
        if let Some(message) = model_process_summary.as_ref() {
            let task_id = format!("model_process_status:{}", req.turn_id);
            let event = build_runtime_event_status_event(
                req.session_id.as_str(),
                req.turn_id.as_str(),
                task_id.as_str(),
                req.turn_id.as_str(),
                message.as_str(),
                continuation_event_status(&routed.continuation),
                Some(StatusStage::ModelProcessSummary),
                Some("model_process_summary"),
            );
            push_runtime_event_with_optional_stream(&mut runtime_events, &mut stream_sink, event);
        }
        if should_emit_tool_call_events {
            runtime_events.extend(build_runtime_event_tool_call_events(
                req.session_id.as_str(),
                req.turn_id.as_str(),
                effective_generate_result.tool_calls.as_slice(),
                Some(&tool_permission_preview),
            )?);
        }
        match routed.continuation {
            QueryContinuation::AwaitQuestion => {
                let question_message = "Waiting for user input.";
                self.message_handler.push_assistant_message(
                    &mut session,
                    question_message,
                    JsonMap::new(),
                );
                runtime_events.push(build_runtime_event_status_event(
                    req.session_id.as_str(),
                    req.turn_id.as_str(),
                    req.turn_id.as_str(),
                    req.turn_id.as_str(),
                    question_message,
                    "running",
                    Some(StatusStage::QuestionWait),
                    Some("await_question"),
                ));
                runtime_events.push(build_runtime_event_question_required_event(
                    req.session_id.as_str(),
                    req.turn_id.as_str(),
                    req.turn_id.as_str(),
                    json!({
                        "message": question_message,
                    }),
                ));
            }
            QueryContinuation::ExecuteTools => {
                let execution_batch = self
                    .execute_tool_calls_with_safe_point_async(
                        req.session_id.as_str(),
                        req.turn_id.as_str(),
                        &session,
                        effective_generate_result.clone(),
                        req.agent_run_identity.as_ref(),
                        stream_sink,
                        tool_safe_point,
                    )
                    .await?;
                merge_recovery_traces(
                    &mut recovery_policy_trace_json,
                    execution_batch.recovery_policy_trace_json.as_slice(),
                );
                runtime_events.extend(execution_batch.tool_progress_events);
                tool_results = execution_batch.tool_results;
                if !execution_batch.runtime_job_waits.is_empty() {
                    let agent_run_identity = req.agent_run_identity.as_ref().ok_or_else(|| {
                        "runtime_job_wait_requires_agent_run_identity".to_string()
                    })?;
                    let wait_checkpoint = RuntimeAwaitJobCheckpointV1::new(
                        agent_run_identity,
                        req.turn_id.as_str(),
                        execution_batch.runtime_job_waits,
                    )?;
                    let at_ms = now_ms();
                    let pending = PendingRuntimeToolBatchV1 {
                        schema: "runtime.pending_tool_batch.v1".to_string(),
                        turn_id: req.turn_id.clone(),
                        waiting_at_ms: at_ms,
                        wait_checkpoint: wait_checkpoint.clone(),
                        agent_run_resource_usage: req.agent_run_resource_usage.clone(),
                        system_prompt_manifest_json: system_prompt_manifest_json.clone(),
                        compression_stats_json: compression_stats_json.clone(),
                        lifecycle_hook_contexts: execution_batch.lifecycle_hook_contexts,
                        transition_reason: execution_batch.transition_reason,
                        recovery_policy_trace_json: execution_batch.recovery_policy_trace_json,
                    };
                    session.metadata.insert(
                        RUNTIME_PENDING_TOOL_BATCH_META_KEY.to_string(),
                        serde_json::to_string(&pending).map_err(|error| {
                            format!("serialize pending runtime tool batch failed: {error}")
                        })?,
                    );
                    self.session_manager.save_session(&session)?;

                    let (waiting_checkpoint, event, changed) = runtime_waiting_transition(
                        req.session_id.as_str(),
                        req.turn_id.as_str(),
                        &wait_checkpoint,
                        at_ms,
                    )?;
                    self.runtime_store.save_wait_checkpoint(
                        crate::session::store::SaveWaitCheckpointRequest {
                            checkpoint: waiting_checkpoint.clone(),
                            event: event.clone(),
                        },
                    )?;
                    runtime_events.push(build_runtime_event_runtime_wait_changed(
                        req.session_id.as_str(),
                        req.turn_id.as_str(),
                        &changed,
                    )?);
                    let resumed = self
                        .resume_runtime_job_wait_async(waiting_checkpoint, Some(agent_run_identity))
                        .await?;
                    continuation = resumed.continuation;
                    checkpoint = resumed.checkpoint;
                    tool_results = resumed.tool_results;
                    tool_use_summary = resumed.tool_use_summary;
                    tool_operations_json = resumed.tool_operations_json;
                    runtime_events.extend(resumed.runtime_events);
                    session = resumed.session_snapshot;
                } else {
                    tool_operations_json = project_tool_operations_json(&tool_results);
                    let tool_result_events = build_runtime_event_tool_result_events(
                        req.session_id.as_str(),
                        req.turn_id.as_str(),
                        &tool_results,
                        tool_operations_json.as_deref(),
                    )?;
                    runtime_events.extend(tool_result_events.clone());
                    runtime_events.extend(
                        build_runtime_event_subagent_tool_group_events_from_tool_results(
                            req.session_id.as_str(),
                            req.turn_id.as_str(),
                            &tool_results,
                            tool_result_events.as_slice(),
                        ),
                    );
                    let _tool_context_write_summary =
                        tool_context_writer::write_tool_results_to_context(
                            &self.message_handler,
                            &mut session,
                            tool_results.as_slice(),
                        )?;
                    append_lifecycle_hook_context_messages(
                        &self.message_handler,
                        &mut session,
                        execution_batch
                            .lifecycle_hook_contexts
                            .iter()
                            .map(String::as_str),
                    );
                    if self.config.enable_tool_use_summary {
                        tool_use_summary = Some(build_tool_use_summary(&tool_results));
                    }
                    if self.should_complete_turn_after_tool_success(
                        &effective_generate_result,
                        &tool_results,
                    )? {
                        continuation = QueryContinuation::CompleteTerminalTool;
                    } else {
                        continuation = QueryContinuation::ExecuteTools;
                    }
                }
            }
            QueryContinuation::AwaitRuntimeJob => {
                return Err(
                    "runtime job continuation cannot be routed from provider output".to_string(),
                );
            }
            QueryContinuation::CompleteTerminalTool => {
                return Err("terminal tool continuation cannot be routed".to_string());
            }
            QueryContinuation::Finalize => {
                continuation = QueryContinuation::Finalize;
                let text = effective_generate_result.content.as_str();
                self.message_handler.push_model_assistant_message(
                    &mut session,
                    text,
                    JsonMap::new(),
                    build_model_assistant_semantics(&effective_generate_result),
                );
                runtime_events.push(build_runtime_event_final_event(
                    req.session_id.as_str(),
                    req.turn_id.as_str(),
                    text,
                    Some(&effective_generate_result),
                ));
            }
        }

        if continuation == QueryContinuation::CompleteTerminalTool {
            mark_terminal_tool_transcript_committed(&mut session, &effective_generate_result)?;
        }
        refresh_session_context_window(&mut session);
        self.session_manager.save_session(&session)?;
        Ok(TurnStepResult {
            turn_id: req.turn_id,
            continuation,
            checkpoint,
            provider_tool_calls: effective_generate_result.tool_calls,
            tool_results,
            tool_use_summary,
            tool_operations_json,
            agent_run_resource_usage: req.agent_run_resource_usage,
            runtime_events,
            session_snapshot: session,
        })
    }
}
