use super::driver::AsyncGenerateDriverPromptCompactionCandidateProducer;
use super::*;

type ResolvedModelInputImages = (
    Vec<ModelMessageV1>,
    Vec<ModelInputImageV1>,
    Vec<ModelInputImageObservationV1>,
);
use crate::model::prepared_prompt::{
    project_session_messages_to_model_messages, ModelInputImageObservationV1, ModelInputImageRefV1,
    ModelInputImageSourceRefV1, ModelInputImageV1, ModelMessageRoleV1, ModelMessageV1,
    ModelToolCallV1, PreparedPromptV1,
};
use base64::Engine as _;

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
    pub(super) async fn compact_session_with_async_driver<D: AsyncGenerateDriver>(
        &self,
        session_id: &str,
        turn_id: &str,
        driver: &D,
    ) -> Result<bool, String> {
        let mut session = self.session_manager.load_or_create_session(session_id)?;
        clear_prompt_compaction_failure_metadata(&mut session);
        refresh_session_context_window(&mut session);
        let candidate_producer = AsyncGenerateDriverPromptCompactionCandidateProducer {
            driver,
            resource_usage: Mutex::new(AgentRunResourceUsageV1::default()),
        };
        let result = self
            .apply_prompt_compaction_for_generate_request_async(
                &mut session,
                turn_id,
                PromptCompactionScopeV1::main(),
                None,
                true,
                Some(&candidate_producer),
            )
            .await?;
        self.session_manager.save_session(&session)?;
        Ok(result.stats_json.as_deref().is_some_and(|raw| {
            serde_json::from_str::<Value>(raw)
                .ok()
                .and_then(|value| {
                    value
                        .get("decision")
                        .and_then(|decision| decision.get("action"))
                        .and_then(Value::as_str)
                        .map(|action| action == "compact")
                })
                .unwrap_or(false)
        }))
    }

    pub(super) fn apply_system_prompt_template(
        &self,
        session: &mut SessionStateSnapshot,
        _req: &ProcessTurnRequest,
        _effective_user_message: &str,
        _compression_stats_json: Option<&str>,
    ) -> Result<Option<String>, String> {
        if !self.config.enable_system_prompt_template {
            session.metadata.remove(SYSTEM_PROMPT_MANIFEST_META_KEY);
            return Ok(None);
        }
        let (_, manifest_json) = prompt_projection::render_system_prompt_artifacts("generate")?;
        session.metadata.insert(
            SYSTEM_PROMPT_MANIFEST_META_KEY.to_string(),
            manifest_json.clone(),
        );
        Ok(Some(manifest_json))
    }

    pub(super) fn build_generate_tool_projection(
        &self,
        session: &SessionStateSnapshot,
    ) -> Result<(Vec<ModelToolDefinition>, String), String> {
        tool_projection::build_generate_tool_projection(
            session,
            self.tools_port.dynamic_tool_registry(),
            self.config.allowed_tools.as_deref(),
            self.tools_port.execution_host_kind(),
        )
    }

    #[cfg(test)]
    pub(super) fn build_generate_driver_request(
        &self,
        session_id: &str,
        turn_id: &str,
        user_message: &str,
        loop_index: u32,
    ) -> Result<GenerateDriverRequest, String> {
        let input = TurnInput::UserMessage(user_message.to_string());
        self.build_generate_driver_request_with_runtime_scope(
            session_id,
            turn_id,
            &input,
            loop_index,
            PromptCompactionScopeV1::main(),
        )
    }

    #[cfg(test)]
    pub(super) fn build_generate_driver_request_with_runtime_scope(
        &self,
        session_id: &str,
        turn_id: &str,
        input: &TurnInput,
        loop_index: u32,
        runtime_scope: PromptCompactionScopeV1,
    ) -> Result<GenerateDriverRequest, String> {
        let mut session = self.session_manager.load_or_create_session(session_id)?;
        if input.semantic_kind() == MESSAGE_SEMANTIC_USER_REQUEST {
            clear_prompt_compaction_failure_metadata(&mut session);
        }
        refresh_session_context_window(&mut session);
        let prompt_input_token_estimate = self.estimate_generate_prompt_input_tokens(
            &session, session_id, turn_id, input, loop_index,
        )?;

        let compression_stats_json = self
            .apply_prompt_compaction_for_generate_request(
                &mut session,
                turn_id,
                runtime_scope,
                Some(prompt_input_token_estimate),
                None,
            )?
            .stats_json;
        self.session_manager.save_session(&session)?;
        self.build_generate_driver_request_from_session(
            session,
            session_id,
            turn_id,
            input,
            loop_index,
            compression_stats_json,
        )
    }

    #[cfg(test)]
    pub(super) async fn build_generate_driver_request_with_async_driver<D: AsyncGenerateDriver>(
        &self,
        session_id: &str,
        turn_id: &str,
        user_message: &str,
        loop_index: u32,
        driver: &D,
    ) -> Result<GenerateDriverRequest, String> {
        let session = self.session_manager.load_or_create_session(session_id)?;
        self.session_manager.save_session(&session)?;
        let input = TurnInput::UserMessage(user_message.to_string());
        self.build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            turn_id,
            &input,
            loop_index,
            PromptCompactionScopeV1::main(),
            None,
            None,
            driver,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "generate orchestration keeps runtime scope and sinks explicit"
    )]
    pub(super) async fn build_generate_driver_request_with_async_driver_and_runtime_scope<
        D: AsyncGenerateDriver,
    >(
        &self,
        session_id: &str,
        turn_id: &str,
        input: &TurnInput,
        loop_index: u32,
        runtime_scope: PromptCompactionScopeV1,
        agent_run_resource_usage: Option<&mut AgentRunResourceUsageV1>,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
        driver: &D,
    ) -> Result<GenerateDriverRequest, String> {
        let mut session = self.session_manager.load_or_create_session(session_id)?;
        if input.semantic_kind() == MESSAGE_SEMANTIC_USER_REQUEST {
            clear_prompt_compaction_failure_metadata(&mut session);
            self.close_unpaired_tool_calls_for_new_turn(
                &mut session,
                session_id,
                turn_id,
                tool_safe_point,
            )
            .await?;
        }
        refresh_session_context_window(&mut session);
        let prompt_input_token_estimate = self.estimate_generate_prompt_input_tokens(
            &session, session_id, turn_id, input, loop_index,
        )?;

        let candidate_producer = AsyncGenerateDriverPromptCompactionCandidateProducer {
            driver,
            resource_usage: Mutex::new(
                agent_run_resource_usage
                    .as_deref()
                    .cloned()
                    .unwrap_or_default(),
            ),
        };
        let compaction_result = self
            .apply_prompt_compaction_for_generate_request_async(
                &mut session,
                turn_id,
                runtime_scope,
                Some(prompt_input_token_estimate),
                false,
                Some(&candidate_producer),
            )
            .await?;
        let tracked_resource_usage = candidate_producer
            .resource_usage
            .into_inner()
            .map_err(|_| "root AgentRun resource usage lock poisoned after prompt compaction")?;
        if let Some(usage) = agent_run_resource_usage {
            *usage = tracked_resource_usage;
        }
        self.session_manager.save_session(&session)?;
        self.build_generate_driver_request_from_session(
            session,
            session_id,
            turn_id,
            input,
            loop_index,
            compaction_result.stats_json,
        )
    }

    pub(super) fn build_generate_driver_request_from_session(
        &self,
        session: SessionStateSnapshot,
        session_id: &str,
        turn_id: &str,
        input: &TurnInput,
        loop_index: u32,
        compression_stats_json: Option<String>,
    ) -> Result<GenerateDriverRequest, String> {
        let (request, mut session) = self.project_generate_driver_request_from_session(
            session,
            session_id,
            turn_id,
            input,
            loop_index,
            compression_stats_json,
        )?;
        validate_model_input_budget(
            request.context_token_estimate,
            self.config.model_context_tokens,
            self.config.model_max_output_tokens,
        )?;
        self.persist_generate_driver_input_messages(&mut session, session_id, turn_id, input)?;
        Ok(request)
    }

    fn estimate_generate_prompt_input_tokens(
        &self,
        session: &SessionStateSnapshot,
        session_id: &str,
        turn_id: &str,
        input: &TurnInput,
        loop_index: u32,
    ) -> Result<u32, String> {
        validate_model_input_budget_config(
            self.config.model_context_tokens,
            self.config.model_max_output_tokens,
        )?;
        self.project_generate_driver_request_from_session(
            session.clone(),
            session_id,
            turn_id,
            input,
            loop_index,
            None,
        )
        .map(|(request, _)| {
            let estimate = u64::from(request.context_token_estimate);
            let scale = u64::from(self.config.prompt_token_estimate_scale_basis_points);
            estimate
                .saturating_mul(scale)
                .saturating_add(9_999)
                .checked_div(10_000)
                .unwrap_or(estimate)
                .min(u64::from(u32::MAX)) as u32
        })
    }

    fn project_generate_driver_request_from_session(
        &self,
        mut session: SessionStateSnapshot,
        session_id: &str,
        turn_id: &str,
        input: &TurnInput,
        loop_index: u32,
        compression_stats_json: Option<String>,
    ) -> Result<(GenerateDriverRequest, SessionStateSnapshot), String> {
        let answer_now = input.answer_now_intervention().is_some();
        let (tool_definitions, _) = if answer_now {
            (Vec::new(), String::new())
        } else {
            self.build_generate_tool_projection(&session)?
        };
        let (system_prompt, system_prompt_manifest_json) =
            if self.config.enable_system_prompt_template {
                let (content, manifest_json) =
                    prompt_projection::render_system_prompt_artifacts("generate")?;
                (Some(content), Some(manifest_json))
            } else {
                (None, None)
            };

        crate::runtime::context_window::validate_model_context_window(
            session.context_window.as_slice(),
            &session.model_semantics,
        )?;
        crate::runtime::context_window::validate_context_window_materialization(&session)?;
        let (context_messages, runtime_context_anchor_message_id) = build_model_context_messages(
            session.context_window.as_slice(),
            session_id,
            turn_id,
            input.user_message(),
        )?;
        for message in &context_messages {
            if session
                .model_semantics
                .contains_key(message.message_id.as_str())
            {
                continue;
            }
            if !matches!(message.role, MessageRole::System | MessageRole::User) {
                return Err(format!(
                    "model_message_semantics_missing: messageId={}",
                    message.message_id
                ));
            }
            session.model_semantics.insert(
                message.message_id.clone(),
                crate::session::state::ModelMessageSemanticsV1::Plain,
            );
        }
        let model_messages =
            project_session_messages_to_model_messages(&session, context_messages.as_slice())?;
        let (mut model_messages, input_images, mut input_image_observations) =
            self.resolve_model_input_images(context_messages.as_slice(), model_messages)?;
        let insertion_index = model_messages
            .iter()
            .position(|message| message.message_id == runtime_context_anchor_message_id)
            .ok_or_else(|| "runtime_context_user_anchor_projection_missing".to_string())?;
        let mut runtime_context_messages = Vec::new();
        if !answer_now {
            if let Some(cwd) = self.tools_port.cwd() {
                runtime_context_messages.push(prompt_projection::build_execution_context_message(
                    session_id,
                    turn_id,
                    cwd,
                    self.tools_port.bash_description()?,
                ));
            }
        }
        let agent_instructions_message = prompt_projection::build_agent_instructions_message(
            session_id,
            turn_id,
            self.config.agent_instructions.as_str(),
        )?;
        let agent_instructions_hash = agent_instructions_message
            .as_ref()
            .map(|message| stable_text_hash(message.content.as_str()));
        if let Some(message) = agent_instructions_message {
            runtime_context_messages.push(message);
        }
        let agents_context_message = match (
            self.tools_port.cwd(),
            self.tools_port.read_agents_instructions()?,
        ) {
            (Some(cwd), Some((instructions, _file_hash))) => {
                Some(prompt_projection::build_agents_context_message(
                    session_id,
                    turn_id,
                    cwd,
                    instructions.as_str(),
                ))
            }
            _ => None,
        };
        let agents_context_hash = agents_context_message
            .as_ref()
            .map(|message| stable_text_hash(message.content.as_str()));
        if let Some(message) = agents_context_message {
            runtime_context_messages.push(message);
        }
        let skill_catalog_message = if answer_now {
            None
        } else {
            prompt_projection::build_skill_catalog_message(
                session_id,
                turn_id,
                self.tools_port.skill_index(),
                skill_catalog_prompt_budget_chars(self.config.model_context_tokens),
            )?
        };
        let skill_catalog_hash = skill_catalog_message
            .as_ref()
            .map(|_| self.tools_port.skill_index().snapshot().catalog_hash);
        if let Some(message) = skill_catalog_message {
            runtime_context_messages.push(message);
        }
        if let Some(partial_content) = input.output_token_recovery_partial() {
            let rejected_tool_calls = input.output_token_recovery_tool_calls();
            if !partial_content.is_empty() || !rejected_tool_calls.is_empty() {
                runtime_context_messages.push(ModelMessageV1 {
                    message_id: format!("message:{turn_id}:output-token-recovery-assistant"),
                    role: ModelMessageRoleV1::Assistant,
                    content: partial_content.to_string(),
                    tool_calls: rejected_tool_calls
                        .iter()
                        .map(|call| ModelToolCallV1 {
                            id: call.call_id.clone(),
                            name: call.tool_name.clone(),
                            args_json: "{}".to_string(),
                        })
                        .collect(),
                    tool_call_id: None,
                    reasoning_content: None,
                });
            }
            runtime_context_messages.extend(rejected_tool_calls.iter().enumerate().map(
                |(index, call)| ModelMessageV1 {
                    message_id: format!(
                        "message:{turn_id}:output-token-recovery-tool-result:{}",
                        index + 1
                    ),
                    role: ModelMessageRoleV1::Tool,
                    content: format!(
                        "Tool call \"{}\" was not executed because the provider hit the output token limit and its arguments may be truncated. Re-issue the tool call with complete arguments.",
                        call.tool_name
                    ),
                    tool_calls: vec![],
                    tool_call_id: Some(call.call_id.clone()),
                    reasoning_content: None,
                },
            ));
        }
        model_messages.splice(insertion_index..insertion_index, runtime_context_messages);
        let tool_choice = if tool_definitions.is_empty() {
            ModelToolChoice::None
        } else {
            ModelToolChoice::Auto
        };
        let mut prepared_prompt = PreparedPromptV1::new(
            system_prompt.clone(),
            model_messages,
            tool_definitions.clone(),
            tool_choice,
            self.config.model_max_output_tokens,
        )?;
        prepared_prompt.set_input_images(input_images)?;
        let context_token_estimate = estimate_prepared_prompt_input_tokens(&prepared_prompt)?;
        let provider_prompt_cache_key = build_provider_prompt_cache_key(
            system_prompt.as_deref(),
            tool_definitions.as_slice(),
            skill_catalog_hash.as_deref(),
            agents_context_hash.as_deref(),
            agent_instructions_hash.as_deref(),
            &self.config,
        )?;
        let mut observations = Vec::new();
        if let Some(content) = system_prompt {
            observations.push(ModelObservationV1::SystemPrompt { content });
        }
        observations.extend(
            prepared_prompt
                .messages
                .iter()
                .cloned()
                .map(|message| ModelObservationV1::ContextMessage { message }),
        );
        observations.extend(
            input_image_observations
                .drain(..)
                .map(|image| ModelObservationV1::InputImage { image }),
        );
        if !tool_definitions.is_empty() {
            observations.push(ModelObservationV1::ToolCatalog { tool_definitions });
        }
        Ok((
            GenerateDriverRequest {
                session_id: session_id.to_string(),
                turn_id: turn_id.to_string(),
                loop_index,
                provider_prompt_cache_key,
                provider_prompt_cache_retention: self
                    .config
                    .provider_prompt_cache_retention
                    .clone(),
                system_prompt_manifest_json,
                compression_stats_json,
                context_token_estimate,
                prepared_prompt,
                observations,
                live_content_prefix: input
                    .output_token_recovery_partial()
                    .unwrap_or_default()
                    .to_string(),
            },
            session,
        ))
    }

    fn resolve_model_input_images(
        &self,
        messages: &[ChatMessage],
        model_messages: Vec<ModelMessageV1>,
    ) -> Result<ResolvedModelInputImages, String> {
        let mut images = Vec::new();
        let mut observations = Vec::new();
        for message in messages {
            if let Some(raw) = message
                .metadata
                .get(crate::runtime::keys::metadata::MODEL_INPUT_IMAGES)
            {
                let references = serde_json::from_str::<Vec<ModelInputImageRefV1>>(raw)
                    .map_err(|error| format!("decode model input image refs failed: {error}"))?;
                for reference in references {
                    let source = ModelInputImageSourceRefV1::InputRef {
                        input_ref: reference.input_ref,
                        content_type: reference.content_type,
                        placeholder: reference.placeholder,
                    };
                    let (content_type, placeholder, bytes) =
                        self.resolve_model_input_image_source(&source)?;
                    images.push(ModelInputImageV1 {
                        message_id: message.message_id.clone(),
                        content_type,
                        placeholder,
                        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    });
                    observations.push(ModelInputImageObservationV1 {
                        message_id: message.message_id.clone(),
                        source,
                    });
                }
            }
            if message.role == MessageRole::Tool {
                continue;
            }
            let Some(raw) = message
                .metadata
                .get(crate::runtime::keys::metadata::MODEL_INPUT_IMAGE_SOURCES)
            else {
                continue;
            };
            let sources = serde_json::from_str::<Vec<ModelInputImageSourceRefV1>>(raw)
                .map_err(|error| format!("decode model input image sources failed: {error}"))?;
            for source in sources {
                let (content_type, placeholder, bytes) =
                    self.resolve_model_input_image_source(&source)?;
                images.push(ModelInputImageV1 {
                    message_id: message.message_id.clone(),
                    content_type,
                    placeholder,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                });
                observations.push(ModelInputImageObservationV1 {
                    message_id: message.message_id.clone(),
                    source,
                });
            }
        }
        let mut projected = Vec::with_capacity(model_messages.len());
        let mut pending_tool_sources = Vec::<ModelInputImageSourceRefV1>::new();
        for (index, (message, model_message)) in
            messages.iter().zip(model_messages.into_iter()).enumerate()
        {
            projected.push(model_message);
            if message.role == MessageRole::Tool {
                if let Some(raw) = message
                    .metadata
                    .get(crate::runtime::keys::metadata::MODEL_INPUT_IMAGE_SOURCES)
                {
                    pending_tool_sources.extend(
                        serde_json::from_str::<Vec<ModelInputImageSourceRefV1>>(raw).map_err(
                            |error| {
                                format!("decode tool model input image sources failed: {error}")
                            },
                        )?,
                    );
                }
            }
            let tool_batch_ends = message.role == MessageRole::Tool
                && messages
                    .get(index + 1)
                    .is_none_or(|next| next.role != MessageRole::Tool);
            if !tool_batch_ends || pending_tool_sources.is_empty() {
                continue;
            }
            let observation_message_id = format!("{}:image-observation", message.message_id);
            let mut content = String::from(
                "Model-visible image observations from the preceding tool result batch:\n",
            );
            for source in pending_tool_sources.drain(..) {
                let (content_type, placeholder, bytes) =
                    self.resolve_model_input_image_source(&source)?;
                let label = match &source {
                    ModelInputImageSourceRefV1::InputRef { .. } => "attached input",
                    ModelInputImageSourceRefV1::ExecutionFile { image } => image.path.as_str(),
                };
                content.push_str(format!("{placeholder} {label}\n").as_str());
                images.push(ModelInputImageV1 {
                    message_id: observation_message_id.clone(),
                    content_type,
                    placeholder,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                });
                observations.push(ModelInputImageObservationV1 {
                    message_id: observation_message_id.clone(),
                    source,
                });
            }
            projected.push(ModelMessageV1 {
                message_id: observation_message_id,
                role: ModelMessageRoleV1::User,
                content: content.trim_end().to_string(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        Ok((projected, images, observations))
    }

    fn resolve_model_input_image_source(
        &self,
        source: &ModelInputImageSourceRefV1,
    ) -> Result<(String, String, Vec<u8>), String> {
        match source {
            ModelInputImageSourceRefV1::InputRef {
                input_ref,
                content_type,
                placeholder,
            } => {
                let resolver = self
                    .model_input_image_resolver
                    .as_ref()
                    .ok_or_else(|| "model_input_image_resolver_missing".to_string())?;
                let bytes = resolver.resolve(input_ref.as_str(), content_type.as_str())?;
                let (actual_content_type, _, _) =
                    crate::model::prepared_prompt::inspect_model_input_image(bytes.as_slice())?;
                if actual_content_type != content_type {
                    return Err("model_input_image_content_type_mismatch".to_string());
                }
                Ok((content_type.clone(), placeholder.clone(), bytes))
            }
            ModelInputImageSourceRefV1::ExecutionFile { image } => Ok((
                image.content_type.clone(),
                image.placeholder.clone(),
                self.tools_port.resolve_execution_model_input_image(image)?,
            )),
        }
    }

    pub(super) fn persist_generate_driver_input_messages(
        &self,
        session: &mut SessionStateSnapshot,
        session_id: &str,
        turn_id: &str,
        input: &TurnInput,
    ) -> Result<(), String> {
        if input.output_token_recovery_partial().is_some() {
            return Ok(());
        }
        let user_message = input.user_message();
        if user_message.is_none() {
            return Ok(());
        }
        if model_input_already_persisted(session, session_id, turn_id) {
            return Ok(());
        }
        let mut input_messages = vec![prompt_projection::build_current_user_message(
            session_id,
            turn_id,
            user_message.expect("user message is checked above"),
        )];
        if let Some(user_message) = input_messages
            .iter_mut()
            .rev()
            .find(|message| message.role == MessageRole::User)
        {
            user_message.metadata.insert(
                MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
                input.semantic_kind().to_string(),
            );
        }
        for message in &input_messages {
            session.model_semantics.insert(
                message.message_id.clone(),
                crate::session::state::ModelMessageSemanticsV1::Plain,
            );
        }
        session.messages.extend(input_messages);
        refresh_session_context_window(session);
        self.session_manager.save_session(session)
    }

    pub(super) async fn close_unpaired_tool_calls_for_new_turn(
        &self,
        session: &mut SessionStateSnapshot,
        session_id: &str,
        turn_id: &str,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<usize, String> {
        let open_tool_call_ids =
            crate::runtime::context_window::trailing_unpaired_model_tool_call_ids(
                session.messages.as_slice(),
                &session.model_semantics,
            )?;
        if open_tool_call_ids.is_empty() {
            return Ok(0);
        }
        let open_call_by_id = open_tool_call_ids
            .iter()
            .map(|tool_call_id| {
                (
                    tool_call_id.clone(),
                    find_model_assistant_semantics_tool_call(session, tool_call_id.as_str()),
                )
            })
            .collect::<HashMap<_, _>>();
        let now = now_ms();
        let mut result_by_call_id = HashMap::<String, ToolExecutionResult>::new();
        for tool_call_id in &open_tool_call_ids {
            let call = open_call_by_id.get(tool_call_id).cloned().unwrap_or(None);
            if let Some((intent, effective_call, result)) = call
                .as_ref()
                .map(|call| self.recover_interrupted_tool_execution_result(session_id, call))
                .transpose()?
                .flatten()
            {
                if let Some(sink) = tool_safe_point {
                    let agent_run_id = intent
                        .agent_run_identity
                        .as_ref()
                        .ok_or_else(|| {
                            "interrupted tool recovery requires AgentRun identity".to_string()
                        })?
                        .agent_run_id
                        .clone();
                    sink.commit(ToolSafePoint::DurableToolCall {
                        session_id: intent.session_id.clone(),
                        turn_id: intent.turn_id.clone(),
                        agent_run_id: agent_run_id.clone(),
                        call: effective_call.clone(),
                        provider_id: intent.provider_id.clone(),
                        tool_contract_digest: intent.tool_contract_digest.clone(),
                        recorded_at_ms: intent.recorded_at_ms,
                    })?;
                    sink.commit(ToolSafePoint::DurableReceipt {
                        session_id: intent.session_id,
                        turn_id: intent.turn_id,
                        agent_run_id,
                        call: effective_call,
                        result: result.clone(),
                    })?;
                }
                result_by_call_id.insert(tool_call_id.clone(), result);
            }
        }
        for tool_call_id in open_tool_call_ids.iter() {
            if result_by_call_id.contains_key(tool_call_id) {
                continue;
            }
            let call = open_call_by_id.get(tool_call_id).cloned().unwrap_or(None);
            let tool_name = call
                .as_ref()
                .map(|call| call.tool_name.clone())
                .unwrap_or_else(|| "unknown".to_string());
            let permission = call.as_ref().map(|call| {
                self.evaluate_tool_permission_decision(
                    call.tool_name.as_str(),
                    Some(call.args_json.as_str()),
                )
            });
            let normalized_input = permission
                .as_ref()
                .map(|decision| json!(decision.normalized_input))
                .unwrap_or(Value::Null);
            let permission_decision = permission
                .as_ref()
                .map(PermissionDecision::audit_json)
                .unwrap_or(Value::Null);
            result_by_call_id.insert(tool_call_id.clone(), ToolExecutionResult {
                tool_call_id: tool_call_id.clone(),
                tool_name: tool_name.clone(),
                status: "blocked".to_string(),
                content: "The previous unpaired tool call was closed before execution; it was not replayed.".to_string(),
                details: json!({
                    "schema": "tool_result_tombstone_v1",
                    "status": "blocked",
                    "reason": "unpaired_tool_call_closed_by_new_user_turn",
                    "message": "The previous tool call had no recorded tool result before a new user turn started; the tool call was not executed by this recovery step.",
                    "sessionId": session_id,
                    "turnId": turn_id,
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "normalizedInput": normalized_input,
                    "permissionDecision": permission_decision,
                }),
                facts: Vec::new(),
                error: Some(ToolErrorInfo::new(
                    ToolFailureKind::Cancelled,
                    "unpaired tool call closed before execution",
                    "Unpaired tool call closed before execution",
                )),
                started_at_ms: now,
                completed_at_ms: now,
                latency_ms: 0,
                parallel_group: None,
                transition_reason: Some("unpaired_tool_call_closed_by_new_user_turn".to_string()),
            });
        }
        let results = open_tool_call_ids
            .iter()
            .filter_map(|tool_call_id| result_by_call_id.remove(tool_call_id))
            .collect::<Vec<_>>();
        if results.is_empty() {
            return Ok(0);
        }
        let summary = tool_context_writer::write_tool_results_to_context(
            &self.message_handler,
            session,
            results.as_slice(),
        )?;
        Ok(summary.tool_messages_written)
    }
}

fn skill_catalog_prompt_budget_chars(context_limit_tokens: u32) -> usize {
    let proportional = (context_limit_tokens as usize)
        .saturating_mul(2)
        .saturating_div(100)
        .saturating_mul(4);
    proportional.clamp(8_000, 65_536)
}

fn build_model_context_messages(
    base_context_window: &[ChatMessage],
    session_id: &str,
    turn_id: &str,
    user_message: Option<&str>,
) -> Result<(Vec<ChatMessage>, String), String> {
    let continuation_anchor = if user_message.is_none() {
        let (last_model_message_index, last_model_message) = base_context_window
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| message.role != MessageRole::System)
            .ok_or_else(|| "tool_continuation_requires_transcript".to_string())?;
        if last_model_message.role != MessageRole::Tool {
            return Err(format!(
                "tool_continuation_requires_tool_result_tail: messageId={} role={:?}",
                last_model_message.message_id, last_model_message.role
            ));
        }
        if let Some(message) = base_context_window[last_model_message_index.saturating_add(1)..]
            .iter()
            .find(|message| !crate::runtime::context_window::is_lifecycle_hook_context(message))
        {
            return Err(format!(
                "tool_continuation_trailing_context_invalid: messageId={}",
                message.message_id
            ));
        }
        Some(
            base_context_window[..last_model_message_index]
                .iter()
                .rfind(|message| {
                    crate::runtime::context_window::is_reliable_tool_chain_user_anchor(message)
                })
                .map(|message| message.message_id.clone())
                .ok_or_else(|| "tool_continuation_reliable_user_anchor_missing".to_string())?,
        )
    } else {
        None
    };

    let mut context_messages = base_context_window.to_vec();
    if let Some(anchor_message_id) = continuation_anchor.as_deref() {
        let mut lifecycle_contexts = context_messages
            .iter()
            .rposition(|message| message.role == MessageRole::Tool)
            .map(|last_tool_index| context_messages.split_off(last_tool_index + 1))
            .unwrap_or_default();
        if !lifecycle_contexts.is_empty() {
            for message in &mut lifecycle_contexts {
                message.role = MessageRole::User;
            }
            let anchor_index = context_messages
                .iter()
                .position(|message| message.message_id == anchor_message_id)
                .expect("resolved continuation anchor remains in context");
            context_messages.splice(anchor_index..anchor_index, lifecycle_contexts);
        }
    }
    let anchor_message_id = if let Some(user_message) = user_message {
        let current =
            prompt_projection::build_current_user_message(session_id, turn_id, user_message);
        let message_id = current.message_id.clone();
        match context_messages
            .iter()
            .find(|message| message.message_id == current.message_id)
        {
            Some(existing)
                if existing.role == current.role && existing.content == current.content => {}
            Some(_) => return Err("current_user_message_identity_conflict".to_string()),
            None => context_messages.push(current),
        }
        message_id
    } else {
        continuation_anchor.expect("tool continuation anchor is resolved above")
    };
    Ok((context_messages, anchor_message_id))
}

fn estimate_prepared_prompt_input_tokens(
    prepared_prompt: &PreparedPromptV1,
) -> Result<u32, String> {
    let system_tokens = prepared_prompt
        .system_prompt
        .as_deref()
        .map(crate::model::prepared_prompt::estimate_text_tokens)
        .unwrap_or_default();
    let context_tokens = prepared_prompt
        .messages
        .iter()
        .try_fold(0u32, |total, message| {
            let serialized = serde_json::to_string(message).map_err(|error| {
                format!("serialize model message for context budget failed: {error}")
            })?;
            Ok::<u32, String>(total.saturating_add(
                crate::model::prepared_prompt::estimate_text_tokens(serialized.as_str()),
            ))
        })?;
    let tool_schema_tokens = if prepared_prompt.tool_definitions.is_empty() {
        0
    } else {
        let serialized =
            serde_json::to_string(&prepared_prompt.tool_definitions).map_err(|error| {
                format!("serialize model tool definitions for context budget failed: {error}")
            })?;
        crate::model::prepared_prompt::estimate_text_tokens(serialized.as_str())
    };
    Ok(system_tokens
        .saturating_add(context_tokens)
        .saturating_add(tool_schema_tokens)
        .saturating_add((prepared_prompt.input_images.len() as u32).saturating_mul(1_024)))
}

fn validate_model_input_budget_config(
    model_context_tokens: u32,
    main_max_output_tokens: u32,
) -> Result<u32, String> {
    model_context_tokens
        .checked_sub(main_max_output_tokens)
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| {
            format!(
                "model_context_budget_invalid: modelContextTokens={model_context_tokens} mainMaxOutputTokens={main_max_output_tokens}"
            )
        })
}

fn validate_model_input_budget(
    estimated_tokens: u32,
    model_context_tokens: u32,
    main_max_output_tokens: u32,
) -> Result<u32, String> {
    let main_input_limit_tokens =
        validate_model_input_budget_config(model_context_tokens, main_max_output_tokens)?;
    if estimated_tokens > main_input_limit_tokens {
        return Err(format!(
            "model_context_budget_exceeded: estimatedTokens={estimated_tokens} mainInputLimitTokens={main_input_limit_tokens}"
        ));
    }
    Ok(estimated_tokens)
}
