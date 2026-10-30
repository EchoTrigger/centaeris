//! Session-scoped content objects beneath an immutable workspace manifest.
use super::*;

pub(super) const ENCODING: &str = "fileManifestV1";
const FILE_BATCH_SIZE: usize = 256;
pub(super) fn encoding(snapshot_size_bytes: u64) -> &'static str {
    if snapshot_size_bytes == 0 {
        "frameV1"
    } else {
        ENCODING
    }
}

const MISSING_SCHEMA: &str = "runtime.workspace_files.missing.v1";
const MISSING_RESULT_SCHEMA: &str = "runtime.workspace_files.missing.result.v1";
const UPLOAD_SCHEMA: &str = "runtime.workspace_files.upload.v1";
const UPLOAD_RESULT_SCHEMA: &str = "runtime.workspace_files.upload.result.v1";
const DOWNLOAD_SCHEMA: &str = "runtime.workspace_files.download.v1";

pub(super) struct CapturedFiles {
    frame: fs::File,
    ranges: Vec<FileRange>,
}

struct FileRange {
    sha256: String,
    size_bytes: u64,
    offset: u64,
}

/// Reuse the validated capture's manifest bytes and body digests. The source
/// frame remains private until missing bodies have been acknowledged by API.
pub(super) fn prepare(artifact: &mut ImmutableWorkspaceSnapshot) -> Result<(), String> {
    let Some(manifest) = &artifact.manifest else {
        return Ok(());
    };
    if artifact.file_source.is_some() {
        return Ok(());
    }
    artifact.file.rewind().map_err(spool_error)?;
    let mut header = [0_u8; 4];
    artifact.file.read_exact(&mut header).map_err(spool_error)?;
    let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
    artifact.file.read_exact(&mut bytes).map_err(spool_error)?;
    let descriptor = WorkspaceSnapshotDescriptor {
        sha256: format!("sha256:{:x}", Sha256::digest(&bytes)),
        size_bytes: bytes.len() as u64,
        expanded_size_bytes: artifact.descriptor.expanded_size_bytes,
        file_count: artifact.descriptor.file_count,
    };
    let mut offset = 4 + descriptor.size_bytes;
    let ranges = manifest
        .files
        .iter()
        .map(|file| {
            let range = FileRange {
                sha256: file.sha256.clone(),
                size_bytes: file.size_bytes,
                offset,
            };
            offset += file.size_bytes;
            range
        })
        .collect();
    let mut stored = tempfile::tempfile().map_err(spool_error)?;
    stored.write_all(&bytes).map_err(spool_error)?;
    stored.rewind().map_err(spool_error)?;
    let frame = std::mem::replace(&mut artifact.file, stored);
    artifact.file_source = Some(Box::new(CapturedFiles { frame, ranges }));
    artifact.descriptor = descriptor;
    artifact.observation.count(
        ProtectionMetric::SpoolWrittenBytes,
        bytes.len() as u64,
        true,
    );
    Ok(())
}

fn spool_error(error: std::io::Error) -> String {
    format!("workspace file spool failed: {error}")
}

pub(super) struct FileAccess<'a> {
    client: &'a reqwest::blocking::Client,
    api_url: &'a str,
    api_token: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    lease: &'a SessionWorkspaceLease,
    checkpoint_id: Option<&'a str>,
    preservation: Option<&'a serde_json::Value>,
}

impl<'a> FileAccess<'a> {
    pub(super) fn new(
        client: &'a reqwest::blocking::Client,
        api_url: &'a str,
        api_token: &'a str,
        agent_run_id: &'a str,
        authorization_digest: &'a str,
        lease: &'a SessionWorkspaceLease,
        checkpoint_id: Option<&'a str>,
    ) -> Self {
        Self {
            client,
            api_url,
            api_token,
            agent_run_id,
            authorization_digest,
            lease,
            checkpoint_id,
            preservation: None,
        }
    }

