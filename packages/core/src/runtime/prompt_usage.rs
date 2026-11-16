//! Request-bound usage anchors for incremental model-input admission estimates.

use std::io::{self, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::prepared_prompt::{
    estimate_prepared_prompt_tokens_with_images, ModelInputImageObservationV1, ModelMessageRoleV1,
    ModelMessageV1, ModelToolCallV1, PreparedPromptV1, PromptTokenEstimate,
};
use crate::model::{GenerateResult, ModelSessionConfig};

const ANCHOR_SCHEMA: &str = "runtime.prompt_usage_anchor.v2";

/// A measured request prefix, containing identities and costs rather than text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptUsageAnchorV2 {
    schema: String,
    binding_digest: String,
    covered_message_digests: Vec<String>,
    actual_total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromptMessageCostV1 {
    digest: String,
    estimated_tokens: u32,
}

/// A frozen request's basis, retained until its actual input usage is known.
#[derive(Debug, Clone)]
pub struct PromptUsageBasisV1 {
    binding_digest: Option<String>,
    messages: Arc<[PromptMessageCostV1]>,
    token_estimate: Arc<PromptTokenEstimate>,
    estimated_input_tokens: u32,
}

impl PromptUsageBasisV1 {
    pub(super) fn from_prompt(
        prompt: &PreparedPromptV1,
        config: Option<&ModelSessionConfig>,
    ) -> Result<Self, String> {
        Self::from_prompt_with_images(prompt, config, &[])
    }

    pub(super) fn from_prompt_with_images(
        prompt: &PreparedPromptV1,
        config: Option<&ModelSessionConfig>,
        pending_images: &[ModelInputImageObservationV1],
    ) -> Result<Self, String> {
        let token_estimate =
            estimate_prepared_prompt_tokens_with_images(prompt, config, pending_images)?;
        let binding_digest = config
            .filter(|_| pending_images.is_empty())
            .map(|config| request_binding(prompt, config))
            .transpose()?;
        let messages = if binding_digest.is_some() {
            prompt
                .messages
                .iter()
                .zip(&token_estimate.message_tokens)
                .map(|(message, &estimated_tokens)| {
                    Ok(PromptMessageCostV1 {
                        digest: message_digest(message)?,
                        estimated_tokens,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?
        } else {
            // Opaque routes and unresolved images cannot establish coverage.
            // Their content costs need no prefix identity serialization.
            Vec::new()
        };
        let estimated_input_tokens = token_estimate.total_tokens();
        Ok(Self {
            binding_digest,
            messages: messages.into(),
            token_estimate: Arc::new(token_estimate),
            estimated_input_tokens,
        })
    }

    pub(super) fn estimated_input_tokens(&self) -> u32 {
        self.estimated_input_tokens
    }

    pub(super) fn token_estimate(&self) -> &PromptTokenEstimate {
        &self.token_estimate
    }

    pub(super) fn pending_output(
        self,
        result: &GenerateResult,
    ) -> Result<PendingOutputUsage, String> {
        let total = result
            .total_tokens
            .and_then(|tokens| u64::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0)
            .filter(|tokens| {
                result
                    .input_tokens
                    .is_none_or(|input| input >= 0 && *tokens >= input as u64)
            });
        let reported_output = match (self.binding_digest.as_ref(), total) {
            (Some(_), Some(total_tokens)) => {
                let output = ModelMessageV1 {
                    message_id: String::new(),
                    role: ModelMessageRoleV1::Assistant,
                    content: result.content.trim().to_string(),
                    tool_calls: result
                        .tool_calls
                        .iter()
                        .map(|call| ModelToolCallV1 {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            args_json: call.args_json.clone(),
                        })
                        .collect(),
                    tool_call_id: None,
                    reasoning_content: result.continuation_reasoning_content.clone(),
                };
                Some(ReportedOutputUsage {
                    total_tokens,
                    output_digest: message_digest(&output)?,
                })
            }
            _ => None,
        };
        Ok(PendingOutputUsage {
            basis: self,
            reported_output,
        })
    }

    pub(super) fn retain_matching_anchor(&self, anchor: &mut Option<PromptUsageAnchorV2>) -> bool {
        if anchor
            .as_ref()
            .is_some_and(|anchor| anchor.estimate(self).is_none())
        {
            *anchor = None;
            return true;
        }
        false
    }
}

/// Usage is promoted only alongside the exact Assistant projection it covers.
#[derive(Debug, Clone)]
pub(super) struct PendingOutputUsage {
    basis: PromptUsageBasisV1,
    reported_output: Option<ReportedOutputUsage>,
}

#[derive(Debug, Clone)]
struct ReportedOutputUsage {
    total_tokens: u64,
    output_digest: String,
}

impl PendingOutputUsage {
    pub(super) fn commit_assistant(
        &self,
        session: &mut crate::session::state::SessionStateSnapshot,
        message_id: &str,
    ) -> Result<bool, String> {
        if let Some(reported) = &self.reported_output {
            let message = session
                .messages
                .iter()
                .find(|message| message.message_id == message_id)
                .ok_or("usage-covered Assistant message missing")?;
            let output = crate::model::prepared_prompt::project_chat_message_to_model_message(
                message,
                session.model_semantics_for(message_id)?,
            )?;
            // Truncated or recombined recovery output is not the reported projection.
            if message_digest(&output)? == reported.output_digest {
                let mut covered_message_digests = self
                    .basis
                    .messages
                    .iter()
                    .map(|message| message.digest.clone())
                    .collect::<Vec<_>>();
                covered_message_digests.push(reported.output_digest.clone());
                let anchor = PromptUsageAnchorV2 {
                    schema: ANCHOR_SCHEMA.into(),
                    binding_digest: self
                        .basis
                        .binding_digest
                        .clone()
                        .ok_or("usage request binding missing")?,
                    covered_message_digests,
                    actual_total_tokens: reported.total_tokens,
                };
                if session.history.prompt_usage_anchor.as_ref() == Some(&anchor) {
                    return Ok(false);
                }
                session.history.prompt_usage_anchor = Some(anchor);
                return Ok(true);
            }
        }
        Ok(self
            .basis
            .retain_matching_anchor(&mut session.history.prompt_usage_anchor))
    }
}

fn request_binding(
    prompt: &PreparedPromptV1,
    config: &ModelSessionConfig,
) -> Result<String, String> {
    let images = prompt
        .input_images
        .iter()
        .map(|image| {
            let digest = Sha256::digest(image.data_base64.as_bytes());
            (
                &image.message_id,
                &image.content_type,
                &image.placeholder,
                format!("sha256:{digest:x}"),
            )
        })
        .collect::<Vec<_>>();
    super::canonical_json::sha256(
        "centaeris.prompt_usage_binding.utf8_content.v1",
        &(
            config,
            &prompt.system_prompt,
            &prompt.tool_definitions,
            &prompt.tool_choice,
            prompt.max_output_tokens,
            images,
        ),
    )
}

impl PromptUsageAnchorV2 {
    /// Rejects unsupported or damaged persisted usage identities.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ANCHOR_SCHEMA
            || self.actual_total_tokens == 0
            || !is_digest(&self.binding_digest)
            || self.covered_message_digests.len() < 2
            || self
                .covered_message_digests
                .iter()
                .any(|digest| !is_digest(digest))
        {
            return Err("prompt_usage_anchor_invalid".into());
        }
        Ok(())
    }

    pub(super) fn estimate(&self, basis: &PromptUsageBasisV1) -> Option<u32> {
        if self.schema != ANCHOR_SCHEMA
            || Some(&self.binding_digest) != basis.binding_digest.as_ref()
            || self.covered_message_digests.len() > basis.messages.len()
            || !self
                .covered_message_digests
                .iter()
                .zip(basis.messages.iter())
                .all(|(previous, current)| previous == &current.digest)
        {
            return None;
        }
        Some(
            basis.messages[self.covered_message_digests.len()..]
                .iter()
                .fold(self.actual_total_tokens, |tokens, message| {
                    tokens.saturating_add(u64::from(message.estimated_tokens))
                })
                .min(u64::from(u32::MAX)) as u32,
        )
    }
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn message_digest(message: &ModelMessageV1) -> Result<String, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ModelContent<'a> {
        role: &'a crate::model::prepared_prompt::ModelMessageRoleV1,
        content: &'a str,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: &'a Vec<crate::model::prepared_prompt::ModelToolCallV1>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_call_id: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: &'a Option<String>,
    }
    // Internal provenance IDs are not sent as chat message text. Identical
    // runtime instructions with a new turn identity remain the same prefix.
    let content = ModelContent {
        role: &message.role,
        content: &message.content,
        tool_calls: &message.tool_calls,
        tool_call_id: &message.tool_call_id,
        reasoning_content: &message.reasoning_content,
    };
    let mut measured = MessageMeasure(Sha256::new());
    serde_json::to_writer(&mut measured, &content)
        .map_err(|error| format!("measure model input contribution failed: {error}"))?;
    Ok(format!("sha256:{:x}", measured.0.finalize()))
}

struct MessageMeasure(Sha256);

impl Write for MessageMeasure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::prepared_prompt::project_chat_message_to_model_message;
    use crate::runtime::message_handler::{MessageHandler, MessageHandlerConfig};
    use crate::session::state::SessionStateSnapshot;
    use crate::tool::ModelToolChoice;

    fn message(id: &str, role: ModelMessageRoleV1, content: &str) -> ModelMessageV1 {
        ModelMessageV1 {
            message_id: id.into(),
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning_content: None,
        }
    }
    fn prompt() -> PreparedPromptV1 {
        PreparedPromptV1::new(
            Some("Fixed instructions".into()),
            vec![message(
                "original",
                ModelMessageRoleV1::User,
                "Read the source",
            )],
            Vec::new(),
            ModelToolChoice::None,
            4096,
        )
        .unwrap()
    }
    fn usage() -> GenerateResult {
        GenerateResult {
            content: "Observed source".into(),
            tool_calls: Vec::new(),
            continuation_reasoning_content: None,
            reasoning_content: None,
            input_tokens: Some(10_000),
            total_tokens: Some(10_500),
            prompt_cache_hit_tokens: Some(8_000),
            prompt_cache_miss_tokens: Some(2_000),
        }
    }
    fn commit_output(
        prompt: &mut PreparedPromptV1,
        config: Option<&ModelSessionConfig>,
        result: &GenerateResult,
        previous: Option<PromptUsageAnchorV2>,
    ) -> Option<PromptUsageAnchorV2> {
        let pending = PromptUsageBasisV1::from_prompt(prompt, config)
            .unwrap()
            .pending_output(result)
            .unwrap();
        let mut session = SessionStateSnapshot::new("usage-test".into(), 0);
        session.history.prompt_usage_anchor = previous;
        let id = MessageHandler::new(MessageHandlerConfig::default()).push_model_assistant_message(
            &mut session,
            &result.content,
            Default::default(),
            crate::runtime::build_model_assistant_semantics(result),
        );
        pending.commit_assistant(&mut session, &id).unwrap();
        let output = project_chat_message_to_model_message(
            &session.messages[0],
            session.model_semantics_for(&id).unwrap(),
        )
        .unwrap();
        prompt.messages.push(output);
        session.history.prompt_usage_anchor
    }
    #[test]
    fn total_usage_covers_assistant_output_but_not_tool_results_or_cache_twice() {
        let config = ModelSessionConfig::default();
        let mut prompt = prompt();
        let mut result = usage();
        result.content.clear();
        result.tool_calls.push(crate::model::ToolCallEnvelope {
            id: "call-one".into(),
            name: "read".into(),
            args_json: "{}".into(),
        });
        let anchor = commit_output(&mut prompt, Some(&config), &result, None).unwrap();
        prompt.messages[0].message_id = "same-content-new-provenance".into();
        let basis = PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap();
        assert_eq!(anchor.estimate(&basis), Some(10_500));
        let mut tool = message("result", ModelMessageRoleV1::Tool, &"x".repeat(800));
        tool.tool_call_id = Some("call-one".into());
        prompt.messages.push(tool);
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap()),
            Some(10_700)
        );
        anchor.validate().unwrap();
    }
    #[test]
    fn usage_anchor_rejects_changed_input_output_model_or_binding() {
        let config = ModelSessionConfig::default();
        let mut prompt = prompt();
        let anchor = commit_output(&mut prompt, Some(&config), &usage(), None).unwrap();
        for index in 0..prompt.messages.len() {
            let mut changed = prompt.clone();
            changed.messages[index].content.push_str(" changed");
            assert_eq!(
                anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
                None
            );
        }
        let mut changed_config = config.clone();
        changed_config.model = "another-model".into();
        assert_eq!(
            anchor.estimate(
                &PromptUsageBasisV1::from_prompt(&prompt, Some(&changed_config)).unwrap()
            ),
            None
        );
        let mut changed = prompt.clone();
        changed.system_prompt = Some("Changed instructions".into());
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
            None
        );
        changed = prompt.clone();
        changed
            .tool_definitions
            .push(crate::tool::ModelToolDefinition {
                name: "read".into(),
                description: "Read source".into(),
                input_schema: serde_json::json!({"type":"object"}),
            });
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
            None
        );
        changed = prompt.clone();
        changed.messages[0].content.push_str(" [image]");
        changed
            .input_images
            .push(crate::model::prepared_prompt::ModelInputImageV1 {
                message_id: "original".into(),
                content_type: "image/png".into(),
                placeholder: "[image]".into(),
                data_base64: "eA==".into(),
            });
        let image_anchor = commit_output(&mut changed, Some(&config), &usage(), None).unwrap();
        changed.input_images[0].data_base64 = "eHg=".into();
        assert_eq!(
            image_anchor
                .estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
            None
        );
    }
    #[test]
    fn usage_basis_matches_model_visible_estimate_with_unicode() {
        let mut prompt = prompt();
        prompt.messages[0].message_id = "identifier\"雪".into();
        prompt.messages[0].content = "世界\n\"escaped\"".into();
        assert_eq!(
            PromptUsageBasisV1::from_prompt(&prompt, Some(&ModelSessionConfig::default()))
                .unwrap()
                .estimated_input_tokens(),
            crate::runtime::generate_request::estimate_prepared_prompt_input_tokens(&prompt)
                .unwrap()
        );
    }
    #[test]
    fn missing_total_usage_keeps_the_previous_output_boundary() {
        let config = ModelSessionConfig::default();
        let mut prompt = prompt();
        let anchor = commit_output(&mut prompt, Some(&config), &usage(), None).unwrap();
        let mut missing = usage();
        missing.content = "done".into();
        missing.total_tokens = None;
        let retained =
            commit_output(&mut prompt, Some(&config), &missing, Some(anchor.clone())).unwrap();
        assert_eq!(retained, anchor);
        assert_eq!(
            retained.estimate(&PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap()),
            Some(10_501)
        );
        let changed_config = ModelSessionConfig {
            model: "changed-model".into(),
            ..config
        };
        assert!(
            commit_output(&mut prompt, Some(&changed_config), &missing, Some(retained)).is_none()
        );
    }
    #[test]
    fn unreported_or_invalid_total_does_not_promote_input_only_usage() {
        for total in [None, Some(0), Some(-1), Some(9_999)] {
            let mut prompt = prompt();
            let mut result = usage();
            result.total_tokens = total;
            assert!(commit_output(
                &mut prompt,
                Some(&ModelSessionConfig::default()),
                &result,
                None
            )
            .is_none());
        }
    }
    #[test]
    fn coverage_is_committed_once_and_rejects_recombined_or_truncated_output() {
        let config = ModelSessionConfig::default();
        let prompt = prompt();
        let result = usage();
        let pending = PromptUsageBasisV1::from_prompt(&prompt, Some(&config))
            .unwrap()
            .pending_output(&result)
            .unwrap();
        let handler = MessageHandler::new(MessageHandlerConfig::default());
        let mut session = SessionStateSnapshot::new("covered-output".into(), 0);
        let id = handler.push_model_assistant_message(
            &mut session,
            &result.content,
            Default::default(),
            crate::runtime::build_model_assistant_semantics(&result),
        );
        assert!(pending.commit_assistant(&mut session, &id).unwrap());
        assert!(!pending.commit_assistant(&mut session, &id).unwrap());
        for altered in ["old partialObserved source", "Observed"] {
            let mut session = SessionStateSnapshot::new("altered-output".into(), 0);
            let id = handler.push_model_assistant_message(
                &mut session,
                altered,
                Default::default(),
                crate::runtime::build_model_assistant_semantics(&result),
            );
            assert!(!pending.commit_assistant(&mut session, &id).unwrap());
            assert!(session.history.prompt_usage_anchor.is_none());
        }
    }
    #[test]
    fn output_anchor_round_trips_and_old_schema_is_rejected() {
        let config = ModelSessionConfig::default();
        let mut prompt = prompt();
        prompt.messages[0].content.clear();
        let anchor = commit_output(&mut prompt, Some(&config), &usage(), None).unwrap();
        let mut value = serde_json::to_value(&anchor).unwrap();
        let restored: PromptUsageAnchorV2 = serde_json::from_value(value.clone()).unwrap();
        restored.validate().unwrap();
        assert_eq!(
            restored.estimate(&PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap()),
            Some(10_500)
        );
        value["schema"] = serde_json::json!("runtime.prompt_usage_anchor.v1");
        assert!(serde_json::from_value::<PromptUsageAnchorV2>(value)
            .unwrap()
            .validate()
            .is_err());
    }
    #[test]
    fn opaque_routes_cannot_create_or_retain_coverage() {
        let mut prompt = prompt();
        let anchor = commit_output(
            &mut prompt,
            Some(&ModelSessionConfig::default()),
            &usage(),
            None,
        );
        assert!(commit_output(&mut prompt, None, &usage(), anchor).is_none());
    }
}
