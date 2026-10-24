use super::*;
use crate::e2b_execution_host::tests::{binding, profile};
use hosted_execution::protocol::{
    SandboxArtifactRequest, SandboxSnapshotCaptureResponse, SandboxWorkspaceSnapshotFile,
    SandboxWorkspaceSnapshotManifest, SnapshotCaptureDisposition, SANDBOX_SNAPSHOT_CAPTURE_SCHEMA,
    SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA,
};
use serde_json::json;
use std::io::{Read, Seek, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Condvar};

struct Authority {
    denied: AtomicBool,
    calls: AtomicUsize,
}
impl E2bDispatchAuthority for Authority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.denied.load(Ordering::SeqCst) {
            return Err(E2bHostError::NotDispatched("fixture_authority_denied"));
        }
        operation()
    }
}
struct Fixture {
    calls: Mutex<Vec<(TrustedHelperKind, Vec<u8>, usize)>>,
    starts: AtomicUsize,
    quiescences: AtomicUsize,
    count: AtomicUsize,
    helper_unknown: AtomicBool,
    capture_unknown: AtomicBool,
    wait_unknown: AtomicBool,
    block_command: bool,
    block_helper: bool,
    entered: mpsc::Sender<()>,
    released: Mutex<bool>,
    changed: Condvar,
    artifact: Mutex<Vec<u8>>,
    capture: Vec<u8>,
    restore_output: Mutex<Vec<u8>>,
}
impl Fixture {
    fn wait_release(&self) {
        let _ = self.entered.send(());
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
impl E2bHostTransport for Fixture {
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
        limit: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.calls
            .lock()
            .unwrap()
            .push((kind, input.to_vec(), limit));
        if kind == TrustedHelperKind::FileSystem {
            if self.block_helper {
                self.wait_release();
            }
            if self.helper_unknown.load(Ordering::SeqCst) {
                return Err(E2bHostError::Unknown("fixture_mutator_unknown"));
            }
        }
        if kind == TrustedHelperKind::SnapshotCapture && self.capture_unknown.load(Ordering::SeqCst)
        {
            return Err(E2bHostError::Unknown("fixture_capture_unknown"));
        }
        Ok(match kind {
            TrustedHelperKind::AgentProcessState => {
                serde_json::to_vec(&json!({"activeProcessCount":self.count.load(Ordering::SeqCst)}))
                    .unwrap()
            }
            TrustedHelperKind::QuiesceAgentProcesses => {
                self.count.store(0, Ordering::SeqCst);
                Vec::new()
            }
            TrustedHelperKind::SnapshotRestore => self.restore_output.lock().unwrap().clone(),
            TrustedHelperKind::SnapshotCapture => self.capture.clone(),
            TrustedHelperKind::ReadArtifact => self.artifact.lock().unwrap().clone(),
            _ => Vec::new(),
        })
    }
    fn start(
        &self,
        _: &E2bSandboxBinding,
        _: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(71)
    }
    fn wait(
        &self,
        binding: &E2bSandboxBinding,
        pid: i32,
        _: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        if self.block_command {
            self.wait_release();
        }
        if self.wait_unknown.load(Ordering::SeqCst) {
            return Err(E2bHostError::Unknown("fixture_command_wait_unknown"));
        }
        Ok(E2bCommandOutput {
            sandbox_id: binding.sandbox_id.clone(),
            pid,
            exit_code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
    fn quiesce(&self, _: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        self.quiescences.fetch_add(1, Ordering::SeqCst);
        self.count.store(0, Ordering::SeqCst);
        Ok(())
    }
}
fn frame(metadata: &impl serde::Serialize, bytes: &[u8]) -> Vec<u8> {
    let metadata = serde_json::to_vec(metadata).unwrap();
    let mut result = (metadata.len() as u32).to_be_bytes().to_vec();
    result.extend(metadata);
    result.extend(bytes);
    result
}
fn snapshot() -> Vec<u8> {
    let bytes = [0, 255, 65];
    frame(
        &SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "binary".into(),
                size_bytes: 3,
                sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                executable: false,
            }],
        },
        &bytes,
    )
}
fn capture_request() -> SandboxSnapshotCaptureRequest {
    SandboxSnapshotCaptureRequest {
        schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
        capture_id: "capture-owned".into(),
        baseline: None,
    }
}
fn setup(
    block_command: bool,
    block_helper: bool,
) -> (
    Arc<E2bExecutionHostRunner>,
    Arc<Fixture>,
    Arc<Authority>,
    mpsc::Receiver<()>,
) {
    let (entered, receiver) = mpsc::channel();
    let bytes = [0, 255, 65];
    let metadata = SandboxArtifactMetadata {
        filename: "binary".into(),
        size_bytes: 3,
        sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
    };
    let response = SandboxSnapshotCaptureResponse {
        schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
        capture_id: "capture-owned".into(),
        disposition: SnapshotCaptureDisposition::Snapshot,
        baseline_snapshot_sha256: None,
        manifest_sha256: format!(
            "sha256:{:x}",
            Sha256::digest(
                serde_json::to_vec(&SandboxWorkspaceSnapshotManifest {
                    schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
                    files: vec![SandboxWorkspaceSnapshotFile {
                        path: "binary".into(),
                        size_bytes: 3,
                        sha256: metadata.sha256.clone(),
                        executable: false
                    }]
                })
                .unwrap()
            )
        ),
    };
    let transport = Arc::new(Fixture {
        calls: Mutex::new(Vec::new()),
        starts: AtomicUsize::new(0),
        quiescences: AtomicUsize::new(0),
        count: AtomicUsize::new(0),
        helper_unknown: AtomicBool::new(false),
        capture_unknown: AtomicBool::new(false),
        wait_unknown: AtomicBool::new(false),
        block_command,
        block_helper,
        entered,
        released: Mutex::new(false),
        changed: Condvar::new(),
        artifact: Mutex::new(frame(&metadata, &bytes)),
        capture: frame(&response, &snapshot()),
        restore_output: Mutex::new(Vec::new()),
    });
    let authority = Arc::new(Authority {
        denied: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    });
    let mut binding = binding();
    binding.metadata.insert(
        "executionId".into(),
        format!("execution-{}", uuid::Uuid::new_v4()),
    );
    let runner = Arc::new(
        E2bExecutionHostRunner::new(profile(), binding, transport.clone(), authority.clone())
            .unwrap(),
    );
    (runner, transport, authority, receiver)
}
fn command(runner: &E2bExecutionHostRunner) -> ExecutionCommandRequest {
    ExecutionCommandRequest {
        program: "/bin/true".into(),
        args: Vec::new(),
        cwd: "/mnt/data".into(),
        env: HashMap::new(),
        timeout_ms: 1000,
        policy: runner.execution_policy(),
    }
}
fn file(bytes: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file.rewind().unwrap();
    file
}
fn descriptor(bytes: &[u8]) -> WorkspaceSnapshotDescriptor {
    WorkspaceSnapshotDescriptor {
        sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
        size_bytes: bytes.len() as u64,
        expanded_size_bytes: 3,
        file_count: 1,
    }
}

fn successor(
    runner: &E2bExecutionHostRunner,
    authority: Arc<Authority>,
) -> Arc<E2bExecutionHostRunner> {
    Arc::new(crate::e2b_execution_host::tests::successor_for_same_environment(runner, authority))
}

fn next_authority() -> Arc<Authority> {
    Arc::new(Authority {
        denied: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    })
}

// Store authorization is covered separately. This fixture represents its
// already-pinned current Cleanup callback and exercises only the host proof.
fn cleanup_proof_under_current_store_pins(
    runner: &E2bExecutionHostRunner,
) -> Result<(), E2bHostError> {
    runner.confirm_run_quiescence_under_cleanup_pins(
        &runner.binding.metadata["instanceId"],
        &runner.binding.sandbox_id,
    )
}

#[test]
fn successor_keeps_physical_activity_exclusion_across_distinct_run_handles() {
    let (first, _, _, _) = setup(false, false);
    let second = successor(&first, next_authority());
    let held = first.try_exclusive().unwrap().unwrap();
    let successor_is_busy = matches!(second.try_exclusive(), Ok(None));
    drop(held);
    assert!(
        successor_is_busy,
        "a second Run cannot acquire another snapshot guard for the same environment"
    );
    assert!(second.try_exclusive().unwrap().is_some());
}

#[test]
fn successor_keeps_verified_capture_baseline_in_the_shared_environment() {
    let (first, _, _, _) = setup(false, false);
    let wire = snapshot();
    let artifact =
        crate::session_workspace::snapshot::capture_snapshot_artifact(&mut file(&wire), 4096)
            .unwrap();
    let expected = artifact.descriptor.clone();
    let profile_digest = profile().digest().unwrap();
    first
        .try_exclusive()
        .unwrap()
        .unwrap()
        .set_capture_baseline(
            SnapshotCaptureCache::from_verified_snapshot(&artifact),
            &profile_digest,
        )
        .unwrap();
    let second = successor(&first, next_authority());
    let held = second.try_exclusive().unwrap().unwrap();
    assert!(
        held.take_capture_baseline(&expected, &profile_digest)
            .unwrap()
            .is_some(),
        "a verified physical workspace baseline must survive a new Run handle"
    );
}

#[test]
fn successor_cannot_lose_unknown_physical_helper_effects_from_the_old_run() {
    let (first, transport, _, _) = setup(false, false);
    let second = successor(&first, next_authority());
    transport.helper_unknown.store(true, Ordering::SeqCst);
    assert!(matches!(
        first.invoke_helper(TrustedHelperKind::FileSystem, b"request", 1024),
        Err(E2bHostError::Unknown(_))
    ));
    assert!(
        !second.helper_effects_known(),
        "a new Run handle must retain the environment's unresolved helper effect"
    );
    assert!(matches!(
        second.try_exclusive(),
        Err(E2bHostError::Unknown(_))
    ));
    assert!(second
        .run_host_command(Some("new-run-start"), command(&second), None)
        .is_err());
    assert_eq!(transport.starts.load(Ordering::SeqCst), 0);
}

#[test]
fn successor_never_upgrades_stale_run_authority_or_reuses_its_command_receipt() {
    let (first, transport, old_authority, _) = setup(false, false);
    first
        .run_host_command(Some("same-local-operation"), command(&first), None)
        .unwrap();
    let second = successor(&first, next_authority());
    old_authority.denied.store(true, Ordering::SeqCst);
    let helper_count = transport.calls.lock().unwrap().len();
    for kind in [
        TrustedHelperKind::QuiesceAgentProcesses,
        TrustedHelperKind::FileSystem,
    ] {
        assert!(matches!(
            first.invoke_helper(kind, b"", 1024),
            Err(E2bHostError::NotDispatched(_))
        ));
    }
    assert!(first
        .run_host_command(Some("stale-new-start"), command(&first), None)
        .is_err());
    let filesystem = ExecutionFileSystemRequest {
        operation_id: Some("stale-filesystem".into()),
        model_path: "binary".into(),
        cwd: "/mnt/data".into(),
        policy: first.execution_policy(),
        operation: ExecutionFileSystemOperation::ReadFile { max_bytes: 10 },
    };
    assert!(first.run_file_system_operation(filesystem).is_err());
    assert_eq!(transport.calls.lock().unwrap().len(), helper_count);
    second
        .run_host_command(Some("same-local-operation"), command(&second), None)
        .unwrap();
    assert_eq!(transport.starts.load(Ordering::SeqCst), 2);
}

#[test]
fn terminal_run_quiescence_rejects_unresolved_original_command_evidence() {
    let (runner, transport, _, _) = setup(false, false);
    transport.wait_unknown.store(true, Ordering::SeqCst);
    assert!(runner
        .run_host_command(Some("unknown-original-wait"), command(&runner), None)
        .is_err());
    transport.wait_unknown.store(false, Ordering::SeqCst);
    assert!(
        cleanup_proof_under_current_store_pins(&runner).is_err(),
        "an empty agent process observation does not close unknown command evidence"
    );
    assert_eq!(transport.quiescences.load(Ordering::SeqCst), 0);
}

#[test]
fn terminal_run_quiescence_uses_cleanup_pins_without_reviving_execute_authority() {
    let (runner, transport, old_authority, _) = setup(false, false);
    runner
        .run_host_command(Some("finished-original-wait"), command(&runner), None)
        .unwrap();
    old_authority.denied.store(true, Ordering::SeqCst);
    cleanup_proof_under_current_store_pins(&runner).unwrap();
    assert_eq!(transport.quiescences.load(Ordering::SeqCst), 1);
    assert!(runner
        .run_host_command(Some("still-stale-start"), command(&runner), None)
        .is_err());
    assert_eq!(transport.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn terminal_run_quiescence_rejects_a_foreign_physical_target_before_control() {
    let (runner, transport, _, _) = setup(false, false);
    for (instance, sandbox) in [
        ("foreign-instance", runner.binding.sandbox_id.as_str()),
        (
            runner.binding.metadata["instanceId"].as_str(),
            "foreign-sandbox",
        ),
    ] {
        assert!(runner
            .confirm_run_quiescence_under_cleanup_pins(instance, sandbox)
            .is_err());
    }
    assert_eq!(transport.quiescences.load(Ordering::SeqCst), 0);
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[test]
fn terminal_run_quiescence_accepts_commands_proven_not_dispatched() {
    let (runner, transport, _, _) = setup(false, false);
    let cancelled = || Ok(Some("cancelled before dispatch".into()));
    assert!(runner
        .run_host_command(Some("never-started"), command(&runner), Some(&cancelled))
        .is_err());
    cleanup_proof_under_current_store_pins(&runner).unwrap();
    assert_eq!(transport.starts.load(Ordering::SeqCst), 0);
    assert_eq!(transport.quiescences.load(Ordering::SeqCst), 1);
}

#[test]
fn successor_rejects_old_run_with_unknown_command_evidence() {
    let (runner, transport, _, _) = setup(false, false);
    transport.wait_unknown.store(true, Ordering::SeqCst);
    assert!(runner
        .run_host_command(Some("old-original-unknown"), command(&runner), None)
        .is_err());
    assert!(runner.successor(next_authority()).is_err());
    assert_eq!(transport.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn terminal_run_quiescence_cannot_overlap_an_original_wait_or_unknown_helper() {
    let (runner, transport, _, entered) = setup(true, false);
    let child = runner.clone();
    let thread = std::thread::spawn(move || {
        child.run_host_command(Some("original-active-wait"), command(&child), None)
    });
    let entered_result = entered.recv_timeout(Duration::from_secs(2));
    let blocked = cleanup_proof_under_current_store_pins(&runner).is_err();
    transport.release();
    assert!(thread.join().unwrap().is_ok());
    assert!(entered_result.is_ok());
    assert!(blocked);
    assert_eq!(transport.quiescences.load(Ordering::SeqCst), 0);

    transport.helper_unknown.store(true, Ordering::SeqCst);
    assert!(runner
        .invoke_helper(TrustedHelperKind::FileSystem, b"request", 1024)
        .is_err());
    assert!(cleanup_proof_under_current_store_pins(&runner).is_err());
    assert_eq!(transport.quiescences.load(Ordering::SeqCst), 0);
}

#[test]
fn exclusive_workspace_guard_rejects_second_capture_and_releases_on_drop() {
    let (runner, _, _, _) = setup(false, false);
    let first = runner.try_exclusive().unwrap().expect("exclusive guard");
    let second_is_busy = runner.try_exclusive().unwrap().is_none();
    drop(first);
    let released = runner.try_exclusive().unwrap().is_some();
    assert!(
        second_is_busy,
        "a live snapshot guard excludes another snapshot"
    );
    assert!(released);
}
#[test]
fn original_command_wait_keeps_workspace_capture_busy() {
    let (runner, transport, _, entered) = setup(true, false);
    let child = runner.clone();
    let thread = std::thread::spawn(move || {
        child.run_host_command(Some("wait-owned"), command(&child), None)
    });
    let entered_result = entered.recv_timeout(Duration::from_secs(2));
    let capture = runner.try_exclusive();
    let capture_is_busy = matches!(&capture, Ok(None));
    drop(capture);
    transport.release();
    let result = thread.join().unwrap();
    assert!(entered_result.is_ok());
    assert!(result.is_ok());
    assert!(
        capture_is_busy,
        "original command lifetime owns activity through wait"
    );
}
#[test]
fn mutating_helper_keeps_workspace_capture_busy_until_result_is_received() {
    let (runner, transport, _, entered) = setup(false, true);
    let child = runner.clone();
    let thread = std::thread::spawn(move || {
        child.invoke_helper(TrustedHelperKind::FileSystem, b"request", 1024)
    });
    let entered_result = entered.recv_timeout(Duration::from_secs(2));
    let capture = runner.try_exclusive();
    let capture_is_busy = matches!(&capture, Ok(None));
    drop(capture);
    transport.release();
    let result = thread.join().unwrap();
    assert!(entered_result.is_ok());
    assert!(result.is_ok());
    assert!(
        capture_is_busy,
        "helper must retain activity through its outcome latch"
    );
}
#[test]
fn exclusive_workspace_guard_holds_new_command_and_mutating_helper_dispatch() {
    for helper in [false, true] {
        let (runner, transport, _, _) = setup(false, false);
        let guard = runner.try_exclusive().unwrap().expect("exclusive guard");
        let child = runner.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let success = if helper {
                child
                    .invoke_helper(TrustedHelperKind::FileSystem, b"request", 1024)
                    .is_ok()
            } else {
                child
                    .run_host_command(Some("after-capture"), command(&child), None)
                    .is_ok()
            };
            done_tx.send(success).unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let early = done_rx.recv_timeout(Duration::from_millis(200));
        let starts_while_held = transport.starts.load(Ordering::SeqCst);
        let helpers_while_held = transport.calls.lock().unwrap().len();
        drop(guard);
        let result = early
            .as_ref()
            .copied()
            .unwrap_or_else(|_| done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        thread.join().unwrap();
        assert!(
            matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
            "dispatch completed while snapshot guard was held"
        );
        assert_eq!(starts_while_held, 0);
        assert_eq!(helpers_while_held, 0);
        assert!(result);
    }
}
#[test]
fn unknown_mutating_helper_never_becomes_snapshot_eligible_after_quiesce() {
    let (runner, transport, _, _) = setup(false, false);
    transport.helper_unknown.store(true, Ordering::SeqCst);
    assert!(runner
        .invoke_helper(TrustedHelperKind::FileSystem, b"request", 1024)
        .is_err());
    assert!(runner
        .invoke_helper(TrustedHelperKind::QuiesceAgentProcesses, b"", 1024)
        .is_ok());
    assert!(!runner.helper_effects_known());
    assert!(matches!(
        runner.try_exclusive(),
        Err(E2bHostError::Unknown(_))
    ));
}
#[test]
fn fixed_workspace_helpers_preserve_exact_restore_and_capture_frames() {
    let (runner, transport, authority, _) = setup(false, false);
    let guard = runner.try_exclusive().unwrap().expect("exclusive guard");
    guard.require_quiescent().unwrap();
    let wire = snapshot();
    guard
        .restore_snapshot(&mut file(&wire), &descriptor(&wire))
        .unwrap();
    guard.quiesce().unwrap();
    let mut output = guard
        .capture_snapshot(&capture_request(), 4096, &WorkspaceProtection::new())
        .unwrap();
    let mut actual = Vec::new();
    output.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, transport.capture);
    let calls = transport.calls.lock().unwrap();
    let restore = calls
        .iter()
        .find(|(kind, _, _)| *kind == TrustedHelperKind::SnapshotRestore)
        .unwrap();
    assert_eq!(restore.1, wire);
    let capture = calls
        .iter()
        .find(|(kind, _, _)| *kind == TrustedHelperKind::SnapshotCapture)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&capture.1).unwrap(),
        serde_json::to_value(capture_request()).unwrap()
    );
    assert_eq!(capture.2, 4096);
    assert!(authority.calls.load(Ordering::SeqCst) >= 4);
}
#[test]
fn fixed_state_requires_zero_processes_and_current_authority() {
    let (runner, transport, authority, _) = setup(false, false);
    let guard = runner.try_exclusive().unwrap().expect("exclusive guard");
    guard.require_quiescent().unwrap();
    transport.count.store(2, Ordering::SeqCst);
    assert!(guard.require_quiescent().is_err());
    let before = transport.calls.lock().unwrap().len();
    authority.denied.store(true, Ordering::SeqCst);
    assert!(guard.require_quiescent().is_err());
    assert_eq!(
        transport.calls.lock().unwrap().len(),
        before,
        "denied owner cannot start fixed helpers"
    );
}
#[test]
fn restore_rejects_corrupt_input_before_mutating_dispatch() {
    let (runner, transport, _, _) = setup(false, false);
    let guard = runner.try_exclusive().unwrap().expect("exclusive guard");
    let wire = snapshot();
    for fault in [0, 1, 2] {
        let mut desc = descriptor(&wire);
        let mut input = wire.clone();
        match fault {
            0 => desc.sha256 = format!("sha256:{}", "0".repeat(64)),
            1 => desc.size_bytes += 1,
            _ => input.push(0),
        }
        assert!(matches!(
            guard.restore_snapshot(&mut file(&input), &desc),
            Err(E2bHostError::NotDispatched(_))
        ));
    }
    assert!(transport.calls.lock().unwrap().is_empty());
}
#[test]
fn unexpected_restore_output_latches_unknown_and_blocks_snapshot() {
    let (runner, transport, _, _) = setup(false, false);
    *transport.restore_output.lock().unwrap() = b"unexpected".to_vec();
    let guard = runner.try_exclusive().unwrap().expect("exclusive guard");
    let wire = snapshot();
    assert!(matches!(
        guard.restore_snapshot(&mut file(&wire), &descriptor(&wire)),
        Err(E2bHostError::Unknown(_))
    ));
    assert!(!runner.helper_effects_known());
    let before = transport.calls.lock().unwrap().len();
    assert!(guard
        .capture_snapshot(&capture_request(), 4096, &WorkspaceProtection::new())
        .is_err());
    assert_eq!(transport.calls.lock().unwrap().len(), before);
}
#[test]
fn artifact_read_preserves_binary_bytes_and_enforces_fixed_helper_request() {
    let (runner, transport, _, _) = setup(false, false);
    let (metadata, bytes) = runner.read_artifact("binary").unwrap();
    assert_eq!(bytes, [0, 255, 65]);
    assert_eq!(metadata.size_bytes, 3);
    assert_eq!(
        metadata.sha256,
        format!("sha256:{:x}", Sha256::digest(&bytes))
    );
    let calls = transport.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, TrustedHelperKind::ReadArtifact);
    let request: SandboxArtifactRequest = serde_json::from_slice(&calls[0].1).unwrap();
    assert_eq!(request.path, "binary");
    assert_eq!(request.max_bytes, MAX_PUBLISHED_ARTIFACT_BYTES);
}
#[test]
fn artifact_read_rejects_corrupt_size_hash_and_truncated_metadata() {
    let (runner, transport, _, _) = setup(false, false);
    let valid = transport.artifact.lock().unwrap().clone();
    for fault in [0, 1, 2, 3] {
        let mut metadata: SandboxArtifactMetadata =
            serde_json::from_slice(&valid[4..valid.len() - 3]).unwrap();
        match fault {
            0 => metadata.size_bytes += 1,
            1 => metadata.sha256 = format!("sha256:{}", "0".repeat(64)),
            2 => metadata.size_bytes = MAX_PUBLISHED_ARTIFACT_BYTES + 1,
            _ => {}
        }
        *transport.artifact.lock().unwrap() = if fault == 3 {
            vec![0, 0, 0, 100, 123]
        } else {
            frame(&metadata, &[0, 255, 65])
        };
        assert!(runner.read_artifact("binary").is_err());
    }
}

#[test]
fn raw_capture_spool_counts_successful_partial_writes_without_guessing_total() {
    struct Partial(usize);
    impl Write for Partial {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("private disk error"));
            }
            let count = self.0.min(bytes.len());
            self.0 -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (result, events) = crate::observations::capture_protection(|| {
        write_raw_capture_frame(
            &mut Partial(3),
            b"a much larger private frame",
            &WorkspaceProtection::new(),
        )
    });
    assert!(result.is_err());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["metric"], "rawCaptureSpoolWrittenBytes");
    assert_eq!(events[0]["value"], 3);
    assert_eq!(events[0]["outcome"], "error");
    assert!(!serde_json::to_string(&events).unwrap().contains("private"));
}

#[test]
fn observed_capture_counts_exact_raw_spool_and_preserves_fencing_and_unknown_latch() {
    let (runner, transport, authority, _) = setup(false, false);
    let guard = runner.try_exclusive().unwrap().unwrap();
    let (file, events) = crate::observations::capture_protection(|| {
        let observation = WorkspaceProtection::new().with_context(
            crate::observations::ProtectionTrigger::TerminalCommit,
            "run-exact",
            Some("job-exact"),
        );
        guard
            .capture_snapshot(&capture_request(), 4096, &observation)
            .unwrap()
    });
    assert_eq!(
        file.metadata().unwrap().len(),
        transport.capture.len() as u64
    );
    let measured = events
        .iter()
        .find(|e| e["metric"] == "rawCaptureSpoolWrittenBytes")
        .unwrap();
    assert_eq!(measured["value"], transport.capture.len());
    assert_eq!(measured["outcome"], "ok");
    assert!(events
        .iter()
        .all(|e| e["operationId"] == measured["operationId"]
            && e["runId"] == "run-exact"
            && e["jobId"] == "job-exact"));
    let before = transport.calls.lock().unwrap().len();
    authority.denied.store(true, Ordering::SeqCst);
    assert!(matches!(
        guard.capture_snapshot(&capture_request(), 4096, &WorkspaceProtection::new()),
        Err(E2bHostError::NotDispatched(_))
    ));
    assert_eq!(transport.calls.lock().unwrap().len(), before);
    assert!(runner.helper_effects_known());
    authority.denied.store(false, Ordering::SeqCst);
    transport.capture_unknown.store(true, Ordering::SeqCst);
    assert!(matches!(
        guard.capture_snapshot(&capture_request(), 4096, &WorkspaceProtection::new()),
        Err(E2bHostError::Unknown("fixture_capture_unknown"))
    ));
    assert!(!runner.helper_effects_known());
    drop(guard);
    assert!(matches!(
        runner.try_exclusive(),
        Err(E2bHostError::Unknown(_))
    ));
}

fn task_image_setup() -> (Arc<E2bExecutionHostRunner>, Arc<Fixture>, Arc<Authority>) {
    let (_, transport, authority, _) = setup(false, false);
    let mut value = serde_json::to_value(profile()).unwrap();
    value["workspaceInitialization"] = json!("taskImage");
    value["lifetime"] = json!("retained");
    value["user"] = json!("root");
    value["workspaceRoot"] = json!("/app");
    let profile: EnvironmentProfile = serde_json::from_value(value).unwrap();
    let mut binding = binding();
    binding
        .metadata
        .insert("profileDigest".into(), profile.digest().unwrap());
    let runner = Arc::new(
        E2bExecutionHostRunner::new(profile, binding, transport.clone(), authority.clone())
            .unwrap(),
    );
    (runner, transport, authority)
}
#[test]
fn task_image_retention_proves_current_identity_without_quiescing_environment_services() {
    let (runner, transport, authority) = task_image_setup();
    transport.count.store(5, Ordering::SeqCst);
    let session = runner.try_exclusive().unwrap().unwrap();
    let binding = session.verify_retained_provider_binding().unwrap();
    assert_eq!(
        binding.environment_id,
        runner.binding.metadata["environmentId"]
    );
    assert_eq!(binding.instance_id, runner.binding.metadata["instanceId"]);
    assert_eq!(binding.sandbox_id, runner.binding.sandbox_id);
    assert_eq!(binding.profile_digest, runner.profile.digest().unwrap());
    assert_eq!(transport.count.load(Ordering::SeqCst), 5);
    assert!(transport.calls.lock().unwrap().is_empty());
    authority.denied.store(true, Ordering::SeqCst);
    assert_eq!(
        session.verify_retained_provider_binding(),
        Err(E2bHostError::NotDispatched("fixture_authority_denied"))
    );
}
#[test]
fn task_image_retention_rejects_unclosed_original_commands_even_when_sdk_identity_is_known() {
    let (runner, transport, _) = task_image_setup();
    transport.wait_unknown.store(true, Ordering::SeqCst);
    let mut request = command(&runner);
    request.cwd = "/app".into();
    assert!(runner
        .run_host_command(Some("1:unclosed"), request, None)
        .is_err());
    let session = runner.try_exclusive().unwrap().unwrap();
    assert_eq!(
        session.verify_retained_provider_binding(),
        Err(E2bHostError::NotDispatched("e2b_run_commands_unresolved"))
    );
    assert!(transport.calls.lock().unwrap().is_empty());
}
