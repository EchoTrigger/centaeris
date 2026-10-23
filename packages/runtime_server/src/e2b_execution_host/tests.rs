use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct FixtureAuthority {
    denied: AtomicBool,
    calls: AtomicUsize,
}
impl FixtureAuthority {
    fn new() -> Self {
        Self {
            denied: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }
}
impl E2bDispatchAuthority for FixtureAuthority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.denied.load(Ordering::SeqCst) {
            return Err(E2bHostError::NotDispatched(
                "fixture_current_authority_denied",
            ));
        }
        operation()
    }
}
struct FixtureTransport {
    inspections: AtomicUsize,
    starts: AtomicUsize,
    waits: AtomicUsize,
    helpers: AtomicUsize,
    inspect: Mutex<E2bSandboxObservation>,
    file_response: Mutex<Vec<u8>>,
    last_file_request: Mutex<Option<ExecutionFileSystemRequest>>,
    start_error: Mutex<Option<E2bHostError>>,
    wait_error: Mutex<Option<E2bHostError>>,
    helper_error: Mutex<Option<E2bHostError>>,
    prepare_response: Mutex<Vec<u8>>,
    helper_kinds: Mutex<Vec<TrustedHelperKind>>,
}
impl FixtureTransport {
    fn new() -> Self {
        let mut response = Vec::new();
        hosted_execution::protocol::write_filesystem_result(
            &mut response,
            Ok(ExecutionFileSystemOutput::ReadFile(
                ExecutionFileReadOutput {
                    identity: identity(),
                    bytes: vec![0, 255, 65],
                    file_hash: "sha256:fixture".into(),
                },
            ))
            .into(),
        )
        .unwrap();
        Self {
            inspections: AtomicUsize::new(0),
            starts: AtomicUsize::new(0),
            waits: AtomicUsize::new(0),
            helpers: AtomicUsize::new(0),
            inspect: Mutex::new(E2bSandboxObservation {
                binding: binding(),
                running: true,
                on_timeout_kill: true,
                auto_resume: false,
            }),
            file_response: Mutex::new(response),
            last_file_request: Mutex::new(None),
            start_error: Mutex::new(None),
            wait_error: Mutex::new(None),
            helper_error: Mutex::new(None),
            prepare_response: Mutex::new(Vec::new()),
            helper_kinds: Mutex::new(Vec::new()),
        }
    }
}
impl E2bHostTransport for FixtureTransport {
    fn inspect(&self, _: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        self.inspections.fetch_add(1, Ordering::SeqCst);
        Ok(self.inspect.lock().unwrap().clone())
    }
    fn helper(
        &self,
        _: &E2bSandboxBinding,
        kind: TrustedHelperKind,
        input: &[u8],
        _: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.helpers.fetch_add(1, Ordering::SeqCst);
        self.helper_kinds.lock().unwrap().push(kind);
        if matches!(
            kind,
            TrustedHelperKind::AgentProcessState | TrustedHelperKind::QuiesceAgentProcesses
        ) {
            return Ok(br#"{"activeProcessCount":0}"#.to_vec());
        }
        if let Some(error) = *self.helper_error.lock().unwrap() {
            return Err(error);
        }
        if matches!(
            kind,
            TrustedHelperKind::PrepareWorkspace | TrustedHelperKind::PrepareManagedWorkspace
        ) {
            assert!(input.is_empty());
            return Ok(self.prepare_response.lock().unwrap().clone());
        }
        assert_eq!(kind, TrustedHelperKind::FileSystem);
        let request =
            hosted_execution::protocol::read_filesystem_request(&mut std::io::Cursor::new(input))
                .unwrap();
        *self.last_file_request.lock().unwrap() = Some(request);
        Ok(self.file_response.lock().unwrap().clone())
    }
    fn start(
        &self,
        _: &E2bSandboxBinding,
        _: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.start_error.lock().unwrap().map_or(Ok(73), Err)
    }
    fn wait(
        &self,
        b: &E2bSandboxBinding,
        pid: i32,
        _: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        self.waits.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = *self.wait_error.lock().unwrap() {
            return Err(error);
        }
        Ok(E2bCommandOutput {
            sandbox_id: b.sandbox_id.clone(),
            pid,
            exit_code: 7,
            stdout: vec![65, 255, 66],
            stderr: vec![0, 67],
        })
    }
    fn quiesce(&self, _: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        Ok(())
    }
}
fn identity() -> ExecutionFileIdentity {
    ExecutionFileIdentity {
        key: "fixture".into(),
        display_path: "binary".into(),
    }
}
pub(super) fn binding() -> E2bSandboxBinding {
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
    .map(|k| (k.into(), format!("{k}-owned")))
    .collect();
    metadata.insert("profileDigest".into(), profile().digest().unwrap());
    // Independent tests must not claim the same physical execution in the
    // process-wide activity registry. Rebuilt runners in one test still share it.
    metadata.insert(
        "executionId".into(),
        format!("execution-{:?}", std::thread::current().id()),
    );
    E2bSandboxBinding {
        sandbox_id: "sandbox-owned".into(),
        template_id: "core-template".into(),
        metadata,
    }
}
pub(crate) fn profile() -> EnvironmentProfile {
    serde_json::from_value(serde_json::json!({"schema":"runtime.e2b_environment_profile.v1","provider":"e2b","providerConfigDigest":format!("sha256:{}","a".repeat(64)),"templateRef":{"templateId":"core-template","buildId":"123e4567-e89b-12d3-a456-426614174000"},"workspaceRoot":"/mnt/data","user":"agent","network":"disabled","lifetime":"perRun","timeoutSeconds":120,"inputBudgetBytes":67108864})).unwrap()
}
fn host(transport: Arc<FixtureTransport>) -> E2bExecutionHostRunner {
    E2bExecutionHostRunner::new(
        profile(),
        binding(),
        transport,
        Arc::new(FixtureAuthority::new()),
    )
    .unwrap()
}

pub(crate) fn fixture_host() -> E2bExecutionHostRunner {
    host(Arc::new(FixtureTransport::new()))
}
pub(super) fn successor_for_same_environment(
    original: &E2bExecutionHostRunner,
    authority: Arc<dyn E2bDispatchAuthority>,
) -> E2bExecutionHostRunner {
    original.successor(authority).unwrap()
}
fn command(host: &E2bExecutionHostRunner) -> ExecutionCommandRequest {
    ExecutionCommandRequest {
        program: "/bin/bash".into(),
        args: vec!["-c".into(), "printf test".into()],
        cwd: "/mnt/data".into(),
        env: HashMap::new(),
        timeout_ms: 1000,
        policy: host.execution_policy(),
    }
}
fn read_request(host: &E2bExecutionHostRunner) -> ExecutionFileSystemRequest {
    ExecutionFileSystemRequest {
        operation_id: Some("fs-op".into()),
        model_path: "binary".into(),
        cwd: "/mnt/data".into(),
        policy: host.execution_policy(),
        operation: ExecutionFileSystemOperation::ReadFile { max_bytes: 10 },
    }
}
#[test]
fn status_requires_matching_running_provider_evidence() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    assert_eq!(
        runner.status(&runner.execution_policy()).unwrap().health,
        ExecutionHostHealth::Ready
    );
    for mode in 0..4 {
        let mut observation = fixture.inspect.lock().unwrap();
        *observation = E2bSandboxObservation {
            binding: binding(),
            running: true,
            on_timeout_kill: true,
            auto_resume: false,
        };
        match mode {
            0 => observation.binding.sandbox_id = "foreign".into(),
            1 => observation.running = false,
            2 => observation.on_timeout_kill = false,
            _ => observation.auto_resume = true,
        };
        drop(observation);
        assert!(runner.status(&runner.execution_policy()).is_err());
    }
}
#[test]
fn status_requires_current_authority_before_starting_observation_helper() {
    let fixture = Arc::new(FixtureTransport::new());
    let authority = Arc::new(FixtureAuthority::new());
    authority.denied.store(true, Ordering::SeqCst);
    let runner =
        E2bExecutionHostRunner::new(profile(), binding(), fixture.clone(), authority.clone())
            .unwrap();

    let result = runner.status(&runner.execution_policy());
    assert_eq!(
        fixture.inspections.load(Ordering::SeqCst),
        0,
        "a denied controller must not start the provider observation helper"
    );
    assert!(result.is_err());
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn filesystem_uses_shared_binary_frame_and_preserves_result() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    let output = runner
        .run_file_system_operation(read_request(&runner))
        .unwrap();
    let ExecutionFileSystemOutput::ReadFile(file) = output else {
        panic!("read expected")
    };
    assert_eq!(file.bytes, vec![0, 255, 65]);
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 1);
}
#[test]
fn filesystem_bulk_frame_preserves_large_write_and_observed_hash() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    let bytes = vec![255; 2 * 1024 * 1024 + 17];
    let mut request = read_request(&runner);
    request.operation = ExecutionFileSystemOperation::WriteFile {
        content: bytes.clone(),
        observed_file_hash: Some("sha256:observed".into()),
        create_only: true,
    };
    let mut response = Vec::new();
    hosted_execution::protocol::write_filesystem_result(
        &mut response,
        Ok(ExecutionFileSystemOutput::WriteFile(
            ExecutionFileWriteOutput {
                identity: identity(),
                created: true,
            },
        ))
        .into(),
    )
    .unwrap();
    *fixture.file_response.lock().unwrap() = response;
    assert!(matches!(
        runner.run_file_system_operation(request).unwrap(),
        ExecutionFileSystemOutput::WriteFile(_)
    ));
    let request = fixture.last_file_request.lock().unwrap().take().unwrap();
    assert_eq!(
        request.operation,
        ExecutionFileSystemOperation::WriteFile {
            content: bytes,
            observed_file_hash: Some("sha256:observed".into()),
            create_only: true
        }
    );
}
#[test]
fn corrupt_filesystem_payload_is_not_published() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    fixture.file_response.lock().unwrap().pop();
    assert!(runner
        .run_file_system_operation(read_request(&runner))
        .is_err());
}
#[test]
fn command_preserves_raw_decode_and_nonzero_exit_without_repeating_start() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    let request = command(&runner);
    let first = runner
        .run_host_command(Some("operation-a"), request.clone(), None)
        .unwrap();
    let repeat = runner
        .run_host_command(Some("operation-a"), request.clone(), None)
        .unwrap();
    assert_eq!(first, repeat);
    assert_eq!(first.process.exit_code, Some(7));
    assert_eq!(first.failure_kind, ExecutionHostFailureKind::CommandFailed);
    assert_eq!(first.process.stdout_decode.raw_byte_length, 3);
    assert_eq!(first.process.stdout_decode.status, "lossy");
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.waits.load(Ordering::SeqCst), 1);
    assert!(matches!(
        runner
            .recover_host_command("missing", request.clone(), None)
            .unwrap(),
        ExecutionHostCommandRecovery::Unknown { .. }
    ));
    assert!(matches!(
        runner
            .recover_host_command("operation-a", request, None)
            .unwrap(),
        ExecutionHostCommandRecovery::Completed(_)
    ));
}
#[test]
fn unknown_start_or_wait_is_never_reexecuted() {
    for start_unknown in [true, false] {
        let fixture = Arc::new(FixtureTransport::new());
        if start_unknown {
            *fixture.start_error.lock().unwrap() =
                Some(E2bHostError::Unknown("fixture_response_lost"));
        } else {
            *fixture.wait_error.lock().unwrap() =
                Some(E2bHostError::Unknown("fixture_output_lost"));
        }
        let runner = host(fixture.clone());
        let request = command(&runner);
        assert!(runner
            .run_host_command(Some("uncertain"), request.clone(), None)
            .is_err());
        assert!(runner
            .run_host_command(Some("uncertain"), request.clone(), None)
            .is_err());
        assert!(matches!(
            runner
                .recover_host_command("uncertain", request, None)
                .unwrap(),
            ExecutionHostCommandRecovery::Unknown { .. }
        ));
        assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn restarted_host_never_guesses_a_missing_command_was_not_accepted() {
    let fixture = Arc::new(FixtureTransport::new());
    let old = host(fixture.clone());
    let request = command(&old);
    old.run_host_command(Some("original"), request.clone(), None)
        .unwrap();
    let restarted = host(fixture.clone());
    assert!(matches!(
        restarted
            .recover_host_command("original", request.clone(), None)
            .unwrap(),
        ExecutionHostCommandRecovery::Unknown { .. }
    ));
    assert!(restarted
        .run_host_command(Some("original"), request, None)
        .is_err());
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
}
#[test]
fn operation_binding_and_frozen_policy_reject_mutation_before_dispatch() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    let request = command(&runner);
    runner
        .run_host_command(Some("owned"), request.clone(), None)
        .unwrap();
    let mut changed = request.clone();
    changed.args.push("another".into());
    assert!(runner
        .run_host_command(Some("owned"), changed, None)
        .is_err());
    let mut changed = request;
    changed.policy.network = NetworkPolicy::PublicInternet;
    assert!(runner.run_host_command(Some("new"), changed, None).is_err());
    let mut fs = read_request(&runner);
    fs.policy.filesystem.denied_read_paths.clear();
    assert!(runner.run_file_system_operation(fs).is_err());
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 0);
}
#[test]
fn authority_denial_proves_no_start_but_never_grants_default_authority() {
    let fixture = Arc::new(FixtureTransport::new());
    let authority = Arc::new(FixtureAuthority::new());
    authority.denied.store(true, Ordering::SeqCst);
    let runner =
        E2bExecutionHostRunner::new(profile(), binding(), fixture.clone(), authority.clone())
            .unwrap();
    let request = command(&runner);
    assert!(runner
        .run_host_command(Some("denied"), request.clone(), None)
        .is_err());
    assert!(matches!(
        runner
            .recover_host_command("denied", request, None)
            .unwrap(),
        ExecutionHostCommandRecovery::NotAccepted { .. }
    ));
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
    assert_eq!(authority.calls.load(Ordering::SeqCst), 1);
    assert!(runner
        .run_file_system_operation(read_request(&runner))
        .is_err());
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 0);
}
#[test]
fn retained_profile_constructs_a_bound_runner_without_provider_side_effects() {
    let fixture = Arc::new(FixtureTransport::new());
    let mut retained = serde_json::to_value(profile()).unwrap();
    retained["lifetime"] = "retained".into();
    let retained: EnvironmentProfile = serde_json::from_value(retained).unwrap();
    let mut bound = binding();
    bound
        .metadata
        .insert("profileDigest".into(), retained.digest().unwrap());
    let runner = E2bExecutionHostRunner::new(
        retained,
        bound,
        fixture.clone(),
        Arc::new(FixtureAuthority::new()),
    )
    .expect("an explicitly frozen retained profile supports the same bounded host tools");
    assert_eq!(runner.workspace_root(), "/mnt/data");
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 0);
}

