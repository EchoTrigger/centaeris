use crate::{user_config, user_data_layout};
use centaeris_core::extension::skills::{
    add_skill_source, remove_skill_source, set_skill_enabled, set_skill_source_enabled,
    SkillCatalogLoadConfig, SkillCatalogRequest, SkillCatalogSnapshotV1, SkillDetailRequest,
    SkillDetailV1, SkillIndex, SkillSetEnabledRequest, SkillSourceAddRequest,
    SkillSourceConfigStore, SkillSourceConfigV1, SkillSourceIdRequest, SkillSourceKindV1,
    SkillSourceListRequest, SkillSourceRefV1, SkillSourceScopeV1, SkillSourceSetEnabledRequest,
    SkillSourcesConfigV1,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SYSTEM_SKILLS_SOURCE_ID: &str = "centaeris-system-skills";
static SKILL_STORE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn catalog(request: SkillCatalogRequest) -> Result<SkillCatalogSnapshotV1, String> {
    let _guard = skill_store_guard()?;
    Ok(load_index(request.cwd.as_deref())?.snapshot())
}

pub(crate) fn detail(request: SkillDetailRequest) -> Result<SkillDetailV1, String> {
    let _guard = skill_store_guard()?;
    load_index(request.cwd.as_deref())?.detail(request.skill_id.as_str())
}

pub(crate) fn list_sources(
    _request: SkillSourceListRequest,
) -> Result<SkillSourcesConfigV1, String> {
    let _guard = skill_store_guard()?;
    load_sources_config()
}

pub(crate) fn add_source(request: SkillSourceAddRequest) -> Result<SkillSourcesConfigV1, String> {
    let _guard = skill_store_guard()?;
    let store = config_store();
    let mut config = load_sources_config()?;
    add_skill_source(&mut config, request)?;
    store.save(&config)?;
    Ok(config)
}

pub(crate) fn remove_source(request: SkillSourceIdRequest) -> Result<SkillSourcesConfigV1, String> {
    let _guard = skill_store_guard()?;
    let store = config_store();
    let mut config = load_sources_config()?;
    remove_skill_source(&mut config, request.source_id.as_str())?;
    store.save(&config)?;
    Ok(config)
}

pub(crate) fn set_source_enabled(
    request: SkillSourceSetEnabledRequest,
) -> Result<SkillSourcesConfigV1, String> {
    let _guard = skill_store_guard()?;
    let store = config_store();
    let mut config = load_sources_config()?;
    set_skill_source_enabled(&mut config, request.source_id.as_str(), request.enabled)?;
    store.save(&config)?;
    Ok(config)
}

pub(crate) fn set_enabled(
    request: SkillSetEnabledRequest,
) -> Result<SkillCatalogSnapshotV1, String> {
    let _guard = skill_store_guard()?;
    let index = load_index(request.cwd.as_deref())?;
    let entry = index
        .find_by_id(request.skill_id.as_str())
        .ok_or_else(|| format!("skill not found: {}", request.skill_id))?;
    let store = config_store();
    let mut config = load_sources_config()?;
    set_skill_enabled(
        &mut config,
        entry.source_id.as_str(),
        entry.name.as_str(),
        request.enabled,
    )?;
    store.save(&config)?;
    Ok(load_index(request.cwd.as_deref())?.snapshot())
}

pub(crate) fn source_ref(request: SkillSourceIdRequest) -> Result<SkillSourceRefV1, String> {
    let _guard = skill_store_guard()?;
    let config = load_sources_config()?;
    let source = config
        .sources
        .into_iter()
        .find(|source| source.source_id == request.source_id)
        .ok_or_else(|| format!("skill source not found: {}", request.source_id))?;
    Ok(SkillSourceRefV1 {
        kind: "local_path".to_string(),
        path: source.path,
    })
}

pub(crate) fn skill_catalog_config_for_workspace_root(
    workspace_root: &Path,
) -> Result<SkillCatalogLoadConfig, String> {
    let _guard = skill_store_guard()?;
    Ok(SkillCatalogLoadConfig {
        cwd: Some(workspace_root.to_path_buf()),
        sources_config: load_sources_config()?,
        max_skills: 256,
    })
}

fn skill_store_guard() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    SKILL_STORE_LOCK
        .lock()
        .map_err(|_| "skill store lock poisoned".to_string())
}

fn load_index(cwd: Option<&str>) -> Result<SkillIndex, String> {
    let cwd = cwd
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    SkillIndex::load(SkillCatalogLoadConfig {
        cwd,
        sources_config: load_sources_config()?,
        max_skills: 256,
    })
}

fn load_sources_config() -> Result<SkillSourcesConfigV1, String> {
    let mut config = config_store().load()?;
    let path = user_data_layout::system_skills_dir();
    if !path.is_dir() {
        return Err(format!(
            "System Skills directory is unavailable: {}; restart the Centaeris host to repair the user data layout",
            path.display()
        ));
    }
    let path = fs::canonicalize(path.as_path())
        .map_err(|error| format!("resolve Electron system Skills directory failed: {error}"))?;
    let path = path
        .to_str()
        .ok_or_else(|| "Electron system Skills path must be valid UTF-8".to_string())?;
    merge_system_source(&mut config, path)?;
    Ok(config)
}

fn merge_system_source(config: &mut SkillSourcesConfigV1, path: &str) -> Result<(), String> {
    let source = SkillSourceConfigV1 {
        source_id: SYSTEM_SKILLS_SOURCE_ID.to_string(),
        scope: SkillSourceScopeV1::System,
        kind: SkillSourceKindV1::CatalogDirectory,
        path: path.to_string(),
        workspace_root: None,
        enabled: true,
    };
    if let Some(existing) = config
        .sources
        .iter_mut()
        .find(|item| item.source_id == SYSTEM_SKILLS_SOURCE_ID)
    {
        if existing.scope != SkillSourceScopeV1::System {
            return Err(format!(
                "reserved system Skill sourceId has invalid scope: {SYSTEM_SKILLS_SOURCE_ID}"
            ));
        }
        let enabled = existing.enabled;
        *existing = SkillSourceConfigV1 { enabled, ..source };
    } else {
        config.sources.push(source);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct UserSkillSourceConfigStore;

impl SkillSourceConfigStore for UserSkillSourceConfigStore {
    fn load(&self) -> Result<SkillSourcesConfigV1, String> {
        Ok(user_config::load()?.skills)
    }

    fn save(&self, skills: &SkillSourcesConfigV1) -> Result<(), String> {
        user_config::update(|config| {
            config.skills = skills.clone();
            Ok(())
        })
    }
}

fn config_store() -> UserSkillSourceConfigStore {
    UserSkillSourceConfigStore
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_source_is_fixed_while_its_enablement_is_preserved() {
        let mut config = SkillSourcesConfigV1::default();
        merge_system_source(&mut config, "C:/release-a/system-skills").expect("add source");
        config.sources[0].enabled = false;

        merge_system_source(&mut config, "C:/release-b/system-skills").expect("refresh source");

        assert_eq!(config.sources.len(), 1);
        assert_eq!(config.sources[0].source_id, SYSTEM_SKILLS_SOURCE_ID);
        assert_eq!(config.sources[0].scope, SkillSourceScopeV1::System);
        assert_eq!(config.sources[0].path, "C:/release-b/system-skills");
        assert!(!config.sources[0].enabled);
    }
}
