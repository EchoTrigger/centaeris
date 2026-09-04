use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::{now_ms, ToolRuntimeContext};
use crate::session::reliability::{
    AcquireResourceClaimDisposition, AcquireResourceClaimRequest, ReleaseResourceClaimRequest,
    ResourceClaimStorePort,
};

static FILE_WRITE_LEASES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
fn file_write_leases() -> &'static Mutex<HashMap<String, String>> {
    FILE_WRITE_LEASES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug)]
pub(super) struct FileWriteLease {
    path_identity: String,
}

impl Drop for FileWriteLease {
    fn drop(&mut self) {
        if let Ok(mut leases) = file_write_leases().lock() {
            leases.remove(self.path_identity.as_str());
        }
    }
}

pub(super) fn acquire_file_write_lease(
    path_identity: &str,
    owner: &str,
) -> Result<FileWriteLease, String> {
    let lease_path = normalize_file_write_lease_identity(path_identity)?;
    let mut leases = file_write_leases()
        .lock()
        .map_err(|_| "file write lease state lock poisoned".to_string())?;
    if let Some(existing_owner) = leases.get(lease_path.as_str()) {
        return Err(format!(
            "file write conflict: {lease_path} is already leased by {existing_owner}"
        ));
    }
    leases.insert(lease_path.clone(), owner.to_string());
    Ok(FileWriteLease {
        path_identity: lease_path,
    })
}

struct DurableResourceClaimGuard {
    store: Arc<dyn ResourceClaimStorePort + Send + Sync>,
    resource_kind: String,
    resource_key: String,
    owner: String,
}

impl Drop for DurableResourceClaimGuard {
    fn drop(&mut self) {
        let _ = self
            .store
            .release_resource_claim(ReleaseResourceClaimRequest {
                resource_kind: self.resource_kind.clone(),
                resource_key: self.resource_key.clone(),
                owner: self.owner.clone(),
                released_at_ms: now_ms(),
            });
    }
}

pub(super) struct FileWriteGuard {
    _durable_claim: Option<DurableResourceClaimGuard>,
    _process_lease: FileWriteLease,
}

pub(super) fn acquire_file_write_guard(
    path_identity: &str,
    runtime_context: &ToolRuntimeContext,
) -> Result<FileWriteGuard, String> {
    let durable_claim = acquire_durable_file_write_claim(path_identity, runtime_context)?;
    let process_lease =
        match acquire_file_write_lease(path_identity, runtime_context.write_lease_owner()) {
            Ok(lease) => lease,
            Err(error) => {
                drop(durable_claim);
                return Err(error);
            }
        };
    Ok(FileWriteGuard {
        _durable_claim: durable_claim,
        _process_lease: process_lease,
    })
}

fn acquire_durable_file_write_claim(
    path_identity: &str,
    runtime_context: &ToolRuntimeContext,
) -> Result<Option<DurableResourceClaimGuard>, String> {
    let Some(store) = runtime_context.resource_claim_store() else {
        return Ok(None);
    };
    let owner = runtime_context.write_lease_owner().to_string();
    let resource_key = normalize_file_write_lease_identity(path_identity)?;
    let result = store.acquire_resource_claim(AcquireResourceClaimRequest {
        resource_kind: "file".to_string(),
        resource_key: resource_key.clone(),
        owner: owner.clone(),
        owner_kind: "tool_runtime".to_string(),
        session_id: None,
        branch_id: None,
        now_ms: now_ms(),
        ttl_ms: runtime_context.resource_claim_ttl_ms(),
        metadata_json: json!({
            "schema": "file_write_claim_v1",
            "path": resource_key.as_str(),
        })
        .to_string(),
    })?;
    match result.disposition {
        AcquireResourceClaimDisposition::Acquired
        | AcquireResourceClaimDisposition::AlreadyOwned => Ok(Some(DurableResourceClaimGuard {
            store,
            resource_kind: "file".to_string(),
            resource_key,
            owner,
        })),
        AcquireResourceClaimDisposition::Conflict => Err(format!(
            "file write conflict: {} is already claimed by {}",
            result.claim.resource_key, result.claim.owner
        )),
    }
}

fn normalize_file_write_lease_identity(path_identity: &str) -> Result<String, String> {
    let normalized = path_identity.trim();
    if normalized.is_empty() {
        return Err("file path identity cannot be empty".to_string());
    }
    Ok(normalized.to_string())
}

pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(format!("{byte:02x}").as_str());
    }
    format!("sha256:{hex}")
}

const WRITE_DIFF_PREVIEW_MAX_CHARS: usize = 2_400;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct WriteDiffPreview {
    pub text: String,
    pub added_lines: usize,
    pub removed_lines: usize,
}