#[test]
fn unsupported_profile_and_binding_fail_before_any_provider_action() {
    let fixture = Arc::new(FixtureTransport::new());
    for (field, value) in [
        ("lifetime", "unsupported"),
        ("user", "root"),
        ("workspaceRoot", "/another"),
    ] {
        let mut value_profile = serde_json::to_value(profile()).unwrap();
        value_profile[field] = value.into();
        let profile = serde_json::from_value(value_profile).unwrap();
        assert!(E2bExecutionHostRunner::new(
            profile,
            binding(),
            fixture.clone(),
            Arc::new(FixtureAuthority::new())
        )
        .is_err());
    }
    let mut changed = binding();
    changed
        .metadata
        .insert("profileDigest".into(), "wrong".into());
    assert!(E2bExecutionHostRunner::new(
        profile(),
        changed,
        fixture.clone(),
        Arc::new(FixtureAuthority::new())
    )
    .is_err());
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
}

struct BlockingTransport {
    base: FixtureTransport,
    started: std::sync::Barrier,
    stopped: Mutex<bool>,
    changed: std::sync::Condvar,
    quiesces: AtomicUsize,
}
impl E2bHostTransport for BlockingTransport {
    fn inspect(&self, b: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        self.base.inspect(b)
    }
    fn helper(
        &self,
        b: &E2bSandboxBinding,
        k: TrustedHelperKind,
        i: &[u8],
        l: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.base.helper(b, k, i, l)
    }
    fn start(
        &self,
        b: &E2bSandboxBinding,
        r: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        self.base.start(b, r)
    }
    fn wait(
        &self,
        b: &E2bSandboxBinding,
        p: i32,
        d: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        self.started.wait();
        let mut stopped = self.stopped.lock().unwrap();
        while !*stopped {
            stopped = self.changed.wait(stopped).unwrap();
        }
        self.base.wait(b, p, d)
    }
    fn quiesce(&self, _: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        self.quiesces.fetch_add(1, Ordering::SeqCst);
        *self.stopped.lock().unwrap() = true;
        self.changed.notify_all();
        Ok(())
    }
}
#[test]
fn cancellation_advances_while_original_wait_is_blocked_without_tight_probe_polling() {
    let fixture = Arc::new(BlockingTransport {
        base: FixtureTransport::new(),
        started: std::sync::Barrier::new(2),
        stopped: Mutex::new(false),
        changed: std::sync::Condvar::new(),
        quiesces: AtomicUsize::new(0),
    });
    let runner = E2bExecutionHostRunner::new(
        profile(),
        binding(),
        fixture.clone(),
        Arc::new(FixtureAuthority::new()),
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let probe_fixture = fixture.clone();
    let probe_calls = calls.clone();
    let probe = move || {
        if probe_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(None)
        } else {
            probe_fixture.started.wait();
            Ok(Some("cancelled".into()))
        }
    };
    let output = runner
        .run_host_command(Some("blocked"), command(&runner), Some(&probe))
        .unwrap();
    assert_eq!(output.failure_kind, ExecutionHostFailureKind::Cancelled);
    assert_eq!(fixture.quiesces.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.base.starts.load(Ordering::SeqCst), 1);
}

