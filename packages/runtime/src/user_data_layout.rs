use centaeris_core::runtime::contracts::current_timestamp_ms;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const LAYOUT_SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UserDataLayoutManifest {
    schema_version: u32,
    created_at_ms: i64,
}

pub(crate) fn ensure_user_data_layout() -> Result<(), String> {
    let data_root = desktop_data_root_dir();
    ensure_user_data_layout_at(data_root.as_path())
}

pub(crate) fn ensure_runtime_endpoint_layout() -> Result<(), String> {
    ensure_runtime_endpoint_layout_at(desktop_data_root_dir().as_path())
}

fn ensure_runtime_endpoint_layout_at(data_root: &Path) -> Result<(), String> {
    ensure_dir(data_root, "user data root")?;
    ensure_dir(data_root.join("runtime").as_path(), "runtime directory")
}

fn ensure_user_data_layout_at(data_root: &Path) -> Result<(), String> {
    ensure_dir(data_root, "user data root")?;
    for (path, label) in [
        (data_root.join("config"), "config directory"),
        (data_root.join("secrets"), "secrets directory"),
        (data_root.join("sessions"), "sessions directory"),
        (
            data_root.join("runtime").join("document"),
            "runtime document directory",
        ),
        (
            data_root.join("runtime").join("live-text"),
            "runtime live text journal directory",
        ),
        (
            data_root.join("runtime").join("inputs"),
            "runtime input directory",
        ),
        (data_root.join("plugins"), "plugins directory"),
        (
            data_root.join("skills").join("system"),
            "system skills directory",
        ),
    ] {
        ensure_dir(path.as_path(), label)?;
    }
    crate::user_config::ensure_at(data_root.join("config.toml").as_path())?;
    ensure_layout_manifest_at(data_root.join("layout.json").as_path())?;
    Ok(())
}

pub(crate) fn desktop_data_root_dir() -> PathBuf {
    if let Some(path_raw) = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR") {
        return PathBuf::from(path_raw);
    }
    if let Some(home_dir) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        return PathBuf::from(home_dir).join(".centaeris");
    }
    panic!("CENTAERIS_DESKTOP_DATA_DIR or USERPROFILE/HOME is required to resolve .centaeris user data root")
}

pub(crate) fn profile_identity() -> Result<String, String> {
    profile_identity_for(desktop_data_root_dir().as_path())
}

pub(crate) fn profile_identity_for(data_root: &Path) -> Result<String, String> {
    path_identity(data_root, "user data root")
}

pub(crate) fn runtime_store_identity() -> Result<String, String> {
    path_identity(runtime_store_db_path().as_path(), "runtime store")
}

fn path_identity(path: &Path, label: &str) -> Result<String, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("canonicalize {label} {} failed: {error}", path.display()))?;
    let mut digest = Sha256::new();
    digest.update(canonical.to_string_lossy().as_bytes());
    let hex = format!("{:x}", digest.finalize());
    Ok(hex[..16].to_string())
}

pub(crate) fn user_config_file_path() -> PathBuf {
    if let Some(path_raw) = std::env::var_os("CENTAERIS_CONFIG_PATH") {
        return PathBuf::from(path_raw);
    }
    desktop_data_root_dir().join("config.toml")
}

pub(crate) fn runtime_secret_file_path() -> PathBuf {
    secrets_dir_path().join("runtime-secrets.json")
}

pub(crate) fn mcp_credential_file_path() -> PathBuf {
    secrets_dir_path().join("mcp-credentials.json")
}

pub(crate) fn workspace_state_file_path() -> PathBuf {
    config_dir_path().join("workspace.json")
}

pub(crate) fn runtime_store_db_path() -> PathBuf {
    runtime_dir_path().join("runtime.sqlite3")
}

pub(crate) fn plugin_roots() -> Vec<PathBuf> {
    let mut roots = vec![plugins_dir_path()];
    if let Some(root) = bundled_native_plugin_root() {
        roots.push(root);
    }
    roots
}

fn bundled_native_plugin_root() -> Option<PathBuf> {
    let executable_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))?;
    for directory in executable_dir.ancestors() {
        let packaged = directory.join("native-plugins");
        if packaged.is_dir() {
            return Some(packaged);
        }
    }
    None
}

