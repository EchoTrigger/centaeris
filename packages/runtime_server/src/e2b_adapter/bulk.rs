//! Private bounded staging for host-owned execution inputs and helper frames.
//!
//! Sources and sinks must be finite owned local IO. Deadlines are checked
//! between chunks; arbitrary blocking Read/Write implementations are not
//! interruptible. A download sink remains unpublished until this API returns Ok.
use super::{AdapterFailure, AdapterOutcome, AdapterProcess, AdapterResponse};
use crate::e2b_creation::E2bCreationAdapter;
use same_file::Handle;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use uuid::Uuid;

#[cfg(test)]
mod tests;

pub const MAX_TRANSFER_BYTES: u64 = 128 * 1024 * 1024;
pub const TRANSFER_CHUNK_BYTES: usize = 64 * 1024;

pub struct BulkFileRequest<'a> {
    pub sandbox_id: &'a str,
    pub path: &'a str,
    pub user: Option<&'a str>,
    pub max_bytes: u64,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransferReceipt {
    pub sandbox_id: String,
    pub transfer_id: String,
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkTransferError {
    NotDispatched(&'static str),
    Unknown(&'static str),
}

/// Owns its SDK child and spool together. No process handle or spool path escapes.
/// The host's temporary directory must provide private ownership/ACLs.
pub struct SpoolingAdapterProcess {
    process: AdapterProcess,
    spool: Option<TempDir>,
    root: PathBuf,
    lane: Mutex<()>,
}

impl SpoolingAdapterProcess {
    pub fn spawn(mut command: Command) -> Result<Self, BulkTransferError> {
        let spool = tempfile::Builder::new()
            .prefix("centaeris-e2b-bulk-")
            .tempdir()
            .map_err(|_| BulkTransferError::NotDispatched("bulk_spool_unavailable"))?;
        // Resolve OS temporary-directory aliases once; Python rejects linked ancestors.
        let root = spool
            .path()
            .canonicalize()
            .map_err(|_| BulkTransferError::NotDispatched("bulk_spool_unavailable"))?;
        command.env("E2B_ADAPTER_SPOOL_ROOT", &root);
        let process = AdapterProcess::spawn(command)
            .map_err(|_| BulkTransferError::NotDispatched("bulk_adapter_unavailable"))?;
        Ok(Self {
            process,
            spool: Some(spool),
            root,
            lane: Mutex::new(()),
        })
    }

    pub fn call(
        &self,
        request_id: &str,
        operation: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        if matches!(operation, "uploadFile" | "downloadFile") {
            return Err(AdapterFailure::NotDispatched("bulk_stream_api_required"));
        }
        let _lane = self
            .lane
            .try_lock()
            .map_err(|_| AdapterFailure::NotDispatched("adapter_busy"))?;
        self.process.call(request_id, operation, payload, timeout)
    }

    pub fn upload_reader(
        &self,
        request: &BulkFileRequest<'_>,
        source: &mut impl Read,
    ) -> Result<TransferReceipt, BulkTransferError> {
        let clock = TransferClock::new(request.timeout)?;
        let _lane = self.try_lane()?;
        validate_request(request)?;
        let transfer_id = Uuid::new_v4().to_string();
        let path = self.root.join(format!("{transfer_id}.upload"));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut staged = options
            .open(&path)
            .map_err(|_| BulkTransferError::NotDispatched("bulk_spool_unavailable"))?;
        let owned = OwnedTransfer::from_file(path.clone(), &staged)
            .map_err(|_| BulkTransferError::NotDispatched("bulk_spool_identity_unavailable"))?;
        let staged_result = copy_hash(source, &mut staged, request.max_bytes, &clock, false)
            .and_then(|stats| {
                staged
                    .flush()
                    .map_err(|_| BulkTransferError::NotDispatched("bulk_spool_write_failed"))?;
                Ok(stats)
            });
        drop(staged);
        let result = staged_result.and_then(|(length, digest)| {
            let remaining = clock.remaining(false)?;
            let mut payload = json!({"sandboxId":request.sandbox_id,"path":request.path,
                "transferId":transfer_id,"byteLength":length,"sha256":digest});
            add_user(&mut payload, request.user);
            let response = self
                .process
                .call(&transfer_id, "uploadFile", payload, remaining);
            let receipt = response_receipt(response, request, &transfer_id)?;
            if receipt.byte_length != length || receipt.sha256 != digest {
                return Err(BulkTransferError::Unknown("bulk_integrity_mismatch"));
            }
            clock.remaining(true)?;
            Ok(receipt)
        });
        self.finish_transfer(&path, Some(owned), result)
    }

    pub fn download_into(
        &self,
        request: &BulkFileRequest<'_>,
        destination: &mut impl Write,
    ) -> Result<TransferReceipt, BulkTransferError> {
        let clock = TransferClock::new(request.timeout)?;
        let _lane = self.try_lane()?;
        validate_request(request)?;
        if request.max_bytes == 0 {
            return Err(BulkTransferError::NotDispatched(
                "bulk_download_budget_invalid",
            ));
        }
        let transfer_id = Uuid::new_v4().to_string();
        let path = self.root.join(format!("{transfer_id}.download"));
        let mut payload = json!({"sandboxId":request.sandbox_id,"path":request.path,
            "transferId":transfer_id,"maxBytes":request.max_bytes});
        add_user(&mut payload, request.user);
        let response = self.process.call(
            &transfer_id,
            "downloadFile",
            payload,
            clock.remaining(false)?,
        );
        let mut owned = None;
        let result = response_receipt(response, request, &transfer_id).and_then(|receipt| {
            let file = OwnedTransfer::open(&path)
                .map_err(|_| BulkTransferError::Unknown("bulk_spool_identity_unavailable"))?
                .ok_or(BulkTransferError::Unknown("bulk_download_missing"))?;
            let size = file
                .handle
                .as_file()
                .metadata()
                .map_err(|_| BulkTransferError::Unknown("bulk_spool_identity_unavailable"))?
                .len();
            owned = Some(file);
            if size != receipt.byte_length {
                return Err(BulkTransferError::Unknown("bulk_integrity_mismatch"));
            }
            let source = owned
                .as_mut()
                .ok_or(BulkTransferError::Unknown("bulk_download_missing"))?;
            let (length, digest) = copy_hash(
                source.handle.as_file_mut(),
                destination,
                request.max_bytes,
                &clock,
                true,
            )?;
            if length != receipt.byte_length || digest != receipt.sha256 {
                return Err(BulkTransferError::Unknown("bulk_integrity_mismatch"));
            }
            Ok(receipt)
        });
        self.finish_transfer(&path, owned, result)
    }

    pub fn close(&self) -> Result<(), BulkTransferError> {
        self.process
            .stop_and_wait()
            .map_err(|_| BulkTransferError::Unknown("bulk_child_stop_unconfirmed"))
    }

    fn try_lane(&self) -> Result<MutexGuard<'_, ()>, BulkTransferError> {
        self.lane
            .try_lock()
            .map_err(|_| BulkTransferError::NotDispatched("adapter_busy"))
    }

    fn finish_transfer<T>(
        &self,
        path: &Path,
        owned: Option<OwnedTransfer>,
        result: Result<T, BulkTransferError>,
    ) -> Result<T, BulkTransferError> {
        if matches!(result, Err(BulkTransferError::Unknown(_))) {
            // A timed-out SDK may still hold a spool writer. Confirm child exit
            // before local cleanup; this proves nothing about remote side effects.
            self.close()?;
        }
        let owned = match owned {
            Some(file) => Some(file),
            None => OwnedTransfer::open(path)
                .map_err(|_| BulkTransferError::Unknown("bulk_local_cleanup_pending"))?,
        };
        if let Some(file) = owned {
            file.remove()
                .map_err(|_| BulkTransferError::Unknown("bulk_local_cleanup_pending"))?;
        }
        result
    }
}

impl E2bCreationAdapter for SpoolingAdapterProcess {
    fn create(
        &self,
        request_id: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        self.call(request_id, "create", payload, timeout)
    }
}

impl Drop for SpoolingAdapterProcess {
    fn drop(&mut self) {
        let stopped = self.process.stop_and_wait().is_ok();
        if let Some(spool) = self.spool.take() {
            if let Err(retained) = finish_spool(spool, stopped) {
                // Private host diagnostic only. Never included in adapter JSON.
                eprintln!("e2b bulk local cleanup pending: {}", retained.display());
            }
        }
    }
}

fn finish_spool(spool: TempDir, stop_confirmed: bool) -> Result<(), PathBuf> {
    if !stop_confirmed {
        return Err(spool.keep());
    }
    let path = spool.path().to_path_buf();
    spool.close().map_err(|_| path)
}

struct TransferClock {
    started: Instant,
    timeout: Duration,
}
impl TransferClock {
    fn new(timeout: Duration) -> Result<Self, BulkTransferError> {
        if timeout.is_zero() {
            return Err(BulkTransferError::NotDispatched("bulk_deadline_invalid"));
        }
        Ok(Self {
            started: Instant::now(),
            timeout,
        })
    }
    fn remaining(&self, dispatched: bool) -> Result<Duration, BulkTransferError> {
        let remaining = self.timeout.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(transfer_error(dispatched, "bulk_deadline_exceeded"));
        }
        Ok(remaining)
    }
}

fn transfer_error(dispatched: bool, code: &'static str) -> BulkTransferError {
    if dispatched {
        BulkTransferError::Unknown(code)
    } else {
        BulkTransferError::NotDispatched(code)
    }
}

fn copy_hash(
    source: &mut impl Read,
    destination: &mut impl Write,
    max_bytes: u64,
    clock: &TransferClock,
    dispatched: bool,
) -> Result<(u64, String), BulkTransferError> {
    let mut buffer = [0u8; TRANSFER_CHUNK_BYTES];
    let mut count = 0u64;
    let mut hash = Sha256::new();
    loop {
        clock.remaining(dispatched)?;
        // One extra byte detects growth without reading an unbounded source.
        let limit =
            (max_bytes.saturating_sub(count).saturating_add(1)).min(buffer.len() as u64) as usize;
        let size = source
            .read(&mut buffer[..limit])
            .map_err(|_| transfer_error(dispatched, "bulk_local_read_failed"))?;
        clock.remaining(dispatched)?;
        if size == 0 {
            break;
        }
        if count + size as u64 > max_bytes {
            return Err(transfer_error(dispatched, "bulk_transfer_limit_exceeded"));
        }
        destination
            .write_all(&buffer[..size])
            .map_err(|_| transfer_error(dispatched, "bulk_local_write_failed"))?;
        hash.update(&buffer[..size]);
        count += size as u64;
    }
    clock.remaining(dispatched)?;
    Ok((count, format!("{:x}", hash.finalize())))
}

fn validate_request(request: &BulkFileRequest<'_>) -> Result<(), BulkTransferError> {
    let text = |value: &str, limit: usize| {
        !value.is_empty() && value.len() <= limit && !value.contains('\0')
    };
    if !text(request.sandbox_id, 256)
        || !text(request.path, 4096)
        || !request.path.starts_with('/')
        || request.user.is_some_and(|user| !text(user, 256))
        || request.max_bytes > MAX_TRANSFER_BYTES
    {
        return Err(BulkTransferError::NotDispatched("bulk_request_invalid"));
    }
    Ok(())
}

fn add_user(payload: &mut Value, user: Option<&str>) {
    if let Some(user) = user {
        payload["user"] = json!(user);
    }
}

fn response_receipt(
    response: Result<AdapterResponse, AdapterFailure>,
    request: &BulkFileRequest<'_>,
    transfer_id: &str,
) -> Result<TransferReceipt, BulkTransferError> {
    let response = response.map_err(|error| match error {
        AdapterFailure::NotDispatched(code) => BulkTransferError::NotDispatched(code),
        AdapterFailure::Unknown(code) => BulkTransferError::Unknown(code),
    })?;
    match response.outcome {
        AdapterOutcome::NotDispatched => {
            return Err(BulkTransferError::NotDispatched("bulk_adapter_rejected"))
        }
        AdapterOutcome::Unknown => {
            return Err(BulkTransferError::Unknown("bulk_adapter_result_unknown"))
        }
        AdapterOutcome::Succeeded => {}
    }
    let receipt: TransferReceipt = serde_json::from_value(
        response
            .result
            .ok_or(BulkTransferError::Unknown("bulk_receipt_invalid"))?,
    )
    .map_err(|_| BulkTransferError::Unknown("bulk_receipt_invalid"))?;
    if response.request_id != transfer_id
        || receipt.sandbox_id != request.sandbox_id
        || receipt.transfer_id != transfer_id
        || receipt.byte_length > request.max_bytes
        || receipt.sha256.len() != 64
        || !receipt
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(BulkTransferError::Unknown(
            "bulk_receipt_identity_or_budget_rejected",
        ));
    }
    Ok(receipt)
}

struct OwnedTransfer {
    path: PathBuf,
    handle: Handle,
}
impl OwnedTransfer {
    fn from_file(path: PathBuf, file: &File) -> std::io::Result<Self> {
        Ok(Self {
            path,
            handle: Handle::from_file(file.try_clone()?)?,
        })
    }
    fn open(path: &Path) -> std::io::Result<Option<Self>> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !regular(&metadata) {
            return Err(std::io::Error::other("bulk spool file is not regular"));
        }
        let handle = Handle::from_path(path)?;
        if !regular(&handle.as_file().metadata()?) || handle != Handle::from_path(path)? {
            return Err(std::io::Error::other("bulk spool identity changed"));
        }
        Ok(Some(Self {
            path: path.into(),
            handle,
        }))
    }
    fn remove(self) -> std::io::Result<()> {
        let Some(current) = Self::open(&self.path)? else {
            return Ok(());
        };
        if current.handle != self.handle {
            return Err(std::io::Error::other("bulk spool replacement preserved"));
        }
        fs::remove_file(&self.path)
    }
}

fn regular(metadata: &Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}