#[test]
fn unknown_helper_blocks_snapshot_protection_but_keeps_fixed_control_available() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    *fixture.helper_error.lock().unwrap() =
        Some(E2bHostError::Unknown("fixture_helper_completion_lost"));
    assert!(runner
        .run_file_system_operation(read_request(&runner))
        .is_err());
    assert!(runner
        .invoke_helper(TrustedHelperKind::SnapshotCapture, b"{}", 1024)
        .is_err());
    assert_eq!(
        fixture.helpers.load(Ordering::SeqCst),
        1,
        "uncertain root helper must block snapshot dispatch"
    );
    assert!(!runner.helper_effects_known());
    assert!(runner
        .invoke_helper(TrustedHelperKind::AgentProcessState, b"", 1024)
        .is_ok());
    assert!(runner
        .invoke_helper(TrustedHelperKind::QuiesceAgentProcesses, b"", 1024)
        .is_ok());
    assert!(
        !runner.helper_effects_known(),
        "quiescing agent UID cannot settle an unknown root helper"
    );
}
#[test]
fn definitely_undispatched_helper_does_not_invent_unknown_side_effects() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    *fixture.helper_error.lock().unwrap() =
        Some(E2bHostError::NotDispatched("fixture_authority_denied"));
    assert!(runner
        .run_file_system_operation(read_request(&runner))
        .is_err());
    assert!(runner.helper_effects_known());
    *fixture.helper_error.lock().unwrap() = None;
    assert!(runner
        .run_file_system_operation(read_request(&runner))
        .is_ok());
}

