//! Tar/zstd workspace codec used by the host restore adapter. Stored identities remain unchanged.
//!
//! Consumers must write to an isolated destination and publish only after the
//! entire decode succeeds. An error can leave partial output. This module does
//! not provide authorization, capture consistency, filesystem extraction, or
//! durable publication. Empty workspaces retain the existing no-object form.

use hosted_execution::protocol::{SandboxWorkspaceSnapshotFile, SandboxWorkspaceSnapshotManifest};
use sha2::{Digest, Sha256};
use std::io::{self, BufReader, Read, Write};

const MANIFEST_PATH: &str = "workspace-manifest.json";
const CONTENT_PREFIX: &str = "workspace/";
const IO_BYTES: usize = 64 * 1024;
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_LONG_NAME_BYTES: u64 = (4096 + CONTENT_PREFIX.len() + 1) as u64;

#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    pub compressed_bytes: u64,
    /// Maximum sum of file body bytes; excludes the manifest and tar headers/padding.
    pub expanded_bytes: u64,
    pub file_count: u32,
    pub manifest_bytes: usize,
    pub window_log_max: u32,
}

/// Logical stream byte counts; these do not measure physical filesystem I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveSummary {
    /// SHA-256 of the complete, single compressed zstd frame.
    pub compressed_sha256: String,
    /// Compressed stream bytes written/read by a successful encode/decode.
    pub compressed_bytes: u64,
    /// Sum of file body bytes only; excludes manifest, tar headers/padding, and zstd framing.
    pub expanded_bytes: u64,
    /// Workspace files only; excludes the manifest and GNU long-name entries.
    pub file_count: u32,
    pub manifest: SandboxWorkspaceSnapshotManifest,
}

pub fn encode<R: Read>(
    manifest: &SandboxWorkspaceSnapshotManifest,
    mut open: impl FnMut(&SandboxWorkspaceSnapshotFile) -> io::Result<R>,
    output: impl Write,
    limits: ArchiveLimits,
) -> io::Result<ArchiveSummary> {
    encode_with(manifest, output, limits, |archive, file| {
        append_verified_file(archive, file, open(file)?)
    })
}

/// Encode concatenated file bodies in manifest order without seeking or staging
/// an uncompressed frame. The entire body must end at the last declared byte.
/// Keep output isolated until success: even a final source error can leave a
/// complete-looking compressed archive in the destination.
pub fn encode_ordered(
    manifest: &SandboxWorkspaceSnapshotManifest,
    mut body: impl Read,
    output: impl Write,
    limits: ArchiveLimits,
) -> io::Result<ArchiveSummary> {
    let summary = encode_with(manifest, output, limits, |archive, file| {
        append_verified_file(archive, file, body.by_ref().take(file.size_bytes))
    })?;
    if body.read(&mut [0; 1])? != 0 {
        return Err(invalid("ordered archive source has trailing bytes"));
    }
    Ok(summary)
}

type ArchiveWriter<W> = tar::Builder<zstd::stream::write::Encoder<'static, MeasuredWriter<W>>>;

fn encode_with<W: Write>(
    manifest: &SandboxWorkspaceSnapshotManifest,
    output: W,
    limits: ArchiveLimits,
    mut append: impl FnMut(&mut ArchiveWriter<W>, &SandboxWorkspaceSnapshotFile) -> io::Result<()>,
) -> io::Result<ArchiveSummary> {
    let expanded = validate_manifest(manifest, limits)?;
    let mut json = MeasuredWriter::new(Vec::new(), limits.manifest_bytes as u64);
    serde_json::to_writer(&mut json, manifest).map_err(invalid)?;
    let json = json.inner;
    let output = MeasuredWriter::new(output, limits.compressed_bytes);
    let mut encoder = zstd::stream::write::Encoder::new(output, 3)?;
    encoder.window_log(limits.window_log_max)?;
    encoder.include_checksum(true)?;
    let mut archive = tar::Builder::new(encoder);
    append_file(
        &mut archive,
        MANIFEST_PATH,
        json.len() as u64,
        false,
        json.as_slice(),
    )?;
    for file in &manifest.files {
        append(&mut archive, file)?;
    }
    let output = archive.into_inner()?.finish()?;
    Ok(summary(
        manifest.clone(),
        expanded,
        output.bytes,
        output.digest,
    ))
}

