use super::*;
use crate::hosted_execution_context::HostedExecutionPorts;
use crate::session_workspace::SESSION_WORKSPACE_RESOLVED_SCHEMA;
use centaeris_core::execution::*;
#[cfg(target_os = "linux")]
use centaeris_core::extension::skills::SkillCatalogLoadConfig;
#[cfg(target_os = "linux")]
use centaeris_core::model::{
    EmptyModelSessionConfigStore, GenerateResult, ModelClient, ModelClientFuture,
    ModelClientRequest, ModelClientResponse, ToolCallEnvelope,
};
#[cfg(target_os = "linux")]
use centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1;
#[cfg(target_os = "linux")]
use centaeris_core::runtime::{
    AgentRunInitialInput, AgentRunRequest, AgentRunResult, AgentRunStop, AgentRuntime,
    AgentRuntimeConfig, ToolConcurrencyCoordinator, ToolSafePoint,
};
#[cfg(target_os = "linux")]
use centaeris_core::tool::layer::{ToolExecutionResult, ToolLayer};
#[cfg(target_os = "linux")]
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

#[derive(Default)]
struct LivePorts {
    calls: Mutex<Vec<String>>,
    files: Mutex<Vec<ExecutionFileSystemRequest>>,
}
impl ExecutionHostRunner for LivePorts {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::RemoteHost
    }
    fn status(&self, _: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        self.calls.lock().unwrap().push("status".into());
        Ok(ExecutionHostStatus::remote(
            true,
            ExecutionHostHealth::Ready,
            Some("live-ready".into()),
        ))
    }
    fn workspace_generation(&self) -> ExecutionWorkspaceGeneration {
        ExecutionWorkspaceGeneration::Known {
            token: ExecutionWorkspaceGenerationV1 {
                instance_epoch: "live-host".into(),
                generation: 9,
            },
        }
    }
    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        self.files.lock().unwrap().push(request);
        Ok(ExecutionFileSystemOutput::ReadFile(
            ExecutionFileReadOutput {
                identity: ExecutionFileIdentity {
                    key: "live-file".into(),
                    display_path: "AGENTS.md".into(),
                },
                bytes: b"live instructions".to_vec(),
                file_hash: format!("sha256:{:x}", Sha256::digest(b"live instructions")),
            },
        ))
    }
    fn recover_host_command(
        &self,
        operation_id: &str,
        _: ExecutionCommandRequest,
        _: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandRecovery, ExecutionError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("recover:{operation_id}"));
        Err(ExecutionError::Denied {
            reason: "live-recovery-rejected".into(),
        })
    }

    fn run_host_command(
        &self,
        id: Option<&str>,
        _: ExecutionCommandRequest,
        _: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("command:{}", id.unwrap_or_default()));
        Err(ExecutionError::Denied {
            reason: "live-command-rejected".into(),
        })
    }
}
impl ArtifactReadPort for LivePorts {
    fn read_artifact(&self, path: &str) -> Result<(SandboxArtifactMetadata, Vec<u8>), String> {
        self.calls.lock().unwrap().push(format!("artifact:{path}"));
        Err("live-artifact-rejected".into())
    }
}
impl SessionWorkspacePort for LivePorts {
    fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("resolve:{}:{}", lease.job_id, frozen.generation));
        Ok(SessionWorkspaceResolution::RetainedLive)
    }
    fn restore(
        &self,
        _: &SessionWorkspaceLease,
        _: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.calls.lock().unwrap().push(format!("restore:{inputs}"));
        Ok(SessionWorkspaceResolution::Download)
    }
    fn collect_and_commit(
        &self,
        _: &SessionWorkspaceLease,
        _: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        self.calls.lock().unwrap().push(format!("collect:{inputs}"));
        Ok(SessionWorkspaceCommitOutcome::ProtectionDeferred)
    }
}
fn ready(ports: Arc<LivePorts>) -> HostedExecutionContext {
    HostedExecutionContext::e2b_ports(
        HostedExecutionPorts {
            runner: ports.clone(),
            artifact_reader: ports.clone(),
            workspace: ports,
        },
        "logical-execution".into(),
        policy(),
    )
}
fn policy() -> ExecutionPolicy {
    ExecutionPolicy::workspace_write_no_network("/mnt/data")
}
fn empty() -> SessionWorkspace {
    SessionWorkspace {
        generation: 0,
        snapshot_sha256: String::new(),
        snapshot_size_bytes: 0,
        expanded_size_bytes: 0,
        file_count: 0,
    }
}
fn lease() -> SessionWorkspaceLease {
    SessionWorkspaceLease {
        job_id: "job-owned".into(),
        lease_owner: "lease-owned".into(),
    }
}
fn binding(url: &str) -> DeferredWorkspaceBinding {
    DeferredWorkspaceBinding {
        client: reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap(),
        api_url: url.into(),
        api_token: "synthetic-token".into(),
        agent_run_id: "run-owned".into(),
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
        lease: lease(),
        frozen: empty(),
    }
}
fn uncancelled() -> Arc<ExecutionCancellationProbe> {
    Arc::new(|| Ok(None))
}
fn request() -> ExecutionFileSystemRequest {
    ExecutionFileSystemRequest {
        operation_id: Some("read-owned".into()),
        cwd: "/mnt/data".into(),
        policy: policy(),
        model_path: "AGENTS.md".into(),
        operation: ExecutionFileSystemOperation::ReadFile { max_bytes: 128 },
    }
}
fn command() -> ExecutionCommandRequest {
    ExecutionCommandRequest {
        program: "bash".into(),
        args: vec!["-c".into(), "true".into()],
        cwd: "/mnt/data".into(),
        env: Default::default(),
        timeout_ms: 1000,
        policy: policy(),
    }
}
fn resolved(disposition: &str) -> Value {
    let mut frozen = empty();
    if disposition == "advanced" {
        frozen.generation = 1;
    }
    json!({"schema":SESSION_WORKSPACE_RESOLVED_SCHEMA,"disposition":disposition,"sessionWorkspace":frozen})
}
struct Api {
    url: String,
    requests: Receiver<Value>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Api {
    fn new(replies: Vec<(u16, Value)>) -> Self {
        Self::with_gate(replies, None)
    }
    fn with_gate(replies: Vec<(u16, Value)>, gate: Option<Receiver<()>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (tx, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut gate = gate;
            let mut replies = replies.into_iter();
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture listener failed: {error}"),
                };
                let Some((status, body)) = replies.next() else {
                    panic!("unexpected workspace request");
                };
                socket
                    .set_read_timeout(Some(Duration::from_millis(500)))
                    .unwrap();
                let request = read_request(&mut socket);
                tx.send(request).unwrap();
                if let Some(receiver) = gate.take() {
                    receiver.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                let bytes = serde_json::to_vec(&body).unwrap();
                write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
                socket.write_all(&bytes).unwrap();
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(worker),
        }
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.thread.take() {
            worker.join().unwrap();
        }
    }
}
fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut one = [0; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        socket.read_exact(&mut one).unwrap();
        bytes.push(one[0]);
    }
    let headers = String::from_utf8(bytes).unwrap();
    assert!(headers.starts_with("POST /internal/agent-runs/session-workspace/resolve HTTP/1.1\r\n"));
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .map(str::to_owned)
        })
        .unwrap()
        .parse()
        .unwrap();
    let mut body = vec![0; length];
    socket.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn deferred_execution_empty_workspace_finishes_without_factory_or_snapshot_mutation() {
    let api = Api::new(vec![(200, resolved("empty")); 3]);
    let creates = Arc::new(AtomicUsize::new(0));
    let count = creates.clone();
    let proxy = DeferredE2bExecution::new(
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            Err("unexpected factory".into())
        },
        binding(&api.url),
        uncancelled(),
    );
    assert!(matches!(
        proxy
            .recover_host_command("original-command", command(), None)
            .unwrap(),
        ExecutionHostCommandRecovery::Unknown { .. }
    ));
    assert_eq!(proxy.kind(), ExecutionHostKind::RemoteHost);
    assert!(matches!(
        proxy.workspace_generation(),
        ExecutionWorkspaceGeneration::Unknown { .. }
    ));
    assert_eq!(
        proxy.resolve(&lease(), &empty()).unwrap(),
        SessionWorkspaceResolution::Empty
    );
    assert_eq!(
        proxy.restore(&lease(), &empty(), 0).unwrap(),
        SessionWorkspaceResolution::Empty
    );
    assert_eq!(
        proxy.collect_and_commit(&lease(), &empty(), 0).unwrap(),
        SessionWorkspaceCommitOutcome::Unchanged
    );
    for _ in 0..3 {
        assert_eq!(
            api.requests.recv_timeout(Duration::from_secs(2)).unwrap(),
            json!({"schema":"runtime.session_workspace.resolve.v1","jobId":"job-owned","leaseOwner":"lease-owned","agentRunId":"run-owned","authorizationDigest":format!("sha256:{}","a".repeat(64))})
        );
    }
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}
#[test]
fn deferred_execution_ready_ports_preserve_requests_results_and_live_instructions() {
    let ports = Arc::new(LivePorts::default());
    let original = ports.clone();
    let creates = Arc::new(AtomicUsize::new(0));
    let count = creates.clone();
    let proxy = DeferredE2bExecution::new(
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(ready(original))
        },
        binding("http://127.0.0.1:1"),
        uncancelled(),
    );
    assert_eq!(
        proxy.status(&policy()).unwrap().detail.as_deref(),
        Some("live-ready")
    );
    assert!(matches!(
        proxy.workspace_generation(),
        ExecutionWorkspaceGeneration::Known { .. }
    ));
    assert!(matches!(
        proxy.run_file_system_operation(request()).unwrap(),
        ExecutionFileSystemOutput::ReadFile(_)
    ));
    assert_eq!(
        proxy
            .run_host_command(Some("command-owned"), command(), None)
            .err()
            .unwrap(),
        ExecutionError::Denied {
            reason: "live-command-rejected".into()
        }
    );
    assert_eq!(
        proxy.read_artifact("out.bin").err().unwrap(),
        "live-artifact-rejected"
    );
    assert_eq!(
        proxy
            .recover_host_command("original-command", command(), None)
            .err()
            .unwrap(),
        ExecutionError::Denied {
            reason: "live-recovery-rejected".into()
        }
    );

    assert_eq!(
        proxy.resolve(&lease(), &empty()).unwrap(),
        SessionWorkspaceResolution::RetainedLive
    );
    assert_eq!(
        proxy.restore(&lease(), &empty(), 0).unwrap(),
        SessionWorkspaceResolution::Download
    );
    assert_eq!(
        proxy.collect_and_commit(&lease(), &empty(), 0).unwrap(),
        SessionWorkspaceCommitOutcome::ProtectionDeferred
    );
    let requests = ports.files.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].operation_id.as_deref(), Some("read-owned"));
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}
#[test]
fn deferred_execution_concurrent_first_demand_is_singleflight() {
    let ports = Arc::new(LivePorts::default());
    let creates = Arc::new(AtomicUsize::new(0));
    let count = creates.clone();
    let (entered, started) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let proxy = Arc::new(DeferredE2bExecution::new(
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(ready(ports))
        },
        binding("http://127.0.0.1:1"),
        uncancelled(),
    ));
    let workers: Vec<_> = (0..6)
        .map(|_| {
            let proxy = proxy.clone();
            thread::spawn(move || proxy.ensure_ready())
        })
        .collect();
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(creates.load(Ordering::SeqCst), 1);
    let recovering = proxy.clone();
    let recovering =
        thread::spawn(move || recovering.recover_host_command("original-command", command(), None));
    release.send(()).unwrap();
    assert_eq!(
        recovering.join().unwrap().err().unwrap(),
        ExecutionError::Denied {
            reason: "live-recovery-rejected".into()
        }
    );
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().expect("prepared"))
        .collect();
    assert!(results.iter().all(|value| Arc::ptr_eq(value, &results[0])));
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}
#[test]
fn deferred_execution_failed_or_panicking_factory_is_latched_without_retries() {
    for panics in [false, true] {
        let creates = Arc::new(AtomicUsize::new(0));
        let count = creates.clone();
        let proxy = Arc::new(DeferredE2bExecution::new(
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                assert!(!panics, "synthetic factory panic");
                Err("e2b_creation_unknown".into())
            },
            binding("http://127.0.0.1:1"),
            uncancelled(),
        ));
        let first = proxy.ensure_ready().err().expect("failure");
        let (tx, rx) = mpsc::channel();
        let later = proxy.clone();
        thread::spawn(move || {
            tx.send(later.ensure_ready().err().expect("latched failure"))
                .unwrap()
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), first);
        if !panics {
            assert_eq!(first, "e2b_creation_unknown");
        }
        assert_eq!(creates.load(Ordering::SeqCst), 1);
    }
}
#[test]
fn deferred_execution_cancelled_waiter_does_not_wait_for_or_repeat_factory() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let current = cancelled.clone();
    let probe: Arc<ExecutionCancellationProbe> =
        Arc::new(move || Ok(current.load(Ordering::SeqCst).then(|| "cancelled".into())));
    let ports = Arc::new(LivePorts::default());
    let creates = Arc::new(AtomicUsize::new(0));
    let count = creates.clone();
    let (entered, started) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let proxy = Arc::new(DeferredE2bExecution::new(
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            entered.send(()).unwrap();
            gate.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(ready(ports))
        },
        binding("http://127.0.0.1:1"),
        probe,
    ));
    let owner = proxy.clone();
    let owner = thread::spawn(move || owner.ensure_ready());
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    cancelled.store(true, Ordering::SeqCst);
    let waiter = proxy.clone();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(waiter.ensure_ready().is_err()).unwrap());
    assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
    release.send(()).unwrap();
    assert!(owner.join().unwrap().is_err());
    cancelled.store(false, Ordering::SeqCst);
    assert!(
        proxy.ensure_ready().is_ok(),
        "prepared context remains owned after cancellation"
    );
    assert_eq!(creates.load(Ordering::SeqCst), 1);
}
#[test]
fn deferred_execution_scope_and_nonempty_inputs_fail_before_http_or_creation() {
    let creates = Arc::new(AtomicUsize::new(0));
    let count = creates.clone();
    let proxy = DeferredE2bExecution::new(
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            Err("unexpected factory".into())
        },
        binding("http://127.0.0.1:1"),
        uncancelled(),
    );
    let wrong = SessionWorkspaceLease {
        lease_owner: "foreign".into(),
        ..lease()
    };
    assert!(matches!(
        proxy.resolve(&wrong, &empty()),
        Err(SessionWorkspaceApiError::Rejected(_))
    ));
    assert!(matches!(
        proxy.restore(&lease(), &empty(), 1),
        Err(SessionWorkspaceApiError::Rejected(_))
    ));
    assert!(matches!(
        proxy.collect_and_commit(&lease(), &empty(), 1).unwrap(),
        SessionWorkspaceCommitOutcome::Rejected(_)
    ));
    let changed = SessionWorkspace {
        generation: 1,
        ..empty()
    };
    assert!(matches!(
        proxy.resolve(&lease(), &changed),
        Err(SessionWorkspaceApiError::Rejected(_))
    ));
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}
#[test]
fn deferred_execution_unavailable_or_advanced_workspace_never_becomes_unchanged() {
    for (status, body, expected) in [
        (
            503,
            json!({"error":"unavailable"}),
            SessionWorkspaceCommitOutcome::Pending,
        ),
        (
            200,
            resolved("advanced"),
            SessionWorkspaceCommitOutcome::Rejected("e2b_deferred_workspace_not_empty".into()),
        ),
    ] {
        let api = Api::new(vec![(status, body)]);
        let proxy = DeferredE2bExecution::new(
            || Err("unexpected factory".into()),
            binding(&api.url),
            uncancelled(),
        );
        assert_eq!(
            proxy.collect_and_commit(&lease(), &empty(), 0).unwrap(),
            expected
        );
    }
}
#[test]
fn deferred_execution_empty_workspace_check_excludes_concurrent_materialization() {
    let (release, response_gate) = mpsc::channel();
    let api = Api::with_gate(vec![(200, resolved("empty"))], Some(response_gate));
    let ports = Arc::new(LivePorts::default());
    let (created, creation) = mpsc::channel();
    let proxy = Arc::new(DeferredE2bExecution::new(
        move || {
            created.send(()).unwrap();
            Ok(ready(ports))
        },
        binding(&api.url),
        uncancelled(),
    ));
    let collector = proxy.clone();
    let collector = thread::spawn(move || collector.collect_and_commit(&lease(), &empty(), 0));
    api.requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let demand = proxy.clone();
    let demand = thread::spawn(move || demand.ensure_ready());
    assert!(
        creation.recv_timeout(Duration::from_millis(100)).is_err(),
        "factory raced the untouched-workspace check"
    );
    release.send(()).unwrap();
    assert_eq!(
        collector.join().unwrap().unwrap(),
        SessionWorkspaceCommitOutcome::Unchanged
    );
    assert!(demand.join().unwrap().is_ok());
    creation.recv_timeout(Duration::from_secs(2)).unwrap();
}