pub(crate) fn find_session_log_file_path(session_id: &str) -> Result<Option<PathBuf>, String> {
    let sessions_dir = sessions_dir_path();
    if !sessions_dir.exists() {
        return Ok(None);
    }
    let target_name = format!("{}.jsonl", sanitize_path_segment(session_id, "session"));
    let mut matches = Vec::new();
    for year_entry in fs::read_dir(sessions_dir.as_path())
        .map_err(|error| format!("read sessions year dir failed: {error}"))?
    {
        let year_entry =
            year_entry.map_err(|error| format!("read sessions year entry failed: {error}"))?;
        let year_path = year_entry.path();
        if !year_path.is_dir() {
            continue;
        }
        for month_entry in fs::read_dir(year_path.as_path())
            .map_err(|error| format!("read sessions month dir failed: {error}"))?
        {
            let month_entry = month_entry
                .map_err(|error| format!("read sessions month entry failed: {error}"))?;
            let month_path = month_entry.path();
            if !month_path.is_dir() {
                continue;
            }
            for day_entry in fs::read_dir(month_path.as_path())
                .map_err(|error| format!("read sessions day dir failed: {error}"))?
            {
                let day_entry = day_entry
                    .map_err(|error| format!("read sessions day entry failed: {error}"))?;
                let candidate = day_entry.path().join(target_name.as_str());
                if candidate.is_file() {
                    matches.push(candidate);
                }
            }
        }
    }
    matches.sort();
    matches.dedup();
    if matches.len() > 1 {
        return Err(format!(
            "multiple session logs found for sessionId={session_id}: {}",
            matches
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(matches.into_iter().next())
}

pub(crate) fn session_log_file_paths() -> Result<Vec<PathBuf>, String> {
    let sessions_dir = sessions_dir_path();
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }
    let mut file_paths = Vec::new();
    for year_entry in fs::read_dir(sessions_dir.as_path())
        .map_err(|error| format!("read sessions year dir failed: {error}"))?
    {
        let year_entry =
            year_entry.map_err(|error| format!("read sessions year entry failed: {error}"))?;
        let year_path = year_entry.path();
        if !year_path.is_dir() {
            continue;
        }
        for month_entry in fs::read_dir(year_path.as_path())
            .map_err(|error| format!("read sessions month dir failed: {error}"))?
        {
            let month_entry = month_entry
                .map_err(|error| format!("read sessions month entry failed: {error}"))?;
            let month_path = month_entry.path();
            if !month_path.is_dir() {
                continue;
            }
            for day_entry in fs::read_dir(month_path.as_path())
                .map_err(|error| format!("read sessions day dir failed: {error}"))?
            {
                let day_entry = day_entry
                    .map_err(|error| format!("read sessions day entry failed: {error}"))?;
                let day_path = day_entry.path();
                if !day_path.is_dir() {
                    continue;
                }
                for file_entry in fs::read_dir(day_path.as_path())
                    .map_err(|error| format!("read session log dir failed: {error}"))?
                {
                    let file_entry = file_entry
                        .map_err(|error| format!("read session log file entry failed: {error}"))?;
                    let path = file_entry.path();
                    if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                        file_paths.push(path);
                    }
                }
            }
        }
    }
    file_paths.sort();
    Ok(file_paths)
}

fn ensure_layout_manifest_at(file_path: &Path) -> Result<(), String> {
    if file_path.exists() {
        if !file_path.is_file() {
            return Err(format!(
                "layout manifest path is not a file: {}",
                file_path.display()
            ));
        }
        let raw = fs::read_to_string(file_path).map_err(|error| {
            format!(
                "read user data layout manifest failed for {}: {error}",
                file_path.display()
            )
        })?;
        let envelope =
            serde_json::from_str::<serde_json::Value>(raw.as_str()).map_err(|error| {
                format!(
                    "parse user data layout manifest failed for {}: {error}",
                    file_path.display()
                )
            })?;
        let schema_version = envelope
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                "user data layout schemaVersion must be an unsigned integer".to_string()
            })?;
        match schema_version {
            value if value == u64::from(LAYOUT_SCHEMA_VERSION) => {}
            value if value < u64::from(LAYOUT_SCHEMA_VERSION) => {
                return Err(format!(
                    "no user data layout forward migration exists from schemaVersion {value} to {LAYOUT_SCHEMA_VERSION}"
                ));
            }
            value => {
                return Err(format!(
                    "refusing user data layout downgrade from schemaVersion {value} to {LAYOUT_SCHEMA_VERSION}"
                ));
            }
        }
        serde_json::from_str::<UserDataLayoutManifest>(raw.as_str()).map_err(|error| {
            format!(
                "parse user data layout v{LAYOUT_SCHEMA_VERSION} manifest failed for {}: {error}",
                file_path.display()
            )
        })?;
        return Ok(());
    }
    let encoded = serde_json::to_string_pretty(&UserDataLayoutManifest {
        schema_version: LAYOUT_SCHEMA_VERSION,
        created_at_ms: current_timestamp_ms(),
    })
    .map_err(|error| format!("serialize user data layout manifest failed: {error}"))?;
    write_seed_file_if_missing(
        file_path,
        format!("{encoded}\n").as_str(),
        "layout manifest",
    )
}

