use crate::user_data_layout;
use centaeris_core::runtime::contracts::current_timestamp_ms;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const RUNTIME_GARBAGE_DOCUMENT_CACHE_GRACE_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const RUNTIME_GARBAGE_MAX_TTL_MS: u64 = 365 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RuntimeGarbageCollectRequest {
    pub(crate) dry_run: Option<bool>,
    pub(crate) document_cache_grace_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeGarbageCollectItem {
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) size_bytes: u64,
    pub(crate) modified_at_ms: Option<i64>,
    pub(crate) expires_at_ms: i64,
    pub(crate) reason: String,
    pub(crate) deleted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeGarbageCollectResponse {
    pub(crate) schema: String,
    pub(crate) dry_run: bool,
    pub(crate) data_root: String,
    pub(crate) candidate_count: usize,
    pub(crate) deleted_count: usize,
    pub(crate) total_candidate_bytes: u64,
    pub(crate) total_deleted_bytes: u64,
    pub(crate) items: Vec<RuntimeGarbageCollectItem>,
    pub(crate) generated_at_ms: i64,
}

pub(crate) fn collect(
    request: RuntimeGarbageCollectRequest,
) -> Result<RuntimeGarbageCollectResponse, String> {
    let dry_run = request.dry_run.unwrap_or(true);
    let now_ms = current_timestamp_ms();
    let data_root = desktop_data_root_dir();
    let canonical_data_root = ensure_directory_for_runtime_garbage_root(data_root.as_path())?;
    let mut items =
        collect_runtime_garbage_candidates(canonical_data_root.as_path(), &request, now_ms)?;
    let total_candidate_bytes = items
        .iter()
        .fold(0u64, |total, item| total.saturating_add(item.size_bytes));
    let mut deleted_count = 0usize;
    let mut total_deleted_bytes = 0u64;
    if !dry_run {
        for item in items.iter_mut() {
            let item_path = PathBuf::from(item.path.as_str());
            remove_runtime_garbage_path(canonical_data_root.as_path(), item_path.as_path())?;
            item.deleted = true;
            deleted_count = deleted_count.saturating_add(1);
            total_deleted_bytes = total_deleted_bytes.saturating_add(item.size_bytes);
        }
    }

    Ok(RuntimeGarbageCollectResponse {
        schema: String::from("runtime_garbage_collect_v1"),
        dry_run,
        data_root: canonical_data_root.to_string_lossy().to_string(),
        candidate_count: items.len(),
        deleted_count,
        total_candidate_bytes,
        total_deleted_bytes,
        items,
        generated_at_ms: now_ms,
    })
}

fn collect_runtime_garbage_candidates(
    data_root: &Path,
    request: &RuntimeGarbageCollectRequest,
    now_ms: i64,
) -> Result<Vec<RuntimeGarbageCollectItem>, String> {
    let mut items = Vec::new();
    collect_expired_document_cache_garbage(
        data_root,
        request
            .document_cache_grace_ms
            .unwrap_or(RUNTIME_GARBAGE_DOCUMENT_CACHE_GRACE_MS),
        now_ms,
        &mut items,
    )?;
    items.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(items)
}

fn collect_expired_document_cache_garbage(
    data_root: &Path,
    grace_ms: u64,
    now_ms: i64,
    items: &mut Vec<RuntimeGarbageCollectItem>,
) -> Result<(), String> {
    let grace_ms = normalize_runtime_garbage_grace_ms(grace_ms);
    let document_root = data_root.join("runtime").join("document");
    if !document_root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(document_root.as_path())
        .map_err(|error| format!("read document cache directory failed: {error}"))?
    {
        let entry = entry.map_err(|error| format!("read document cache entry failed: {error}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        maybe_push_runtime_garbage_candidate(
            data_root,
            path.as_path(),
            "documentCache",
            "document cache exceeded documentCacheGraceMs",
            grace_ms,
            now_ms,
            items,
        )?;
    }
    Ok(())
}

fn maybe_push_runtime_garbage_candidate(
    data_root: &Path,
    path: &Path,
    kind: &str,
    reason: &str,
    ttl_ms: u64,
    now_ms: i64,
    items: &mut Vec<RuntimeGarbageCollectItem>,
) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "read runtime garbage metadata failed: {}: {error}",
            path.display()
        )
    })?;
    let modified_at_ms = metadata_modified_at_ms(&metadata)?;
    let expires_at_ms = modified_at_ms.saturating_add(ttl_ms as i64);
    if expires_at_ms > now_ms {
        return Ok(());
    }
    push_runtime_garbage_candidate(data_root, path, kind, reason, expires_at_ms, items)
}

fn push_runtime_garbage_candidate(
    data_root: &Path,
    path: &Path,
    kind: &str,
    reason: &str,
    expires_at_ms: i64,
    items: &mut Vec<RuntimeGarbageCollectItem>,
) -> Result<(), String> {
    let canonical_path = canonicalize_runtime_garbage_child(data_root, path)?;
    let metadata = fs::metadata(canonical_path.as_path()).map_err(|error| {
        format!(
            "read runtime garbage candidate metadata failed: {}: {error}",
            canonical_path.display()
        )
    })?;
    let modified_at_ms = metadata_modified_at_ms(&metadata).ok();
    let size_bytes = runtime_garbage_path_size(canonical_path.as_path())?;
    items.push(RuntimeGarbageCollectItem {
        kind: kind.to_string(),
        path: canonical_path.to_string_lossy().to_string(),
        size_bytes,
        modified_at_ms,
        expires_at_ms,
        reason: reason.to_string(),
        deleted: false,
    });
    Ok(())
}

