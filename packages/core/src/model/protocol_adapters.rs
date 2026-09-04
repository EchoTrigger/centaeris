use super::*;

mod anthropic_messages;
mod openai_completions;
mod openai_responses;
mod shared;
mod tool_calls;

pub use anthropic_messages::AnthropicMessagesModelClient;
pub use openai_completions::OpenAiCompatibleModelClient;
pub use openai_responses::OpenAiResponsesModelClient;
pub use tool_calls::validate_provider_tool_call_arguments;

use openai_completions::*;
use shared::*;
use tool_calls::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderInputPart {
    Text(String),
    Image {
        content_type: String,
        data_base64: String,
    },
}

fn provider_input_parts(
    request: &ModelClientRequest,
    message: &ModelMessageV1,
) -> Result<Vec<ProviderInputPart>, ModelClientError> {
    let mut images = request
        .prepared_prompt
        .input_images
        .iter()
        .filter(|image| image.message_id == message.message_id)
        .map(|image| {
            message
                .content
                .find(image.placeholder.as_str())
                .map(|position| (position, image))
                .ok_or_else(|| {
                    invalid_openai_compatible_request(format!(
                        "image placeholder is missing: messageId={} placeholder={}",
                        message.message_id, image.placeholder
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    images.sort_by_key(|(position, _)| *position);
    if images.is_empty() {
        return Ok(message
            .content
            .trim()
            .is_empty()
            .then(Vec::new)
            .unwrap_or_else(|| vec![ProviderInputPart::Text(message.content.trim().to_string())]));
    }
    let mut parts = Vec::new();
    let mut cursor = 0;
    for (position, image) in images {
        if position > cursor {
            parts.push(ProviderInputPart::Text(
                message.content[cursor..position].to_string(),
            ));
        }
        parts.push(ProviderInputPart::Image {
            content_type: image.content_type.clone(),
            data_base64: image.data_base64.clone(),
        });
        cursor = position + image.placeholder.len();
    }
    if cursor < message.content.len() {
        parts.push(ProviderInputPart::Text(
            message.content[cursor..].to_string(),
        ));
    }
    Ok(parts)
}

fn image_data_url(content_type: &str, data_base64: &str) -> String {
    format!("data:{content_type};base64,{data_base64}")
}

fn validate_provider_image_capability(
    provider: &ModelProviderInfo,
    request: &ModelClientRequest,
) -> Result<(), ModelClientError> {
    if !request.prepared_prompt.input_images.is_empty()
        && !provider.capability_profile.supports_vision
    {
        return Err(invalid_openai_compatible_request(format!(
            "model provider does not support image input: {}",
            provider.provider_key
        )));
    }
    Ok(())
}

#[cfg(test)]
use openai_responses::*;
#[cfg(test)]
mod tests;
