//! Publish a captured original workspace under the current cleanup authority.
use super::*;
use crate::contract::AgentRunCleanup;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use centaeris_core::session::RuntimeJobLeaseFence;

const PRESERVATION_SCHEMA: &str = "runtime.session_workspace.preserve.v1";
const PRESERVATION_RESULT_SCHEMA: &str = "runtime.session_workspace.preserve.result.v1";
const PRESERVATION_RESPONSE_LIMIT: usize = 16 * 1024;
const PRESERVATION_METADATA_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreservationProof {
    pub(super) preservation_receipt_id: String,
    pub(super) session_workspace: SessionWorkspace,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreservationRequest<'a> {
    #[serde(flatten)]
    commit: SessionWorkspaceCommitRequest<'a>,
    environment_authorization: &'a crate::environment_authorization::EnvironmentAuthorization,
    allocation_execution_id: &'a str,
    instance_id: &'a str,
    container_id: &'a str,
    profile_digest: &'a str,
    baseline: &'a SessionWorkspace,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreservationResponse {
    schema: String,
    disposition: String,
    session_workspace: SessionWorkspace,
    preservation_receipt_id: String,
}

/// Upload only an already captured immutable artifact. The caller releases DB
/// pins before HTTP and retains the same artifact for an exact lost-ACK retry.
pub(super) fn upload_stopped_workspace(
    api_url: &str,
    api_token: &str,
    cleanup: &AgentRunCleanup,
    lease: &RuntimeJobLeaseFence,
    allocation: &RetainedInstanceAllocation,
    artifact: &mut ImmutableWorkspaceSnapshot,
) -> Result<PreservationProof, String> {
    let mut upload = || upload_blocking(api_url, api_token, cleanup, lease, allocation, artifact);
    // Reqwest's blocking runtime must be created and dropped off the async thread.
    if tokio::runtime::Handle::try_current().is_ok() {
        thread::scope(|scope| match scope.spawn(upload).join() {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        })
    } else {
        upload()
    }
}

fn lease_digest(owner: &str) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(
            [
                b"workspace:environment-controller-lease:v1\0".as_slice(),
                owner.as_bytes()
            ]
            .concat()
        )
    )
}
fn validate_binding(
    cleanup: &AgentRunCleanup,
    lease: &RuntimeJobLeaseFence,
    allocation: &RetainedInstanceAllocation,
) -> Result<(), String> {
    let rejected = || "stopped_preservation_binding_rejected".to_string();
    cleanup
        .authorization
        .checked_binding(&cleanup.agent_run_id)
        .and_then(|grant| grant.verify_digest_binding(&cleanup.authorization_digest))
        .map_err(|_| rejected())?;
    let environment = &cleanup.environment_authorization.payload;
    environment.validate().map_err(|_| rejected())?;
    let controller = environment.controller.as_ref().ok_or_else(rejected)?;
    if cleanup.schema != "workspace.agent_run.cleanup.v1"
        || environment.agent_run_id != cleanup.agent_run_id
        || environment.agent_run_authorization_digest != cleanup.authorization_digest
        || environment.session_id != cleanup.authorization.session_id
        || environment.environment_id != allocation.environment_id
        || controller.controller_purpose != "cleanup"
        || controller.controller_lease_digest != lease_digest(&lease.lease_owner)
        || lease.lease_owner.is_empty()
        || lease.job_kind != "agent_run.lifecycle"
        || lease.job_id
            != crate::agent_run_lifecycle_job_id(&cleanup.agent_run_id).map_err(|_| rejected())?
        || allocation.allocation_execution_id.is_empty()
        || allocation
            .allocation_execution_id
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
        || !allocation
            .instance_id
            .strip_prefix("instance_")
            .is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        || !allocation.container_id.as_deref().is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || !is_workspace_sha256(&allocation.profile_digest)
    {
        return Err(rejected());
    }
    Ok(())
}