fn runtime_garbage_path_size(path: &Path) -> Result<u64, String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "read runtime garbage size metadata failed: {}: {error}",
            path.display()
        )
    })?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(path)
        .map_err(|error| format!("read runtime garbage size directory failed: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("read runtime garbage size entry failed: {error}"))?;
        total = total.saturating_add(runtime_garbage_path_size(entry.path().as_path())?);
    }
    Ok(total)
}

fn ensure_directory_for_runtime_garbage_root(path: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("create runtime garbage data root failed: {error}"))?;
    path.canonicalize()
        .map_err(|error| format!("resolve runtime garbage data root failed: {error}"))
}

fn canonicalize_runtime_garbage_child(data_root: &Path, path: &Path) -> Result<PathBuf, String> {
    let canonical_data_root = data_root
        .canonicalize()
        .map_err(|error| format!("resolve runtime garbage root failed: {error}"))?;
    let canonical_path = path.canonicalize().map_err(|error| {
        format!(
            "resolve runtime garbage path failed: {}: {error}",
            path.display()
        )
    })?;
    if canonical_path == canonical_data_root || !canonical_path.starts_with(canonical_data_root) {
        return Err(format!(
            "runtime garbage path outside data root is not allowed: {}",
            canonical_path.display()
        ));
    }
    Ok(canonical_path)
}

fn remove_runtime_garbage_path(data_root: &Path, path: &Path) -> Result<(), String> {
    let canonical_path = canonicalize_runtime_garbage_child(data_root, path)?;
    let metadata = fs::metadata(canonical_path.as_path()).map_err(|error| {
        format!(
            "read runtime garbage delete metadata failed: {}: {error}",
            canonical_path.display()
        )
    })?;
    if metadata.is_dir() {
        fs::remove_dir_all(canonical_path.as_path()).map_err(|error| {
            format!(
                "delete runtime garbage directory failed: {}: {error}",
                canonical_path.display()
            )
        })?;
    } else if metadata.is_file() {
        fs::remove_file(canonical_path.as_path()).map_err(|error| {
            format!(
                "delete runtime garbage file failed: {}: {error}",
                canonical_path.display()
            )
        })?;
    }
    Ok(())
}

fn normalize_runtime_garbage_grace_ms(value: u64) -> u64 {
    value.min(RUNTIME_GARBAGE_MAX_TTL_MS)
}

fn metadata_modified_at_ms(metadata: &fs::Metadata) -> Result<i64, String> {
    let modified = metadata
        .modified()
        .map_err(|error| format!("read runtime garbage modified time failed: {error}"))?;
    let duration = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("runtime garbage modified time is before unix epoch: {error}"))?;
    Ok(duration.as_millis().min(i64::MAX as u128) as i64)
}

fn desktop_data_root_dir() -> PathBuf {
    user_data_layout::desktop_data_root_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_log;

    #[test]
    fn runtime_garbage_request_rejects_unknown_ttl_field() {
        let error = serde_json::from_value::<RuntimeGarbageCollectRequest>(serde_json::json!({
            "dryRun": true,
            "unknownTtlMs": 1000
        }))
        .expect_err("unknown ttl field must fail loudly");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn runtime_garbage_request_accepts_document_cache_grace_field() {
        let request = serde_json::from_value::<RuntimeGarbageCollectRequest>(serde_json::json!({
            "dryRun": true,
            "documentCacheGraceMs": 1000
        }))
        .expect("documentCacheGraceMs should deserialize");

        assert_eq!(request.document_cache_grace_ms, Some(1000));
    }

    #[test]
    fn runtime_garbage_collects_document_cache_without_index_json() {
        let guard = message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp_root = std::env::temp_dir().join(format!(
            "centaeris-runtime-garbage-document-cache-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let document_cache = temp_root
            .join("runtime")
            .join("document")
            .join("document-cache");
        std::fs::create_dir_all(document_cache.as_path()).expect("document cache");
        std::fs::write(document_cache.join("sample.txt"), "cached").expect("cache file");
        let previous_data_dir = std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR");
        std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", &temp_root);
        let result = collect(RuntimeGarbageCollectRequest {
            dry_run: Some(true),
            document_cache_grace_ms: Some(0),
        });
        if let Some(previous) = previous_data_dir {
            std::env::set_var("CENTAERIS_DESKTOP_DATA_DIR", previous);
        } else {
            std::env::remove_var("CENTAERIS_DESKTOP_DATA_DIR");
        }
        let _ = std::fs::remove_dir_all(temp_root.as_path());
        drop(guard);
        let response = result.expect("document cache GC");
        assert!(response
            .items
            .iter()
            .any(|item| item.kind == "documentCache"));
    }
}