fn config_dir_path() -> PathBuf {
    desktop_data_root_dir().join("config")
}

fn secrets_dir_path() -> PathBuf {
    desktop_data_root_dir().join("secrets")
}

pub(crate) fn sessions_dir_path() -> PathBuf {
    if let Some(path_raw) = std::env::var_os("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR") {
        return PathBuf::from(path_raw);
    }
    desktop_data_root_dir().join("sessions")
}

pub(crate) fn system_skills_dir() -> PathBuf {
    desktop_data_root_dir().join("skills").join("system")
}

fn runtime_dir_path() -> PathBuf {
    desktop_data_root_dir().join("runtime")
}

pub(crate) fn plugins_dir_path() -> PathBuf {
    desktop_data_root_dir().join("plugins")
}

pub(crate) fn runtime_live_text_journal_dir_path() -> PathBuf {
    runtime_dir_path().join("live-text")
}

pub(crate) fn runtime_inputs_dir_path() -> PathBuf {
    runtime_dir_path().join("inputs")
}

fn ensure_dir(path: &Path, label: &str) -> Result<(), String> {
    if path.exists() && !path.is_dir() {
        return Err(format!(
            "{label} path is not a directory: {}",
            path.display()
        ));
    }
    fs::create_dir_all(path).map_err(|error| format!("create {label} failed: {error}"))
}

fn write_seed_file_if_missing(path: &Path, content: &str, label: &str) -> Result<(), String> {
    if path.exists() {
        if path.is_file() {
            return Ok(());
        }
        return Err(format!("{label} path is not a file: {}", path.display()));
    }
    if let Some(parent) = path.parent() {
        ensure_dir(parent, "seed file parent")?;
    }
    fs::write(path, content).map_err(|error| format!("write {label} failed: {error}"))
}

pub(crate) fn sanitize_path_segment(value: &str, fallback: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.trim().is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn layout_creates_distinct_plugin_and_skill_directories() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-user-layout-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));

        ensure_user_data_layout_at(root.as_path()).expect("create user data layout");

        assert!(root.join("config").is_dir());
        assert!(root.join("plugins").is_dir());
        assert!(root.join("skills").is_dir());
        assert!(root.join("skills").join("system").is_dir());
        assert!(root.join("config.toml").is_file());
        assert_eq!(
            fs::read_dir(root.join("config"))
                .expect("read config directory")
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(root.join("skills"))
                .expect("read skills directory")
                .count(),
            1
        );

        fs::remove_dir_all(root).expect("remove test user data layout");
    }

    #[test]
    fn layout_schema_dispatch_rejects_unimplemented_upgrade_and_downgrade() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-user-layout-schema-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(root.as_path()).expect("create root");
        let manifest = root.join("layout.json");
        fs::write(&manifest, r#"{"schemaVersion":0,"createdAtMs":1}"#).expect("write old layout");
        assert!(ensure_layout_manifest_at(manifest.as_path())
            .expect_err("old layout")
            .contains("no user data layout forward migration exists"));
        fs::write(&manifest, r#"{"schemaVersion":2,"createdAtMs":1}"#)
            .expect("write future layout");
        assert!(ensure_layout_manifest_at(manifest.as_path())
            .expect_err("future layout")
            .contains("refusing user data layout downgrade"));
        fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn endpoint_layout_creates_only_data_root_and_runtime_directory() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-runtime-endpoint-layout-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        ensure_runtime_endpoint_layout_at(root.as_path()).expect("endpoint layout");
        assert!(root.join("runtime").is_dir());
        assert!(!root.join("layout.json").exists());
        assert!(!root.join("config.toml").exists());
        assert!(!root.join("sessions").exists());
        fs::remove_dir_all(root).expect("remove root");
    }
}
