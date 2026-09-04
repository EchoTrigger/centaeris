use crate::extension::hooks::{
    LifecycleHookEventNameV1, LifecycleHookHandlerV1, LifecycleHookSourceKindV1,
    LifecycleHookSourceV1,
};
use crate::tool::limits::read_tool_contract_file;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const PLUGIN_HOOKS_SCHEMA_V1: &str = "plugin_hooks_v1";
const MAX_PLUGIN_HOOK_HANDLERS: usize = 64;
const MAX_PLUGIN_HOOK_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestV1 {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub paths: PluginManifestPathsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<PluginInterfaceV1>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifestPathsV1 {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub cli: Vec<String>,
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default)]
    pub hooks: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInterfaceV1 {
    #[serde(
        default,
        rename = "displayName",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
    #[serde(
        default,
        rename = "shortDescription",
        skip_serializing_if = "Option::is_none"
    )]
    pub short_description: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHookDeclarationV1 {
    #[serde(rename = "pluginName")]
    pub plugin_name: String,
    #[serde(rename = "hookPath")]
    pub hook_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHooksFileV1 {
    pub schema: String,
    pub handlers: Vec<PluginHookHandlerConfigV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginHookHandlerConfigV1 {
    pub id: String,
    pub event: LifecycleHookEventNameV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout_ms", rename = "timeoutMs")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginTrustPolicyV1 {
    #[serde(default, rename = "trustedPlugins")]
    pub trusted_plugins: Vec<String>,
}

impl PluginTrustPolicyV1 {
    pub fn is_plugin_trusted(&self, plugin_name: &str) -> bool {
        self.trusted_plugins
            .iter()
            .any(|trusted| trusted == plugin_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginRegistryV1 {
    pub plugins: Vec<PluginManifestV1>,
    #[serde(rename = "hookHandlers")]
    pub hook_handlers: Vec<LifecycleHookHandlerV1>,
}

impl PluginManifestV1 {
    pub fn validate(&self) -> Result<(), String> {
        validate_plugin_name(self.name.as_str())?;
        validate_plugin_version(self.version.as_str())?;
        validate_paths("skills", &self.paths.skills)?;
        validate_paths("cli", &self.paths.cli)?;
        validate_paths("mcpServers", &self.paths.mcp_servers)?;
        validate_paths("apps", &self.paths.apps)?;
        validate_paths("hooks", &self.paths.hooks)?;
        Ok(())
    }

    pub fn hook_declarations(&self) -> Result<Vec<PluginHookDeclarationV1>, String> {
        self.validate()?;
        Ok(self
            .paths
            .hooks
            .iter()
            .map(|hook_path| PluginHookDeclarationV1 {
                plugin_name: self.name.clone(),
                hook_path: hook_path.clone(),
            })
            .collect())
    }
}

pub fn load_plugin_manifest_file(path: &Path) -> Result<PluginManifestV1, String> {
    let raw = read_tool_contract_file(path)
        .map_err(|error| format!("read plugin manifest failed {}: {error}", path.display()))?;
    let manifest: PluginManifestV1 = serde_json::from_str(raw.as_str())
        .map_err(|error| format!("parse plugin manifest failed {}: {error}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn load_plugin_registry_from_manifests(
    manifest_paths: &[PathBuf],
    trust_policy: &PluginTrustPolicyV1,
) -> Result<PluginRegistryV1, String> {
    let mut plugins = Vec::new();
    let mut hook_handlers = Vec::new();
    let mut seen_plugin_names = HashSet::new();
    let mut manifests = manifest_paths
        .iter()
        .map(|path| Ok((path, load_plugin_manifest_file(path)?)))
        .collect::<Result<Vec<_>, String>>()?;
    manifests.sort_by(|left, right| left.1.name.cmp(&right.1.name));
    for (manifest_path, manifest) in manifests {
        if !seen_plugin_names.insert(manifest.name.clone()) {
            return Err(format!("duplicate plugin name: {}", manifest.name));
        }
        let manifest_directory = manifest_path
            .parent()
            .ok_or_else(|| format!("plugin manifest has no parent: {}", manifest_path.display()))?;
        if manifest_directory
            .file_name()
            .and_then(|name| name.to_str())
            != Some(".centaeris-plugin")
        {
            return Err(format!(
                "plugin manifest must be .centaeris-plugin/plugin.json: {}",
                manifest_path.display()
            ));
        }
        let plugin_root = manifest_directory.parent().ok_or_else(|| {
            format!(
                "plugin manifest has no package root: {}",
                manifest_path.display()
            )
        })?;
        let trusted = trust_policy.is_plugin_trusted(manifest.name.as_str());
        let mut hook_paths = manifest.paths.hooks.clone();
        hook_paths.sort();
        let mut seen_hook_ids = HashSet::new();
        for hook_path in &hook_paths {
            let hook_file_path = resolve_plugin_resource_path(plugin_root, hook_path)?;
            let mut hooks_file = load_plugin_hooks_file(hook_file_path.as_path())?;
            hooks_file
                .handlers
                .sort_by(|left, right| left.id.cmp(&right.id));
            for hook in hooks_file.handlers {
                if !seen_hook_ids.insert(hook.id.clone()) {
                    return Err(format!(
                        "duplicate plugin hook handler id: {}:{}",
                        manifest.name, hook.id
                    ));
                }
                hook_handlers.push(hook.into_lifecycle_handler(
                    manifest.name.as_str(),
                    plugin_root,
                    trusted,
                )?);
            }
        }
        plugins.push(manifest);
    }
    Ok(PluginRegistryV1 {
        plugins,
        hook_handlers,
    })
}

impl PluginHookHandlerConfigV1 {
    fn into_lifecycle_handler(
        self,
        plugin_name: &str,
        plugin_root: &Path,
        trusted: bool,
    ) -> Result<LifecycleHookHandlerV1, String> {
        self.validate(plugin_name)?;
        let cwd = plugin_root
            .canonicalize()
            .map_err(|error| format!("canonicalize plugin root failed: {error}"))?;
        Ok(LifecycleHookHandlerV1 {
            id: format!("{plugin_name}:{}", self.id),
            event: self.event,
            matcher: self.matcher,
            source: LifecycleHookSourceV1 {
                kind: LifecycleHookSourceKindV1::Plugin,
                name: plugin_name.to_string(),
            },
            trusted,
            program: self.program,
            args: self.args,
            cwd: Some(cwd.to_string_lossy().to_string()),
            timeout_ms: self.timeout_ms,
        })
    }

    fn validate(&self, plugin_name: &str) -> Result<(), String> {
        if self.id.trim() != self.id || self.id.is_empty() || self.id.len() > 128 {
            return Err(format!("plugin {plugin_name} hook id is invalid"));
        }
        if self.program.trim() != self.program || self.program.is_empty() {
            return Err(format!(
                "plugin {plugin_name} hook {} program is invalid",
                self.id
            ));
        }
        if !(1..=MAX_PLUGIN_HOOK_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(format!(
                "plugin {plugin_name} hook {} timeoutMs must be 1..={MAX_PLUGIN_HOOK_TIMEOUT_MS}",
                self.id
            ));
        }
        let tool_event = matches!(
            self.event,
            LifecycleHookEventNameV1::PreToolUse
                | LifecycleHookEventNameV1::PermissionRequest
                | LifecycleHookEventNameV1::PostToolUse
        );
        if !matches!(
            self.event,
            LifecycleHookEventNameV1::UserPromptSubmit
                | LifecycleHookEventNameV1::PreToolUse
                | LifecycleHookEventNameV1::PermissionRequest
                | LifecycleHookEventNameV1::PostToolUse
                | LifecycleHookEventNameV1::PreCompact
                | LifecycleHookEventNameV1::PostCompact
                | LifecycleHookEventNameV1::SubagentStart
                | LifecycleHookEventNameV1::SubagentStop
        ) {
            return Err(format!(
                "plugin {plugin_name} hook {} event is unsupported",
                self.id
            ));
        }
        match (tool_event, self.matcher.as_deref()) {
            (true, Some(matcher)) => {
                super::mcp::validate_lower_snake("plugin hook matcher", matcher)?
            }
            (true, None) => {
                return Err(format!(
                    "plugin {plugin_name} hook {} matcher is required",
                    self.id
                ));
            }
            (false, Some(_)) => {
                return Err(format!(
                    "plugin {plugin_name} hook {} matcher is unsupported for this event",
                    self.id
                ));
            }
            (false, None) => {}
        }
        Ok(())
    }
}

pub(crate) fn load_plugin_hooks_file(path: &Path) -> Result<PluginHooksFileV1, String> {
    let raw = read_tool_contract_file(path)
        .map_err(|error| format!("read plugin hooks failed {}: {error}", path.display()))?;
    let hooks: PluginHooksFileV1 = serde_json::from_str(raw.as_str())
        .map_err(|error| format!("parse plugin hooks failed {}: {error}", path.display()))?;
    if hooks.schema != PLUGIN_HOOKS_SCHEMA_V1 {
        return Err(format!("plugin hooks schema mismatch: {}", path.display()));
    }
    if hooks.handlers.len() > MAX_PLUGIN_HOOK_HANDLERS {
        return Err(format!(
            "plugin hooks exceeded {MAX_PLUGIN_HOOK_HANDLERS} handlers: {}",
            path.display()
        ));
    }
    let mut ids = HashSet::new();
    for handler in &hooks.handlers {
        handler.validate("package")?;
        if !ids.insert(handler.id.as_str()) {
            return Err(format!("duplicate plugin hook handler id: {}", handler.id));
        }
    }
    Ok(hooks)
}

fn validate_paths(field: &str, paths: &[String]) -> Result<(), String> {
    for path in paths {
        validate_relative_resource_path(field, path)?;
    }
    Ok(())
}

pub(crate) fn resolve_plugin_resource_path(
    plugin_root: &Path,
    raw_path: &str,
) -> Result<PathBuf, String> {
    validate_relative_resource_path("resource", raw_path)?;
    let root = plugin_root
        .canonicalize()
        .map_err(|error| format!("canonicalize plugin root failed: {error}"))?;
    let joined = root.join(raw_path);
    let resolved = joined.canonicalize().map_err(|error| {
        format!(
            "canonicalize plugin resource failed {}: {error}",
            joined.display()
        )
    })?;
    if !resolved.starts_with(root.as_path()) {
        return Err(format!(
            "plugin resource path escaped plugin root: {}",
            raw_path
        ));
    }
    Ok(resolved)
}

pub(crate) fn validate_relative_resource_path(field: &str, raw_path: &str) -> Result<(), String> {
    if raw_path.trim().is_empty() {
        return Err(format!("plugin manifest {field} contains an empty path"));
    }
    if raw_path.contains(['\\', ':'])
        || raw_path.split('/').any(|part| part.is_empty())
        || raw_path.chars().any(|character| character.is_control())
        || raw_path.nfc().collect::<String>() != raw_path
    {
        return Err(format!(
            "plugin manifest {field} path must be a canonical relative POSIX path: {raw_path}"
        ));
    }
    let path = Path::new(raw_path);
    if path.is_absolute() {
        return Err(format!(
            "plugin manifest {field} path must be relative: {raw_path}"
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "plugin manifest {field} path must stay inside the plugin: {raw_path}"
        ));
    }
    Ok(())
}

pub(crate) fn validate_plugin_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('-')
        || name.ends_with('-')
        || name.contains("--")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("plugin manifest name must be lower-kebab-case".to_string());
    }
    Ok(())
}

pub(crate) fn validate_plugin_version(version: &str) -> Result<(), String> {
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err("plugin manifest version must be major.minor.patch".to_string());
    }
    Ok(())
}

fn default_timeout_ms() -> u64 {
    5_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn plugin_manifest_lists_hook_declarations() {
        let manifest = PluginManifestV1 {
            name: "wiki".to_string(),
            version: "1.0.0".to_string(),
            paths: PluginManifestPathsV1 {
                hooks: vec!["hooks/hooks.json".to_string()],
                skills: vec!["skills/wiki".to_string()],
                ..Default::default()
            },
            interface: None,
        };

        let declarations = manifest.hook_declarations().unwrap();

        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].plugin_name, "wiki");
        assert_eq!(declarations[0].hook_path, "hooks/hooks.json");
    }

    #[test]
    fn plugin_manifest_rejects_path_escape() {
        let manifest = PluginManifestV1 {
            name: "bad".to_string(),
            paths: PluginManifestPathsV1 {
                hooks: vec!["../hooks.json".to_string()],
                ..Default::default()
            },
            version: "1.0.0".to_string(),
            interface: None,
        };

        let error = manifest.validate().unwrap_err();
        assert!(error.contains("must stay inside"));
    }

    #[test]
    fn plugin_manifest_rejects_non_nfc_resource_path() {
        let manifest = PluginManifestV1 {
            name: "bad".to_string(),
            paths: PluginManifestPathsV1 {
                skills: vec!["skills/cafe\u{301}/SKILL.md".to_string()],
                ..Default::default()
            },
            version: "1.0.0".to_string(),
            interface: None,
        };

        assert!(manifest.validate().unwrap_err().contains("canonical"));
    }

    #[test]
    fn plugin_manifest_rejects_unknown_fields() {
        let error = serde_json::from_str::<PluginManifestV1>(
            r#"{"name":"bad","paths":{},"unexpected":true}"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn plugin_registry_loads_hook_handlers_with_trust_state() {
        let root = temp_plugin_root("trusted");
        fs::create_dir_all(root.join(".centaeris-plugin")).unwrap();
        fs::create_dir_all(root.join("hooks")).unwrap();
        write_file(
            root.join(".centaeris-plugin/plugin.json").as_path(),
            r#"{"name":"wiki","version":"1.0.0","paths":{"hooks":["hooks/hooks.json"]}}"#,
        );
        write_file(
            root.join("hooks/hooks.json").as_path(),
            r#"{"schema":"plugin_hooks_v1","handlers":[{"id":"guard","event":"PreToolUse","matcher":"write","program":"hook-bin","args":["--check"]}]}"#,
        );

        let expected_root = root.canonicalize().unwrap();
        let registry = load_plugin_registry_from_manifests(
            &[root.join(".centaeris-plugin/plugin.json")],
            &PluginTrustPolicyV1 {
                trusted_plugins: vec!["wiki".to_string()],
            },
        )
        .unwrap();
        assert_eq!(registry.plugins.len(), 1);
        assert_eq!(registry.hook_handlers.len(), 1);
        assert_eq!(registry.hook_handlers[0].id, "wiki:guard");
        assert!(registry.hook_handlers[0].trusted);
        assert_eq!(
            registry.hook_handlers[0].event,
            LifecycleHookEventNameV1::PreToolUse
        );
        assert_eq!(
            registry.hook_handlers[0].cwd.as_deref(),
            expected_root.to_str()
        );
        let _ = fs::remove_dir_all(root.as_path());
    }

    #[test]
    fn plugin_registry_marks_untrusted_plugins_without_executing_policy() {
        let root = temp_plugin_root("untrusted");
        fs::create_dir_all(root.join(".centaeris-plugin")).unwrap();
        fs::create_dir_all(root.join("hooks")).unwrap();
        write_file(
            root.join(".centaeris-plugin/plugin.json").as_path(),
            r#"{"name":"wiki","version":"1.0.0","paths":{"hooks":["hooks/hooks.json"]}}"#,
        );
        write_file(
            root.join("hooks/hooks.json").as_path(),
            r#"{"schema":"plugin_hooks_v1","handlers":[{"id":"guard","event":"PreToolUse","matcher":"write","program":"hook-bin"}]}"#,
        );

        let registry = load_plugin_registry_from_manifests(
            &[root.join(".centaeris-plugin/plugin.json")],
            &PluginTrustPolicyV1::default(),
        )
        .unwrap();
        let _ = fs::remove_dir_all(root.as_path());

        assert!(!registry.hook_handlers[0].trusted);
    }

    #[test]
    fn plugin_hook_rejects_configurable_cwd() {
        let root = temp_plugin_root("cwd_escape");
        fs::create_dir_all(root.join(".centaeris-plugin")).unwrap();
        fs::create_dir_all(root.join("hooks")).unwrap();
        write_file(
            root.join(".centaeris-plugin/plugin.json").as_path(),
            r#"{"name":"wiki","version":"1.0.0","paths":{"hooks":["hooks/hooks.json"]}}"#,
        );
        write_file(
            root.join("hooks/hooks.json").as_path(),
            r#"{"schema":"plugin_hooks_v1","handlers":[{"id":"guard","event":"PreToolUse","matcher":"write","program":"hook-bin","cwd":".."}]}"#,
        );

        let error = load_plugin_registry_from_manifests(
            &[root.join(".centaeris-plugin/plugin.json")],
            &PluginTrustPolicyV1::default(),
        )
        .unwrap_err();
        let _ = fs::remove_dir_all(root.as_path());

        assert!(error.contains("unknown field"));
    }

    #[test]
    fn plugin_hooks_reject_unknown_fields() {
        let error = serde_json::from_str::<PluginHooksFileV1>(
            r#"{"schema":"plugin_hooks_v1","handlers":[],"extra":true}"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn workspace_plugin_hooks_reject_unsupported_events_and_matchers() {
        let unsupported_event = PluginHookHandlerConfigV1 {
            id: "session-start".to_string(),
            event: LifecycleHookEventNameV1::SessionStart,
            matcher: None,
            program: "node".to_string(),
            args: Vec::new(),
            timeout_ms: 5_000,
        };
        assert!(unsupported_event
            .validate("wiki")
            .unwrap_err()
            .contains("event is unsupported"));

        let invalid_matcher = PluginHookHandlerConfigV1 {
            id: "guard".to_string(),
            event: LifecycleHookEventNameV1::PreToolUse,
            matcher: Some("WriteFile".to_string()),
            program: "node".to_string(),
            args: Vec::new(),
            timeout_ms: 5_000,
        };
        assert!(invalid_matcher
            .validate("wiki")
            .unwrap_err()
            .contains("lower_snake_case"));
    }

    fn temp_plugin_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "centaeris_plugin_{}_{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.as_path()).unwrap();
        root
    }

    fn write_file(path: &Path, content: &str) {
        let mut file = fs::File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }
}