pub(super) fn write_diff_preview(
    path: &str,
    previous_content: Option<&[u8]>,
    content: &str,
) -> WriteDiffPreview {
    let new_lines = content.lines().collect::<Vec<_>>();
    let Some(previous_content) = previous_content else {
        return WriteDiffPreview {
            text: bounded_diff(
                format!(
                    "--- /dev/null\n+++ {path}\n@@ -0,0 +1,{} @@\n{}",
                    new_lines.len(),
                    prefixed_block(new_lines.as_slice(), '+', WRITE_DIFF_PREVIEW_MAX_CHARS)
                )
                .as_str(),
                WRITE_DIFF_PREVIEW_MAX_CHARS,
            ),
            added_lines: new_lines.len(),
            removed_lines: 0,
        };
    };
    let Ok(previous_text) = std::str::from_utf8(previous_content) else {
        return WriteDiffPreview {
            text: bounded_diff(
                format!(
                    "--- {path}\n+++ {path}\n@@ binary content replaced @@\n-<previous content is not UTF-8>\n{}",
                    prefixed_block(new_lines.as_slice(), '+', WRITE_DIFF_PREVIEW_MAX_CHARS)
                )
                .as_str(),
                WRITE_DIFF_PREVIEW_MAX_CHARS,
            ),
            added_lines: new_lines.len(),
            removed_lines: byte_line_count(previous_content),
        };
    };
    let old_lines = previous_text.lines().collect::<Vec<_>>();
    let force_full_replacement = previous_text != content && old_lines == new_lines;
    let prefix = if force_full_replacement {
        0
    } else {
        old_lines
            .iter()
            .zip(new_lines.iter())
            .take_while(|(old, new)| old == new)
            .count()
    };
    let suffix = if force_full_replacement {
        0
    } else {
        old_lines[prefix..]
            .iter()
            .rev()
            .zip(new_lines[prefix..].iter().rev())
            .take_while(|(old, new)| old == new)
            .count()
    };
    let old_changed = &old_lines[prefix..old_lines.len().saturating_sub(suffix)];
    let new_changed = &new_lines[prefix..new_lines.len().saturating_sub(suffix)];
    let header = format!(
        "--- {path}\n+++ {path}\n@@ -{},{} +{},{} @@\n",
        prefix.saturating_add(1),
        old_changed.len(),
        prefix.saturating_add(1),
        new_changed.len(),
    );
    let remaining_chars = WRITE_DIFF_PREVIEW_MAX_CHARS.saturating_sub(header.chars().count());
    let (old_budget, new_budget) = if old_changed.is_empty() {
        (0, remaining_chars)
    } else if new_changed.is_empty() {
        (remaining_chars, 0)
    } else {
        (
            remaining_chars / 2,
            remaining_chars.saturating_sub(remaining_chars / 2),
        )
    };
    let old_block = prefixed_block(old_changed, '-', old_budget);
    let new_block = prefixed_block(new_changed, '+', new_budget);
    let body = if old_block.is_empty() && new_block.is_empty() {
        " <no textual changes>".to_string()
    } else {
        [old_block, new_block]
            .into_iter()
            .filter(|block| !block.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    WriteDiffPreview {
        text: bounded_diff(
            format!("{header}{body}").as_str(),
            WRITE_DIFF_PREVIEW_MAX_CHARS,
        ),
        added_lines: new_changed.len(),
        removed_lines: old_changed.len(),
    }
}

fn byte_line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let separators = bytes.iter().filter(|byte| **byte == b'\n').count();
    separators + usize::from(!bytes.ends_with(b"\n"))
}

fn prefixed_block(lines: &[&str], prefix: char, max_chars: usize) -> String {
    if lines.is_empty() || max_chars == 0 {
        return String::new();
    }
    let full = lines
        .iter()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    bounded_diff(full.as_str(), max_chars)
}

fn bounded_diff(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut bounded = value.chars().take(keep).collect::<String>();
    bounded.push_str("...");
    bounded
}

#[cfg(test)]
mod tests {
    use super::{write_diff_preview, WRITE_DIFF_PREVIEW_MAX_CHARS};

    #[test]
    fn write_diff_is_bounded_and_preserves_both_sides() {
        let previous = format!("head\n{}\ntail\n", "old\n".repeat(2_000));
        let content = format!("head\n{}\ntail\n", "new\n".repeat(2_000));
        let preview = write_diff_preview("banana.txt", Some(previous.as_bytes()), content.as_str());

        assert!(preview.text.chars().count() <= WRITE_DIFF_PREVIEW_MAX_CHARS);
        assert!(preview.text.contains("-old"));
        assert!(preview.text.contains("+new"));
        assert_eq!(preview.removed_lines, 2_000);
        assert_eq!(preview.added_lines, 2_000);

        let newline_only = write_diff_preview("banana.txt", Some(b"same"), "same\n");
        assert!(newline_only.text.contains("-same"));
        assert!(newline_only.text.contains("+same"));
    }
}
