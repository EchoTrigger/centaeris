use crate::{user_config, user_data_layout};
use centaeris_core::extension::{
    build_plugin_activation_snapshot, list_plugins, plugin_catalog_state, plugin_detail,
    plugin_source_ref, reload_plugins, resolve_plugin_package, set_plugin_enabled,
    PluginCatalogRoots, PluginCatalogStateV1, PluginConfigStore, PluginDescriptorV1,
    PluginDetailRequestV1, PluginDetailV1, PluginListRequestV1, PluginSetEnabledRequestV1,
    PluginSourceRefV1,
};
use centaeris_core::runtime::contracts::current_timestamp_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static PLUGIN_STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginSourceRefRequestV1 {
    pub(crate) id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginInstallRequestV1 {
    pub(crate) source_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PluginRemoveRequestV1 {
    pub(crate) id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginRemoveResponseV1 {
    pub(crate) removed_id: String,
    pub(crate) catalog: PluginCatalogStateV1,
}

pub(crate) fn list(request: PluginListRequestV1) -> Result<Vec<PluginDescriptorV1>, String> {
    let _guard = plugin_store_guard()?;
    let roots = plugin_roots();
    let mut descriptors = list_plugins(request, &roots, &config_store())?;
    project_descriptor_sources(descriptors.as_mut_slice(), &roots)?;
    Ok(descriptors)
}

pub(crate) fn detail(request: PluginDetailRequestV1) -> Result<PluginDetailV1, String> {
    let _guard = plugin_store_guard()?;
    let roots = plugin_roots();
    let mut detail = plugin_detail(request, &roots, &config_store())?;
    project_descriptor_sources(std::slice::from_mut(&mut detail.descriptor), &roots)?;
    Ok(detail)
}

pub(crate) fn set_enabled(
    request: PluginSetEnabledRequestV1,
) -> Result<PluginCatalogStateV1, String> {
    let _guard = plugin_store_guard()?;
    set_plugin_enabled(request, &plugin_roots(), &config_store())
}

pub(crate) fn reload() -> Result<PluginCatalogStateV1, String> {
    let _guard = plugin_store_guard()?;
    reload_plugins(&plugin_roots(), &config_store())
}

pub(crate) fn source_ref(request: PluginSourceRefRequestV1) -> Result<PluginSourceRefV1, String> {
    let _guard = plugin_store_guard()?;
    plugin_source_ref(request.id.as_str(), &plugin_roots(), &config_store())
}

pub(crate) fn current_catalog_state() -> Result<PluginCatalogStateV1, String> {
    let _guard = plugin_store_guard()?;
    let descriptors = list_plugins(
        PluginListRequestV1::default(),
        &plugin_roots(),
        &config_store(),
    )?;
    Ok(plugin_catalog_state(descriptors.as_slice()))
}

pub(crate) fn install(request: PluginInstallRequestV1) -> Result<PluginDetailV1, String> {
    let _guard = plugin_store_guard()?;
    let source_raw = request.source_path.trim();
    if source_raw.is_empty() {
        return Err("plugin install sourcePath is required".to_string());
    }
    let source = Path::new(source_raw).canonicalize().map_err(|error| {
        format!(
            "canonicalize plugin install source failed {}: {error}",
            Path::new(source_raw).display()
        )
    })?;
    let managed_root = user_data_layout::plugins_dir_path();
    fs::create_dir_all(managed_root.as_path())
        .map_err(|error| format!("create managed plugin directory failed: {error}"))?;
    let managed_root = managed_root
        .canonicalize()
        .map_err(|error| format!("canonicalize managed plugin directory failed: {error}"))?;
    if source.starts_with(managed_root.as_path()) || managed_root.starts_with(source.as_path()) {
        return Err(
            "plugin install source must not overlap the managed plugin directory".to_string(),
        );
    }
    let source_package = resolve_plugin_package(source.as_path())?;

    let descriptors = list_plugins(
        PluginListRequestV1::default(),
        &plugin_roots(),
        &config_store(),
    )?;
    if descriptors
        .iter()
        .any(|descriptor| descriptor.id == source_package.name)
    {
        return Err(format!(
            "plugin is already installed: {}",
            source_package.name
        ));
    }
    let target = managed_root.join(source_package.name.as_str());
    if target.exists() {
        return Err(format!(
            "managed plugin destination already exists: {}",
            target.display()
        ));
    }
    let staging_root = managed_root.join(".staging");
    fs::create_dir_all(staging_root.as_path())
        .map_err(|error| format!("create plugin staging directory failed: {error}"))?;
    let staging = staging_root.join(format!(
        "install-{}-{}-{}",
        source_package.name,
        std::process::id(),
        current_timestamp_ms()
    ));
    if staging.exists() {
        return Err(format!(
            "plugin install staging path already exists: {}",
            staging.display()
        ));
    }

    let mut activated = false;
    let install_result = (|| {
        copy_directory_tree(source.as_path(), staging.as_path())?;
        let managed_package = resolve_plugin_package(staging.as_path())?;
        if managed_package != source_package {
            return Err(
                "managed plugin copy does not match the validated source package".to_string(),
            );
        }
        let mut enabled_roots = descriptors
            .iter()
            .filter(|descriptor| descriptor.enabled && descriptor.errors.is_empty())
            .map(|descriptor| PathBuf::from(descriptor.path.as_str()))
            .collect::<Vec<_>>();
        enabled_roots.push(staging.clone());
        build_plugin_activation_snapshot(enabled_roots.as_slice())?;
        fs::rename(staging.as_path(), target.as_path()).map_err(|error| {
            format!(
                "activate managed plugin failed {} -> {}: {error}",
                staging.display(),
                target.display()
            )
        })?;
        activated = true;
        config_store().set_enabled(source_package.name.as_str(), true)?;
        let roots = plugin_roots();
        let mut detail = plugin_detail(
            PluginDetailRequestV1 {
                id: source_package.name.clone(),
            },
            &roots,
            &config_store(),
        )?;
        project_descriptor_sources(std::slice::from_mut(&mut detail.descriptor), &roots)?;
        Ok(detail)
    })();
    if install_result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(staging.as_path());
    }
    if install_result.is_err() && activated && target.exists() {
        let _ = fs::remove_dir_all(target.as_path());
    }
    install_result
}

pub(crate) fn remove(request: PluginRemoveRequestV1) -> Result<PluginRemoveResponseV1, String> {
    let _guard = plugin_store_guard()?;
    let id = request.id.trim();
    if id.is_empty() {
        return Err("plugin remove id is required".to_string());
    }
    let managed_root = user_data_layout::plugins_dir_path()
        .canonicalize()
        .map_err(|error| format!("canonicalize managed plugin directory failed: {error}"))?;
    let descriptors = list_plugins(
        PluginListRequestV1::default(),
        &plugin_roots(),
        &config_store(),
    )?;
    let descriptor = descriptors
        .into_iter()
        .filter(|descriptor| descriptor.id == id)
        .find(|descriptor| {
            Path::new(descriptor.path.as_str())
                .canonicalize()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent == managed_root))
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("managed plugin not found: id={id}"))?;
    let target = Path::new(descriptor.path.as_str())
        .canonicalize()
        .map_err(|error| format!("canonicalize managed plugin failed: {error}"))?;
    if target.parent() != Some(managed_root.as_path()) {
        return Err(format!("plugin is not managed by Desktop: id={id}"));
    }
    let trash_root = managed_root.join(".trash");
    fs::create_dir_all(trash_root.as_path())
        .map_err(|error| format!("create plugin removal directory failed: {error}"))?;
    let quarantine = trash_root.join(format!(
        "remove-{}-{}-{}",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("plugin"),
        std::process::id(),
        current_timestamp_ms()
    ));
    config_store().set_enabled(id, true)?;
    if let Err(error) = fs::rename(target.as_path(), quarantine.as_path()) {
        if !descriptor.enabled {
            let _ = config_store().set_enabled(id, false);
        }
        return Err(format!("detach managed plugin failed: {error}"));
    }
    if let Err(error) = fs::remove_dir_all(quarantine.as_path()) {
        let restore = fs::rename(quarantine.as_path(), target.as_path());
        if !descriptor.enabled {
            let _ = config_store().set_enabled(id, false);
        }
        return Err(match restore {
            Ok(()) => format!("remove managed plugin failed: {error}"),
            Err(restore_error) => {
                format!("remove managed plugin failed: {error}; restore failed: {restore_error}")
            }
        });
    }
    let descriptors = list_plugins(
        PluginListRequestV1::default(),
        &plugin_roots(),
        &config_store(),
    )?;
    Ok(PluginRemoveResponseV1 {
        removed_id: id.to_string(),
        catalog: plugin_catalog_state(descriptors.as_slice()),
    })
}

fn copy_directory_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("inspect plugin install source failed: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "plugin package must not contain symlinks: {}",
            source.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "plugin install source is not a directory: {}",
            source.display()
        ));
    }
    fs::create_dir(destination)
        .map_err(|error| format!("create managed plugin directory failed: {error}"))?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| format!("read plugin install source failed: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read plugin install entry failed: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let entry_source = entry.path();
        let entry_destination = destination.join(entry.file_name());
        let entry_metadata = fs::symlink_metadata(entry_source.as_path())
            .map_err(|error| format!("inspect plugin install entry failed: {error}"))?;
        if entry_metadata.file_type().is_symlink() {
            return Err(format!(
                "plugin package must not contain symlinks: {}",
                entry_source.display()
            ));
        }
        if entry_metadata.is_dir() {
            copy_directory_tree(entry_source.as_path(), entry_destination.as_path())?;
        } else if entry_metadata.is_file() {
            fs::copy(entry_source.as_path(), entry_destination.as_path()).map_err(|error| {
                format!(
                    "copy managed plugin file failed {}: {error}",
                    entry_source.display()
                )
            })?;
        } else {
            return Err(format!(
                "plugin package contains unsupported entry: {}",
                entry_source.display()
            ));
        }
    }
    Ok(())
}