#[cfg(target_os = "linux")]
struct ReadThenFinishModel {
    requests: Mutex<Vec<ModelClientRequest>>,
    creates: Arc<AtomicUsize>,
}
#[cfg(target_os = "linux")]
impl ModelClient for ReadThenFinishModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let mut requests = self.requests.lock().unwrap();
            let index = requests.len();
            assert!(index < 2, "the controlled model must not repeat its tool");
            if index == 0 {
                assert_eq!(self.creates.load(Ordering::SeqCst), 0);
                assert!(
                    request
                        .prepared_prompt
                        .messages
                        .iter()
                        .all(|message| !message.content.contains("live instructions")),
                    "the unread managed workspace instructions must not enter the first prompt"
                );
            } else {
                let created = self.creates.load(Ordering::SeqCst);
                assert!(created <= 1);
                let result =
                    request.prepared_prompt.messages.iter().find(|message| {
                        message.tool_call_id.as_deref() == Some("deferred-core-read")
                    });
                assert!(
                    result.is_some(),
                    "the dispatched read's outcome must enter the next model context"
                );
                let result = result.expect("checked");
                if created == 1 {
                    assert!(
                        result.content.contains("live instructions"),
                        "a materialized workspace returns the live instructions"
                    );
                } else {
                    assert!(
                        !result.content.contains("live instructions"),
                        "expired authority must never fabricate live workspace content"
                    );
                }
                assert_eq!(
                    requests[0].provider_prompt_cache_key,
                    request.provider_prompt_cache_key,
                    "an explicit AGENTS.md read must not invalidate the provider prompt cache identity"
                );
            }
            requests.push(request.clone());
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if index == 0 {
                        String::new()
                    } else {
                        "Read completed.".into()
                    },
                    tool_calls: if index == 0 {
                        vec![ToolCallEnvelope {
                            id: "deferred-core-read".into(),
                            name: "read".into(),
                            args_json: json!({"path":"AGENTS.md"}).to_string(),
                        }]
                    } else {
                        vec![]
                    },
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

