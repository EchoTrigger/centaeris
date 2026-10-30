use std::path::{Path, PathBuf};

use centaeris_core::extension::skills::{
    SkillCatalogLoadConfig, SkillSourceConfigV1, SkillSourceKindV1, SkillSourceScopeV1,
    SkillSourcesConfigV1, SKILL_SOURCES_CONFIG_SCHEMA_V1,
};
use centaeris_core::extension::{validate_plugin_activation_snapshot, PluginActivationSnapshotV1};

#[cfg(not(test))]
use centaeris_core::extension::build_plugin_activation_snapshot;

#[cfg(test)]
fn build_plugin_activation_snapshot(
    package_roots: &[PathBuf],
) -> Result<PluginActivationSnapshotV1, String> {
    tests::SNAPSHOT_BUILDS.with(|observed| {
        if let Some(builds) = observed.borrow_mut().as_mut() {
            builds.push(package_roots.to_vec());
        }
    });
    centaeris_core::extension::build_plugin_activation_snapshot(package_roots)
}

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

pub(crate) fn workspace_skill_catalog_config_at(
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
    // The verifier compared a freshly built package snapshot with this activation.

    let mut sources = vec![SkillSourceConfigV1 {
        source_id: "centaeris-workspace-system-skills".to_string(),
        scope: SkillSourceScopeV1::System,
        kind: SkillSourceKindV1::CatalogDirectory,
        path: system_skill_root.to_string_lossy().to_string(),
        workspace_root: None,
        enabled: true,
    }];
    for (package, package_root) in activation.packages.iter().zip(package_roots) {
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
            .map(|package| {
                package_root(
                    catalog_root.as_path(),
                    package.name.as_str(),
                    package.package_digest.as_str(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    let actual = build_plugin_activation_snapshot(package_roots.as_slice())?;
    if &actual != activation {
        return Err("plugin_activation_package_mismatch".to_string());
    }
    Ok(package_roots)
}

fn package_root(
    catalog_root: &Path,
    package_name: &str,
    package_digest: &str,
) -> Result<PathBuf, String> {
    // The activation is already validated. Tenant packages use their immutable
    // digest; the released platform layout remains the explicit shared source.
    let digest = package_digest
        .strip_prefix("sha256:")
        .ok_or("plugin_package_digest_invalid")?;
    let immutable = catalog_root
        .join(".packages")
        .join(digest)
        .join(package_name);
    for path in [
        catalog_root.join(".packages"),
        catalog_root.join(".packages").join(digest),
        immutable.clone(),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("plugin_package_symlink_rejected".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect Plugin package failed: {error}")),
        }
    }
    let candidate = if immutable.exists() {
        immutable
    } else {
        catalog_root.join(package_name)
    };
    let root = candidate.canonicalize().map_err(|error| {
        format!("canonicalize activated Plugin package failed {package_name}: {error}")
    })?;
    if !root.starts_with(catalog_root)
        || (root.parent() != Some(catalog_root)
            && root
                != catalog_root
                    .join(".packages")
                    .join(digest)
                    .join(package_name))
    {
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

    #[test]
    fn immutable_package_digest_resolves_independent_workspace_versions() {
        let root = temp_root();
        let catalog = root.join("plugins");
        fs::create_dir_all(&catalog).unwrap();
        let staged = root.join("candidate");
        write_package(&staged, "Workspace private skill bytes.");
        let activation = build_plugin_activation_snapshot(std::slice::from_ref(&staged)).unwrap();
        let hash = activation.packages[0]
            .package_digest
            .strip_prefix("sha256:")
            .unwrap();
        let immutable = catalog.join(".packages").join(hash).join("banana");
        fs::create_dir_all(immutable.parent().unwrap()).unwrap();
        fs::rename(&staged, &immutable).unwrap();
        write_package(&catalog.join("banana"), "Platform shared skill bytes.");
        let roots = verified_plugin_package_roots_at(&activation, &catalog).unwrap();
        assert_eq!(roots, vec![immutable.canonicalize().unwrap()]);
        fs::write(immutable.join("skills/banana/SKILL.md"), skill("tampered")).unwrap();
        assert_eq!(
            verified_plugin_package_roots_at(&activation, &catalog).unwrap_err(),
            "plugin_activation_package_mismatch"
        );
        let _ = fs::remove_dir_all(root);
    }

    thread_local! {
        pub(super) static SNAPSHOT_BUILDS: std::cell::RefCell<Option<Vec<Vec<PathBuf>>>> = const { std::cell::RefCell::new(None) };
    }

    fn observe_snapshot_builds<T>(action: impl FnOnce() -> T) -> (T, Vec<Vec<PathBuf>>) {
        SNAPSHOT_BUILDS.with(|observed| *observed.borrow_mut() = Some(Vec::new()));
        let result = action();
        let builds = SNAPSHOT_BUILDS.with(|observed| observed.borrow_mut().take().unwrap());
        (result, builds)
    }

    struct ProjectionFixture {
        root: PathBuf,
        catalog: PathBuf,
        system: PathBuf,
    }

    impl ProjectionFixture {
        fn new() -> Self {
            let root = temp_root();
            let catalog = root.join("plugins");
            let system = root.join("system-skills");
            fs::create_dir_all(&catalog).expect("Plugin catalog");
            write_system_skill(&system);
            Self {
                root,
                catalog,
                system,
            }
        }
    }

    impl Drop for ProjectionFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn projection_preserves_system_package_and_resource_order() {
        let fixture = ProjectionFixture::new();
        let roots = ["zeta", "banana"].map(|name| {
            let root = fixture.catalog.join(name);
            fs::create_dir_all(root.join(".centaeris-plugin")).expect("manifest root");
            fs::create_dir_all(root.join("skills/beta")).expect("directory skill");
            fs::create_dir_all(root.join("skills/alpha")).expect("file skill");
            fs::write(root.join("skills/alpha/SKILL.md"), skill("file resource"))
                .expect("file resource");
            fs::write(
                root.join("skills/beta/SKILL.md"),
                skill("directory resource"),
            )
            .expect("directory resource");
            let manifest = serde_json::json!({
                "name": name,
                "version": "1.0.0",
                "paths": { "skills": ["skills/beta", "skills/alpha/SKILL.md"] },
            });
            fs::write(
                root.join(".centaeris-plugin/plugin.json"),
                manifest.to_string(),
            )
            .expect("manifest");
            root
        });
        let activation = build_plugin_activation_snapshot(&roots).expect("activation");
        let config =
            workspace_skill_catalog_config_at(&activation, &fixture.catalog, &fixture.system)
                .expect("projection");
        let sources = config.sources_config.sources;
        assert_eq!(sources.len(), 5);
        assert_eq!(
            config.sources_config.schema_version,
            SKILL_SOURCES_CONFIG_SCHEMA_V1
        );
        assert!(config.sources_config.skill_policies.is_empty());
        assert!(config.cwd.is_none());
        let catalog = fixture.catalog.canonicalize().expect("canonical catalog");
        let expected = [
            (
                "centaeris-workspace-system-skills",
                SkillSourceScopeV1::System,
                SkillSourceKindV1::CatalogDirectory,
                fixture.system.canonicalize().unwrap(),
            ),
            (
                "plugin-banana-0",
                SkillSourceScopeV1::Plugin,
                SkillSourceKindV1::SkillFile,
                catalog.join("banana/skills/alpha/SKILL.md"),
            ),
            (
                "plugin-banana-1",
                SkillSourceScopeV1::Plugin,
                SkillSourceKindV1::CatalogDirectory,
                catalog.join("banana/skills/beta"),
            ),
            (
                "plugin-zeta-0",
                SkillSourceScopeV1::Plugin,
                SkillSourceKindV1::SkillFile,
                catalog.join("zeta/skills/alpha/SKILL.md"),
            ),
            (
                "plugin-zeta-1",
                SkillSourceScopeV1::Plugin,
                SkillSourceKindV1::CatalogDirectory,
                catalog.join("zeta/skills/beta"),
            ),
        ];
        for (source, (id, scope, kind, path)) in sources.iter().zip(expected) {
            assert_eq!(source.source_id, id);
            assert_eq!(source.scope, scope);
            assert_eq!(source.kind, kind);
            assert_eq!(source.path, path.to_string_lossy());
            assert!(source.workspace_root.is_none());
            assert!(source.enabled);
        }
    }

    #[test]
    fn projection_reverifies_package_bytes_on_each_call() {
        let fixture = ProjectionFixture::new();
        let package = fixture.catalog.join("banana");
        write_package(&package, "accepted skill bytes");
        let activation =
            build_plugin_activation_snapshot(std::slice::from_ref(&package)).expect("activation");
        workspace_skill_catalog_config_at(&activation, &fixture.catalog, &fixture.system)
            .expect("first projection");
        // A non-skill file is still covered by the package digest.
        fs::write(package.join("README.md"), "changed package bytes").expect("mutate package");
        assert_eq!(
            workspace_skill_catalog_config_at(&activation, &fixture.catalog, &fixture.system)
                .expect_err("every projection must reverify current package bytes"),
            "plugin_activation_package_mismatch",
        );
        assert_eq!(
            verified_plugin_package_roots_at(&activation, &fixture.catalog)
                .expect_err("other callers must retain package verification"),
            "plugin_activation_package_mismatch",
        );
    }

    #[test]
    fn projection_builds_one_verified_snapshot() {
        let fixture = ProjectionFixture::new();
        let package = fixture.catalog.join("banana");
        write_package(&package, "accepted skill bytes");
        let activation =
            build_plugin_activation_snapshot(std::slice::from_ref(&package)).expect("activation");
        let (result, builds) = observe_snapshot_builds(|| {
            workspace_skill_catalog_config_at(&activation, &fixture.catalog, &fixture.system)
        });
        let config = result.expect("projection");
        assert_eq!(config.sources_config.sources.len(), 2);
        assert_eq!(
            builds,
            vec![vec![package.canonicalize().unwrap()]],
            "projection must construct and verify the current package snapshot once"
        );
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
