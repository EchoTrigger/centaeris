use super::driver::TurnControlInput;
use super::*;
use std::time::Duration;

const MAX_OUTPUT_TOKEN_RECOVERY_ATTEMPTS: u8 = 5;
const OUTPUT_TOKEN_RECOVERY_MESSAGE: &str =
    "Output token limit hit. Resume directly from the incomplete response without apology or recap. Break the remaining work into smaller pieces.";
const INCOMPLETE_TOOL_IDENTITY_RECOVERY_MESSAGE: &str =
    "Provider truncated a tool call identity. Do not infer or reuse an id. Re-issue the complete tool call with a fresh valid identity.";
const CANCELLATION_POLL_INTERVAL_MS: u64 = 50;

enum GenerateStep<T> {
    Completed(T),
    Cancelled(String),
}

async fn wait_for_loop_cancellation(
    cancellation_probe: Option<&(dyn Fn() -> Result<Option<String>, String> + Sync)>,
) -> Result<String, String> {
    let Some(cancellation_probe) = cancellation_probe else {
        return std::future::pending().await;
    };
    loop {
        tokio::time::sleep(Duration::from_millis(CANCELLATION_POLL_INTERVAL_MS)).await;
        if let Some(reason) = poll_loop_cancellation(Some(cancellation_probe))? {
            return Ok(reason);
        }
    }
}

struct TurnControlCloseGuard {
    control: Option<TurnControl>,
}

#[derive(Debug, Clone)]
struct PendingAnswerNow {
    intervention: AgentRunInterventionV1,
    applied: bool,
}

#[derive(Debug, Clone)]
struct PendingOutputTokenRecovery {
    partial_content: String,
    message: String,
    rejected_tool_calls: Vec<RejectedToolCallIdentity>,
}

