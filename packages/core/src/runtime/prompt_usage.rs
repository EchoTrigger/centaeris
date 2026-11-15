//! Request-bound usage anchors for incremental model-input admission estimates.

use std::io::{self, Write};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::prepared_prompt::{
    estimate_prepared_prompt_tokens_with_images, ModelInputImageObservationV1, ModelMessageV1,
    PreparedPromptV1, PromptTokenEstimate,
};
use crate::model::{GenerateResult, ModelSessionConfig};

const ANCHOR_SCHEMA: &str = "runtime.prompt_usage_anchor.v1";

/// A measured request prefix, containing identities and costs rather than text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptUsageAnchorV1 {
    schema: String,
    binding_digest: String,
    messages: Vec<PromptMessageCostV1>,
    actual_input_tokens: u64,
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

    pub(super) fn into_anchor(self, result: &GenerateResult) -> Option<PromptUsageAnchorV1> {
        // Cache-hit input is included by the adapter. Output is counted only
        // when it is projected into a later request's uncovered replay tail.
        let actual_input_tokens = u64::try_from(result.input_tokens?)
            .ok()
            .filter(|n| *n > 0)?;
        Some(PromptUsageAnchorV1 {
            schema: ANCHOR_SCHEMA.into(),
            binding_digest: self.binding_digest?,
            messages: self.messages.to_vec(),
            actual_input_tokens,
        })
    }

    pub(super) fn retain_matching_anchor(&self, anchor: &mut Option<PromptUsageAnchorV1>) {
        if anchor
            .as_ref()
            .is_some_and(|anchor| anchor.estimate(self).is_none())
        {
            *anchor = None;
        }
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

impl PromptUsageAnchorV1 {
    /// Rejects unsupported or damaged persisted usage identities.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != ANCHOR_SCHEMA
            || self.actual_input_tokens == 0
            || !is_digest(&self.binding_digest)
            || self
                .messages
                .iter()
                .any(|message| !is_digest(&message.digest))
        {
            return Err("prompt_usage_anchor_invalid".into());
        }
        Ok(())
    }

    pub(super) fn estimate(&self, basis: &PromptUsageBasisV1) -> Option<u32> {
        if self.schema != ANCHOR_SCHEMA
            || Some(&self.binding_digest) != basis.binding_digest.as_ref()
            || self.messages.len() > basis.messages.len()
            || !self
                .messages
                .iter()
                .zip(basis.messages.iter())
                .all(|(previous, current)| previous.digest == current.digest)
        {
            return None;
        }
        Some(
            basis.messages[self.messages.len()..]
                .iter()
                .fold(self.actual_input_tokens, |tokens, message| {
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

#[cfg(test)]
fn message_cost(message: &ModelMessageV1) -> Result<PromptMessageCostV1, String> {
    Ok(PromptMessageCostV1 {
        digest: message_digest(message)?,
        estimated_tokens: crate::model::prepared_prompt::estimate_model_message_tokens(message),
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
    use crate::model::prepared_prompt::ModelMessageRoleV1;
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
            total_tokens: Some(12_000),
            prompt_cache_hit_tokens: Some(8_000),
            prompt_cache_miss_tokens: Some(2_000),
        }
    }

    #[test]
    fn usage_anchor_adds_only_new_messages_without_counting_cache_or_output_twice() {
        let config = ModelSessionConfig::default();
        let mut prompt = prompt();
        let anchor = PromptUsageBasisV1::from_prompt(&prompt, Some(&config))
            .unwrap()
            .into_anchor(&usage())
            .unwrap();
        prompt.messages[0].message_id = "same-content-new-provenance".into();
        let assistant = message("answer", ModelMessageRoleV1::Assistant, "Observed source");
        let delta = message_cost(&assistant).unwrap().estimated_tokens;
        prompt.messages.push(assistant);
        let basis = PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap();
        assert_eq!(anchor.estimate(&basis), Some(10_000 + delta));
        anchor.validate().unwrap();
    }

    #[test]
    fn usage_anchor_rejects_changed_prefix_model_or_prompt_binding() {
        let config = ModelSessionConfig::default();
        let prompt = prompt();
        let anchor = PromptUsageBasisV1::from_prompt(&prompt, Some(&config))
            .unwrap()
            .into_anchor(&usage())
            .unwrap();
        let mut changed = prompt.clone();
        changed.messages[0].content = "Different source".into();
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
            None
        );
        let mut changed_config = config.clone();
        changed_config.model = "another-model".into();
        assert_eq!(
            anchor.estimate(
                &PromptUsageBasisV1::from_prompt(&prompt, Some(&changed_config)).unwrap()
            ),
            None
        );
        changed = prompt.clone();
        changed.system_prompt = Some("Different instructions".into());
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
            None
        );
        changed = prompt.clone();
        changed
            .tool_definitions
            .push(crate::tool::ModelToolDefinition {
                name: "source_read".into(),
                description: "Read a selected source".into(),
                input_schema: serde_json::json!({"type": "object"}),
            });
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
            None
        );
        changed = prompt.clone();
        changed
            .input_images
            .push(crate::model::prepared_prompt::ModelInputImageV1 {
                message_id: "original".into(),
                content_type: "image/png".into(),
                placeholder: "[image]".into(),
                data_base64: "aW1hZ2U=".into(),
            });
        let image_anchor = PromptUsageBasisV1::from_prompt(&changed, Some(&config))
            .unwrap()
            .into_anchor(&usage())
            .unwrap();
        changed.input_images[0].data_base64 = "Y2hhbmdlZA==".into();
        assert_eq!(
            image_anchor
                .estimate(&PromptUsageBasisV1::from_prompt(&changed, Some(&config)).unwrap()),
            None
        );
    }

    #[test]
    fn usage_basis_matches_model_visible_estimate_with_unicode_and_tool_calls() {
        let mut prompt = prompt();
        prompt.messages[0].message_id = "identifier\"雪".into();
        prompt.messages[0].content = "世界\n\"escaped\"".into();
        let basis =
            PromptUsageBasisV1::from_prompt(&prompt, Some(&ModelSessionConfig::default())).unwrap();
        assert_eq!(
            basis.estimated_input_tokens(),
            crate::runtime::generate_request::estimate_prepared_prompt_input_tokens(&prompt)
                .unwrap()
        );
    }

    #[test]
    fn missing_input_usage_does_not_create_an_anchor() {
        let mut result = usage();
        result.input_tokens = None;
        assert!(
            PromptUsageBasisV1::from_prompt(&prompt(), Some(&ModelSessionConfig::default()))
                .unwrap()
                .into_anchor(&result)
                .is_none()
        );
    }

    #[test]
    fn missing_usage_retains_coverage_across_multiple_tool_tails() {
        let config = ModelSessionConfig::default();
        let mut prompt = prompt();
        let mut anchor = PromptUsageBasisV1::from_prompt(&prompt, Some(&config))
            .unwrap()
            .into_anchor(&usage());
        let mut assistant = message("tool-call", ModelMessageRoleV1::Assistant, "");
        assistant
            .tool_calls
            .push(crate::model::prepared_prompt::ModelToolCallV1 {
                id: "call-one".into(),
                name: "read".into(),
                args_json: "{}".into(),
            });
        let mut tool = message("tool-result", ModelMessageRoleV1::Tool, "结果");
        tool.tool_call_id = Some("call-one".into());
        prompt.messages.extend([assistant, tool]);
        let basis = PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap();
        let mut missing = usage();
        missing.input_tokens = None;
        assert!(basis.clone().into_anchor(&missing).is_none());
        basis.retain_matching_anchor(&mut anchor);
        assert_eq!(anchor.as_ref().unwrap().estimate(&basis), Some(10_004));

        prompt
            .messages
            .push(message("answer", ModelMessageRoleV1::Assistant, "done"));
        let basis = PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap();
        basis.retain_matching_anchor(&mut anchor);
        assert_eq!(anchor.as_ref().unwrap().estimate(&basis), Some(10_005));
        let mut changed = config.clone();
        changed.model = "changed-model".into();
        PromptUsageBasisV1::from_prompt(&prompt, Some(&changed))
            .unwrap()
            .retain_matching_anchor(&mut anchor);
        assert!(anchor.is_none());
    }

    #[test]
    fn zero_cost_and_previous_policy_anchors_remain_readable() {
        let config = ModelSessionConfig::default();
        let mut prompt = prompt();
        prompt.messages[0].content.clear();
        let basis = PromptUsageBasisV1::from_prompt(&prompt, Some(&config)).unwrap();
        let mut anchor = basis.clone().into_anchor(&usage()).unwrap();
        assert_eq!(anchor.messages[0].estimated_tokens, 0);
        let encoded = serde_json::to_string(&anchor).unwrap();
        let restored: PromptUsageAnchorV1 = serde_json::from_str(&encoded).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.estimate(&basis), Some(10_000));

        // The previous policy remains valid persisted data, but is not a
        // calibration for the new model-visible content projection.
        anchor.binding_digest = super::super::canonical_json::sha256(
            "centaeris.prompt_usage_binding.v1",
            &(
                &config,
                &prompt.system_prompt,
                &prompt.tool_definitions,
                &prompt.tool_choice,
                prompt.max_output_tokens,
                Vec::<String>::new(),
            ),
        )
        .unwrap();
        anchor.validate().unwrap();
        assert_eq!(anchor.estimate(&basis), None);
    }

    #[test]
    fn opaque_model_routes_cannot_create_or_retain_a_calibration() {
        let prompt = prompt();
        let basis = PromptUsageBasisV1::from_prompt(&prompt, None).unwrap();
        assert!(basis.clone().into_anchor(&usage()).is_none());
        let mut anchor =
            PromptUsageBasisV1::from_prompt(&prompt, Some(&ModelSessionConfig::default()))
                .unwrap()
                .into_anchor(&usage());
        basis.retain_matching_anchor(&mut anchor);
        assert!(anchor.is_none());
    }
}