/// Convert to the existing v1 manifest-length/JSON/body stream. Output must be
/// isolated until success; a later archive integrity error can follow emitted
/// v1 bytes. Returned byte counts describe the compressed input and file bodies,
/// not the output frame size or physical I/O.
pub fn decode_to_v1(
    input: impl Read,
    output: impl Write,
    limits: ArchiveLimits,
) -> io::Result<ArchiveSummary> {
    decode_with(input, V1Consumer(output), limits)
}

/// The consumer must stage output privately. No file may become authoritative
/// until this function has verified the archive end and returned successfully.
pub fn decode(
    input: impl Read,
    consume: impl FnMut(&SandboxWorkspaceSnapshotFile, &mut dyn Read) -> io::Result<()>,
    limits: ArchiveLimits,
) -> io::Result<ArchiveSummary> {
    decode_with(input, FileConsumer(consume), limits)
}

trait ArchiveConsumer {
    fn manifest(&mut self, _json: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn file(
        &mut self,
        file: &SandboxWorkspaceSnapshotFile,
        source: &mut dyn Read,
    ) -> io::Result<()>;
}

struct FileConsumer<F>(F);

impl<F> ArchiveConsumer for FileConsumer<F>
where
    F: FnMut(&SandboxWorkspaceSnapshotFile, &mut dyn Read) -> io::Result<()>,
{
    fn file(
        &mut self,
        file: &SandboxWorkspaceSnapshotFile,
        source: &mut dyn Read,
    ) -> io::Result<()> {
        self.0(file, source)
    }
}

struct V1Consumer<W>(W);

impl<W: Write> ArchiveConsumer for V1Consumer<W> {
    fn manifest(&mut self, json: &[u8]) -> io::Result<()> {
        let size = u32::try_from(json.len()).map_err(invalid)?;
        self.0.write_all(&size.to_be_bytes())?;
        self.0.write_all(json)
    }

