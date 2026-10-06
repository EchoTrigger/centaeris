//! Optional, host-configured completion requires a real committed tool receipt.
use super::*;

const STATE_KEY: &str = "required_completion_delivery_v1_json";
type CompletionDelivery = RequiredCompletionDeliveryV1;

fn read(session: &SessionStateSnapshot) -> Result<Option<CompletionDelivery>, String> {
    let Some(raw) = session.metadata.get(STATE_KEY) else {
        return Ok(None);
    };
    let state: CompletionDelivery = serde_json::from_str(raw)
        .map_err(|_| "completion_tool_delivery_state_invalid".to_string())?;
    state.validate()?;
    Ok(Some(state))
}
fn write(session: &mut SessionStateSnapshot, state: &CompletionDelivery) -> Result<(), String> {
    session.metadata.insert(
        STATE_KEY.into(),
        serde_json::to_string(state).map_err(|e| e.to_string())?,
    );
    Ok(())
}

pub(crate) fn restore_observation(
    session: &mut SessionStateSnapshot,
    state: &RequiredCompletionDeliveryV1,
    agent_run_id: &str,
    turn_id: &str,
) -> Result<(), String> {
    state.validate()?;
    if state.agent_run_id != agent_run_id || !state.turn_ids.iter().any(|id| id == turn_id) {
        return Err("completion_tool_delivery_observation_owner_mismatch".into());
    }
    write(session, state)
}
fn matches(state: &CompletionDelivery, identity: &RuntimeAgentRunIdentityV1, tool: &str) -> bool {
    state.agent_run_id == identity.agent_run_id
        && state.authorization_digest == identity.authorization_digest
        && state.tool_name == tool
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
    pub(super) fn completion_delivery_repair_pending(
        &self,
        session_id: &str,
        identity: Option<&RuntimeAgentRunIdentityV1>,
    ) -> Result<bool, String> {
        let (Some(tool), Some(identity)) =
            (self.config.required_completion_tool.as_deref(), identity)
        else {
            return Ok(false);
        };
        let session = self.session_manager.load_or_create_session(session_id)?;
        Ok(read(&session)?.is_some_and(|state| {
            matches(&state, identity, tool)
                && state.repair_attempted
                && state.delivered_call_id.is_none()
        }))
    }
    /// Called after input admission and before provider dispatch. Actual durable
    /// receipts are replayed through the same idempotent Session commit boundary.
    #[expect(
        clippy::too_many_arguments,
        reason = "delivery keeps input, run, scope and commit authority explicit"
    )]
    pub(super) fn prepare_completion_delivery(
        &self,
        session_id: &str,
        turn_id: &str,
        input: &TurnInput,
        identity: Option<&RuntimeAgentRunIdentityV1>,
        request: &mut GenerateDriverRequest,
        sink: Option<&ToolSafePointDispatcher<'_>>,
        scope: &PromptCompactionScopeV1,
    ) -> Result<(), String> {
        let Some(tool) = self.config.required_completion_tool.as_deref() else {
            return Ok(());
        };
        // Internal subagent workers return their own Final to their parent.
        if scope.agent_scope != "main" {
            return Ok(());
        }
        if tool.is_empty()
            || tool
                .bytes()
                .any(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
        {
            return Err("completion_tool_name_invalid".into());
        }
        let identity = identity.ok_or("completion_tool_agent_run_identity_required")?;
        let mut session = self.session_manager.load_or_create_session(session_id)?;
        let mut state = read(&session)?.filter(|state| matches(state, identity, tool));
        if matches!(
            input,
            TurnInput::UserMessage(_)
                | TurnInput::UserMessageBatch { .. }
                | TurnInput::TurnSupplement { .. }
        ) {
            // Typed input identity survives image/file prompt transformation on restore.
            // Only legacy input without an admitted ID uses its original text.
            let key = if input.supplement_ids().is_empty() {
                serde_json::to_string(&("text", input.objective()))
            } else {
                serde_json::to_string(&("inputs", input.supplement_ids()))
            }
            .map_err(|e| e.to_string())?;
            let input_key = super::tool_execution::sha256_digest(key.as_bytes());
            if state
                .as_ref()
                .is_none_or(|state| state.input_key != input_key)
            {
                state = Some(CompletionDelivery {
                    schema: "runtime.completion_delivery.v1".into(),
                    agent_run_id: identity.agent_run_id.clone(),
                    authorization_digest: identity.authorization_digest.clone(),
                    tool_name: tool.into(),
                    input_key,
                    turn_ids: vec![],
                    delivered_call_id: None,
                    repair_attempted: false,
                    draft: None,
                });
            }
        }
        let Some(mut state) = state else {
            return Ok(());
        }; // Host-only background runs may be silent.
        if state.delivered_call_id.is_none() {
            state.delivered_call_id = self.replay_completion_delivery_receipt(
                session_id,
                &state.turn_ids,
                identity,
                tool,
                sink,
            )?;
        }
        if !state.turn_ids.iter().any(|known| known == turn_id) {
            state.turn_ids.push(turn_id.into());
        }
        if state.delivered_call_id.is_none() {
            if !request
                .prepared_prompt
                .tool_definitions
                .iter()
                .any(|definition| definition.name == tool)
            {
                return Err("completion_tool_unavailable".into());
            }
            if state.repair_attempted {
                request.prepared_prompt.tool_choice =
                    ModelToolChoice::Specific { name: tool.into() };
                let instruction = format!("A user-facing delivery is still required. Call {tool} with the complete answer. A private Final cannot satisfy this input. Do not repeat any already committed delivery. Draft answer:\n{}", state.draft.as_deref().unwrap_or_default());
                let system = match request.prepared_prompt.system_prompt.take() {
                    Some(system) => format!("{system}\n\n{instruction}"),
                    None => instruction,
                };
                request
                    .observations
                    .retain(|item| !matches!(item, ModelObservationV1::SystemPrompt { .. }));
                request.observations.insert(
                    0,
                    ModelObservationV1::SystemPrompt {
                        content: system.clone(),
                    },
                );
                request.prepared_prompt.system_prompt = Some(system);
                request.context_token_estimate =
                    super::generate_request::estimate_prepared_prompt_input_tokens(
                        &request.prepared_prompt,
                    )?;
                if request
                    .context_token_estimate
                    .saturating_add(self.config.model_max_output_tokens)
                    > self.config.model_context_tokens
                {
                    return Err("completion_delivery_repair_prompt_too_large".into());
                }
            }
        }
        state.validate()?;
        let position = request
            .observations
            .iter()
            .position(|item| matches!(item, ModelObservationV1::InputUptake { .. }))
            .unwrap_or(request.observations.len());
        request.observations.insert(
            position,
            ModelObservationV1::RequiredCompletionDelivery {
                state: state.clone(),
            },
        );
        write(&mut session, &state)?;
        self.session_manager.save_session(&session)?;
        Ok(())
    }

    /// No Final is persisted or projected until the configured delivery commits.
    /// A provider gets one real repair request; ignoring it is an explicit failure.
    pub(super) fn repair_undelivered_final(
        &self,
        session_id: &str,
        identity: Option<&RuntimeAgentRunIdentityV1>,
        draft: &str,
        scope: &PromptCompactionScopeV1,
    ) -> Result<bool, String> {
        if scope.agent_scope != "main" {
            return Ok(false);
        }
        let Some(tool) = self.config.required_completion_tool.as_deref() else {
            return Ok(false);
        };
        let mut session = self.session_manager.load_or_create_session(session_id)?;
        let Some(mut state) = read(&session)? else {
            return Ok(false);
        };
        let identity = identity.ok_or("completion_tool_agent_run_identity_required")?;
        if !matches(&state, identity, tool) {
            return Ok(false);
        }
        if state.delivered_call_id.is_some() {
            return Ok(false);
        }
        if state.repair_attempted {
            return Err("completion_tool_delivery_required".into());
        }
        state.repair_attempted = true;
        state.draft = Some(draft.chars().take(self.config.max_message_chars).collect());
        write(&mut session, &state)?;
        self.session_manager.save_session(&session)?;
        Ok(true)
    }

    pub(super) fn require_completion_delivery_before_terminal(
        &self,
        session_id: &str,
        identity: Option<&RuntimeAgentRunIdentityV1>,
        sink: Option<&ToolSafePointDispatcher<'_>>,
        scope: &PromptCompactionScopeV1,
    ) -> Result<(), String> {
        if scope.agent_scope != "main" {
            return Ok(());
        }
        let (Some(tool), Some(identity)) =
            (self.config.required_completion_tool.as_deref(), identity)
        else {
            return Ok(());
        };
        let mut session = self.session_manager.load_or_create_session(session_id)?;
        let Some(mut state) = read(&session)?.filter(|state| matches(state, identity, tool)) else {
            return Ok(());
        };
        if state.delivered_call_id.is_none() {
            state.delivered_call_id = self.replay_completion_delivery_receipt(
                session_id,
                &state.turn_ids,
                identity,
                tool,
                sink,
            )?;
        }
        if state.delivered_call_id.is_none() {
            return Err("completion_tool_delivery_required".into());
        }
        write(&mut session, &state)?;
        self.session_manager.save_session(&session)
    }
}
