use crate::{atomic_file::write_file_atomically, sessions, user_data_layout};
use base64::{engine::general_purpose, Engine as _};
use centaeris_core::runtime::contracts::current_timestamp_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const WORKSPACE_READ_FILE_MAX_BYTES: u64 = 2 * 1024 * 1024;
const WORKSPACE_CATALOG_CORRUPT: &str = "workspace_catalog_corrupt";
const WORKSPACE_CATALOG_IO: &str = "workspace_catalog_io";
const WORKSPACE_CATALOG_PATH_INVALID: &str = "workspace_catalog_path_invalid";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceFileTreeRequest {
    pub(crate) session_id: Option<String>,
    pub(crate) workspace_root: String,
    pub(crate) max_depth: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceReadFileRequest {
    pub(crate) session_id: Option<String>,
    pub(crate) task_id: Option<String>,
    pub(crate) workspace_root: Option<String>,
    pub(crate) path: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceRenameRequest {
    pub(crate) root: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceRootRequest {
    pub(crate) root: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedWorkspaceItem {
    root: Option<String>,
    display_name: Option<String>,
    active_session_id: Option<String>,
    active_session_selected_at_ms: Option<i64>,
    sort_order: Option<i64>,
    updated_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedWorkspaceState {
    active_workspace_root: Option<String>,
    workspaces: Vec<PersistedWorkspaceItem>,
    updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceInfoResponse {
    pub(crate) root: String,
    pub(crate) name: String,
    pub(crate) active_session_id: Option<String>,
    pub(crate) sort_order: i64,
    pub(crate) updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceSnapshotResponse {
    pub(crate) active_workspace_root: Option<String>,
    pub(crate) workspaces: Vec<WorkspaceInfoResponse>,
    pub(crate) cancelled: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceRemoveResponse {
    pub(crate) removed: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceCatalogResetRequest {
    pub(crate) confirm: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceCatalogResetResponse {
    pub(crate) snapshot: WorkspaceSnapshotResponse,
    pub(crate) quarantined_path: String,
}

#[derive(Debug)]
enum WorkspaceCatalogError {
    Corrupt(String),
    Io(String),
    PathInvalid(String),
}

impl WorkspaceCatalogError {
    fn message(&self) -> String {
        match self {
            Self::Corrupt(message) => format!("{WORKSPACE_CATALOG_CORRUPT}: {message}"),
            Self::Io(message) => format!("{WORKSPACE_CATALOG_IO}: {message}"),
            Self::PathInvalid(message) => {
                format!("{WORKSPACE_CATALOG_PATH_INVALID}: {message}")
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceFileTreeEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) is_directory: bool,
    pub(crate) children: Vec<WorkspaceFileTreeEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceFileTreeResponse {
    pub(crate) root: String,
    pub(crate) entries: Vec<WorkspaceFileTreeEntry>,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceReadFileResponse {
    pub(crate) root: String,
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) content: String,
    pub(crate) byte_len: u64,
    pub(crate) encoding: String,
    pub(crate) content_kind: String,
    pub(crate) mime_type: Option<String>,
    pub(crate) data_url: Option<String>,
}

struct ResolvedPreviewFilePath {
    root: PathBuf,
    path: PathBuf,
    response_path: String,
}

pub(crate) fn get() -> Result<WorkspaceSnapshotResponse, String> {
    let state = read_workspace_state()?;
    let cancelled = read_configured_workspace_root(&state)?.is_none();
    to_workspace_snapshot_response(cancelled, &state)
}

pub(crate) fn activate(request: WorkspaceRootRequest) -> Result<WorkspaceSnapshotResponse, String> {
    let root = resolve_cwd_from_request(request.root.as_str())?;
    let state = activate_workspace_root(root.as_path())?;
    to_workspace_snapshot_response(false, &state)
}

pub(crate) fn open_folder() -> Result<WorkspaceSnapshotResponse, String> {
    Err(String::from(
        "workspace_open_folder requires Electron native dialog ownership",
    ))
}

pub(crate) fn reveal_folder(
    request: WorkspaceRootRequest,
) -> Result<WorkspaceSnapshotResponse, String> {
    let root = resolve_cwd_from_request(request.root.as_str())?;
    #[cfg(target_os = "windows")]
    let status = Command::new("explorer.exe")
        .arg(root.as_os_str())
        .status()
        .map_err(|error| format!("open workspace in explorer failed: {error}"))?;
    #[cfg(target_os = "macos")]
    let status = Command::new("open")
        .arg(root.as_os_str())
        .status()
        .map_err(|error| format!("open workspace in finder failed: {error}"))?;
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let status = Command::new("xdg-open")
        .arg(root.as_os_str())
        .status()
        .map_err(|error| format!("open workspace in file manager failed: {error}"))?;

    if !status.success() {
        return Err(format!(
            "open workspace file manager failed status={status}"
        ));
    }
    get()
}

pub(crate) fn rename(request: WorkspaceRenameRequest) -> Result<WorkspaceSnapshotResponse, String> {
    let next_name = request.name.trim();
    if next_name.is_empty() {
        return Err(String::from("workspace name cannot be empty"));
    }
    let root = resolve_cwd_from_request(request.root.as_str())?;
    let mut state = read_workspace_state()?;
    let item = upsert_workspace_state_item(&mut state, root.as_path());
    item.display_name = Some(next_name.to_string());
    item.updated_at = current_timestamp_ms();
    state.updated_at = current_timestamp_ms();
    persist_workspace_state(&state)?;
    to_workspace_snapshot_response(false, &state)
}

pub(crate) fn remove(request: WorkspaceRootRequest) -> Result<WorkspaceRemoveResponse, String> {
    let root = resolve_cwd_from_request(request.root.as_str())?;
    let root_key = normalized_workspace_root_key_from_path(root.as_path());
    let mut state = read_workspace_state()?;
    let previous_len = state.workspaces.len();
    state
        .workspaces
        .retain(|item| !workspace_item_matches_root(item, root.as_path()));
    if state.active_workspace_root.as_deref() == Some(root_key.as_str()) {
        state.active_workspace_root = state
            .workspaces
            .iter()
            .find_map(workspace_item_root)
            .map(|item_root| normalized_workspace_root_key_from_path(item_root.as_path()));
    }
    state.updated_at = current_timestamp_ms();
    persist_workspace_state(&state)?;
    Ok(WorkspaceRemoveResponse {
        removed: state.workspaces.len() != previous_len,
    })
}

pub(crate) fn reset_catalog(
    request: WorkspaceCatalogResetRequest,
) -> Result<WorkspaceCatalogResetResponse, String> {
    if !request.confirm {
        return Err("workspace catalog reset requires confirm=true".to_string());
    }
    reset_corrupt_workspace_catalog_at(workspace_state_file_path().as_path())
}

pub(crate) fn runtime_host_error_code(error: &str) -> &'static str {
    if error.starts_with(WORKSPACE_CATALOG_CORRUPT) {
        WORKSPACE_CATALOG_CORRUPT
    } else if error.starts_with(WORKSPACE_CATALOG_IO) {
        WORKSPACE_CATALOG_IO
    } else if error.starts_with(WORKSPACE_CATALOG_PATH_INVALID) {
        WORKSPACE_CATALOG_PATH_INVALID
    } else {
        "workspace_failed"
    }
}

pub(crate) fn file_tree(
    request: Option<WorkspaceFileTreeRequest>,
) -> Result<WorkspaceFileTreeResponse, String> {
    let request =
        request.ok_or_else(|| "workspace file access requires a workspace session".to_string())?;
    let root = resolve_cwd_from_request(request.workspace_root.as_str())?;
    validate_workspace_file_access(request.session_id.as_deref(), root.as_path())?;
    let max_depth = request.max_depth.unwrap_or(12).clamp(1, 32);
    let entries = list_workspace_file_entries(root.as_path(), root.as_path(), 1, max_depth)?;
    Ok(WorkspaceFileTreeResponse {
        root: display_path(root.as_path()),
        entries,
        truncated: false,
    })
}

pub(crate) fn read_file(
    request: WorkspaceReadFileRequest,
) -> Result<WorkspaceReadFileResponse, String> {
    if request
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Err(String::from(
            "task runtime file access requires agent task migration before Local Runtime Host ownership",
        ));
    }
    let workspace_root = request
        .workspace_root
        .as_deref()
        .ok_or_else(|| "workspaceRoot is required for workspace file access".to_string())?;
    let root = resolve_cwd_from_request(workspace_root)?;
    validate_workspace_file_access(request.session_id.as_deref(), root.as_path())?;
    let resolved = resolve_workspace_preview_file_path(root.as_path(), request.path.as_str())?;
    read_preview_file(resolved, request.path.as_str())
}

fn resolve_cwd_from_request(raw_root: &str) -> Result<PathBuf, String> {
    let normalized_root = raw_root.trim();
    if normalized_root.is_empty() {
        return Err(String::from("workspaceRoot is required"));
    }
    normalize_workspace_root(PathBuf::from(normalized_root))
        .ok_or_else(|| "workspace root is not a directory".to_string())
}

fn normalize_workspace_root(path: PathBuf) -> Option<PathBuf> {
    if path.is_dir() {
        return fs::canonicalize(path.as_path()).ok();
    }
    None
}

fn read_workspace_state() -> Result<PersistedWorkspaceState, String> {
    read_persisted_workspace_state_at(workspace_state_file_path().as_path())
        .map_err(|error| error.message())
}

fn read_persisted_workspace_state_at(
    file_path: &Path,
) -> Result<PersistedWorkspaceState, WorkspaceCatalogError> {
    let raw = match fs::read_to_string(file_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedWorkspaceState::default());
        }
        Err(error) => {
            return Err(WorkspaceCatalogError::Io(format!(
                "read {} failed: {error}",
                file_path.display()
            )));
        }
    };
    let value = serde_json::from_str::<serde_json::Value>(raw.as_str()).map_err(|error| {
        WorkspaceCatalogError::Corrupt(format!("parse {} failed: {error}", file_path.display()))
    })?;
    require_exact_object_keys(
        &value,
        &["activeWorkspaceRoot", "workspaces", "updatedAt"],
        "workspace catalog",
    )?;
    let items = value
        .get("workspaces")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            WorkspaceCatalogError::Corrupt(
                "workspace catalog workspaces must be an array".to_string(),
            )
        })?;
    for (index, item) in items.iter().enumerate() {
        require_exact_object_keys(
            item,
            &[
                "root",
                "displayName",
                "activeSessionId",
                "activeSessionSelectedAtMs",
                "sortOrder",
                "updatedAt",
            ],
            format!("workspace catalog workspaces[{index}]").as_str(),
        )?;
    }
    let mut state = serde_json::from_value::<PersistedWorkspaceState>(value).map_err(|error| {
        WorkspaceCatalogError::Corrupt(format!("decode {} failed: {error}", file_path.display()))
    })?;
    validate_persisted_workspace_state(&mut state)?;
    Ok(state)
}

fn persist_workspace_state(state: &PersistedWorkspaceState) -> Result<(), String> {
    persist_workspace_state_at(workspace_state_file_path().as_path(), state)
}

fn persist_workspace_state_at(
    file_path: &Path,
    state: &PersistedWorkspaceState,
) -> Result<(), String> {
    let mut persisted = state.clone();
    validate_persisted_workspace_state(&mut persisted).map_err(|error| error.message())?;
    let encoded = serde_json::to_string_pretty(&persisted)
        .map_err(|error| format!("serialize workspace state failed: {error}"))?;
    write_file_atomically(file_path, encoded.as_bytes(), "workspace catalog")
        .map_err(|error| format!("{WORKSPACE_CATALOG_IO}: {error}"))
}

fn require_exact_object_keys(
    value: &serde_json::Value,
    expected_keys: &[&str],
    label: &str,
) -> Result<(), WorkspaceCatalogError> {
    let object = value
        .as_object()
        .ok_or_else(|| WorkspaceCatalogError::Corrupt(format!("{label} must be an object")))?;
    for expected in expected_keys {
        if !object.contains_key(*expected) {
            return Err(WorkspaceCatalogError::Corrupt(format!(
                "{label} missing field {expected}"
            )));
        }
    }
    for actual in object.keys() {
        if !expected_keys.contains(&actual.as_str()) {
            return Err(WorkspaceCatalogError::Corrupt(format!(
                "{label} unknown field {actual}"
            )));
        }
    }
    Ok(())
}

fn validate_persisted_workspace_state(
    state: &mut PersistedWorkspaceState,
) -> Result<(), WorkspaceCatalogError> {
    if state.updated_at < 0 {
        return Err(WorkspaceCatalogError::Corrupt(
            "workspace catalog updatedAt must be non-negative".to_string(),
        ));
    }
    let mut roots = HashSet::new();
    let mut sort_orders = HashSet::new();
    for (index, item) in state.workspaces.iter_mut().enumerate() {
        let raw_root = item.root.as_deref().ok_or_else(|| {
            WorkspaceCatalogError::Corrupt(format!(
                "workspace catalog workspaces[{index}].root must be a string"
            ))
        })?;
        let root =
            canonical_workspace_root(raw_root, format!("workspaces[{index}].root").as_str())?;
        let root_key = display_path(root.as_path());
        if !roots.insert(root_key.clone()) {
            return Err(WorkspaceCatalogError::Corrupt(format!(
                "workspace catalog contains duplicate root {root_key}"
            )));
        }
        item.root = Some(root_key);
        if item.updated_at < 0 {
            return Err(WorkspaceCatalogError::Corrupt(format!(
                "workspace catalog workspaces[{index}].updatedAt must be non-negative"
            )));
        }
        let sort_order = item.sort_order.ok_or_else(|| {
            WorkspaceCatalogError::Corrupt(format!(
                "workspace catalog workspaces[{index}].sortOrder must be an integer"
            ))
        })?;
        if sort_order < 0 || !sort_orders.insert(sort_order) {
            return Err(WorkspaceCatalogError::Corrupt(format!(
                "workspace catalog workspaces[{index}].sortOrder must be unique and non-negative"
            )));
        }
        validate_optional_non_empty(item.display_name.as_deref(), "displayName", index)?;
        validate_optional_non_empty(item.active_session_id.as_deref(), "activeSessionId", index)?;
        match (
            item.active_session_id.as_ref(),
            item.active_session_selected_at_ms,
        ) {
            (Some(_), Some(value)) if value >= 0 => {}
            (None, None) => {}
            _ => {
                return Err(WorkspaceCatalogError::Corrupt(format!(
                    "workspace catalog workspaces[{index}] active session identity is incomplete"
                )));
            }
        }
    }
    if let Some(raw_active_root) = state.active_workspace_root.as_deref() {
        let active_root = canonical_workspace_root(raw_active_root, "activeWorkspaceRoot")?;
        let active_root_key = display_path(active_root.as_path());
        if !roots.contains(active_root_key.as_str()) {
            return Err(WorkspaceCatalogError::Corrupt(
                "workspace catalog activeWorkspaceRoot is not in workspaces".to_string(),
            ));
        }
        state.active_workspace_root = Some(active_root_key);
    }
    Ok(())
}

fn canonical_workspace_root(raw_root: &str, label: &str) -> Result<PathBuf, WorkspaceCatalogError> {
    let trimmed = raw_root.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceCatalogError::Corrupt(format!(
            "workspace catalog {label} cannot be empty"
        )));
    }
    let path = PathBuf::from(trimmed);
    if !path.is_dir() {
        return Err(WorkspaceCatalogError::PathInvalid(format!(
            "workspace catalog {label} is not an existing directory: {trimmed}"
        )));
    }
    fs::canonicalize(path.as_path()).map_err(|error| {
        WorkspaceCatalogError::PathInvalid(format!(
            "canonicalize workspace catalog {label} failed: {error}"
        ))
    })
}

fn validate_optional_non_empty(
    value: Option<&str>,
    field: &str,
    index: usize,
) -> Result<(), WorkspaceCatalogError> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        return Err(WorkspaceCatalogError::Corrupt(format!(
            "workspace catalog workspaces[{index}].{field} cannot be empty"
        )));
    }
    Ok(())
}

fn workspace_state_file_path() -> PathBuf {
    user_data_layout::workspace_state_file_path()
}

fn reset_corrupt_workspace_catalog_at(
    file_path: &Path,
) -> Result<WorkspaceCatalogResetResponse, String> {
    match read_persisted_workspace_state_at(file_path) {
        Err(WorkspaceCatalogError::Corrupt(_)) => {}
        Err(error) => return Err(error.message()),
        Ok(_) => {
            return Err(
                "workspace catalog reset is only allowed for JSON or schema corruption".to_string(),
            );
        }
    }
    let parent = file_path
        .parent()
        .ok_or_else(|| "workspace catalog path has no parent".to_string())?;
    let quarantined_path = (0..1_000i64)
        .map(|offset| {
            parent.join(format!(
                "workspace.corrupt-{}.json",
                current_timestamp_ms().saturating_add(offset)
            ))
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| "workspace catalog quarantine path is unavailable".to_string())?;
    fs::rename(file_path, quarantined_path.as_path()).map_err(|error| {
        format!("{WORKSPACE_CATALOG_IO}: quarantine workspace catalog failed: {error}")
    })?;
    let state = PersistedWorkspaceState::default();
    persist_workspace_state_at(file_path, &state)?;
    Ok(WorkspaceCatalogResetResponse {
        snapshot: to_workspace_snapshot_response(true, &state)?,
        quarantined_path: display_path(quarantined_path.as_path()),
    })
}

fn read_configured_workspace_root(
    state: &PersistedWorkspaceState,
) -> Result<Option<PathBuf>, String> {
    if let Some(path_raw) = std::env::var_os("CENTAERIS_WORKSPACE_ROOT") {
        return normalize_workspace_root(PathBuf::from(path_raw))
            .map(Some)
            .ok_or_else(|| {
                "CENTAERIS_WORKSPACE_ROOT is not an existing canonical directory".to_string()
            });
    }
    Ok(active_workspace_root_from_state(state))
}

fn active_workspace_root_from_state(state: &PersistedWorkspaceState) -> Option<PathBuf> {
    let active_root = state
        .active_workspace_root
        .as_deref()
        .and_then(normalize_workspace_root_text)
        .and_then(|root| normalize_workspace_root(PathBuf::from(root)));
    active_root.or_else(|| state.workspaces.iter().find_map(workspace_item_root))
}

pub(crate) fn normalize_workspace_root_text(raw_root: &str) -> Option<String> {
    let normalized = normalize_workspace_root(PathBuf::from(raw_root.trim()))?;
    Some(display_path(normalized.as_path()))
}

fn workspace_item_root(item: &PersistedWorkspaceItem) -> Option<PathBuf> {
    item.root
        .as_deref()
        .and_then(normalize_workspace_root_text)
        .and_then(|root| normalize_workspace_root(PathBuf::from(root)))
}

pub(crate) fn bind_active_session_to_workspace_selected_at(
    session_id: &str,
    workspace_root: Option<&str>,
    selected_at_ms: i64,
    reject_stale: bool,
) -> Result<bool, String> {
    let normalized_session_id = session_id.trim();
    if normalized_session_id.is_empty() {
        return Ok(false);
    }
    let root = workspace_root
        .and_then(normalize_workspace_root_text)
        .ok_or_else(|| "workspaceRoot is required to bind active workspace session".to_string())?;
    let normalized_root = normalize_workspace_root(PathBuf::from(root))
        .ok_or_else(|| "workspace root is not a directory".to_string())?;
    let mut state = read_workspace_state()?;
    let item = upsert_workspace_state_item(&mut state, normalized_root.as_path());
    if reject_stale {
        if let Some(current_selected_at_ms) = item.active_session_selected_at_ms {
            if selected_at_ms < current_selected_at_ms {
                return Ok(false);
            }
        }
    }
    item.active_session_id = Some(normalized_session_id.to_string());
    item.active_session_selected_at_ms = Some(selected_at_ms);
    state.updated_at = current_timestamp_ms();
    persist_workspace_state(&state)?;
    Ok(true)
}

pub(crate) fn unbind_deleted_session(session_id: &str) -> Result<(), String> {
    let normalized_session_id = session_id.trim();
    if normalized_session_id.is_empty() {
        return Err("sessionId is required to clear workspace binding".to_string());
    }
    let mut state = read_workspace_state()?;
    let mut changed = false;
    for item in &mut state.workspaces {
        if item.active_session_id.as_deref() == Some(normalized_session_id) {
            item.active_session_id = None;
            item.active_session_selected_at_ms = None;
            item.updated_at = current_timestamp_ms();
            changed = true;
        }
    }
    if changed {
        state.updated_at = current_timestamp_ms();
        persist_workspace_state(&state)?;
    }
    Ok(())
}

fn workspace_item_matches_root(item: &PersistedWorkspaceItem, root: &Path) -> bool {
    workspace_item_root(item).as_deref() == Some(root)
}

fn normalized_workspace_root_key_from_path(root: &Path) -> String {
    display_path(root)
}

fn activate_workspace_root(root: &Path) -> Result<PersistedWorkspaceState, String> {
    let normalized = normalize_workspace_root(root.to_path_buf())
        .ok_or_else(|| "workspace root is not a directory".to_string())?;
    let mut state = read_workspace_state()?;
    upsert_workspace_state_item(&mut state, normalized.as_path());
    state.active_workspace_root = Some(normalized_workspace_root_key_from_path(
        normalized.as_path(),
    ));
    state.updated_at = current_timestamp_ms();
    persist_workspace_state(&state)?;
    Ok(state)
}

fn upsert_workspace_state_item<'a>(
    state: &'a mut PersistedWorkspaceState,
    root: &Path,
) -> &'a mut PersistedWorkspaceItem {
    normalize_workspace_state_sort_orders(state);
    let normalized_root = normalized_workspace_root_key_from_path(root);
    if let Some(index) = state
        .workspaces
        .iter()
        .position(|item| workspace_item_matches_root(item, root))
    {
        state.workspaces[index].root = Some(normalized_root);
        state.workspaces[index].updated_at = current_timestamp_ms();
        return &mut state.workspaces[index];
    }
    let sort_order = next_workspace_sort_order(state);
    state.workspaces.push(PersistedWorkspaceItem {
        root: Some(normalized_root),
        display_name: None,
        active_session_id: None,
        active_session_selected_at_ms: None,
        sort_order: Some(sort_order),
        updated_at: current_timestamp_ms(),
    });
    state.workspaces.last_mut().expect("workspace item exists")
}

fn normalize_workspace_state_sort_orders(state: &mut PersistedWorkspaceState) {
    let mut used_orders = HashSet::<i64>::new();
    let mut next_order = 0i64;
    for item in &mut state.workspaces {
        match item.sort_order {
            Some(order) if order >= 0 && used_orders.insert(order) => {
                next_order = next_order.max(order.saturating_add(1));
            }
            _ => {
                while used_orders.contains(&next_order) {
                    next_order = next_order.saturating_add(1);
                }
                item.sort_order = Some(next_order);
                used_orders.insert(next_order);
                next_order = next_order.saturating_add(1);
            }
        }
    }
}

fn next_workspace_sort_order(state: &PersistedWorkspaceState) -> i64 {
    state
        .workspaces
        .iter()
        .filter_map(|item| item.sort_order)
        .max()
        .map(|order| order.saturating_add(1))
        .unwrap_or(0)
}

fn to_workspace_snapshot_response(
    cancelled: bool,
    state: &PersistedWorkspaceState,
) -> Result<WorkspaceSnapshotResponse, String> {
    let active_workspace_root = active_workspace_root_from_state(state)
        .as_ref()
        .map(|root| normalized_workspace_root_key_from_path(root.as_path()));
    let mut seen_roots = HashSet::<String>::new();
    let mut workspaces = state
        .workspaces
        .iter()
        .filter_map(workspace_item_root)
        .filter_map(|root| {
            let key = normalized_workspace_root_key_from_path(root.as_path());
            if !seen_roots.insert(key) {
                return None;
            }
            Some(to_workspace_info_response(root.as_path(), state))
        })
        .collect::<Vec<_>>();
    workspaces.sort_by(|left, right| {
        left.sort_order
            .cmp(&right.sort_order)
            .then_with(|| left.root.cmp(&right.root))
    });
    Ok(WorkspaceSnapshotResponse {
        active_workspace_root,
        workspaces,
        cancelled,
    })
}

fn to_workspace_info_response(
    root: &Path,
    state: &PersistedWorkspaceState,
) -> WorkspaceInfoResponse {
    let item = state
        .workspaces
        .iter()
        .cloned()
        .into_iter()
        .find(|item| workspace_item_matches_root(item, root));
    WorkspaceInfoResponse {
        root: normalized_workspace_root_key_from_path(root),
        name: item
            .as_ref()
            .and_then(|item| item.display_name.clone())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| workspace_name_from_root(root)),
        active_session_id: item.and_then(|item| {
            item.active_session_id
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
        }),
        sort_order: state
            .workspaces
            .iter()
            .find(|item| workspace_item_matches_root(item, root))
            .and_then(|item| item.sort_order)
            .unwrap_or(0),
        updated_at: state
            .workspaces
            .iter()
            .find(|item| workspace_item_matches_root(item, root))
            .map(|item| item.updated_at)
            .unwrap_or_else(current_timestamp_ms),
    }
}

fn workspace_name_from_root(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

pub(crate) fn validate_workspace_file_access(
    session_id: Option<&str>,
    workspace_root: &Path,
) -> Result<(), String> {
    validate_open_workspace_file_access(workspace_root)?;
    let Some(session_id) = session_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let request_root = normalize_workspace_root(workspace_root.to_path_buf())
        .ok_or_else(|| "workspace file access requires an open workspace".to_string())?;
    let request_root_key = normalized_workspace_root_key_from_path(request_root.as_path());
    let session_root = sessions::cwd_for_session_id(session_id)?;
    let session_root = normalize_workspace_root(PathBuf::from(session_root))
        .ok_or_else(|| "session working directory is not a directory".to_string())?;
    let session_root_key = normalized_workspace_root_key_from_path(session_root.as_path());
    if session_root_key != request_root_key {
        return Err(
            "workspace file access denied: session cwd does not match request root".to_string(),
        );
    }
    Ok(())
}

fn validate_open_workspace_file_access(workspace_root: &Path) -> Result<(), String> {
    let request_root = normalize_workspace_root(workspace_root.to_path_buf())
        .ok_or_else(|| "workspace file access requires an open workspace".to_string())?;
    let request_root_key = normalized_workspace_root_key_from_path(request_root.as_path());
    let state = read_workspace_state()?;
    let is_open = state.workspaces.iter().any(|item| {
        workspace_item_root(item)
            .map(|root| normalized_workspace_root_key_from_path(root.as_path()) == request_root_key)
            .unwrap_or(false)
    });
    if !is_open {
        return Err(String::from(
            "workspace file access requires an open workspace",
        ));
    }
    Ok(())
}

fn list_workspace_file_entries(
    root: &Path,
    dir: &Path,
    depth: usize,
    max_depth: usize,
) -> Result<Vec<WorkspaceFileTreeEntry>, String> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir).map_err(|error| format!("read workspace dir failed: {error}"))? {
        let item = item.map_err(|error| format!("read workspace entry failed: {error}"))?;
        let path = item.path();
        let file_name = item.file_name().to_string_lossy().to_string();
        let is_directory = path.is_dir();
        if should_skip_workspace_tree_entry(file_name.as_str(), is_directory) {
            continue;
        }
        let children = if is_directory && depth < max_depth {
            list_workspace_file_entries(root, path.as_path(), depth.saturating_add(1), max_depth)?
        } else {
            Vec::new()
        };
        entries.push(WorkspaceFileTreeEntry {
            name: file_name,
            path: workspace_relative_path(root, path.as_path()),
            is_directory,
            children,
        });
    }
    entries.sort_by(|left, right| {
        right.is_directory.cmp(&left.is_directory).then_with(|| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        })
    });
    Ok(entries)
}

