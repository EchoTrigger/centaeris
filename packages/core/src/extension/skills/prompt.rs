use super::catalog::SkillIndex;

pub fn render_available_skills(
    index: &SkillIndex,
    max_chars: usize,
) -> Result<Option<String>, String> {
    if max_chars == 0 {
        return Err("skill catalog prompt budget must be greater than zero".to_string());
    }
    let items = index.catalog_items();
    if items.is_empty() {
        return Ok(None);
    }
    let mut output = String::from(
        "<available_skills>\n  <instructions>Use a skill when its description matches the task. Read its SKILL.md before following it. If read returns a continuation, continue until the complete file is loaded. Resolve relative paths from the directory containing SKILL.md.</instructions>\n",
    );
    for item in items {
        output.push_str("  <skill>\n");
        output.push_str(format!("    <name>{}</name>\n", xml_escape(item.name.as_str())).as_str());
        output.push_str(
            format!(
                "    <description>{}</description>\n",
                xml_escape(item.description.as_str())
            )
            .as_str(),
        );
        output.push_str(
            format!(
                "    <location>{}</location>\n",
                xml_escape(item.location.as_str())
            )
            .as_str(),
        );
        output.push_str("  </skill>\n");
    }
    output.push_str("</available_skills>");
    let actual_chars = output.chars().count();
    if actual_chars > max_chars {
        return Err(format!(
            "skill_catalog_prompt_budget_exceeded: actualChars={actual_chars} maxChars={max_chars}"
        ));
    }
    Ok(Some(output))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::skills::{
        SkillCatalogLoadConfig, SkillSourceConfigV1, SkillSourceKindV1, SkillSourceScopeV1,
        SkillSourcesConfigV1, SKILL_SOURCES_CONFIG_SCHEMA_V1,
    };
    use std::fs;

    #[test]
    fn prompt_contains_only_metadata_and_location() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-skill-prompt-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        let skill_dir = root.join("prompt-skill");
        fs::create_dir_all(skill_dir.as_path()).expect("skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: prompt-skill\ndescription: Use this prompt skill.\n---\nSECRET BODY\n",
        )
        .expect("skill file");
        let source_path = root
            .canonicalize()
            .expect("canonical root")
            .to_string_lossy()
            .replace('\\', "/");
        let index = SkillIndex::load(SkillCatalogLoadConfig {
            cwd: None,
            sources_config: SkillSourcesConfigV1 {
                schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
                sources: vec![SkillSourceConfigV1 {
                    source_id: "source-user".to_string(),
                    scope: SkillSourceScopeV1::User,
                    kind: SkillSourceKindV1::CatalogDirectory,
                    path: source_path,
                    workspace_root: None,
                    enabled: true,
                }],
                skill_policies: Vec::new(),
            },
            max_skills: 16,
        })
        .expect("skill index");
        let rendered = render_available_skills(&index, 8_000)
            .expect("render")
            .expect("catalog");
        assert!(rendered.contains("<name>prompt-skill</name>"));
        assert!(rendered.contains("<location>"));
        assert!(!rendered.contains("SECRET BODY"));
        assert!(!rendered.contains("allowed-tools"));
        let _ = fs::remove_dir_all(root);
    }
}
