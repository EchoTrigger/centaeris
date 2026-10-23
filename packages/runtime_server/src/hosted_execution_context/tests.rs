use super::*;
use centaeris_core::execution::*;
use centaeris_core::extension::build_plugin_activation_snapshot;
use hosted_execution::protocol::SandboxArtifactMetadata;
use std::sync::Mutex;

struct Runner(Mutex<Vec<ExecutionPolicy>>);
impl ExecutionHostRunner for Runner {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::RemoteHost
    }
    fn status(&self, policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        self.0.lock().unwrap().push(policy.clone());
        Ok(ExecutionHostStatus {
            kind: ExecutionHostKind::RemoteHost,
            policy_enforced: true,
            health: ExecutionHostHealth::Degraded,
            detail: Some("fixture-existing-runner-status".into()),
        })
    }
    fn run_file_system_operation(
        &self,
        _: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::PermissionDenied,
            "fixture-no-files",
        ))
    }
    fn run_host_command(
        &self,
        _: Option<&str>,
        _: ExecutionCommandRequest,
        _: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        Err(ExecutionError::HostUnavailable {
            reason: "fixture-no-command".into(),
        })
    }
}
struct Artifacts(Mutex<Vec<String>>);
impl ArtifactReadPort for Artifacts {
    fn read_artifact(&self, path: &str) -> Result<(SandboxArtifactMetadata, Vec<u8>), String> {
        self.0.lock().unwrap().push(path.into());
        Ok((
            SandboxArtifactMetadata {
                filename: "result.bin".into(),
                size_bytes: 3,
                sha256: "fixture-digest".into(),
            },
            vec![0, 255, 65],
        ))
    }
}
struct Hooks;
impl LifecycleHookRunner for Hooks {
    fn run_hook(
        &self,
        _: &LifecycleHookHandlerV1,
        _: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1 {
        LifecycleHookCommandResultV1 {
            exit_code: Some(7),
            stdout: "fixture-hook".into(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            spawn_error: None,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
struct Call {
    operation: &'static str,
    job_id: String,
    lease_owner: String,
    frozen: SessionWorkspace,
    inputs: Option<u64>,
}
struct Workspace {
    calls: Mutex<Vec<Call>>,
    resolve: Mutex<Result<SessionWorkspaceResolution, SessionWorkspaceApiError>>,
    restore: Mutex<Result<SessionWorkspaceResolution, SessionWorkspaceApiError>>,
    commit: Mutex<Result<SessionWorkspaceCommitOutcome, String>>,
}
impl Workspace {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            resolve: Mutex::new(Ok(SessionWorkspaceResolution::Empty)),
            restore: Mutex::new(Ok(SessionWorkspaceResolution::Download)),
            commit: Mutex::new(Ok(SessionWorkspaceCommitOutcome::Accepted)),
        })
    }
    fn record(
        &self,
        operation: &'static str,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: Option<u64>,
    ) {
        self.calls.lock().unwrap().push(Call {
            operation,
            job_id: lease.job_id.clone(),
            lease_owner: lease.lease_owner.clone(),
            frozen: frozen.clone(),
            inputs,
        });
    }
}
impl SessionWorkspacePort for Workspace {
    fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.record("resolve", lease, frozen, None);
        self.resolve.lock().unwrap().clone()
    }
    fn restore(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.record("restore", lease, frozen, Some(inputs));
        self.restore.lock().unwrap().clone()
    }
    fn collect_and_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        self.record("commit", lease, frozen, Some(inputs));
        self.commit.lock().unwrap().clone()
    }
}
fn context(
    workspace: Arc<Workspace>,
    runner: Arc<Runner>,
    artifacts: Arc<Artifacts>,
    hooks: Arc<Hooks>,
) -> HostedExecutionContext {
    HostedExecutionContext {
        runner,
        artifact_reader: artifacts,
        hook_runner: hooks,
        workspace,
        execution_id: "execution-frozen".into(),
        workspace_root: "/mnt/data".into(),
        policy: ExecutionPolicy::workspace_write_no_network("/mnt/data"),
        legacy: None,
    }
}
fn fixture(workspace: Arc<Workspace>) -> HostedExecutionContext {
    context(
        workspace,
        Arc::new(Runner(Mutex::new(Vec::new()))),
        Arc::new(Artifacts(Mutex::new(Vec::new()))),
        Arc::new(Hooks),
    )
}
fn request() -> (SessionWorkspaceLease, SessionWorkspace) {
    (
        SessionWorkspaceLease {
            job_id: "job-frozen".into(),
            lease_owner: "owner-current".into(),
        },
        SessionWorkspace {
            generation: 3,
            snapshot_sha256: format!("sha256:{}", "f".repeat(64)),
            snapshot_size_bytes: 11,
            expanded_size_bytes: 5,
            file_count: 1,
        },
    )
}
fn hook_request() -> (LifecycleHookHandlerV1, LifecycleHookEventV1) {
    use serde_json::json;
    (serde_json::from_value(json!({"id":"fixture","event":"SessionStart","source":{"kind":"Plugin","name":"fixture"},"trusted":true,"program":"must-not-execute","args":[]})).unwrap(),
     serde_json::from_value(json!({"schema":"lifecycle_hook_event_v1","event":"SessionStart","sessionId":"session-frozen"})).unwrap())
}