#[test]
fn unknown_mutating_helper_prevents_new_bash_without_dispatching() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    *fixture.helper_error.lock().unwrap() =
        Some(E2bHostError::Unknown("fixture_filesystem_reply_lost"));
    let mut write = read_request(&runner);
    write.operation = ExecutionFileSystemOperation::WriteFile {
        content: b"possibly-still-writing".to_vec(),
        observed_file_hash: None,
        create_only: true,
    };
    assert!(runner.run_file_system_operation(write).is_err());
    assert!(!runner.helper_effects_known());
    let result = runner.run_host_command(
        Some("new-command-after-unknown-helper"),
        command(&runner),
        None,
    );
    assert!(
        result.is_err(),
        "a root helper may still mutate the workspace"
    );
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.waits.load(Ordering::SeqCst), 0);
}

#[test]
fn unknown_mutating_helper_cannot_report_ready_or_be_cleared_by_fixed_control() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    *fixture.helper_error.lock().unwrap() =
        Some(E2bHostError::Unknown("fixture_filesystem_reply_lost"));
    let mut write = read_request(&runner);
    write.operation = ExecutionFileSystemOperation::WriteFile {
        content: b"possibly-still-writing".to_vec(),
        observed_file_hash: None,
        create_only: true,
    };
    assert!(runner.run_file_system_operation(write).is_err());
    assert!(
        runner.status(&runner.execution_policy()).is_err(),
        "running provider evidence is insufficient while helper effects remain uncertain"
    );
    for kind in [
        TrustedHelperKind::AgentProcessState,
        TrustedHelperKind::QuiesceAgentProcesses,
    ] {
        assert!(runner.invoke_helper(kind, &[], 1024).is_ok());
    }
    assert!(!runner.helper_effects_known());
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.starts.load(Ordering::SeqCst), 0);
}