    pub(super) fn with_preservation(mut self, proof: &'a serde_json::Value) -> Self {
        self.preservation = Some(proof);
        self
    }

    fn identity(&self, schema: &'static str) -> FileIdentity<'_> {
        FileIdentity {
            schema,
            job_id: &self.lease.job_id,
            lease_owner: &self.lease.lease_owner,
            agent_run_id: self.agent_run_id,
            authorization_digest: self.authorization_digest,
            preservation: self.preservation,
        }
    }

    pub(super) fn upload_missing(
        &self,
        artifact: &mut ImmutableWorkspaceSnapshot,
    ) -> Result<(), String> {
        let observation = artifact.observation.clone();
        observation.timed(ProtectionMetric::UploadElapsedMs, || {
            self.upload_missing_inner(artifact)
        })
    }

    fn upload_missing_inner(
        &self,
        artifact: &mut ImmutableWorkspaceSnapshot,
    ) -> Result<(), String> {
        let Some(source) = &mut artifact.file_source else {
            return Ok(());
        };
        let mut unique = HashSet::new();
        let files = source
            .ranges
            .iter()
            .filter(|file| unique.insert(file.sha256.as_str()))
            .map(|file| FileIdentityDescriptor {
                sha256: &file.sha256,
                size_bytes: file.size_bytes,
            })
            .collect();
        let response = self
            .client
            .post(format!(
                "{}/internal/agent-runs/workspace-files/missing",
                self.api_url
            ))
            .header("X-Internal-Token", self.api_token)
            .json(&MissingRequest {
                identity: self.identity(MISSING_SCHEMA),
                files,
            })
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|error| format!("workspace missing files request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "workspace missing files returned {}",
                response.status().as_u16()
            ));
        }
        let missing = response
            .json::<MissingResponse>()
            .map_err(|error| format!("workspace missing files response invalid: {error}"))?;
        if missing.schema != MISSING_RESULT_SCHEMA {
            return Err("workspace missing files response schema mismatch".into());
        }
        let mut missing_hashes = HashSet::new();
        for hash in missing.missing {
            if !unique.contains(hash.as_str()) || !missing_hashes.insert(hash) {
                return Err("workspace missing file identity mismatch".into());
            }
        }
        let selected: Vec<_> = source
            .ranges
            .iter()
            .filter(|range| missing_hashes.remove(range.sha256.as_str()))
            .collect();
        for batch in selected.chunks(FILE_BATCH_SIZE) {
            let metadata = serde_json::to_vec(&UploadRequest {
                identity: self.identity(UPLOAD_SCHEMA),
                files: batch
                    .iter()
                    .map(|range| FileIdentityDescriptor {
                        sha256: &range.sha256,
                        size_bytes: range.size_bytes,
                    })
                    .collect(),
            })
            .map_err(|error| format!("workspace file metadata invalid: {error}"))?;
            let mut prefix = (metadata.len() as u32).to_be_bytes().to_vec();
            prefix.extend(metadata);
            let length =
                prefix.len() as u64 + batch.iter().map(|range| range.size_bytes).sum::<u64>();
            let bodies = FileBatchReader {
                frame: source.frame.try_clone().map_err(spool_error)?,
                ranges: batch
                    .iter()
                    .map(|range| (range.offset, range.size_bytes))
                    .collect::<Vec<_>>()
                    .into_iter(),
                remaining: 0,
            };
            let body = Cursor::new(prefix).chain(bodies);
            artifact
                .observation
                .count(ProtectionMetric::UploadAttempts, 1, true);
            let response = self
                .client
                .post(format!(
                    "{}/internal/agent-runs/workspace-files/upload",
                    self.api_url
                ))
                .header("X-Internal-Token", self.api_token)
                .header("Content-Type", "application/octet-stream")
                .body(reqwest::blocking::Body::sized(
                    artifact.observation.upload_reader(body, length),
                    length,
                ))
                .timeout(Duration::from_secs(300))
                .send()
                .map_err(|error| format!("workspace file upload failed: {error}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "workspace file upload returned {}",
                    response.status().as_u16()
                ));
            }
            let uploaded = response
                .json::<UploadResponse>()
                .map_err(|error| format!("workspace file upload response invalid: {error}"))?;
            if uploaded.schema != UPLOAD_RESULT_SCHEMA
                || uploaded.files.len() != batch.len()
                || uploaded.files.iter().zip(batch).any(|(uploaded, range)| {
                    uploaded.sha256 != range.sha256 || uploaded.size_bytes != range.size_bytes
                })
            {
                return Err("workspace file upload response binding mismatch".into());
            }
        }
        // Keep the captured bytes until the manifest publication is acknowledged.
        // A lost publication response must leave the exact artifact replayable.
        Ok(())
    }

    fn download(
        &self,
        snapshot: &WorkspaceSnapshotDescriptor,
        files: &[SandboxWorkspaceSnapshotFile],
    ) -> Result<reqwest::blocking::Response, String> {
        let size_bytes = files.iter().map(|file| file.size_bytes).sum();
        let response = self
            .client
            .post(format!(
                "{}/internal/agent-runs/workspace-files/download",
                self.api_url
            ))
            .header("X-Internal-Token", self.api_token)
            .json(&DownloadRequest {
                identity: self.identity(DOWNLOAD_SCHEMA),
                checkpoint_id: self.checkpoint_id,
                snapshot: StoredSnapshotDescriptor {
                    snapshot_sha256: &snapshot.sha256,
                    snapshot_size_bytes: snapshot.size_bytes,
                    expanded_size_bytes: snapshot.expanded_size_bytes,
                    file_count: snapshot.file_count,
                },
                files: files
                    .iter()
                    .map(|file| FileIdentityDescriptor {
                        sha256: &file.sha256,
                        size_bytes: file.size_bytes,
                    })
                    .collect(),
            })
            .timeout(Duration::from_secs(300))
            .send()
            .map_err(|error| format!("workspace file download failed: {error}"))?;
        if !response.status().is_success() || response.content_length() != Some(size_bytes) {
            return Err("workspace file download binding mismatch".into());
        }
        Ok(response)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileIdentity<'a> {
    schema: &'static str,
    job_id: &'a str,
    lease_owner: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    preservation: Option<&'a serde_json::Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileIdentityDescriptor<'a> {
    sha256: &'a str,
    size_bytes: u64,
}

#[derive(Serialize)]
struct MissingRequest<'a> {
    #[serde(flatten)]
    identity: FileIdentity<'a>,
    files: Vec<FileIdentityDescriptor<'a>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MissingResponse {
    schema: String,
    missing: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadRequest<'a> {
    #[serde(flatten)]
    identity: FileIdentity<'a>,
    files: Vec<FileIdentityDescriptor<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UploadResponse {
    schema: String,
    files: Vec<UploadedFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UploadedFile {
    sha256: String,
    size_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadRequest<'a> {
    #[serde(flatten)]
    identity: FileIdentity<'a>,
    checkpoint_id: Option<&'a str>,
    snapshot: StoredSnapshotDescriptor<'a>,
    files: Vec<FileIdentityDescriptor<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredSnapshotDescriptor<'a> {
    snapshot_sha256: &'a str,
    snapshot_size_bytes: u64,
    expanded_size_bytes: u64,
    file_count: u32,
}

pub(super) fn restore_from_api(
    source: &mut impl Read,
    stored: &WorkspaceSnapshotDescriptor,
    expanded_limit: u64,
    observation: WorkspaceProtection,
    access: &FileAccess<'_>,
) -> Result<workspace_restore::PreparedWorkspaceRestore, String> {
    let download_observation = observation.clone();
    restore_with_writer(
        source,
        stored,
        expanded_limit,
        observation,
        |manifest, output| {
            for batch in manifest.files.chunks(FILE_BATCH_SIZE) {
                let mut input = access.download(stored, batch)?;
                download_observation.download(&mut input, |input| {
                    write_download_batch(input, batch, output)
                })?;
            }
            Ok(())
        },
    )
}

fn write_download_batch(
    input: &mut impl Read,
    files: &[SandboxWorkspaceSnapshotFile],
    output: &mut FrameWriter<'_>,
) -> Result<(), String> {
    for file in files {
        let mut body = input.by_ref().take(file.size_bytes);
        copy_exact(&mut body, output, file.size_bytes, &file.sha256)?;
    }
    let mut trailing = [0; 1];
    if input.read(&mut trailing).map_err(spool_error)? != 0 {
        return Err("workspace file batch has trailing bytes".into());
    }
    Ok(())
}

#[cfg(test)]
fn restore<R: Read>(
    source: &mut impl Read,
    stored: &WorkspaceSnapshotDescriptor,
    expanded_limit: u64,
    observation: WorkspaceProtection,
    mut open: impl FnMut(&SandboxWorkspaceSnapshotFile) -> Result<R, String>,
) -> Result<workspace_restore::PreparedWorkspaceRestore, String> {
    restore_with_writer(
        source,
        stored,
        expanded_limit,
        observation,
        |manifest, output| {
            for file in &manifest.files {
                let mut input = open(file)?;
                copy_exact(&mut input, output, file.size_bytes, &file.sha256)?;
            }
            Ok(())
        },
    )
}

fn restore_with_writer(
    source: &mut impl Read,
    stored: &WorkspaceSnapshotDescriptor,
    expanded_limit: u64,
    observation: WorkspaceProtection,
    write_files: impl FnOnce(
        &SandboxWorkspaceSnapshotManifest,
        &mut FrameWriter<'_>,
    ) -> Result<(), String>,
) -> Result<workspace_restore::PreparedWorkspaceRestore, String> {
    if stored.size_bytes > SESSION_WORKSPACE_MANIFEST_LIMIT as u64
        || stored.expanded_size_bytes > expanded_limit
    {
        return Err("workspace_restore_budget_exceeded".into());
    }
    let mut bytes = Vec::with_capacity(stored.size_bytes as usize);
    copy_exact(source, &mut bytes, stored.size_bytes, &stored.sha256)?;
    let manifest = serde_json::from_slice::<SandboxWorkspaceSnapshotManifest>(&bytes)
        .map_err(|error| format!("workspace file manifest invalid: {error}"))?;
    if serde_json::to_vec(&manifest)
        .map_err(|error| format!("workspace file manifest invalid: {error}"))?
        != bytes
        || validate_workspace_snapshot_manifest(&manifest)? != stored.expanded_size_bytes
        || manifest.files.len() as u64 != u64::from(stored.file_count)
    {
        return Err("workspace_restore_stored_binding_rejected".into());
    }
    let frame_size = 4_u64
        .checked_add(stored.size_bytes)
        .and_then(|size| size.checked_add(stored.expanded_size_bytes))
        .ok_or("workspace_restore_budget_exceeded")?;
    if frame_size > hosted_execution::host_supervisor::HELPER_STREAM_LIMIT {
        return Err("workspace_restore_budget_exceeded".into());
    }
    let mut frame = tempfile::tempfile().map_err(spool_error)?;
    let mut output = FrameWriter {
        file: &mut frame,
        digest: Sha256::new(),
    };
    output
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .map_err(spool_error)?;
    output.write_all(&bytes).map_err(spool_error)?;
    write_files(&manifest, &mut output)?;
    let descriptor = WorkspaceSnapshotDescriptor {
        sha256: format!("sha256:{:x}", output.digest.finalize()),
        size_bytes: frame_size,
        expanded_size_bytes: stored.expanded_size_bytes,
        file_count: stored.file_count,
    };
    observation.count(ProtectionMetric::SpoolWrittenBytes, frame_size, true);
    frame.rewind().map_err(spool_error)?;
    Ok(workspace_restore::PreparedWorkspaceRestore {
        stored_descriptor: stored.clone(),
        helper_frame: ImmutableWorkspaceSnapshot {
            file: frame,
            descriptor,
            manifest: Some(manifest),
            observation,
            file_source: None,
        },
    })
}

struct FileBatchReader {
    frame: fs::File,
    ranges: std::vec::IntoIter<(u64, u64)>,
    remaining: u64,
}

impl Read for FileBatchReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        while self.remaining == 0 {
            let Some((offset, size)) = self.ranges.next() else {
                return Ok(0);
            };
            self.frame.seek(std::io::SeekFrom::Start(offset))?;
            self.remaining = size;
        }
        let wanted = self.remaining.min(bytes.len() as u64) as usize;
        let count = self.frame.read(&mut bytes[..wanted])?;
        if count == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "workspace captured file is truncated",
            ));
        }
        self.remaining -= count as u64;
        Ok(count)
    }
}

