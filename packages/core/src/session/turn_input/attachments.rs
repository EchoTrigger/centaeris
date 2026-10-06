use crate::model::prepared_prompt::ModelInputImageRefV1;
use crate::session::state::ChatMessage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInputAttachment {
    pub input_ref: String,
    pub display_name: String,
    pub content_type: String,
}

pub fn validate_user_input(
    message: &str,
    attachments: &[UserInputAttachment],
) -> Result<(), String> {
    if attachments.len() > 50
        || message.len() > super::super::supplement::MAX_TURN_SUPPLEMENT_BYTES
        || message.contains('\0')
        || (message.trim().is_empty() && attachments.is_empty())
    {
        return Err("user_input_content_invalid".into());
    }
    let mut refs = std::collections::HashSet::new();
    for attachment in attachments {
        if attachment.input_ref.trim().is_empty()
            || attachment.display_name.trim().is_empty()
            || attachment.content_type.trim().is_empty()
            || !refs.insert(&attachment.input_ref)
            || (attachment.content_type.starts_with("image/")
                && !matches!(
                    attachment.content_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp"
                ))
        {
            return Err("user_input_attachment_invalid".into());
        }
    }
    Ok(())
}

pub fn apply_user_input_attachments(
    message: &mut ChatMessage,
    attachments: &[UserInputAttachment],
) {
    if attachments.is_empty() {
        return;
    }
    let mut images = Vec::new();
    message
        .content
        .push_str("\n\nAttached files for this input:\n");
    for attachment in attachments {
        if attachment.content_type.starts_with("image/") {
            let mut placeholder = format!("[image:{}]", attachment.input_ref);
            while message.content.contains(&placeholder) {
                placeholder.push('_');
            }
            message
                .content
                .push_str(&format!("- {} {}\n", attachment.display_name, placeholder));
            images.push(ModelInputImageRefV1 {
                input_ref: attachment.input_ref.clone(),
                content_type: attachment.content_type.clone(),
                placeholder,
            });
        } else {
            message.content.push_str(&format!(
                "- {} (inputRef: {}; contentType: {}).\n",
                attachment.display_name, attachment.input_ref, attachment.content_type
            ));
        }
    }
    if !images.is_empty() {
        message.metadata.insert(
            crate::runtime::keys::metadata::MODEL_INPUT_IMAGES.into(),
            serde_json::to_string(&images).expect("typed image references serialize"),
        );
    }
}
