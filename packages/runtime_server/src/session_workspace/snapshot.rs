//! Existing workspace snapshot framing shared by hosted execution providers.
use crate::agent_run_authorization::SessionWorkspace;
use crate::observations::{MeasuredWriter, ProtectionMetric, WorkspaceProtection};
use centaeris_core::runtime::contracts::RecoveryWorkspaceSnapshotV1;
use hosted_execution::protocol::{
    SandboxSnapshotCaptureRequest, SandboxSnapshotCaptureResponse, SandboxWorkspaceSnapshotFile,
    SandboxWorkspaceSnapshotManifest, SnapshotCaptureDisposition, SANDBOX_SNAPSHOT_CAPTURE_SCHEMA,
    SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Cursor, Read, Seek, Write};
use unicode_normalization::UnicodeNormalization;
pub(crate) const SESSION_WORKSPACE_MANIFEST_LIMIT: usize = 1024 * 1024;
const SESSION_WORKSPACE_IO_BUFFER_BYTES: usize = 64 * 1024;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceSnapshotDescriptor {
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) expanded_size_bytes: u64,
    pub(crate) file_count: u32,
}

pub(crate) struct ImmutableWorkspaceSnapshot {
    pub(crate) file: fs::File,
    pub(crate) descriptor: WorkspaceSnapshotDescriptor,
    pub(crate) observation: WorkspaceProtection,
    pub(crate) manifest: Option<SandboxWorkspaceSnapshotManifest>,
}

struct SnapshotSpoolReader<'a, R, W> {
    source: &'a mut R,
    file: &'a mut W,
    remaining: u64,
}

impl<R: Read, W: Write> Read for SnapshotSpoolReader<'_, R, W> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.source.read(bytes)?;
        if count as u64 > self.remaining {
            return Err(std::io::Error::other("snapshot exceeds spool capacity"));
        }
        self.file.write_all(&bytes[..count])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

#[cfg(test)]
pub(crate) fn capture_snapshot_artifact(
    source: &mut impl Read,
    limit: u64,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    capture_snapshot_artifact_observed(source, limit, WorkspaceProtection::new())
}

pub(crate) fn capture_snapshot_artifact_observed(
    source: &mut impl Read,
    limit: u64,
    observation: WorkspaceProtection,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    let mut file =
        tempfile::tempfile().map_err(|error| format!("create snapshot spool failed: {error}"))?;
    let mut measured = MeasuredWriter {
        inner: &mut file,
        written: 0,
    };
    let descriptor = inspect_workspace_snapshot_with_manifest(&mut SnapshotSpoolReader {
        source,
        file: &mut measured,
        remaining: limit,
    });
    observation.count(
        ProtectionMetric::SpoolWrittenBytes,
        measured.written,
        descriptor.is_ok(),
    );
    let (descriptor, manifest) = descriptor?;
    file.rewind()
        .map_err(|error| format!("rewind snapshot spool failed: {error}"))?;
    Ok(ImmutableWorkspaceSnapshot {
        file,
        descriptor,
        observation,
        manifest,
    })
}

pub(crate) struct SessionWorkspaceUploadBody {
    pub(crate) prefix: Cursor<Vec<u8>>,
    pub(crate) snapshot: fs::File,
}

impl Read for SessionWorkspaceUploadBody {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let prefix = self.prefix.read(buffer)?;
        if prefix != 0 {
            return Ok(prefix);
        }
        self.snapshot.read(buffer)
    }
}

#[cfg(test)]
pub(crate) fn inspect_workspace_snapshot(
    source: &mut impl Read,
) -> Result<WorkspaceSnapshotDescriptor, String> {
    inspect_workspace_snapshot_with_manifest(source).map(|(descriptor, _)| descriptor)
}

#[cfg(test)]
pub(crate) fn inspect_workspace_snapshot_manifest_for_test(
    source: &mut impl Read,
) -> Result<Option<SandboxWorkspaceSnapshotManifest>, String> {
    inspect_workspace_snapshot_with_manifest(source).map(|(_, manifest)| manifest)
}

pub(crate) fn inspect_workspace_snapshot_with_manifest(
    source: &mut impl Read,
) -> Result<
    (
        WorkspaceSnapshotDescriptor,
        Option<SandboxWorkspaceSnapshotManifest>,
    ),
    String,