    fn file(
        &mut self,
        _file: &SandboxWorkspaceSnapshotFile,
        source: &mut dyn Read,
    ) -> io::Result<()> {
        io::copy(source, &mut self.0)?;
        Ok(())
    }
}

fn decode_with(
    input: impl Read,
    mut consume: impl ArchiveConsumer,
    limits: ArchiveLimits,
) -> io::Result<ArchiveSummary> {
    validate_limits(limits)?;
    let input = MeasuredReader::new(input, limits.compressed_bytes);
    let mut decoder =
        zstd::stream::read::Decoder::with_buffer(BufReader::with_capacity(IO_BYTES, input))?
            .single_frame();
    decoder.window_log_max(limits.window_log_max)?;
    let mut archive = tar::Archive::new(decoder);
    let manifest;
    let expanded;
    {
        // Raw iteration exposes extension headers instead of allowing the tar
        // library to allocate arbitrary PAX/long-name payloads on our behalf.
        let mut entries = archive.entries()?.raw(true);
        let mut entry = entries
            .next()
            .ok_or_else(|| invalid("missing manifest"))??;
        require_regular_header(entry.header(), 0o644)?;
        if entry.path_bytes().as_ref() != MANIFEST_PATH.as_bytes()
            || entry.size() == 0
            || entry.size() > limits.manifest_bytes as u64
        {
            return Err(invalid("archive manifest entry is invalid"));
        }
        let mut json = vec![0; entry.size() as usize];
        entry.read_exact(&mut json)?;
        manifest =
            serde_json::from_slice::<SandboxWorkspaceSnapshotManifest>(&json).map_err(invalid)?;
        if serde_json::to_vec(&manifest).map_err(invalid)? != json {
            return Err(invalid("archive manifest is not canonical"));
        }
        expanded = validate_manifest(&manifest, limits)?;
        consume.manifest(&json)?;
        drop(entry);
        for file in &manifest.files {
            let mut entry = entries
                .next()
                .ok_or_else(|| invalid("missing archive file"))??;
            let path = if entry.header().entry_type() == tar::EntryType::GNULongName {
                if entry.path_bytes().as_ref() != b"././@LongLink"
                    || entry.size() <= 100
                    || entry.size() > MAX_LONG_NAME_BYTES
                {
                    return Err(invalid("archive long name is invalid"));
                }
                let mut name = vec![0; entry.size() as usize];
                entry.read_exact(&mut name)?;
                if name.pop() != Some(0) || name.contains(&0) {
                    return Err(invalid("archive long name is invalid"));
                }
                drop(entry);
                entry = entries
                    .next()
                    .ok_or_else(|| invalid("orphan archive long name"))??;
                name
            } else {
                entry.path_bytes().into_owned()
            };
            require_regular_header(entry.header(), if file.executable { 0o755 } else { 0o644 })?;
            if path != format!("{CONTENT_PREFIX}{}", file.path).as_bytes()
                || entry.size() != file.size_bytes
            {
                return Err(invalid("archive file does not match manifest"));
            }
            let mut source = VerifiedReader::new(&mut entry, file);
            consume.file(file, &mut source)?;
            source.require_complete()?;
        }
        if entries.next().transpose()?.is_some() {
            return Err(invalid("archive has unexpected entries"));
        }
    }
    // tar iteration consumed the first zero end block. Require the second,
    // then exact decompressed EOF, including the zstd checksum/trailer.
    let mut decoder = archive.into_inner();
    let mut final_block = [0; 512];
    decoder.read_exact(&mut final_block)?;
    if final_block != [0; 512] || decoder.read(&mut [0; 1])? != 0 {
        return Err(invalid("archive has an invalid end or trailing tar bytes"));
    }
    // single_frame prevents zstd from accepting concatenated/skippable frames.
    // The buffered source must also be at EOF, not merely the first decoder.
    let mut input = decoder.finish();
    if input.read(&mut [0; 1])? != 0 {
        return Err(invalid("archive has trailing compressed bytes"));
    }
    let input = input.into_inner();
    Ok(summary(manifest, expanded, input.bytes, input.digest))
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn validate_limits(limits: ArchiveLimits) -> io::Result<()> {
    if limits.compressed_bytes == 0
        || limits.file_count == 0
        || limits.manifest_bytes == 0
        || limits.manifest_bytes > MAX_MANIFEST_BYTES
        || !(10..=27).contains(&limits.window_log_max)
    {
        return Err(invalid("archive limits are invalid"));
    }
    Ok(())
}

fn validate_manifest(
    manifest: &SandboxWorkspaceSnapshotManifest,
    limits: ArchiveLimits,
) -> io::Result<u64> {
    validate_limits(limits)?;
    let expanded = crate::docker_execution_host::validate_workspace_snapshot_manifest(manifest)
        .map_err(invalid)?;
    if manifest.files.len() as u64 > u64::from(limits.file_count)
        || expanded > limits.expanded_bytes
    {
        return Err(invalid("archive exceeds declared workspace limits"));
    }
    Ok(expanded)
}

fn summary(
    manifest: SandboxWorkspaceSnapshotManifest,
    expanded: u64,
    bytes: u64,
    digest: Sha256,
) -> ArchiveSummary {
    ArchiveSummary {
        compressed_sha256: format!("sha256:{:x}", digest.finalize()),
        compressed_bytes: bytes,
        expanded_bytes: expanded,
        file_count: manifest.files.len() as u32,
        manifest,
    }
}

fn append_file<W: Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    size: u64,
    executable: bool,
    source: impl Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(if executable { 0o755 } else { 0o644 });
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    archive.append_data(&mut header, path, source)
}

fn append_verified_file<W: Write>(
    archive: &mut tar::Builder<W>,
    file: &SandboxWorkspaceSnapshotFile,
    source: impl Read,
) -> io::Result<()> {
    let mut source = VerifiedReader::new(source, file);
    append_file(
        archive,
        &format!("{CONTENT_PREFIX}{}", file.path),
        file.size_bytes,
        file.executable,
        &mut source,
    )?;
    source.require_complete()
}

fn require_regular_header(header: &tar::Header, mode: u32) -> io::Result<()> {
    if header.entry_type() != tar::EntryType::Regular
        || header.mode()? != mode
        || header.uid()? != 0
        || header.gid()? != 0
        || header.mtime()? != 0
        || header.link_name()?.is_some()
    {
        return Err(invalid("archive entry type or metadata is invalid"));
    }
    Ok(())
}

struct VerifiedReader<'a, R> {
    inner: R,
    expected: &'a SandboxWorkspaceSnapshotFile,
    bytes: u64,
    digest: Sha256,
}

