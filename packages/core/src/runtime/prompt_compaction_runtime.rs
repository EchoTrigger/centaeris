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
    pub(super) fn apply_prompt_compaction(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
    ) -> Result<PromptCompactionApplyResult, String> {
        self.apply_prompt_compaction_for_generate_request(
            session,
            turn_id,
            PromptCompactionScopeV1::main(),
            None,
            None,
        )
    }

    pub(super) fn apply_prompt_compaction_for_generate_request(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        runtime_scope: PromptCompactionScopeV1,
        prompt_input_token_estimate: Option<u32>,
        model_candidate_producer: Option<&dyn ModelCompactionSummaryCandidateProducer>,
    ) -> Result<PromptCompactionApplyResult, String> {
        if !self.config.enable_prompt_compaction {
            session.metadata.remove("prompt_compaction_stats_json");
            return Ok(PromptCompactionApplyResult::default());
        }
        if prompt_compaction_circuit_is_open(session) {
            return Ok(self.prompt_compaction_circuit_result(session, turn_id));
        }

        let mut compacted_session = session.clone();
        let outcome = match self.run_prompt_compaction_summary_producer(
            &mut compacted_session,
            turn_id,
            runtime_scope,
            session,
            model_candidate_producer,
            prompt_input_token_estimate,
        ) {
            Ok(outcome) => outcome,
            Err(error) if error.phase == "session_log" => return Err(error.to_string()),
            Err(error) => return Ok(self.prompt_compaction_failure_result(session, turn_id, error)),
        };
        self.apply_prompt_compaction_outcome(session, turn_id, compacted_session, outcome)
    }

    pub(super) async fn apply_prompt_compaction_for_generate_request_async(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        runtime_scope: PromptCompactionScopeV1,
        prompt_input_token_estimate: Option<u32>,
        force: bool,
        model_candidate_producer: Option<
            &(dyn AsyncModelCompactionSummaryCandidateProducer + Send + Sync),
        >,
    ) -> Result<PromptCompactionApplyResult, String> {
        if !self.config.enable_prompt_compaction {
            session.metadata.remove("prompt_compaction_stats_json");
            return Ok(PromptCompactionApplyResult::default());
        }
        if prompt_compaction_circuit_is_open(session) {
            return Ok(self.prompt_compaction_circuit_result(session, turn_id));
        }

        let mut compacted_session = session.clone();
        let outcome = match self
            .run_prompt_compaction_summary_producer_async(
                &mut compacted_session,
                turn_id,
                runtime_scope,
                session,
                model_candidate_producer,
                prompt_input_token_estimate,
                force,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) if error.phase == "session_log" => return Err(error.to_string()),
            Err(error) => return Ok(self.prompt_compaction_failure_result(session, turn_id, error)),
        };
        self.apply_prompt_compaction_outcome(session, turn_id, compacted_session, outcome)
    }

    pub(super) fn apply_prompt_compaction_outcome(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        mut compacted_session: SessionStateSnapshot,
        outcome: PromptCompactionOutcome,
    ) -> Result<PromptCompactionApplyResult, String> {
        let stats_json = serde_json::to_string(&outcome.stats)
            .map_err(|error| format!("serialize prompt compaction stats failed: {error}"))?;
        let Some(commit) = outcome.commit.as_ref() else {
            session.metadata.insert(
                "prompt_compaction_stats_json".to_string(),
                stats_json.clone(),
            );
            let status = if outcome.stats.decision.action == "blocked" {
                "blocked"
            } else {
                "skipped"
            };
            let phase = if outcome.stats.decision.strategy == "pre_compact_hook" {
                "pre_compact_hook"
            } else {
                "planning"
            };
            let runtime_events = self
                .run_post_compact_hook_for_result(
                    session,
                    turn_id,
                    status,
                    phase,
                    outcome.stats.reason.as_str(),
                    None,
                )
                .into_iter()
                .collect();
            return Ok(PromptCompactionApplyResult {
                stats_json: Some(stats_json),
                runtime_events,
            });
        };

        let session_id = session.session_id.clone();
        compacted_session.metadata.insert(
            "prompt_compaction_stats_json".to_string(),
            stats_json.clone(),
        );
        let post_hook_event = self.run_post_compact_hook_for_result(
            &mut compacted_session,
            turn_id,
            "succeeded",
            "committed",
            "context_pressure_threshold_reached",
            Some(commit),
        );
        clear_prompt_compaction_failure_metadata(&mut compacted_session);
        refresh_session_context_window(&mut compacted_session);
        *session = compacted_session;

        let mut runtime_events = vec![build_runtime_event_prompt_compaction_event(
            session_id.as_str(),
            turn_id,
            "prompt_compaction",
            "done",
            "上下文已压缩",
            None,
            json!({
                "compactionId": commit.compaction_id,
                "summaryMessageId": commit.summary_message_id,
                "summaryMarkdown": commit.summary_markdown,
                "firstKeptMessageId": commit.first_kept_message_id,
            }),
        )];
        if let Some(event) = post_hook_event {
            runtime_events.push(event);
        }
        Ok(PromptCompactionApplyResult {
            stats_json: Some(stats_json),
            runtime_events,
        })
    }

    fn prompt_compaction_failure_result(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        error: PromptCompactionError,
    ) -> PromptCompactionApplyResult {
        self.record_prompt_compaction_failure(
            session,
            turn_id,
            error.phase.as_str(),
            error.reason.as_str(),
        );
        let mut runtime_events = vec![build_runtime_event_prompt_compaction_event(
            session.session_id.as_str(),
            turn_id,
            "prompt_compaction",
            "error",
            "上下文压缩失败",
            Some(error.reason.as_str()),
            json!({
                "schema": "prompt_compaction_event_v1",
                "phase": error.phase,
                "reason": compact_text(error.reason.as_str(), 600),
            }),
        )];
        if let Some(event) = self.run_post_compact_hook_for_result(
            session,
            turn_id,
            "failed",
            error.phase.as_str(),
            error.reason.as_str(),
            None,
        ) {
            runtime_events.push(event);
        }
        PromptCompactionApplyResult {
            stats_json: None,
            runtime_events,
        }
    }

    fn prompt_compaction_circuit_result(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
    ) -> PromptCompactionApplyResult {
        let runtime_events = self
            .run_post_compact_hook_for_result(
                session,
                turn_id,
                "circuitOpen",
                "circuit",
                "circuit_open",
                None,
            )
            .into_iter()
            .collect();
        PromptCompactionApplyResult {
            stats_json: None,
            runtime_events,
        }
    }

    fn pre_compact_hook_decision(
        &self,
        session_id: &str,
        plan: &PromptCompactionPlanV1,
    ) -> PromptCompactionPreCompactHookDecision {
        let payload = json!({
            "schema": "prompt_compaction_pre_compact_hook_v1",
            "phase": "planned",
            "plan": plan,
        });
        match self.run_pre_compact_hook(session_id, payload) {
            Ok(outcome) if outcome.blocked => PromptCompactionPreCompactHookDecision::block(
                outcome
                    .block_reason
                    .unwrap_or_else(|| "blocked by PreCompact lifecycle hook".to_string()),
            ),
            Ok(_) => PromptCompactionPreCompactHookDecision::allow(),
            Err(error) => PromptCompactionPreCompactHookDecision::block(format!(
                "PreCompact lifecycle hook failed: {error}"
            )),
        }
    }

    fn run_post_compact_hook_for_result(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        status: &str,
        phase: &str,
        reason: &str,
        commit: Option<&PromptCompactionCommit>,
    ) -> Option<RuntimeEventProjection> {
        let payload = json!({
            "schema": "prompt_compaction_post_compact_hook_v1",
            "phase": phase,
            "status": status,
            "reason": reason,
            "compactionId": commit.map(|item| item.compaction_id.as_str()),
            "summaryMessageId": commit.map(|item| item.summary_message_id.as_str()),
            "firstKeptMessageId": commit.and_then(|item| item.first_kept_message_id.as_deref()),
        });
        match self.run_post_compact_hook(session.session_id.as_str(), payload) {
            Ok(result) if result.blocked => Some(
                self.record_prompt_compaction_post_hook_failed(
                    session,
                    turn_id,
                    result
                        .block_reason
                        .as_deref()
                        .unwrap_or("PostCompact lifecycle hook failed"),
                ),
            ),
            Ok(_) => None,
            Err(error) => Some(self.record_prompt_compaction_post_hook_failed(
                session,
                turn_id,
                error.as_str(),
            )),
        }
    }

    fn record_prompt_compaction_post_hook_failed(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        reason: &str,
    ) -> RuntimeEventProjection {
        let payload = json!({
            "schema": "prompt_compaction_post_hook_failure_v1",
            "turnId": turn_id,
            "reason": compact_text(reason, 800),
            "recordedAtMs": now_ms(),
        });
        let _ = self.runtime_store.append_event(RuntimeEvent {
            event_id: format!(
                "evt_prompt_compaction_post_hook_failed_{}_{}",
                turn_id,
                now_ms()
            ),
            session_id: session.session_id.clone(),
            task_id: Some(turn_id.to_string()),
            event_type: "prompt.compaction.post_hook_failed".to_string(),
            at_ms: now_ms(),
            visibility: EventVisibility::Internal,
            payload_json: payload.to_string(),
        });
        build_runtime_event_prompt_compaction_event(
            session.session_id.as_str(),
            turn_id,
            "prompt_compaction_hook",
            "error",
            "上下文压缩后置 Hook 失败",
            Some(reason),
            payload,
        )
    }

    pub(super) fn run_prompt_compaction_summary_producer(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        runtime_scope: PromptCompactionScopeV1,
        hook_session: &SessionStateSnapshot,
        model_candidate_producer: Option<&dyn ModelCompactionSummaryCandidateProducer>,
        prompt_input_token_estimate: Option<u32>,
    ) -> Result<PromptCompactionOutcome, PromptCompactionError> {
        let candidate_producer = model_candidate_producer.ok_or_else(|| {
            PromptCompactionError::provider(
                "model prompt compaction requires a model-capable generate driver",
            )
        })?;
        let hook_session_id = hook_session.session_id.clone();
        let pre_compact_hook = |plan: &PromptCompactionPlanV1| {
            self.pre_compact_hook_decision(hook_session_id.as_str(), plan)
        };
        run_one_turn_model_compaction_and_pre_hook(
            session,
            turn_id,
            &self.prompt_compaction_config,
            runtime_scope,
            candidate_producer,
            prompt_input_token_estimate,
            Some(&pre_compact_hook),
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "compaction orchestration keeps producer inputs explicit"
    )]
    pub(super) async fn run_prompt_compaction_summary_producer_async(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        runtime_scope: PromptCompactionScopeV1,
        hook_session: &SessionStateSnapshot,
        model_candidate_producer: Option<
            &(dyn AsyncModelCompactionSummaryCandidateProducer + Send + Sync),
        >,
        prompt_input_token_estimate: Option<u32>,
        force: bool,
    ) -> Result<PromptCompactionOutcome, PromptCompactionError> {
        let candidate_producer = model_candidate_producer.ok_or_else(|| {
            PromptCompactionError::provider(
                "model prompt compaction requires an async model-capable generate driver",
            )
        })?;
        let hook_session_id = hook_session.session_id.clone();
        let pre_compact_hook = |plan: &PromptCompactionPlanV1| {
            self.pre_compact_hook_decision(hook_session_id.as_str(), plan)
        };
        run_one_turn_model_compaction_async_and_pre_hook(
            session,
            turn_id,
            &self.prompt_compaction_config,
            runtime_scope,
            candidate_producer,
            prompt_input_token_estimate,
            force,
            Some(&pre_compact_hook),
        )
        .await
    }

    pub(super) fn record_prompt_compaction_failure(
        &self,
        session: &mut SessionStateSnapshot,
        turn_id: &str,
        phase: &str,
        reason: &str,
    ) {
        let failure_count = session
            .metadata
            .get(PROMPT_COMPACTION_FAILURE_COUNT_META_KEY)
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0)
            .saturating_add(1);
        let circuit_open = failure_count >= PROMPT_COMPACTION_CIRCUIT_FAILURE_THRESHOLD;
        session.metadata.insert(
            PROMPT_COMPACTION_FAILURE_COUNT_META_KEY.to_string(),
            failure_count.to_string(),
        );
        let payload = json!({
            "schema": "prompt_compaction_failure_v1",
            "turnId": turn_id,
            "phase": phase,
            "reason": compact_text(reason, 800),
            "failureCount": failure_count,
            "circuitOpen": circuit_open,
            "recordedAtMs": now_ms(),
        });
        session.metadata.insert(
            PROMPT_COMPACTION_FAILURE_META_KEY.to_string(),
            payload.to_string(),
        );
        if circuit_open {
            session.metadata.insert(
                PROMPT_COMPACTION_CIRCUIT_META_KEY.to_string(),
                json!({
                    "schema": "prompt_compaction_circuit_v1",
                    "status": "open",
                    "reason": "failure_threshold_exceeded",
                    "failureCount": failure_count,
                    "threshold": PROMPT_COMPACTION_CIRCUIT_FAILURE_THRESHOLD,
                    "openedAtMs": now_ms(),
                })
                .to_string(),
            );
        }
        let _ = self.runtime_store.append_event(RuntimeEvent {
            event_id: format!("evt_prompt_compaction_failed_{}_{}", turn_id, now_ms()),
            session_id: session.session_id.clone(),
            task_id: Some(turn_id.to_string()),
            event_type: "prompt.compaction.failed".to_string(),
            at_ms: now_ms(),
            visibility: EventVisibility::Internal,
            payload_json: payload.to_string(),
        });
    }
}
