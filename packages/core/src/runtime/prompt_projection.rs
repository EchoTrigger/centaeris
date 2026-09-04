use super::*;
use crate::extension::skills::{render_available_skills, SkillIndex};
use crate::model::prepared_prompt::{ModelMessageRoleV1, ModelMessageV1};
use crate::model::prompt::system::render_system_prompt;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

pub(super) fn render_system_prompt_artifacts(stage: &str) -> Result<(String, String), String> {
    let render_result = render_system_prompt()?;
    let content = render_result.content.clone();
    let manifest_json = serde_json::to_string(&json!({
        "schema": "system_prompt_manifest_v1",
        "promptSchema": render_result.schema,
        "promptHash": render_result.prompt_hash,
        "sectionCount": render_result.section_count,
        "includedSections": render_result.included_sections,
        "sectionMetadata": render_result.section_metadata,
        "stage": stage,
    }))
    .map_err(|error| format!("serialize system prompt manifest failed: {error}"))?;
    Ok((content, manifest_json))
}

pub(super) fn build_current_user_message(
    session_id: &str,
    turn_id: &str,
    user_message: &str,
) -> ChatMessage {
    ChatMessage {
        message_id: driver_user_message_id(session_id, turn_id),
        role: MessageRole::User,
        content: user_message.trim().to_string(),
        created_at_ms: now_ms(),
        metadata: JsonMap::new(),
    }
}

pub(super) fn build_execution_context_message(
    session_id: &str,
    turn_id: &str,
    cwd: &Path,
    bash_description: &str,
) -> ModelMessageV1 {
    let cwd = model_path_text(cwd);
    let content = format!(
        "<environment_context>\n  <cwd>{}</cwd>\n  <bash>{}</bash>\n</environment_context>",
        xml_escape(cwd.as_str()),
        xml_escape(bash_description),
    );
    ModelMessageV1 {
        message_id: format!("msg:{session_id}:{turn_id}:execution_context"),
        role: ModelMessageRoleV1::User,
        content,
        tool_calls: Vec::new(),
        tool_call_id: None,
        reasoning_content: None,
    }
}

pub(super) fn build_agent_instructions_message(
    session_id: &str,
    turn_id: &str,
    instructions: &str,
) -> Result<Option<ModelMessageV1>, String> {
    if instructions.is_empty() {
        return Ok(None);
    }
    if instructions != instructions.trim()
        || instructions.chars().count() > 16_000
        || instructions.nfc().ne(instructions.chars())
        || instructions
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err("agent_instructions_invalid".to_string());
    }
    Ok(Some(ModelMessageV1 {
        message_id: format!("msg:{session_id}:{turn_id}:agent_instructions"),
        role: ModelMessageRoleV1::User,
        content: format!(
            "# Instructions for this Agent\n\n<INSTRUCTIONS>\n{instructions}\n</INSTRUCTIONS>"
        ),
        tool_calls: Vec::new(),
        tool_call_id: None,
        reasoning_content: None,
    }))
}

pub(super) fn build_agents_context_message(
    session_id: &str,
    turn_id: &str,
    cwd: &Path,
    instructions: &str,
) -> ModelMessageV1 {
    let content = format!(
        "# AGENTS.md instructions for {}\n\n<INSTRUCTIONS>\n{}\n</INSTRUCTIONS>",
        model_path_text(cwd),
        instructions
    );
    ModelMessageV1 {
        message_id: format!("msg:{session_id}:{turn_id}:agents_context"),
        role: ModelMessageRoleV1::User,
        content,
        tool_calls: Vec::new(),
        tool_call_id: None,
        reasoning_content: None,
    }
}

pub(super) fn build_skill_catalog_message(
    session_id: &str,
    turn_id: &str,
    skill_index: &SkillIndex,
    max_chars: usize,
) -> Result<Option<ModelMessageV1>, String> {
    let Some(content) = render_available_skills(skill_index, max_chars)? else {
        return Ok(None);
    };
    Ok(Some(ModelMessageV1 {
        message_id: format!("msg:{session_id}:{turn_id}:skill_catalog"),
        role: ModelMessageRoleV1::User,
        content,
        tool_calls: Vec::new(),
        tool_call_id: None,
        reasoning_content: None,
    }))
}

