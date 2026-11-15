use std::collections::{HashMap, HashSet};

use crate::model::protocol_adapters::replayed_reasoning_text_bytes;
use crate::model::ModelSessionConfig;
use crate::tool::ModelToolDefinition;

use super::{ModelInputImageObservationV1, ModelMessageV1, PreparedPromptV1};

/// Codex's ordinary image estimate, converted with the same four-byte token heuristic.
const IMAGE_VISIBLE_BYTES: usize = 7373;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptTokenEstimate {
    pub(crate) system_tokens: u32,
    pub(crate) message_tokens: Vec<u32>,
    pub(crate) tool_tokens: Vec<u32>,
}

impl PromptTokenEstimate {
    pub(crate) fn total_tokens(&self) -> u32 {
        self.message_tokens
            .iter()
            .chain(&self.tool_tokens)
            .fold(self.system_tokens, |total, tokens| {
                total.saturating_add(*tokens)
            })
    }
}

pub(crate) fn estimate_prepared_prompt_tokens(
    prompt: &PreparedPromptV1,
    config: Option<&ModelSessionConfig>,
) -> Result<PromptTokenEstimate, String> {
    estimate_prepared_prompt_tokens_with_images(prompt, config, &[])
}

/// Estimates validated prompt content before or after image bytes have been resolved.
pub(crate) fn estimate_prepared_prompt_tokens_with_images(
    prompt: &PreparedPromptV1,
    config: Option<&ModelSessionConfig>,
    pending_images: &[ModelInputImageObservationV1],
) -> Result<PromptTokenEstimate, String> {
    let mut image_placeholders = HashSet::new();
    let mut image_costs = HashMap::<&str, (usize, usize)>::new();
    for (message_id, placeholder) in prompt
        .input_images
        .iter()
        .map(|image| (image.message_id.as_str(), image.placeholder.as_str()))
        .chain(
            pending_images
                .iter()
                .map(|image| (image.message_id.as_str(), image.source.placeholder())),
        )
    {
        if image_placeholders.insert((message_id, placeholder)) {
            let (placeholder_bytes, image_bytes) = image_costs.entry(message_id).or_default();
            *placeholder_bytes = placeholder_bytes.saturating_add(placeholder.len());
            *image_bytes = image_bytes.saturating_add(IMAGE_VISIBLE_BYTES);
        }
    }
    let message_tokens = prompt
        .messages
        .iter()
        .map(|message| {
            let (placeholder_bytes, image_bytes) = image_costs
                .get(message.message_id.as_str())
                .copied()
                .unwrap_or_default();
            model_message_visible_bytes(message, config).map(|bytes| {
                estimate_bytes_tokens(
                    bytes
                        .saturating_sub(placeholder_bytes)
                        .saturating_add(image_bytes),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tool_tokens = prompt
        .tool_definitions
        .iter()
        .map(estimate_tool_tokens)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PromptTokenEstimate {
        system_tokens: prompt
            .system_prompt
            .as_deref()
            .map_or(0, super::estimate_text_tokens),
        message_tokens,
        tool_tokens,
    })
}

#[cfg(test)]
pub(crate) fn estimate_model_message_tokens(message: &ModelMessageV1) -> u32 {
    let bytes = message
        .content
        .len()
        .saturating_add(message.reasoning_content.as_deref().map_or(0, str::len));
    estimate_bytes_tokens(tool_call_bytes(message, bytes))
}

fn model_message_visible_bytes(
    message: &ModelMessageV1,
    config: Option<&ModelSessionConfig>,
) -> Result<usize, String> {
    let reasoning_bytes = message
        .reasoning_content
        .as_deref()
        .map(|reasoning| replayed_reasoning_text_bytes(config, reasoning))
        .transpose()?
        .unwrap_or_default();
    Ok(tool_call_bytes(
        message,
        message.content.len().saturating_add(reasoning_bytes),
    ))
}

fn tool_call_bytes(message: &ModelMessageV1, bytes: usize) -> usize {
    message.tool_calls.iter().fold(bytes, |bytes, call| {
        bytes
            .saturating_add(call.name.len())
            .saturating_add(call.args_json.len())
    })
}

fn estimate_tool_tokens(tool: &ModelToolDefinition) -> Result<u32, String> {
    let schema = serde_json::to_vec(&tool.input_schema)
        .map_err(|error| format!("serialize model tool input schema failed: {error}"))?;
    Ok(estimate_bytes_tokens(
        tool.name
            .len()
            .saturating_add(tool.description.len())
            .saturating_add(schema.len()),
    ))
}

pub(super) fn estimate_bytes_tokens(bytes: usize) -> u32 {
    u32::try_from(bytes.div_ceil(4)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::model::prepared_prompt::{
        estimate_projected_message_tokens, estimate_text_tokens, ModelInputImageSourceRefV1,
        ModelInputImageV1, ModelMessageRoleV1, ModelToolCallV1,
    };
    use crate::model::ModelProviderKind;
    use crate::runtime::contracts::JsonMap;
    use crate::session::state::{
        ChatMessage, MessageRole, ModelMessageSemanticsV1, ModelToolCallStateV1,
    };
    use crate::tool::ModelToolChoice;

    use super::*;

    fn message(id: &str, role: ModelMessageRoleV1, content: &str) -> ModelMessageV1 {
        ModelMessageV1 {
            message_id: id.to_string(),
            role,
            content: content.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn tool_prompt() -> PreparedPromptV1 {
        let mut assistant = message("assistant", ModelMessageRoleV1::Assistant, "好");
        assistant.reasoning_content = Some("考虑".to_string());
        assistant.tool_calls.push(ModelToolCallV1 {
            id: "call".to_string(),
            name: "read".to_string(),
            args_json: "{}".to_string(),
        });
        let mut result = message("result", ModelMessageRoleV1::Tool, "结果");
        result.tool_call_id = Some("call".to_string());
        PreparedPromptV1::new(
            Some("系统".to_string()),
            vec![
                message("user", ModelMessageRoleV1::User, "中"),
                assistant,
                result,
            ],
            vec![
                ModelToolDefinition {
                    name: "read".to_string(),
                    description: "读取".to_string(),
                    input_schema: json!({}),
                },
                ModelToolDefinition {
                    name: "edit".to_string(),
                    description: "改".to_string(),
                    input_schema: json!({}),
                },
            ],
            ModelToolChoice::Auto,
            32,
        )
        .expect("valid paired tool prompt")
    }

    #[test]
    fn text_tokens_use_utf8_bytes_and_ceiling() {
        for (text, expected) in [
            ("", 0),
            ("a", 1),
            ("abcd", 1),
            ("abcde", 2),
            ("中文", 2),
            ("🦀", 1),
        ] {
            assert_eq!(estimate_text_tokens(text), expected, "text {text:?}");
        }
    }

    #[test]
    fn message_cost_ignores_transport_identity_and_outer_json_escaping() {
        let mut original = message("short", ModelMessageRoleV1::Assistant, "\"\\\n");
        original.tool_calls.push(ModelToolCallV1 {
            id: "call".to_string(),
            name: "read".to_string(),
            args_json: "{}".to_string(),
        });
        let mut renamed = original.clone();
        renamed.message_id = "long message identity".repeat(100);
        renamed.tool_calls[0].id = "long call identity".repeat(100);
        assert_eq!(estimate_model_message_tokens(&original), 3);
        assert_eq!(
            estimate_model_message_tokens(&renamed),
            estimate_model_message_tokens(&original)
        );
        let mut result = message("tool", ModelMessageRoleV1::Tool, "\"\\\n");
        result.tool_call_id = Some("call".to_string());
        assert_eq!(estimate_model_message_tokens(&result), 1);
        result.tool_call_id = Some("different identity".repeat(100));
        assert_eq!(estimate_model_message_tokens(&result), 1);
    }

    #[test]
    fn prompt_costs_visible_content_reasoning_and_each_tool_once() {
        let estimate = estimate_prepared_prompt_tokens(&tool_prompt(), None).expect("estimate");
        assert_eq!(estimate.system_tokens, 2);
        assert_eq!(estimate.message_tokens, [1, 4, 2]);
        assert_eq!(estimate.tool_tokens, [3, 3]);
        assert_eq!(estimate.total_tokens(), 15);
    }

    #[test]
    fn projected_messages_count_typed_semantics_without_metadata_cost() {
        let message = ChatMessage {
            message_id: "assistant".to_string(),
            role: MessageRole::Assistant,
            content: "好".to_string(),
            created_at_ms: 0,
            metadata: JsonMap::from_iter([("hostMetadata".to_string(), "x".repeat(1000))]),
        };
        let semantics = ModelMessageSemanticsV1::Assistant {
            reasoning_content: Some("考虑".to_string()),
            tool_calls: vec![ModelToolCallStateV1 {
                id: "call identity".repeat(100),
                name: "read".to_string(),
                args_json: "{}".to_string(),
            }],
        };
        assert_eq!(
            estimate_projected_message_tokens(&message, &semantics).expect("estimate projection"),
            4
        );
    }

    #[test]
    fn provider_replay_counts_plain_thinking_and_excludes_opaque_signatures() {
        let mut prompt = tool_prompt();
        let deepseek = ModelSessionConfig {
            provider_kind: ModelProviderKind::DeepSeek,
            provider_id: "deepseek.default".to_string(),
            ..ModelSessionConfig::default()
        };
        assert_eq!(
            estimate_prepared_prompt_tokens(&prompt, Some(&deepseek))
                .expect("DeepSeek reasoning")
                .message_tokens[1],
            4
        );
        let gemini = ModelSessionConfig {
            provider_kind: ModelProviderKind::Gemini,
            provider_id: "gemini.default".to_string(),
            ..ModelSessionConfig::default()
        };
        prompt.messages[1].reasoning_content = Some(
            json!({
                "schema": "gemini_chat_tool_signatures.v1",
                "toolSignatures": { "call": "opaque".repeat(100) }
            })
            .to_string(),
        );
        assert_eq!(
            estimate_prepared_prompt_tokens(&prompt, Some(&gemini))
                .expect("Gemini continuation")
                .message_tokens[1],
            3
        );
        let minimax = ModelSessionConfig {
            provider_kind: ModelProviderKind::Anthropic,
            provider_id: "minimax.default".to_string(),
            ..ModelSessionConfig::default()
        };
        prompt.messages[1].reasoning_content = Some(
            json!([
                { "type": "thinking", "thinking": "考虑", "signature": "opaque".repeat(100) },
                { "type": "text", "text": "好" },
                { "type": "tool_use", "id": "call", "name": "read", "input": {} }
            ])
            .to_string(),
        );
        assert_eq!(
            estimate_prepared_prompt_tokens(&prompt, Some(&minimax))
                .expect("MiniMax signed blocks")
                .message_tokens[1],
            4
        );
        prompt.messages[1].reasoning_content = Some("unsupported continuation".to_string());
        assert!(estimate_prepared_prompt_tokens(&prompt, Some(&minimax)).is_err());
    }

    #[test]
    fn images_replace_placeholders_equally_before_and_after_resolution() {
        let mut prompt = PreparedPromptV1::new(
            None,
            vec![
                message("first", ModelMessageRoleV1::User, "look [image-a]"),
                message("second", ModelMessageRoleV1::User, "[image-b]"),
            ],
            Vec::new(),
            ModelToolChoice::None,
            32,
        )
        .expect("text prompt");
        let pending = vec![
            ModelInputImageObservationV1 {
                message_id: "first".to_string(),
                source: ModelInputImageSourceRefV1::InputRef {
                    input_ref: "input-a".to_string(),
                    content_type: "image/png".to_string(),
                    placeholder: "[image-a]".to_string(),
                },
            },
            ModelInputImageObservationV1 {
                message_id: "second".to_string(),
                source: ModelInputImageSourceRefV1::InputRef {
                    input_ref: "input-b".to_string(),
                    content_type: "image/jpeg".to_string(),
                    placeholder: "[image-b]".to_string(),
                },
            },
        ];
        let unresolved = estimate_prepared_prompt_tokens_with_images(&prompt, None, &pending)
            .expect("pending image estimate");
        assert_eq!(unresolved.message_tokens, [1845, 1844]);
        prompt
            .set_input_images(
                pending
                    .iter()
                    .map(|image| ModelInputImageV1 {
                        message_id: image.message_id.clone(),
                        content_type: image.source.content_type().to_string(),
                        placeholder: image.source.placeholder().to_string(),
                        data_base64: "eA==".to_string(),
                    })
                    .collect(),
            )
            .expect("resolved images");
        assert_eq!(
            estimate_prepared_prompt_tokens(&prompt, None).expect("resolved estimate"),
            unresolved
        );
        assert_eq!(
            estimate_prepared_prompt_tokens_with_images(&prompt, None, &pending)
                .expect("resolved and pending references to the same images"),
            unresolved
        );
        prompt.input_images[0].data_base64 = "eHh4".repeat(1000);
        prompt.input_images[0].placeholder = "[a much longer image placeholder]".to_string();
        prompt.messages[0].content = "look [a much longer image placeholder]".to_string();
        prompt.validate().expect("valid larger image payload");
        assert_eq!(
            estimate_prepared_prompt_tokens(&prompt, None).expect("larger encoded payload"),
            unresolved
        );
    }
}