fn project_descriptor_sources(
    descriptors: &mut [PluginDescriptorV1],
    roots: &PluginCatalogRoots,
) -> Result<(), String> {
    let mut canonical_roots = roots
        .plugin_roots
        .iter()
        .map(|root| {
            root.canonicalize().map_err(|error| {
                format!(
                    "canonicalize Desktop plugin catalog root failed {}: {error}",
                    root.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if canonical_roots.is_empty() {
        return Err("Desktop plugin catalog roots are unavailable".to_string());
    }
    let managed_root = canonical_roots.remove(0);
    for descriptor in descriptors {
        let plugin_root = Path::new(descriptor.path.as_str())
            .canonicalize()
            .map_err(|error| {
                format!(
                    "canonicalize Desktop plugin path failed {}: {error}",
                    descriptor.path
                )
            })?;
        let parent = plugin_root.parent().ok_or_else(|| {
            format!(
                "Desktop plugin path has no catalog parent: {}",
                plugin_root.display()
            )
        })?;
        descriptor.source = if parent == managed_root {
            "managed"
        } else if canonical_roots.iter().any(|root| parent == root) {
            "bundled"
        } else {
            return Err(format!(
                "Desktop plugin path is outside its catalog roots: {}",
                plugin_root.display()
            ));
        }
        .to_string();
    }
    Ok(())
}

fn plugin_store_guard() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    PLUGIN_STORE_LOCK
        .lock()
        .map_err(|_| "plugin store lock poisoned".to_string())
}

fn plugin_roots() -> PluginCatalogRoots {
    PluginCatalogRoots {
        plugin_roots: user_data_layout::plugin_roots(),
    }
}

#[derive(Clone, Copy)]
struct UserPluginConfigStore;

impl PluginConfigStore for UserPluginConfigStore {
    fn disabled_ids(&self) -> Result<HashSet<String>, String> {
        Ok(user_config::load()?.plugins.disabled.into_iter().collect())
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let id = id.trim();
        if id.is_empty() {
            return Err("plugin id is required".to_string());
        }
        user_config::update(|config| {
            let mut disabled = config
                .plugins
                .disabled
                .iter()
                .cloned()
                .collect::<HashSet<_>>();
            if enabled {
                disabled.remove(id);
            } else {
                disabled.insert(id.to_string());
            }
            config.plugins.disabled = disabled.into_iter().collect();
            config.plugins.disabled.sort();
            Ok(())
        })
    }
}

fn config_store() -> UserPluginConfigStore {
    UserPluginConfigStore
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn plugin_roots_use_the_plugin_package_directory() {
        let roots = plugin_roots();
        assert!(roots
            .plugin_roots
            .iter()
            .any(|path| path.ends_with("plugins")));
    }

    #[test]
    fn install_copies_validated_source_and_remove_keeps_original() {
        let _environment_guard = crate::message_log::test_env_mutex()
            .lock()
            .expect("environment mutex");
        let root = std::env::temp_dir().join(format!(
            "centaeris-managed-plugin-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        let data_root = root.join("data");
        let source = root.join("source");
        fs::create_dir_all(source.join(".centaeris-plugin")).expect("manifest directory");
        fs::create_dir_all(source.join("bin")).expect("CLI directory");
        fs::create_dir_all(source.join("hooks")).expect("hook directory");
        fs::write(
            source.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"managed-demo","version":"1.0.0","paths":{"cli":["bin/managed-demo"],"hooks":["hooks/hooks.json"]}}"#,
        )
        .expect("manifest");
        fs::write(source.join("bin/managed-demo"), "demo").expect("CLI");
        fs::write(
            source.join("hooks/hooks.json"),
            r#"{"schema":"plugin_hooks_v1","handlers":[{"id":"check","event":"UserPromptSubmit","program":"managed-demo"}]}"#,
        )
        .expect("hook declaration");
        let source_package = resolve_plugin_package(source.as_path()).expect("source package");
        let previous_data_root = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", data_root.as_os_str());
        user_data_layout::ensure_user_data_layout().expect("user data layout");
        let overlap_error = install(PluginInstallRequestV1 {
            source_path: data_root.to_string_lossy().to_string(),
        })
        .expect_err("managed directory ancestor must be rejected before copy");
        assert!(overlap_error.contains("must not overlap"));

        let installed = install(PluginInstallRequestV1 {
            source_path: source.to_string_lossy().to_string(),
        })
        .expect("install plugin");
        let managed_path = PathBuf::from(installed.descriptor.path.as_str());
        assert_eq!(installed.descriptor.id, "managed-demo");
        assert_eq!(installed.descriptor.source, "managed");
        assert_ne!(managed_path, source);
        assert!(managed_path.starts_with(data_root.join("plugins")));
        assert_eq!(
            resolve_plugin_package(managed_path.as_path()).expect("managed package"),
            source_package
        );

        let removed = remove(PluginRemoveRequestV1 {
            id: "managed-demo".to_string(),
        })
        .expect("remove plugin");
        assert_eq!(removed.removed_id, "managed-demo");
        assert!(!managed_path.exists());
        assert_eq!(
            resolve_plugin_package(source.as_path()).expect("original package remains"),
            source_package
        );

        restore_environment("CENTAERIS_DESKTOP_DATA_DIR", previous_data_root);
        fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn desktop_source_projection_uses_catalog_root_identity() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-plugin-source-projection-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        let managed_root = root.join("plugins");
        let bundled_root = root.join("native-plugins");
        let managed_plugin = managed_root.join("managed-demo");
        let bundled_plugin = bundled_root.join("bundled-demo");
        fs::create_dir_all(managed_plugin.as_path()).expect("managed plugin");
        fs::create_dir_all(bundled_plugin.as_path()).expect("bundled plugin");
        let mut descriptors = vec![
            test_descriptor("managed-demo", managed_plugin.as_path()),
            test_descriptor("bundled-demo", bundled_plugin.as_path()),
        ];

        project_descriptor_sources(
            descriptors.as_mut_slice(),
            &PluginCatalogRoots {
                plugin_roots: vec![managed_root, bundled_root],
            },
        )
        .expect("project Desktop sources");

        assert_eq!(descriptors[0].source, "managed");
        assert_eq!(descriptors[1].source, "bundled");
        fs::remove_dir_all(root).expect("remove projection root");
    }

    fn test_descriptor(id: &str, path: &Path) -> PluginDescriptorV1 {
        PluginDescriptorV1 {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            source: "local".to_string(),
            enabled: true,
            path: path.to_string_lossy().to_string(),
            manifest_path: None,
            errors: Vec::new(),
            version: Some("1.0.0".to_string()),
            tools: Vec::new(),
            scopes: Vec::new(),
            activation_status: "enabled".to_string(),
            policy_source: "user".to_string(),
        }
    }

    fn restore_environment(name: &str, value: Option<OsString>) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
}
