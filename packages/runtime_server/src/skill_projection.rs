use std::path::{Path, PathBuf};

use centaeris_core::extension::skills::{
    SkillCatalogLoadConfig, SkillSourceConfigV1, SkillSourceKindV1, SkillSourceScopeV1,
    SkillSourcesConfigV1, SKILL_SOURCES_CONFIG_SCHEMA_V1,
};
use centaeris_core::extension::{
    build_plugin_activation_snapshot, validate_plugin_activation_snapshot,
    PluginActivationSnapshotV1,
};

pub const PLUGIN_CATALOG_ROOT: &str = "/opt/centaeris/plugins";
#[cfg(not(test))]
const SYSTEM_SKILL_CATALOG_ROOT: &str = "/opt/centaeris/system-skills";

pub fn workspace_skill_catalog_config(
    activation: &PluginActivationSnapshotV1,
) -> Result<SkillCatalogLoadConfig, String> {
    let system_skill_root = system_skill_catalog_root();
    workspace_skill_catalog_config_at(
        activation,
        Path::new(PLUGIN_CATALOG_ROOT),
        system_skill_root.as_path(),
    )
}

#[cfg(not(test))]
fn system_skill_catalog_root() -> PathBuf {
    PathBuf::from(SYSTEM_SKILL_CATALOG_ROOT)
}

#[cfg(test)]
fn system_skill_catalog_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/system")
}

fn workspace_skill_catalog_config_at(
    activation: &PluginActivationSnapshotV1,
    catalog_root: &Path,
    system_skill_root: &Path,
) -> Result<SkillCatalogLoadConfig, String> {
    let system_skill_root = system_skill_root
        .canonicalize()
        .map_err(|error| format!("canonicalize System Skill catalog root failed: {error}"))?;
    if !system_skill_root.is_dir() {
        return Err("System Skill catalog root is not a directory".to_string());
    }
    let package_roots = verified_plugin_package_roots_at(activation, catalog_root)?;
    let actual = build_plugin_activation_snapshot(package_roots.as_slice())?;

    let mut sources = vec![SkillSourceConfigV1 {
        source_id: "centaeris-workspace-system-skills".to_string(),
        scope: SkillSourceScopeV1::System,
        kind: SkillSourceKindV1::CatalogDirectory,
        path: system_skill_root.to_string_lossy().to_string(),
        workspace_root: None,
        enabled: true,
    }];
    for (package, package_root) in actual.packages.iter().zip(package_roots) {
        for (index, resource) in package.skills.iter().enumerate() {
            let path = package_root.join(resource.path.as_str());
            let kind = if path.is_file() {
                SkillSourceKindV1::SkillFile
            } else if path.is_dir() {
                SkillSourceKindV1::CatalogDirectory
            } else {
                return Err(format!(
                    "plugin skill resource is missing: {}",
                    resource.path
                ));
            };
            sources.push(SkillSourceConfigV1 {
                source_id: format!("plugin-{}-{index}", package.name),
                scope: SkillSourceScopeV1::Plugin,
                kind,
                path: path.to_string_lossy().to_string(),
                workspace_root: None,
                enabled: true,
            });
        }
    }
    Ok(SkillCatalogLoadConfig {
        cwd: None,
        sources_config: SkillSourcesConfigV1 {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources,
            skill_policies: Vec::new(),
        },
        ..SkillCatalogLoadConfig::default()
    })
}

pub(crate) fn verified_plugin_package_roots_at(
    activation: &PluginActivationSnapshotV1,
    catalog_root: &Path,
) -> Result<Vec<PathBuf>, String> {
    validate_plugin_activation_snapshot(activation)?;
    let package_roots = if activation.packages.is_empty() {
        Vec::new()
    } else {
        let catalog_root = catalog_root
            .canonicalize()
            .map_err(|error| format!("canonicalize Plugin catalog root failed: {error}"))?;
        activation
            .packages
            .iter()
            .map(|package| package_root(catalog_root.as_path(), package.name.as_str()))
            .collect::<Result<Vec<_>, _>>()?
    };
    let actual = build_plugin_activation_snapshot(package_roots.as_slice())?;
    if &actual != activation {
        return Err("plugin_activation_package_mismatch".to_string());
    }
    Ok(package_roots)
}

