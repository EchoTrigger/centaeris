//! Product workspace orchestration exercised through the real E2B host guard.
use super::*;
use crate::e2b_execution_host::{
    E2bCommandOutput, E2bDispatchAuthority, E2bDispatchResult, E2bExecutionHostRunner,
    E2bHostTransport, E2bSandboxBinding, E2bSandboxObservation,
};
use crate::observations::capture_protection;
use centaeris_core::execution::ExecutionCommandRequest;
use hosted_execution::host_supervisor::TrustedHelperKind;
use std::collections::BTreeMap;

struct Authority;
impl E2bDispatchAuthority for Authority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        operation()
    }
}
struct Guest {
    data: Mutex<Vec<u8>>,
    requests: Mutex<Vec<SandboxSnapshotCaptureRequest>>,
    restore_error: Mutex<Option<E2bHostError>>,
    forged_receipt: AtomicBool,
}
impl Guest {
    fn new(data: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(data.to_vec()),
            requests: Mutex::new(Vec::new()),
            restore_error: Mutex::new(None),
            forged_receipt: AtomicBool::new(false),
        })
    }
}
impl E2bHostTransport for Guest {
    fn inspect(&self, binding: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        Ok(E2bSandboxObservation {
            binding: binding.clone(),
            running: true,
            on_timeout_kill: true,
            auto_resume: false,
        })
    }
    fn helper(
        &self,
        _: &E2bSandboxBinding,
        kind: TrustedHelperKind,
        input: &[u8],
        _: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        match kind {
            TrustedHelperKind::AgentProcessState => Ok(br#"{"activeProcessCount":0}"#.to_vec()),
            TrustedHelperKind::QuiesceAgentProcesses | TrustedHelperKind::PrepareWorkspace => {
                Ok(Vec::new())
            }
            TrustedHelperKind::SnapshotRestore => {
                if let Some(error) = *self.restore_error.lock().unwrap() {
                    return Err(error);
                }
                let length = u32::from_be_bytes(input[..4].try_into().unwrap()) as usize;
                let manifest: SandboxWorkspaceSnapshotManifest =
                    serde_json::from_slice(&input[4..4 + length]).unwrap();
                assert_eq!(manifest.files.len(), 1);
                let bytes = &input[4 + length..];
                assert_eq!(
                    manifest.files[0].sha256,
                    format!("sha256:{:x}", Sha256::digest(bytes))
                );
                *self.data.lock().unwrap() = bytes.to_vec();
                Ok(Vec::new())
            }
            TrustedHelperKind::SnapshotCapture => {
                let request: SandboxSnapshotCaptureRequest = serde_json::from_slice(input).unwrap();
                let data = self.data.lock().unwrap();
                let (wire, manifest) = frame(&data);
                let unchanged = request
                    .baseline
                    .as_ref()
                    .filter(|value| value.manifest == manifest);
                let response = SandboxSnapshotCaptureResponse {
                    schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
                    capture_id: if self.forged_receipt.load(Ordering::SeqCst) {
                        "foreign-capture".into()
                    } else {
                        request.capture_id.clone()
                    },
                    disposition: if unchanged.is_some() {
                        SnapshotCaptureDisposition::Unchanged
                    } else {
                        SnapshotCaptureDisposition::Snapshot
                    },
                    baseline_snapshot_sha256: unchanged.map(|value| value.snapshot_sha256.clone()),
                    manifest_sha256: format!(
                        "sha256:{:x}",
                        Sha256::digest(serde_json::to_vec(&manifest).unwrap())
                    ),
                };
                self.requests.lock().unwrap().push(request);
                let encoded = serde_json::to_vec(&response).unwrap();
                let mut output = (encoded.len() as u32).to_be_bytes().to_vec();
                output.extend(encoded);
                if response.disposition == SnapshotCaptureDisposition::Snapshot {
                    output.extend(wire);
                }
                Ok(output)
            }
            _ => panic!("unexpected fixed helper"),
        }
    }
    fn start(
        &self,
        _: &E2bSandboxBinding,
        _: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        panic!("no workload command")
    }
    fn wait(
        &self,
        _: &E2bSandboxBinding,
        _: i32,
        _: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        panic!("no workload wait")
    }
    fn quiesce(&self, _: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        Ok(())
    }
}
const LIMIT: u64 = 64 * 1024 * 1024;
fn runner(guest: Arc<Guest>) -> Arc<E2bExecutionHostRunner> {
    let profile = profile(LIMIT);
    let value = serde_json::to_value(&profile).unwrap();
    let mut metadata: BTreeMap<String, String> = [
        "centaerisAttemptId",
        "environmentId",
        "instanceId",
        "executionId",
        "agentRunId",
        "sessionId",
        "workspaceId",
        "authorizationDigest",
        "processingJobId",
    ]
    .into_iter()
    .map(|key| (key.into(), format!("{key}-{}", uuid::Uuid::new_v4())))
    .collect();
    metadata.insert("profileDigest".into(), profile.digest().unwrap());
    Arc::new(
        E2bExecutionHostRunner::new(
            profile,
            E2bSandboxBinding {
                sandbox_id: format!("sandbox-{}", uuid::Uuid::new_v4()),
                template_id: value["templateRef"]["templateId"].as_str().unwrap().into(),
                metadata,
            },
            guest,
            Arc::new(Authority),
        )
        .unwrap(),
    )
}
fn workspace(
    url: &str,
    host: Arc<E2bExecutionHostRunner>,
    selected: EnvironmentProfile,
) -> E2bSessionWorkspace {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    E2bSessionWorkspace::new(
        selected,
        SessionWorkspaceApi {
            client: &client,
            api_url: url,
            api_token: "fixture-only",
            agent_run_id: "run-a",
            authorization_digest: &digest("run-a"),
        },
        host,
    )
    .unwrap()
}
fn source_server(data: &[u8], corrupt_download: Arc<AtomicBool>) -> (Server, SessionWorkspace) {
    let (wire, _) = frame(data);
    let expected = frozen(&wire, data.len());
    let current = expected.clone();
    let server = Server::new(move |request| {
        if request.path.ends_with("/resolve") {
            Some(resolved(&current, "download"))
        } else if request.path.ends_with("/download") {
            let mut body = wire.clone();
            if corrupt_download.load(Ordering::SeqCst) {
                *body.last_mut().unwrap() ^= 1;
            }
            Some(Reply {
                status: 200,
                body,
                hash: Some(current.snapshot_sha256.clone()),
                length: None,
            })
        } else {
            panic!("unchanged workspace must not publish a new generation")
        }
    });
    (server, expected)
}
fn restore(url: &str, host: Arc<E2bExecutionHostRunner>, frozen: &SessionWorkspace) {
    let service = workspace(url, host, profile(LIMIT));
    assert_eq!(
        service.restore(&lease("run-a"), frozen, 0).unwrap(),
        SessionWorkspaceResolution::Download
    );
}
fn metric<'a>(events: &'a [Value], name: &str) -> &'a Value {
    &events
        .iter()
        .find(|event| event["metric"] == name)
        .unwrap_or_else(|| panic!("missing metric {name}"))["value"]
}
#[test]
fn restored_baseline_survives_service_replacement_and_avoids_full_spool() {
    let data = vec![b'x'; 128 * 1024];
    let guest = Guest::new(&data);
    let host = runner(guest.clone());
    let (server, frozen) = source_server(&data, Arc::new(AtomicBool::new(false)));
    restore(&server.url, host.clone(), &frozen);
    let service = workspace(&server.url, host, profile(LIMIT));
    let (outcome, events) = capture_protection(|| {
        service
            .collect_and_commit(&lease("run-a"), &frozen, 0)
            .unwrap()
    });
    assert_eq!(outcome, SessionWorkspaceCommitOutcome::Unchanged);
    let requests = guest.requests.lock().unwrap();
    let baseline = requests[0]
        .baseline
        .as_ref()
        .expect("verified restore must seed this runner across service replacement");
    assert_eq!(baseline.snapshot_sha256, frozen.snapshot_sha256);
    assert_eq!(metric(&events, "runtimeSpoolWrittenBytes"), &json!(0));
    assert_eq!(metric(&events, "uploadAttempts"), &json!(0));
    assert_eq!(metric(&events, "uploadedBytes"), &json!(0));
    assert_eq!(metric(&events, "snapshotLogicalBytes"), &json!(data.len()));
    assert_eq!(
        metric(&events, "workspaceCommitDisposition"),
        &json!("unchanged")
    );
    assert!(
        metric(&events, "rawCaptureSpoolWrittenBytes")
            .as_u64()
            .unwrap()
            < 1024
    );
    assert!(events
        .iter()
        .all(|event| event["operationId"] == events[0]["operationId"]));
    let seen = server.finish();
    assert_eq!(seen.len(), 3);
    assert!(seen
        .iter()
        .all(|request| !request.path.ends_with("/commit")));
    assert_eq!(frozen.generation, 1);
}
#[test]
fn reverted_content_matches_restored_baseline_but_changed_content_keeps_full_frame() {
    for revert in [true, false] {
        let data = b"original-content";
        let guest = Guest::new(data);
        let host = runner(guest.clone());
        let (wire, _) = frame(data);
        let frozen = frozen(&wire, data.len());
        let current = frozen.clone();
        let (changed, _) = frame(b"changed-content");
        let mut next = super::frozen(&changed, 15);
        next.generation = 2;
        let server = Server::new(move |request| {
            if request.path.ends_with("/resolve") {
                Some(resolved(&current, "download"))
            } else if request.path.ends_with("/download") {
                Some(Reply {
                    status: 200,
                    body: wire.clone(),
                    hash: Some(current.snapshot_sha256.clone()),
                    length: None,
                })
            } else {
                assert!(!revert);
                assert!(request.path.ends_with("/commit"));
                Some(Reply::json(
                    json!({"schema":"runtime.session_workspace.commit.result.v1","disposition":"committed","sessionWorkspace":next}),
                ))
            }
        });
        restore(&server.url, host.clone(), &frozen);
        *guest.data.lock().unwrap() = b"changed-content".to_vec();
        if revert {
            *guest.data.lock().unwrap() = data.to_vec();
        }
        let (outcome, events) = capture_protection(|| {
            workspace(&server.url, host, profile(LIMIT))
                .collect_and_commit(&lease("run-a"), &frozen, 0)
                .unwrap()
        });
        assert_eq!(
            outcome,
            if revert {
                SessionWorkspaceCommitOutcome::Unchanged
            } else {
                SessionWorkspaceCommitOutcome::Accepted
            }
        );
        assert!(guest.requests.lock().unwrap()[0].baseline.is_some());
        assert_eq!(
            metric(&events, "runtimeSpoolWrittenBytes")
                .as_u64()
                .unwrap()
                == 0,
            revert
        );
        assert_eq!(
            server
                .finish()
                .iter()
                .filter(|request| request.path.ends_with("/commit"))
                .count(),
            usize::from(!revert)
        );
    }
}
#[test]
fn failed_restore_invalidates_old_seed_before_validation_or_apply() {
    for corrupt in [true, false] {
        let guest = Guest::new(b"original");
        let host = runner(guest.clone());
        let corrupt_download = Arc::new(AtomicBool::new(false));
        let (server, frozen) = source_server(b"original", corrupt_download.clone());
        restore(&server.url, host.clone(), &frozen);
        corrupt_download.store(corrupt, Ordering::SeqCst);
        if !corrupt {
            *guest.restore_error.lock().unwrap() =
                Some(E2bHostError::NotDispatched("fixture_restore_denied"));
        }
        assert!(matches!(
            workspace(&server.url, host.clone(), profile(LIMIT)).restore(
                &lease("run-a"),
                &frozen,
                0
            ),
            Err(SessionWorkspaceApiError::Rejected(_))
        ));
        *guest.restore_error.lock().unwrap() = None;
        corrupt_download.store(false, Ordering::SeqCst);
        let (outcome, events) = capture_protection(|| {
            workspace(&server.url, host, profile(LIMIT))
                .collect_and_commit(&lease("run-a"), &frozen, 0)
                .unwrap()
        });
        assert_eq!(outcome, SessionWorkspaceCommitOutcome::Unchanged);
        assert!(
            guest.requests.lock().unwrap()[0].baseline.is_none(),
            "failed restore must discard previous seed"
        );
        assert!(
            metric(&events, "runtimeSpoolWrittenBytes")
                .as_u64()
                .unwrap()
                > 0
        );
        server.finish();
    }
}
#[test]
fn unknown_restore_remains_ineligible_after_service_replacement() {
    let guest = Guest::new(b"original");
    let host = runner(guest.clone());
    let (server, frozen) = source_server(b"original", Arc::new(AtomicBool::new(false)));
    restore(&server.url, host.clone(), &frozen);
    *guest.restore_error.lock().unwrap() = Some(E2bHostError::Unknown("fixture_restore_unknown"));
    assert!(matches!(
        workspace(&server.url, host.clone(), profile(LIMIT)).restore(&lease("run-a"), &frozen, 0),
        Err(SessionWorkspaceApiError::Unavailable(_))
    ));
    *guest.restore_error.lock().unwrap() = None;
    assert_eq!(
        workspace(&server.url, host, profile(LIMIT))
            .collect_and_commit(&lease("run-a"), &frozen, 0)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Pending
    );
    assert!(guest.requests.lock().unwrap().is_empty());
    server.finish();
}
#[test]
fn new_instance_does_not_inherit_matching_descriptor_baseline() {
    let guest = Guest::new(b"original");
    let host = runner(guest.clone());
    let (server, frozen) = source_server(b"original", Arc::new(AtomicBool::new(false)));
    restore(&server.url, host, &frozen);
    let fresh_guest = Guest::new(b"original");
    let fresh_host = runner(fresh_guest.clone());
    let (outcome, events) = capture_protection(|| {
        workspace(&server.url, fresh_host, profile(LIMIT))
            .collect_and_commit(&lease("run-a"), &frozen, 0)
            .unwrap()
    });
    assert_eq!(outcome, SessionWorkspaceCommitOutcome::Unchanged);
    assert!(fresh_guest.requests.lock().unwrap()[0].baseline.is_none());
    assert!(
        metric(&events, "runtimeSpoolWrittenBytes")
            .as_u64()
            .unwrap()
            > 0
    );
    server.finish();
}
#[test]
fn changed_profile_cannot_use_another_profile_restored_seed() {
    let guest = Guest::new(b"original");
    let host = runner(guest.clone());
    let (server, frozen) = source_server(b"original", Arc::new(AtomicBool::new(false)));
    restore(&server.url, host.clone(), &frozen);
    let selected = profile(LIMIT - 1);
    let outcome = workspace(&server.url, host, selected)
        .collect_and_commit(&lease("run-a"), &frozen, 0)
        .unwrap();
    assert!(matches!(
        outcome,
        SessionWorkspaceCommitOutcome::Rejected(_)
    ));
    assert!(guest.requests.lock().unwrap().is_empty());
    server.finish();
}
#[test]
fn forged_capture_receipt_rejects_commit_and_discards_seed() {
    let guest = Guest::new(b"original");
    let host = runner(guest.clone());
    let (server, frozen) = source_server(b"original", Arc::new(AtomicBool::new(false)));
    restore(&server.url, host.clone(), &frozen);
    guest.forged_receipt.store(true, Ordering::SeqCst);
    assert!(matches!(
        workspace(&server.url, host.clone(), profile(LIMIT))
            .collect_and_commit(&lease("run-a"), &frozen, 0)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Rejected(_)
    ));
    guest.forged_receipt.store(false, Ordering::SeqCst);
    let (outcome, events) = capture_protection(|| {
        workspace(&server.url, host, profile(LIMIT))
            .collect_and_commit(&lease("run-a"), &frozen, 0)
            .unwrap()
    });
    assert_eq!(outcome, SessionWorkspaceCommitOutcome::Unchanged);
    let seen = guest.requests.lock().unwrap();
    assert!(seen[0].baseline.is_some());
    assert!(seen[1].baseline.is_none());
    assert!(
        metric(&events, "runtimeSpoolWrittenBytes")
            .as_u64()
            .unwrap()
            > 0
    );
    server.finish();
}
#[test]
fn verified_unchanged_receipt_still_obeys_expanded_transfer_admission() {
    let guest = Guest::new(b"original");
    let host = runner(guest.clone());
    let (server, frozen) = source_server(b"original", Arc::new(AtomicBool::new(false)));
    restore(&server.url, host.clone(), &frozen);
    let outcome = workspace(&server.url, host, profile(LIMIT))
        .collect_and_commit(&lease("run-a"), &frozen, LIMIT - OVERHEAD)
        .unwrap();
    assert!(matches!(
        outcome,
        SessionWorkspaceCommitOutcome::Rejected(_)
    ));
    assert!(guest.requests.lock().unwrap()[0].baseline.is_some());
    server.finish();
}