impl<'a, R: Read> VerifiedReader<'a, R> {
    fn new(inner: R, expected: &'a SandboxWorkspaceSnapshotFile) -> Self {
        Self {
            inner,
            expected,
            bytes: 0,
            digest: Sha256::new(),
        }
    }

    fn require_complete(&mut self) -> io::Result<()> {
        if self.bytes != self.expected.size_bytes
            || self.read(&mut [0; 1])? != 0
            || format!("sha256:{:x}", self.digest.clone().finalize()) != self.expected.sha256
        {
            return Err(invalid("archive file content integrity mismatch"));
        }
        Ok(())
    }
}

impl<R: Read> Read for VerifiedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = self.expected.size_bytes - self.bytes;
        if remaining == 0 {
            if self.inner.read(&mut [0; 1])? != 0 {
                return Err(invalid("archive file exceeds manifest size"));
            }
            return Ok(0);
        }
        let wanted = buffer
            .len()
            .min(IO_BYTES)
            .min(remaining.min(usize::MAX as u64) as usize);
        let count = self.inner.read(&mut buffer[..wanted])?;
        if count == 0 {
            return Err(invalid("archive file is truncated"));
        }
        self.bytes += count as u64;
        self.digest.update(&buffer[..count]);
        Ok(count)
    }
}

struct MeasuredWriter<W> {
    inner: W,
    limit: u64,
    bytes: u64,
    digest: Sha256,
}

impl<W> MeasuredWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            limit,
            bytes: 0,
            digest: Sha256::new(),
        }
    }
}

impl<W: Write> Write for MeasuredWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit - self.bytes {
            return Err(invalid("archive output exceeds byte limit"));
        }
        let count = self.inner.write(bytes)?;
        self.bytes += count as u64;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct MeasuredReader<R> {
    inner: R,
    limit: u64,
    bytes: u64,
    digest: Sha256,
}

impl<R> MeasuredReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            limit,
            bytes: 0,
            digest: Sha256::new(),
        }
    }
}

impl<R: Read> Read for MeasuredReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let wanted = buffer.len().min(IO_BYTES).min(
            (self.limit - self.bytes)
                .saturating_add(1)
                .min(usize::MAX as u64) as usize,
        );
        let count = self.inner.read(&mut buffer[..wanted])?;
        if count as u64 > self.limit - self.bytes {
            return Err(invalid("archive input exceeds compressed byte limit"));
        }
        self.bytes += count as u64;
        self.digest.update(&buffer[..count]);
        Ok(count)
    }
}

#[cfg(test)]
mod tests;
