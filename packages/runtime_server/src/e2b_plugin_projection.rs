//! Frozen, authorized package bytes for the native E2B files port.
use crate::skill_projection::{verified_plugin_package_roots_at, PLUGIN_CATALOG_ROOT};
use centaeris_core::extension::PluginActivationSnapshotV1;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;

// Match the existing SDK bulk transfer ceiling and native directory budget.
const MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 10_000;

pub(crate) struct PluginFile {
    pub bytes: Vec<u8>,
    pub executable: bool,
}
pub(crate) struct E2bPluginProjection {
    pub digest: String,
    pub files: BTreeMap<String, PluginFile>,
    pub directories: Vec<String>,
    pub command_directories: Vec<String>,
}
impl E2bPluginProjection {
    pub(crate) fn load_at(
        activation: &PluginActivationSnapshotV1,
        catalog: &Path,
    ) -> Result<Self, String> {
        let roots = verified_plugin_package_roots_at(activation, catalog)?;
        // Verify the private copy against Core's digest before publishing its bytes.
        // A package changed while being copied cannot become an authorized projection.
        let frozen = tempfile::tempdir().map_err(|_| "e2b_plugin_freeze_unavailable")?;
        let mut budget = (0u64, 0usize);
        for (package, source) in activation.packages.iter().zip(roots) {
            copy_package(&source, &frozen.path().join(&package.name), &mut budget)?;
        }
        verified_plugin_package_roots_at(activation, frozen.path())?;
        let mut projection = Self {
            digest: activation.digest.clone(),
            files: BTreeMap::new(),
            directories: Vec::new(),
            command_directories: crate::docker_execution_host::plugin_command_directories(
                activation,
            )?,
        };
        for package in &activation.packages {
            projection.collect(
                &frozen.path().join(&package.name),
                &format!("{PLUGIN_CATALOG_ROOT}/{}", package.name),
            )?;
        }
        Ok(projection)
    }
    fn collect(&mut self, source: &Path, guest: &str) -> Result<(), String> {
        if guest.len() > 4096 {
            return Err("e2b_plugin_path_invalid".into());
        }
        let meta = fs::symlink_metadata(source).map_err(|_| "e2b_plugin_freeze_unavailable")?;
        if meta.is_dir() {
            self.directories.push(guest.into());
            for entry in fs::read_dir(source).map_err(|_| "e2b_plugin_freeze_unavailable")? {
                let entry = entry.map_err(|_| "e2b_plugin_freeze_unavailable")?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "e2b_plugin_path_invalid")?;
                if name.contains(['\\', ':']) || name.chars().any(char::is_control) {
                    return Err("e2b_plugin_path_invalid".into());
                }
                self.collect(&entry.path(), &format!("{guest}/{name}"))?;
            }
        } else {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            self.files.insert(
                guest.into(),
                PluginFile {
                    bytes: fs::read(source).map_err(|_| "e2b_plugin_freeze_unavailable")?,
                    executable,
                },
            );
        }
        Ok(())
    }
    pub(crate) fn contains_path(&self, path: &str) -> bool {
        self.files.contains_key(path) || self.directories.iter().any(|p| p == path)
    }
}
fn copy_package(
    source: &Path,
    destination: &Path,
    budget: &mut (u64, usize),
) -> Result<(), String> {
    let meta = fs::symlink_metadata(source).map_err(|_| "e2b_plugin_freeze_unavailable")?;
    budget.1 += 1;
    if meta.file_type().is_symlink()
        || !(meta.is_dir() || meta.is_file())
        || budget.1 > MAX_PACKAGE_ENTRIES
    {
        return Err("e2b_plugin_entry_unsupported".into());
    }
    if meta.is_dir() {
        fs::create_dir(destination).map_err(|_| "e2b_plugin_freeze_unavailable")?;
        for entry in fs::read_dir(source).map_err(|_| "e2b_plugin_freeze_unavailable")? {
            let entry = entry.map_err(|_| "e2b_plugin_freeze_unavailable")?;
            copy_package(&entry.path(), &destination.join(entry.file_name()), budget)?;
        }
    } else {
        budget.0 = budget
            .0
            .checked_add(meta.len())
            .ok_or("e2b_plugin_budget_exceeded")?;
        if budget.0 > MAX_PACKAGE_BYTES {
            return Err("e2b_plugin_budget_exceeded".into());
        }
        // Core validates the resulting copy before any SDK call.
        let file = fs::File::open(source).map_err(|_| "e2b_plugin_freeze_unavailable")?;
        let mut bounded = file.take(meta.len().saturating_add(1));
        let mut output =
            fs::File::create(destination).map_err(|_| "e2b_plugin_freeze_unavailable")?;
        let copied = std::io::copy(&mut bounded, &mut output)
            .map_err(|_| "e2b_plugin_freeze_unavailable")?;
        if copied != meta.len() {
            return Err("e2b_plugin_package_changed".into());
        }
        fs::set_permissions(destination, meta.permissions())
            .map_err(|_| "e2b_plugin_freeze_unavailable")?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::extension::build_plugin_activation_snapshot;
    #[test]
    fn freezes_complete_authorized_package_and_rejects_changed_activation() {
        let catalog = tempfile::tempdir().unwrap();
        let root = catalog.path().join("demo");
        fs::create_dir_all(root.join(".centaeris-plugin")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("resources")).unwrap();
        fs::write(
            root.join(".centaeris-plugin/plugin.json"),
            br#"{"name":"demo","version":"1.0.0","paths":{"cli":["bin/demo"]}}"#,
        )
        .unwrap();
        fs::write(root.join("bin/demo"), b"#!/bin/sh\necho useful").unwrap();
        fs::write(root.join("resources/data"), [0, 255, 65]).unwrap();
        let activation = build_plugin_activation_snapshot(std::slice::from_ref(&root)).unwrap();
        let projection = E2bPluginProjection::load_at(&activation, catalog.path()).unwrap();
        assert_eq!(
            projection.files["/opt/centaeris/plugins/demo/resources/data"].bytes,
            [0, 255, 65]
        );
        assert_eq!(
            projection.command_directories,
            ["/opt/centaeris/plugins/demo/bin"]
        );
        assert!(!projection.contains_path("/opt/centaeris/plugins/other/bin/cli"));
        assert!(!projection.contains_path("/opt/centaeris/plugins/demo/../other"));
        fs::write(root.join("resources/data"), b"changed").unwrap();
        assert!(E2bPluginProjection::load_at(&activation, catalog.path()).is_err());
        assert_eq!(
            projection.files["/opt/centaeris/plugins/demo/resources/data"].bytes,
            [0, 255, 65]
        );
    }
}
