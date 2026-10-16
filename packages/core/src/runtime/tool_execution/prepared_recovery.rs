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
    /// Reconcile the committed prepared tool phase before accepting new inputs
    /// or issuing another provider request. No model/hook retry is authorized here.
    pub(in crate::runtime) async fn recover_prepared_tool_phase(
        &self,
        session_id: &str,
        identity: &RuntimeAgentRunIdentityV1,
        initial_input: &AgentRunInitialInput,
        sink: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<bool, String> {
        let Some(raw) = self.runtime_store.load_agent_runtime_snapshot(session_id)? else {
            return Ok(false);
        };
        let mut session: SessionStateSnapshot = serde_json::from_str(&raw)
            .map_err(|error| format!("decode prepared recovery snapshot failed: {error}"))?;
        if session.session_id != session_id {
            return Err("prepared_tool_recovery_session_mismatch".into());
        }
        let Some(turn) = crate::session::turn_input::recovery::current_prepared_turn(
            &session,
            &identity.agent_run_id,
        )?
        else {
            return Ok(false);
        };
        let ids = crate::session::turn_input::recovery::prepared_ids(
            &session,
            &identity.agent_run_id,
            &turn,
        )?
        .ok_or("prepared_tool_recovery_membership_missing")?;
        let Some(assistant)=session.context_window.iter().rev().find(|message|matches!(session.model_semantics.get(&message.message_id),Some(ModelMessageSemanticsV1::Assistant {tool_calls,..}) if !tool_calls.is_empty())) else {return Ok(false)};
        let assistant_id = assistant.message_id.clone();
        let Some(ModelMessageSemanticsV1::Assistant { tool_calls, .. }) =
            session.model_semantics.get(&assistant_id)
        else {
            unreachable!()
        };
        let calls = tool_calls.clone();
        let open = crate::runtime::context_window::trailing_unpaired_model_tool_call_ids(
            &session.context_window,
            &session.model_semantics,
        )?;
        let mut evidence_by_call = Vec::new();
        for call in &calls {
            let model_call = crate::runtime::contracts::ToolCall {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                args_json: call.args_json.clone(),
            };
            let Some(evidence) = self.read_unpaired_tool_call_evidence(session_id, &model_call)?
            else {
                if open.is_empty() {
                    return Ok(false);
                }
                return Err(format!(
                    "prepared_tool_recovery_intent_missing: callId={}",
                    call.id
                ));
            };
            if evidence.source_turn_id != turn {
                if open.is_empty() {
                    return Ok(false);
                }
                return Err("prepared_tool_recovery_source_turn_mismatch".into());
            }
            let source = evidence
                .intent
                .agent_run_identity
                .as_ref()
                .ok_or("prepared_tool_recovery_source_run_missing")?;
            let contract = self
                .tools_port
                .tool_contract(&evidence.effective_call.name)?;
            if source.agent_run_id != identity.agent_run_id
                || source.authorization_digest != identity.authorization_digest
                || evidence.intent.session_id != session_id
                || evidence.intent.session_tool_call_event_id
                    != crate::runtime::canonical_tool_call_event_id(session_id, &turn, &call.id)
                || contract.provider_id.as_deref() != Some(evidence.intent.provider_id.as_str())
                || contract.contract_digest()? != evidence.intent.tool_contract_digest
            {
                return Err("prepared_tool_recovery_source_identity_mismatch".into());
            }
            if matches!(
                evidence.effective_call.name.as_str(),
                "agent" | "task_output" | "question" | "finalize"
            ) {
                return Err(
                    "prepared_tool_recovery_requires_existing_wait_or_terminal_protocol".into(),
                );
            }
            evidence_by_call.push(evidence);
        }
        if !ids.is_empty() {
            super::super::loop_runtime::restore_prepared_input(
                &session,
                &turn,
                initial_input,
                &ids,
                &mut Vec::new(),
            )?;
        }
        let hook_facts = Self::load_tool_execution_facts(&self.runtime_store, session_id, &turn)?;
        let mut required_hook_contexts = Vec::new();
        let mut has_original_hooks = self.lifecycle_hooks.has_handlers();
        for evidence in &evidence_by_call {
            let is_open = open.contains(&evidence.effective_call.id);
            if is_open
                && (self.lifecycle_hooks.has_handlers()
                    || hook_facts
                        .post_hook_intents
                        .contains_key(&evidence.effective_call.id)
                    || evidence
                        .receipt
                        .as_ref()
                        .is_some_and(|receipt| !receipt.pre_hook_contexts.is_empty()))
            {
                return Err("prepared_tool_recovery_hooks_unsupported".into());
            }
            if !is_open {
                let facts = &hook_facts;
                if let Some(receipt) = &evidence.receipt {
                    required_hook_contexts.extend(receipt.pre_hook_contexts.iter().cloned());
                    has_original_hooks |= !receipt.pre_hook_contexts.is_empty()
                        || facts
                            .post_hook_intents
                            .contains_key(&evidence.effective_call.id);
                    if receipt.run_post_hook
                        && (self.lifecycle_hooks.has_handlers()
                            || facts
                                .post_hook_intents
                                .contains_key(&evidence.effective_call.id))
                    {
                        let completed = facts
                            .post_hook_receipts
                            .get(&evidence.effective_call.id)
                            .ok_or("prepared_tool_recovery_hooks_pending")?;
                        let result = receipt.decode_result()?;
                        let digest = sha256_digest(
                            &serde_json::to_vec(&result).map_err(|error| error.to_string())?,
                        );
                        if completed.result_digest != digest {
                            return Err("prepared_tool_recovery_hook_receipt_conflict".into());
                        }
                        required_hook_contexts.extend(completed.contexts.iter().cloned());
                    }
                } else if self.lifecycle_hooks.has_handlers() {
                    return Err("prepared_tool_recovery_hooks_pending".into());
                }
            }
        }
        if open.is_empty() && has_original_hooks {
            // A fully closed batch needs no execution recovery. Keep its valid
            // original prepared input and context byte-for-byte; never recreate
            // hook output or replay hooks as part of command reconciliation.
            let mut available = session
                .context_window
                .iter()
                .filter(|message| {
                    message.role == MessageRole::System
                        && message
                            .metadata
                            .get(LIFECYCLE_HOOK_CONTEXT_META_KEY)
                            .is_some_and(|value| value == "true")
                })
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>();
            for context in required_hook_contexts
                .iter()
                .map(|context| context.trim())
                .filter(|context| !context.is_empty())
            {
                let expected = format!("[Lifecycle hook context]\n{context}");
                let index = available
                    .iter()
                    .position(|content| *content == expected)
                    .ok_or("prepared_tool_recovery_hook_context_unsupported")?;
                available.remove(index);
            }
            super::super::loop_runtime::restore_prepared_input(
                &session,
                &turn,
                initial_input,
                &ids,
                &mut Vec::new(),
            )
            .map_err(|_| "prepared_tool_recovery_hook_context_unsupported".to_string())?;
            return Ok(false);
        }
        let sink = sink.ok_or("prepared_tool_recovery_requires_durable_safe_point")?;
        let mut reports = Vec::new();
        for evidence in evidence_by_call {
            if !open.contains(&evidence.effective_call.id) {
                continue;
            }
            let report = if let Some(receipt) = &evidence.receipt {
                let report = receipt.decode_result()?;
                notify_tool_safe_point(
                    Some(sink),
                    &evidence.intent,
                    &evidence.effective_call,
                    &report,
                )?;
                report
            } else {
                let report = self
                    .tools_port
                    .recover_execution_async(ToolInvocationRequest {
                        tool_call_id: evidence.effective_call.id.clone(),
                        tool_name: evidence.effective_call.name.clone(),
                        args_json: evidence.effective_call.args_json.clone(),
                    })
                    .await?;
                let mut facts =
                    Self::load_tool_execution_facts(&self.runtime_store, session_id, &turn)?;
                self.persist_tool_execution_receipt(
                    &mut facts,
                    session_id,
                    &turn,
                    &evidence.effective_call,
                    &evidence.effective_call,
                    &[],
                    false,
                    report,
                    Some(sink),
                )?
            };
            reports.push(report);
        }
        super::super::tool_context_writer::write_tool_results_to_context(
            &self.message_handler,
            &mut session,
            &reports,
        )?;
        // A partial parallel batch may already have a later result. Keep the
        // original provider call order after inserting only missing results.
        let mut result_messages = HashMap::new();
        session.messages.retain(|message| {
            if let Some(ModelMessageSemanticsV1::ToolResult { tool_call_id, .. }) =
                session.model_semantics.get(&message.message_id)
            {
                if calls.iter().any(|call| &call.id == tool_call_id) {
                    result_messages.insert(tool_call_id.clone(), message.clone());
                    return false;
                }
            }
            true
        });
        let insertion = session
            .messages
            .iter()
            .position(|message| message.message_id == assistant_id)
            .ok_or("prepared_tool_recovery_assistant_missing")?
            + 1;
        let ordered = calls
            .iter()
            .map(|call| {
                result_messages
                    .remove(&call.id)
                    .ok_or("prepared_tool_recovery_result_missing".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        session.messages.splice(insertion..insertion, ordered);
        crate::session::turn_input::recovery::retire_prepared(
            &mut session,
            &identity.agent_run_id,
            &turn,
            &ids,
        )?;
        // Retiring the original phase and preparing its continuation share one
        // snapshot CAS. A second crash must never re-materialize the old input.
        crate::session::turn_input::recovery::freeze_prepared(
            &mut session,
            &identity.agent_run_id,
            &new_turn_id(),
            &[],
        )?;
        refresh_session_context_window(&mut session);
        let updated = serde_json::to_string(&session).map_err(|error| error.to_string())?;
        if !self.runtime_store.compare_and_save_agent_runtime_snapshot(
            session_id,
            Some(&raw),
            &updated,
            now_ms(),
        )? {
            return Err("prepared_tool_recovery_snapshot_conflict".into());
        }
        Ok(true)
    }
}
