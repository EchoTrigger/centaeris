use base64::Engine as _;
use image::{ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;

use crate::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, SessionStateSnapshot,
};
use crate::tool::{ModelToolChoice, ModelToolDefinition};

pub const PREPARED_PROMPT_SCHEMA: &str = "prepared_prompt.v1";
pub const MODEL_INPUT_IMAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const MODEL_INPUT_IMAGE_MAX_PIXELS: u64 = 100_000_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMessageRoleV1 {
    System,
    User,
    Assistant,
    Tool,
}

impl From<&MessageRole> for ModelMessageRoleV1 {
    fn from(role: &MessageRole) -> Self {
        match role {
            MessageRole::System => Self::System,
            MessageRole::User => Self::User,
            MessageRole::Assistant => Self::Assistant,
            MessageRole::Tool => Self::Tool,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelToolCallV1 {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelMessageV1 {
    pub message_id: String,
    pub role: ModelMessageRoleV1,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ModelToolCallV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelInputImageRefV1 {
    pub input_ref: String,
    pub content_type: String,
    pub placeholder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionModelInputImageRefV1 {
    pub path: String,
    pub content_type: String,
    pub sha256: String,
    pub byte_length: u64,
    pub width_px: u32,
    pub height_px: u32,
    pub placeholder: String,
}

impl ExecutionModelInputImageRefV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.path.trim().is_empty()
            || self.placeholder.trim().is_empty()
            || self.sha256.strip_prefix("sha256:").is_none_or(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            || self.byte_length == 0
            || self.byte_length > MODEL_INPUT_IMAGE_MAX_BYTES as u64
            || self.width_px == 0
            || self.height_px == 0
            || u64::from(self.width_px).saturating_mul(u64::from(self.height_px))
                > MODEL_INPUT_IMAGE_MAX_PIXELS
            || !matches!(
                self.content_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
        {
            return Err("execution_model_input_image_ref_invalid".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "sourceKind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ModelInputImageSourceRefV1 {
    InputRef {
        #[serde(rename = "inputRef")]
        input_ref: String,
        #[serde(rename = "contentType")]
        content_type: String,
        placeholder: String,
    },
    ExecutionFile {
        image: ExecutionModelInputImageRefV1,
    },
}

impl ModelInputImageSourceRefV1 {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::InputRef {
                input_ref,
                content_type,
                placeholder,
            } if !input_ref.trim().is_empty()
                && matches!(
                    content_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp"
                )
                && !placeholder.trim().is_empty() =>
            {
                Ok(())
            }
            Self::ExecutionFile { image } => image.validate(),
            _ => Err("model_input_image_source_ref_invalid".to_string()),
        }
    }

    pub fn content_type(&self) -> &str {
        match self {
            Self::InputRef { content_type, .. } => content_type,
            Self::ExecutionFile { image } => image.content_type.as_str(),
        }
    }

    pub fn placeholder(&self) -> &str {
        match self {
            Self::InputRef { placeholder, .. } => placeholder,
            Self::ExecutionFile { image } => image.placeholder.as_str(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelInputImageObservationV1 {
    pub message_id: String,
    pub source: ModelInputImageSourceRefV1,
}

pub fn inspect_model_input_image(bytes: &[u8]) -> Result<(&'static str, u32, u32), String> {
    if bytes.is_empty() || bytes.len() > MODEL_INPUT_IMAGE_MAX_BYTES {
        return Err("model_input_image_byte_length_invalid".to_string());
    }
    let format = image::guess_format(bytes)
        .map_err(|_| "model_input_image_format_unsupported".to_string())?;
    let content_type = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::WebP => "image/webp",
        _ => return Err("model_input_image_format_unsupported".to_string()),
    };
    let reader = ImageReader::with_format(Cursor::new(bytes), format);
    let (width_px, height_px) = reader
        .into_dimensions()
        .map_err(|_| "model_input_image_dimensions_invalid".to_string())?;
    if width_px == 0
        || height_px == 0
        || u64::from(width_px).saturating_mul(u64::from(height_px)) > MODEL_INPUT_IMAGE_MAX_PIXELS
    {
        return Err("model_input_image_dimensions_invalid".to_string());
    }
    Ok((content_type, width_px, height_px))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelInputImageV1 {
    pub message_id: String,
    pub content_type: String,
    pub placeholder: String,
    pub data_base64: String,
}

pub trait ModelInputImageResolverPort: Send + Sync {
    fn resolve(&self, input_ref: &str, content_type: &str) -> Result<Vec<u8>, String>;
}

pub type SharedModelInputImageResolver = Arc<dyn ModelInputImageResolverPort>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedPromptV1 {
    pub schema: String,
    pub system_prompt: Option<String>,
    pub messages: Vec<ModelMessageV1>,
    pub tool_definitions: Vec<ModelToolDefinition>,
    pub tool_choice: ModelToolChoice,
    pub max_output_tokens: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_images: Vec<ModelInputImageV1>,
}

impl PreparedPromptV1 {
    pub fn new(
        system_prompt: Option<String>,
        messages: Vec<ModelMessageV1>,
        tool_definitions: Vec<ModelToolDefinition>,
        tool_choice: ModelToolChoice,
        max_output_tokens: u32,
    ) -> Result<Self, String> {
        let prompt = Self {
            schema: PREPARED_PROMPT_SCHEMA.to_string(),
            system_prompt,
            messages,
            tool_definitions,
            tool_choice,
            max_output_tokens,
            input_images: Vec::new(),
        };
        prompt.validate()?;
        Ok(prompt)
    }

    pub fn set_input_images(&mut self, input_images: Vec<ModelInputImageV1>) -> Result<(), String> {
        self.input_images = input_images;
        self.validate()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != PREPARED_PROMPT_SCHEMA {
            return Err(format!("prepared_prompt_schema_invalid:{}", self.schema));
        }
        if self.max_output_tokens == 0 {
            return Err("prepared_prompt_max_output_tokens_invalid".to_string());
        }
        if self.messages.is_empty() {
            return Err("prepared_prompt_messages_empty".to_string());
        }
        if self
            .system_prompt
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("prepared_prompt_system_prompt_blank".to_string());
        }
        let mut tool_names = HashSet::new();
        for definition in &self.tool_definitions {
            if definition.name.trim().is_empty()
                || definition.description.trim().is_empty()
                || !definition.input_schema.is_object()
            {
                return Err("prepared_prompt_tool_definition_invalid".to_string());
            }
            if !tool_names.insert(definition.name.as_str()) {
                return Err(format!(
                    "prepared_prompt_tool_definition_duplicate:{}",
                    definition.name
                ));
            }
        }
        if self.tool_definitions.is_empty() && self.tool_choice != ModelToolChoice::None {
            return Err("prepared_prompt_tool_choice_invalid:no_tools".to_string());
        }
        if let ModelToolChoice::Specific { name } = &self.tool_choice {
            if name.trim().is_empty() || !tool_names.contains(name.as_str()) {
                return Err(format!("prepared_prompt_tool_choice_invalid:{name}"));
            }
        }

        let mut message_ids = HashSet::new();
        let mut seen_tool_call_ids = HashSet::new();
        let mut pending_tool_call_ids = Vec::<String>::new();
        for message in &self.messages {
            if message.message_id.trim().is_empty() {
                return Err(format!(
                    "prepared_prompt_message_id_invalid:{}",
                    message.message_id
                ));
            }
            if !message_ids.insert(message.message_id.as_str()) {
                return Err(format!(
                    "prepared_prompt_message_id_duplicate:{}",
                    message.message_id
                ));
            }
            if !pending_tool_call_ids.is_empty() && message.role != ModelMessageRoleV1::Tool {
                return Err(format!(
                    "prepared_prompt_tool_pairing_invalid: assistant tool call {} must be followed by a tool result before {}",
                    pending_tool_call_ids[0], message.message_id
                ));
            }
            match message.role {
                ModelMessageRoleV1::Assistant => {
                    if message.tool_call_id.is_some() {
                        return Err(format!(
                            "prepared_prompt_role_fields_invalid:{}",
                            message.message_id
                        ));
                    }
                    for call in &message.tool_calls {
                        if call.id.trim().is_empty() || call.name.trim().is_empty() {
                            return Err(format!(
                                "prepared_prompt_tool_call_invalid: messageId={}",
                                message.message_id
                            ));
                        }
                        serde_json::from_str::<Value>(call.args_json.as_str()).map_err(|error| {
                            format!(
                                "prepared_prompt_tool_call_args_invalid: messageId={} error={error}",
                                message.message_id
                            )
                        })?;
                        if !seen_tool_call_ids.insert(call.id.as_str()) {
                            return Err(format!(
                                "prepared_prompt_tool_call_id_duplicate:{}",
                                call.id
                            ));
                        }
                        pending_tool_call_ids.push(call.id.clone());
                    }
                }
                ModelMessageRoleV1::Tool => {
                    if !message.tool_calls.is_empty() || message.reasoning_content.is_some() {
                        return Err(format!(
                            "prepared_prompt_role_fields_invalid:{}",
                            message.message_id
                        ));
                    }
                    let tool_call_id = message
                        .tool_call_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| {
                            format!(
                                "prepared_prompt_tool_result_missing_call_id: messageId={}",
                                message.message_id
                            )
                        })?;
                    let expected = pending_tool_call_ids.first().ok_or_else(|| {
                        format!(
                            "prepared_prompt_tool_result_without_call: messageId={}",
                            message.message_id
                        )
                    })?;
                    if expected != tool_call_id {
                        return Err(format!(
                            "prepared_prompt_tool_pairing_invalid: messageId={} toolCallId={} expected={expected}",
                            message.message_id, tool_call_id
                        ));
                    }
                    pending_tool_call_ids.remove(0);
                }
                ModelMessageRoleV1::System | ModelMessageRoleV1::User => {
                    if !message.tool_calls.is_empty()
                        || message.tool_call_id.is_some()
                        || message.reasoning_content.is_some()
                    {
                        return Err(format!(
                            "prepared_prompt_role_fields_invalid:{}",
                            message.message_id
                        ));
                    }
                }
            }
        }
        if let Some(tool_call_id) = pending_tool_call_ids.first() {
            return Err(format!(
                "prepared_prompt_tool_pairing_invalid: assistant tool call {tool_call_id} has no result"
            ));
        }
        let mut image_placeholders = HashSet::new();
        for image in &self.input_images {
            let message = self
                .messages
                .iter()
                .find(|message| message.message_id == image.message_id)
                .ok_or_else(|| "prepared_prompt_image_message_missing".to_string())?;
            if message.role != ModelMessageRoleV1::User
                || !matches!(
                    image.content_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp"
                )
                || image.placeholder.trim().is_empty()
                || image.data_base64.trim().is_empty()
                || message
                    .content
                    .match_indices(image.placeholder.as_str())
                    .count()
                    != 1
                || !image_placeholders
                    .insert((image.message_id.as_str(), image.placeholder.as_str()))
            {
                return Err("prepared_prompt_image_invalid".to_string());
            }
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(image.data_base64.as_str())
                .map_err(|_| "prepared_prompt_image_base64_invalid".to_string())?;
            if decoded.is_empty() {
                return Err("prepared_prompt_image_base64_invalid".to_string());
            }
        }
        Ok(())
    }
}

pub fn project_session_messages_to_model_messages(
    session: &SessionStateSnapshot,
    messages: &[ChatMessage],
) -> Result<Vec<ModelMessageV1>, String> {
    messages
        .iter()
        .map(|message| {
            project_chat_message_to_model_message(
                message,
                session.model_semantics_for(message.message_id.as_str())?,
            )
        })
        .collect()
}

pub fn project_chat_message_to_model_message(
    message: &ChatMessage,
    semantics: &ModelMessageSemanticsV1,
) -> Result<ModelMessageV1, String> {
    let (tool_calls, tool_call_id, reasoning_content) = match (message.role.clone(), semantics) {
        (
            MessageRole::System | MessageRole::User | MessageRole::Assistant,
            ModelMessageSemanticsV1::Plain,
        ) => (Vec::new(), None, None),
        (
            MessageRole::Assistant,
            ModelMessageSemanticsV1::Assistant {
                reasoning_content,
                tool_calls,
            },
        ) => (
            tool_calls
                .iter()
                .map(|call| ModelToolCallV1 {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    args_json: call.args_json.clone(),
                })
                .collect(),
            None,
            reasoning_content.clone(),
        ),
        (MessageRole::Tool, ModelMessageSemanticsV1::ToolResult { tool_call_id, .. }) => {
            (Vec::new(), Some(tool_call_id.clone()), None)
        }
        _ => {
            return Err(format!(
                "model_message_semantics_role_mismatch: messageId={} role={:?}",
                message.message_id, message.role
            ));
        }
    };
    Ok(ModelMessageV1 {
        message_id: message.message_id.clone(),
        role: ModelMessageRoleV1::from(&message.role),
        content: message.content.clone(),
        tool_calls,
        tool_call_id,
        reasoning_content,
    })
}

pub fn estimate_projected_message_tokens(
    message: &ChatMessage,
    semantics: &ModelMessageSemanticsV1,
) -> Result<u32, String> {
    let projected = project_chat_message_to_model_message(message, semantics)?;
    let encoded = serde_json::to_string(&projected)
        .map_err(|error| format!("serialize projected model message failed: {error}"))?;
    Ok(estimate_text_tokens(encoded.as_str()))
}

pub fn estimate_text_tokens(text: &str) -> u32 {
    let chars = text.chars().count();
    u32::try_from(chars.saturating_add(3) / 4).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use crate::runtime::contracts::JsonMap;
    use crate::session::state::{
        ChatMessage, MessageRole, ModelMessageSemanticsV1, ModelToolCallStateV1,
        SessionStateSnapshot,
    };

    use super::{
        inspect_model_input_image, project_session_messages_to_model_messages, ModelMessageRoleV1,
        ModelMessageV1, ModelToolCallV1, PreparedPromptV1,
    };

    #[test]
    fn image_inspection_accepts_png_jpeg_and_webp_headers() {
        for (format, expected_content_type) in [
            (image::ImageFormat::Png, "image/png"),
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
        ] {
            let mut bytes = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgb8(2, 3)
                .write_to(&mut bytes, format)
                .expect("encode image fixture");
            assert_eq!(
                inspect_model_input_image(bytes.get_ref()).expect("inspect image"),
                (expected_content_type, 2, 3)
            );
        }
    }

    fn message(id: &str, role: MessageRole, content: &str, metadata: JsonMap) -> ChatMessage {
        ChatMessage {
            message_id: id.to_string(),
            role,
            content: content.to_string(),
            created_at_ms: 0,
            metadata,
        }
    }

    #[test]
    fn projection_consumes_typed_runtime_semantics() {
        let mut session = SessionStateSnapshot::new("chat-1".to_string(), 0);
        session.messages = vec![
            message("assistant", MessageRole::Assistant, "", JsonMap::new()),
            message("tool", MessageRole::Tool, "read ok", JsonMap::new()),
        ];
        session.model_semantics.insert(
            "assistant".to_string(),
            ModelMessageSemanticsV1::Assistant {
                reasoning_content: None,
                tool_calls: vec![ModelToolCallStateV1 {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    args_json: r#"{"path":"a.md"}"#.to_string(),
                }],
            },
        );
        session.model_semantics.insert(
            "tool".to_string(),
            ModelMessageSemanticsV1::ToolResult {
                tool_call_id: "call-1".to_string(),
                tool_name: "read".to_string(),
                status: "ok".to_string(),
                result_state: "success_with_output".to_string(),
                error_kind: None,
                object_refs: vec![],
                transition_reason: None,
            },
        );
        let messages =
            project_session_messages_to_model_messages(&session, session.messages.as_slice())
                .expect("project messages");
        assert_eq!(messages[0].role, ModelMessageRoleV1::Assistant);
        assert_eq!(messages[0].tool_calls[0].name, "read");
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call-1"));
        PreparedPromptV1::new(
            None,
            messages,
            vec![],
            crate::tool::ModelToolChoice::None,
            32,
        )
        .expect("tool pairing must remain valid");
    }

    #[test]
    fn validation_rejects_unpaired_explicit_tool_call() {
        let error = PreparedPromptV1::new(
            None,
            vec![ModelMessageV1 {
                message_id: "assistant".to_string(),
                role: ModelMessageRoleV1::Assistant,
                content: String::new(),
                tool_calls: vec![ModelToolCallV1 {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    args_json: r#"{"path":"README.md"}"#.to_string(),
                }],
                tool_call_id: None,
                reasoning_content: None,
            }],
            vec![],
            crate::tool::ModelToolChoice::None,
            32,
        )
        .expect_err("missing tool result must fail");
        assert!(error.contains("prepared_prompt_tool_pairing_invalid"));
    }

    #[test]
    fn validation_rejects_duplicate_and_role_incompatible_semantics() {
        let duplicate_message = PreparedPromptV1::new(
            None,
            vec![
                ModelMessageV1 {
                    message_id: "duplicate".to_string(),
                    role: ModelMessageRoleV1::User,
                    content: "first".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ModelMessageV1 {
                    message_id: "duplicate".to_string(),
                    role: ModelMessageRoleV1::Assistant,
                    content: "second".to_string(),
                    tool_calls: vec![],
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            vec![],
            crate::tool::ModelToolChoice::None,
            32,
        )
        .expect_err("duplicate message id must fail");
        assert!(duplicate_message.contains("prepared_prompt_message_id_duplicate"));

        let incompatible_role = PreparedPromptV1::new(
            None,
            vec![ModelMessageV1 {
                message_id: "user".to_string(),
                role: ModelMessageRoleV1::User,
                content: "inspect".to_string(),
                tool_calls: vec![],
                tool_call_id: Some("call-1".to_string()),
                reasoning_content: None,
            }],
            vec![],
            crate::tool::ModelToolChoice::None,
            32,
        )
        .expect_err("user tool identity must fail");
        assert!(incompatible_role.contains("prepared_prompt_role_fields_invalid"));
    }

    #[test]
    fn validation_rejects_specific_choice_outside_projected_catalog() {
        let error = PreparedPromptV1::new(
            None,
            vec![ModelMessageV1 {
                message_id: "user".to_string(),
                role: ModelMessageRoleV1::User,
                content: "inspect".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            }],
            vec![],
            crate::tool::ModelToolChoice::Specific {
                name: "banana".to_string(),
            },
            32,
        )
        .expect_err("unprojected specific tool must fail");
        assert!(error.contains("prepared_prompt_tool_choice_invalid"));
    }
}
