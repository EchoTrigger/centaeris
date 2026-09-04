use super::manifest::{
    load_plugin_manifest_file, resolve_plugin_resource_path, validate_plugin_name,
    validate_plugin_version, validate_relative_resource_path,
};
use super::types::{
    ActivatedPluginPackageV1, PluginActivationSnapshotV1, PluginCatalogStateV1, PluginDescriptorV1,
    PluginResourceDigestV1,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const PLUGIN_ACTIVATION_SNAPSHOT_SCHEMA_V1: &str = "plugin_activation_snapshot_v1";
const PLUGIN_MANIFEST_PATH: &str = ".centaeris-plugin/plugin.json";

pub fn plugin_catalog_state(descriptors: &[PluginDescriptorV1]) -> PluginCatalogStateV1 {
    let mut enabled_plugins = Vec::new();
    let mut disabled_plugins = Vec::new();
    for descriptor in descriptors {
        if descriptor.enabled && descriptor.errors.is_empty() {
            enabled_plugins.push(descriptor.id.clone());
        } else {
            disabled_plugins.push(descriptor.id.clone());
        }
    }
    enabled_plugins.sort();
    disabled_plugins.sort();
    PluginCatalogStateV1 {
        schema: "plugin_catalog_state_v1".to_string(),
        enabled_plugins,
        disabled_plugins,
    }
}

pub fn build_plugin_activation_snapshot(
    plugin_roots: &[PathBuf],
) -> Result<PluginActivationSnapshotV1, String> {
    let mut packages = plugin_roots
        .iter()
        .map(|root| resolve_plugin_package(root.as_path()))
        .collect::<Result<Vec<_>, _>>()?;
    packages.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.package_digest.cmp(&right.package_digest))
    });
    let mut names = HashSet::new();
    let mut cli_names = HashSet::new();
    for package in &packages {
        if !names.insert(package.name.as_str()) {
            return Err(format!("duplicate activated plugin name: {}", package.name));
        }
        for cli in &package.cli {
            let name = Path::new(cli.path.as_str())
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("plugin CLI identity is invalid: {}", cli.path))?;
            if !cli_names.insert(name.to_string()) {
                return Err(format!("duplicate plugin CLI identity: {name}"));
            }
        }
    }
    let digest = activation_digest(packages.as_slice())?;
    Ok(PluginActivationSnapshotV1 {
        schema: PLUGIN_ACTIVATION_SNAPSHOT_SCHEMA_V1.to_string(),
        digest,
        packages,
    })
}

pub fn resolve_plugin_package(root: &Path) -> Result<ActivatedPluginPackageV1, String> {
    let root = root.canonicalize().map_err(|error| {
        format!(
            "canonicalize plugin root failed {}: {error}",
            root.display()
        )
    })?;
    if !root.is_dir() {
        return Err(format!(
            "plugin root is not a directory: {}",
            root.display()
        ));
    }
    let manifest = load_plugin_manifest_file(root.join(PLUGIN_MANIFEST_PATH).as_path())?;
    if !manifest.paths.apps.is_empty() {
        return Err("unsupported_app_contribution".to_string());
    }
    let skills = resource_digests(root.as_path(), manifest.paths.skills.as_slice(), false)?;
    let cli = resource_digests(root.as_path(), manifest.paths.cli.as_slice(), true)?;
    let mcp_servers =
        resource_digests(root.as_path(), manifest.paths.mcp_servers.as_slice(), true)?;
    let hooks = resource_digests(root.as_path(), manifest.paths.hooks.as_slice(), true)?;
    for hook in &hooks {
        super::manifest::load_plugin_hooks_file(root.join(hook.path.as_str()).as_path())?;
    }
    Ok(ActivatedPluginPackageV1 {
        name: manifest.name,
        version: manifest.version,
        package_digest: tree_digest(root.as_path(), root.as_path())?,
        skills,
        cli,
        mcp_servers,
        hooks,
    })
}