struct FrameWriter<'a> {
    file: &'a mut fs::File,
    digest: Sha256,
}
impl Write for FrameWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.digest.update(&bytes[..written]);
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn fixture() -> (SandboxWorkspaceSnapshotManifest, Vec<u8>) {
        let files = [
            ("first.txt", b"old".as_slice(), false),
            ("next.txt", b"new".as_slice(), true),
            ("same.txt", b"old".as_slice(), false),
        ];
        let manifest = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: files
                .iter()
                .map(|(path, content, executable)| SandboxWorkspaceSnapshotFile {
                    path: (*path).into(),
                    sha256: format!("sha256:{:x}", Sha256::digest(content)),
                    size_bytes: content.len() as u64,
                    executable: *executable,
                })
                .collect(),
        };
        let frame = encode_frame(&manifest, b"oldnewold").unwrap();
        (manifest, frame)
    }

    fn captured() -> ImmutableWorkspaceSnapshot {
        let (_, frame) = fixture();
        capture_snapshot_artifact(&mut Cursor::new(frame), 4096).unwrap()
    }

    #[test]
    fn stored_manifest_identity_is_independent_of_the_helper_frame() {
        let (manifest, frame) = fixture();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let mut artifact = captured();
        let helper_identity = artifact.descriptor.clone();
        prepare(&mut artifact).unwrap();
        assert_eq!(artifact.descriptor.size_bytes, manifest_bytes.len() as u64);
        assert_eq!(artifact.descriptor.expanded_size_bytes, 9);
        assert_eq!(artifact.descriptor.file_count, 3);
        assert_eq!(
            artifact.descriptor.sha256,
            format!("sha256:{:x}", Sha256::digest(&manifest_bytes))
        );
        assert_ne!(artifact.descriptor.sha256, helper_identity.sha256);
        let mut stored = Vec::new();
        artifact.file.read_to_end(&mut stored).unwrap();
        assert_eq!(stored, manifest_bytes);
        let source = artifact.file_source.as_mut().unwrap();
        source.frame.rewind().unwrap();
        let mut source_bytes = Vec::new();
        source.frame.read_to_end(&mut source_bytes).unwrap();
        assert_eq!(source_bytes, frame);
        assert_eq!(source.ranges[1].offset, 4 + stored.len() as u64 + 3);
    }

    #[test]
    fn empty_capture_remains_the_existing_no_object_form() {
        let mut artifact = capture_snapshot_artifact(&mut Cursor::new([]), 4096).unwrap();
        prepare(&mut artifact).unwrap();
        assert_eq!(artifact.descriptor.size_bytes, 0);
        assert_eq!(artifact.descriptor.sha256, "");
        assert!(artifact.file_source.is_none());
        assert_eq!(encoding(artifact.descriptor.size_bytes), "frameV1");
    }

    #[test]
    fn restoring_manifest_reads_content_and_preserves_executable_and_stored_identity() {
        let (manifest, frame) = fixture();
        let mut stored = captured();
        prepare(&mut stored).unwrap();
        let mut requested = Vec::new();
        let mut restored = restore(
            &mut stored.file,
            &stored.descriptor,
            9,
            WorkspaceProtection::new(),
            |file| {
                requested.push(file.path.clone());
                Ok(Cursor::new(if file.path == "next.txt" {
                    b"new"
                } else {
                    b"old"
                }))
            },
        )
        .unwrap();
        assert_eq!(restored.stored_descriptor, stored.descriptor);
        assert_ne!(
            restored.helper_frame.descriptor.sha256,
            stored.descriptor.sha256
        );
        assert_eq!(restored.helper_frame.manifest, Some(manifest));
        let mut bytes = Vec::new();
        restored.helper_frame.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, frame);
        assert_eq!(requested, ["first.txt", "next.txt", "same.txt"]);
    }

    #[test]
    fn manifest_restore_rejects_wrong_content_and_expanded_budget_before_publication() {
        let mut stored = captured();
        prepare(&mut stored).unwrap();
        assert!(restore(
            &mut stored.file,
            &stored.descriptor,
            8,
            WorkspaceProtection::new(),
            |_| -> Result<Cursor<Vec<u8>>, String> {
                panic!("budget must reject before content reads")
            },
        )
        .is_err());
        assert!(restore(
            &mut stored.file,
            &stored.descriptor,
            9,
            WorkspaceProtection::new(),
            |_| Ok(Cursor::new(b"bad")),
        )
        .is_err());
    }

    fn read_request(socket: &mut TcpStream) -> (String, Vec<u8>) {
        let mut bytes = Vec::new();
        let end = loop {
            if let Some(index) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                break index + 4;
            }
            let mut buffer = [0; 4096];
            let count = socket.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buffer[..count]);
        };
        let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
        let length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
            })
            .unwrap();
        let mut body = bytes[end..].to_vec();
        body.resize(length, 0);
        let received = bytes.len() - end;
        socket.read_exact(&mut body[received..]).unwrap();
        (headers.lines().next().unwrap().into(), body)
    }

    fn respond(socket: &mut TcpStream, body: &serde_json::Value) {
        let bytes = serde_json::to_vec(body).unwrap();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
        socket.write_all(&bytes).unwrap();
    }

    #[test]
    fn api_upload_sends_only_missing_file_bodies_and_deduplicates_manifest_references() {
        let mut artifact = captured();
        prepare(&mut artifact).unwrap();
        let missing_hash = artifact.manifest.as_ref().unwrap().files[1].sha256.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let (path, body) = read_request(&mut socket);
            assert!(path.starts_with("POST /internal/agent-runs/workspace-files/missing "));
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["schema"], MISSING_SCHEMA);
            assert_eq!(request["files"].as_array().unwrap().len(), 2);
            assert_eq!(request["jobId"], "original-job");
            assert_eq!(request["leaseOwner"], "original-lease");
            assert!(request.get("preservation").is_none());
            respond(
                &mut socket,
                &serde_json::json!({
                    "schema": MISSING_RESULT_SCHEMA, "missing": [missing_hash],
                }),
            );
            drop(socket);
            let (mut socket, _) = listener.accept().unwrap();
            let (path, body) = read_request(&mut socket);
            assert!(path.starts_with("POST /internal/agent-runs/workspace-files/upload "));
            let length = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
            let metadata: serde_json::Value = serde_json::from_slice(&body[4..4 + length]).unwrap();
            assert_eq!(metadata["schema"], UPLOAD_SCHEMA);
            assert_eq!(
                metadata["files"],
                serde_json::json!([{"sha256": missing_hash, "sizeBytes": 3}])
            );
            assert_eq!(&body[4 + length..], b"new");
            respond(
                &mut socket,
                &serde_json::json!({
                    "schema": UPLOAD_RESULT_SCHEMA, "files": [{"sha256": missing_hash, "sizeBytes": 3}],
                }),
            );
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let lease = SessionWorkspaceLease {
            job_id: "original-job".into(),
            lease_owner: "original-lease".into(),
        };
        FileAccess::new(
            &client,
            &format!("http://{address}"),
            "token",
            "original-run",
            "original-digest",
            &lease,
            None,
        )
        .upload_missing(&mut artifact)
        .unwrap();
        worker.join().unwrap();
        let mut manifest = Vec::new();
        artifact.file.read_to_end(&mut manifest).unwrap();
        assert_eq!(
            manifest,
            serde_json::to_vec(artifact.manifest.as_ref().unwrap()).unwrap()
        );
    }

    #[test]
    fn object_downloads_bind_the_exact_checkpoint_and_stored_manifest() {
        let (manifest, expected_frame) = fixture();
        let mut stored = captured();
        prepare(&mut stored).unwrap();
        let expected_snapshot = stored.descriptor.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let (path, body) = read_request(&mut socket);
            assert!(path.starts_with("POST /internal/agent-runs/workspace-files/download "));
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["schema"], DOWNLOAD_SCHEMA);
            assert_eq!(request["checkpointId"], "selected-checkpoint");
            assert_eq!(
                request["snapshot"]["snapshotSha256"],
                expected_snapshot.sha256
            );
            assert_eq!(
                request["snapshot"]["snapshotSizeBytes"],
                expected_snapshot.size_bytes
            );
            let expected_files: Vec<_> = manifest
                .files
                .iter()
                .map(|file| {
                    serde_json::json!({
                        "sha256": file.sha256, "sizeBytes": file.size_bytes,
                    })
                })
                .collect();
            assert_eq!(request["files"], serde_json::json!(expected_files));
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            socket.write_all(b"oldnewold").unwrap();
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let lease = SessionWorkspaceLease {
            job_id: "original-job".into(),
            lease_owner: "original-lease".into(),
        };
        let api_url = format!("http://{address}");
        let access = FileAccess::new(
            &client,
            &api_url,
            "token",
            "original-run",
            "original-digest",
            &lease,
            Some("selected-checkpoint"),
        );
        let mut result = restore_from_api(
            &mut stored.file,
            &stored.descriptor,
            9,
            WorkspaceProtection::new(),
            &access,
        )
        .unwrap();
        let mut frame = Vec::new();
        result.helper_frame.file.read_to_end(&mut frame).unwrap();
        assert_eq!(frame, expected_frame);
        worker.join().unwrap();
    }

    #[test]
    fn batches_stream_missing_files_and_restore_them_in_multiple_requests() {
        let file_count = 2 * FILE_BATCH_SIZE + 1;
        let contents: Vec<Vec<u8>> = (0..file_count as u32)
            .map(|index| {
                if index == 0 {
                    Vec::new()
                } else {
                    index.to_be_bytes().to_vec()
                }
            })
            .collect();
        let manifest = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: contents
                .iter()
                .enumerate()
                .map(|(index, bytes)| SandboxWorkspaceSnapshotFile {
                    path: format!("file-{index:03}.bin"),
                    sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                    size_bytes: bytes.len() as u64,
                    executable: false,
                })
                .collect(),
        };
        let bodies: Vec<u8> = contents.iter().flatten().copied().collect();
        let expected_frame = encode_frame(&manifest, &bodies).unwrap();
        let mut artifact = capture_snapshot_artifact(
            &mut Cursor::new(&expected_frame),
            expected_frame.len() as u64,
        )
        .unwrap();
        prepare(&mut artifact).unwrap();
        let expected_snapshot = artifact.descriptor.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let (path, body) = read_request(&mut socket);
            assert!(path.starts_with("POST /internal/agent-runs/workspace-files/missing "));
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["files"].as_array().unwrap().len(), file_count);
            let missing: Vec<_> = manifest
                .files
                .iter()
                .rev()
                .map(|file| file.sha256.clone())
                .collect();
            respond(
                &mut socket,
                &serde_json::json!({"schema": MISSING_RESULT_SCHEMA, "missing": missing}),
            );
            drop(socket);
            for (batch_index, batch) in manifest.files.chunks(FILE_BATCH_SIZE).enumerate() {
                let expected_files: Vec<_> = batch
                    .iter()
                    .map(|file| {
                        serde_json::json!({
                            "sha256": file.sha256, "sizeBytes": file.size_bytes,
                        })
                    })
                    .collect();
                let expected_body: Vec<u8> = contents
                    [batch_index * FILE_BATCH_SIZE..batch_index * FILE_BATCH_SIZE + batch.len()]
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
                let (mut socket, _) = listener.accept().unwrap();
                let (path, body) = read_request(&mut socket);
                assert!(path.starts_with("POST /internal/agent-runs/workspace-files/upload "));
                let prefix_size = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
                let metadata: serde_json::Value =
                    serde_json::from_slice(&body[4..4 + prefix_size]).unwrap();
                assert_eq!(metadata["schema"], UPLOAD_SCHEMA);
                assert_eq!(metadata["files"], serde_json::json!(expected_files));
                assert_eq!(&body[4 + prefix_size..], expected_body);
                respond(
                    &mut socket,
                    &serde_json::json!({"schema": UPLOAD_RESULT_SCHEMA, "files": expected_files}),
                );
            }
            for (batch_index, batch) in manifest.files.chunks(FILE_BATCH_SIZE).enumerate() {
                let expected_files: Vec<_> = batch
                    .iter()
                    .map(|file| {
                        serde_json::json!({
                            "sha256": file.sha256, "sizeBytes": file.size_bytes,
                        })
                    })
                    .collect();
                let (mut socket, _) = listener.accept().unwrap();
                let (path, body) = read_request(&mut socket);
                assert!(path.starts_with("POST /internal/agent-runs/workspace-files/download "));
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(
                    request["snapshot"]["snapshotSha256"],
                    expected_snapshot.sha256
                );
                assert_eq!(request["files"], serde_json::json!(expected_files));
                let response_body: Vec<u8> = contents
                    [batch_index * FILE_BATCH_SIZE..batch_index * FILE_BATCH_SIZE + batch.len()]
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response_body.len()
                )
                .unwrap();
                socket.write_all(&response_body).unwrap();
            }
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let lease = SessionWorkspaceLease {
            job_id: "original-job".into(),
            lease_owner: "original-lease".into(),
        };
        let api_url = format!("http://{address}");
        let access = FileAccess::new(
            &client,
            &api_url,
            "token",
            "original-run",
            "original-digest",
            &lease,
            None,
        );
        access.upload_missing(&mut artifact).unwrap();
        let mut restored = restore_from_api(
            &mut artifact.file,
            &artifact.descriptor,
            bodies.len() as u64,
            WorkspaceProtection::new(),
            &access,
        )
        .unwrap();
        let mut restored_frame = Vec::new();
        restored
            .helper_frame
            .file
            .read_to_end(&mut restored_frame)
            .unwrap();
        assert_eq!(restored_frame, expected_frame);
        worker.join().unwrap();
    }

    #[test]
    fn download_batch_rejects_truncation_per_file_corruption_and_trailing_bytes() {
        for (body, valid) in [
            (b"oldnewold".as_slice(), true),
            (b"oldne".as_slice(), false),
            (b"oldBADold".as_slice(), false),
            (b"oldnewold-extra".as_slice(), false),
        ] {
            let mut artifact = captured();
            prepare(&mut artifact).unwrap();
            let result = restore_with_writer(
                &mut artifact.file,
                &artifact.descriptor,
                9,
                WorkspaceProtection::new(),
                |manifest, output| {
                    write_download_batch(&mut Cursor::new(body), &manifest.files, output)
                },
            );
            assert_eq!(result.is_ok(), valid);
        }
    }
}
