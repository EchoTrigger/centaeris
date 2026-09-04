use super::activation::plugin_catalog_state;
use super::config::PluginConfigStore;
use super::manifest::load_plugin_manifest_file;
use super::types::{
    PluginCapabilitiesV1, PluginCatalogStateV1, PluginDescriptorV1, PluginDetailRequestV1,
    PluginDetailV1, PluginListRequestV1, PluginSetEnabledRequestV1, PluginSourceRefV1,
};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PluginCatalogRoots {
    pub plugin_roots: Vec<PathBuf>,
}

pub fn list_plugins(
    _request: PluginListRequestV1,
    roots: &PluginCatalogRoots,
    config_store: &dyn PluginConfigStore,
) -> Result<Vec<PluginDescriptorV1>, String> {
    let mut descriptors = load_plugin_descriptors(roots, config_store)?;
    descriptors.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(descriptors)
}

pub fn reload_plugins(
    roots: &PluginCatalogRoots,
    config_store: &dyn PluginConfigStore,
) -> Result<PluginCatalogStateV1, String> {
    let descriptors = list_plugins(PluginListRequestV1::default(), roots, config_store)?;
    Ok(plugin_catalog_state(descriptors.as_slice()))
}

pub fn set_plugin_enabled(
    request: PluginSetEnabledRequestV1,
    roots: &PluginCatalogRoots,
    config_store: &dyn PluginConfigStore,
) -> Result<PluginCatalogStateV1, String> {
    let before = list_plugins(PluginListRequestV1::default(), roots, config_store)?;
    if !before.iter().any(|item| item.id == request.id) {
        return Err(format!("plugin not found: id={}", request.id));
    }
    config_store.set_enabled(request.id.as_str(), request.enabled)?;
    reload_plugins(roots, config_store)
}

pub fn plugin_detail(
    request: PluginDetailRequestV1,
    roots: &PluginCatalogRoots,
    config_store: &dyn PluginConfigStore,
) -> Result<PluginDetailV1, String> {
    let descriptors = list_plugins(PluginListRequestV1::default(), roots, config_store)?;
    let descriptor = descriptors
        .into_iter()
        .find(|item| item.id == request.id)
        .ok_or_else(|| format!("plugin not found: id={}", request.id))?;
    let capabilities = plugin_capabilities(&descriptor)?;
    Ok(PluginDetailV1 {
        descriptor,
        capabilities,
    })
}

pub fn plugin_source_ref(
    id: &str,
    roots: &PluginCatalogRoots,
    config_store: &dyn PluginConfigStore,
) -> Result<PluginSourceRefV1, String> {
    let descriptors = list_plugins(PluginListRequestV1::default(), roots, config_store)?;
    let descriptor = descriptors
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("plugin not found: id={id}"))?;
    Ok(PluginSourceRefV1 {
        kind: "local_path".to_string(),
        path: descriptor.manifest_path.unwrap_or(descriptor.path),
    })
}

fn plugin_capabilities(descriptor: &PluginDescriptorV1) -> Result<PluginCapabilitiesV1, String> {
    let manifest_path = descriptor
        .manifest_path
        .as_ref()
        .ok_or_else(|| format!("plugin missing manifestPath: {}", descriptor.id))?;
    let manifest = load_plugin_manifest_file(Path::new(manifest_path))?;
    Ok(PluginCapabilitiesV1 {
        skills: manifest.paths.skills,
        cli: manifest.paths.cli,
        mcp_servers: manifest.paths.mcp_servers,
        apps: manifest.paths.apps,
        hooks: manifest.paths.hooks,
        capabilities: manifest
            .interface
            .map(|interface| interface.capabilities)
            .unwrap_or_default(),
    })
}

fn load_plugin_descriptors(
    roots: &PluginCatalogRoots,
    config_store: &dyn PluginConfigStore,
) -> Result<Vec<PluginDescriptorV1>, String> {
    let disabled = config_store.disabled_ids()?;
    let mut descriptors = Vec::new();
    for manifest_path in plugin_manifest_paths(roots.plugin_roots.as_slice())? {
        let descriptor = match load_plugin_manifest_file(manifest_path.as_path()) {
            Ok(manifest) => {
                let plugin_root = manifest_path
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or_else(|| Path::new(""))
                    .to_path_buf();
                let id = manifest.name.clone();
                let enabled = !disabled.contains(id.as_str());
                PluginDescriptorV1 {
                    id,
                    name: manifest
                        .interface
                        .as_ref()
                        .and_then(|interface| interface.display_name.clone())
                        .unwrap_or_else(|| manifest.name.clone()),
                    description: manifest
                        .interface
                        .as_ref()
                        .and_then(|interface| interface.short_description.clone())
                        .unwrap_or_default(),
                    source: source_label(plugin_root.as_path()),
                    enabled,
                    path: path_to_string(plugin_root.as_path()),
                    manifest_path: Some(path_to_string(manifest_path.as_path())),
                    errors: Vec::new(),
                    version: Some(manifest.version),
                    tools: manifest
                        .interface
                        .map(|interface| interface.capabilities)
                        .unwrap_or_default(),
                    scopes: Vec::new(),
                    activation_status: if enabled { "enabled" } else { "disabled" }.to_string(),
                    policy_source: "user".to_string(),
                }
            }
            Err(error) => PluginDescriptorV1 {
                id: path_to_string(manifest_path.as_path()),
                name: manifest_path
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .map(|value| value.to_string_lossy().to_string())
                    .unwrap_or_else(|| "plugin".to_string()),
                description: String::new(),
                source: source_label(manifest_path.as_path()),
                enabled: false,
                path: manifest_path
                    .parent()
                    .and_then(Path::parent)
                    .map(path_to_string)
                    .unwrap_or_else(|| path_to_string(manifest_path.as_path())),
                manifest_path: Some(path_to_string(manifest_path.as_path())),
                errors: vec![error],
                version: None,
                tools: Vec::new(),
                scopes: Vec::new(),
                activation_status: "error".to_string(),
                policy_source: "manifest".to_string(),
            },
        };
        descriptors.push(descriptor);
    }
    Ok(descriptors)
}