fn upload_blocking(
    api_url: &str,
    api_token: &str,
    cleanup: &AgentRunCleanup,
    lease: &RuntimeJobLeaseFence,
    allocation: &RetainedInstanceAllocation,
    artifact: &mut ImmutableWorkspaceSnapshot,
) -> Result<PreservationProof, String> {
    validate_binding(cleanup, lease, allocation)?;
    if api_token.is_empty() {
        return Err("stopped_preservation_api_unavailable".into());
    }
    let descriptor = &artifact.descriptor;
    let candidate = SessionWorkspace {
        generation: cleanup
            .authorization
            .session_workspace
            .generation
            .checked_add(1)
            .ok_or("stopped_preservation_binding_rejected")?,
        snapshot_sha256: descriptor.sha256.clone(),
        snapshot_size_bytes: descriptor.size_bytes,
        expanded_size_bytes: descriptor.expanded_size_bytes,
        file_count: descriptor.file_count,
    };
    candidate
        .validate()
        .map_err(|_| "stopped_preservation_artifact_rejected")?;
    if artifact
        .file
        .metadata()
        .map_err(|_| "stopped_preservation_spool_unavailable")?
        .len()
        != descriptor.size_bytes
    {
        return Err("stopped_preservation_artifact_rejected".into());
    }
    let request = PreservationRequest {
        commit: SessionWorkspaceCommitRequest {
            schema: PRESERVATION_SCHEMA,
            job_id: &lease.job_id,
            lease_owner: &lease.lease_owner,
            agent_run_id: &cleanup.agent_run_id,
            authorization_digest: &cleanup.authorization_digest,
            snapshot_sha256: &descriptor.sha256,
            snapshot_size_bytes: descriptor.size_bytes,
            expanded_size_bytes: descriptor.expanded_size_bytes,
            file_count: descriptor.file_count,
        },
        environment_authorization: &cleanup.environment_authorization,
        allocation_execution_id: &allocation.allocation_execution_id,
        instance_id: &allocation.instance_id,
        container_id: allocation
            .container_id
            .as_deref()
            .ok_or("stopped_preservation_binding_rejected")?,
        profile_digest: &allocation.profile_digest,
        baseline: &cleanup.authorization.session_workspace,
    };
    let metadata =
        serde_json::to_vec(&request).map_err(|_| "stopped_preservation_metadata_rejected")?;
    if metadata.len() > PRESERVATION_METADATA_LIMIT {
        return Err("stopped_preservation_metadata_rejected".into());
    }
    let mut prefix = (metadata.len() as u32).to_be_bytes().to_vec();
    prefix.extend_from_slice(&metadata);
    let length = (prefix.len() as u64)
        .checked_add(descriptor.size_bytes)
        .ok_or("stopped_preservation_metadata_rejected")?;
    artifact
        .file
        .rewind()
        .map_err(|_| "stopped_preservation_spool_unavailable")?;
    let snapshot = artifact
        .file
        .try_clone()
        .map_err(|_| "stopped_preservation_spool_unavailable")?;
    let body = SessionWorkspaceUploadBody {
        prefix: Cursor::new(prefix),
        snapshot,
    };
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| "stopped_preservation_api_unavailable")?;
    let observation = &artifact.observation;
    let response = observation
        .timed(ProtectionMetric::UploadElapsedMs, || {
            observation.count(ProtectionMetric::UploadAttempts, 1, true);
            client
                .post(format!(
                    "{}/internal/agent-runs/session-workspace/preserve",
                    api_url.trim_end_matches('/')
                ))
                .header("X-Internal-Token", api_token)
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", length.to_string())
                .body(reqwest::blocking::Body::sized(
                    observation.upload_reader(body, length),
                    length,
                ))
                .timeout(Duration::from_secs(if descriptor.size_bytes == 0 {
                    30
                } else {
                    300
                }))
                .send()
        })
        .map_err(|_| "stopped_preservation_acknowledgement_unknown")?;
    let response = read_preservation_response(response)?;
    if response.schema != PRESERVATION_RESULT_SCHEMA
        || !matches!(response.disposition.as_str(), "committed" | "idempotent")
        || response.session_workspace != candidate
        || !is_workspace_sha256(&response.preservation_receipt_id)
    {
        return Err("stopped_preservation_acknowledgement_rejected".into());
    }
    Ok(PreservationProof {
        preservation_receipt_id: response.preservation_receipt_id,
        session_workspace: response.session_workspace,
    })
}
fn read_preservation_response(
    response: reqwest::blocking::Response,
) -> Result<PreservationResponse, String> {
    if !response.status().is_success() {
        return Err("stopped_preservation_acknowledgement_rejected".into());
    }
    if response
        .content_length()
        .is_some_and(|length| length > PRESERVATION_RESPONSE_LIMIT as u64)
    {
        return Err("stopped_preservation_acknowledgement_rejected".into());
    }
    let mut bytes = Vec::new();
    response
        .take(PRESERVATION_RESPONSE_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "stopped_preservation_acknowledgement_unknown")?;
    if bytes.len() > PRESERVATION_RESPONSE_LIMIT {
        return Err("stopped_preservation_acknowledgement_rejected".into());
    }
    let response: PreservationResponse = serde_json::from_slice(&bytes)
        .map_err(|_| "stopped_preservation_acknowledgement_rejected")?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::EnvironmentTaskOwner;
    use std::net::TcpListener;

    fn fixture() -> (
        AgentRunCleanup,
        RuntimeJobLeaseFence,
        RetainedInstanceAllocation,
    ) {
        let authorization: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
            serde_json::from_str(include_str!(
                "../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
            ))
            .unwrap();
        let lease = RuntimeJobLeaseFence {
            job_id: crate::agent_run_lifecycle_job_id(&authorization.agent_run_id).unwrap(),
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: "synthetic-owner".into(),
        };
        let mut environment = crate::environment_authorization::EnvironmentAuthorization::for_test(
            &authorization,
            b"synthetic-only-signing-key",
        );
        let controller = environment.payload.controller.as_mut().unwrap();
        controller.controller_purpose = "cleanup".into();
        controller.controller_lease_digest = lease_digest(&lease.lease_owner);
        use hmac::Mac;
        let mut mac = hmac::Hmac::<Sha256>::new_from_slice(b"synthetic-only-signing-key").unwrap();
        mac.update(b"workspace:environment-authorization:v1\0");
        mac.update(environment.payload.digest().unwrap().as_bytes());
        environment.signature = format!(
            "hmac-sha256:{}",
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let cleanup = AgentRunCleanup {
            schema: "workspace.agent_run.cleanup.v1".into(),
            agent_run_id: authorization.agent_run_id.clone(),
            authorization_digest: authorization.digest().unwrap(),
            authorization_signature: authorization
                .signature(b"synthetic-only-signing-key")
                .unwrap(),
            authorization: authorization.clone(),
            environment_authorization: environment,
        };
        assert!(cleanup
            .clone()
            .validate(b"synthetic-only-signing-key")
            .is_ok());
        let allocation = RetainedInstanceAllocation {
            allocation_execution_id: "original-execution".into(),
            allocation_agent_run_id: Some("original-run".into()),
            allocation_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "original-run".into(),
            },
            instance_id: format!("instance_{}", "d".repeat(64)),
            environment_id: cleanup
                .environment_authorization
                .payload
                .environment_id
                .clone(),
            host_id: "original-host".into(),
            image_digest: authorization.environment_profile.image_digest.clone(),
            authorization_digest: format!("sha256:{}", "a".repeat(64)),
            binding_digest: format!("sha256:{}", "b".repeat(64)),
            profile_digest: authorization.environment_profile_digest.clone(),
            container_id: Some("c".repeat(64)),
            reused: true,
        };
        (cleanup, lease, allocation)
    }
    fn artifact(payload: &[u8]) -> ImmutableWorkspaceSnapshot {
        if payload.is_empty() {
            return capture_snapshot_artifact(&mut std::io::empty(), 0).unwrap();
        }
        let manifest = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "recovered.bin".into(),
                size_bytes: payload.len() as u64,
                sha256: format!("sha256:{:x}", Sha256::digest(payload)),
                executable: false,
            }],
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&bytes);
        frame.extend_from_slice(payload);
        capture_snapshot_artifact(&mut Cursor::new(frame.clone()), frame.len() as u64).unwrap()
    }
    fn ack(cleanup: &AgentRunCleanup, artifact: &ImmutableWorkspaceSnapshot) -> serde_json::Value {
        serde_json::json!({"schema":"runtime.session_workspace.preserve.result.v1","disposition":"committed","preservationReceiptId":format!("sha256:{}","f".repeat(64)),"sessionWorkspace":{"generation":cleanup.authorization.session_workspace.generation+1,"snapshotSha256":artifact.descriptor.sha256,"snapshotSizeBytes":artifact.descriptor.size_bytes,"expandedSizeBytes":artifact.descriptor.expanded_size_bytes,"fileCount":artifact.descriptor.file_count}})
    }
    fn serve(
        reply: Option<(u16, String)>,
    ) -> (
        String,
        thread::JoinHandle<(String, serde_json::Value, Vec<u8>)>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("preservation fixture did not receive request: {error}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 65536);
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            let header = String::from_utf8(header).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|value| value.parse::<usize>().unwrap())
                })
                .unwrap();
            assert!(length < 65536);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            let length = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
            let metadata = serde_json::from_slice(&body[4..4 + length]).unwrap();
            let snapshot = body[4 + length..].to_vec();
            if let Some((status, reply)) = reply {
                write!(socket,"HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).unwrap();
            }
            (header, metadata, snapshot)
        });
        (url, server)
    }
    #[test]
    fn posts_exact_original_target_cleanup_authority_baseline_and_snapshot_frame() {
        let (cleanup, lease, allocation) = fixture();
        let mut artifact = artifact(b"original\0\xff");
        let expected = ack(&cleanup, &artifact);
        let (url, server) = serve(Some((200, expected.to_string())));
        let proof = upload_stopped_workspace(
            &url,
            "synthetic-internal-token",
            &cleanup,
            &lease,
            &allocation,
            &mut artifact,
        )
        .unwrap();
        let (header, metadata, snapshot) = server.join().unwrap();
        assert!(header.starts_with("POST /internal/agent-runs/session-workspace/preserve HTTP/1.1"));
        assert!(header
            .to_ascii_lowercase()
            .contains("x-internal-token: synthetic-internal-token"));
        let expected = serde_json::json!({"schema":"runtime.session_workspace.preserve.v1","jobId":lease.job_id,"leaseOwner":lease.lease_owner,"agentRunId":cleanup.agent_run_id,"authorizationDigest":cleanup.authorization_digest,"snapshotSha256":artifact.descriptor.sha256,"snapshotSizeBytes":artifact.descriptor.size_bytes,"expandedSizeBytes":artifact.descriptor.expanded_size_bytes,"fileCount":artifact.descriptor.file_count,"environmentAuthorization":cleanup.environment_authorization,"allocationExecutionId":allocation.allocation_execution_id,"instanceId":allocation.instance_id,"containerId":allocation.container_id,"profileDigest":allocation.profile_digest,"baseline":cleanup.authorization.session_workspace});
        assert_eq!(metadata, expected);
        assert!(metadata.get("generation").is_none());
        assert_eq!(snapshot.len() as u64, artifact.descriptor.size_bytes);
        assert_eq!(
            inspect_workspace_snapshot(&mut Cursor::new(snapshot)).unwrap(),
            artifact.descriptor
        );
        assert_eq!(
            proof.session_workspace.generation,
            cleanup.authorization.session_workspace.generation + 1
        );
        assert_eq!(
            proof.preservation_receipt_id,
            format!("sha256:{}", "f".repeat(64))
        );
    }
    #[test]
    fn empty_workspace_sends_metadata_without_substituting_an_unconfirmed_baseline() {
        let (cleanup, lease, allocation) = fixture();
        let mut artifact = artifact(b"");
        let (url, server) = serve(Some((200, ack(&cleanup, &artifact).to_string())));
        let proof = upload_stopped_workspace(
            &url,
            "synthetic-token",
            &cleanup,
            &lease,
            &allocation,
            &mut artifact,
        )
        .unwrap();
        let (_, metadata, snapshot) = server.join().unwrap();
        assert!(snapshot.is_empty());
        assert_eq!(
            metadata["baseline"],
            serde_json::to_value(&cleanup.authorization.session_workspace).unwrap()
        );
        assert_eq!(metadata["snapshotSha256"], "");
        assert_eq!(proof.session_workspace.file_count, 0);
    }
    #[test]
    fn lost_ack_keeps_the_exact_artifact_replayable_with_identical_metadata() {
        let (cleanup, lease, allocation) = fixture();
        let mut artifact = artifact(b"preserved");
        let descriptor = artifact.descriptor.clone();
        let (url, server) = serve(None);
        assert!(upload_stopped_workspace(
            &url,
            "synthetic-token",
            &cleanup,
            &lease,
            &allocation,
            &mut artifact
        )
        .is_err());
        let (_, first, first_bytes) = server.join().unwrap();
        let mut reply = ack(&cleanup, &artifact);
        reply["disposition"] = "idempotent".into();
        let (url, server) = serve(Some((200, reply.to_string())));
        assert!(upload_stopped_workspace(
            &url,
            "synthetic-token",
            &cleanup,
            &lease,
            &allocation,
            &mut artifact
        )
        .is_ok());
        let (_, second, second_bytes) = server.join().unwrap();
        assert_eq!(first, second);
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(artifact.descriptor, descriptor);
        artifact.file.rewind().unwrap();
        assert_eq!(
            inspect_workspace_snapshot(&mut artifact.file).unwrap(),
            descriptor
        );
    }
    #[test]
    fn malformed_non_success_or_oversized_acknowledgements_never_prove_preservation() {
        let (cleanup, lease, allocation) = fixture();
        for mutation in [
            "schema",
            "disposition",
            "generation",
            "hash",
            "size",
            "receipt",
            "unknown",
            "bad-json",
            "non-success",
            "too-large",
        ] {
            let mut artifact = artifact(b"source");
            let mut reply = ack(&cleanup, &artifact);
            let mut status = 200;
            match mutation {
                "schema" => reply["schema"] = "wrong".into(),
                "disposition" => reply["disposition"] = "retained".into(),
                "generation" => {
                    reply["sessionWorkspace"]["generation"] =
                        cleanup.authorization.session_workspace.generation.into()
                }
                "hash" => {
                    reply["sessionWorkspace"]["snapshotSha256"] =
                        format!("sha256:{}", "0".repeat(64)).into()
                }
                "size" => reply["sessionWorkspace"]["snapshotSizeBytes"] = 1.into(),
                "receipt" => reply["preservationReceiptId"] = "bad".into(),
                "unknown" => reply["extra"] = true.into(),
                "non-success" => status = 503,
                _ => {}
            }
            let text = match mutation {
                "bad-json" => "{".into(),
                "too-large" => "x".repeat(PRESERVATION_RESPONSE_LIMIT + 1),
                _ => reply.to_string(),
            };
            let (url, server) = serve(Some((status, text)));
            assert!(
                upload_stopped_workspace(
                    &url,
                    "synthetic-token",
                    &cleanup,
                    &lease,
                    &allocation,
                    &mut artifact
                )
                .is_err(),
                "ack {mutation}"
            );
            server.join().unwrap();
        }
    }
    #[test]
    fn mismatched_lease_controller_and_original_environment_are_rejected_before_http() {
        let (cleanup, lease, allocation) = fixture();
        for kind in ["job", "owner", "purpose", "environment", "authorization"] {
            let mut cleanup = cleanup.clone();
            let mut lease = lease.clone();
            let mut allocation = allocation.clone();
            let mut artifact = artifact(b"source");
            match kind {
                "job" => lease.job_id = "different-job".into(),
                "owner" => lease.lease_owner = "different-owner".into(),
                "purpose" => {
                    cleanup
                        .environment_authorization
                        .payload
                        .controller
                        .as_mut()
                        .unwrap()
                        .controller_purpose = "execute".into()
                }
                "environment" => allocation.environment_id = "foreign".into(),
                "authorization" => {
                    cleanup.authorization_digest = format!("sha256:{}", "0".repeat(64))
                }
                _ => unreachable!(),
            }
            assert!(
                matches!(upload_stopped_workspace("http://127.0.0.1:1","synthetic-token",&cleanup,&lease,&allocation,&mut artifact),Err(error) if error=="stopped_preservation_binding_rejected"),
                "identity {kind}"
            );
        }
    }
    #[test]
    fn preservation_uploader_is_safe_inside_an_async_runtime() {
        let (cleanup, lease, allocation) = fixture();
        let mut artifact = artifact(b"source");
        let (url, server) = serve(Some((200, ack(&cleanup, &artifact).to_string())));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(async {
                upload_stopped_workspace(
                    &url,
                    "synthetic-token",
                    &cleanup,
                    &lease,
                    &allocation,
                    &mut artifact,
                )
            })
            .unwrap();
        server.join().unwrap();
    }
}