fn should_skip_workspace_tree_entry(name: &str, is_directory: bool) -> bool {
    if name.starts_with('.') {
        return true;
    }
    if !is_directory {
        return false;
    }
    matches!(
        name,
        "node_modules"
            | "target"
            | "dist"
            | "build"
            | "coverage"
            | ".next"
            | ".nuxt"
            | ".svelte-kit"
            | "out"
    )
}

fn resolve_workspace_preview_file_path(
    root: &Path,
    raw_path: &str,
) -> Result<ResolvedPreviewFilePath, String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(String::from("workspace file path is empty"));
    }
    let requested_path = Path::new(trimmed);
    if requested_path.is_absolute() {
        let canonical_root = root
            .canonicalize()
            .map_err(|error| format!("resolve workspace root failed: {error}"))?;
        let canonical_path = requested_path
            .canonicalize()
            .map_err(|error| format!("resolve workspace file failed: {error}"))?;
        if !canonical_path.starts_with(canonical_root.as_path()) {
            return Err(String::from(
                "workspace file path cannot escape the workspace",
            ));
        }
        if !canonical_path.is_file() {
            return Err(String::from("workspace path is not a file"));
        }
        return Ok(ResolvedPreviewFilePath {
            root: canonical_root.clone(),
            response_path: workspace_relative_path(
                canonical_root.as_path(),
                canonical_path.as_path(),
            ),
            path: canonical_path,
        });
    }
    let path = resolve_workspace_relative_file_path(root, trimmed)?;
    Ok(ResolvedPreviewFilePath {
        root: root.to_path_buf(),
        response_path: workspace_relative_path(root, path.as_path()),
        path,
    })
}