> {
    let mut header = [0_u8; 4];
    let first = source
        .read(&mut header)
        .map_err(|error| format!("read snapshot collect frame failed: {error}"))?;
    if first == 0 {
        return Ok((
            WorkspaceSnapshotDescriptor {
                sha256: String::new(),
                size_bytes: 0,
                expanded_size_bytes: 0,
                file_count: 0,
            },
            None,
        ));
    }
    let mut digest = Sha256::new();
    digest.update(&header[..first]);
    read_snapshot_exact(source, &mut header[first..], &mut digest)?;
    let manifest_length = u32::from_be_bytes(header) as usize;
    if manifest_length == 0 || manifest_length > SESSION_WORKSPACE_MANIFEST_LIMIT {
        return Err("snapshot manifest length is invalid".to_string());
    }
    let mut manifest_bytes = vec![0_u8; manifest_length];
    read_snapshot_exact(source, manifest_bytes.as_mut_slice(), &mut digest)?;
    let manifest =
        serde_json::from_slice::<SandboxWorkspaceSnapshotManifest>(manifest_bytes.as_slice())
            .map_err(|error| format!("decode snapshot manifest failed: {error}"))?;
    if serde_json::to_vec(&manifest)
        .map_err(|error| format!("encode snapshot manifest failed: {error}"))?
        != manifest_bytes
    {
        return Err("snapshot manifest is not canonical".to_string());
    }
    let expanded_size_bytes = validate_workspace_snapshot_manifest(&manifest)?;
    for file in &manifest.files {
        let mut file_digest = Sha256::new();
        let mut remaining = file.size_bytes;
        let mut buffer = [0_u8; SESSION_WORKSPACE_IO_BUFFER_BYTES];
        while remaining != 0 {
            let wanted = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| "snapshot file size is invalid".to_string())?;
            read_snapshot_exact(source, &mut buffer[..wanted], &mut digest)?;
            file_digest.update(&buffer[..wanted]);
            remaining -= wanted as u64;
        }
        if format!("sha256:{:x}", file_digest.finalize()) != file.sha256 {
            return Err("snapshot file integrity mismatch".to_string());
        }
    }
    let mut trailing = [0_u8; 1];
    if source
        .read(&mut trailing)
        .map_err(|error| format!("read snapshot collect trailing bytes failed: {error}"))?
        != 0
    {
        return Err("snapshot collect frame has trailing bytes".to_string());
    }
    let size_bytes = 4_u64
        .checked_add(manifest_length as u64)
        .and_then(|value| value.checked_add(expanded_size_bytes))
        .ok_or_else(|| "snapshot size overflow".to_string())?;
    Ok((
        WorkspaceSnapshotDescriptor {
            sha256: format!("sha256:{:x}", digest.finalize()),
            size_bytes,
            expanded_size_bytes,
            file_count: u32::try_from(manifest.files.len())
                .map_err(|_| "snapshot file count overflow".to_string())?,
        },
        Some(manifest),
    ))
}

pub(crate) fn snapshot_matches_frozen_workspace(
    descriptor: &WorkspaceSnapshotDescriptor,
    frozen: &SessionWorkspace,
) -> bool {
    descriptor.sha256 == frozen.snapshot_sha256
        && descriptor.size_bytes == frozen.snapshot_size_bytes
        && descriptor.expanded_size_bytes == frozen.expanded_size_bytes
        && descriptor.file_count == frozen.file_count
}

pub(crate) fn snapshot_matches_recovery_workspace(
    descriptor: &WorkspaceSnapshotDescriptor,
    snapshot: &RecoveryWorkspaceSnapshotV1,
) -> bool {
    descriptor.sha256 == snapshot.snapshot_sha256
        && descriptor.size_bytes == snapshot.snapshot_size_bytes
        && descriptor.expanded_size_bytes == snapshot.expanded_size_bytes
        && descriptor.file_count == snapshot.file_count
}

fn read_snapshot_exact(
    source: &mut impl Read,
    bytes: &mut [u8],
    digest: &mut Sha256,
) -> Result<(), String> {
    let mut offset = 0;
    while offset < bytes.len() {
        let count = source
            .read(&mut bytes[offset..])
            .map_err(|error| format!("read snapshot bytes failed: {error}"))?;
        if count == 0 {
            return Err("snapshot frame is truncated".to_string());
        }
        digest.update(&bytes[offset..offset + count]);
        offset += count;
    }
    Ok(())
}

pub(crate) fn validate_workspace_snapshot_manifest(
    manifest: &SandboxWorkspaceSnapshotManifest,
) -> Result<u64, String> {
    if manifest.schema != SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA || manifest.files.is_empty() {
        return Err("snapshot manifest is invalid".to_string());
    }
    let mut previous = None;
    let mut expanded_size_bytes = 0_u64;
    for SandboxWorkspaceSnapshotFile {
        path,
        size_bytes,
        sha256,
        ..
    } in &manifest.files
    {
        if path.is_empty()
            || path.trim() != path
            || path.len() > 4 * 1024
            || path.split('/').count() > 64
            || path.contains('\\')
            || path.starts_with('/')
            || path.chars().any(char::is_control)
            || path.split('/').any(|part| matches!(part, "" | "." | ".."))
            || path.nfc().collect::<String>() != *path
            || !is_workspace_sha256(sha256)
            || previous.is_some_and(|value: &str| value >= path.as_str())
            || previous.is_some_and(|value: &str| {
                path.strip_prefix(value)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            })
        {
            return Err("snapshot manifest paths or hashes are invalid".to_string());
        }
        expanded_size_bytes = expanded_size_bytes
            .checked_add(*size_bytes)
            .ok_or_else(|| "snapshot expanded size overflow".to_string())?;
        previous = Some(path.as_str());
    }
    Ok(expanded_size_bytes)
}

