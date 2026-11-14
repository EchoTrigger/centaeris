//! Request-bound usage anchors for incremental model-input admission estimates.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::prepared_prompt::{ModelMessageV1, PreparedPromptV1};
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
    binding_digest: String,
    messages: Vec<PromptMessageCostV1>,
    estimated_input_tokens: u32,
}

impl PromptUsageBasisV1 {
    pub(super) fn from_prompt(
        prompt: &PreparedPromptV1,
        config: &ModelSessionConfig,
    ) -> Result<Self, String> {
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
        let binding_digest = super::canonical_json::sha256(
            "centaeris.prompt_usage_binding.v1",
            &(
                config,
                &prompt.system_prompt,
                &prompt.tool_definitions,
                &prompt.tool_choice,
                prompt.max_output_tokens,
                images,
            ),
        )?;
        let mut estimated_input_tokens = prompt
            .system_prompt
            .as_deref()
            .map(crate::model::prepared_prompt::estimate_text_tokens)
            .unwrap_or_default();
        let mut messages = Vec::with_capacity(prompt.messages.len());
        for message in &prompt.messages {
            let (cost, raw_tokens) = measured_message_cost(message)?;
            messages.push(cost);
            estimated_input_tokens = estimated_input_tokens.saturating_add(raw_tokens);
        }
        if !prompt.tool_definitions.is_empty() {
            let mut measure = MessageMeasure {
                hash: Sha256::new(),
                chars: 0,
            };
            serde_json::to_writer(&mut measure, &prompt.tool_definitions)
                .map_err(|error| format!("measure tool catalog failed: {error}"))?;
            estimated_input_tokens =
                estimated_input_tokens.saturating_add(tokens_for_chars(measure.chars));
        }
        estimated_input_tokens = estimated_input_tokens
            .saturating_add((prompt.input_images.len() as u32).saturating_mul(1024));
        Ok(Self {
            binding_digest,
            messages,
            estimated_input_tokens,
        })
    }

    pub(super) fn estimated_input_tokens(&self) -> u32 {
        self.estimated_input_tokens
    }

    pub(super) fn into_anchor(self, result: &GenerateResult) -> Option<PromptUsageAnchorV1> {
        // GenerateResult.input_tokens is already normalized by the provider
        // adapter. Cache-hit input is included; output belongs to later messages.
        let actual_input_tokens = u64::try_from(result.input_tokens?)
            .ok()
            .filter(|n| *n > 0)?;
        Some(PromptUsageAnchorV1 {
            schema: ANCHOR_SCHEMA.into(),
            binding_digest: self.binding_digest,
            messages: self.messages,
            actual_input_tokens,
        })
    }
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
                .any(|message| !is_digest(&message.digest) || message.estimated_tokens == 0)
        {
            return Err("prompt_usage_anchor_invalid".into());
        }
        Ok(())
    }

    pub(super) fn estimate(&self, basis: &PromptUsageBasisV1) -> Option<u32> {
        if self.schema != ANCHOR_SCHEMA
            || self.binding_digest != basis.binding_digest
            || self.messages.len() > basis.messages.len()
            || !self
                .messages
                .iter()
                .zip(&basis.messages)
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
    measured_message_cost(message).map(|(cost, _)| cost)
}

fn tokens_for_chars(chars: u64) -> u32 {
    ((chars.saturating_add(3) / 4).min(u64::from(u32::MAX))) as u32
}

fn measured_message_cost(message: &ModelMessageV1) -> Result<(PromptMessageCostV1, u32), String> {
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
    let mut measured = MessageMeasure {
        hash: Sha256::new(),
        chars: 0,
    };
    serde_json::to_writer(&mut measured, &content)
        .map_err(|error| format!("measure model input contribution failed: {error}"))?;
    // ModelMessageV1 additionally stores the internal messageId field. Count
    // that small field for the existing lexical metric without cloning the body.
    let encoded_id = serde_json::to_string(&message.message_id)
        .map_err(|error| format!("measure message identity failed: {error}"))?;
    let raw_chars = measured
        .chars
        .saturating_add(encoded_id.chars().count() as u64)
        .saturating_add(13);
    Ok((
        PromptMessageCostV1 {
            digest: format!("sha256:{:x}", measured.hash.finalize()),
            estimated_tokens: tokens_for_chars(measured.chars),
        },
        tokens_for_chars(raw_chars),
    ))
}

struct MessageMeasure {
    hash: Sha256,
    chars: u64,
}

impl Write for MessageMeasure {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.hash.update(bytes);
        self.chars = self
            .chars
            .saturating_add(bytes.iter().filter(|byte| **byte & 0xc0 != 0x80).count() as u64);
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
        let anchor = PromptUsageBasisV1::from_prompt(&prompt, &config)
            .unwrap()
            .into_anchor(&usage())
            .unwrap();
        prompt.messages[0].message_id = "same-content-new-provenance".into();
        let assistant = message("answer", ModelMessageRoleV1::Assistant, "Observed source");
        let delta = message_cost(&assistant).unwrap().estimated_tokens;
        prompt.messages.push(assistant);
        let basis = PromptUsageBasisV1::from_prompt(&prompt, &config).unwrap();
        assert_eq!(anchor.estimate(&basis), Some(10_000 + delta));
        anchor.validate().unwrap();
    }

    #[test]
    fn usage_anchor_rejects_changed_prefix_model_or_prompt_binding() {
        let config = ModelSessionConfig::default();
        let prompt = prompt();
        let anchor = PromptUsageBasisV1::from_prompt(&prompt, &config)
            .unwrap()
            .into_anchor(&usage())
            .unwrap();
        let mut changed = prompt.clone();
        changed.messages[0].content = "Different source".into();
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, &config).unwrap()),
            None
        );
        let mut changed_config = config.clone();
        changed_config.model = "another-model".into();
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&prompt, &changed_config).unwrap()),
            None
        );
        changed = prompt.clone();
        changed.system_prompt = Some("Different instructions".into());
        assert_eq!(
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, &config).unwrap()),
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
            anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, &config).unwrap()),
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
        let image_anchor = PromptUsageBasisV1::from_prompt(&changed, &config)
            .unwrap()
            .into_anchor(&usage())
            .unwrap();
        changed.input_images[0].data_base64 = "Y2hhbmdlZA==".into();
        assert_eq!(
            image_anchor.estimate(&PromptUsageBasisV1::from_prompt(&changed, &config).unwrap()),
            None
        );
    }

    #[test]
    fn streamed_usage_basis_preserves_lexical_estimate_with_unicode_and_tool_calls() {
        let mut prompt = prompt();
        prompt.messages[0].message_id = "identifier\"雪".into();
        prompt.messages[0].content = "世界\n\"escaped\"".into();
        let basis =
            PromptUsageBasisV1::from_prompt(&prompt, &ModelSessionConfig::default()).unwrap();
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
            PromptUsageBasisV1::from_prompt(&prompt(), &ModelSessionConfig::default())
                .unwrap()
                .into_anchor(&result)
                .is_none()
        );
    }
}