#[test]
fn existing_runner_artifact_and_hook_ports_are_preserved_without_core_wrapper() {
    let runner = Arc::new(Runner(Mutex::new(Vec::new())));
    let artifacts = Arc::new(Artifacts(Mutex::new(Vec::new())));
    let hooks = Arc::new(Hooks);
    let execution = context(
        Workspace::new(),
        runner.clone(),
        artifacts.clone(),
        hooks.clone(),
    );
    let runner_port: Arc<dyn ExecutionHostRunner> = runner.clone();
    let artifact_port: Arc<dyn ArtifactReadPort> = artifacts.clone();
    let hook_port: Arc<dyn LifecycleHookRunner + Send + Sync> = hooks;
    assert!(Arc::ptr_eq(&execution.runner(), &runner_port));
    assert!(Arc::ptr_eq(&execution.artifact_reader(), &artifact_port));
    assert!(Arc::ptr_eq(&execution.hook_runner(), &hook_port));
    let status = execution
        .runner()
        .status(&execution.execution_policy())
        .unwrap();
    assert_eq!(status.health, ExecutionHostHealth::Degraded);
    assert_eq!(
        status.detail.as_deref(),
        Some("fixture-existing-runner-status")
    );
    assert_eq!(
        *runner.0.lock().unwrap(),
        vec![execution.execution_policy()]
    );
    assert!(matches!(
        execution.runner().workspace_generation(),
        ExecutionWorkspaceGeneration::Unknown { .. }
    ));
    let (metadata, bytes) = execution
        .artifact_reader()
        .read_artifact("/mnt/data/result.bin")
        .unwrap();
    assert_eq!(metadata.filename, "result.bin");
    assert_eq!(bytes, vec![0, 255, 65]);
    assert_eq!(*artifacts.0.lock().unwrap(), vec!["/mnt/data/result.bin"]);
    let (handler, event) = hook_request();
    assert_eq!(
        execution.hook_runner().run_hook(&handler, &event).exit_code,
        Some(7)
    );
    assert_eq!(execution.execution_id(), "execution-frozen");
    assert_eq!(execution.workspace_root(), "/mnt/data");
    assert!(execution.legacy().is_none());
}

#[test]
fn workspace_dispatch_preserves_exact_lease_descriptor_budget_and_result() {
    let workspace = Workspace::new();
    let execution = fixture(workspace.clone());
    let (lease, frozen) = request();
    assert_eq!(
        execution
            .resolve_session_workspace(&lease, &frozen)
            .unwrap(),
        SessionWorkspaceResolution::Empty
    );
    assert_eq!(
        execution
            .restore_session_workspace(&lease, &frozen, 123)
            .unwrap(),
        SessionWorkspaceResolution::Download
    );
    assert_eq!(
        execution
            .collect_and_commit_session_workspace(&lease, &frozen, 456)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Accepted
    );
    assert_eq!(
        *workspace.calls.lock().unwrap(),
        vec![
            Call {
                operation: "resolve",
                job_id: lease.job_id.clone(),
                lease_owner: lease.lease_owner.clone(),
                frozen: frozen.clone(),
                inputs: None
            },
            Call {
                operation: "restore",
                job_id: lease.job_id.clone(),
                lease_owner: lease.lease_owner.clone(),
                frozen: frozen.clone(),
                inputs: Some(123)
            },
            Call {
                operation: "commit",
                job_id: lease.job_id,
                lease_owner: lease.lease_owner,
                frozen,
                inputs: Some(456)
            },
        ]
    );
}