#[cfg(target_os = "linux")]
fn deferred_tools(proxy: Arc<DeferredE2bExecution>) -> ToolLayer {
    let host = Arc::new(
        ExecutionHostBinding::new(
            ExecutionHostMode::Remote,
            proxy.clone(),
            "/mnt/data".into(),
            policy(),
        )
        .unwrap(),
    );
    ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        host,
    )
    .unwrap()
}

#[cfg(target_os = "linux")]
fn run_deferred_read_query(
    tools: ToolLayer,
    model: &ReadThenFinishModel,
    call_committed: &AtomicBool,
    receipts: &mut Vec<ToolExecutionResult>,
) -> Result<AgentRunResult, String> {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteRuntimeStore::new(directory.path().join("runtime.sqlite")).unwrap();
    let jobs = Arc::new(store.job_store(129).unwrap());
    let engine = AgentRuntime::new(
        jobs.clone(),
        store,
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
        ToolConcurrencyCoordinator::new(1),
    );
    let executor = tokio::runtime::Runtime::new().unwrap();
    let outcome = executor.block_on(
        engine.process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            AgentRunRequest {
                session_id: "deferred-core-session".into(),
                initial_turn_id: "deferred-core-turn".into(),
                initial_input: AgentRunInitialInput::UserMessage("Read AGENTS.md once.".into()),
                agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
                    agent_run_id: "run-owned".into(),
                    execution_id: "logical-execution".into(),
                    authorization_digest: format!("sha256:{}", "a".repeat(64)),
                }),
                runtime_scope: centaeris_core::model::prompt::PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            }, model, &EmptyModelSessionConfigStore::new(), &mut |_| {}, &|| Ok(None),
            &mut |point| {
                match point {
                    ToolSafePoint::DurableToolCall { call, .. } => {
                        assert_eq!(call.id, "deferred-core-read");
                        assert_eq!(call.name, "read");
                        assert!(!call_committed.swap(true, Ordering::SeqCst));
                        assert_eq!(model.creates.load(Ordering::SeqCst), 0,
                            "filesystem materialization must follow the Core durable-call boundary");
                    }
                    ToolSafePoint::DurableReceipt { result, .. } => receipts.push(result),
                    _ => {}
                }
                Ok(())
            },
        ),
    );
    executor.block_on(jobs.close()).unwrap();
    drop(engine);
    outcome
}