fn resolve_workspace_relative_file_path(
    root: &Path,
    relative_path: &str,
) -> Result<PathBuf, String> {
    let requested_path = Path::new(relative_path);
    if relative_path.trim().is_empty() {
        return Err(String::from("workspace file path is empty"));
    }
    if requested_path.is_absolute() {
        return Err(String::from("workspace file path must be relative"));
    }
    if requested_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(String::from(
            "workspace file path cannot escape the workspace",
        ));
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("resolve workspace root failed: {error}"))?;
    let canonical_path = canonical_root
        .join(requested_path)
        .canonicalize()
        .map_err(|error| format!("resolve workspace file failed: {error}"))?;
    if !canonical_path.starts_with(canonical_root.as_path()) {
        return Err(String::from(
            "workspace file path cannot escape the workspace",
        ));
    }
    if !canonical_path.is_file() {
        return Err(String::from("workspace path is not a file"));
    }
    Ok(canonical_path)
}

fn read_preview_file(
    resolved: ResolvedPreviewFilePath,
    fallback_name: &str,
) -> Result<WorkspaceReadFileResponse, String> {
    let metadata = fs::metadata(resolved.path.as_path())
        .map_err(|error| format!("read file metadata failed: {error}"))?;
    if metadata.len() > WORKSPACE_READ_FILE_MAX_BYTES {
        return Err(format!(
            "workspace file is too large to preview ({} bytes, max {})",
            metadata.len(),
            WORKSPACE_READ_FILE_MAX_BYTES
        ));
    }
    let bytes = fs::read(resolved.path.as_path())
        .map_err(|error| format!("read workspace file failed: {error}"))?;
    let byte_len = bytes.len() as u64;
    let name = resolved
        .path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| fallback_name.to_string());
    if let Some(mime_type) = workspace_preview_image_mime_type(resolved.path.as_path()) {
        let data_url = format!(
            "data:{};base64,{}",
            mime_type,
            general_purpose::STANDARD.encode(bytes)
        );
        return Ok(WorkspaceReadFileResponse {
            root: display_path(resolved.root.as_path()),
            path: resolved.response_path,
            name,
            content: String::new(),
            byte_len,
            encoding: String::from("base64"),
            content_kind: String::from("image"),
            mime_type: Some(mime_type.to_string()),
            data_url: Some(data_url),
        });
    }
    let content = String::from_utf8(bytes)
        .map_err(|_| "workspace file is not valid UTF-8 text".to_string())?
        .trim_start_matches('\u{feff}')
        .to_string();
    Ok(WorkspaceReadFileResponse {
        root: display_path(resolved.root.as_path()),
        path: resolved.response_path,
        name,
        content,
        byte_len,
        encoding: String::from("utf-8"),
        content_kind: String::from("text"),
        mime_type: Some(String::from("text/plain; charset=utf-8")),
        data_url: None,
    })
}

