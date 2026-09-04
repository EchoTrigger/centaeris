use crate::atomic_file::write_file_atomically;
use crate::runtime_config::{validate_persisted_model_state, PersistedRuntimeConfigState};
use crate::user_data_layout;
use centaeris_core::extension::skills::{
    validate_skill_sources_config, SkillPolicyV1, SkillSourceConfigV1, SkillSourceKindV1,
    SkillSourceScopeV1, SkillSourcesConfigV1,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

const USER_CONFIG_SCHEMA_VERSION: u32 = 1;
const USER_CONFIG_UNSUPPORTED: &str = "user_config_unsupported";
const USER_CONFIG_IO: &str = "user_config_io";
static USER_CONFIG_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginConfigSection {
    pub(crate) disabled: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct UserConfigDocument {
    pub(crate) schema_version: u32,
    pub(crate) runtime: PersistedRuntimeConfigState,
    pub(crate) plugins: PluginConfigSection,
    pub(crate) skills: SkillSourcesConfigV1,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredUserConfigDocument {
    schema_version: u32,
    runtime: PersistedRuntimeConfigState,
    plugins: PluginConfigSection,
    skills: StoredSkillSourcesConfig,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSkillSourcesConfig {
    schema_version: String,
    sources: Vec<StoredSkillSourceConfig>,
    skill_policies: Vec<StoredSkillPolicy>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSkillSourceConfig {
    source_id: String,
    scope: StoredSkillSourceScope,
    kind: StoredSkillSourceKind,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_root: Option<String>,
    enabled: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSkillPolicy {
    source_id: String,
    skill_name: String,
    enabled: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredSkillSourceScope {
    Workspace,
    User,
    System,
    Plugin,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredSkillSourceKind {
    CatalogDirectory,
    SkillFile,
}

impl Default for UserConfigDocument {
    fn default() -> Self {
        Self {
            schema_version: USER_CONFIG_SCHEMA_VERSION,
            runtime: PersistedRuntimeConfigState::default(),
            plugins: PluginConfigSection::default(),
            skills: SkillSourcesConfigV1::default(),
        }
    }
}

pub(crate) fn ensure_at(path: &Path) -> Result<(), String> {
    let _guard = lock()?;
    if path.exists() {
        load_at(path).map(|_| ())
    } else {
        persist_at(path, &UserConfigDocument::default())
    }
}

pub(crate) fn load() -> Result<UserConfigDocument, String> {
    let _guard = lock()?;
    load_at(user_data_layout::user_config_file_path().as_path())
}

pub(crate) fn update<TResult>(
    update: impl FnOnce(&mut UserConfigDocument) -> Result<TResult, String>,
) -> Result<TResult, String> {
    let _guard = lock()?;
    let path = user_data_layout::user_config_file_path();
    let mut config = load_at(path.as_path())?;
    let result = update(&mut config)?;
    persist_at(path.as_path(), &config)?;
    Ok(result)
}

pub(crate) fn load_at(path: &Path) -> Result<UserConfigDocument, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UserConfigDocument::default());
        }
        Err(error) => {
            return Err(format!(
                "{USER_CONFIG_IO}: read user config failed for {}: {error}",
                path.display()
            ));
        }
    };
    let envelope = toml::from_str::<toml::Value>(raw.as_str()).map_err(|error| {
        format!(
            "{USER_CONFIG_UNSUPPORTED}: parse user config failed for {}: {error}",
            path.display()
        )
    })?;
    let schema_version = envelope
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| {
            format!("{USER_CONFIG_UNSUPPORTED}: user config schema_version must be an integer")
        })?;
    match schema_version {
        value if value == i64::from(USER_CONFIG_SCHEMA_VERSION) => {}
        value if value < i64::from(USER_CONFIG_SCHEMA_VERSION) => {
            return Err(format!(
                "{USER_CONFIG_UNSUPPORTED}: no forward migration exists from schema_version {value} to {USER_CONFIG_SCHEMA_VERSION}"
            ));
        }
        value => {
            return Err(format!(
                "{USER_CONFIG_UNSUPPORTED}: refusing config downgrade from schema_version {value} to {USER_CONFIG_SCHEMA_VERSION}"
            ));
        }
    }
    let stored = toml::from_str::<StoredUserConfigDocument>(raw.as_str()).map_err(|error| {
        format!(
            "{USER_CONFIG_UNSUPPORTED}: parse user config v{USER_CONFIG_SCHEMA_VERSION} failed for {}: {error}",
            path.display()
        )
    })?;
    let config = UserConfigDocument::from(stored);
    validate(&config)?;
    Ok(config)
}

pub(crate) fn persist_at(path: &Path, config: &UserConfigDocument) -> Result<(), String> {
    validate(config)?;
    let encoded =
        toml::to_string_pretty(&StoredUserConfigDocument::from(config)).map_err(|error| {
            format!("{USER_CONFIG_UNSUPPORTED}: serialize user config failed: {error}")
        })?;
    write_file_atomically(path, format!("{encoded}\n").as_bytes(), "user config")
        .map_err(|error| format!("{USER_CONFIG_IO}: {error}"))
}

fn validate(config: &UserConfigDocument) -> Result<(), String> {
    if config.schema_version != USER_CONFIG_SCHEMA_VERSION {
        return Err(format!(
            "{USER_CONFIG_UNSUPPORTED}: unsupported schema_version: expected {USER_CONFIG_SCHEMA_VERSION}, got {}",
            config.schema_version
        ));
    }
    validate_persisted_model_state(&config.runtime)
        .map_err(|error| format!("{USER_CONFIG_UNSUPPORTED}: invalid runtime config: {error}"))?;
    validate_plugin_config(&config.plugins)?;
    validate_skill_sources_config(&config.skills)
        .map_err(|error| format!("{USER_CONFIG_UNSUPPORTED}: invalid skill config: {error}"))
}

fn validate_plugin_config(config: &PluginConfigSection) -> Result<(), String> {
    let mut ids = HashSet::new();
    for id in &config.disabled {
        let normalized = id.trim();
        if normalized.is_empty() || normalized != id {
            return Err(format!(
                "{USER_CONFIG_UNSUPPORTED}: invalid disabled plugin id: {id:?}"
            ));
        }
        if !ids.insert(normalized) {
            return Err(format!(
                "{USER_CONFIG_UNSUPPORTED}: duplicate disabled plugin id: {normalized}"
            ));
        }
    }
    Ok(())
}

fn lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    USER_CONFIG_LOCK
        .lock()
        .map_err(|_| "user config lock poisoned".to_string())
}

impl From<StoredUserConfigDocument> for UserConfigDocument {
    fn from(value: StoredUserConfigDocument) -> Self {
        Self {
            schema_version: value.schema_version,
            runtime: value.runtime,
            plugins: value.plugins,
            skills: value.skills.into(),
        }
    }
}

impl From<&UserConfigDocument> for StoredUserConfigDocument {
    fn from(value: &UserConfigDocument) -> Self {
        Self {
            schema_version: value.schema_version,
            runtime: value.runtime.clone(),
            plugins: value.plugins.clone(),
            skills: (&value.skills).into(),
        }
    }
}

impl From<StoredSkillSourcesConfig> for SkillSourcesConfigV1 {
    fn from(value: StoredSkillSourcesConfig) -> Self {
        Self {
            schema_version: value.schema_version,
            sources: value.sources.into_iter().map(Into::into).collect(),
            skill_policies: value.skill_policies.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&SkillSourcesConfigV1> for StoredSkillSourcesConfig {
    fn from(value: &SkillSourcesConfigV1) -> Self {
        Self {
            schema_version: value.schema_version.clone(),
            sources: value.sources.iter().map(Into::into).collect(),
            skill_policies: value.skill_policies.iter().map(Into::into).collect(),
        }
    }
}

impl From<StoredSkillSourceConfig> for SkillSourceConfigV1 {
    fn from(value: StoredSkillSourceConfig) -> Self {
        Self {
            source_id: value.source_id,
            scope: value.scope.into(),
            kind: value.kind.into(),
            path: value.path,
            workspace_root: value.workspace_root,
            enabled: value.enabled,
        }
    }
}

impl From<&SkillSourceConfigV1> for StoredSkillSourceConfig {
    fn from(value: &SkillSourceConfigV1) -> Self {
        Self {
            source_id: value.source_id.clone(),
            scope: value.scope.into(),
            kind: value.kind.into(),
            path: value.path.clone(),
            workspace_root: value.workspace_root.clone(),
            enabled: value.enabled,
        }
    }
}

impl From<StoredSkillPolicy> for SkillPolicyV1 {
    fn from(value: StoredSkillPolicy) -> Self {
        Self {
            source_id: value.source_id,
            skill_name: value.skill_name,
            enabled: value.enabled,
        }
    }
}

impl From<&SkillPolicyV1> for StoredSkillPolicy {
    fn from(value: &SkillPolicyV1) -> Self {
        Self {
            source_id: value.source_id.clone(),
            skill_name: value.skill_name.clone(),
            enabled: value.enabled,
        }
    }
}

impl From<StoredSkillSourceScope> for SkillSourceScopeV1 {
    fn from(value: StoredSkillSourceScope) -> Self {
        match value {
            StoredSkillSourceScope::Workspace => Self::Workspace,
            StoredSkillSourceScope::User => Self::User,
            StoredSkillSourceScope::System => Self::System,
            StoredSkillSourceScope::Plugin => Self::Plugin,
        }
    }
}

impl From<SkillSourceScopeV1> for StoredSkillSourceScope {
    fn from(value: SkillSourceScopeV1) -> Self {
        match value {
            SkillSourceScopeV1::Workspace => Self::Workspace,
            SkillSourceScopeV1::User => Self::User,
            SkillSourceScopeV1::System => Self::System,
            SkillSourceScopeV1::Plugin => Self::Plugin,
        }
    }
}

impl From<StoredSkillSourceKind> for SkillSourceKindV1 {
    fn from(value: StoredSkillSourceKind) -> Self {
        match value {
            StoredSkillSourceKind::CatalogDirectory => Self::CatalogDirectory,
            StoredSkillSourceKind::SkillFile => Self::SkillFile,
        }
    }
}

impl From<SkillSourceKindV1> for StoredSkillSourceKind {
    fn from(value: SkillSourceKindV1) -> Self {
        match value {
            SkillSourceKindV1::CatalogDirectory => Self::CatalogDirectory,
            SkillSourceKindV1::SkillFile => Self::SkillFile,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn round_trip_preserves_all_sections() {
        let root = temp_root("round-trip");
        let path = root.join("config.toml");
        let mut config = UserConfigDocument::default();
        config.plugins.disabled.push("plugin-a".to_string());
        persist_at(path.as_path(), &config).expect("persist config");

        let restored = load_at(path.as_path()).expect("load config");
        assert_eq!(restored.plugins.disabled, vec!["plugin-a"]);
        assert_eq!(restored.skills, SkillSourcesConfigV1::default());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_and_duplicate_values_fail_loudly() {
        let root = temp_root("invalid");
        let path = root.join("config.toml");
        let valid = toml::to_string_pretty(&StoredUserConfigDocument::from(
            &UserConfigDocument::default(),
        ))
        .expect("encode");
        fs::write(path.as_path(), format!("{valid}\nbanana = true\n"))
            .expect("write unknown field");
        assert!(load_at(path.as_path())
            .expect_err("unknown field")
            .starts_with(USER_CONFIG_UNSUPPORTED));

        fs::write(path.as_path(), "schema_version = 1\n").expect("write incomplete config");
        assert!(load_at(path.as_path())
            .expect_err("missing sections")
            .starts_with(USER_CONFIG_UNSUPPORTED));

        let mut duplicate = UserConfigDocument::default();
        duplicate.plugins.disabled = vec!["plugin-a".to_string(), "plugin-a".to_string()];
        assert!(persist_at(path.as_path(), &duplicate)
            .expect_err("duplicate plugin")
            .contains("duplicate disabled plugin id"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn schema_dispatch_rejects_unimplemented_upgrade_and_downgrade() {
        let root = temp_root("schema-dispatch");
        let path = root.join("config.toml");
        fs::write(path.as_path(), "schema_version = 0\n").expect("write old config");
        assert!(load_at(path.as_path())
            .expect_err("old config")
            .contains("no forward migration exists"));
        fs::write(path.as_path(), "schema_version = 2\n").expect("write future config");
        assert!(load_at(path.as_path())
            .expect_err("future config")
            .contains("refusing config downgrade"));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "centaeris-user-config-{label}-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(root.as_path()).expect("create temp root");
        root
    }
}