#[test]
fn workspace_preparation_requires_fixed_helper_and_exact_empty_output() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    assert_eq!(runner.prepare_workspace(), Ok(()));
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 1);
    assert!(runner.helper_effects_known());
    *fixture.prepare_response.lock().unwrap() = b"unexpected-output".to_vec();
    assert_eq!(
        runner.prepare_workspace(),
        Err(E2bHostError::Unknown(
            "e2b_workspace_preparation_output_invalid"
        ))
    );
    assert!(!runner.helper_effects_known());
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 2);
    assert!(runner.prepare_workspace().is_err());
    assert_eq!(
        fixture.helpers.load(Ordering::SeqCst),
        2,
        "an uncertain preparation cannot be retried as a fresh physical effect"
    );
}

#[test]
fn workspace_preparation_not_dispatched_remains_known_and_can_retry() {
    let fixture = Arc::new(FixtureTransport::new());
    let runner = host(fixture.clone());
    *fixture.helper_error.lock().unwrap() =
        Some(E2bHostError::NotDispatched("fixture_authority_expired"));
    assert_eq!(
        runner.prepare_workspace(),
        Err(E2bHostError::NotDispatched("fixture_authority_expired"))
    );
    assert!(runner.helper_effects_known());
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 1);
    *fixture.helper_error.lock().unwrap() = None;
    assert_eq!(runner.prepare_workspace(), Ok(()));
    assert!(runner.helper_effects_known());
    assert_eq!(fixture.helpers.load(Ordering::SeqCst), 2);
}

