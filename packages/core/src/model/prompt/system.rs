use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SYSTEM_PROMPT: &str = include_str!("prompt.md");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptSectionStability {
    Stable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptSectionPlacement {
    SystemRoot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemPromptSectionMetadata {
    pub name: String,
    pub stability: PromptSectionStability,
    pub placement: PromptSectionPlacement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemPromptRenderResult {
    pub schema: String,
    #[serde(rename = "promptHash")]
    pub prompt_hash: String,
    #[serde(rename = "sectionCount")]
    pub section_count: usize,
    #[serde(rename = "includedSections")]
    pub included_sections: Vec<String>,
    #[serde(rename = "sectionMetadata")]
    pub section_metadata: Vec<SystemPromptSectionMetadata>,
    pub content: String,
}

pub fn render_system_prompt() -> Result<SystemPromptRenderResult, String> {
    let content = SYSTEM_PROMPT.trim().to_string();
    let included_sections = parse_section_names(content.as_str())?;
    validate_harness_sections(&included_sections)?;
    let section_metadata = included_sections
        .iter()
        .map(|name| SystemPromptSectionMetadata {
            name: name.clone(),
            stability: PromptSectionStability::Stable,
            placement: PromptSectionPlacement::SystemRoot,
        })
        .collect::<Vec<_>>();
    Ok(SystemPromptRenderResult {
        schema: "system_prompt_v1".to_string(),
        prompt_hash: hash_prompt(content.as_str()),
        section_count: included_sections.len(),
        included_sections,
        section_metadata,
        content,
    })
}

fn validate_harness_sections(included_sections: &[String]) -> Result<(), String> {
    if included_sections == ["Harness"] {
        return Ok(());
    }
    Err(
        "compiled system prompt must contain exactly one top-level section named Harness"
            .to_string(),
    )
}

fn parse_section_names(content: &str) -> Result<Vec<String>, String> {
    if content.is_empty() {
        return Err("compiled system prompt is empty".to_string());
    }
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current_section = None;
    let mut has_content = false;
    for line in content.lines() {
        if let Some(name) = line.strip_prefix("# ") {
            if let Some(previous) = current_section.take() {
                if !has_content {
                    return Err(format!("system prompt section is empty: {previous}"));
                }
            }
            let name = name.trim();
            if name.is_empty() {
                return Err("system prompt section name is empty".to_string());
            }
            if !seen.insert(name.to_string()) {
                return Err(format!("system prompt section is duplicated: {name}"));
            }
            names.push(name.to_string());
            current_section = Some(name.to_string());
            has_content = false;
        } else if current_section.is_some() && !line.trim().is_empty() {
            has_content = true;
        }
    }
    let Some(previous) = current_section else {
        return Err("system prompt has no top-level sections".to_string());
    };
    if !has_content {
        return Err(format!("system prompt section is empty: {previous}"));
    }
    Ok(names)
}

fn hash_prompt(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"system_prompt_v1:");
    hasher.update(content.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(format!("{byte:02x}").as_str());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_prompt_is_valid() {
        let prompt = render_system_prompt().expect("render system prompt");
        assert_eq!(prompt.schema, "system_prompt_v1");
        assert_eq!(prompt.section_count, 1);
        assert_eq!(prompt.included_sections, vec!["Harness"]);
        assert_eq!(prompt.section_metadata.len(), 1);
        assert_eq!(prompt.section_metadata[0].name, "Harness");
        assert!(prompt
            .content
            .contains("Use only the tools supplied for the current turn."));
        assert!(prompt
            .content
            .contains("Inspect only the context needed to understand the task"));
        assert!(prompt
            .content
            .contains("make the smallest complete change that satisfies the request"));
        assert!(prompt
            .content
            .contains("diagnose it from the actual tool result instead of blindly repeating"));
        assert!(prompt.content.contains(
            "When changing declared project dependencies, use the project's existing package manager"
        ));
        assert!(prompt
            .content
            .contains("keep manifests and lockfiles consistent"));
        assert!(prompt.content.contains(
            "In the final response, state what changed, what was verified, and any remaining uncertainty"
        ));
        assert!(prompt.content.contains(
            "Use the language of the user's current request for every user-visible progress update and the final response"
        ));
        assert!(prompt.content.contains(
            "When a response relies on external tools and those tools provide reliable source links, include the relevant links"
        ));
        assert!(prompt.content.contains(
            "Never invent a source link, retry solely to obtain a missing link, or delay or fail an otherwise complete response solely because a source link is unavailable"
        ));
        assert!(prompt
            .content
            .contains("Never claim that a check was run when it was not."));
        assert!(!prompt.prompt_hash.is_empty());
    }

    #[test]
    fn invalid_prompt_sections_fail_loudly() {
        assert!(parse_section_names("# One\n\n# One\nbody").is_err());
        assert!(parse_section_names("# One\n").is_err());
        assert!(parse_section_names("body").is_err());
        assert!(validate_harness_sections(&["Identity".to_string()]).is_err());
        assert!(validate_harness_sections(&["Harness".to_string(), "Tools".to_string()]).is_err());
    }
}
