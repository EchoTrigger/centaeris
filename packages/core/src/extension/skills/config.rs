use super::catalog::{canonical_local_path, sha256_hex, validate_skill_name, validate_source_path};
use super::types::{
    SkillPolicyV1, SkillSourceAddRequest, SkillSourceConfigV1, SkillSourceScopeV1,
    SkillSourcesConfigV1, SKILL_SOURCES_CONFIG_SCHEMA_V1,
};
use std::path::{Path, PathBuf};

pub trait SkillSourceConfigStore {
    fn load(&self) -> Result<SkillSourcesConfigV1, String>;
    fn save(&self, config: &SkillSourcesConfigV1) -> Result<(), String>;
}

pub fn add_skill_source(
    config: &mut SkillSourcesConfigV1,
    request: SkillSourceAddRequest,
) -> Result<SkillSourceConfigV1, String> {
    validate_skill_sources_config(config)?;
    if !matches!(
        request.scope,
        SkillSourceScopeV1::User | SkillSourceScopeV1::Workspace
    ) {
        return Err("user-managed skill source scope must be user or workspace".to_string());
    }
    let path = validate_source_path(request.kind, request.path.as_str())?;
    let workspace_root = match request.scope {
        SkillSourceScopeV1::Workspace => {
            let raw = request
                .workspace_root
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "workspace skill source requires workspaceRoot".to_string())?;
            let root = PathBuf::from(raw);
            if !root.is_absolute() || !root.is_dir() {
                return Err(format!(
                    "workspaceRoot must be an existing absolute directory: {raw}"
                ));
            }
            Some(canonical_local_path(root.as_path())?)
        }
        SkillSourceScopeV1::User => {
            if request.workspace_root.is_some() {
                return Err("user skill source must not include workspaceRoot".to_string());
            }
            None
        }
        SkillSourceScopeV1::System | SkillSourceScopeV1::Plugin => unreachable!(),
    };
    let identity = format!(
        "{}\0{}\0{}\0{}",
        request.scope.identity_key(),
        request.kind.identity_key(),
        path,
        workspace_root.as_deref().unwrap_or_default()
    );
    let source_id = format!("source_{}", &sha256_hex(identity.as_bytes())[..24]);
    if config
        .sources
        .iter()
        .any(|source| source.source_id == source_id)
    {
        return Err(format!("skill source already configured: {source_id}"));
    }
    let source = SkillSourceConfigV1 {
        source_id,
        scope: request.scope,
        kind: request.kind,
        path,
        workspace_root,
        enabled: true,
    };
    config.sources.push(source.clone());
    Ok(source)
}

pub fn remove_skill_source(
    config: &mut SkillSourcesConfigV1,
    source_id: &str,
) -> Result<(), String> {
    validate_skill_sources_config(config)?;
    let normalized = required_source_id(source_id)?;
    let source = config
        .sources
        .iter()
        .find(|source| source.source_id == normalized)
        .ok_or_else(|| format!("skill source not found: {normalized}"))?;
    if !matches!(
        source.scope,
        SkillSourceScopeV1::Workspace | SkillSourceScopeV1::User
    ) {
        return Err(format!(
            "skill source is owner-managed and cannot be removed by the user API: sourceId={normalized} scope={:?}",
            source.scope
        ));
    }
    config
        .sources
        .retain(|source| source.source_id != normalized);
    config
        .skill_policies
        .retain(|policy| policy.source_id != normalized);
    Ok(())
}

pub fn set_skill_source_enabled(
    config: &mut SkillSourcesConfigV1,
    source_id: &str,
    enabled: bool,
) -> Result<(), String> {
    validate_skill_sources_config(config)?;
    let normalized = required_source_id(source_id)?;
    let source = config
        .sources
        .iter_mut()
        .find(|source| source.source_id == normalized)
        .ok_or_else(|| format!("skill source not found: {normalized}"))?;
    source.enabled = enabled;
    Ok(())
}

pub fn set_skill_enabled(
    config: &mut SkillSourcesConfigV1,
    source_id: &str,
    skill_name: &str,
    enabled: bool,
) -> Result<(), String> {
    validate_skill_sources_config(config)?;
    let source_id = required_source_id(source_id)?;
    let skill_name = skill_name.trim();
    if skill_name.is_empty() {
        return Err("skillName is required".to_string());
    }
    if !config
        .sources
        .iter()
        .any(|source| source.source_id == source_id)
    {
        return Err(format!("skill source not found: {source_id}"));
    }
    if enabled {
        config.skill_policies.retain(|policy| {
            !(policy.source_id == source_id && policy.skill_name.eq_ignore_ascii_case(skill_name))
        });
        return Ok(());
    }
    if let Some(policy) = config.skill_policies.iter_mut().find(|policy| {
        policy.source_id == source_id && policy.skill_name.eq_ignore_ascii_case(skill_name)
    }) {
        policy.enabled = false;
    } else {
        config.skill_policies.push(SkillPolicyV1 {
            source_id: source_id.to_string(),
            skill_name: skill_name.to_string(),
            enabled: false,
        });
    }
    config.skill_policies.sort_by(|left, right| {
        left.source_id
            .cmp(&right.source_id)
            .then_with(|| left.skill_name.cmp(&right.skill_name))
    });
    Ok(())
}