struct ContextScopeAuthority;
impl E2bDispatchAuthority for ContextScopeAuthority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        std::thread::scope(|scope| scope.spawn(operation).join().unwrap())
    }
}

struct RetainedContextTransport {
    inner: FixtureTransport,
    spawn_context: crate::observations::ContextSnapshot,
    events: Mutex<Vec<(&'static str, Option<String>)>>,
}
impl RetainedContextTransport {
    fn observe(&self, operation: &'static str) {
        let (_, records) = crate::observations::capture_timings(true, || {
            let _spawn_fallback =
                crate::observations::Context::enter_if_absent(&self.spawn_context);
            crate::observations::timed("retainedContextProbe", || Ok(())).unwrap();
        });
        assert_eq!(records.len(), 1);
        self.events
            .lock()
            .unwrap()
            .push((operation, records[0]["jobId"].as_str().map(str::to_string)));
    }
}
impl E2bHostTransport for RetainedContextTransport {
    fn inspect(&self, binding: &E2bSandboxBinding) -> Result<E2bSandboxObservation, E2bHostError> {
        self.observe("inspect");
        self.inner.inspect(binding)
    }
    fn helper(
        &self,
        binding: &E2bSandboxBinding,
        kind: TrustedHelperKind,
        input: &[u8],
        limit: usize,
    ) -> Result<Vec<u8>, E2bHostError> {
        self.observe("helper");
        self.inner.helper(binding, kind, input, limit)
    }
    fn start(
        &self,
        binding: &E2bSandboxBinding,
        request: &ExecutionCommandRequest,
    ) -> Result<i32, E2bHostError> {
        self.observe("start");
        self.inner.start(binding, request)
    }
    fn wait(
        &self,
        binding: &E2bSandboxBinding,
        pid: i32,
        timeout: Duration,
    ) -> Result<E2bCommandOutput, E2bHostError> {
        self.observe("wait");
        self.inner.wait(binding, pid, timeout)
    }
    fn quiesce(&self, binding: &E2bSandboxBinding) -> Result<(), E2bHostError> {
        self.observe("quiesce");
        self.inner.quiesce(binding)
    }
}

fn retained_context_fixture() -> (E2bExecutionHostRunner, Arc<RetainedContextTransport>) {
    let _creator =
        crate::observations::Context::enter(br#"{"jobId":"agent_run.lifecycle:context-A"}"#);
    let mut retained = profile();
    let EnvironmentProfile::E2b(value) = &mut retained else {
        unreachable!()
    };
    value.lifetime = "retained".into();
    let mut target = binding();
    target
        .metadata
        .insert("profileDigest".into(), retained.digest().unwrap());
    let inner = FixtureTransport::new();
    inner.inspect.lock().unwrap().binding = target.clone();
    let transport = Arc::new(RetainedContextTransport {
        inner,
        spawn_context: crate::observations::Context::snapshot(),
        events: Mutex::new(Vec::new()),
    });
    let runner = E2bExecutionHostRunner::new(
        retained,
        target,
        transport.clone(),
        Arc::new(ContextScopeAuthority),
    )
    .unwrap();
    (runner, transport)
}

#[test]
fn retained_run_context_successor_restores_b_across_authority_and_wait_threads() {
    let _ = crate::observations::capture_timings(true, || {
        let (original, transport) = retained_context_fixture();
        let successor = {
            let _current = crate::observations::Context::enter(
                br#"{"jobId":"agent_run.lifecycle:context-B"}"#,
            );
            original.successor(Arc::new(ContextScopeAuthority)).unwrap()
        };
        successor.prepare_workspace().unwrap();
        successor.status(&successor.execution_policy()).unwrap();
        successor
            .run_host_command(Some("context-command"), command(&successor), None)
            .unwrap();
        successor
            .confirm_run_quiescence_under_cleanup_pins("instanceId-owned", "sandbox-owned")
            .unwrap();
        let events = transport.events.lock().unwrap();
        assert_eq!(
            events.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["helper", "inspect", "start", "wait", "quiesce", "helper"]
        );
        assert!(events.iter().all(|(_, job)| job.as_deref() == Some("agent_run.lifecycle:context-B")), "successor observations must not use the shared transport's creator context: {events:?}");
        assert_eq!(transport.inner.starts.load(Ordering::SeqCst), 1);
        assert_eq!(transport.inner.waits.load(Ordering::SeqCst), 1);
        assert_eq!(transport.inner.helpers.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn retained_run_context_preserves_existing_ambient_context_over_host_snapshot() {
    let _ = crate::observations::capture_timings(true, || {
        let (original, transport) = retained_context_fixture();
        let successor = {
            let _current = crate::observations::Context::enter(
                br#"{"jobId":"agent_run.lifecycle:context-B"}"#,
            );
            original.successor(Arc::new(ContextScopeAuthority)).unwrap()
        };
        {
            let _ambient = crate::observations::Context::enter(
                br#"{"jobId":"agent_run.lifecycle:context-C"}"#,
            );
            successor.prepare_workspace().unwrap();
            successor
                .run_host_command(Some("ambient-command"), command(&successor), None)
                .unwrap();
            transport.observe("ambient-after");
        }
        assert!(transport
            .events
            .lock()
            .unwrap()
            .iter()
            .all(|(_, job)| job.as_deref() == Some("agent_run.lifecycle:context-C")));
        transport.observe("outside");
        assert_eq!(
            transport
                .events
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .1
                .as_deref(),
            Some("agent_run.lifecycle:context-A"),
            "entry and callback guards must restore their previous contexts"
        );
    });
}

#[test]
fn retained_run_context_unbound_successor_suppresses_creator_fallback_without_guessing() {
    let _ = crate::observations::capture_timings(true, || {
        let (original, transport) = retained_context_fixture();
        let successor = original.successor(Arc::new(ContextScopeAuthority)).unwrap();
        successor.prepare_workspace().unwrap();
        transport.observe("after-unbound");
        assert_eq!(*transport.events.lock().unwrap(), [
            ("helper", None),
            ("after-unbound", Some("agent_run.lifecycle:context-A".into())),
        ], "an unbound Run must not inherit the physical creator's Job or leak its diagnostic barrier");
    });
}

#[test]
fn managed_profile_selects_owned_workspace_and_denies_private_backing_paths() {
    let fixture = Arc::new(FixtureTransport::new());
    let mut profile = profile();
    let EnvironmentProfile::E2b(value) = &mut profile else {
        unreachable!()
    };
    value.workspace_initialization =
        Some(crate::agent_run_authorization::WorkspaceInitialization::ManagedFiles);
    let mut binding = binding();
    binding
        .metadata
        .insert("profileDigest".into(), profile.digest().unwrap());
    fixture.inspect.lock().unwrap().binding = binding.clone();
    let host = E2bExecutionHostRunner::new(
        profile,
        binding,
        fixture.clone(),
        Arc::new(FixtureAuthority::new()),
    )
    .unwrap();
    host.prepare_workspace().unwrap();
    assert_eq!(
        *fixture.helper_kinds.lock().unwrap(),
        vec![TrustedHelperKind::PrepareManagedWorkspace]
    );
    let policy = host.execution_policy();
    assert!(policy
        .filesystem
        .denied_read_paths
        .contains(&"/var/lib/centaeris-host".into()));
    assert!(policy
        .filesystem
        .denied_write_paths
        .contains(&"/var/lib/centaeris-host".into()));
}