pub fn validate_plugin_activation_snapshot(
    snapshot: &PluginActivationSnapshotV1,
) -> Result<(), String> {
    if snapshot.schema != PLUGIN_ACTIVATION_SNAPSHOT_SCHEMA_V1 {
        return Err("plugin activation snapshot schema mismatch".to_string());
    }
    require_sha256("plugin activation digest", snapshot.digest.as_str())?;
    if activation_digest(snapshot.packages.as_slice())? != snapshot.digest {
        return Err("plugin activation snapshot digest mismatch".to_string());
    }
    let mut sorted = snapshot.packages.clone();
    sorted.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.package_digest.cmp(&right.package_digest))
    });
    if sorted != snapshot.packages {
        return Err("plugin activation packages must be sorted".to_string());
    }
    let mut names = HashSet::new();
    let mut cli_names = HashSet::new();
    for package in &snapshot.packages {
        validate_plugin_name(package.name.as_str())?;
        validate_plugin_version(package.version.as_str())?;
        if !names.insert(package.name.as_str()) {
            return Err(format!("duplicate activated plugin name: {}", package.name));
        }
        require_sha256("plugin package digest", package.package_digest.as_str())?;
        validate_sorted_resources("skill", package.skills.as_slice())?;
        validate_sorted_resources("CLI", package.cli.as_slice())?;
        validate_sorted_resources("MCP server", package.mcp_servers.as_slice())?;
        validate_sorted_resources("hook", package.hooks.as_slice())?;
        for resource in package
            .skills
            .iter()
            .chain(package.cli.iter())
            .chain(package.mcp_servers.iter())
            .chain(package.hooks.iter())
        {
            validate_relative_resource_path("activation resource", resource.path.as_str())?;
            require_sha256("plugin resource digest", resource.digest.as_str())?;
        }
        for cli in &package.cli {
            let name = Path::new(cli.path.as_str())
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("plugin CLI identity is invalid: {}", cli.path))?;
            if !cli_names.insert(name.to_string()) {
                return Err(format!("duplicate plugin CLI identity: {name}"));
            }
        }
    }
    Ok(())
}

fn validate_sorted_resources(
    kind: &str,
    resources: &[PluginResourceDigestV1],
) -> Result<(), String> {
    if resources
        .windows(2)
        .any(|pair| pair[0].path >= pair[1].path)
    {
        return Err(format!("plugin {kind} resources must be sorted and unique"));
    }
    Ok(())
}

fn resource_digests(
    root: &Path,
    paths: &[String],
    require_file: bool,
) -> Result<Vec<PluginResourceDigestV1>, String> {
    let mut resources = Vec::with_capacity(paths.len());
    let mut seen = HashSet::new();
    for path in paths {
        if !seen.insert(path.as_str()) {
            return Err(format!("duplicate plugin resource path: {path}"));
        }
        let resolved = resolve_plugin_resource_path(root, path)?;
        if require_file && !resolved.is_file() {
            return Err(format!("plugin resource must be a file: {path}"));
        }
        resources.push(PluginResourceDigestV1 {
            path: path.clone(),
            digest: tree_digest(root, resolved.as_path())?,
        });
    }
    resources.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(resources)
}

fn activation_digest(packages: &[ActivatedPluginPackageV1]) -> Result<String, String> {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "schema": PLUGIN_ACTIVATION_SNAPSHOT_SCHEMA_V1,
        "packages": packages,
    }))
    .map_err(|error| format!("serialize plugin activation snapshot failed: {error}"))?;
    Ok(sha256(encoded.as_slice()))
}