fn workspace_preview_image_mime_type(file_path: &Path) -> Option<&'static str> {
    let extension = file_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.trim().to_ascii_lowercase())?;
    match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

fn workspace_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    raw.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "centaeris-electron-workspace-{name}-{}",
            current_timestamp_ms()
        ));
        fs::create_dir_all(path.as_path()).expect("create temp dir");
        path
    }

    #[test]
    fn resolve_workspace_relative_file_path_rejects_escape() {
        let root = unique_temp_dir("escape-root");
        let error = resolve_workspace_relative_file_path(root.as_path(), "../secret.txt")
            .expect_err("escape rejected");

        assert!(error.contains("cannot escape"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn display_path_strips_windows_verbatim_prefix() {
        assert_eq!(
            display_path(Path::new(r"\\?\D:\Projects\Centaeris")),
            r"D:\Projects\Centaeris"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\UNC\server\share")),
            r"\\server\share"
        );
    }

    fn write_workspace_fixture(file_path: &Path, workspace_root: &Path) {
        fs::write(
            file_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "activeWorkspaceRoot": display_path(workspace_root),
                "workspaces": [{
                    "root": display_path(workspace_root),
                    "displayName": null,
                    "activeSessionId": null,
                    "activeSessionSelectedAtMs": null,
                    "sortOrder": 0,
                    "updatedAt": 1
                }],
                "updatedAt": 1
            }))
            .expect("encode workspace fixture"),
        )
        .expect("write workspace fixture");
    }

    #[test]
    fn workspace_catalog_only_treats_not_found_as_empty() {
        let root = unique_temp_dir("catalog-not-found");
        let missing = root.join("workspace.json");
        let state = read_persisted_workspace_state_at(missing.as_path()).expect("empty state");
        assert!(state.workspaces.is_empty());

        fs::create_dir(missing.as_path()).expect("create unreadable catalog path");
        let error = read_persisted_workspace_state_at(missing.as_path())
            .expect_err("directory is not an empty catalog");
        assert!(matches!(error, WorkspaceCatalogError::Io(_)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_catalog_rejects_truncated_missing_and_banana_fields() {
        let root = unique_temp_dir("catalog-schema");
        let file_path = root.join("workspace.json");
        for raw in [
            "{\"activeWorkspaceRoot\":null",
            "{\"activeWorkspaceRoot\":null,\"workspaces\":[]}",
            "{\"activeWorkspaceRoot\":null,\"workspaces\":[],\"updatedAt\":0,\"banana\":true}",
        ] {
            fs::write(file_path.as_path(), raw).expect("write invalid catalog");
            let error = read_persisted_workspace_state_at(file_path.as_path())
                .expect_err("invalid catalog rejected");
            assert!(matches!(error, WorkspaceCatalogError::Corrupt(_)));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_catalog_canonicalize_failure_is_not_resettable_corruption() {
        let root = unique_temp_dir("catalog-invalid-root");
        let file_path = root.join("workspace.json");
        let missing_workspace = root.join("missing-workspace");
        write_workspace_fixture(file_path.as_path(), missing_workspace.as_path());

        let error = read_persisted_workspace_state_at(file_path.as_path())
            .expect_err("missing workspace rejected");
        assert!(matches!(error, WorkspaceCatalogError::PathInvalid(_)));
        let reset_error = reset_corrupt_workspace_catalog_at(file_path.as_path())
            .expect_err("path failure cannot be reset");
        assert!(reset_error.starts_with(WORKSPACE_CATALOG_PATH_INVALID));
        assert!(file_path.is_file());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_catalog_reset_quarantines_only_corrupt_catalog() {
        let root = unique_temp_dir("catalog-reset");
        let config_dir = root.join("config");
        let project_dir = root.join("project");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(config_dir.as_path()).expect("create config dir");
        fs::create_dir_all(project_dir.as_path()).expect("create project dir");
        fs::create_dir_all(sessions_dir.as_path()).expect("create sessions dir");
        fs::write(project_dir.join("keep.txt"), "project").expect("write project sentinel");
        fs::write(sessions_dir.join("keep.jsonl"), "session").expect("write session sentinel");
        let file_path = config_dir.join("workspace.json");
        fs::write(file_path.as_path(), "{truncated").expect("write corrupt catalog");

        let response =
            reset_corrupt_workspace_catalog_at(file_path.as_path()).expect("reset catalog");
        assert!(response.snapshot.workspaces.is_empty());
        assert!(Path::new(response.quarantined_path.as_str()).is_file());
        assert_eq!(
            fs::read_to_string(response.quarantined_path).expect("read quarantine"),
            "{truncated"
        );
        let state =
            read_persisted_workspace_state_at(file_path.as_path()).expect("read empty catalog");
        assert!(state.workspaces.is_empty());
        assert!(project_dir.join("keep.txt").is_file());
        assert!(sessions_dir.join("keep.jsonl").is_file());
        let _ = fs::remove_dir_all(root);
    }
}
