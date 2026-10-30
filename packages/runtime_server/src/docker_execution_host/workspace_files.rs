//! Session-scoped content objects beneath an immutable workspace manifest.
use super::*;

pub(super) const ENCODING: &str = "fileManifestV1";
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
        let ranges: HashMap<_, _> = source
            .ranges
            .iter()
            .map(|range| (range.sha256.as_str(), range))
            .collect();
        let mut sent = HashSet::new();
        for hash in missing.missing {
            let range = ranges
                .get(hash.as_str())
                .ok_or("workspace missing file identity mismatch")?;
            if !sent.insert(hash) {
                return Err("workspace missing files repeated an identity".into());
            }
            let metadata = serde_json::to_vec(&UploadRequest {
                identity: self.identity(UPLOAD_SCHEMA),
                sha256: &range.sha256,
                size_bytes: range.size_bytes,
            })
            .map_err(|error| format!("workspace file metadata invalid: {error}"))?;
            let mut prefix = (metadata.len() as u32).to_be_bytes().to_vec();
            prefix.extend(metadata);
            let length = prefix.len() as u64 + range.size_bytes;
            let mut file = source.frame.try_clone().map_err(spool_error)?;
            file.seek(std::io::SeekFrom::Start(range.offset))
                .map_err(spool_error)?;
            let body = Cursor::new(prefix).chain(file.take(range.size_bytes));
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
                || uploaded.sha256 != range.sha256
                || uploaded.size_bytes != range.size_bytes
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
        file: &SandboxWorkspaceSnapshotFile,
    ) -> Result<reqwest::blocking::Response, String> {
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
                sha256: &file.sha256,
                size_bytes: file.size_bytes,
            })
            .timeout(Duration::from_secs(300))
            .send()
            .map_err(|error| format!("workspace file download failed: {error}"))?;
        if !response.status().is_success()
            || response.content_length() != Some(file.size_bytes)
            || response
                .headers()
                .get("x-content-sha256")
                .and_then(|value| value.to_str().ok())
                != Some(file.sha256.as_str())
        {
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
    sha256: &'a str,
    size_bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UploadResponse {
    schema: String,
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
    sha256: &'a str,
    size_bytes: u64,
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
    restore(source, stored, expanded_limit, observation, |file| {
        access.download(stored, file)
    })
}

fn restore<R: Read>(
    source: &mut impl Read,
    stored: &WorkspaceSnapshotDescriptor,
    expanded_limit: u64,
    observation: WorkspaceProtection,
    mut open: impl FnMut(&SandboxWorkspaceSnapshotFile) -> Result<R, String>,
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
    for file in &manifest.files {
        let mut input = open(file)?;
        observation.download(&mut input, |input| {
            copy_exact(input, &mut output, file.size_bytes, &file.sha256)
        })?;
    }
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
            assert_eq!(metadata["sha256"], missing_hash);
            assert_eq!(metadata["sizeBytes"], 3);
            assert_eq!(&body[4 + length..], b"new");
            respond(
                &mut socket,
                &serde_json::json!({
                    "schema": UPLOAD_RESULT_SCHEMA, "sha256": missing_hash, "sizeBytes": 3,
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
            for file in &manifest.files {
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
                assert_eq!(request["sha256"], file.sha256);
                assert_eq!(request["sizeBytes"], file.size_bytes);
                let content = if file.path == "next.txt" {
                    b"new"
                } else {
                    b"old"
                };
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nX-Content-Sha256: {}\r\nConnection: close\r\n\r\n", file.sha256).unwrap();
                socket.write_all(content).unwrap();
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
}
