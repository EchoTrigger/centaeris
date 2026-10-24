//! Bounded runner-local completed observations, never durable replay authority.
//!
//! The byte budget covers encoded contents, including an in-progress spill.
//! Filesystem blocks and metadata have additional overhead bounded by entry count.
use centaeris_core::execution::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Write};
#[cfg(test)]
use std::path::PathBuf;
use tempfile::TempDir;

const MAX_COLD_ENTRIES: usize = 8192;
const MAX_COLD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 512 * 1024;
const RECEIPT_SCHEMA: &str = "centaeris.e2b.completed_command_cache.v1";

#[derive(Clone, Copy)]
pub(super) struct CacheLimits {
    pub(super) entries: usize,
    pub(super) bytes: u64,
    pub(super) record_bytes: usize,
}
impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            entries: MAX_COLD_ENTRIES,
            bytes: MAX_COLD_BYTES,
            record_bytes: MAX_RECORD_BYTES,
        }
    }
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheFault {
    IndexReserve,
    Write,
    Publish,
}

// Membership is retained even if a file is lost: absence is never permission to
// dispatch that operation again. This index is strictly bounded and has no full
// output bodies. It is neither persisted nor reconstructed after a restart.
struct ColdEntry {
    request_digest: String,
    byte_length: u64,
    content_sha256: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredReceipt<T> {
    schema: String,
    operation_id: String,
    request_digest: String,
    output: T,
}

pub(super) struct CompletedCache {
    directory: Option<TempDir>,
    entries: HashMap<[u8; 32], ColdEntry>,
    encoded_bytes: u64,
    limits: CacheLimits,
    #[cfg(test)]
    fault: Option<CacheFault>,
}
impl CompletedCache {
    pub(super) fn new() -> Self {
        Self {
            directory: None,
            entries: HashMap::new(),
            encoded_bytes: 0,
            limits: CacheLimits::default(),
            #[cfg(test)]
            fault: None,
        }
    }
    pub(super) fn lookup(
        &self,
        operation_id: &str,
        digest: &str,
    ) -> Result<Option<ExecutionHostCommandOutput>, ExecutionError> {
        let key: [u8; 32] = Sha256::digest(operation_id.as_bytes()).into();
        let Some(entry) = self.entries.get(&key) else {
            return Ok(None);
        };
        if entry.request_digest != digest {
            return Err(ExecutionError::Denied {
                reason: "e2b_command_operation_binding_mismatch".into(),
            });
        }
        let root = self.directory.as_ref().ok_or_else(cached_unavailable)?;
        let path = root.path().join(file_name(key));
        observe_io(CacheIoPhase::Load, || {
            let metadata = fs::symlink_metadata(&path).map_err(|_| cached_unavailable())?;
            if !metadata.is_file()
                || metadata.len() != entry.byte_length
                || entry.byte_length > MAX_RECORD_BYTES as u64
            {
                return Err(cached_unavailable());
            }
            let file = File::open(&path).map_err(|_| cached_unavailable())?;
            let metadata = file.metadata().map_err(|_| cached_unavailable())?;
            if !metadata.is_file() || metadata.len() != entry.byte_length {
                return Err(cached_unavailable());
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(entry.byte_length as usize)
                .map_err(|_| cached_unavailable())?;
            file.take(entry.byte_length.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|_| cached_unavailable())?;
            let content_sha256: [u8; 32] = Sha256::digest(&bytes).into();
            if bytes.len() as u64 != entry.byte_length || content_sha256 != entry.content_sha256 {
                return Err(cached_unavailable());
            }
            let receipt: StoredReceipt<ExecutionHostCommandOutput> =
                serde_json::from_slice(&bytes).map_err(|_| cached_unavailable())?;
            if receipt.schema != RECEIPT_SCHEMA
                || receipt.operation_id != operation_id
                || receipt.request_digest != digest
            {
                return Err(cached_unavailable());
            }
            Ok(Some(receipt.output))
        })
    }
    // The same finite serialization and limit as an actual spill, directed to
    // a sink. A record that cannot fit must not consume temporary-file IO or
    // prevent a later smaller confirmed completion from freeing a hot slot.
    pub(super) fn candidate_fits(
        &self,
        operation_id: &str,
        digest: &str,
        output: &ExecutionHostCommandOutput,
    ) -> Result<bool, ExecutionError> {
        let (_, max_bytes) = self.archive_limit(operation_id, digest)?;
        let receipt = StoredReceipt {
            schema: RECEIPT_SCHEMA.to_string(),
            operation_id: operation_id.to_string(),
            request_digest: digest.to_string(),
            output,
        };
        let mut writer = ReceiptWriter {
            file: std::io::sink(),
            limit: max_bytes,
            written: 0,
            hasher: Sha256::new(),
            limit_hit: false,
        };
        if serde_json::to_writer(&mut writer, &receipt).is_err() {
            return if writer.limit_hit {
                Ok(false)
            } else {
                Err(spill_unavailable())
            };
        }
        Ok(true)
    }
    fn archive_limit(
        &self,
        operation_id: &str,
        digest: &str,
    ) -> Result<([u8; 32], u64), ExecutionError> {
        // Keep even index strings bounded independently of the caller's digest
        // construction. These inputs never become filesystem path components.
        if operation_id.len() > 512 || digest.len() > 71 {
            return Err(storage_capacity());
        }
        let key: [u8; 32] = Sha256::digest(operation_id.as_bytes()).into();
        if self.entries.contains_key(&key) {
            return Err(spill_unavailable());
        }
        if self.entries.len() >= self.limits.entries {
            return Err(storage_capacity());
        }
        let max_bytes = self
            .limits
            .bytes
            .saturating_sub(self.encoded_bytes)
            .min(self.limits.record_bytes as u64);
        if max_bytes == 0 {
            return Err(storage_capacity());
        }
        Ok((key, max_bytes))
    }
    pub(super) fn archive(
        &mut self,
        operation_id: &str,
        digest: &str,
        output: &ExecutionHostCommandOutput,
    ) -> Result<(), ExecutionError> {
        let (key, max_bytes) = self.archive_limit(operation_id, digest)?;
        #[cfg(test)]
        if self.take_fault(CacheFault::IndexReserve) {
            return Err(spill_unavailable());
        }
        // Reserve membership before writing. Publication and insertion below run
        // under the caller's cache mutex; insertion cannot require another grow.
        self.entries
            .try_reserve(1)
            .map_err(|_| spill_unavailable())?;
        observe_io(CacheIoPhase::Spill, || {
            self.archive_io(operation_id, digest, output, key, max_bytes)
        })
    }
    fn archive_io(
        &mut self,
        operation_id: &str,
        digest: &str,
        output: &ExecutionHostCommandOutput,
        key: [u8; 32],
        max_bytes: u64,
    ) -> Result<(), ExecutionError> {
        if self.directory.is_none() {
            self.directory = Some(
                tempfile::Builder::new()
                    .prefix("centaeris-e2b-command-cache-")
                    .tempdir()
                    .map_err(|_| spill_unavailable())?,
            );
        }
        let root = self
            .directory
            .as_ref()
            .ok_or_else(spill_unavailable)?
            .path()
            .to_path_buf();
        let receipt = StoredReceipt {
            schema: RECEIPT_SCHEMA.to_string(),
            operation_id: operation_id.to_string(),
            request_digest: digest.to_string(),
            output,
        };
        let mut temporary = tempfile::Builder::new()
            .prefix("receipt-")
            .tempfile_in(&root)
            .map_err(|_| spill_unavailable())?;
        #[cfg(test)]
        if self.take_fault(CacheFault::Write) {
            return Err(spill_unavailable());
        }
        let (byte_length, content_sha256) = {
            let mut writer = ReceiptWriter {
                file: temporary.as_file_mut(),
                limit: max_bytes,
                written: 0,
                hasher: Sha256::new(),
                limit_hit: false,
            };
            if serde_json::to_writer(&mut writer, &receipt).is_err() {
                return Err(if writer.limit_hit {
                    storage_capacity()
                } else {
                    spill_unavailable()
                });
            }
            writer.flush().map_err(|_| spill_unavailable())?;
            (writer.written, writer.hasher.clone().finalize().into())
        };
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| spill_unavailable())?;
        #[cfg(test)]
        if self.take_fault(CacheFault::Publish) {
            return Err(spill_unavailable());
        }
        // No overwrite, including of an unexpected file in our owned directory.
        // Tempfile cleans its own incomplete staging path on every error branch.
        let published = temporary
            .persist_noclobber(root.join(file_name(key)))
            .map_err(|_| spill_unavailable())?;
        drop(published);
        self.entries.insert(
            key,
            ColdEntry {
                request_digest: receipt.request_digest,
                byte_length,
                content_sha256,
            },
        );
        self.encoded_bytes += byte_length;
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn with_limits(limits: CacheLimits) -> Self {
        assert!(limits.entries > 0 && limits.entries <= MAX_COLD_ENTRIES);
        assert!(limits.bytes > 0 && limits.bytes <= MAX_COLD_BYTES);
        assert!(limits.record_bytes > 0 && limits.record_bytes <= MAX_RECORD_BYTES);
        Self {
            limits,
            ..Self::new()
        }
    }
    #[cfg(test)]
    pub(super) fn fail_next(&mut self, fault: CacheFault) {
        self.fault = Some(fault);
    }
    #[cfg(test)]
    fn take_fault(&mut self, fault: CacheFault) -> bool {
        if self.fault == Some(fault) {
            self.fault = None;
            true
        } else {
            false
        }
    }
    #[cfg(test)]
    pub(super) fn entry_path(&self, operation_id: &str) -> Option<PathBuf> {
        let key: [u8; 32] = Sha256::digest(operation_id.as_bytes()).into();
        self.entries.get(&key)?;
        Some(self.directory.as_ref()?.path().join(file_name(key)))
    }
    #[cfg(test)]
    pub(super) fn root_path(&self) -> Option<PathBuf> {
        self.directory
            .as_ref()
            .map(|root| root.path().to_path_buf())
    }
    #[cfg(test)]
    pub(super) fn used_bytes(&self) -> u64 {
        self.encoded_bytes
    }
    #[cfg(test)]
    pub(super) fn limit_bytes(&mut self, bytes: u64) {
        assert!(bytes > 0 && bytes <= MAX_COLD_BYTES);
        self.limits.bytes = bytes;
    }
}

struct ReceiptWriter<W> {
    file: W,
    limit: u64,
    written: u64,
    hasher: Sha256,
    limit_hit: bool,
}
impl<W: Write> Write for ReceiptWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .written
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.limit)
        {
            self.limit_hit = true;
            return Err(std::io::Error::other(
                "completed receipt encoding budget exceeded",
            ));
        }
        let written = self.file.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.written += written as u64;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
fn file_name(key: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    key.into_iter()
        .flat_map(|byte| {
            [
                HEX[(byte >> 4) as usize] as char,
                HEX[(byte & 15) as usize] as char,
            ]
        })
        .collect()
}
fn spill_unavailable() -> ExecutionError {
    ExecutionError::CapacityRejected {
        stage: ExecutionOperationStage::Start,
        reason: "e2b_command_evidence_spill_unavailable".into(),
    }
}
pub(super) fn storage_capacity() -> ExecutionError {
    ExecutionError::CapacityRejected {
        stage: ExecutionOperationStage::Start,
        reason: "e2b_command_evidence_storage_capacity".into(),
    }
}
fn cached_unavailable() -> ExecutionError {
    ExecutionError::OperationIndeterminate {
        stage: ExecutionOperationStage::Query,
        reason: "e2b_command_cached_output_unavailable".into(),
    }
}

#[derive(Clone, Copy)]
enum CacheIoPhase {
    Spill,
    Load,
}
fn observe_io<T>(
    phase: CacheIoPhase,
    operation: impl FnOnce() -> Result<T, ExecutionError>,
) -> Result<T, ExecutionError> {
    let mut span = crate::observations::enabled().then(|| {
        crate::observations::Span::new(match phase {
            CacheIoPhase::Spill => "e2bCommandEvidenceSpill",
            CacheIoPhase::Load => "e2bCommandEvidenceLoad",
        })
    });
    let result = operation();
    if let Some(span) = span.as_mut() {
        span.finished(result.is_ok());
    }
    result
}