fn rejected_tool_calls_for_recovery(
    calls: &[crate::model::TruncatedToolCall],
) -> (Vec<RejectedToolCallIdentity>, bool) {
    let mut call_id_counts = HashMap::<&str, usize>::new();
    for call in calls {
        if let Some(call_id) = call.call_id.as_deref() {
            *call_id_counts.entry(call_id).or_default() += 1;
        }
    }
    let has_incomplete_identity = calls.iter().any(|call| {
        call.call_id.is_none()
            || call.tool_name.is_none()
            || call.call_id.as_deref().is_some_and(|call_id| {
                call_id_counts.get(call_id).copied().unwrap_or_default() != 1
            })
    });
    let rejected = calls
        .iter()
        .filter_map(|call| {
            let call_id = call.call_id.as_deref()?;
            let tool_name = call.tool_name.as_deref()?;
            (call_id_counts.get(call_id).copied() == Some(1)).then(|| RejectedToolCallIdentity {
                call_id: call_id.to_string(),
                tool_name: tool_name.to_string(),
            })
        })
        .collect();
    (rejected, has_incomplete_identity)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeWaitToolResultFactV1 {
    schema: String,
    continuation_id: String,
    result: ToolExecutionResult,
}

impl TurnControlCloseGuard {
    fn new(control: Option<&TurnControl>) -> Self {
        Self {
            control: control.cloned(),
        }
    }

    fn keep_open(&mut self) {
        self.control = None;
    }
}

impl Drop for TurnControlCloseGuard {
    fn drop(&mut self) {
        if let Some(control) = self.control.as_ref() {
            let _ = control.close_after_loop();
        }
    }
}

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
    pub async fn resume_turn_async(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<TurnStepResult, String> {
        self.resume_turn_with_agent_run_identity_async(session_id, turn_id, None)
            .await
    }

    pub async fn resume_turn_with_agent_run_identity_async(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
    ) -> Result<TurnStepResult, String> {
        self.resume_turn_with_agent_run_identity_and_tool_safe_point_async(
            session_id,
            turn_id,
            agent_run_identity,
            None,
        )
        .await
    }

    async fn resume_turn_with_agent_run_identity_and_tool_safe_point_async(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        _tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<TurnStepResult, String> {
        let session = self.session_manager.load_or_create_session(session_id)?;
        let checkpoint = self
            .checkpoint_store
            .load_checkpoint_by_turn_async(session_id, turn_id)
            .await?;
        if let Some(checkpoint) = checkpoint {
            match checkpoint.done_reason.as_deref() {
                Some("runtime_job") => {
                    return self
                        .resume_runtime_job_wait_async(checkpoint, agent_run_identity)
                        .await
                }
                Some("question") => {
                    return self.consume_question_wait_checkpoint(
                        checkpoint,
                        agent_run_identity,
                        session,
                    )
                }
                Some(other) => return Err(format!("unsupported_runtime_wait_done_reason:{other}")),
                None => return Err("runtime checkpoint missing doneReason".to_string()),
            }
        }

        if let Some(pending) = pending_runtime_tool_batch(&session)? {
            if pending.turn_id == turn_id {
                return self.repair_consumed_runtime_job_wait(session, pending);
            }
        }
        Err("runtime wait checkpoint missing".to_string())
    }

    fn repair_consumed_runtime_job_wait(
        &self,
        mut session: SessionStateSnapshot,
        pending: PendingRuntimeToolBatchV1,
    ) -> Result<TurnStepResult, String> {
        pending.wait_checkpoint.validate()?;
        let turn_id = pending.turn_id.clone();
        let session_id = session.session_id.clone();
        let required_ids = pending
            .wait_checkpoint
            .waits
            .iter()
            .map(|wait| wait.tool_call_id.clone())
            .collect::<Vec<_>>();
        let generate_result =
            generate_result_from_persisted_tool_batch(&session, required_ids.as_slice())?;
        let changed = self.load_resolved_runtime_wait_change(
            session_id.as_str(),
            turn_id.as_str(),
            pending.wait_checkpoint.continuation_id.as_str(),
        )?;
        let mut result_by_id = HashMap::new();
        let mut offset = 0;
        const PAGE_SIZE: usize = 256;
        loop {
            let events = self
                .runtime_store
                .list_events(session_id.as_str(), PAGE_SIZE, offset)
                .map_err(|error| error.to_string())?;
            let count = events.len();
            for event in events.into_iter().filter(|event| {
                event.event_type == "runtime_wait_tool_result.v1"
                    && event.task_id.as_deref() == Some(turn_id.as_str())
            }) {
                let fact = serde_json::from_str::<RuntimeWaitToolResultFactV1>(
                    event.payload_json.as_str(),
                )
                .map_err(|error| format!("decode runtime wait tool result failed: {error}"))?;
                if fact.schema != "runtime_wait_tool_result.v1"
                    || fact.continuation_id != pending.wait_checkpoint.continuation_id
                    || event.event_id
                        != format!(
                            "runtime_wait_result:{}:{}",
                            fact.continuation_id, fact.result.tool_call_id
                        )
                    || result_by_id
                        .insert(fact.result.tool_call_id.clone(), fact.result)
                        .is_some()
                {
                    return Err("runtime wait tool result identity mismatch".to_string());
                }
            }
            if count < PAGE_SIZE {
                break;
            }
            offset += count;
        }
        let tool_results = generate_result
            .tool_calls
            .iter()
            .map(|call| {
                let result = result_by_id.remove(call.id.as_str()).ok_or_else(|| {
                    format!(
                        "runtime wait durable tool result missing: callId={}",
                        call.id
                    )
                })?;
                if result.tool_name != call.name {
                    return Err(format!(
                        "runtime wait durable tool result name mismatch: callId={}",
                        call.id
                    ));
                }
                Ok(result)
            })
            .collect::<Result<Vec<_>, String>>()?;
        if !result_by_id.is_empty() {
            return Err("runtime wait durable tool result contains unknown call".to_string());
        }
        let mut lifecycle_contexts = pending.lifecycle_hook_contexts;
        lifecycle_contexts.extend(self.run_waited_post_tool_use_hooks_exactly_once(
            session_id.as_str(),
            turn_id.as_str(),
            generate_result.tool_calls.as_slice(),
            tool_results.as_slice(),
            pending.wait_checkpoint.waits.as_slice(),
        )?);
        self.repair_tool_batch_transcript(&mut session, &generate_result, tool_results.as_slice())?;
        append_lifecycle_hook_context_messages(
            &self.message_handler,
            &mut session,
            lifecycle_contexts.iter().map(String::as_str),
        );
        let complete_turn =
            self.should_complete_turn_after_tool_success(&generate_result, &tool_results)?;
        if complete_turn {
            mark_terminal_tool_transcript_committed(&mut session, &generate_result)?;
        }
        session.metadata.remove(RUNTIME_PENDING_TOOL_BATCH_META_KEY);
        self.session_manager.save_session(&session)?;
        let tool_operations_json = project_tool_operations_json(&tool_results);
        Ok(TurnStepResult {
            turn_id: turn_id.clone(),
            continuation: if complete_turn {
                QueryContinuation::CompleteTerminalTool
            } else {
                QueryContinuation::ExecuteTools
            },
            checkpoint: None,
            provider_tool_calls: generate_result.tool_calls,
            tool_use_summary: self
                .config
                .enable_tool_use_summary
                .then(|| build_tool_use_summary(&tool_results)),
            tool_operations_json,
            agent_run_resource_usage: pending.agent_run_resource_usage,
            runtime_events: vec![build_runtime_event_runtime_wait_changed(
                session_id.as_str(),
                turn_id.as_str(),
                &changed,
            )?],
            tool_results,
            session_snapshot: session,
        })
    }

    fn consume_question_wait_checkpoint(
        &self,
        checkpoint: CheckpointRecord,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        session: SessionStateSnapshot,
    ) -> Result<TurnStepResult, String> {
        let wait = serde_json::from_str::<
            crate::runtime::contracts::RuntimeAwaitQuestionCheckpointV1,
        >(checkpoint.payload_json.as_str())
        .map_err(|error| format!("decode question wait checkpoint failed: {error}"))?;
        wait.validate()?;
        let agent_run_identity = agent_run_identity
            .ok_or_else(|| "question_wait_resume_requires_agent_run_identity".to_string())?;
        agent_run_identity.validate()?;
        if wait.agent_run_id != agent_run_identity.agent_run_id
            || wait.authorization_digest != agent_run_identity.authorization_digest
            || wait.turn_id != checkpoint.turn_id
        {
            return Err("question wait resume identity mismatch".to_string());
        }
        let changed = RuntimeWaitChangedV1 {
            schema: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            continuation_id: wait.continuation_id.clone(),
            agent_run_id: wait.agent_run_id,
            status: RuntimeWaitStatusV1::Resumed,
            transition_reason: "question_answered".to_string(),
            at_ms: now_ms(),
        };
        changed.validate()?;
        let event = RuntimeEvent {
            event_id: format!("runtime_wait:{}:resumed", wait.continuation_id),
            session_id: checkpoint.session_id.clone(),
            task_id: Some(checkpoint.turn_id.clone()),
            event_type: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            at_ms: changed.at_ms,
            visibility: EventVisibility::User,
            payload_json: serde_json::to_string(&changed)
                .map_err(|error| format!("serialize question wait outcome failed: {error}"))?,
        };
        self.runtime_store.consume_wait_checkpoint(
            crate::session::store::ConsumeWaitCheckpointRequest {
                checkpoint: checkpoint.clone(),
                events: vec![event],
            },
        )?;
        Ok(TurnStepResult {
            turn_id: checkpoint.turn_id.clone(),
            continuation: QueryContinuation::ExecuteTools,
            checkpoint: None,
            provider_tool_calls: Vec::new(),
            tool_results: Vec::new(),
            tool_use_summary: None,
            tool_operations_json: None,
            agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            runtime_events: vec![build_runtime_event_runtime_wait_changed(
                checkpoint.session_id.as_str(),
                checkpoint.turn_id.as_str(),
                &changed,
            )?],
            session_snapshot: session,
        })
    }

    fn restore_pending_runtime_job_wait_if_needed(
        &self,
        session: &SessionStateSnapshot,
        session_id: &str,
        turn_id: &str,
    ) -> Result<(), String> {
        let Some(pending) = pending_runtime_tool_batch(session)? else {
            return Ok(());
        };
        if pending.turn_id != turn_id {
            return Ok(());
        }
        pending.wait_checkpoint.validate()?;
        let current = self
            .runtime_store
            .load_checkpoint_by_turn(session_id, turn_id)
            .map_err(|error| error.to_string())?;
        let (checkpoint, event, _) = runtime_waiting_transition(
            session_id,
            turn_id,
            &pending.wait_checkpoint,
            pending.waiting_at_ms,
        )?;
        match current {
            Some(current) if current == checkpoint => Ok(()),
            Some(_) => Err("runtime_job_wait_checkpoint_identity_conflict".to_string()),
            None => self.runtime_store.save_wait_checkpoint(
                crate::session::store::SaveWaitCheckpointRequest { checkpoint, event },
            ),
        }
    }

    pub(super) async fn resume_runtime_job_wait_async(
        &self,
        checkpoint: CheckpointRecord,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
    ) -> Result<TurnStepResult, String> {
        self.resolve_runtime_job_wait_async(checkpoint, agent_run_identity, None)
            .await
    }

    pub fn pending_runtime_job_wait_identity(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, RuntimeAgentRunIdentityV1)>, String> {
        let session = self.session_manager.load_or_create_session(session_id)?;
        let Some(pending) = pending_runtime_tool_batch(&session)? else {
            return Ok(None);
        };
        pending.wait_checkpoint.validate()?;
        Ok(Some((
            pending.turn_id,
            RuntimeAgentRunIdentityV1 {
                agent_run_id: pending.wait_checkpoint.agent_run_id,
                execution_id: pending.wait_checkpoint.execution_id,
                authorization_digest: pending.wait_checkpoint.authorization_digest,
            },
        )))
    }

    pub async fn abandon_pending_runtime_job_wait_async(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: &RuntimeAgentRunIdentityV1,
        transition_reason: &str,
    ) -> Result<(), String> {
        self.resume_runtime_job_wait_abandoned_async(
            session_id,
            turn_id,
            Some(agent_run_identity),
            transition_reason,
        )
        .await?;
        Ok(())
    }

    async fn resume_runtime_job_wait_for_answer_now_async(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
    ) -> Result<TurnStepResult, String> {
        self.resume_runtime_job_wait_abandoned_async(
            session_id,
            turn_id,
            agent_run_identity,
            "cancelled_by_answer_now",
        )
        .await
    }

    async fn resume_runtime_job_wait_abandoned_async(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        transition_reason: &str,
    ) -> Result<TurnStepResult, String> {
        let checkpoint = self
            .checkpoint_store
            .load_checkpoint_by_turn_async(session_id, turn_id)
            .await?
            .ok_or_else(|| "runtime wait checkpoint missing".to_string())?;
        if checkpoint.done_reason.as_deref() != Some("runtime_job") {
            return Err("runtime_wait_abandon_checkpoint_required".to_string());
        }
        self.resolve_runtime_job_wait_async(checkpoint, agent_run_identity, Some(transition_reason))
            .await
    }

    async fn resume_question_wait_for_answer_now_async(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<TurnStepResult, String> {
        self.resume_question_wait_abandoned_async(
            session_id,
            turn_id,
            "question_cancelled_by_answer_now",
        )
        .await
    }

    async fn resume_question_wait_abandoned_async(
        &self,
        session_id: &str,
        turn_id: &str,
        transition_reason: &str,
    ) -> Result<TurnStepResult, String> {
        let checkpoint = self
            .checkpoint_store
            .load_checkpoint_by_turn_async(session_id, turn_id)
            .await?
            .ok_or_else(|| "question wait checkpoint missing".to_string())?;
        if checkpoint.done_reason.as_deref() != Some("question") {
            return Err("answer_now_question_checkpoint_required".to_string());
        }
        let wait = serde_json::from_str::<
            crate::runtime::contracts::RuntimeAwaitQuestionCheckpointV1,
        >(checkpoint.payload_json.as_str())
        .map_err(|error| format!("decode question checkpoint failed: {error}"))?;
        wait.validate()?;
        let changed = RuntimeWaitChangedV1 {
            schema: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            continuation_id: wait.continuation_id.clone(),
            agent_run_id: wait.agent_run_id,
            status: RuntimeWaitStatusV1::Abandoned,
            transition_reason: transition_reason.to_string(),
            at_ms: now_ms(),
        };
        changed.validate()?;
        let event = RuntimeEvent {
            event_id: format!("runtime_wait:{}:abandoned", wait.continuation_id),
            session_id: session_id.to_string(),
            task_id: Some(turn_id.to_string()),
            event_type: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            at_ms: changed.at_ms,
            visibility: EventVisibility::User,
            payload_json: serde_json::to_string(&changed)
                .map_err(|error| format!("serialize question abandonment failed: {error}"))?,
        };
        self.runtime_store.consume_wait_checkpoint(
            crate::session::store::ConsumeWaitCheckpointRequest {
                checkpoint,
                events: vec![event],
            },
        )?;
        let session = self.session_manager.load_or_create_session(session_id)?;
        Ok(TurnStepResult {
            turn_id: turn_id.to_string(),
            continuation: QueryContinuation::ExecuteTools,
            checkpoint: None,
            provider_tool_calls: Vec::new(),
            tool_results: Vec::new(),
            tool_use_summary: None,
            tool_operations_json: None,
            agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            runtime_events: vec![build_runtime_event_runtime_wait_changed(
                session_id, turn_id, &changed,
            )?],
            session_snapshot: session,
        })
    }

    async fn abandon_wait_for_agent_run_cancellation_async(
        &self,
        continuation: QueryContinuation,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
    ) -> Result<TurnStepResult, String> {
        match continuation {
            QueryContinuation::AwaitRuntimeJob => {
                self.resume_runtime_job_wait_abandoned_async(
                    session_id,
                    turn_id,
                    agent_run_identity,
                    "agent_run_cancelled",
                )
                .await
            }
            QueryContinuation::AwaitQuestion => {
                self.resume_question_wait_abandoned_async(
                    session_id,
                    turn_id,
                    "agent_run_cancelled",
                )
                .await
            }
            _ => Err("AgentRun cancellation requires a wait continuation".to_string()),
        }
    }

    async fn resolve_runtime_job_wait_async(
        &self,
        checkpoint: CheckpointRecord,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        abandon_transition_reason: Option<&str>,
    ) -> Result<TurnStepResult, String> {
        let wait_checkpoint =
            serde_json::from_str::<RuntimeAwaitJobCheckpointV1>(checkpoint.payload_json.as_str())
                .map_err(|error| format!("decode runtime wait checkpoint failed: {error}"))?;
        wait_checkpoint.validate()?;
        let agent_run_identity = agent_run_identity
            .ok_or_else(|| "runtime_job_wait_resume_requires_agent_run_identity".to_string())?;
        agent_run_identity.validate()?;
        if wait_checkpoint.agent_run_id != agent_run_identity.agent_run_id
            || wait_checkpoint.authorization_digest != agent_run_identity.authorization_digest
        {
            return Err("runtime_job_wait_resume_identity_mismatch".to_string());
        }

        let mut session = self
            .session_manager
            .load_or_create_session(checkpoint.session_id.as_str())?;
        let pending = pending_runtime_tool_batch(&session)?
            .ok_or_else(|| "runtime_job_wait_pending_tool_batch_missing".to_string())?;
        if pending.turn_id != checkpoint.turn_id || pending.wait_checkpoint != wait_checkpoint {
            return Err("runtime_job_wait_pending_tool_batch_identity_mismatch".to_string());
        }
        let waited_call_ids = wait_checkpoint
            .waits
            .iter()
            .map(|wait| wait.tool_call_id.clone())
            .collect::<Vec<_>>();
        let generate_result =
            generate_result_from_persisted_tool_batch(&session, waited_call_ids.as_slice())?;
        let calls_by_id = generate_result
            .tool_calls
            .iter()
            .map(|call| (call.id.as_str(), call))
            .collect::<HashMap<_, _>>();
        if calls_by_id.len() != generate_result.tool_calls.len() {
            return Err("runtime_job_wait_route_state_has_duplicate_tool_call_id".to_string());
        }

        let waited_call_id_set = waited_call_ids.iter().collect::<HashSet<_>>();
        let mut terminal_by_call_id = HashMap::new();
        for call in generate_result
            .tool_calls
            .iter()
            .filter(|call| !waited_call_id_set.contains(&call.id))
        {
            let recovered = self.recover_interrupted_tool_execution_result(
                checkpoint.session_id.as_str(),
                &crate::runtime::contracts::ToolCall {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    args_json: call.args_json.clone(),
                },
            )?;
            let (_, _, result) = recovered.ok_or_else(|| {
                format!(
                    "runtime_job_wait_sibling_tool_receipt_missing: callId={}",
                    call.id
                )
            })?;
            terminal_by_call_id.insert(call.id.clone(), result);
        }
        let mut waiting = false;
        let mut abandoned_any = false;
        for wait in &wait_checkpoint.waits {
            let call = calls_by_id.get(wait.tool_call_id.as_str()).ok_or_else(|| {
                format!(
                    "runtime_job_wait_tool_call_missing: callId={}",
                    wait.tool_call_id
                )
            })?;
            if call.name != wait.source_tool_name {
                return Err(format!(
                    "runtime_job_wait_tool_identity_mismatch: callId={} expected={} actual={}",
                    wait.tool_call_id, wait.source_tool_name, call.name
                ));
            }
            let current_definition_digest =
                self.projected_tool_definition_digest(&session, wait.source_tool_name.as_str())?;
            if current_definition_digest != wait.tool_definition_digest {
                return Err(format!(
                    "runtime_job_wait_tool_definition_digest_mismatch: callId={}",
                    wait.tool_call_id
                ));
            }
            let job = self
                .runtime_store
                .get_runtime_job(wait.job_id.as_str())?
                .ok_or_else(|| format!("runtime_job_wait_job_missing: jobId={}", wait.job_id))?;
            if job.job_kind != wait.job_kind
                || !runtime_job_session_scope_matches(
                    job.session_id.as_deref(),
                    checkpoint.session_id.as_str(),
                )
            {
                return Err(format!(
                    "runtime_job_wait_job_identity_mismatch: jobId={}",
                    wait.job_id
                ));
            }
            if !job.status.is_terminal() {
                if let Some(transition_reason) = abandon_transition_reason {
                    abandoned_any = true;
                    if terminal_by_call_id
                        .insert(
                            wait.tool_call_id.clone(),
                            runtime_job_abandoned_tool_result(wait, &job, transition_reason),
                        )
                        .is_some()
                    {
                        return Err(format!(
                            "runtime_job_wait_duplicate_abandoned_tool_result: callId={}",
                            wait.tool_call_id
                        ));
                    }
                } else {
                    waiting = true;
                }
                continue;
            }
            let result = self.runtime_job_terminal_tool_result(
                checkpoint.session_id.as_str(),
                checkpoint.turn_id.as_str(),
                wait,
                &job,
            )?;
            if terminal_by_call_id
                .insert(wait.tool_call_id.clone(), result)
                .is_some()
            {
                return Err(format!(
                    "runtime_job_wait_duplicate_terminal_tool_result: callId={}",
                    wait.tool_call_id
                ));
            }
        }
        if waiting {
            return Ok(TurnStepResult {
                turn_id: checkpoint.turn_id.clone(),
                continuation: QueryContinuation::AwaitRuntimeJob,
                checkpoint: Some(checkpoint),
                provider_tool_calls: generate_result.tool_calls,
                tool_results: vec![],
                tool_use_summary: None,
                tool_operations_json: None,
                agent_run_resource_usage: pending.agent_run_resource_usage,
                runtime_events: Vec::new(),
                session_snapshot: session,
            });
        }

        if terminal_by_call_id.len() != generate_result.tool_calls.len() {
            return Err(format!(
                "runtime_job_wait_terminal_result_count_mismatch: calls={} results={}",
                generate_result.tool_calls.len(),
                terminal_by_call_id.len()
            ));
        }
        let tool_results = generate_result
            .tool_calls
            .iter()
            .map(|call| {
                terminal_by_call_id.remove(call.id.as_str()).ok_or_else(|| {
                    format!(
                        "runtime_job_wait_terminal_result_missing: callId={}",
                        call.id
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut lifecycle_hook_contexts = pending.lifecycle_hook_contexts;
        lifecycle_hook_contexts.extend(self.run_waited_post_tool_use_hooks_exactly_once(
            checkpoint.session_id.as_str(),
            checkpoint.turn_id.as_str(),
            generate_result.tool_calls.as_slice(),
            tool_results.as_slice(),
            wait_checkpoint.waits.as_slice(),
        )?);
        let tool_operations_json = project_tool_operations_json(&tool_results);
        let tool_use_summary = self
            .config
            .enable_tool_use_summary
            .then(|| build_tool_use_summary(&tool_results));
        let complete_turn = self
            .should_complete_turn_after_tool_success(&generate_result, tool_results.as_slice())?;
        let at_ms = now_ms();
        let wait_status = if abandoned_any {
            RuntimeWaitStatusV1::Abandoned
        } else {
            RuntimeWaitStatusV1::Resumed
        };
        let wait_transition_reason = abandon_transition_reason
            .filter(|_| abandoned_any)
            .unwrap_or("runtime_jobs_terminal");
        let changed = RuntimeWaitChangedV1 {
            schema: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            continuation_id: wait_checkpoint.continuation_id.clone(),
            agent_run_id: wait_checkpoint.agent_run_id.clone(),
            status: wait_status,
            transition_reason: wait_transition_reason.to_string(),
            at_ms,
        };
        changed.validate()?;
        let resumed_event = RuntimeEvent {
            event_id: format!(
                "runtime_wait:{}:{}",
                wait_checkpoint.continuation_id,
                runtime_wait_status_name(wait_status)
            ),
            session_id: checkpoint.session_id.clone(),
            task_id: Some(checkpoint.turn_id.clone()),
            event_type: crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.to_string(),
            at_ms,
            visibility: EventVisibility::User,
            payload_json: serde_json::to_string(&changed)
                .map_err(|error| format!("serialize runtime wait resumed event failed: {error}"))?,
        };
        let mut durable_events = tool_results
            .iter()
            .map(|result| {
                Ok(RuntimeEvent {
                    event_id: format!(
                        "runtime_wait_result:{}:{}",
                        wait_checkpoint.continuation_id, result.tool_call_id
                    ),
                    session_id: checkpoint.session_id.clone(),
                    task_id: Some(checkpoint.turn_id.clone()),
                    event_type: "runtime_wait_tool_result.v1".to_string(),
                    at_ms,
                    visibility: EventVisibility::Internal,
                    payload_json: json!({
                        "schema": "runtime_wait_tool_result.v1",
                        "continuationId": wait_checkpoint.continuation_id,
                        "result": result,
                    })
                    .to_string(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        durable_events.push(resumed_event.clone());
        self.runtime_store.consume_wait_checkpoint(
            crate::session::store::ConsumeWaitCheckpointRequest {
                checkpoint: checkpoint.clone(),
                events: durable_events,
            },
        )?;

        self.repair_tool_batch_transcript(&mut session, &generate_result, tool_results.as_slice())?;
        append_lifecycle_hook_context_messages(
            &self.message_handler,
            &mut session,
            lifecycle_hook_contexts.iter().map(String::as_str),
        );
        if complete_turn {
            mark_terminal_tool_transcript_committed(&mut session, &generate_result)?;
        }
        session.metadata.remove(RUNTIME_PENDING_TOOL_BATCH_META_KEY);
        self.session_manager.save_session(&session)?;

        let mut runtime_events = Vec::new();
        runtime_events.push(build_runtime_event_runtime_wait_changed(
            checkpoint.session_id.as_str(),
            checkpoint.turn_id.as_str(),
            &changed,
        )?);
        let tool_result_events = build_runtime_event_tool_result_events(
            checkpoint.session_id.as_str(),
            checkpoint.turn_id.as_str(),
            tool_results.as_slice(),
            tool_operations_json.as_deref(),
        )?;
        runtime_events.extend(tool_result_events.clone());
        runtime_events.extend(
            build_runtime_event_subagent_tool_group_events_from_tool_results(
                checkpoint.session_id.as_str(),
                checkpoint.turn_id.as_str(),
                tool_results.as_slice(),
                tool_result_events.as_slice(),
            ),
        );
        Ok(TurnStepResult {
            turn_id: checkpoint.turn_id.clone(),
            continuation: if complete_turn {
                QueryContinuation::CompleteTerminalTool
            } else {
                QueryContinuation::ExecuteTools
            },
            checkpoint: None,
            provider_tool_calls: generate_result.tool_calls,
            tool_results,
            tool_use_summary,
            tool_operations_json,
            agent_run_resource_usage: pending.agent_run_resource_usage,
            runtime_events,
            session_snapshot: session,
        })
    }

    fn load_resolved_runtime_wait_change(
        &self,
        session_id: &str,
        turn_id: &str,
        continuation_id: &str,
    ) -> Result<RuntimeWaitChangedV1, String> {
        const PAGE_SIZE: usize = 256;
        let mut offset = 0usize;
        let mut resolved = None;
        loop {
            let events = self
                .runtime_store
                .list_events(session_id, PAGE_SIZE, offset)
                .map_err(|error| error.to_string())?;
            let event_count = events.len();
            for event in events.into_iter().filter(|event| {
                event.event_type == crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1
            }) {
                let change =
                    serde_json::from_str::<RuntimeWaitChangedV1>(event.payload_json.as_str())
                        .map_err(|error| format!("decode runtime wait event failed: {error}"))?;
                change.validate()?;
                let expected_event_id = format!(
                    "runtime_wait:{}:{}",
                    change.continuation_id,
                    runtime_wait_status_name(change.status)
                );
                if event.event_id != expected_event_id || event.session_id != session_id {
                    return Err("runtime wait event identity mismatch".to_string());
                }
                if change.continuation_id != continuation_id
                    || change.status == RuntimeWaitStatusV1::Waiting
                {
                    continue;
                }
                if event.task_id.as_deref() != Some(turn_id) || resolved.replace(change).is_some() {
                    return Err("resolved runtime wait event identity mismatch".to_string());
                }
            }
            if event_count < PAGE_SIZE {
                break;
            }
            offset = offset.saturating_add(PAGE_SIZE);
        }
        resolved.ok_or_else(|| "resolved runtime wait event is missing".to_string())
    }

    pub(super) fn runtime_job_terminal_tool_result(
        &self,
        session_id: &str,
        turn_id: &str,
        wait: &RuntimeJobWaitV1,
        job: &RuntimeJobRecord,
    ) -> Result<ToolExecutionResult, String> {
        let (status, content, details, facts, error, transition_reason) = match job.status {
            RuntimeJobStatus::Succeeded => {
                let objects = if job.job_kind == SUBAGENT_RUN_JOB_KIND {
                    self.load_subagent_runtime_job_result_objects(session_id, wait, job)?
                } else {
                    job.output_refs
                        .iter()
                        .map(|object_id| {
                        let link = self
                            .runtime_store
                            .load_external_context_object_link(
                                session_id,
                                object_id.as_str(),
                                turn_id,
                                wait.tool_call_id.as_str(),
                            )?
                            .ok_or_else(|| {
                                format!(
                                    "runtime_job_wait_output_ref_scope_mismatch: jobId={} objectId={object_id}",
                                    job.job_id
                                )
                            })?;
                        if link.source_tool_name != wait.source_tool_name {
                            return Err(format!(
                                "runtime_job_wait_output_ref_tool_mismatch: jobId={} objectId={object_id}",
                                job.job_id
                            ));
                        }
                        let object = self.runtime_store
                            .load_external_context_object(object_id.as_str())?
                            .ok_or_else(|| {
                                format!(
                                    "runtime_job_wait_output_ref_missing: jobId={} objectId={object_id}",
                                    job.job_id
                                )
                            })?;
                        if object.source_provider_id != link.source_provider_id
                            || object.source_tool_name != link.source_tool_name
                        {
                            return Err(format!(
                                "runtime_job_wait_output_ref_source_mismatch: jobId={} objectId={object_id}",
                                job.job_id
                            ));
                        }
                            Ok(object)
                        })
                        .collect::<Result<Vec<_>, String>>()?
                };
                let content = if objects.is_empty() {
                    "Background tool completed without model-visible output.".to_string()
                } else {
                    objects
                        .iter()
                        .map(|object| object.content.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                };
                let facts = tool_execution_facts_from_external_objects(objects.as_slice())?;
                (
                    "ok",
                    content,
                    json!({
                        "schema": "runtime_job_tool_result.v1",
                        "jobId": job.job_id,
                        "jobKind": job.job_kind,
                        "outputRefs": job.output_refs,
                        "objectId": job.output_refs.first(),
                        "externalObjects": objects,
                    }),
                    facts,
                    None,
                    "runtime_job_succeeded",
                )
            }
            RuntimeJobStatus::Cancelled => (
                "cancelled",
                "The background tool operation was cancelled before completion.".to_string(),
                json!({
                    "schema": "runtime_job_tool_result.v1",
                    "jobId": job.job_id,
                    "jobKind": job.job_kind,
                    "status": "cancelled",
                }),
                Vec::new(),
                Some(
                    ToolErrorInfo::new(
                        ToolFailureKind::Cancelled,
                        "background tool operation was cancelled",
                        "Background tool operation cancelled",
                    )
                    .with_diagnostic(format!("runtime_job:{}", job.job_id)),
                ),
                "runtime_job_cancelled",
            ),
            RuntimeJobStatus::Failed | RuntimeJobStatus::DeadLettered => (
                "error",
                "The background tool operation failed and produced no result.".to_string(),
                json!({
                    "schema": "runtime_job_tool_result.v1",
                    "jobId": job.job_id,
                    "jobKind": job.job_kind,
                    "status": if job.status == RuntimeJobStatus::DeadLettered {
                        "dead_lettered"
                    } else {
                        "failed"
                    },
                }),
                Vec::new(),
                Some(
                    ToolErrorInfo::new(
                        ToolFailureKind::ProviderError,
                        "background tool operation failed",
                        "Background tool operation failed",
                    )
                    .with_diagnostic(format!("runtime_job:{}", job.job_id)),
                ),
                if job.status == RuntimeJobStatus::DeadLettered {
                    "runtime_job_dead_lettered"
                } else {
                    "runtime_job_failed"
                },
            ),
            RuntimeJobStatus::Queued | RuntimeJobStatus::Leased | RuntimeJobStatus::Running => {
                return Err(format!(
                    "runtime_job_wait_non_terminal_result_requested: jobId={}",
                    job.job_id
                ));
            }
        };
        Ok(ToolExecutionResult {
            tool_call_id: wait.tool_call_id.clone(),
            tool_name: wait.source_tool_name.clone(),
            status: status.to_string(),
            content,
            details,
            facts,
            error,
            started_at_ms: job.created_at_ms,
            completed_at_ms: job.updated_at_ms,
            latency_ms: job.updated_at_ms.saturating_sub(job.created_at_ms),
            parallel_group: None,
            transition_reason: Some(transition_reason.to_string()),
        })
    }

    fn load_subagent_runtime_job_result_objects(
        &self,
        session_id: &str,
        wait: &RuntimeJobWaitV1,
        job: &RuntimeJobRecord,
    ) -> Result<Vec<ExternalContextObject>, String> {
        if wait.source_tool_name != "task_output" {
            return Err(format!(
                "subagent_runtime_job_wait_tool_mismatch: jobId={} expected=task_output actual={}",
                job.job_id, wait.source_tool_name
            ));
        }
        let expected_result_ref =
            runtime_external_context_keys::subagent_result_ref(job.job_id.as_str());
        if job.output_refs.len() != 1 || job.output_refs[0] != expected_result_ref {
            return Err(format!(
                "subagent_runtime_job_output_ref_mismatch: jobId={} expected={} actual={:?}",
                job.job_id, expected_result_ref, job.output_refs
            ));
        }
        let object = self
            .runtime_store
            .load_external_context_object(expected_result_ref.as_str())?
            .ok_or_else(|| {
                format!(
                    "subagent_runtime_job_result_missing: jobId={} objectId={expected_result_ref}",
                    job.job_id
                )
            })?;
        if object.object_kind != "subagent_result"
            || object.source_provider_id != "centaeris.core"
            || object.source_tool_name != "agent"
        {
            return Err(format!(
                "subagent_runtime_job_result_source_mismatch: jobId={} objectId={expected_result_ref}",
                job.job_id
            ));
        }
        let work_packet = load_subagent_work_packet(&self.runtime_store, job)?;
        let binding = subagent_work_packet_runtime_binding(&work_packet, job)?;
        for (field, expected) in [
            ("schema", "subagent_result_v1"),
            ("runtimeJobId", job.job_id.as_str()),
            ("parentSessionId", session_id),
            ("parentTurnId", binding.parent_turn_id.as_str()),
            ("subagentId", binding.subagent_id.as_str()),
            ("childSessionId", binding.child_session_id.as_str()),
        ] {
            if object.metadata.get(field).and_then(Value::as_str) != Some(expected) {
                return Err(format!(
                    "subagent_runtime_job_result_{field}_mismatch: jobId={} objectId={expected_result_ref}",
                    job.job_id
                ));
            }
        }
        Ok(vec![object])
    }

    fn repair_tool_batch_transcript(
        &self,
        session: &mut SessionStateSnapshot,
        generate_result: &GenerateResult,
        tool_results: &[ToolExecutionResult],
    ) -> Result<bool, String> {
        if generate_result.tool_calls.len() != tool_results.len() {
            return Err(format!(
                "tool batch transcript repair count mismatch: calls={} results={}",
                generate_result.tool_calls.len(),
                tool_results.len()
            ));
        }
        for (call, result) in generate_result.tool_calls.iter().zip(tool_results) {
            if call.id != result.tool_call_id || call.name != result.tool_name {
                return Err(format!(
                    "tool batch transcript repair identity mismatch: expectedCallId={} actualCallId={} expectedTool={} actualTool={}",
                    call.id, result.tool_call_id, call.name, result.tool_name
                ));
            }
        }
        let mut changed = ensure_model_assistant_semantics_message(
            &self.message_handler,
            session,
            generate_result,
        )?;
        let assistant_index = session
            .messages
            .iter()
            .position(|message| {
                matches!(
                    session.model_semantics.get(message.message_id.as_str()),
                    Some(ModelMessageSemanticsV1::Assistant { tool_calls, .. })
                        if tool_calls.len() == generate_result.tool_calls.len()
                            && tool_calls.iter().zip(&generate_result.tool_calls).all(|(actual, expected)| actual.id == expected.id)
                )
            })
            .ok_or_else(|| "tool batch transcript assistant message missing".to_string())?;

        let mut first_missing = None;
        for (offset, result) in tool_results.iter().enumerate() {
            let indices = session
                .messages
                .iter()
                .enumerate()
                .filter_map(|(index, message)| {
                    matches!(
                        session.model_semantics.get(message.message_id.as_str()),
                        Some(ModelMessageSemanticsV1::ToolResult { tool_call_id, .. })
                            if tool_call_id == &result.tool_call_id
                    )
                    .then_some(index)
                })
                .collect::<Vec<_>>();
            match indices.as_slice() {
                [] => {
                    first_missing = Some(offset);
                    break;
                }
                [index] if *index == assistant_index + offset + 1 => {
                    let mut expected = SessionStateSnapshot::new(session.session_id.clone(), 0);
                    tool_context_writer::write_tool_results_to_context(
                        &self.message_handler,
                        &mut expected,
                        std::slice::from_ref(result),
                    )?;
                    let expected_message = &expected.messages[0];
                    let actual_message = &session.messages[*index];
                    if actual_message.role != expected_message.role
                        || actual_message.content != expected_message.content
                        || session.model_semantics_for(actual_message.message_id.as_str())?
                            != expected.model_semantics_for(expected_message.message_id.as_str())?
                    {
                        return Err(format!(
                            "tool batch transcript result conflict: callId={}",
                            result.tool_call_id
                        ));
                    }
                }
                [index] => {
                    return Err(format!(
                        "tool batch transcript result order mismatch: callId={} expectedIndex={} actualIndex={index}",
                        result.tool_call_id,
                        assistant_index + offset + 1
                    ));
                }
                _ => {
                    return Err(format!(
                        "tool batch transcript duplicate result: callId={} count={}",
                        result.tool_call_id,
                        indices.len()
                    ));
                }
            }
        }
        if let Some(first_missing) = first_missing {
            for later in &tool_results[first_missing + 1..] {
                if session.messages.iter().any(|message| {
                    matches!(
                        session.model_semantics.get(message.message_id.as_str()),
                        Some(ModelMessageSemanticsV1::ToolResult { tool_call_id, .. })
                            if tool_call_id == &later.tool_call_id
                    )
                }) {
                    return Err(format!(
                        "tool batch transcript has a result after a missing predecessor: callId={}",
                        later.tool_call_id
                    ));
                }
            }
            tool_context_writer::write_tool_results_to_context(
                &self.message_handler,
                session,
                &tool_results[first_missing..],
            )?;
            changed = true;
        }
        Ok(changed)
    }

    #[cfg(test)]
    pub(crate) async fn process_turn_loop_online_with_model_client_async<
        M: ModelClient,
        CfgStore: ModelSessionConfigStore,
    >(
        &self,
        req: AgentRunRequest,
        model_client: &M,
        session_config_store: &CfgStore,
    ) -> Result<AgentRunResult, String> {
        let driver = ModelClientGenerateDriver::new(model_client, session_config_store);
        self.process_turn_loop_with_async_driver_and_sink_cancellable(
            req, &driver, None, None, None, None,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn process_turn_loop_online_with_model_client_stream_cancellable_async<
        M: ModelClient,
        CfgStore: ModelSessionConfigStore,
    >(
        &self,
        req: AgentRunRequest,
        model_client: &M,
        session_config_store: &CfgStore,
        stream_sink: &mut (dyn FnMut(TurnUpdate) + Send),
        cancellation_probe: &(dyn Fn() -> Result<Option<String>, String> + Sync),
    ) -> Result<AgentRunResult, String> {
        let driver = ModelClientGenerateDriver::new(model_client, session_config_store);
        self.process_turn_loop_with_async_driver_and_sink_cancellable(
            req,
            &driver,
            Some(stream_sink),
            Some(cancellation_probe),
            None,
            None,
        )
        .await
    }

    pub async fn process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async<
        M: ModelClient,
        CfgStore: ModelSessionConfigStore,
    >(
        &self,
        req: AgentRunRequest,
        model_client: &M,
        session_config_store: &CfgStore,
        stream_sink: &mut (dyn FnMut(TurnUpdate) + Send),
        cancellation_probe: &(dyn Fn() -> Result<Option<String>, String> + Sync),
        tool_safe_point: &mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send),
    ) -> Result<AgentRunResult, String> {
        let tool_safe_point = ToolSafePointDispatcher {
            sink: Mutex::new(tool_safe_point),
        };
        let composition_environment = self.agent_composition_environment()?;
        let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
            model_client,
            session_config_store,
            &tool_safe_point,
            &composition_environment,
        );
        self.process_turn_loop_with_async_driver_and_sink_cancellable(
            req,
            &driver,
            Some(stream_sink),
            Some(cancellation_probe),
            None,
            Some(&tool_safe_point),
        )
        .await
    }

    pub async fn compact_session_online_with_model_client_and_tool_safe_point_async<
        M: ModelClient,
        CfgStore: ModelSessionConfigStore,
    >(
        &self,
        session_id: &str,
        turn_id: &str,
        model_client: &M,
        session_config_store: &CfgStore,
        tool_safe_point: &mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send),
    ) -> Result<bool, String> {
        let tool_safe_point = ToolSafePointDispatcher {
            sink: Mutex::new(tool_safe_point),
        };
        let composition_environment = self.agent_composition_environment()?;
        let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
            model_client,
            session_config_store,
            &tool_safe_point,
            &composition_environment,
        );
        self.compact_session_with_async_driver(session_id, turn_id, &driver)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn process_turn_loop_online_with_model_client_stream_controlled_async<
        M: ModelClient,
        CfgStore: ModelSessionConfigStore,
    >(
        &self,
        req: AgentRunRequest,
        model_client: &M,
        session_config_store: &CfgStore,
        stream_sink: &mut (dyn FnMut(TurnUpdate) + Send),
        cancellation_probe: &(dyn Fn() -> Result<Option<String>, String> + Sync),
        turn_control: &TurnControl,
    ) -> Result<AgentRunResult, String> {
        let driver = ModelClientGenerateDriver::new(model_client, session_config_store);
        self.process_turn_loop_with_async_driver_and_sink_cancellable(
            req,
            &driver,
            Some(stream_sink),
            Some(cancellation_probe),
            Some(turn_control),
            None,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "query loop boundary keeps clients and safe points explicit"
    )]
    pub async fn process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async<
        M: ModelClient,
        CfgStore: ModelSessionConfigStore,
    >(
        &self,
        req: AgentRunRequest,
        model_client: &M,
        session_config_store: &CfgStore,
        stream_sink: &mut (dyn FnMut(TurnUpdate) + Send),
        cancellation_probe: &(dyn Fn() -> Result<Option<String>, String> + Sync),
        turn_control: &TurnControl,
        tool_safe_point: &mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send),
    ) -> Result<AgentRunResult, String> {
        let tool_safe_point = ToolSafePointDispatcher {
            sink: Mutex::new(tool_safe_point),
        };
        let composition_environment = self.agent_composition_environment()?;
        let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
            model_client,
            session_config_store,
            &tool_safe_point,
            &composition_environment,
        );
        self.process_turn_loop_with_async_driver_and_sink_cancellable(
            req,
            &driver,
            Some(stream_sink),
            Some(cancellation_probe),
            Some(turn_control),
            Some(&tool_safe_point),
        )
        .await
    }

    pub async fn run_subagent_worker_with_model_client_async<
        M: ModelClient,
        CfgStore: ModelSessionConfigStore,
    >(
        &self,
        req: SubagentWorkerRunRequest,
        model_client: &M,
        session_config_store: &CfgStore,
        config: &AgentRuntimeSubagentRunnerConfig,
        stream_sink: Option<&mut (dyn FnMut(TurnUpdate) + Send)>,
        tool_safe_point: Option<&mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send)>,
    ) -> SubagentWorkerRunOutcome {
        let request = match build_subagent_query_loop_request(&req, config) {
            Ok(request) => request,
            Err(error) => return SubagentWorkerRunOutcome::Failed { error, retry: None },
        };
        let job_id = req.job.job_id.clone();
        let cancellation_probe = || self.subagent_worker_cancellation_reason(job_id.as_str());
        let result = if let Some(tool_safe_point) = tool_safe_point {
            let tool_safe_point = ToolSafePointDispatcher {
                sink: Mutex::new(tool_safe_point),
            };
            let composition_environment = match self.agent_composition_environment() {
                Ok(environment) => environment,
                Err(error) => return SubagentWorkerRunOutcome::Failed { error, retry: None },
            };
            let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
                model_client,
                session_config_store,
                &tool_safe_point,
                &composition_environment,
            );
            self.process_turn_loop_with_async_driver_and_sink_cancellable(
                request,
                &driver,
                stream_sink,
                Some(&cancellation_probe),
                None,
                Some(&tool_safe_point),
            )
            .await
        } else {
            let driver = ModelClientGenerateDriver::new(model_client, session_config_store);
            self.process_turn_loop_with_async_driver_and_sink_cancellable(
                request,
                &driver,
                stream_sink,
                Some(&cancellation_probe),
                None,
                None,
            )
            .await
        };
        match result {
            Ok(response) => subagent_worker_outcome_from_query_loop_response(response),
            Err(error) => SubagentWorkerRunOutcome::Failed { error, retry: None },
        }
    }

    pub(super) fn subagent_worker_cancellation_reason(
        &self,
        job_id: &str,
    ) -> Result<Option<String>, String> {
        let Some(job) = self.runtime_store.get_runtime_job(job_id)? else {
            return Ok(None);
        };
        if job.status != RuntimeJobStatus::Cancelled {
            return Ok(None);
        }
        Ok(Some(
            job.last_error
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("subagent_cancelled")
                .to_string(),
        ))
    }

    async fn process_turn_request_with_stream_tracking_and_tool_safe_point_async(
        &self,
        turn_req: ProcessTurnRequest,
        stream_sink: &mut (dyn FnMut(TurnUpdate) + Send),
        streamed_session_event_ids: &mut HashSet<String>,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<TurnStepResult, String> {
        let mut tracked_sink = |event: TurnUpdate| {
            track_streamed_runtime_event_id(&event, streamed_session_event_ids);
            stream_sink(event);
        };
        self.process_turn_with_stream_sink_and_tool_safe_point_async(
            turn_req,
            Some(&mut tracked_sink),
            tool_safe_point,
        )
        .await
    }

    pub(super) fn build_tool_batch_executor_async(&self) -> ToolBatchExecutor {
        let async_tools_port = self.tools_port.clone();
        let async_execute_tool = Arc::new(move |request: ToolInvocationRequest| {
            let tools_port = async_tools_port.clone();
            Box::pin(async move { tools_port.execute_async(request).await })
                as tool_batch_executor::AsyncToolExecutionFuture
        });

        ToolBatchExecutor::new_with_executors_and_coordinator(
            self.tools_port.clone(),
            self.tool_concurrency.clone(),
            async_execute_tool,
        )
    }

    pub fn persist_answer_now_requested(
        &self,
        session_id: &str,
        display_turn_id: &str,
        intervention: &AgentRunInterventionV1,
        actor_id: &str,
    ) -> Result<RuntimeEventProjection, String> {
        persist_answer_now_requested_fact(
            &self.runtime_store,
            session_id,
            display_turn_id,
            intervention,
            actor_id,
        )
    }

    fn list_agent_run_intervention_changes(
        &self,
        session_id: &str,
    ) -> Result<Vec<AgentRunInterventionChangedV1>, String> {
        list_agent_run_intervention_changes(&self.runtime_store, session_id)
    }

    fn pending_answer_now_from_changes(
        &self,
        changes: &[AgentRunInterventionChangedV1],
        agent_run_id: &str,
    ) -> Result<Option<PendingAnswerNow>, String> {
        pending_answer_now_from_changes(changes, agent_run_id)
    }

    fn persist_agent_run_intervention_change(
        &self,
        session_id: &str,
        turn_id: &str,
        change: &AgentRunInterventionChangedV1,
        _display_turn_id: &str,
    ) -> Result<Vec<RuntimeEventProjection>, String> {
        change.validate()?;
        let changes = self.list_agent_run_intervention_changes(session_id)?;
        let matching = changes
            .iter()
            .filter(|existing| existing.intervention_id == change.intervention_id)
            .collect::<Vec<_>>();
        if matching.iter().any(|existing| {
            existing.agent_run_id != change.agent_run_id || existing.kind != change.kind
        }) {
            return Err("agent_run_intervention_idempotency_conflict".to_string());
        }
        let has_requested = matching
            .iter()
            .any(|existing| existing.status == AgentRunInterventionStatusV1::Requested);
        let has_terminal_transition = matching.iter().any(|existing| {
            matches!(
                existing.status,
                AgentRunInterventionStatusV1::Applied
                    | AgentRunInterventionStatusV1::SatisfiedByFinal
            )
        });
        if !has_requested {
            return Err("agent_run_intervention_invalid_state_transition".to_string());
        }
        let requested_event = load_agent_run_intervention_projection(
            &self.runtime_store,
            session_id,
            change.intervention_id.as_str(),
            AgentRunInterventionStatusV1::Requested,
        )?;
        if let Some(existing) = matching
            .iter()
            .find(|existing| existing.status == change.status)
        {
            let current_event = load_agent_run_intervention_projection(
                &self.runtime_store,
                session_id,
                change.intervention_id.as_str(),
                existing.status,
            )?;
            return Ok(vec![requested_event, current_event]);
        }
        if has_terminal_transition {
            return Err("agent_run_intervention_invalid_state_transition".to_string());
        }
        let event = runtime_event_for_agent_run_intervention(session_id, turn_id, change)?;
        self.runtime_store
            .append_event_idempotent(event)
            .map_err(|error| error.to_string())?;
        let current_event =
            build_runtime_event_agent_run_intervention_changed(session_id, turn_id, change)?;
        Ok(vec![requested_event, current_event])
    }

    pub(super) async fn process_turn_loop_with_async_driver_and_sink_cancellable<
        D: AsyncGenerateDriver,
    >(
        &self,
        req: AgentRunRequest,
        driver: &D,
        mut stream_sink: Option<&mut (dyn FnMut(TurnUpdate) + Send)>,
        cancellation_probe: Option<&(dyn Fn() -> Result<Option<String>, String> + Sync)>,
        turn_control: Option<&TurnControl>,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<AgentRunResult, String> {
        let AgentRunRequest {
            session_id,
            initial_turn_id,
            user_message,
            agent_run_identity,
            runtime_scope,
            resume_from_turn_id,
            auto_continue_after_resume_wait,
        } = req;
        let _active_agent_run_guard = self.acquire_active_agent_run(session_id.as_str())?;
        let mut turn_control_close_guard = TurnControlCloseGuard::new(turn_control);

        let mut responses = vec![];
        let mut next_turn_id = initial_turn_id.clone();
        let mut next_turn_supplements = Vec::new();
        let mut pending_answer_now = agent_run_identity
            .as_ref()
            .map(|identity| {
                self.list_agent_run_intervention_changes(session_id.as_str())
                    .and_then(|changes| {
                        self.pending_answer_now_from_changes(
                            changes.as_slice(),
                            identity.agent_run_id.as_str(),
                        )
                    })
            })
            .transpose()?
            .flatten();
        let root_user_message = user_message.clone();
        let mut agent_run_resource_usage = AgentRunResourceUsageV1::default();
        let auto_continue_after_resume_wait =
            auto_continue_after_resume_wait.unwrap_or(self.config.auto_continue_after_resume_wait);

        let initial_cancellation_reason = poll_loop_cancellation(cancellation_probe)?;

        let resume_session = self
            .session_manager
            .load_or_create_session(session_id.as_str())?;
        let pending_runtime_wait_turn_id =
            pending_runtime_tool_batch(&resume_session)?.map(|pending| pending.turn_id);

        let effective_resume_turn_id = if let Some(resume_turn_id) = resume_from_turn_id {
            Some(resume_turn_id)
        } else if pending_runtime_wait_turn_id.is_some() {
            pending_runtime_wait_turn_id
        } else {
            self.checkpoint_store
                .load_checkpoint_by_turn_async(session_id.as_str(), initial_turn_id.as_str())
                .await?
                .filter(|checkpoint| checkpoint.done_reason.as_deref() == Some("runtime_job"))
                .map(|checkpoint| checkpoint.turn_id)
        };

        if let Some(resume_turn_id) = effective_resume_turn_id.as_deref() {
            self.restore_pending_runtime_job_wait_if_needed(
                &resume_session,
                session_id.as_str(),
                resume_turn_id,
            )?;
            let resume_done_reason = self
                .checkpoint_store
                .load_checkpoint_by_turn_async(session_id.as_str(), resume_turn_id)
                .await?
                .and_then(|checkpoint| checkpoint.done_reason);
            if let Some(reason) = initial_cancellation_reason.as_deref() {
                if reason == "agent_run_cancel_requested" {
                    let continuation = match resume_done_reason.as_deref() {
                        Some("runtime_job") => Some(QueryContinuation::AwaitRuntimeJob),
                        Some("question") => Some(QueryContinuation::AwaitQuestion),
                        _ => None,
                    };
                    if let Some(continuation) = continuation {
                        let resumed = self
                            .abandon_wait_for_agent_run_cancellation_async(
                                continuation,
                                session_id.as_str(),
                                resume_turn_id,
                                agent_run_identity.as_ref(),
                            )
                            .await?;
                        if !resumed.tool_results.is_empty() {
                            if let Some(sink) = tool_safe_point {
                                sink.commit(ToolSafePoint::CompletedTurn(resumed.clone()))?;
                            }
                        }
                        if let Some(sink) = stream_sink.as_deref_mut() {
                            emit_runtime_events_to_stream(resumed.runtime_events.as_slice(), sink);
                        }
                        responses.push(resumed);
                    }
                }
                return Ok(AgentRunResult::new(
                    responses,
                    AgentRunStop::Cancelled(reason.to_string()),
                ));
            }
            let resumed = match (pending_answer_now.is_some(), resume_done_reason.as_deref()) {
                (true, Some("runtime_job")) => {
                    self.resume_runtime_job_wait_for_answer_now_async(
                        session_id.as_str(),
                        resume_turn_id,
                        agent_run_identity.as_ref(),
                    )
                    .await?
                }
                (true, Some("question")) => {
                    self.resume_question_wait_for_answer_now_async(
                        session_id.as_str(),
                        resume_turn_id,
                    )
                    .await?
                }
                _ => {
                    self.resume_turn_with_agent_run_identity_and_tool_safe_point_async(
                        session_id.as_str(),
                        resume_turn_id,
                        agent_run_identity.as_ref(),
                        tool_safe_point,
                    )
                    .await?
                }
            };
            let resumed_continuation = resumed.continuation;
            let stop = continuation_run_stop(resumed_continuation);
            if !resumed.tool_results.is_empty() {
                if let Some(sink) = tool_safe_point {
                    sink.commit(ToolSafePoint::CompletedTurn(resumed.clone()))?;
                }
            }
            if let Some(sink) = stream_sink.as_deref_mut() {
                emit_runtime_events_to_stream(resumed.runtime_events.as_slice(), sink);
            }
            agent_run_resource_usage = resumed.agent_run_resource_usage.clone();
            responses.push(resumed);
            next_turn_id = new_turn_id();
            if let Some(stop) = stop {
                if should_auto_continue_after_resume_wait(
                    auto_continue_after_resume_wait,
                    resumed_continuation,
                ) {
                    // Recovery strategy is explicitly configured to continue after wait states.
                } else if resumed_continuation == QueryContinuation::CompleteTerminalTool {
                    if let Some(control) = turn_control {
                        collect_turn_control_inputs(
                            control.take_pending_or_close(next_turn_id.as_str())?,
                            &mut next_turn_supplements,
                            &mut pending_answer_now,
                        )?;
                    }
                    if next_turn_supplements.is_empty() && pending_answer_now.is_none() {
                        return Ok(AgentRunResult::new(responses, stop));
                    }
                } else {
                    if matches!(
                        resumed_continuation,
                        QueryContinuation::AwaitQuestion | QueryContinuation::AwaitRuntimeJob
                    ) {
                        turn_control_close_guard.keep_open();
                    }
                    return Ok(AgentRunResult::new(responses, stop));
                }
            }
        }

        if let Some(reason) = initial_cancellation_reason {
            return Ok(AgentRunResult::new(
                responses,
                AgentRunStop::Cancelled(reason),
            ));
        }

        let mut pending_internal_events: Vec<RuntimeEventProjection> = vec![];
        let mut streamed_session_event_ids: HashSet<String> = HashSet::new();
        let mut loop_index = responses.len();
        let mut pending_output_token_recovery = None::<PendingOutputTokenRecovery>;
        let mut output_token_recovery_attempts = 0_u8;
        loop {
            if let Some(reason) = poll_loop_cancellation(cancellation_probe)? {
                return Ok(AgentRunResult::new(
                    responses,
                    AgentRunStop::Cancelled(reason),
                ));
            }

            let turn_id = next_turn_id.clone();
            if !(responses.is_empty() && loop_index == 0) {
                if let Some(control) = turn_control {
                    collect_turn_control_inputs(
                        control.take_pending(turn_id.as_str())?,
                        &mut next_turn_supplements,
                        &mut pending_answer_now,
                    )?;
                }
            }
            let mut applied_intervention_events = None;
            let input = if responses.is_empty() && loop_index == 0 {
                TurnInput::UserMessage(user_message.clone())
            } else if let Some(pending) = pending_answer_now.take() {
                let boundary_response = responses
                    .last()
                    .ok_or_else(|| "answer_now_safe_boundary_response_missing".to_string())?;
                if !pending.applied {
                    let change = agent_run_intervention_change(
                        &pending.intervention,
                        AgentRunInterventionStatusV1::Applied,
                        answer_now_safe_boundary(responses.last().ok_or_else(|| {
                            "answer_now_safe_boundary_response_missing".to_string()
                        })?),
                    );
                    let events = self.persist_agent_run_intervention_change(
                        session_id.as_str(),
                        boundary_response.turn_id.as_str(),
                        &change,
                        turn_id.as_str(),
                    )?;
                    if let Some(sink) = stream_sink.as_deref_mut() {
                        emit_runtime_events_to_stream(events.as_slice(), sink);
                        for event in &events {
                            track_streamed_runtime_event_id(
                                &TurnUpdate::RuntimeEvent {
                                    event: event.clone(),
                                },
                                &mut streamed_session_event_ids,
                            );
                        }
                    }
                    applied_intervention_events = Some(events);
                }
                TurnInput::answer_now(
                    pending.intervention,
                    std::mem::take(&mut next_turn_supplements),
                )
            } else if let Some(recovery) = pending_output_token_recovery.take() {
                TurnInput::output_token_recovery(
                    recovery.partial_content,
                    recovery.message,
                    recovery.rejected_tool_calls,
                )
            } else if !next_turn_supplements.is_empty() {
                TurnInput::turn_supplement(std::mem::take(&mut next_turn_supplements))
            } else {
                TurnInput::ToolContinuation {
                    objective: root_user_message.clone(),
                }
            };

            let generate_req = self
                .build_generate_driver_request_with_async_driver_and_runtime_scope(
                    session_id.as_str(),
                    turn_id.as_str(),
                    &input,
                    loop_index as u32,
                    runtime_scope.clone(),
                    Some(&mut agent_run_resource_usage),
                    tool_safe_point,
                    driver,
                )
                .await?;
            if let Some(control) = turn_control {
                control.acknowledge_supplements(input.supplement_ids())?;
            }
            let context_token_estimate = generate_req.context_token_estimate;
            let recovery_content_prefix = input
                .output_token_recovery_partial()
                .unwrap_or_default()
                .to_string();
            let prepared_turn = PreparedTurnGeneration::new(
                session_id.clone(),
                turn_id.clone(),
                input,
                agent_run_identity.clone(),
                generate_req,
            );
            let generate_step = if let Some(sink) = stream_sink.as_deref_mut() {
                tokio::select! {
                    result = drive_prepared_turn_with_sink_async(driver, prepared_turn, sink) => {
                        GenerateStep::Completed(result)
                    }
                    reason = wait_for_loop_cancellation(cancellation_probe) => {
                        GenerateStep::Cancelled(reason?)
                    }
                }
            } else {
                tokio::select! {
                    result = drive_prepared_turn_async(driver, prepared_turn) => {
                        GenerateStep::Completed(result)
                    }
                    reason = wait_for_loop_cancellation(cancellation_probe) => {
                        GenerateStep::Cancelled(reason?)
                    }
                }
            };
            let turn_req_result = match generate_step {
                GenerateStep::Completed(result) => result,
                GenerateStep::Cancelled(reason) => {
                    if let Some(sink) = stream_sink.as_deref_mut() {
                        sink(TurnUpdate::ModelDone {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            finish_reason: Some("user_cancelled".to_string()),
                            process_state: RuntimeProcessState::Waiting,
                        });
                    }
                    return Ok(AgentRunResult::new(
                        responses,
                        AgentRunStop::Cancelled(reason),
                    ));
                }
            };
            let mut turn_req = match turn_req_result {
                Ok(turn_req) => turn_req,
                Err(error) if error.is_output_token_limit() => {
                    let provider_attempts = error.provider_attempts.max(1);
                    agent_run_resource_usage
                        .record_provider_attempts(context_token_estimate, provider_attempts);
                    loop_index = loop_index.saturating_add(1);
                    next_turn_id = new_turn_id();
                    let (rejected_tool_calls, has_incomplete_identity) =
                        rejected_tool_calls_for_recovery(error.truncated_tool_calls.as_slice());
                    if output_token_recovery_attempts < MAX_OUTPUT_TOKEN_RECOVERY_ATTEMPTS {
                        output_token_recovery_attempts =
                            output_token_recovery_attempts.saturating_add(1);
                        pending_output_token_recovery = Some(PendingOutputTokenRecovery {
                            partial_content: format!(
                                "{}{}",
                                recovery_content_prefix, error.partial_content
                            ),
                            message: if has_incomplete_identity {
                                INCOMPLETE_TOOL_IDENTITY_RECOVERY_MESSAGE.to_string()
                            } else {
                                OUTPUT_TOKEN_RECOVERY_MESSAGE.to_string()
                            },
                            rejected_tool_calls,
                        });
                        if let Some(sink) = stream_sink.as_deref_mut() {
                            sink(TurnUpdate::Status {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                message: None,
                                process_state: RuntimeProcessState::Recovering,
                            });
                        }
                        continue;
                    }
                    if let Some(sink) = stream_sink.as_deref_mut() {
                        let terminal_partial =
                            format!("{}{}", recovery_content_prefix, error.partial_content);
                        if !terminal_partial.is_empty() {
                            sink(TurnUpdate::ReplaceContent {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                content: terminal_partial,
                            });
                        }
                        sink(TurnUpdate::RuntimeError {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            message: error.message.clone(),
                            reason: "provider_response_interrupted".to_string(),
                            retryable: false,
                            process_state: RuntimeProcessState::ProviderInterrupted,
                        });
                    }
                    return Err(error.into());
                }
                Err(error) => return Err(error.into()),
            };
            if !recovery_content_prefix.is_empty() {
                turn_req.generate_result.content = format!(
                    "{}{}",
                    recovery_content_prefix, turn_req.generate_result.content
                );
            }
            output_token_recovery_attempts = 0;
            let convergence_intervention = turn_req.input.answer_now_intervention().cloned();
            loop_index = loop_index.saturating_add(1);
            next_turn_id = new_turn_id();
            let provider_attempts = turn_req.agent_run_resource_usage.provider_attempts;
            if provider_attempts == 0 {
                return Err("provider attempt accounting mismatch: used=0".to_string());
            }
            agent_run_resource_usage.record_completed_provider_round(
                &turn_req.generate_result,
                context_token_estimate,
                provider_attempts,
            );
            let provider_usage = provider_token_usage(&turn_req.generate_result)?;
            turn_req.agent_run_resource_usage = agent_run_resource_usage.clone();
            if let Some(sink) = tool_safe_point {
                sink.commit(ToolSafePoint::ProviderUsage {
                    turn_id: turn_id.clone(),
                    usage: provider_usage,
                    recorded_at_ms: crate::runtime::contracts::current_timestamp_ms(),
                })?;
            }
            if let Some(reason) = poll_loop_cancellation(cancellation_probe)? {
                return Ok(AgentRunResult::new(
                    responses,
                    AgentRunStop::Cancelled(reason),
                ));
            }

            let mut satisfied_by_final = None;
            if turn_req.generate_result.tool_calls.is_empty() {
                if let Some(control) = turn_control {
                    collect_turn_control_inputs(
                        control.take_pending_or_close(next_turn_id.as_str())?,
                        &mut next_turn_supplements,
                        &mut pending_answer_now,
                    )?;
                }
                if pending_answer_now.is_some() && next_turn_supplements.is_empty() {
                    satisfied_by_final = pending_answer_now
                        .take()
                        .map(|pending| pending.intervention);
                } else if !next_turn_supplements.is_empty() || pending_answer_now.is_some() {
                    if let Some(sink) = stream_sink.as_deref_mut() {
                        sink(TurnUpdate::ReplaceContent {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            content: String::new(),
                        });
                    }
                    continue;
                }
            }

            let mut response = if let Some(sink) = stream_sink.as_deref_mut() {
                self.process_turn_request_with_stream_tracking_and_tool_safe_point_async(
                    turn_req,
                    sink,
                    &mut streamed_session_event_ids,
                    tool_safe_point,
                )
                .await?
            } else {
                self.process_turn_with_stream_sink_and_tool_safe_point_async(
                    turn_req,
                    None,
                    tool_safe_point,
                )
                .await?
            };
            if let Some(events) = applied_intervention_events {
                response.runtime_events.splice(0..0, events);
            }
            if let Some(intervention) = satisfied_by_final {
                let change = agent_run_intervention_change(
                    &intervention,
                    AgentRunInterventionStatusV1::SatisfiedByFinal,
                    "provider_final",
                );
                let events = self.persist_agent_run_intervention_change(
                    session_id.as_str(),
                    response.turn_id.as_str(),
                    &change,
                    turn_id.as_str(),
                )?;
                response.runtime_events.splice(0..0, events);
            }
            if !pending_internal_events.is_empty() {
                let mut next_runtime_events = std::mem::take(&mut pending_internal_events);
                next_runtime_events.extend(response.runtime_events);
                response.runtime_events = next_runtime_events;
            }
            let continuation = response.continuation;
            let stop = continuation_run_stop(continuation);
            self.session_manager
                .save_session(&response.session_snapshot)?;
            if !response.tool_results.is_empty() {
                if let Some(sink) = tool_safe_point {
                    sink.commit(ToolSafePoint::CompletedTurn(response.clone()))?;
                }
            }
            if let Some(sink) = stream_sink.as_deref_mut() {
                emit_runtime_events_to_stream_excluding(
                    response.runtime_events.as_slice(),
                    sink,
                    Some(&streamed_session_event_ids),
                );
            }
            responses.push(response);

            if convergence_intervention.is_some() && continuation != QueryContinuation::Finalize {
                return Err("answer_now_convergence_request_must_finalize".to_string());
            }

            if let Some(stop) = stop {
                if matches!(continuation, QueryContinuation::CompleteTerminalTool) {
                    if let Some(control) = turn_control {
                        collect_turn_control_inputs(
                            control.take_pending_or_close(next_turn_id.as_str())?,
                            &mut next_turn_supplements,
                            &mut pending_answer_now,
                        )?;
                    }
                    if !next_turn_supplements.is_empty() || pending_answer_now.is_some() {
                        continue;
                    }
                } else if matches!(
                    continuation,
                    QueryContinuation::AwaitQuestion | QueryContinuation::AwaitRuntimeJob
                ) {
                    if let Some(reason) = poll_loop_cancellation(cancellation_probe)? {
                        if reason == "agent_run_cancel_requested" {
                            let wait_checkpoint = responses
                                .last()
                                .ok_or_else(|| {
                                    "agent_run_cancel_wait_response_missing".to_string()
                                })?
                                .checkpoint
                                .as_ref()
                                .ok_or_else(|| {
                                    "agent_run_cancel_wait_checkpoint_missing".to_string()
                                })?;
                            let resumed = self
                                .abandon_wait_for_agent_run_cancellation_async(
                                    continuation,
                                    session_id.as_str(),
                                    wait_checkpoint.turn_id.as_str(),
                                    agent_run_identity.as_ref(),
                                )
                                .await?;
                            if !resumed.tool_results.is_empty() {
                                if let Some(sink) = tool_safe_point {
                                    sink.commit(ToolSafePoint::CompletedTurn(resumed.clone()))?;
                                }
                            }
                            if let Some(sink) = stream_sink.as_deref_mut() {
                                emit_runtime_events_to_stream(
                                    resumed.runtime_events.as_slice(),
                                    sink,
                                );
                            }
                            responses.push(resumed);
                        }
                        return Ok(AgentRunResult::new(
                            responses,
                            AgentRunStop::Cancelled(reason),
                        ));
                    }
                    if let Some(control) = turn_control {
                        if control.is_answer_now_requested()? {
                            collect_turn_control_inputs(
                                control.take_pending(next_turn_id.as_str())?,
                                &mut next_turn_supplements,
                                &mut pending_answer_now,
                            )?;
                        }
                    }
                    if pending_answer_now.is_some() {
                        let wait_checkpoint = responses
                            .last()
                            .ok_or_else(|| "answer_now_wait_response_missing".to_string())?
                            .checkpoint
                            .as_ref()
                            .ok_or_else(|| "answer_now_wait_checkpoint_missing".to_string())?;
                        let resumed = match continuation {
                            QueryContinuation::AwaitRuntimeJob => {
                                self.resume_runtime_job_wait_for_answer_now_async(
                                    session_id.as_str(),
                                    wait_checkpoint.turn_id.as_str(),
                                    agent_run_identity.as_ref(),
                                )
                                .await?
                            }
                            QueryContinuation::AwaitQuestion => {
                                self.resume_question_wait_for_answer_now_async(
                                    session_id.as_str(),
                                    wait_checkpoint.turn_id.as_str(),
                                )
                                .await?
                            }
                            _ => unreachable!("matched wait continuation"),
                        };
                        if !resumed.tool_results.is_empty() {
                            if let Some(sink) = tool_safe_point {
                                sink.commit(ToolSafePoint::CompletedTurn(resumed.clone()))?;
                            }
                        }
                        if let Some(sink) = stream_sink.as_deref_mut() {
                            emit_runtime_events_to_stream(resumed.runtime_events.as_slice(), sink);
                        }
                        agent_run_resource_usage = resumed.agent_run_resource_usage.clone();
                        responses.push(resumed);
                        continue;
                    }
                    turn_control_close_guard.keep_open();
                }
                return Ok(AgentRunResult::new(responses, stop));
            }
        }
    }

    pub(super) async fn route_after_generate_with_retry(
        &self,
        req: RouteGenerateResultRequest,
        stage: &str,
        recovery_policy_trace_json: &mut Vec<String>,
    ) -> Result<RouteGenerateResultResponse, String> {
        recovery::route_after_generate_with_retry(
            &self.checkpoint_store,
            req,
            stage,
            self.config.max_recovery_attempts,
            recovery_policy_trace_json,
        )
        .await
    }

    pub(super) async fn persist_query_state_with_retry(
        &self,
        req: PersistQueryStateRequest,
        stage: &str,
        recovery_policy_trace_json: &mut Vec<String>,
    ) -> Result<SubmitTurnResponse, String> {
        recovery::persist_query_state_with_retry(
            &self.checkpoint_store,
            req,
            stage,
            self.config.max_recovery_attempts,
            recovery_policy_trace_json,
        )
        .await
    }
}

fn collect_turn_control_inputs(
    inputs: Vec<TurnControlInput>,
    supplements: &mut Vec<super::driver::TurnSupplementInput>,
    answer_now: &mut Option<PendingAnswerNow>,
) -> Result<(), String> {
    for input in inputs {
        match input {
            TurnControlInput::Supplement(supplement) if answer_now.is_none() => {
                supplements.push(supplement)
            }
            TurnControlInput::Supplement(_) => {
                return Err("turn_control_supplement_after_answer_now".to_string())
            }
            TurnControlInput::AnswerNow(intervention) => match answer_now.as_ref() {
                None => {
                    *answer_now = Some(PendingAnswerNow {
                        intervention,
                        applied: false,
                    })
                }
                Some(existing)
                    if existing.intervention.intervention_id == intervention.intervention_id
                        && existing.intervention.agent_run_id == intervention.agent_run_id
                        && existing.intervention.kind == intervention.kind => {}
                Some(_) => return Err("turn_control_duplicate_answer_now".to_string()),
            },
        }
    }
    Ok(())
}

fn pending_runtime_tool_batch(
    session: &SessionStateSnapshot,
) -> Result<Option<PendingRuntimeToolBatchV1>, String> {
    session
        .metadata
        .get(RUNTIME_PENDING_TOOL_BATCH_META_KEY)
        .map(|value| {
            let pending = serde_json::from_str::<PendingRuntimeToolBatchV1>(value)
                .map_err(|error| format!("decode pending runtime tool batch failed: {error}"))?;
            if pending.schema != "runtime.pending_tool_batch.v1" {
                return Err(format!(
                    "unsupported_pending_runtime_tool_batch_schema:{}",
                    pending.schema
                ));
            }
            pending.wait_checkpoint.validate()?;
            Ok(pending)
        })
        .transpose()
}

pub fn persist_answer_now_requested_fact<S: RuntimeStore>(
    store: &S,
    session_id: &str,
    display_turn_id: &str,
    intervention: &AgentRunInterventionV1,
    actor_id: &str,
) -> Result<RuntimeEventProjection, String> {
    intervention.validate()?;
    let actor_id = actor_id.trim();
    if actor_id.is_empty() {
        return Err("actorId is required".to_string());
    }
    let changes = list_agent_run_intervention_changes(store, session_id)?;
    if let Some(existing) = changes.iter().find(|change| {
        change.intervention_id == intervention.intervention_id
            && change.status == AgentRunInterventionStatusV1::Requested
    }) {
        if existing.agent_run_id != intervention.agent_run_id || existing.kind != intervention.kind
        {
            return Err("agent_run_intervention_idempotency_conflict".to_string());
        }
        return build_runtime_event_agent_run_intervention_changed(
            session_id,
            display_turn_id,
            existing,
        );
    }
    if pending_answer_now_from_changes(changes.as_slice(), intervention.agent_run_id.as_str())?
        .is_some()
    {
        return Err("alreadyConverging".to_string());
    }

    let change = AgentRunInterventionChangedV1 {
        schema: crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1.to_string(),
        intervention_id: intervention.intervention_id.clone(),
        agent_run_id: intervention.agent_run_id.clone(),
        kind: intervention.kind.clone(),
        status: AgentRunInterventionStatusV1::Requested,
        actor_id: actor_id.to_string(),
        at_ms: now_ms(),
        safe_boundary: None,
    };
    change.validate()?;
    let event = runtime_event_for_agent_run_intervention(session_id, display_turn_id, &change)?;
    if let Err(error) = store.append_event_idempotent(event) {
        let changes = list_agent_run_intervention_changes(store, session_id)?;
        let Some(existing) = changes.iter().find(|existing| {
            existing.intervention_id == change.intervention_id
                && existing.agent_run_id == change.agent_run_id
                && existing.kind == change.kind
                && existing.status == AgentRunInterventionStatusV1::Requested
        }) else {
            return Err(error.to_string());
        };
        return build_runtime_event_agent_run_intervention_changed(
            session_id,
            display_turn_id,
            existing,
        );
    }
    build_runtime_event_agent_run_intervention_changed(session_id, display_turn_id, &change)
}

fn list_agent_run_intervention_changes<S: RuntimeStore>(
    store: &S,
    session_id: &str,
) -> Result<Vec<AgentRunInterventionChangedV1>, String> {
    const PAGE_SIZE: usize = 256;
    let mut changes = Vec::new();
    let mut offset = 0;
    loop {
        let events = store
            .list_events(session_id, PAGE_SIZE, offset)
            .map_err(|error| error.to_string())?;
        let event_count = events.len();
        for event in events.into_iter().filter(|event| {
            event.event_type == crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1
        }) {
            let change =
                serde_json::from_str::<AgentRunInterventionChangedV1>(event.payload_json.as_str())
                    .map_err(|error| {
                        format!("decode AgentRun intervention event failed: {error}")
                    })?;
            change.validate()?;
            let expected_id = format!(
                "agent_run_intervention:{}:{}",
                change.intervention_id,
                agent_run_intervention_status_name(change.status)
            );
            if event.event_id != expected_id || event.session_id != session_id {
                return Err("agent_run_intervention_event_identity_mismatch".to_string());
            }
            changes.push(change);
        }
        if event_count < PAGE_SIZE {
            break;
        }
        offset = offset.saturating_add(PAGE_SIZE);
    }
    Ok(changes)
}

fn load_agent_run_intervention_projection<S: RuntimeStore>(
    store: &S,
    session_id: &str,
    intervention_id: &str,
    status: AgentRunInterventionStatusV1,
) -> Result<RuntimeEventProjection, String> {
    const PAGE_SIZE: usize = 256;
    let expected_event_id = format!(
        "agent_run_intervention:{}:{}",
        intervention_id,
        agent_run_intervention_status_name(status)
    );
    let mut offset = 0usize;
    let mut projection = None;
    loop {
        let events = store
            .list_events(session_id, PAGE_SIZE, offset)
            .map_err(|error| error.to_string())?;
        let event_count = events.len();
        for event in events
            .into_iter()
            .filter(|event| event.event_id == expected_event_id)
        {
            let change =
                serde_json::from_str::<AgentRunInterventionChangedV1>(event.payload_json.as_str())
                    .map_err(|error| {
                        format!("decode AgentRun intervention event failed: {error}")
                    })?;
            change.validate()?;
            let turn_id = event
                .task_id
                .as_deref()
                .filter(|value| !value.is_empty() && value.trim() == *value)
                .ok_or_else(|| "agent_run_intervention_event_turn_id_missing".to_string())?;
            if event.session_id != session_id
                || event.event_type
                    != crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1
                || change.intervention_id != intervention_id
                || change.status != status
                || projection
                    .replace(build_runtime_event_agent_run_intervention_changed(
                        session_id, turn_id, &change,
                    )?)
                    .is_some()
            {
                return Err("agent_run_intervention_event_identity_mismatch".to_string());
            }
        }
        if event_count < PAGE_SIZE {
            break;
        }
        offset = offset.saturating_add(PAGE_SIZE);
    }
    projection.ok_or_else(|| "agent_run_intervention_event_missing".to_string())
}

fn pending_answer_now_from_changes(
    changes: &[AgentRunInterventionChangedV1],
    agent_run_id: &str,
) -> Result<Option<PendingAnswerNow>, String> {
    let mut by_id = HashMap::<String, Vec<&AgentRunInterventionChangedV1>>::new();
    for change in changes
        .iter()
        .filter(|change| change.agent_run_id == agent_run_id)
    {
        by_id
            .entry(change.intervention_id.clone())
            .or_default()
            .push(change);
    }
    let mut active = Vec::new();
    for group in by_id.into_values() {
        let first = group[0];
        if group
            .iter()
            .any(|change| change.agent_run_id != first.agent_run_id || change.kind != first.kind)
        {
            return Err("agent_run_intervention_identity_changed".to_string());
        }
        let requested = group
            .iter()
            .filter(|change| change.status == AgentRunInterventionStatusV1::Requested)
            .count();
        let applied = group
            .iter()
            .filter(|change| change.status == AgentRunInterventionStatusV1::Applied)
            .count();
        let satisfied = group
            .iter()
            .filter(|change| change.status == AgentRunInterventionStatusV1::SatisfiedByFinal)
            .count();
        if requested != 1 || applied > 1 || satisfied > 1 || (applied == 1 && satisfied == 1) {
            return Err("agent_run_intervention_invalid_state_transition".to_string());
        }
        if satisfied == 1 {
            continue;
        }
        active.push(PendingAnswerNow {
            intervention: AgentRunInterventionV1 {
                schema: crate::runtime::contracts::AGENT_RUN_INTERVENTION_SCHEMA_V1.to_string(),
                intervention_id: first.intervention_id.clone(),
                agent_run_id: first.agent_run_id.clone(),
                kind: first.kind.clone(),
            },
            applied: applied == 1,
        });
    }
    if active.len() > 1 {
        return Err("agent_run_intervention_multiple_active_requests".to_string());
    }
    Ok(active.pop())
}

fn answer_now_safe_boundary(response: &TurnStepResult) -> &'static str {
    if response.runtime_events.iter().any(|event| {
        event.event_type == "RuntimeWaitChanged"
            && event
                .payload
                .get("transitionReason")
                .and_then(Value::as_str)
                == Some("question_cancelled_by_answer_now")
    }) {
        return "question_wait_boundary";
    }
    if response.tool_results.iter().any(|result| {
        result.details.get("schema").and_then(Value::as_str) == Some("runtime_job_tool_result.v1")
            && result.transition_reason.as_deref() == Some("cancelled_by_answer_now")
    }) {
        return "runtime_job_wait_boundary";
    }
    if response.continuation == QueryContinuation::AwaitQuestion {
        return "question_wait_boundary";
    }
    match response.continuation {
        QueryContinuation::AwaitQuestion => "question_wait_boundary",
        QueryContinuation::AwaitRuntimeJob => "runtime_job_wait_boundary",
        QueryContinuation::ExecuteTools | QueryContinuation::CompleteTerminalTool => {
            "tool_terminal_boundary"
        }
        QueryContinuation::Finalize => "provider_final_boundary",
    }
}

fn agent_run_intervention_change(
    intervention: &AgentRunInterventionV1,
    status: AgentRunInterventionStatusV1,
    safe_boundary: &str,
) -> AgentRunInterventionChangedV1 {
    AgentRunInterventionChangedV1 {
        schema: crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1.to_string(),
        intervention_id: intervention.intervention_id.clone(),
        agent_run_id: intervention.agent_run_id.clone(),
        kind: intervention.kind.clone(),
        status,
        actor_id: "core.agent_runtime".to_string(),
        at_ms: now_ms(),
        safe_boundary: Some(safe_boundary.to_string()),
    }
}

fn runtime_event_for_agent_run_intervention(
    session_id: &str,
    turn_id: &str,
    change: &AgentRunInterventionChangedV1,
) -> Result<RuntimeEvent, String> {
    change.validate()?;
    Ok(RuntimeEvent {
        event_id: format!(
            "agent_run_intervention:{}:{}",
            change.intervention_id,
            agent_run_intervention_status_name(change.status)
        ),
        session_id: session_id.to_string(),
        task_id: Some(turn_id.to_string()),
        event_type: crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1.to_string(),
        at_ms: change.at_ms,
        visibility: EventVisibility::User,
        payload_json: serde_json::to_string(change).map_err(|error| {
            format!("serialize AgentRun intervention state change failed: {error}")
        })?,
    })
}

fn agent_run_intervention_status_name(status: AgentRunInterventionStatusV1) -> &'static str {
    match status {
        AgentRunInterventionStatusV1::Requested => "requested",
        AgentRunInterventionStatusV1::Applied => "applied",
        AgentRunInterventionStatusV1::SatisfiedByFinal => "satisfied_by_final",
    }
}

fn runtime_wait_status_name(status: RuntimeWaitStatusV1) -> &'static str {
    match status {
        RuntimeWaitStatusV1::Waiting => "waiting",
        RuntimeWaitStatusV1::Resumed => "resumed",
        RuntimeWaitStatusV1::Abandoned => "abandoned",
    }
}

pub(super) fn runtime_job_session_scope_matches(
    job_session_id: Option<&str>,
    session_id: &str,
) -> bool {
    job_session_id.is_none() || job_session_id == Some(session_id)
}

fn tool_execution_facts_from_external_objects(
    objects: &[ExternalContextObject],
) -> Result<Vec<ToolExecutionFact>, String> {
    let mut facts = Vec::new();
    for object in objects {
        let Some(value) = object.metadata.get("toolExecutionFacts") else {
            continue;
        };
        let mut object_facts = serde_json::from_value::<Vec<ToolExecutionFact>>(value.clone())
            .map_err(|error| format!("decode provider poll facts failed: {error}"))?;
        if facts.len().saturating_add(object_facts.len()) > 64 {
            return Err("provider poll facts exceed 64 items".to_string());
        }
        facts.append(&mut object_facts);
    }
    Ok(facts)
}

fn runtime_job_abandoned_tool_result(
    wait: &RuntimeJobWaitV1,
    job: &RuntimeJobRecord,
    transition_reason: &str,
) -> ToolExecutionResult {
    let completed_at_ms = now_ms();
    let answer_now = transition_reason == "cancelled_by_answer_now";
    ToolExecutionResult {
        tool_call_id: wait.tool_call_id.clone(),
        tool_name: wait.source_tool_name.clone(),
        status: "cancelled".to_string(),
        content: if answer_now {
            "The pending background result was not awaited because the user requested an immediate answer."
        } else {
            "The pending background result was not awaited because the AgentRun was cancelled."
        }
        .to_string(),
        details: json!({
            "schema": "runtime_job_tool_result.v1",
            "jobId": job.job_id,
            "jobKind": job.job_kind,
            "status": transition_reason,
        }),
        facts: Vec::new(),
        error: Some(
            ToolErrorInfo::new(
                ToolFailureKind::Cancelled,
                if answer_now {
                    "runtime job waiter cancelled by answer-now"
                } else {
                    "runtime job waiter cancelled with AgentRun"
                },
                if answer_now {
                    "Pending background result was skipped to answer now"
                } else {
                    "Pending background result was skipped because the AgentRun was cancelled"
                },
            )
            .with_diagnostic(format!("runtime_job:{}", job.job_id)),
        ),
        started_at_ms: job.created_at_ms,
        completed_at_ms,
        latency_ms: completed_at_ms.saturating_sub(job.created_at_ms),
        parallel_group: None,
        transition_reason: Some(transition_reason.to_string()),
    }
}

fn provider_token_usage(generate_result: &GenerateResult) -> Result<ProviderTokenUsageV1, String> {
    fn non_negative(value: Option<i64>, field: &str) -> Result<Option<u64>, String> {
        value
            .map(|value| {
                u64::try_from(value)
                    .map_err(|_| format!("provider_usage_{field}_must_be_non_negative"))
            })
            .transpose()
    }
    let input_tokens = non_negative(generate_result.input_tokens, "input_tokens")?;
    let total_tokens = non_negative(generate_result.total_tokens, "total_tokens")?;
    let usage =
        ProviderTokenUsageV1 {
            input_tokens,
            output_tokens: match (total_tokens, input_tokens) {
                (Some(total), Some(input)) => Some(total.checked_sub(input).ok_or_else(|| {
                    "provider_usage_total_tokens_less_than_input_tokens".to_string()
                })?),
                _ => None,
            },
            total_tokens,
            prompt_cache_hit_tokens: non_negative(
                generate_result.prompt_cache_hit_tokens,
                "prompt_cache_hit_tokens",
            )?,
            prompt_cache_miss_tokens: non_negative(
                generate_result.prompt_cache_miss_tokens,
                "prompt_cache_miss_tokens",
            )?,
        };
    usage.validate()?;
    Ok(usage)
}

fn should_auto_continue_after_resume_wait(
    auto_continue_after_resume_wait: bool,
    continuation: QueryContinuation,
) -> bool {
    auto_continue_after_resume_wait && continuation == QueryContinuation::AwaitQuestion
}

#[cfg(test)]
mod tool_execution_fact_tests {
    use super::*;

    #[test]
    fn provider_poll_external_object_restores_typed_facts() {
        let expected = ToolExecutionFact::ExternalEvidenceRef(json!({
            "objectRef": "external_context:fact",
            "contentType": "text/plain",
            "sha256": format!("sha256:{}", "a".repeat(64)),
            "byteLength": 1,
            "sourceKind": "provider",
            "locator": "external_context:fact",
        }));
        let object = ExternalContextObject {
            schema_version: "external_context.v1".to_string(),
            object_id: "external_context:fact".to_string(),
            object_kind: "externalKnowledge".to_string(),
            source_provider_id: "banana.provider".to_string(),
            source_tool_name: "banana".to_string(),
            title: "fact".to_string(),
            content: "x".to_string(),
            metadata: json!({
                "toolExecutionFacts": [expected.clone()],
            }),
            updated_at_ms: 1,
        };
        assert_eq!(
            tool_execution_facts_from_external_objects(&[object]).expect("restore facts"),
            vec![expected]
        );
    }
}
