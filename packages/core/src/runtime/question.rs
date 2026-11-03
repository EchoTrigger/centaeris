//! Durable single-question input and response contracts owned by Core.

use serde::{Deserialize, Serialize};

use super::contracts::{RuntimeAgentRunIdentityV1, RuntimeAwaitQuestionCheckpointV1};
use crate::model::{GenerateResult, ToolCallEnvelope};
use crate::tool::ModelToolDefinition;

pub const TOOL_NAME: &str = "request_user_input";
pub const QUESTION_SCHEMA: &str = "runtime.await_question.v2";
pub const RESPONSE_SCHEMA: &str = "runtime.question.response.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuestionInputCapability {
    #[default]
    Disabled,
    PersistentResponses,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub question: String,
    pub options: Vec<String>,
}

impl Question {
    pub fn validate(&self) -> Result<(), String> {
        if self.question.trim().is_empty()
            || self.options.len() < 2
            || self.options.len() > 6
            || self.options.iter().any(|option| option.trim().is_empty())
        {
            return Err("question_requires_text_and_two_to_six_options".into());
        }
        let unique = self
            .options
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if unique.len() != self.options.len() {
            return Err("question_options_must_be_unique".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionCheckpoint {
    pub schema: String,
    pub continuation_id: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub turn_id: String,
    pub question_id: String,
    pub question: Question,
    pub tool_call: QuestionToolCall,
}

impl QuestionCheckpoint {
    pub fn new(
        identity: &RuntimeAgentRunIdentityV1,
        turn_id: &str,
        result: &GenerateResult,
    ) -> Result<Self, String> {
        let question = question_from_result(result)?
            .ok_or_else(|| "question_tool_call_required".to_string())?;
        let call = &result.tool_calls[0];
        let identity = RuntimeAwaitQuestionCheckpointV1::new(identity, turn_id, &call.id)?;
        Ok(Self {
            schema: QUESTION_SCHEMA.into(),
            continuation_id: identity.continuation_id,
            agent_run_id: identity.agent_run_id,
            authorization_digest: identity.authorization_digest,
            turn_id: identity.turn_id,
            question_id: identity.question_id,
            question,
            tool_call: QuestionToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                args_json: call.args_json.clone(),
            },
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != QUESTION_SCHEMA {
            return Err("question_checkpoint_schema_mismatch".into());
        }
        RuntimeAwaitQuestionCheckpointV1 {
            schema: super::contracts::RUNTIME_AWAIT_QUESTION_SCHEMA_V1.into(),
            continuation_id: self.continuation_id.clone(),
            agent_run_id: self.agent_run_id.clone(),
            authorization_digest: self.authorization_digest.clone(),
            turn_id: self.turn_id.clone(),
            question_id: self.question_id.clone(),
        }
        .validate()?;
        self.question.validate()?;
        let parsed: Question = serde_json::from_str(&self.tool_call.args_json)
            .map_err(|error| format!("invalid question tool arguments: {error}"))?;
        if self.tool_call.name != TOOL_NAME
            || self.tool_call.id != self.question_id
            || parsed != self.question
        {
            return Err("question_tool_call_binding_mismatch".into());
        }
        Ok(())
    }

    pub fn tool_call(&self) -> ToolCallEnvelope {
        ToolCallEnvelope {
            id: self.tool_call.id.clone(),
            name: self.tool_call.name.clone(),
            args_json: self.tool_call.args_json.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionResponse {
    pub schema: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub continuation_id: String,
    pub question_id: String,
    pub operation_id: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

impl QuestionResponse {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != RESPONSE_SCHEMA {
            return Err("question_response_schema_mismatch".into());
        }
        for value in [
            &self.agent_run_id,
            &self.authorization_digest,
            &self.continuation_id,
            &self.question_id,
            &self.operation_id,
        ] {
            if value.trim().is_empty() {
                return Err("question_response_identity_required".into());
            }
        }
        if self.operation_id.len() > 128
            || !self
                .operation_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
        {
            return Err("question_response_operation_id_invalid".into());
        }
        match (self.action.as_str(), self.answer.as_deref()) {
            ("answer", Some(answer)) if !answer.trim().is_empty() => {
                if answer.len() > 65_536 {
                    return Err("question_response_answer_too_large".into());
                }
            }
            ("skip", None) => {}
            _ => return Err("question_response_action_invalid".into()),
        }
        Ok(())
    }

    pub fn event_id(&self) -> String {
        response_event_id(&self.continuation_id)
    }

    pub fn validate_binding(&self, checkpoint: &QuestionCheckpoint) -> Result<(), String> {
        self.validate()?;
        if self.agent_run_id != checkpoint.agent_run_id
            || self.authorization_digest != checkpoint.authorization_digest
            || self.continuation_id != checkpoint.continuation_id
            || self.question_id != checkpoint.question_id
        {
            return Err("question_response_binding_mismatch".into());
        }
        Ok(())
    }
}

pub fn response_event_id(continuation_id: &str) -> String {
    format!("runtime_question_response:{continuation_id}")
}

pub fn question_from_result(result: &GenerateResult) -> Result<Option<Question>, String> {
    let Some(call) = result.tool_calls.iter().find(|call| call.name == TOOL_NAME) else {
        return Ok(None);
    };
    if result.tool_calls.len() != 1 {
        return Err("request_user_input_must_be_the_only_tool_call".into());
    }
    let question: Question = serde_json::from_str(&call.args_json)
        .map_err(|error| format!("invalid question tool arguments: {error}"))?;
    question.validate()?;
    Ok(Some(question))
}

pub fn tool_definition() -> ModelToolDefinition {
    ModelToolDefinition {
        name: TOOL_NAME.into(),
        description: "Ask the user one question and wait for their response. Supply 2 to 6 choices, displayed with numbers starting at 1. The user can select a number, provide free text, or skip. Call this tool alone.".into(),
        input_schema: serde_json::json!({
            "type":"object", "additionalProperties":false,
            "properties":{
                "question":{"type":"string","minLength":1},
                "options":{"type":"array","minItems":2,"maxItems":6,"uniqueItems":true,"items":{"type":"string","minLength":1}}
            },"required":["question","options"]
        }),
    }
}

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
    pub(super) fn repair_closed_question_wait(
        &self,
        identity: &RuntimeAgentRunIdentityV1,
        mut session: SessionStateSnapshot,
    ) -> Result<Option<TurnStepResult>, String> {
        let open = session.model_semantics.values().filter_map(|semantic| {
            match semantic {
                ModelMessageSemanticsV1::Assistant { tool_calls, .. } if tool_calls.len()==1 && tool_calls[0].name==TOOL_NAME => {
                    let id=&tool_calls[0].id;
                    (!session.model_semantics.values().any(|value| matches!(value,ModelMessageSemanticsV1::ToolResult {tool_call_id,..} if tool_call_id==id))).then_some(id.clone())
                }
                _ => None,
            }
        }).collect::<Vec<_>>();
        if open.is_empty() {
            return Ok(None);
        }
        let mut offset = 0;
        loop {
            let events = self
                .runtime_store
                .list_events(&session.session_id, 256, offset)
                .map_err(|error| error.to_string())?;
            let count = events.len();
            for event in events
                .into_iter()
                .filter(|event| event.event_type == "runtime.question.closed.v1")
            {
                let closure: QuestionClosure =
                    serde_json::from_str(&event.payload_json).map_err(|error| error.to_string())?;
                let wait: QuestionCheckpoint =
                    serde_json::from_str(&closure.checkpoint.payload_json)
                        .map_err(|error| error.to_string())?;
                wait.validate()?;
                if wait.agent_run_id != identity.agent_run_id || !open.contains(&wait.tool_call.id)
                {
                    continue;
                }
                if closure.schema != "runtime.question.closed.v1"
                    || wait.authorization_digest != identity.authorization_digest
                    || closure.checkpoint.session_id != session.session_id
                    || closure.checkpoint.turn_id != wait.turn_id
                    || closure.checkpoint.kind != contracts::CheckpointKindV1::Wait
                    || closure.checkpoint.done_reason.as_deref() != Some("question")
                    || event.task_id.as_deref() != Some(wait.turn_id.as_str())
                    || event.event_id != format!("runtime_question_closed:{}", wait.continuation_id)
                    || closure.result.tool_call_id != wait.tool_call.id
                    || closure.result.tool_name != TOOL_NAME
                {
                    return Err("question_closure_recovery_binding_mismatch".into());
                }
                let generate = original_generate(&session, &wait)?;
                self.repair_tool_batch_transcript(
                    &mut session,
                    &generate,
                    std::slice::from_ref(&closure.result),
                )?;
                self.session_manager.save_session(&session)?;
                return Ok(Some(TurnStepResult {
                    current_assistant_reply: None,
                    turn_id: wait.turn_id,
                    continuation: QueryContinuation::ExecuteTools,
                    checkpoint: None,
                    provider_tool_calls: generate.tool_calls,
                    tool_results: vec![closure.result],
                    tool_use_summary: None,
                    tool_operations_json: None,
                    agent_run_resource_usage: AgentRunResourceUsageV1::default(),
                    runtime_events: Vec::new(),
                    session_snapshot: session,
                }));
            }
            if count < 256 {
                break;
            }
            offset += count;
        }
        Ok(None)
    }

    pub(super) fn finish_question_wait(
        &self,
        checkpoint: CheckpointRecord,
        identity: Option<&RuntimeAgentRunIdentityV1>,
        mut session: SessionStateSnapshot,
        abandoned: Option<&str>,
        tool_safe_point: Option<&ToolSafePointDispatcher<'_>>,
    ) -> Result<TurnStepResult, String> {
        let wait: QuestionCheckpoint = serde_json::from_str(&checkpoint.payload_json)
            .map_err(|error| format!("decode question v2 checkpoint failed: {error}"))?;
        wait.validate()?;
        if wait.turn_id != checkpoint.turn_id {
            return Err("question_checkpoint_turn_mismatch".into());
        }
        if abandoned.is_none() {
            let identity = identity.ok_or("question_wait_resume_requires_agent_run_identity")?;
            identity.validate()?;
            if identity.agent_run_id != wait.agent_run_id
                || identity.authorization_digest != wait.authorization_digest
            {
                return Err("question wait resume identity mismatch".into());
            }
        }
        let mut offset = 0;
        let mut response = None;
        while abandoned.is_none() {
            let events = self
                .runtime_store
                .list_events(&checkpoint.session_id, 256, offset)
                .map_err(|error| error.to_string())?;
            let count = events.len();
            if let Some(event) = events
                .into_iter()
                .find(|event| event.event_id == response_event_id(&wait.continuation_id))
            {
                let value: QuestionResponse = serde_json::from_str(&event.payload_json)
                    .map_err(|error| format!("decode question response failed: {error}"))?;
                value.validate_binding(&wait)?;
                response = Some((value, event.at_ms));
                break;
            }
            if count < 256 {
                break;
            }
            offset += count;
        }
        if response.is_none() && abandoned.is_none() {
            return Ok(TurnStepResult {
                current_assistant_reply: None,
                turn_id: checkpoint.turn_id.clone(),
                continuation: QueryContinuation::AwaitQuestion,
                checkpoint: Some(checkpoint),
                provider_tool_calls: Vec::new(),
                tool_results: Vec::new(),
                tool_use_summary: None,
                tool_operations_json: None,
                agent_run_resource_usage: AgentRunResourceUsageV1::default(),
                runtime_events: Vec::new(),
                session_snapshot: session,
            });
        }
        let at_ms = response.as_ref().map(|(_, at)| *at).unwrap_or_else(now_ms);
        let details = if let Some((response, _)) = response {
            serde_json::json!({"questionId":wait.question_id,"action":response.action,"answer":response.answer})
        } else {
            serde_json::json!({"questionId":wait.question_id,"action":"skip","reason":abandoned})
        };
        let reason = abandoned.unwrap_or("question_answered");
        let result = ToolExecutionResult {
            tool_call_id: wait.tool_call.id.clone(),
            tool_name: TOOL_NAME.into(),
            status: "ok".into(),
            content: details.to_string(),
            details,
            facts: Vec::new(),
            error: None,
            started_at_ms: checkpoint.updated_at_ms,
            completed_at_ms: at_ms,
            latency_ms: at_ms.saturating_sub(checkpoint.updated_at_ms),
            parallel_group: None,
            transition_reason: Some(reason.into()),
        };
        let generate = original_generate(&session, &wait)?;
        self.repair_tool_batch_transcript(&mut session, &generate, std::slice::from_ref(&result))?;
        // Persist the paired provider result before removing its recovery checkpoint.
        self.session_manager.save_session(&session)?;
        let changed = RuntimeWaitChangedV1 {
            schema: contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.into(),
            continuation_id: wait.continuation_id.clone(),
            agent_run_id: wait.agent_run_id.clone(),
            status: if abandoned.is_some() {
                RuntimeWaitStatusV1::Abandoned
            } else {
                RuntimeWaitStatusV1::Resumed
            },
            transition_reason: reason.into(),
            at_ms,
        };
        let event = RuntimeEvent {
            event_id: format!(
                "runtime_wait:{}:{}",
                wait.continuation_id,
                if abandoned.is_some() {
                    "abandoned"
                } else {
                    "resumed"
                }
            ),
            session_id: checkpoint.session_id.clone(),
            task_id: Some(checkpoint.turn_id.clone()),
            event_type: contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1.into(),
            at_ms,
            visibility: EventVisibility::User,
            payload_json: serde_json::to_string(&changed).map_err(|error| error.to_string())?,
        };
        if let Some(sink) = tool_safe_point {
            let contract = tool_contract();
            sink.commit(ToolSafePoint::DurableToolCall {
                session_id: checkpoint.session_id.clone(),
                turn_id: checkpoint.turn_id.clone(),
                agent_run_id: wait.agent_run_id.clone(),
                call: wait.tool_call(),
                provider_id: crate::tool::BUILTIN_TOOL_PROVIDER_ID.into(),
                tool_contract_digest: contract.contract_digest()?,
                recorded_at_ms: checkpoint.updated_at_ms,
            })?;
            sink.commit(ToolSafePoint::DurableReceipt {
                session_id: checkpoint.session_id.clone(),
                turn_id: checkpoint.turn_id.clone(),
                agent_run_id: wait.agent_run_id.clone(),
                call: wait.tool_call(),
                result: result.clone(),
            })?;
        }
        self.runtime_store.consume_wait_checkpoint(
            crate::session::store::ConsumeWaitCheckpointRequest {
                checkpoint: checkpoint.clone(),
                events: vec![
                    event,
                    RuntimeEvent {
                        event_id: format!("runtime_question_closed:{}", wait.continuation_id),
                        session_id: checkpoint.session_id.clone(),
                        task_id: Some(checkpoint.turn_id.clone()),
                        event_type: "runtime.question.closed.v1".into(),
                        at_ms,
                        visibility: EventVisibility::Internal,
                        payload_json: serde_json::to_string(&QuestionClosure {
                            schema: "runtime.question.closed.v1".into(),
                            checkpoint: checkpoint.clone(),
                            result: result.clone(),
                        })
                        .map_err(|error| error.to_string())?,
                    },
                ],
            },
        )?;
        let mut runtime_events = build_runtime_event_tool_result_events(
            &checkpoint.session_id,
            &checkpoint.turn_id,
            std::slice::from_ref(&result),
            None,
        )?;
        runtime_events.push(build_runtime_event_runtime_wait_changed(
            &checkpoint.session_id,
            &checkpoint.turn_id,
            &changed,
        )?);
        Ok(TurnStepResult {
            current_assistant_reply: None,
            turn_id: checkpoint.turn_id,
            continuation: QueryContinuation::ExecuteTools,
            checkpoint: None,
            provider_tool_calls: generate.tool_calls,
            tool_results: vec![result],
            tool_use_summary: None,
            tool_operations_json: None,
            agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            runtime_events,
            session_snapshot: session,
        })
    }
}

pub fn tool_contract() -> crate::tool::ToolContract {
    let definition = tool_definition();
    crate::tool::ToolContract {
        name: definition.name,
        category: "user.input".into(),
        summary: definition.description,
        input_schema: definition.input_schema,
        concurrency_safe: false,
        turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        provider_id: Some(crate::tool::BUILTIN_TOOL_PROVIDER_ID.into()),
        schema_hash: None,
        scopes: Vec::new(),
        dynamic: false,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QuestionClosure {
    schema: String,
    checkpoint: CheckpointRecord,
    result: ToolExecutionResult,
}

fn original_generate(
    session: &SessionStateSnapshot,
    wait: &QuestionCheckpoint,
) -> Result<GenerateResult, String> {
    let (assistant_content, assistant_reasoning) = session
        .messages
        .iter()
        .find_map(
            |message| match session.model_semantics.get(&message.message_id) {
                Some(ModelMessageSemanticsV1::Assistant {
                    reasoning_content,
                    tool_calls,
                }) if tool_calls.len() == 1 && tool_calls[0].id == wait.tool_call.id => {
                    Some((message.content.clone(), reasoning_content.clone()))
                }
                _ => None,
            },
        )
        .ok_or("question_original_assistant_message_missing")?;
    Ok(GenerateResult {
        content: assistant_content,
        tool_calls: vec![wait.tool_call()],
        continuation_reasoning_content: assistant_reasoning,
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    })
}