fn is_workspace_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn copy_exact(
    source: &mut impl Read,
    target: &mut impl Write,
    expected_size_bytes: u64,
    expected_sha256: &str,
) -> Result<(), String> {
    let mut digest = Sha256::new();
    let mut remaining = expected_size_bytes;
    let mut buffer = [0_u8; SESSION_WORKSPACE_IO_BUFFER_BYTES];
    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| "snapshot download size is invalid".to_string())?;
        let count = source
            .read(&mut buffer[..wanted])
            .map_err(|error| format!("read snapshot download failed: {error}"))?;
        if count == 0 {
            return Err("snapshot download is truncated".to_string());
        }
        target
            .write_all(&buffer[..count])
            .map_err(|error| format!("write snapshot restore input failed: {error}"))?;
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let mut trailing = [0_u8; 1];
    if source
        .read(&mut trailing)
        .map_err(|error| format!("read snapshot download trailing bytes failed: {error}"))?
        != 0
        || format!("sha256:{:x}", digest.finalize()) != expected_sha256
    {
        return Err("snapshot download integrity mismatch".to_string());
    }
    Ok(())
}

impl From<&SessionWorkspace> for WorkspaceSnapshotDescriptor {
    fn from(value: &SessionWorkspace) -> Self {
        Self {
            sha256: value.snapshot_sha256.clone(),
            size_bytes: value.snapshot_size_bytes,
            expanded_size_bytes: value.expanded_size_bytes,
            file_count: value.file_count,
        }
    }
}

impl From<&RecoveryWorkspaceSnapshotV1> for WorkspaceSnapshotDescriptor {
    fn from(value: &RecoveryWorkspaceSnapshotV1) -> Self {
        Self {
            sha256: value.snapshot_sha256.clone(),
            size_bytes: value.snapshot_size_bytes,
            expanded_size_bytes: value.expanded_size_bytes,
            file_count: value.file_count,
        }
    }
}

pub(crate) enum SnapshotCaptureResult {
    Snapshot(ImmutableWorkspaceSnapshot),
    Unchanged,
}

pub(crate) fn read_capture<R: Read>(
    source: &mut R,
    request: &SandboxSnapshotCaptureRequest,
    spool: impl FnOnce(&mut R) -> Result<ImmutableWorkspaceSnapshot, String>,
) -> Result<SnapshotCaptureResult, String> {
    let mut length = [0_u8; 4];
    source
        .read_exact(&mut length)
        .map_err(|error| format!("read snapshot capture response length failed: {error}"))?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > 1024 {
        return Err("snapshot capture response length is invalid".into());
    }
    let mut bytes = vec![0; length];
    source
        .read_exact(&mut bytes)
        .map_err(|error| format!("read snapshot capture response failed: {error}"))?;
    let response: SandboxSnapshotCaptureResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("decode snapshot capture response failed: {error}"))?;
    if response.schema != SANDBOX_SNAPSHOT_CAPTURE_SCHEMA
        || response.capture_id != request.capture_id
    {
        return Err("snapshot capture response binding mismatch".into());
    }
    match response.disposition {
        SnapshotCaptureDisposition::Unchanged => {
            let baseline = request
                .baseline
                .as_ref()
                .ok_or("snapshot capture receipt has no baseline")?;
            if response.baseline_snapshot_sha256.as_deref()
                != Some(baseline.snapshot_sha256.as_str())
                || response.manifest_sha256 != manifest_digest(&baseline.manifest)?
            {
                return Err("snapshot capture receipt baseline mismatch".into());
            }
            let mut trailing = [0; 1];
            if source
                .read(&mut trailing)
                .map_err(|error| format!("read snapshot capture trailing bytes failed: {error}"))?
                != 0
            {
                return Err("snapshot capture receipt has trailing bytes".into());
            }
            Ok(SnapshotCaptureResult::Unchanged)
        }
        SnapshotCaptureDisposition::Snapshot => {
            if response.baseline_snapshot_sha256.is_some() {
                return Err("full snapshot capture response has a baseline receipt".into());
            }
            let artifact = spool(source)?;
            let empty = SandboxWorkspaceSnapshotManifest {
                schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
                files: vec![],
            };
            if response.manifest_sha256
                != manifest_digest(artifact.manifest.as_ref().unwrap_or(&empty))?
            {
                return Err("snapshot capture manifest binding mismatch".into());
            }
            Ok(SnapshotCaptureResult::Snapshot(artifact))
        }
    }
}

pub(crate) fn manifest_digest(
    manifest: &SandboxWorkspaceSnapshotManifest,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| format!("encode snapshot capture manifest failed: {error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}