fn plugin_manifest_paths(roots: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut manifests = Vec::new();
    for root in roots {
        if !root.exists() {
            continue;
        }
        if root.is_file() {
            if root.ends_with(".centaeris-plugin/plugin.json") {
                manifests.push(root.clone());
            }
            continue;
        }
        if !root.is_dir() {
            continue;
        }
        let direct = root.join(".centaeris-plugin/plugin.json");
        if direct.is_file() {
            manifests.push(direct);
            continue;
        }
        for entry in fs::read_dir(root.as_path())
            .map_err(|error| format!("read plugin root failed {}: {error}", root.display()))?
        {
            let entry = entry.map_err(|error| format!("read plugin entry failed: {error}"))?;
            let candidate = entry.path().join(".centaeris-plugin/plugin.json");
            if candidate.is_file() {
                manifests.push(candidate);
            }
        }
    }
    manifests.sort();
    manifests.dedup();
    Ok(manifests)
}

fn source_label(_path: &Path) -> String {
    "local".to_string()
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::config::PluginConfigStore;
    use std::collections::HashSet;
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestPluginConfigStore {
        disabled: Mutex<HashSet<String>>,
    }

    impl PluginConfigStore for TestPluginConfigStore {
        fn disabled_ids(&self) -> Result<HashSet<String>, String> {
            self.disabled
                .lock()
                .map_err(|_| "test plugin config lock poisoned".to_string())
                .map(|disabled| disabled.clone())
        }

        fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
            let mut disabled = self
                .disabled
                .lock()
                .map_err(|_| "test plugin config lock poisoned".to_string())?;
            if enabled {
                disabled.remove(id);
            } else {
                disabled.insert(id.to_string());
            }
            Ok(())
        }
    }

    #[test]
    fn plugin_registry_lists_details_and_toggles_plugins_after_skill_split() {
        let root = temp_root("plugin-lifecycle");
        let plugin_root = root.join("plugins").join("demo");
        fs::create_dir_all(plugin_root.join(".centaeris-plugin")).expect("plugin root");
        fs::write(
            plugin_root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"demo","version":"1.0.0","paths":{"skills":["skills/demo"],"mcpServers":[".mcp.json"],"apps":[".app.json"],"hooks":["hooks.json"]},"interface":{"displayName":"Demo","shortDescription":"Demo plugin","capabilities":["Instructions"]}}"#,
        )
        .expect("plugin manifest");
        let store = TestPluginConfigStore::default();
        let roots = PluginCatalogRoots {
            plugin_roots: vec![root.join("plugins")],
        };

        let listed =
            list_plugins(PluginListRequestV1::default(), &roots, &store).expect("list plugins");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "demo");
        assert_eq!(listed[0].source, "local");
        assert!(listed[0].enabled);

        let detail = plugin_detail(
            PluginDetailRequestV1 {
                id: "demo".to_string(),
            },
            &roots,
            &store,
        )
        .expect("plugin detail");
        assert_eq!(detail.capabilities.skills, vec!["skills/demo"]);
        assert_eq!(detail.capabilities.mcp_servers, vec![".mcp.json"]);

        let snapshot = set_plugin_enabled(
            PluginSetEnabledRequestV1 {
                id: "demo".to_string(),
                enabled: false,
            },
            &roots,
            &store,
        )
        .expect("disable plugin");
        assert_eq!(snapshot.schema, "plugin_catalog_state_v1");
        assert_eq!(snapshot.disabled_plugins, vec!["demo"]);
        assert!(snapshot.enabled_plugins.is_empty());
        assert!(
            !list_plugins(PluginListRequestV1::default(), &roots, &store)
                .expect("list disabled plugin")[0]
                .enabled
        );

        let _ = fs::remove_dir_all(root);
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "centaeris-plugin-catalog-{label}-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(root.as_path()).expect("temp root");
        root
    }
}