fn tree_digest(package_root: &Path, resource: &Path) -> Result<String, String> {
    let mut files = Vec::new();
    collect_files(package_root, resource, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    digest.update(b"centaeris.plugin.tree.v1\0");
    let mut buffer = [0; 64 * 1024];
    for (path, native_path) in files {
        let mut file = fs::File::open(native_path.as_path())
            .map_err(|error| format!("read plugin resource failed {path}: {error}"))?;
        let before = file.metadata().map_err(|error| error.to_string())?;
        update_len_prefixed(&mut digest, path.as_bytes());
        digest.update(before.len().to_be_bytes());
        let mut bytes_read = 0_u64;
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("read plugin resource failed {path}: {error}"))?;
            if count == 0 {
                break;
            }
            bytes_read += count as u64;
            if bytes_read > before.len() {
                return Err(format!("plugin resource changed while hashing: {path}"));
            }
            digest.update(&buffer[..count]);
        }
        let after = file.metadata().map_err(|error| error.to_string())?;
        if bytes_read != before.len()
            || after.len() != before.len()
            || after.modified().ok() != before.modified().ok()
        {
            return Err(format!("plugin resource changed while hashing: {path}"));
        }
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn collect_files(
    package_root: &Path,
    current: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(current).map_err(|error| {
        format!(
            "inspect plugin resource failed {}: {error}",
            current.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "plugin package must not contain symlinks: {}",
            current.display()
        ));
    }
    if metadata.is_file() {
        let relative = current.strip_prefix(package_root).map_err(|_| {
            format!(
                "plugin resource escaped package root: {}",
                current.display()
            )
        })?;
        let path = relative
            .to_str()
            .ok_or_else(|| "plugin resource path must be UTF-8".to_string())?
            .replace('\\', "/");
        if path.nfc().collect::<String>() != path {
            return Err(format!("plugin resource path must be NFC: {path}"));
        }
        files.push((path, current.to_path_buf()));
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!(
            "plugin package contains unsupported entry: {}",
            current.display()
        ));
    }
    let mut children = fs::read_dir(current)
        .map_err(|error| {
            format!(
                "read plugin directory failed {}: {error}",
                current.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read plugin directory entry failed: {error}"))?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        collect_files(package_root, child.path().as_path(), files)?;
    }
    Ok(())
}

fn update_len_prefixed(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn require_sha256(name: &str, value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(format!("{name} must be sha256"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{name} must be lowercase sha256"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_resource_digest_preserves_length_prefixed_bytes() {
        let root = temp_plugin("streamed-digest");
        let resource = root.join("resource.bin");
        for size in [0, 1, 65_536, 65_537, 131_073] {
            let content = vec![0xa5; size];
            fs::write(&resource, &content).expect("resource");
            let mut expected = Sha256::new();
            expected.update(b"centaeris.plugin.tree.v1\0");
            update_len_prefixed(&mut expected, b"resource.bin");
            update_len_prefixed(&mut expected, &content);
            assert_eq!(
                tree_digest(&root, &resource).expect("streamed digest"),
                format!("sha256:{:x}", expected.finalize()),
            );
        }
        fs::remove_dir_all(root).expect("remove temporary plugin");
    }

    #[test]
    fn activation_is_stable_and_binds_package_bytes() {
        let root = temp_plugin("stable");
        write_package(root.as_path(), "first");
        let first =
            build_plugin_activation_snapshot(std::slice::from_ref(&root)).expect("first snapshot");
        let second =
            build_plugin_activation_snapshot(std::slice::from_ref(&root)).expect("second snapshot");
        assert_eq!(first, second);
        validate_plugin_activation_snapshot(&first).expect("valid snapshot");

        fs::write(root.join("skills/demo/SKILL.md"), skill("second")).expect("change skill");
        let changed = build_plugin_activation_snapshot(std::slice::from_ref(&root))
            .expect("changed snapshot");
        assert_ne!(first.digest, changed.digest);
        assert_ne!(
            first.packages[0].package_digest,
            changed.packages[0].package_digest
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_activation_is_stable() {
        let first = build_plugin_activation_snapshot(&[]).expect("empty snapshot");
        let second = build_plugin_activation_snapshot(&[]).expect("empty snapshot");
        assert_eq!(first, second);
        assert!(first.packages.is_empty());
    }

    #[test]
    fn unsupported_workspace_contributions_loud_fail() {
        let root = temp_plugin("app");
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("manifest root");
        fs::write(
            root.join(PLUGIN_MANIFEST_PATH),
            r#"{"name":"demo","version":"1.0.0","paths":{"apps":["contribution.json"]}}"#,
        )
        .expect("manifest");
        fs::write(root.join("contribution.json"), "{}").expect("contribution");
        assert_eq!(
            build_plugin_activation_snapshot(std::slice::from_ref(&root)).unwrap_err(),
            "unsupported_app_contribution"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activation_freezes_mcp_server_resources() {
        let root = temp_plugin("mcp");
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("manifest root");
        fs::create_dir_all(root.join("mcp")).expect("MCP root");
        fs::write(
            root.join(PLUGIN_MANIFEST_PATH),
            r#"{"name":"demo","version":"1.0.0","paths":{"mcpServers":["mcp/servers.json"]}}"#,
        )
        .expect("manifest");
        fs::write(
            root.join("mcp/servers.json"),
            r#"{"schema":"mcp_servers_v1"}"#,
        )
        .expect("MCP declaration");

        let first =
            build_plugin_activation_snapshot(std::slice::from_ref(&root)).expect("activation");
        assert_eq!(first.packages[0].mcp_servers.len(), 1);
        fs::write(
            root.join("mcp/servers.json"),
            r#"{"schema":"mcp_servers_v1","servers":[]}"#,
        )
        .expect("change MCP declaration");
        let changed =
            build_plugin_activation_snapshot(std::slice::from_ref(&root)).expect("changed");
        assert_ne!(first.digest, changed.digest);
        assert_ne!(
            first.packages[0].mcp_servers[0].digest,
            changed.packages[0].mcp_servers[0].digest
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn activation_freezes_hook_resources() {
        let root = temp_plugin("hooks");
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("manifest root");
        fs::create_dir_all(root.join("hooks")).expect("hook root");
        fs::write(
            root.join(PLUGIN_MANIFEST_PATH),
            r#"{"name":"demo","version":"1.0.0","paths":{"hooks":["hooks/hooks.json"]}}"#,
        )
        .expect("manifest");
        fs::write(
            root.join("hooks/hooks.json"),
            r#"{"schema":"plugin_hooks_v1","handlers":[]}"#,
        )
        .expect("hook declaration");

        let first =
            build_plugin_activation_snapshot(std::slice::from_ref(&root)).expect("activation");
        assert_eq!(first.packages[0].hooks.len(), 1);
        fs::write(
            root.join("hooks/hooks.json"),
            r#"{"schema":"plugin_hooks_v1","handlers":[{"id":"guard","event":"PreToolUse","matcher":"write","program":"node","args":["hooks/guard.mjs"]}]}"#,
        )
        .expect("change hook declaration");
        let changed =
            build_plugin_activation_snapshot(std::slice::from_ref(&root)).expect("changed");
        assert_ne!(first.digest, changed.digest);
        assert_ne!(
            first.packages[0].hooks[0].digest,
            changed.packages[0].hooks[0].digest
        );
        let _ = fs::remove_dir_all(root);
    }

    fn temp_plugin(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "centaeris-plugin-activation-{label}-{}-{}",
            std::process::id(),
            crate::runtime::contracts::current_timestamp_ms()
        ));
        fs::create_dir_all(root.as_path()).expect("plugin root");
        root
    }

    fn write_package(root: &Path, body: &str) {
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("manifest root");
        fs::create_dir_all(root.join("skills/demo")).expect("skill root");
        fs::create_dir_all(root.join("bin")).expect("bin root");
        fs::write(
            root.join(PLUGIN_MANIFEST_PATH),
            r#"{"name":"demo","version":"1.0.0","paths":{"skills":["skills"],"cli":["bin/demo"]}}"#,
        )
        .expect("manifest");
        fs::write(root.join("skills/demo/SKILL.md"), skill(body)).expect("skill");
        fs::write(root.join("bin/demo"), "#!/bin/sh\necho demo\n").expect("cli");
    }

    fn skill(body: &str) -> String {
        format!("---\nname: demo\ndescription: Demo skill\n---\n{body}\n")
    }
}