pub fn validate_skill_sources_config(config: &SkillSourcesConfigV1) -> Result<(), String> {
    if config.schema_version != SKILL_SOURCES_CONFIG_SCHEMA_V1 {
        return Err(format!(
            "unsupported skill source schemaVersion: expected {}, got {}",
            SKILL_SOURCES_CONFIG_SCHEMA_V1, config.schema_version
        ));
    }
    let mut source_ids = std::collections::HashSet::new();
    for source in &config.sources {
        let source_id = required_source_id(source.source_id.as_str())?;
        if !source_ids.insert(source_id.to_string()) {
            return Err(format!("duplicate skill sourceId: {source_id}"));
        }
        if !Path::new(source.path.as_str()).is_absolute() {
            return Err(format!(
                "skill source path must be absolute: sourceId={} path={}",
                source.source_id, source.path
            ));
        }
        match source.scope {
            SkillSourceScopeV1::Workspace => {
                let root = source
                    .workspace_root
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        format!(
                            "workspace skill source requires workspaceRoot: sourceId={}",
                            source.source_id
                        )
                    })?;
                if !Path::new(root).is_absolute() {
                    return Err(format!(
                        "workspaceRoot must be absolute: sourceId={} workspaceRoot={root}",
                        source.source_id
                    ));
                }
            }
            SkillSourceScopeV1::User | SkillSourceScopeV1::System | SkillSourceScopeV1::Plugin => {
                if source.workspace_root.is_some() {
                    return Err(format!(
                        "non-workspace skill source must not include workspaceRoot: sourceId={}",
                        source.source_id
                    ));
                }
            }
        }
    }
    let mut policy_keys = std::collections::HashSet::new();
    for policy in &config.skill_policies {
        let source_id = required_source_id(policy.source_id.as_str())?;
        if !source_ids.contains(source_id) {
            return Err(format!(
                "skill policy references unknown sourceId: {source_id}"
            ));
        }
        let skill_name = policy.skill_name.trim();
        validate_skill_name(skill_name)?;
        let policy_key = (source_id.to_string(), skill_name.to_ascii_lowercase());
        if !policy_keys.insert(policy_key) {
            return Err(format!(
                "duplicate skill policy: sourceId={source_id} skillName={skill_name}"
            ));
        }
    }
    Ok(())
}

fn required_source_id(source_id: &str) -> Result<&str, String> {
    let normalized = source_id.trim();
    if normalized.is_empty() {
        return Err("sourceId is required".to_string());
    }
    if !normalized
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
    {
        return Err(format!("invalid sourceId: {normalized}"));
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::skills::SkillSourceKindV1;
    use std::fs;

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "centaeris-skill-source-config-{label}-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(root.as_path()).expect("create temp root");
        root
    }

    #[test]
    fn policy_for_unknown_source_loud_fails() {
        let config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: Vec::new(),
            skill_policies: vec![SkillPolicyV1 {
                source_id: "banana".to_string(),
                skill_name: "banana-skill".to_string(),
                enabled: false,
            }],
        };
        let error =
            validate_skill_sources_config(&config).expect_err("unknown policy source must fail");
        assert!(error.contains("skill policy references unknown sourceId: banana"));
    }

    #[test]
    fn add_source_records_only_the_selected_absolute_location() {
        let root = temp_root("add");
        let catalog = root.join("catalog");
        fs::create_dir_all(catalog.as_path()).expect("create catalog");
        let mut config = SkillSourcesConfigV1::default();
        let source = add_skill_source(
            &mut config,
            SkillSourceAddRequest {
                scope: SkillSourceScopeV1::User,
                kind: SkillSourceKindV1::CatalogDirectory,
                path: catalog.to_string_lossy().to_string(),
                workspace_root: None,
            },
        )
        .expect("add source");
        assert_eq!(config.sources, vec![source.clone()]);
        assert_eq!(
            config.sources[0].path,
            canonical_local_path(catalog.as_path()).unwrap()
        );

        let mut second_config = SkillSourcesConfigV1::default();
        let second_source = add_skill_source(
            &mut second_config,
            SkillSourceAddRequest {
                scope: SkillSourceScopeV1::User,
                kind: SkillSourceKindV1::CatalogDirectory,
                path: catalog.to_string_lossy().to_string(),
                workspace_root: None,
            },
        )
        .expect("add same source to a separate config");
        assert_eq!(source.source_id, second_source.source_id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn user_api_cannot_remove_owner_managed_source() {
        let root = temp_root("owner-managed");
        let catalog = root.join("catalog");
        fs::create_dir_all(catalog.as_path()).expect("create catalog");
        let mut config = SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: vec![SkillSourceConfigV1 {
                source_id: "source-plugin".to_string(),
                scope: SkillSourceScopeV1::Plugin,
                kind: SkillSourceKindV1::CatalogDirectory,
                path: canonical_local_path(catalog.as_path()).expect("canonical catalog"),
                workspace_root: None,
                enabled: true,
            }],
            skill_policies: Vec::new(),
        };

        let error = remove_skill_source(&mut config, "source-plugin")
            .expect_err("plugin source must remain owner-managed");

        assert!(error.contains("owner-managed"));
        assert_eq!(config.sources.len(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