fn model_path_text(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let without_verbatim_prefix = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        raw.strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or_else(|| raw.into_owned())
    };
    without_verbatim_prefix.replace('\\', "/")
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

pub(super) fn driver_user_message_id(session_id: &str, turn_id: &str) -> String {
    format!("msg:{session_id}:{turn_id}:driver_user")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_manifest_describes_only_the_compiled_prompt() {
        let compiled_prompt = render_system_prompt().expect("compile prompt");
        let (content, manifest_json) =
            render_system_prompt_artifacts("generate").expect("render prompt");
        assert_eq!(content, compiled_prompt.content);
        assert!(content.starts_with("# Harness\n"));
        let manifest = serde_json::from_str::<Value>(&manifest_json).expect("parse manifest");
        assert_eq!(
            manifest.get("schema").and_then(Value::as_str),
            Some("system_prompt_manifest_v1")
        );
        assert_eq!(
            manifest.get("promptSchema").and_then(Value::as_str),
            Some("system_prompt_v1")
        );
        assert_eq!(
            manifest.get("stage").and_then(Value::as_str),
            Some("generate")
        );
        assert_eq!(
            manifest.get("sectionCount").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(manifest["includedSections"], json!(["Harness"]));
        assert_eq!(
            manifest["sectionMetadata"].as_array().map(Vec::len),
            Some(1)
        );
        let keys = manifest
            .as_object()
            .expect("manifest object")
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "includedSections",
                "promptHash",
                "promptSchema",
                "schema",
                "sectionCount",
                "sectionMetadata",
                "stage",
            ])
        );
    }

    #[test]
    fn current_user_message_has_no_hidden_projection() {
        let message = build_current_user_message("chat-1", "turn-1", "inspect the library");
        assert_eq!(message.role, MessageRole::User);
        assert_eq!(message.content, "inspect the library");
        assert!(message.metadata.is_empty());
    }

    #[test]
    fn execution_context_contains_only_model_cwd_and_bash_facts() {
        let message = build_execution_context_message(
            "chat-1",
            "turn-1",
            Path::new(r"\\?\D:\Projects\A&B"),
            "bash (Git for Windows)",
        );

        assert_eq!(message.role, ModelMessageRoleV1::User);
        assert_eq!(
            message.content,
            "<environment_context>\n  <cwd>D:/Projects/A&amp;B</cwd>\n  <bash>bash (Git for Windows)</bash>\n</environment_context>"
        );
        assert!(message.tool_calls.is_empty());
        assert!(message.tool_call_id.is_none());
        assert!(message.reasoning_content.is_none());
    }

    #[test]
    fn agent_instructions_are_canonical_user_context() {
        let message = build_agent_instructions_message(
            "chat-1",
            "turn-1",
            "Be concise.\n\nPrefer primary sources.",
        )
        .expect("build Agent instructions")
        .expect("non-empty Agent instructions");
        assert_eq!(message.role, ModelMessageRoleV1::User);
        assert_eq!(
            message.content,
            "# Instructions for this Agent\n\n<INSTRUCTIONS>\nBe concise.\n\nPrefer primary sources.\n</INSTRUCTIONS>"
        );
        assert!(build_agent_instructions_message("chat-1", "turn-1", "")
            .unwrap()
            .is_none());
        assert_eq!(
            build_agent_instructions_message("chat-1", "turn-1", " trailing ")
                .expect_err("non-canonical instructions must fail"),
            "agent_instructions_invalid"
        );
    }

    #[test]
    fn agents_instructions_are_wrapped_as_user_context() {
        let message = build_agents_context_message(
            "chat-1",
            "turn-1",
            Path::new(r"D:\Projects\Centaeris"),
            "Use exact names.",
        );
        assert_eq!(message.role, ModelMessageRoleV1::User);
        assert_eq!(
            message.content,
            "# AGENTS.md instructions for D:/Projects/Centaeris\n\n<INSTRUCTIONS>\nUse exact names.\n</INSTRUCTIONS>"
        );
    }
}