fn package_root(catalog_root: &Path, package_name: &str) -> Result<PathBuf, String> {
    let root = catalog_root
        .join(package_name)
        .canonicalize()
        .map_err(|error| {
            format!("canonicalize activated Plugin package failed {package_name}: {error}")
        })?;
    if root.parent() != Some(catalog_root) {
        return Err(format!(
            "activated Plugin package escaped catalog: {package_name}"
        ));
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::extension::build_plugin_activation_snapshot;
    use centaeris_core::extension::skills::{render_available_skills, SkillIndex};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn activated_package_projects_only_skill_metadata_and_model_readable_location() {
        let root = temp_root();
        let catalog = root.join("plugins");
        let system_skills = root.join("system-skills");
        fs::create_dir_all(catalog.as_path()).expect("Plugin catalog");
        write_system_skill(system_skills.as_path());
        let package = catalog.join("banana");
        write_package(package.as_path(), "Synthetic extension fixture.");
        let activation =
            build_plugin_activation_snapshot(std::slice::from_ref(&package)).expect("activation");

        let config = workspace_skill_catalog_config_at(
            &activation,
            catalog.as_path(),
            system_skills.as_path(),
        )
        .expect("skill projection");
        let index = SkillIndex::load(config).expect("skill index");
        let prompt = render_available_skills(&index, 8_000)
            .expect("render")
            .expect("skill metadata");
        assert!(prompt.contains("<name>banana</name>"));
        assert!(prompt.contains("<name>memory</name>"));
        assert!(prompt.contains("skills/banana/SKILL.md"));
        assert!(!prompt.contains("SECRET BODY"));
        assert!(!prompt.contains("MEMORY BODY"));

        fs::write(
            package.join("skills/banana/SKILL.md"),
            skill("Changed bytes."),
        )
        .expect("mutate package");
        assert_eq!(
            workspace_skill_catalog_config_at(
                &activation,
                catalog.as_path(),
                system_skills.as_path(),
            )
            .expect_err("stale activation"),
            "plugin_activation_package_mismatch"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn system_memory_skill_is_present_without_plugin_activation() {
        let root = temp_root();
        let catalog = root.join("plugins");
        let system_skills = root.join("system-skills");
        fs::create_dir_all(catalog.as_path()).expect("Plugin catalog");
        write_system_skill(system_skills.as_path());
        let activation = build_plugin_activation_snapshot(&[]).expect("empty activation");
        let config = workspace_skill_catalog_config_at(
            &activation,
            catalog.as_path(),
            system_skills.as_path(),
        )
        .expect("system skill projection");
        let index = SkillIndex::load(config).expect("skill index");
        assert_eq!(index.catalog_items().len(), 1);
        assert_eq!(index.catalog_items()[0].name, "memory");
        let _ = fs::remove_dir_all(root);
    }

    fn temp_root() -> PathBuf {
        static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "centaeris-runtime-plugin-{}-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(root.as_path()).expect("catalog root");
        root
    }

    fn write_system_skill(root: &Path) {
        let skill_root = root.join("memory");
        fs::create_dir_all(skill_root.as_path()).expect("System Skill root");
        fs::write(
            skill_root.join("SKILL.md"),
            "---\nname: memory\ndescription: Use relevant private memory.\n---\nMEMORY BODY\n",
        )
        .expect("System Skill");
    }

    fn write_package(root: &Path, body: &str) {
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("manifest root");
        fs::create_dir_all(root.join("skills/banana")).expect("skill root");
        fs::write(
            root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"banana","version":"1.0.0","paths":{"skills":["skills"]}}"#,
        )
        .expect("manifest");
        fs::write(root.join("skills/banana/SKILL.md"), skill(body)).expect("skill");
    }

    fn skill(body: &str) -> String {
        format!("---\nname: banana\ndescription: Synthetic extension fixture.\n---\nSECRET BODY\n{body}\n")
    }
}
