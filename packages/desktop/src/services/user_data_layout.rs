use centaeris_core::runtime::contracts::current_timestamp_ms;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) const LAYOUT_SCHEMA_VERSION: u32 = 2;
static LAYOUT_MIGRATION_LOCK: Mutex<()> = Mutex::new(());

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
        (
            data_root.join("runtime").join("operation-receipts"),
            "runtime operation receipt directory",
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
    let _guard = crate::message_log::lock_session_logs_for_read()?;
    crate::session_catalog::path_unlocked(&sessions_dir_path(), session_id)
}

fn ensure_layout_manifest_at(file_path: &Path) -> Result<(), String> {
    let _guard = LAYOUT_MIGRATION_LOCK
        .lock()
        .map_err(|_| "layout migration lock poisoned")?;
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
            1 => return migrate_layout_v1(file_path, &raw),
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

// Version two prevents pre-catalog Runtime binaries from writing logs without
// recording dirty intents. Only Host layout metadata changes; logs stay intact.
fn migrate_layout_v1(file_path: &Path, raw: &str) -> Result<(), String> {
    let mut manifest: UserDataLayoutManifest = serde_json::from_str(raw)
        .map_err(|error| format!("parse user data layout v1 manifest failed: {error}"))?;
    let backup = file_path.with_file_name("layout.json.pre-v1-to-v2.backup");
    match fs::read(&backup) {
        Ok(bytes) if bytes == raw.as_bytes() => {}
        Ok(_) => return Err("layout migration backup differs from v1 source".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::atomic_file::write_file_atomically(
                &backup,
                raw.as_bytes(),
                "layout migration backup",
            )?;
        }
        Err(error) => return Err(format!("read layout migration backup failed: {error}")),
    }
    manifest.schema_version = LAYOUT_SCHEMA_VERSION;
    let encoded = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    crate::atomic_file::write_file_atomically(file_path, &encoded, "user data layout migration")
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

pub(crate) fn runtime_operation_receipts_dir_path() -> PathBuf {
    runtime_dir_path().join("operation-receipts")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn catalog_layout_migration_preserves_facts_and_blocks_legacy_writers() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-layout-catalog-upgrade-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join("layout.json");
        let old = br#"{"schemaVersion":1,"createdAtMs":42}"#;
        fs::write(&manifest, old).unwrap();
        let source = root.join("authoritative.jsonl");
        fs::write(&source, b"unchanged source").unwrap();
        ensure_layout_manifest_at(&manifest).unwrap();
        let upgraded: UserDataLayoutManifest =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(upgraded.schema_version, 2);
        assert_eq!(upgraded.created_at_ms, 42);
        assert_eq!(
            fs::read(root.join("layout.json.pre-v1-to-v2.backup")).unwrap(),
            old
        );
        assert_eq!(fs::read(&source).unwrap(), b"unchanged source");
        let bytes = fs::read(&manifest).unwrap();
        ensure_layout_manifest_at(&manifest).unwrap();
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
        // Resume a crash after backup publication but before manifest replacement.
        fs::write(&manifest, old).unwrap();
        ensure_layout_manifest_at(&manifest).unwrap();
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
        fs::write(&manifest, old).unwrap();
        fs::write(root.join("layout.json.pre-v1-to-v2.backup"), b"conflict").unwrap();
        assert!(ensure_layout_manifest_at(&manifest)
            .unwrap_err()
            .contains("backup differs"));
        assert_eq!(fs::read(&manifest).unwrap(), old);
        fs::write(root.join("layout.json.pre-v1-to-v2.backup"), old).unwrap();
        // The previous Runtime rejects any layout version greater than one.
        assert!(upgraded.schema_version > 1);
        fs::write(
            &manifest,
            br#"{"schemaVersion":1,"createdAtMs":42,"unknown":true}"#,
        )
        .unwrap();
        assert!(ensure_layout_manifest_at(&manifest).is_err());
        assert!(fs::read_to_string(&manifest).unwrap().contains("unknown"));
        fs::remove_dir_all(root).unwrap();
    }

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
        assert!(root.join("runtime").join("operation-receipts").is_dir());
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
        fs::write(&manifest, r#"{"schemaVersion":3,"createdAtMs":1}"#)
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