#[cfg(target_os = "linux")]
#[test]
fn deferred_execution_query_loop_materializes_only_after_durable_file_demand_and_reads_live_next() {
    let creates = Arc::new(AtomicUsize::new(0));
    let committed = Arc::new(AtomicBool::new(false));
    let ports = Arc::new(LivePorts::default());
    let factory_creates = creates.clone();
    let factory_committed = committed.clone();
    let live = ports.clone();
    let proxy = Arc::new(DeferredE2bExecution::new(
        move || {
            assert!(factory_committed.load(Ordering::SeqCst));
            factory_creates.fetch_add(1, Ordering::SeqCst);
            Ok(ready(live))
        },
        binding("http://127.0.0.1:1"),
        uncancelled(),
    ));
    let model = ReadThenFinishModel {
        requests: Mutex::new(Vec::new()),
        creates: creates.clone(),
    };
    let mut receipts = Vec::new();
    let result =
        run_deferred_read_query(deferred_tools(proxy), &model, &committed, &mut receipts).unwrap();
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(result.final_reply.as_deref(), Some("Read completed."));
    assert_eq!(creates.load(Ordering::SeqCst), 1);
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].status, "ok");
    assert!(receipts[0].content.contains("live instructions"));
    let files = ports.files.lock().unwrap();
    let tool_reads = files
        .iter()
        .filter(|request| request.operation_id.is_some())
        .collect::<Vec<_>>();
    assert_eq!(
        tool_reads.len(),
        1,
        "Core must dispatch the requested tool once"
    );
    assert_eq!(tool_reads[0].model_path, "AGENTS.md");
    assert!(
        files.iter().all(|request| request.operation_id.is_some()),
        "AGENTS.md may only be read through the durable tool boundary"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn deferred_execution_query_loop_expired_step_tool_layer_clone_cannot_create_or_retry() {
    let owner = Arc::new(());
    let weak_owner = Arc::downgrade(&owner);
    let creates = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let committed = Arc::new(AtomicBool::new(false));
    let factory_creates = creates.clone();
    let factory_attempts = attempts.clone();
    let factory_committed = committed.clone();
    let proxy = Arc::new(DeferredE2bExecution::new(
        move || {
            factory_attempts.fetch_add(1, Ordering::SeqCst);
            assert!(factory_committed.load(Ordering::SeqCst));
            let _owner = weak_owner.upgrade().ok_or("e2b_deferred_step_expired")?;
            factory_creates.fetch_add(1, Ordering::SeqCst);
            Ok(ready(Arc::new(LivePorts::default())))
        },
        binding("http://127.0.0.1:1"),
        uncancelled(),
    ));
    let original_tools = deferred_tools(proxy.clone());
    let stale_tools = original_tools.clone();
    drop(original_tools);
    drop(owner);
    let model = ReadThenFinishModel {
        requests: Mutex::new(Vec::new()),
        creates: creates.clone(),
    };
    let mut receipts = Vec::new();
    let result = run_deferred_read_query(stale_tools.clone(), &model, &committed, &mut receipts);
    let outcome = result.expect("the Run concludes honestly from the failed read receipt");
    assert_eq!(outcome.stop, AgentRunStop::Finalized);
    assert_eq!(outcome.final_reply.as_deref(), Some("Read completed."));
    assert!(committed.load(Ordering::SeqCst));
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert_eq!(receipts.len(), 1);
    assert_ne!(receipts[0].status, "ok");
    assert!(proxy.ensure_ready().is_err());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "expired authority is latched, never retried"
    );
    assert_eq!(
        creates.load(Ordering::SeqCst),
        0,
        "expired authority must never fabricate a live workspace"
    );
}