#[test]
fn workspace_uncertainty_and_rejection_are_not_converted_to_completion() {
    let workspace = Workspace::new();
    let execution = fixture(workspace.clone());
    let (lease, frozen) = request();
    *workspace.resolve.lock().unwrap() = Err(SessionWorkspaceApiError::Unavailable(
        "unavailable-original".into(),
    ));
    *workspace.restore.lock().unwrap() = Err(SessionWorkspaceApiError::Rejected(
        "rejected-original".into(),
    ));
    assert_eq!(
        execution.resolve_session_workspace(&lease, &frozen),
        Err(SessionWorkspaceApiError::Unavailable(
            "unavailable-original".into()
        ))
    );
    assert_eq!(
        execution.restore_session_workspace(&lease, &frozen, 0),
        Err(SessionWorkspaceApiError::Rejected(
            "rejected-original".into()
        ))
    );
    for result in [
        Ok(SessionWorkspaceCommitOutcome::Pending),
        Ok(SessionWorkspaceCommitOutcome::ProtectionDeferred),
        Ok(SessionWorkspaceCommitOutcome::Unchanged),
        Ok(SessionWorkspaceCommitOutcome::Rejected(
            "commit-rejected-original".into(),
        )),
        Err("commit-error-original".into()),
    ] {
        *workspace.commit.lock().unwrap() = result.clone();
        assert_eq!(
            execution.collect_and_commit_session_workspace(&lease, &frozen, 0),
            result
        );
    }
}

#[test]
fn e2b_hook_preflight_rejects_declared_hooks_and_invalid_activation() {
    let empty = build_plugin_activation_snapshot(&[]).unwrap();
    HostedExecutionContext::require_no_lifecycle_hooks(&empty).unwrap();
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join(".centaeris-plugin")).unwrap();
    std::fs::write(
        temp.path().join(".centaeris-plugin/plugin.json"),
        r#"{"name":"fixture","version":"1.0.0","paths":{"hooks":["hooks.json"]}}"#,
    )
    .unwrap();
    std::fs::write(
        temp.path().join("hooks.json"),
        r#"{"schema":"plugin_hooks_v1","handlers":[]}"#,
    )
    .unwrap();
    let activation = build_plugin_activation_snapshot(&[temp.path().to_path_buf()]).unwrap();
    assert_eq!(
        HostedExecutionContext::require_no_lifecycle_hooks(&activation),
        Err("e2b_lifecycle_hooks_unsupported".into())
    );
    let mut invalid = empty;
    invalid.schema = "unknown-activation".into();
    assert!(HostedExecutionContext::require_no_lifecycle_hooks(&invalid).is_err());
}

#[test]
fn unavailable_e2b_hook_runner_never_fabricates_success() {
    let (handler, event) = hook_request();
    let result = UnsupportedLifecycleHooks.run_hook(&handler, &event);
    assert_eq!(result.exit_code, None);
    assert_eq!(
        result.spawn_error.as_deref(),
        Some("e2b_lifecycle_hooks_unsupported")
    );
    assert!(result.stdout.is_empty());
    assert!(result.stderr.is_empty());
}

#[test]
fn e2b_context_uses_successor_logical_execution_without_rewriting_provider_origin() {
    let host = Arc::new(crate::e2b_execution_host::tests::fixture_host());
    let original_execution = host.execution_id().to_owned();
    let client = reqwest::blocking::Client::new();
    let workspace = Arc::new(
        E2bSessionWorkspace::new(
            crate::e2b_execution_host::tests::profile(),
            crate::session_workspace::SessionWorkspaceApi {
                client: &client,
                api_url: "http://127.0.0.1:1",
                api_token: "fixture-only",
                agent_run_id: "run-successor",
                authorization_digest: "fixture-authorization",
            },
            host.clone(),
        )
        .unwrap(),
    );
    let activation = build_plugin_activation_snapshot(&[]).unwrap();
    let execution =
        HostedExecutionContext::e2b(host.clone(), workspace, "execution-successor", &activation)
            .unwrap();
    assert_eq!(execution.execution_id(), "execution-successor");
    assert_eq!(host.execution_id(), original_execution);
}
