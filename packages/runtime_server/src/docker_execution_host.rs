use crate::docker_engine::{ExecChild, ExecRequest};
use crate::execution_recovery_coordinator::OwnedExecutionContinuity;
use crate::execution_recovery_policy::{
    ExecutionActivityWitness, HostFailureEvidence, SnapshotActivityGuard, SnapshotCaptureError,
    WorkspaceReuseEvidence,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Cursor, Read, Seek, Write};
use std::path::Path;
#[cfg(test)]
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

static NEXT_EXECUTION_HOST_INSTANCE: AtomicU64 = AtomicU64::new(1);
use std::thread;
use std::time::{Duration, Instant};

use centaeris_core::execution::{
    classify_execution_host_failure, ExecutionCancellationProbe, ExecutionFileSystemError,
    ExecutionFileSystemErrorKind, ExecutionFileSystemOperation, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest, ExecutionHostCommandOutput, ExecutionHostFailureKind,
    ExecutionHostHealth, ExecutionHostRunner, ExecutionHostStatus, ExecutionInputState,
    ExecutionInputStateChange, ExecutionWorkspaceGeneration, MAX_PUBLISHED_ARTIFACT_BYTES,
    WORKSPACE_DATA_ROOT, WORKSPACE_HOME,
};
use centaeris_core::execution::{
    decode_process_output, ExecutionAttempt, ExecutionCommandRequest, ExecutionError,
    ExecutionPolicy, ExecutionPolicySummary, ExecutionProcessOutput, NetworkPolicy,
};
use centaeris_core::extension::hooks::{
    LifecycleHookCommandResultV1, LifecycleHookEventV1, LifecycleHookHandlerV1,
    LifecycleHookRunner, LifecycleHookSourceKindV1,
};
use centaeris_core::extension::{validate_plugin_activation_snapshot, PluginActivationSnapshotV1};
use centaeris_core::runtime::contracts::RecoveryWorkspaceSnapshotV1;
use hosted_execution::memory::{is_memory_uri, MemoryPath, MEMORY_CONTAINER_ROOT};
use hosted_execution::protocol::{
    SandboxArtifactMetadata, SandboxArtifactRequest, SandboxFileSystemRequest,
    SandboxFileSystemResult, SandboxInputInventory, SandboxInputRevokeRequest,
    SandboxMaterializedInput, SandboxWorkspaceGeneration, SandboxWorkspaceSnapshotFile,
    SandboxWorkspaceSnapshotManifest, SANDBOX_INPUT_INVENTORY_SCHEMA,
    SANDBOX_WORKSPACE_GENERATION_QUERY_LINE, SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::agent_run_authorization::SandboxResources;
use crate::agent_run_authorization::SessionWorkspace;

const VALIDATE_INPUTS_SCHEMA: &str = "runtime.projected_input.validate.v1";
const AGENT_BINARY: &str = "/opt/centaeris/bin/execution_agent";
const AGENT_USER: &str = "10001:10001";
const HELPER_JSON_LIMIT: usize = 1024 * 1024;
const WORKSPACE_GENERATION_RPC_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const DOCKER_DIAGNOSTIC_LIMIT: usize = 64 * 1024;
const PLUGIN_VOLUME_NAME_ENV: &str = "PLUGIN_VOLUME_NAME";
const PLUGIN_CONTAINER_ROOT: &str = "/opt/centaeris/plugins";
const SYSTEM_SKILL_CONTAINER_ROOT: &str = "/opt/centaeris/system-skills";
const AGENT_MEMORY_VOLUME_NAME_ENV: &str = "AGENT_MEMORY_VOLUME_NAME";
const WORKSPACE_GENERAL_IMAGE_ENV: &str = "WORKSPACE_GENERAL_IMAGE";
const MEMORY_RUNTIME_VOLUME_ROOT: &str = "/var/lib/centaeris-agent-memory";
const BASE_COMMAND_PATH: &str =
    "/opt/centaeris/bin:/opt/centaeris/venv/bin:/usr/local/bin:/usr/bin:/bin";
const SANDBOX_NETWORK_MODE: &str = "none";
const SESSION_WORKSPACE_RESOLVE_SCHEMA: &str = "runtime.session_workspace.resolve.v1";
const SESSION_WORKSPACE_RESOLVED_SCHEMA: &str = "runtime.session_workspace.resolved.v1";
const SESSION_WORKSPACE_DOWNLOAD_SCHEMA: &str = "runtime.session_workspace.download.v1";
const SESSION_WORKSPACE_COMMIT_SCHEMA: &str = "runtime.session_workspace.commit.v1";
const SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA: &str = "runtime.session_workspace.commit.result.v1";
const EXECUTION_WORKSPACE_STAGE_SCHEMA: &str = "runtime.execution_workspace.stage.v1";
const EXECUTION_WORKSPACE_STAGE_RESULT_SCHEMA: &str = "runtime.execution_workspace.stage.result.v1";
const EXECUTION_WORKSPACE_DOWNLOAD_SCHEMA: &str = "runtime.execution_workspace.download.v1";
const SESSION_WORKSPACE_MANIFEST_LIMIT: usize = 1024 * 1024;
const SESSION_WORKSPACE_IO_BUFFER_BYTES: usize = 64 * 1024;
const SESSION_WORKSPACE_RESTORE_OVERHEAD_BYTES: u64 = 64 * 1024;
const WORKSPACE_EXECUTION_SENTINEL: &str = "/run/centaeris/execution.json";

#[derive(Debug, Clone)]
pub(crate) struct SessionWorkspaceLease {
    pub job_id: String,
    pub lease_owner: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionWorkspaceResolution {
    Empty,
    Download,
    Advanced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionWorkspaceApiError {
    Rejected(String),
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionWorkspaceCommitOutcome {
    Accepted,
    Unchanged,
    Rejected(String),
    Pending,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentProcessState {
    active_process_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OciRuntime {
    Runc,
    Runsc,
}

impl OciRuntime {
    pub(crate) fn from_environment() -> Result<Self, String> {
        let raw = env::var("OCI_RUNTIME").map_err(|_| "OCI_RUNTIME is required".to_string())?;
        Self::parse(raw.as_str())
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "runc" => Ok(Self::Runc),
            "runsc" => Ok(Self::Runsc),
            _ => Err(format!(
                "OCI_RUNTIME must be exactly runc or runsc, got {value:?}"
            )),
        }
    }

    pub(crate) fn docker_runtime_name(self) -> &'static str {
        match self {
            Self::Runc => "runc",
            Self::Runsc => "runsc",
        }
    }

    pub(crate) fn transition_reason(self) -> &'static str {
        match self {
            Self::Runc => "docker_runc",
            Self::Runsc => "docker_runsc",
        }
    }
}

fn docker_volume_name_from_environment(name: &str) -> Result<String, String> {
    let value = env::var(name).map_err(|_| format!("{name} is required"))?;
    validate_docker_volume_name(name, value.as_str())?;
    Ok(value)
}

fn validate_docker_volume_name(name: &str, value: &str) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if !valid {
        return Err(format!("{name} is not a canonical Docker volume name"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginMount {
    package_name: String,
    destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryMount {
    volume_name: String,
    scope_key: String,
}

fn memory_mount(user_id: &str, agent_id: &str, volume_name: String) -> Result<MemoryMount, String> {
    if user_id.trim().is_empty() || agent_id.trim().is_empty() {
        return Err("Agent Memory binding is invalid".to_string());
    }
    let identity = serde_json::to_vec(&("agent_memory_scope_v1", user_id, agent_id))
        .map_err(|error| format!("encode Agent Memory scope failed: {error}"))?;
    Ok(MemoryMount {
        volume_name,
        scope_key: format!("memory-{:x}", Sha256::digest(identity)),
    })
}

fn prepare_memory_scope(mount: &MemoryMount) -> Result<(), String> {
    prepare_memory_scope_at(Path::new(MEMORY_RUNTIME_VOLUME_ROOT), mount)
}

fn prepare_memory_scope_at(volume_root: &Path, mount: &MemoryMount) -> Result<(), String> {
    let metadata = fs::symlink_metadata(volume_root)
        .map_err(|error| format!("Agent Memory volume is unavailable: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Agent Memory volume root is invalid".to_string());
    }
    let scope = volume_root.join(mount.scope_key.as_str());
    let topics = scope.join("topics");
    for path in [&scope, &topics] {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("create Agent Memory scope failed: {error}")),
        }
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("inspect Agent Memory scope failed: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Agent Memory scope contains an unsupported node".to_string());
        }
        set_private_directory_permissions(path.as_path())?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("protect Agent Memory scope failed: {error}"))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn plugin_mounts(activation: &PluginActivationSnapshotV1) -> Vec<PluginMount> {
    activation
        .packages
        .iter()
        .map(|package| PluginMount {
            package_name: package.name.clone(),
            destination: format!("{PLUGIN_CONTAINER_ROOT}/{}", package.name),
        })
        .collect()
}

fn plugin_command_path(activation: &PluginActivationSnapshotV1) -> Result<String, String> {
    let mut directories = Vec::new();
    let mut seen = HashSet::new();
    for package in &activation.packages {
        for cli in &package.cli {
            let parent = Path::new(cli.path.as_str())
                .parent()
                .and_then(Path::to_str)
                .ok_or_else(|| format!("plugin CLI path is invalid: {}", cli.path))?
                .replace('\\', "/");
            let directory = if parent.is_empty() {
                format!("{PLUGIN_CONTAINER_ROOT}/{}", package.name)
            } else {
                format!("{PLUGIN_CONTAINER_ROOT}/{}/{parent}", package.name)
            };
            if seen.insert(directory.clone()) {
                directories.push(directory);
            }
        }
    }
    directories.push(BASE_COMMAND_PATH.to_string());
    Ok(directories.join(":"))
}

pub struct DockerExecutionHostRunner {
    pub(crate) transcript_capture: Mutex<crate::transcript_capture::CaptureBuffer>,
    pub(crate) transcript_publisher: Arc<crate::transcript_capture::CapturePublisher>,
    recovery_activity: ExecutionActivityWitness,
    recovery_snapshot: Mutex<Option<WorkspaceReuseEvidence>>,
    pre_dispatch_loss: AtomicBool,
    deferred_extension_dispatch: AtomicBool,
    container_name: String,
    agent_run_id: String,
    execution_id: String,
    authorization_digest: String,
    image_digest: String,
    oci_runtime: OciRuntime,
    resources: SandboxResources,
    plugin_volume_name: String,
    plugin_mounts: Vec<PluginMount>,
    memory_mount: MemoryMount,
    command_path: String,
    api_url: String,
    api_token: String,
    api_client: reqwest::blocking::Client,
    input_lock: Mutex<()>,
    materialized_inputs: Mutex<BTreeMap<String, SandboxMaterializedInput>>,
    workspace_generation_rpc: Mutex<Option<WorkspaceGenerationRpc>>,
}

struct WorkspaceGenerationRpc {
    child: RpcChild,
    stdin: Option<Box<dyn Write + Send>>,
    stderr: Option<thread::JoinHandle<Result<BoundedRead, String>>>,
    responses: Option<mpsc::Receiver<Result<Vec<u8>, String>>>,
    reader: Option<thread::JoinHandle<()>>,
}

enum RpcChild {
    Engine(ExecChild),
    #[cfg(test)]
    Process(Child),
}
impl RpcChild {
    #[cfg(test)]
    fn id(&self) -> String {
        match self {
            Self::Process(child) => child.id().to_string(),
            Self::Engine(child) => child.id.clone(),
        }
    }
    fn try_wait(&mut self) -> Result<Option<String>, String> {
        match self {
            Self::Engine(child) => child
                .try_wait()
                .map(|status| status.map(|status| status.to_string())),
            #[cfg(test)]
            Self::Process(child) => child
                .try_wait()
                .map(|status| status.map(|status| status.to_string()))
                .map_err(|e| e.to_string()),
        }
    }
    fn disconnect(&mut self) {
        match self {
            Self::Engine(child) => child.disconnect(),
            #[cfg(test)]
            Self::Process(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl WorkspaceGenerationRpc {
    #[cfg(test)]
    fn spawn(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = Box::new(child.stdin.take().ok_or("RPC stdin missing")?);
        let stdout = Box::new(child.stdout.take().ok_or("RPC stdout missing")?);
        Self::from_streams(RpcChild::Process(child), stdin, stdout, None)
    }
    fn from_exec(mut child: ExecChild) -> Result<Self, String> {
        let stdin = Box::new(child.stdin.take().ok_or("RPC stdin missing")?);
        let stdout = Box::new(child.stdout.take().ok_or("RPC stdout missing")?);
        let stderr = child.stderr.take().ok_or("RPC stderr missing")?;
        let drain = thread::spawn(move || read_bounded(stderr, DOCKER_DIAGNOSTIC_LIMIT));
        Self::from_streams(RpcChild::Engine(child), stdin, stdout, Some(drain))
    }
    fn from_streams(
        child: RpcChild,
        stdin: Box<dyn Write + Send>,
        stdout: Box<dyn Read + Send>,
        stderr: Option<thread::JoinHandle<Result<BoundedRead, String>>>,
    ) -> Result<Self, String> {
        let (sender, responses) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let response = read_workspace_generation_response(&mut stdout);
                let failed = response.is_err();
                if sender.send(response).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stderr,
            stdin: Some(stdin),
            responses: Some(responses),
            reader: Some(reader),
        })
    }

    fn query(&mut self, timeout: Duration) -> Result<SandboxWorkspaceGeneration, String> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| format!("inspect workspace generation RPC failed: {error}"))?
        {
            return Err(format!("workspace generation RPC exited: {status}"));
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "workspace generation RPC stdin is closed".to_string())?;
        stdin
            .write_all(SANDBOX_WORKSPACE_GENERATION_QUERY_LINE)
            .and_then(|_| stdin.flush())
            .map_err(|error| format!("write workspace generation RPC failed: {error}"))?;
        let response = self
            .responses
            .as_ref()
            .ok_or_else(|| "workspace generation RPC response channel is closed".to_string())?
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    "workspace generation RPC response timed out".to_string()
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    "workspace generation RPC response channel disconnected".to_string()
                }
            })??;
        let generation = serde_json::from_slice::<SandboxWorkspaceGeneration>(response.as_slice())
            .map_err(|error| format!("decode workspace generation RPC failed: {error}"))?;
        generation.validate()?;
        Ok(generation)
    }
}

impl Drop for WorkspaceGenerationRpc {
    fn drop(&mut self) {
        self.responses.take();
        self.stdin.take();
        self.child.disconnect();
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_workspace_generation_response(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut response = Vec::new();
    reader
        .take(HELPER_JSON_LIMIT as u64 + 2)
        .read_until(b'\n', &mut response)
        .map_err(|error| format!("read workspace generation RPC failed: {error}"))?;
    if response.last() != Some(&b'\n') {
        return Err("workspace generation RPC response is incomplete".to_string());
    }
    response.pop();
    if response.len() > HELPER_JSON_LIMIT || response.last() == Some(&b'\r') {
        return Err("workspace generation RPC response is invalid".to_string());
    }
    Ok(response)
}

fn query_workspace_generation_rpc(
    slot: &mut Option<WorkspaceGenerationRpc>,
    timeout: Duration,
    start: impl FnOnce() -> Result<WorkspaceGenerationRpc, String>,
) -> Result<SandboxWorkspaceGeneration, String> {
    if slot.is_none() {
        *slot = Some(start()?);
    }
    let result = slot
        .as_mut()
        .expect("generation RPC initialized")
        .query(timeout);
    if result.is_err() {
        slot.take();
    }
    result
}

pub struct DockerExecutionHostRequest<'a> {
    pub agent_run_id: String,
    pub execution_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub authorization_digest: String,
    pub image_digest: String,
    pub resources: SandboxResources,
    pub has_execution_fact: bool,
    pub api_url: String,
    pub api_token: String,
    pub plugin_activation: &'a PluginActivationSnapshotV1,
}

struct ContainerExpectation<'a> {
    name: &'a str,
    agent_run_id: &'a str,
    execution_id: &'a str,
    authorization_digest: &'a str,
    image_digest: &'a str,
    oci_runtime: OciRuntime,
    resources: SandboxResources,
    plugin_volume_name: &'a str,
    plugin_mounts: &'a [PluginMount],
    memory_mount: &'a MemoryMount,
}

impl DockerExecutionHostRunner {
    pub(crate) fn recovery_continuity(&self) -> OwnedExecutionContinuity {
        match inspect_container(&self.container_name) {
            Ok(None) => OwnedExecutionContinuity::OwnedMissing,
            Ok(Some(facts)) if !facts.matches_configuration(&self.container_expectation()) => {
                OwnedExecutionContinuity::IdentityMismatch
            }
            Ok(Some(facts)) if !facts.running => OwnedExecutionContinuity::OwnedStopped,
            Ok(Some(_)) => OwnedExecutionContinuity::OwnedRunning,
            Err(_) => OwnedExecutionContinuity::InspectUnavailable,
        }
    }

    pub(crate) fn recovery_workspace_evidence(
        &self,
    ) -> Result<Option<WorkspaceReuseEvidence>, String> {
        let snapshot = self
            .recovery_snapshot
            .lock()
            .map_err(|_| "recovery snapshot lock poisoned")?;
        Ok(snapshot.as_ref().map(|snapshot| {
            let mut evidence = self.recovery_activity.workspace_evidence(snapshot);
            // An exec request can be dispatched after it was returned. Until that
            // lifecycle is explicitly tracked, never attest a quiesced MCP host.
            evidence.snapshot_was_quiesced &=
                !self.deferred_extension_dispatch.load(Ordering::SeqCst);
            evidence
        }))
    }

    pub(crate) fn recovery_failure_evidence(&self) -> HostFailureEvidence {
        if self.pre_dispatch_loss.load(Ordering::SeqCst) {
            HostFailureEvidence::PreDispatchProcessNotStarted
        } else {
            HostFailureEvidence::Unknown
        }
    }

    pub fn new(request: DockerExecutionHostRequest<'_>) -> Result<Self, String> {
        #[cfg(test)]
        if crate::native_consumption_tests::owns_identity(&request.user_id, &request.agent_id) {
            assert!(
                !request.has_execution_fact,
                "fresh execution required by native fixture"
            );
            validate_plugin_activation_snapshot(request.plugin_activation)?;
            assert!(plugin_mounts(request.plugin_activation).is_empty());
            crate::native_consumption_tests::register_owned_run(&request.agent_run_id);
            let mut runner = tests::test_docker_execution_host_runner();
            runner.container_name = format!("native-consume-test:{}", request.execution_id);
            runner.agent_run_id = request.agent_run_id;
            runner.execution_id = request.execution_id;
            runner.authorization_digest = request.authorization_digest;
            runner.image_digest = request.image_digest;
            runner.resources = request.resources;
            runner.plugin_mounts.clear();
            runner.command_path = "/usr/bin".into();
            runner.api_url = request.api_url;
            runner.api_token = request.api_token;
            return Ok(runner);
        }
        let DockerExecutionHostRequest {
            agent_run_id,
            execution_id,
            user_id,
            agent_id,
            authorization_digest,
            image_digest,
            resources,
            has_execution_fact,
            api_url,
            api_token,
            plugin_activation,
        } = request;
        if agent_run_id.trim().is_empty()
            || execution_id.trim().is_empty()
            || authorization_digest.trim().is_empty()
            || image_digest.trim().is_empty()
            || api_url.is_empty()
            || api_url.trim_end_matches('/') != api_url
            || api_token.trim().is_empty()
        {
            return Err("Docker sandbox binding is invalid".to_string());
        }
        validate_plugin_activation_snapshot(plugin_activation)?;
        let plugin_volume_name = docker_volume_name_from_environment(PLUGIN_VOLUME_NAME_ENV)?;
        let plugin_mounts = plugin_mounts(plugin_activation);
        let memory_mount = memory_mount(
            user_id.as_str(),
            agent_id.as_str(),
            docker_volume_name_from_environment(AGENT_MEMORY_VOLUME_NAME_ENV)?,
        )?;
        prepare_memory_scope(&memory_mount)?;
        let command_path = plugin_command_path(plugin_activation)?;
        let oci_runtime = OciRuntime::from_environment()?;
        let container_name = container_name(execution_id.as_str());
        ensure_container(
            &ContainerExpectation {
                name: container_name.as_str(),
                agent_run_id: agent_run_id.as_str(),
                execution_id: execution_id.as_str(),
                authorization_digest: authorization_digest.as_str(),
                image_digest: image_digest.as_str(),
                oci_runtime,
                resources,
                plugin_volume_name: plugin_volume_name.as_str(),
                plugin_mounts: plugin_mounts.as_slice(),
                memory_mount: &memory_mount,
            },
            has_execution_fact,
        )?;
        let runner = Self {
            transcript_capture: Mutex::new(Default::default()),
            transcript_publisher: Arc::new(Default::default()),
            recovery_activity: ExecutionActivityWitness::for_execution(
                NEXT_EXECUTION_HOST_INSTANCE.fetch_add(1, Ordering::Relaxed),
                &container_name,
            )?,
            recovery_snapshot: Mutex::new(None),
            pre_dispatch_loss: AtomicBool::new(false),
            deferred_extension_dispatch: AtomicBool::new(false),
            container_name,
            agent_run_id,
            execution_id,
            authorization_digest,
            image_digest,
            oci_runtime,
            resources,
            plugin_volume_name,
            plugin_mounts,
            memory_mount,
            command_path,
            api_url,
            api_token,
            api_client: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .build()
                .map_err(|error| format!("build Docker sandbox API client failed: {error}"))?,
            input_lock: Mutex::new(()),
            materialized_inputs: Mutex::new(BTreeMap::new()),
            workspace_generation_rpc: Mutex::new(None),
        };
        if has_execution_fact {
            runner.quiesce_existing_execution()?;
        }
        let inventory = runner.input_inventory()?;
        *runner
            .materialized_inputs
            .lock()
            .map_err(|_| "sandbox input registry lock poisoned".to_string())? = inventory
            .inputs
            .into_iter()
            .map(|input| (input.input_ref.clone(), input))
            .collect();
        Ok(runner)
    }

    pub(crate) fn execution_id(&self) -> &str {
        self.execution_id.as_str()
    }

    fn container_expectation(&self) -> ContainerExpectation<'_> {
        ContainerExpectation {
            name: self.container_name.as_str(),
            agent_run_id: self.agent_run_id.as_str(),
            execution_id: self.execution_id.as_str(),
            authorization_digest: self.authorization_digest.as_str(),
            image_digest: self.image_digest.as_str(),
            oci_runtime: self.oci_runtime,
            resources: self.resources,
            plugin_volume_name: self.plugin_volume_name.as_str(),
            plugin_mounts: self.plugin_mounts.as_slice(),
            memory_mount: &self.memory_mount,
        }
    }

    pub fn validate_host() -> Result<(), String> {
        docker_volume_name_from_environment(PLUGIN_VOLUME_NAME_ENV)?;
        docker_volume_name_from_environment(AGENT_MEMORY_VOLUME_NAME_ENV)?;
        let oci_runtime = OciRuntime::from_environment()?;
        let runtime_name = oci_runtime.docker_runtime_name();
        let info = crate::docker_engine::info()?;
        let runtimes = &info["Runtimes"];
        if runtimes.get(runtime_name).is_none() {
            return Err(format!(
                "configured OCI runtime is unavailable: {runtime_name}"
            ));
        }
        Ok(())
    }

    fn exec_request(&self, command: Vec<String>) -> ExecRequest {
        ExecRequest::owned(
            &self.container_name,
            &self.agent_run_id,
            &self.execution_id,
            command,
        )
    }

    pub(crate) fn mcp_exec_request(
        &self,
        plugin_name: &str,
        program: &str,
        program_args: &[String],
    ) -> Result<ExecRequest, String> {
        let _activity = self.recovery_activity.begin_dispatch_activity()?;
        let mount = self
            .plugin_mounts
            .iter()
            .find(|mount| mount.package_name == plugin_name)
            .ok_or_else(|| format!("MCP plugin is not activated: {plugin_name}"))?;
        let program = format!("{}/{program}", mount.destination);
        if !authorized_plugin_path(self.plugin_mounts.as_slice(), program.as_str())
            .map_err(|_| "validate MCP stdio program failed".to_string())?
        {
            return Err("MCP stdio program must belong to its activated plugin".to_string());
        }
        let path_env = format!("PATH={}", self.command_path);
        self.deferred_extension_dispatch
            .store(true, Ordering::SeqCst);
        _activity.register_persistent_dispatch()?;
        self.recovery_activity.invalidate_before_dispatch()?;
        let mut command = vec![program];
        command.extend_from_slice(program_args);
        let mut request = self.exec_request(command);
        request.user = Some(AGENT_USER.to_string());
        request.cwd = Some(WORKSPACE_DATA_ROOT.to_string());
        request.env = vec!["HOME=/home/agent".into(), path_env, "TMPDIR=/tmp".into()];
        Ok(request)
    }

    fn lifecycle_hook_exec_request(
        &self,
        handler: &LifecycleHookHandlerV1,
    ) -> Result<ExecRequest, String> {
        if handler.source.kind != LifecycleHookSourceKindV1::Plugin {
            return Err("Workspace lifecycle hook source must be Plugin".to_string());
        }
        let mount = self
            .plugin_mounts
            .iter()
            .find(|mount| mount.package_name == handler.source.name)
            .ok_or_else(|| {
                format!(
                    "lifecycle hook plugin is not activated: {}",
                    handler.source.name
                )
            })?;
        let mut request = self.exec_request(Vec::new());
        request.user = Some(AGENT_USER.to_string());
        request.cwd = Some(mount.destination.clone());
        for (name, value) in [
            ("LANG", "C.UTF-8"),
            ("LC_ALL", "C.UTF-8"),
            ("TERM", "dumb"),
            ("HOME", WORKSPACE_HOME),
            ("PATH", self.command_path.as_str()),
            ("TMPDIR", "/tmp"),
        ] {
            request.env.push(format!("{name}={value}"));
        }
        request.command.extend([
            "/usr/bin/timeout".to_string(),
            "--signal=TERM".to_string(),
            "--kill-after=1s".to_string(),
            coreutils_timeout_duration(handler.timeout_ms),
            handler.program.clone(),
        ]);
        request.command.extend(handler.args.clone());
        Ok(request)
    }

    fn run_lifecycle_hook_command(
        &self,
        handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> Result<LifecycleHookCommandResultV1, String> {
        let _activity = self.recovery_activity.begin_dispatch_activity()?;
        let mut stdin_json = serde_json::to_vec(event)
            .map_err(|error| format!("serialize lifecycle hook event failed: {error}"))?;
        stdin_json.push(b'\n');
        let request = self.lifecycle_hook_exec_request(handler)?;
        self.recovery_activity.invalidate_before_dispatch()?;
        let mut child = request.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "lifecycle hook stdout is unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "lifecycle hook stderr is unavailable".to_string())?;
        let stdout_reader = thread::spawn(move || read_bounded(stdout, DOCKER_DIAGNOSTIC_LIMIT));
        let stderr_reader = thread::spawn(move || read_bounded(stderr, DOCKER_DIAGNOSTIC_LIMIT));
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(error) = stdin.write_all(stdin_json.as_slice()) {
                child.disconnect();
                let _ = child.wait();
                return Err(format!("write lifecycle hook stdin failed: {error}"));
            }
        }

        let deadline =
            Instant::now() + Duration::from_millis(handler.timeout_ms.saturating_add(5_000));
        let (exit_code, timed_out) = loop {
            match child.try_wait() {
                Ok(Some(status)) if status.code() == Some(124) => break (None, true),
                Ok(Some(status)) => break (status.code(), false),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                Ok(None) => {
                    let teardown = Self::teardown(self.agent_run_id.as_str());
                    child.disconnect();
                    let _ = child.wait();
                    return Err(teardown.err().unwrap_or_else(|| {
                        "lifecycle hook docker exec exceeded its confirmed deadline; sandbox removed"
                            .to_string()
                    }));
                }
                Err(error) => {
                    child.disconnect();
                    let _ = child.wait();
                    return Err(format!("wait lifecycle hook docker exec failed: {error}"));
                }
            }
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| "lifecycle hook stdout reader panicked".to_string())??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| "lifecycle hook stderr reader panicked".to_string())??;
        Ok(LifecycleHookCommandResultV1 {
            exit_code,
            stdout: String::from_utf8_lossy(stdout.bytes.as_slice()).to_string(),
            stderr: String::from_utf8_lossy(stderr.bytes.as_slice()).to_string(),
            stdout_truncated: stdout.total_bytes > stdout.bytes.len(),
            stderr_truncated: stderr.total_bytes > stderr.bytes.len(),
            timed_out,
            spawn_error: None,
        })
    }

    pub fn teardown(agent_run_id: &str) -> Result<(), String> {
        #[cfg(test)]
        if crate::native_consumption_tests::owns_run(agent_run_id) {
            return Ok(());
        }
        crate::observations::timed("sandboxTeardown", || Self::teardown_inner(agent_run_id))
    }

    fn teardown_inner(agent_run_id: &str) -> Result<(), String> {
        for name in container_ids_for_agent_run(agent_run_id)? {
            let Some(facts) = inspect_container(name.as_str())? else {
                continue;
            };
            if !facts.has_agent_run_identity(agent_run_id) {
                return Err("sandbox container identity mismatch; refusing teardown".to_string());
            }
            crate::docker_engine::remove(&facts.id, false)?;
            if inspect_container(name.as_str())?.is_some() {
                return Err("sandbox container teardown was not confirmed".to_string());
            }
        }
        Ok(())
    }

    pub fn read_artifact(&self, path: &str) -> Result<(SandboxArtifactMetadata, Vec<u8>), String> {
        let request = serde_json::to_vec(&SandboxArtifactRequest {
            path: path.to_string(),
            max_bytes: MAX_PUBLISHED_ARTIFACT_BYTES,
        })
        .map_err(|error| format!("encode artifact helper request failed: {error}"))?;
        let output = self.helper(
            "read-artifact",
            None,
            request.as_slice(),
            MAX_PUBLISHED_ARTIFACT_BYTES as usize + HELPER_JSON_LIMIT + 4,
        )?;
        let (metadata, bytes) = decode_frame::<SandboxArtifactMetadata>(output.as_slice())?;
        if metadata.size_bytes != bytes.len() as u64
            || metadata.size_bytes > MAX_PUBLISHED_ARTIFACT_BYTES
            || format!("sha256:{:x}", Sha256::digest(bytes)) != metadata.sha256
        {
            return Err("artifact helper result integrity mismatch".to_string());
        }
        Ok((metadata, bytes.to_vec()))
    }

    pub(crate) fn resolve_session_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        let response = self
            .api_client
            .post(format!(
                "{}/internal/agent-runs/session-workspace/resolve",
                self.api_url
            ))
            .header("X-Internal-Token", self.api_token.as_str())
            .json(&SessionWorkspaceLeaseRequest::new(
                SESSION_WORKSPACE_RESOLVE_SCHEMA,
                self,
                lease,
            ))
            .timeout(Duration::from_secs(10))
            .send()
            .map_err(|error| {
                SessionWorkspaceApiError::Unavailable(format!(
                    "resolve session workspace failed: {error}"
                ))
            })?;
        if response.status().is_server_error() {
            return Err(SessionWorkspaceApiError::Unavailable(format!(
                "resolve session workspace returned {}",
                response.status().as_u16()
            )));
        }
        if !response.status().is_success() {
            return Err(SessionWorkspaceApiError::Rejected(format!(
                "resolve session workspace returned {}",
                response.status().as_u16()
            )));
        }
        let resolved = response
            .json::<SessionWorkspaceResolveResponse>()
            .map_err(|error| {
                SessionWorkspaceApiError::Rejected(format!(
                    "decode session workspace resolve failed: {error}"
                ))
            })?;
        if resolved.schema != SESSION_WORKSPACE_RESOLVED_SCHEMA {
            return Err(SessionWorkspaceApiError::Rejected(
                "session workspace resolve schema mismatch".to_string(),
            ));
        }
        match resolved.disposition.as_str() {
            "empty" if &resolved.session_workspace == frozen && frozen.snapshot_size_bytes == 0 => {
                Ok(SessionWorkspaceResolution::Empty)
            }
            "download"
                if &resolved.session_workspace == frozen && frozen.snapshot_size_bytes != 0 =>
            {
                Ok(SessionWorkspaceResolution::Download)
            }
            "advanced"
                if resolved.session_workspace.generation
                    == frozen.generation.checked_add(1).ok_or_else(|| {
                        SessionWorkspaceApiError::Rejected(
                            "session workspace generation overflow".to_string(),
                        )
                    })?
                    && resolved.session_workspace.validate().is_ok() =>
            {
                Ok(SessionWorkspaceResolution::Advanced)
            }
            _ => Err(SessionWorkspaceApiError::Rejected(
                "session workspace resolve binding mismatch".to_string(),
            )),
        }
    }

    pub(crate) fn restore_session_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        let resolution = self.resolve_session_workspace(lease, frozen)?;
        #[cfg(test)]
        if self.container_name.starts_with("native-consume-test:") {
            assert_eq!(resolution, SessionWorkspaceResolution::Empty);
            return Ok(resolution);
        }
        if resolution == SessionWorkspaceResolution::Advanced {
            return Ok(resolution);
        }
        self.require_workspace_capacity(frozen.expanded_size_bytes, input_upper_bound_bytes)
            .map_err(SessionWorkspaceApiError::Rejected)?;
        if resolution == SessionWorkspaceResolution::Empty {
            return Ok(resolution);
        }
        let capture = self.restore_activity()?;
        // Admission into the exclusive phase can race an API workspace advance.
        // Reobserve the restore decision after exclusion, before overwriting data.
        let resolution = self.resolve_session_workspace(lease, frozen)?;
        if resolution != SessionWorkspaceResolution::Download {
            return Ok(resolution);
        }
        let mut response = self
            .api_client
            .post(format!(
                "{}/internal/agent-runs/session-workspace/download",
                self.api_url
            ))
            .header("X-Internal-Token", self.api_token.as_str())
            .json(&SessionWorkspaceLeaseRequest::new(
                SESSION_WORKSPACE_DOWNLOAD_SCHEMA,
                self,
                lease,
            ))
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|error| {
                SessionWorkspaceApiError::Unavailable(format!(
                    "download session workspace failed: {error}"
                ))
            })?;
        if response.status().is_server_error() {
            return Err(SessionWorkspaceApiError::Unavailable(format!(
                "download session workspace returned {}",
                response.status().as_u16()
            )));
        }
        if !response.status().is_success()
            || response.content_length() != Some(frozen.snapshot_size_bytes)
            || response
                .headers()
                .get("x-content-sha256")
                .and_then(|value| value.to_str().ok())
                != Some(frozen.snapshot_sha256.as_str())
        {
            return Err(SessionWorkspaceApiError::Rejected(
                "session workspace download binding mismatch".to_string(),
            ));
        }
        self.restore_snapshot_stream(
            &mut response,
            frozen.snapshot_size_bytes,
            frozen.snapshot_sha256.as_str(),
            &capture,
        )
        .map_err(SessionWorkspaceApiError::Rejected)?;
        Ok(resolution)
    }

    pub(crate) fn collect_and_commit_session_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        #[cfg(test)]
        if self.container_name.starts_with("native-consume-test:") {
            return Ok(SessionWorkspaceCommitOutcome::Unchanged);
        }
        self.with_terminal_collection(|capture| {
            let artifact = self.capture_snapshot_collect(capture)?;
            let descriptor = &artifact.descriptor;
            self.require_workspace_capacity(
                descriptor.expanded_size_bytes,
                input_upper_bound_bytes,
            )?;
            if snapshot_matches_frozen_workspace(descriptor, frozen) {
                return Ok(SessionWorkspaceCommitOutcome::Unchanged);
            }
            let candidate = SessionWorkspace {
                generation: frozen
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| "session workspace generation overflow".to_string())?,
                snapshot_sha256: descriptor.sha256.clone(),
                snapshot_size_bytes: descriptor.size_bytes,
                expanded_size_bytes: descriptor.expanded_size_bytes,
                file_count: descriptor.file_count,
            };
            candidate.validate()?;
            let metadata = SessionWorkspaceCommitRequest {
                schema: SESSION_WORKSPACE_COMMIT_SCHEMA,
                job_id: lease.job_id.as_str(),
                lease_owner: lease.lease_owner.as_str(),
                agent_run_id: self.agent_run_id.as_str(),
                authorization_digest: self.authorization_digest.as_str(),
                snapshot_sha256: candidate.snapshot_sha256.as_str(),
                snapshot_size_bytes: candidate.snapshot_size_bytes,
                expanded_size_bytes: candidate.expanded_size_bytes,
                file_count: candidate.file_count,
            };
            let metadata = serde_json::to_vec(&metadata)
                .map_err(|error| format!("encode session workspace commit failed: {error}"))?;
            let metadata_length = u32::try_from(metadata.len())
                .map_err(|_| "session workspace commit metadata is too large".to_string())?;
            let mut prefix = Vec::with_capacity(4 + metadata.len());
            prefix.extend_from_slice(&metadata_length.to_be_bytes());
            prefix.extend_from_slice(metadata.as_slice());
            let response = if descriptor.size_bytes == 0 {
                self.api_client
                    .post(format!(
                        "{}/internal/agent-runs/session-workspace/commit",
                        self.api_url
                    ))
                    .header("X-Internal-Token", self.api_token.as_str())
                    .header("Content-Length", prefix.len().to_string())
                    .header("Content-Type", "application/octet-stream")
                    .body(prefix)
                    .timeout(Duration::from_secs(30))
                    .send()
            } else {
                let snapshot = artifact.file;
                let length = u64::try_from(prefix.len())
                    .map_err(|_| "session workspace commit length overflow".to_string())?
                    .checked_add(descriptor.size_bytes)
                    .ok_or_else(|| "session workspace commit length overflow".to_string())?;
                let body = SessionWorkspaceUploadBody {
                    prefix: Cursor::new(prefix),
                    snapshot,
                };
                self.api_client
                    .post(format!(
                        "{}/internal/agent-runs/session-workspace/commit",
                        self.api_url
                    ))
                    .header("X-Internal-Token", self.api_token.as_str())
                    .header("Content-Length", length.to_string())
                    .header("Content-Type", "application/octet-stream")
                    .body(reqwest::blocking::Body::sized(body, length))
                    .timeout(Duration::from_secs(300))
                    .send()
            };
            match response {
                Ok(response) if response.status().is_success() => {
                    let committed =
                        response
                            .json::<SessionWorkspaceCommitResponse>()
                            .map_err(|error| {
                                format!("decode session workspace commit failed: {error}")
                            })?;
                    if committed.schema != SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA
                        || !matches!(committed.disposition.as_str(), "committed" | "idempotent")
                        || committed.session_workspace != candidate
                    {
                        return Ok(SessionWorkspaceCommitOutcome::Rejected(
                            "session workspace commit response binding mismatch".to_string(),
                        ));
                    }
                    Ok(SessionWorkspaceCommitOutcome::Accepted)
                }
                Ok(response) if response.status().is_server_error() => {
                    Ok(self.reconcile_session_workspace_commit(lease, frozen)?)
                }
                Ok(response) => Ok(SessionWorkspaceCommitOutcome::Rejected(format!(
                    "session workspace commit returned {}",
                    response.status().as_u16()
                ))),
                Err(_) => Ok(self.reconcile_session_workspace_commit(lease, frozen)?),
            }
        })
    }

    fn with_terminal_collection(
        &self,
        collect: impl FnOnce(&SnapshotActivityGuard) -> Result<SessionWorkspaceCommitOutcome, String>,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        let Some(capture) = self.recovery_activity.try_final_snapshot_activity()? else {
            return Ok(SessionWorkspaceCommitOutcome::Pending);
        };
        let capture = capture.observe_as("workspaceFinalSnapshotHold");
        // Only finalization may terminate persistent/background agent processes.
        self.quiesce_agent_processes(&capture)?;
        collect(&capture)
    }

    /// Capture and seal a checkpoint while workspace dispatch is excluded.
    pub(crate) fn prepare_recovery_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        checkpoint_id: &str,
        previous: &RecoveryWorkspaceSnapshotV1,
        input_upper_bound_bytes: u64,
    ) -> Result<Option<PreparedRecoveryWorkspace>, String> {
        #[cfg(test)]
        if self.container_name.starts_with("native-consume-test:") {
            return self.capture_prepared_recovery_workspace_with_observer(
                |_| Ok(0),
                |_| Ok(PreparedRecoveryContent::Unchanged(previous.clone())),
            );
        }
        self.capture_prepared_recovery_workspace_with_observer(
            |capture| self.agent_process_count(capture),
            |capture| {
                let artifact = crate::observations::timed("workspaceSnapshotInspect", || {
                    self.capture_snapshot_collect(capture)
                })?;
                self.require_workspace_capacity(
                    artifact.descriptor.expanded_size_bytes,
                    input_upper_bound_bytes,
                )?;
                if snapshot_matches_recovery_workspace(&artifact.descriptor, previous) {
                    return Ok(PreparedRecoveryContent::Unchanged(previous.clone()));
                }
                Ok(PreparedRecoveryContent::Upload {
                    artifact,
                    lease: lease.clone(),
                    checkpoint_id: checkpoint_id.to_string(),
                })
            },
        )
    }

    pub(crate) fn stage_recovery_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        checkpoint_id: &str,
        previous: &RecoveryWorkspaceSnapshotV1,
        input_upper_bound_bytes: u64,
    ) -> Result<Option<RecoveryWorkspaceSnapshotV1>, String> {
        let Some(prepared) = self.prepare_recovery_workspace(
            lease,
            checkpoint_id,
            previous,
            input_upper_bound_bytes,
        )?
        else {
            return Ok(None);
        };
        // Blocking reqwest must be created, consumed and dropped outside an async context.
        let evidence = prepared.evidence();
        let upload = || self.upload_recovery_workspace(prepared);
        let result = if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::scope(|scope| match scope.spawn(upload).join() {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            })
        } else {
            upload()
        };
        let snapshot = result?;
        if !self.accept_captured_recovery_workspace(evidence)? {
            return Ok(None);
        }
        Ok(Some(snapshot))
    }

    pub(crate) fn accept_captured_recovery_workspace(
        &self,
        evidence: WorkspaceReuseEvidence,
    ) -> Result<bool, String> {
        let Some(_guard) = self.recovery_activity.try_snapshot_activity()? else {
            return Ok(false);
        };
        let current = self.recovery_activity.workspace_evidence(&evidence);
        if current.current_activity_epoch != evidence.snapshot_activity_epoch
            || current.current_host_instance != evidence.snapshot_host_instance
            || self.deferred_extension_dispatch.load(Ordering::SeqCst)
        {
            return Ok(false);
        }
        *self
            .recovery_snapshot
            .lock()
            .map_err(|_| "recovery snapshot lock poisoned")? = Some(evidence);
        Ok(true)
    }

    fn capture_prepared_recovery_workspace_with_observer(
        &self,
        observe: impl FnOnce(&SnapshotActivityGuard) -> Result<u64, String>,
        collect: impl FnOnce(&SnapshotActivityGuard) -> Result<PreparedRecoveryContent, String> + Send,
    ) -> Result<Option<PreparedRecoveryWorkspace>, String> {
        Ok(self
            .capture_sealed_recovery_workspace_with_observer(observe, collect)?
            .map(|(content, evidence)| PreparedRecoveryWorkspace { content, evidence }))
    }

    #[cfg(test)]
    fn capture_recovery_workspace_with_observer<T: Send>(
        &self,
        observe: impl FnOnce(&SnapshotActivityGuard) -> Result<u64, String>,
        collect: impl FnOnce(&SnapshotActivityGuard) -> Result<T, String> + Send,
    ) -> Result<Option<T>, String> {
        let Some((snapshot, evidence)) =
            self.capture_sealed_recovery_workspace_with_observer(observe, collect)?
        else {
            return Ok(None);
        };
        *self
            .recovery_snapshot
            .lock()
            .map_err(|_| "recovery snapshot lock poisoned")? = Some(evidence);
        Ok(Some(snapshot))
    }

    fn capture_sealed_recovery_workspace_with_observer<T: Send>(
        &self,
        observe: impl FnOnce(&SnapshotActivityGuard) -> Result<u64, String>,
        collect: impl FnOnce(&SnapshotActivityGuard) -> Result<T, String> + Send,
    ) -> Result<Option<(T, WorkspaceReuseEvidence)>, String> {
        *self
            .recovery_snapshot
            .lock()
            .map_err(|_| "recovery snapshot lock poisoned")? = None;
        let Some(capture) = self.recovery_activity.try_snapshot_activity()? else {
            return Ok(None);
        };
        let capture = capture.observe_as("workspaceRecoverySnapshotHold");
        if observe(&capture)? != 0 {
            return Ok(None);
        }
        let token = self.recovery_activity.begin_snapshot_capture();
        let collect = || collect(&capture);
        // ModelRequestStarted is invoked inside the async model driver. Keep
        // synchronous collection outside that context without retrying. The
        // immutable artifact can be uploaded after this gate has been released.
        let snapshot = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                tokio::task::block_in_place(collect)
            } else {
                std::thread::scope(|scope| match scope.spawn(collect).join() {
                    Ok(result) => result,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
            }
        } else {
            collect()
        }?;
        let evidence = match self
            .recovery_activity
            .complete_quiesced_snapshot(token, true, true)
        {
            Ok(evidence) => evidence,
            Err(SnapshotCaptureError::ActivityChanged) => {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "event": "recovery_checkpoint_deferred", "agentRunId": self.agent_run_id,
                        "reason": "workspace_activity_changed",
                    })
                );
                return Ok(None);
            }
            Err(SnapshotCaptureError::HostChanged) => {
                return Err(
                    "execution host changed during workspace snapshot collection".to_string(),
                );
            }
        };
        self.pre_dispatch_loss.store(false, Ordering::SeqCst);
        Ok(Some((snapshot, evidence)))
    }

    /// Upload an immutable capture. This method never takes the workspace activity gate.
    pub(crate) fn upload_recovery_workspace(
        &self,
        prepared: PreparedRecoveryWorkspace,
    ) -> Result<RecoveryWorkspaceSnapshotV1, String> {
        let PreparedRecoveryContent::Upload {
            artifact,
            lease,
            checkpoint_id,
        } = prepared.content
        else {
            let PreparedRecoveryContent::Unchanged(snapshot) = prepared.content else {
                unreachable!()
            };
            return Ok(snapshot);
        };
        let descriptor = &artifact.descriptor;
        crate::observations::timed("workspaceSnapshotUpload", || {
            let metadata = ExecutionWorkspaceStageRequest {
                schema: EXECUTION_WORKSPACE_STAGE_SCHEMA,
                job_id: lease.job_id.as_str(),
                lease_owner: lease.lease_owner.as_str(),
                agent_run_id: self.agent_run_id.as_str(),
                authorization_digest: self.authorization_digest.as_str(),
                checkpoint_id: checkpoint_id.as_str(),
                snapshot_sha256: descriptor.sha256.as_str(),
                snapshot_size_bytes: descriptor.size_bytes,
                expanded_size_bytes: descriptor.expanded_size_bytes,
                file_count: descriptor.file_count,
            };
            let metadata = serde_json::to_vec(&metadata)
                .map_err(|error| format!("encode execution workspace stage failed: {error}"))?;
            let metadata_length = u32::try_from(metadata.len())
                .map_err(|_| "execution workspace stage metadata is too large".to_string())?;
            let mut prefix = Vec::with_capacity(4 + metadata.len());
            prefix.extend_from_slice(&metadata_length.to_be_bytes());
            prefix.extend_from_slice(metadata.as_slice());
            let response = if descriptor.size_bytes == 0 {
                self.api_client
                    .post(format!(
                        "{}/internal/agent-runs/execution-workspace/stage",
                        self.api_url
                    ))
                    .header("X-Internal-Token", self.api_token.as_str())
                    .header("Content-Length", prefix.len().to_string())
                    .header("Content-Type", "application/octet-stream")
                    .body(prefix)
                    .timeout(Duration::from_secs(30))
                    .send()
            } else {
                let snapshot = artifact.file;
                let length = u64::try_from(prefix.len())
                    .map_err(|_| "execution workspace stage length overflow".to_string())?
                    .checked_add(descriptor.size_bytes)
                    .ok_or_else(|| "execution workspace stage length overflow".to_string())?;
                self.api_client
                    .post(format!(
                        "{}/internal/agent-runs/execution-workspace/stage",
                        self.api_url
                    ))
                    .header("X-Internal-Token", self.api_token.as_str())
                    .header("Content-Length", length.to_string())
                    .header("Content-Type", "application/octet-stream")
                    .body(reqwest::blocking::Body::sized(
                        SessionWorkspaceUploadBody {
                            prefix: Cursor::new(prefix),
                            snapshot,
                        },
                        length,
                    ))
                    .timeout(Duration::from_secs(300))
                    .send()
            }
            .map_err(|error| format!("stage execution workspace failed: {error}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "stage execution workspace returned {}",
                    response.status().as_u16()
                ));
            }
            let staged = response
                .json::<ExecutionWorkspaceStageResponse>()
                .map_err(|error| format!("decode execution workspace stage failed: {error}"))?;
            let result = RecoveryWorkspaceSnapshotV1 {
                object_ref: staged.object_ref,
                snapshot_sha256: staged.snapshot_sha256,
                snapshot_size_bytes: staged.snapshot_size_bytes,
                expanded_size_bytes: staged.expanded_size_bytes,
                file_count: staged.file_count,
            };
            if staged.schema != EXECUTION_WORKSPACE_STAGE_RESULT_SCHEMA
                || result.snapshot_sha256 != descriptor.sha256
                || result.snapshot_size_bytes != descriptor.size_bytes
                || result.expanded_size_bytes != descriptor.expanded_size_bytes
                || result.file_count != descriptor.file_count
            {
                return Err("execution workspace stage response binding mismatch".to_string());
            }
            result.validate()?;
            Ok(result)
        })
    }

    pub(crate) fn restore_recovery_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        checkpoint_id: &str,
        snapshot: &RecoveryWorkspaceSnapshotV1,
        input_upper_bound_bytes: u64,
    ) -> Result<(), SessionWorkspaceApiError> {
        snapshot
            .validate()
            .map_err(SessionWorkspaceApiError::Rejected)?;
        self.require_workspace_capacity(snapshot.expanded_size_bytes, input_upper_bound_bytes)
            .map_err(SessionWorkspaceApiError::Rejected)?;
        if snapshot.object_ref.is_none() {
            return Ok(());
        }
        let capture = self.restore_activity()?;
        let request = ExecutionWorkspaceDownloadRequest {
            schema: EXECUTION_WORKSPACE_DOWNLOAD_SCHEMA,
            job_id: lease.job_id.as_str(),
            lease_owner: lease.lease_owner.as_str(),
            agent_run_id: self.agent_run_id.as_str(),
            authorization_digest: self.authorization_digest.as_str(),
            checkpoint_id,
        };
        let mut response = self
            .api_client
            .post(format!(
                "{}/internal/agent-runs/execution-workspace/download",
                self.api_url
            ))
            .header("X-Internal-Token", self.api_token.as_str())
            .json(&request)
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|error| {
                SessionWorkspaceApiError::Unavailable(format!(
                    "download execution workspace failed: {error}"
                ))
            })?;
        if response.status().is_server_error() {
            return Err(SessionWorkspaceApiError::Unavailable(format!(
                "download execution workspace returned {}",
                response.status().as_u16()
            )));
        }
        if !response.status().is_success()
            || response.content_length() != Some(snapshot.snapshot_size_bytes)
            || response
                .headers()
                .get("x-content-sha256")
                .and_then(|value| value.to_str().ok())
                != Some(snapshot.snapshot_sha256.as_str())
        {
            return Err(SessionWorkspaceApiError::Rejected(
                "execution workspace download binding mismatch".to_string(),
            ));
        }
        self.restore_snapshot_stream(
            &mut response,
            snapshot.snapshot_size_bytes,
            snapshot.snapshot_sha256.as_str(),
            &capture,
        )
        .map_err(SessionWorkspaceApiError::Rejected)
    }

    fn reconcile_session_workspace_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        match self.resolve_session_workspace(lease, frozen) {
            Ok(SessionWorkspaceResolution::Advanced) => Ok(SessionWorkspaceCommitOutcome::Accepted),
            Ok(SessionWorkspaceResolution::Empty | SessionWorkspaceResolution::Download) => {
                Ok(SessionWorkspaceCommitOutcome::Rejected(
                    "session workspace commit was not accepted".to_string(),
                ))
            }
            Err(SessionWorkspaceApiError::Unavailable(_)) => {
                Ok(SessionWorkspaceCommitOutcome::Pending)
            }
            Err(SessionWorkspaceApiError::Rejected(reason)) => {
                Ok(SessionWorkspaceCommitOutcome::Rejected(reason))
            }
        }
    }

    fn require_workspace_capacity(
        &self,
        expanded_size_bytes: u64,
        input_upper_bound_bytes: u64,
    ) -> Result<(), String> {
        if expanded_size_bytes
            .checked_add(input_upper_bound_bytes)
            .and_then(|value| value.checked_add(SESSION_WORKSPACE_RESTORE_OVERHEAD_BYTES))
            .is_none_or(|value| value > self.resources.data_tmpfs_bytes)
        {
            return Err("session workspace exceeds sandbox dataTmpfsBytes".to_string());
        }
        Ok(())
    }

    fn snapshot_collect_command(
        &self,
        capture: &SnapshotActivityGuard,
    ) -> Result<ExecChild, String> {
        let _ = capture;
        let mut request = self.exec_request(vec![AGENT_BINARY.into(), "snapshot-collect".into()]);
        request.cwd = Some(WORKSPACE_DATA_ROOT.into());
        let mut child = request.spawn()?;
        child.stdin.take();
        Ok(child)
    }

    fn capture_snapshot_collect(
        &self,
        capture: &SnapshotActivityGuard,
    ) -> Result<ImmutableWorkspaceSnapshot, String> {
        let mut child = self.snapshot_collect_command(capture)?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "snapshot collect helper stderr is unavailable".to_string())?;
        let stderr_reader = thread::spawn(move || read_bounded(stderr, DOCKER_DIAGNOSTIC_LIMIT));
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "snapshot collect helper stdout is unavailable".to_string())?;
        let result = match capture_snapshot_artifact(
            &mut stdout,
            self.resources
                .data_tmpfs_bytes
                .saturating_add(SESSION_WORKSPACE_MANIFEST_LIMIT as u64 + 4),
        ) {
            Ok(result) => result,
            Err(error) => {
                drop(stdout);
                child.disconnect();
                let _ = child.wait();
                let _ = stderr_reader.join();
                return Err(error);
            }
        };
        let status = child
            .wait()
            .map_err(|error| format!("wait for snapshot collect helper failed: {error}"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| "snapshot collect helper stderr reader panicked".to_string())?
            .map_err(|error| format!("read snapshot collect helper stderr failed: {error}"))?;
        if !status.success() {
            return Err(format!(
                "snapshot collect helper failed: {}",
                bounded_diagnostic(stderr.bytes.as_slice())
            ));
        }
        Ok(result)
    }

    fn restore_snapshot_stream(
        &self,
        source: &mut impl Read,
        expected_size_bytes: u64,
        expected_sha256: &str,
        capture: &SnapshotActivityGuard,
    ) -> Result<(), String> {
        self.recovery_activity.invalidate_before_dispatch()?;
        let _ = capture;
        let mut request = self.exec_request(vec![AGENT_BINARY.into(), "snapshot-restore".into()]);
        request.cwd = Some(WORKSPACE_DATA_ROOT.into());
        let mut child = request.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or("snapshot restore stdout missing")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("snapshot restore stderr missing")?;
        let stdout_reader = thread::spawn(move || read_bounded(stdout, DOCKER_DIAGNOSTIC_LIMIT));
        let stderr_reader = thread::spawn(move || read_bounded(stderr, DOCKER_DIAGNOSTIC_LIMIT));
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "snapshot restore helper stdin is unavailable".to_string())?;
        let copy = copy_exact(source, &mut stdin, expected_size_bytes, expected_sha256);
        drop(stdin);
        if let Err(error) = copy {
            child.disconnect();
            let _ = child.wait();
            return Err(error);
        }
        let status = child.wait()?;
        let _stdout = stdout_reader
            .join()
            .map_err(|_| "snapshot restore stdout reader panicked")??;
        let stderr = stderr_reader
            .join()
            .map_err(|_| "snapshot restore stderr reader panicked")??;
        if !status.success() {
            return Err(format!(
                "snapshot restore helper failed: {}",
                bounded_diagnostic(&stderr.bytes)
            ));
        }
        Ok(())
    }

    fn restore_activity(&self) -> Result<SnapshotActivityGuard, SessionWorkspaceApiError> {
        self.restore_activity_with_observer(|capture| self.agent_process_count(capture))
    }

    fn restore_activity_with_observer(
        &self,
        observe: impl FnOnce(&SnapshotActivityGuard) -> Result<u64, String>,
    ) -> Result<SnapshotActivityGuard, SessionWorkspaceApiError> {
        let capture = self
            .recovery_activity
            .try_snapshot_activity()
            .map_err(|error| SessionWorkspaceApiError::Unavailable(error.into()))?
            .ok_or_else(|| {
                SessionWorkspaceApiError::Unavailable(
                    "workspace restore deferred while host operations are active".into(),
                )
            })
            .map(|capture| capture.observe_as("workspaceRestoreSnapshotHold"))?;
        if observe(&capture).map_err(SessionWorkspaceApiError::Unavailable)? != 0 {
            return Err(SessionWorkspaceApiError::Unavailable(
                "workspace restore deferred while agent processes are active".into(),
            ));
        }
        Ok(capture)
    }

    fn quiesce_existing_execution(&self) -> Result<bool, String> {
        self.with_idle_quiescence(
            |capture| self.agent_process_count(capture),
            |capture| self.quiesce_agent_processes(capture),
        )
    }

    fn with_idle_quiescence(
        &self,
        observe: impl FnOnce(&SnapshotActivityGuard) -> Result<u64, String>,
        quiesce: impl FnOnce(&SnapshotActivityGuard) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(capture) = self.recovery_activity.try_snapshot_activity()? else {
            return Ok(false);
        };
        let capture = capture.observe_as("workspaceQuiesceHold");
        if observe(&capture)? != 0 {
            return Ok(false);
        }
        quiesce(&capture)?;
        Ok(true)
    }

    fn quiesce_agent_processes(&self, _capture: &SnapshotActivityGuard) -> Result<(), String> {
        self.helper(
            "quiesce-agent-processes",
            Some(AGENT_USER),
            &[],
            HELPER_JSON_LIMIT,
        )?;
        Ok(())
    }

    fn agent_process_count(&self, _capture: &SnapshotActivityGuard) -> Result<u64, String> {
        let output = self.helper("agent-process-state", None, &[], HELPER_JSON_LIMIT)?;
        let state: AgentProcessState = serde_json::from_slice(&output)
            .map_err(|_| "sandbox agent-process-state response invalid".to_string())?;
        Ok(state.active_process_count)
    }

    #[cfg(test)]
    fn test_capture_recovery_workspace(
        &self,
        collect: impl FnOnce(&SnapshotActivityGuard) -> Result<RecoveryWorkspaceSnapshotV1, String>
            + Send,
    ) -> Result<Option<RecoveryWorkspaceSnapshotV1>, String> {
        self.capture_recovery_workspace_with_observer(|_| Ok(0), collect)
    }

    fn validate_policy(&self, policy: &ExecutionPolicy) -> Result<(), ExecutionError> {
        if policy.filesystem.workspace_root != Path::new(WORKSPACE_DATA_ROOT)
            || policy.network != NetworkPolicy::Disabled
        {
            return Err(ExecutionError::Denied {
                reason: "Docker execution requires /mnt/data and disabled network policy"
                    .to_string(),
            });
        }
        Ok(())
    }

    fn unavailable(&self, reason: impl Into<String>) -> ExecutionError {
        ExecutionError::PolicyUnavailable {
            reason: reason.into(),
        }
    }

    fn input_inventory(&self) -> Result<SandboxInputInventory, String> {
        let output = self.helper("input-inventory", None, &[], HELPER_JSON_LIMIT)?;
        let inventory = serde_json::from_slice::<SandboxInputInventory>(output.as_slice())
            .map_err(|error| format!("decode sandbox input inventory failed: {error}"))?;
        if inventory.schema != SANDBOX_INPUT_INVENTORY_SCHEMA {
            return Err("sandbox input inventory schema mismatch".to_string());
        }
        Ok(inventory)
    }

    fn refresh_materialized_inputs(
        &self,
    ) -> Result<Vec<ExecutionInputStateChange>, ExecutionError> {
        let _guard = self
            .input_lock
            .lock()
            .map_err(|_| self.unavailable("sandbox input operation lock poisoned"))?;
        self.refresh_materialized_inputs_locked()
    }

    fn refresh_materialized_inputs_locked(
        &self,
    ) -> Result<Vec<ExecutionInputStateChange>, ExecutionError> {
        let active = self
            .materialized_inputs
            .lock()
            .map_err(|_| self.unavailable("sandbox input registry lock poisoned"))?
            .values()
            .filter(|input| input.state.is_none())
            .cloned()
            .collect::<Vec<_>>();
        if active.is_empty() {
            return Ok(Vec::new());
        }
        let expected = active
            .iter()
            .map(ProjectedInputValidation::from)
            .collect::<Vec<_>>();
        let response = self
            .api_client
            .post(format!(
                "{}/internal/agent-runs/validate-inputs",
                self.api_url
            ))
            .header("X-Internal-Token", self.api_token.as_str())
            .json(&ValidateInputsRequest {
                schema: VALIDATE_INPUTS_SCHEMA,
                agent_run_id: self.agent_run_id.as_str(),
                authorization_digest: self.authorization_digest.as_str(),
                inputs: expected.as_slice(),
            })
            .timeout(Duration::from_secs(10))
            .send()
            .map_err(|error| {
                self.unavailable(format!("validate sandbox inputs failed: {error}"))
            })?;
        if !response.status().is_success() {
            return Err(self.unavailable(format!(
                "validate sandbox inputs returned {}",
                response.status().as_u16()
            )));
        }
        let validation = response.json::<ValidateInputsResponse>().map_err(|error| {
            self.unavailable(format!("decode input validation failed: {error}"))
        })?;
        if validation.schema != VALIDATE_INPUTS_SCHEMA || validation.inputs.len() != active.len() {
            return Err(self.unavailable("input validation response mismatch"));
        }
        let mut changes = Vec::new();
        for (current, validated) in active.iter().zip(validation.inputs) {
            if validated.input_ref != current.input_ref {
                return Err(self.unavailable("input validation response binding mismatch"));
            }
            let state = match validated.state {
                ProjectedInputState::Active => continue,
                ProjectedInputState::AssetRemoved => ExecutionInputState::AssetRemoved,
                ProjectedInputState::AccessRevoked => ExecutionInputState::AccessRevoked,
                ProjectedInputState::SourceDeleted => ExecutionInputState::SourceDeleted,
                ProjectedInputState::StaleGeneration => ExecutionInputState::StaleGeneration,
            };
            self.revoke_input(current, state)?;
            changes.push(ExecutionInputStateChange {
                input_ref: current.input_ref.clone(),
                state,
            });
        }
        Ok(changes)
    }

    fn revoke_input(
        &self,
        input: &SandboxMaterializedInput,
        state: ExecutionInputState,
    ) -> Result<(), ExecutionError> {
        let body = serde_json::to_vec(&SandboxInputRevokeRequest {
            input_ref: input.input_ref.clone(),
            virtual_path: input.virtual_path.clone(),
            state,
        })
        .map_err(|error| self.unavailable(format!("encode input revocation failed: {error}")))?;
        self.helper("revoke-input", None, body.as_slice(), HELPER_JSON_LIMIT)
            .map_err(|error| self.unavailable(error))?;
        let mut registry = self
            .materialized_inputs
            .lock()
            .map_err(|_| self.unavailable("sandbox input registry lock poisoned"))?;
        registry
            .get_mut(input.input_ref.as_str())
            .ok_or_else(|| self.unavailable("sandbox input disappeared during revocation"))?
            .state = Some(state);
        Ok(())
    }

    fn helper(
        &self,
        mode: &str,
        user: Option<&str>,
        input: &[u8],
        output_limit: usize,
    ) -> Result<Vec<u8>, String> {
        self.with_helper_dispatch(mode, || self.helper_inner(mode, user, input, output_limit))
    }

    fn with_helper_dispatch<T>(
        &self,
        mode: &str,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _activity = if matches!(mode, "quiesce-agent-processes" | "agent-process-state") {
            None
        } else {
            Some(self.recovery_activity.begin_dispatch_activity()?)
        };
        operation()
    }

    fn helper_inner(
        &self,
        mode: &str,
        user: Option<&str>,
        input: &[u8],
        output_limit: usize,
    ) -> Result<Vec<u8>, String> {
        if !matches!(
            mode,
            "read-artifact" | "input-inventory" | "quiesce-agent-processes" | "agent-process-state"
        ) {
            self.recovery_activity.invalidate_before_dispatch()?;
        }
        let mut request = self.exec_request(vec![AGENT_BINARY.into(), mode.into()]);
        request.user = user.map(str::to_string);
        request.cwd = Some(WORKSPACE_DATA_ROOT.into());
        let output = exec_with_input(&request, input, output_limit)?;
        if !output.status.success() {
            return Err(helper_failure(
                mode,
                output.status.code(),
                output.stderr.as_slice(),
            ));
        }
        if output.stdout.len() > output_limit {
            return Err(format!("sandbox helper {mode} exceeded its output limit"));
        }
        Ok(output.stdout)
    }

    fn start_workspace_generation_rpc(&self) -> Result<WorkspaceGenerationRpc, String> {
        let mut request =
            self.exec_request(vec![AGENT_BINARY.into(), "workspace-generation-rpc".into()]);
        request.cwd = Some(WORKSPACE_DATA_ROOT.into());
        WorkspaceGenerationRpc::from_exec(request.spawn()?)
    }

    fn protected_input_error(
        &self,
        model_path: &str,
        operation: &ExecutionFileSystemOperation,
    ) -> Result<Option<ExecutionFileSystemError>, ExecutionFileSystemError> {
        let path = normalize_model_path(model_path, operation)?;
        let mutation = matches!(
            operation,
            ExecutionFileSystemOperation::WriteFile { .. }
                | ExecutionFileSystemOperation::DeleteFile { .. }
        );
        let registry = self.materialized_inputs.lock().map_err(|_| {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::HostUnavailable,
                "sandbox input registry lock poisoned",
            )
        })?;
        let Some(input) = registry.values().find(|input| input.virtual_path == path) else {
            return Ok(None);
        };
        if let Some(state) = input.state {
            let kind = match state {
                ExecutionInputState::AssetRemoved => ExecutionFileSystemErrorKind::AssetRemoved,
                ExecutionInputState::AccessRevoked => ExecutionFileSystemErrorKind::AccessRevoked,
                ExecutionInputState::SourceDeleted => ExecutionFileSystemErrorKind::SourceDeleted,
                ExecutionInputState::StaleGeneration => {
                    ExecutionFileSystemErrorKind::StaleGeneration
                }
            };
            return Ok(Some(ExecutionFileSystemError::new(
                kind,
                "authorized input is no longer available",
            )));
        }
        Ok(mutation.then(|| {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                "protected_input",
            )
        }))
    }
}

impl LifecycleHookRunner for DockerExecutionHostRunner {
    fn run_hook(
        &self,
        handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1 {
        self.run_lifecycle_hook_command(handler, event)
            .unwrap_or_else(|error| LifecycleHookCommandResultV1 {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                spawn_error: Some(error),
            })
    }
}

impl ExecutionHostRunner for DockerExecutionHostRunner {
    fn status(&self, policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        self.validate_policy(policy)?;
        let facts = inspect_container(self.container_name.as_str())
            .map_err(|error| self.unavailable(error))?
            .ok_or_else(|| {
                self.pre_dispatch_loss.store(true, Ordering::SeqCst);
                self.unavailable("sandbox container is missing")
            })?;
        if facts.matches_configuration(&self.container_expectation()) && !facts.running {
            self.pre_dispatch_loss.store(true, Ordering::SeqCst);
            return Err(self.unavailable("sandbox container is stopped"));
        }
        if !facts.matches(&self.container_expectation())
            || !workspace_execution_sentinel_matches(
                self.container_name.as_str(),
                self.agent_run_id.as_str(),
                self.execution_id.as_str(),
                self.authorization_digest.as_str(),
            )
            .map_err(|error| self.unavailable(error))?
        {
            return Err(self.unavailable("sandbox container identity mismatch"));
        }
        Ok(ExecutionHostStatus::remote(
            true,
            ExecutionHostHealth::Ready,
            None,
        ))
    }

    fn workspace_generation(&self) -> ExecutionWorkspaceGeneration {
        #[cfg(test)]
        if self.container_name.starts_with("native-consume-test:") {
            return ExecutionWorkspaceGeneration::Known {
                token: centaeris_core::execution::ExecutionWorkspaceGenerationV1 {
                    instance_epoch: self.execution_id.clone(),
                    generation: 0,
                },
            };
        }
        let generation = self
            .workspace_generation_rpc
            .lock()
            .map_err(|_| "workspace generation RPC lock poisoned".to_string())
            .and_then(|mut rpc| {
                query_workspace_generation_rpc(
                    &mut rpc,
                    WORKSPACE_GENERATION_RPC_RESPONSE_TIMEOUT,
                    || self.start_workspace_generation_rpc(),
                )
            });
        match generation {
            Ok(generation) => match generation.generation {
                Some(generation) => ExecutionWorkspaceGeneration::Known { token: generation },
                None => {
                    let reason = generation
                        .diagnostic
                        .unwrap_or_else(|| "workspace generation is unknown".to_string());
                    eprintln!(
                        "Workspace generation unavailable: {reason}; transitionReason=workspace_generation_unknown; forceCollect=true"
                    );
                    ExecutionWorkspaceGeneration::Unknown { reason }
                }
            },
            Err(reason) => {
                eprintln!(
                    "Workspace generation unavailable: {reason}; transitionReason=workspace_generation_unknown; forceCollect=true"
                );
                ExecutionWorkspaceGeneration::Unknown { reason }
            }
        }
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        #[cfg(test)]
        if self.container_name.starts_with("native-consume-test:") {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::NotFound,
                "empty native consume test host",
            ));
        }
        if request.cwd != Path::new(WORKSPACE_DATA_ROOT) {
            return Err(filesystem_unavailable(
                "sandbox filesystem operation escaped /mnt/data",
            ));
        }
        let memory_path = if is_memory_uri(request.model_path.as_str()) {
            MemoryPath::parse(request.model_path.as_str()).map_err(|_| {
                ExecutionFileSystemError::new(
                    ExecutionFileSystemErrorKind::InvalidPath,
                    "plastic-memories URI is invalid",
                )
            })?;
            true
        } else {
            false
        };
        if !memory_path && request.model_path.contains("://") {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::InvalidPath,
                "filesystem URI scheme is unsupported",
            ));
        }
        let plugin_path =
            authorized_plugin_path(self.plugin_mounts.as_slice(), request.model_path.as_str())?;
        let system_skill_path = authorized_system_skill_path(request.model_path.as_str())?;
        let read_only_path = plugin_path || system_skill_path;
        if read_only_path
            && !matches!(
                &request.operation,
                ExecutionFileSystemOperation::ReadFile { .. }
                    | ExecutionFileSystemOperation::ListDirectory { .. }
            )
        {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                "projected Skill and Plugin files are read-only",
            ));
        }
        if !read_only_path && !memory_path {
            self.refresh_materialized_inputs()
                .map_err(|error| filesystem_unavailable(error.to_string()))?;
        }
        if !read_only_path && !memory_path {
            if let Some(error) =
                self.protected_input_error(request.model_path.as_str(), &request.operation)?
            {
                return Err(error);
            }
        }
        let output_limit = filesystem_response_limit(&request.operation)?;
        let sandbox_request = SandboxFileSystemRequest {
            path: request.model_path,
            operation: request.operation,
        };
        let body = serde_json::to_vec(&sandbox_request).map_err(|error| {
            filesystem_unavailable(format!("encode filesystem request failed: {error}"))
        })?;
        let output = self
            .helper(
                "filesystem-once",
                (!memory_path).then_some(AGENT_USER),
                body.as_slice(),
                output_limit,
            )
            .map_err(filesystem_unavailable)?;
        let mut result = serde_json::from_slice::<SandboxFileSystemResult>(output.as_slice())
            .map_err(|error| {
                filesystem_unavailable(format!("decode filesystem result failed: {error}"))
            })?
            .into_result()?;
        if !memory_path && !read_only_path {
            if let Ok(mut buffer) = self.transcript_capture.lock() {
                buffer.observe_write(
                    &sandbox_request.operation,
                    &result,
                    usize::try_from(
                        self.resources
                            .memory_bytes
                            .min(self.resources.data_tmpfs_bytes),
                    )
                    .unwrap_or(usize::MAX),
                );
            }
        }
        if !read_only_path && !memory_path {
            if let ExecutionFileSystemOutput::ListDirectory(list) = &mut result {
                let hidden = self
                    .materialized_inputs
                    .lock()
                    .map_err(|_| filesystem_unavailable("sandbox input registry lock poisoned"))?
                    .values()
                    .filter(|input| input.state.is_some())
                    .map(|input| input.virtual_path.clone())
                    .collect::<HashSet<_>>();
                list.entries
                    .retain(|entry| !hidden.contains(entry.path.as_str()));
            }
        }
        Ok(result)
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        request: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        let _activity = self
            .recovery_activity
            .begin_dispatch_activity()
            .map_err(|error| self.unavailable(error))?;
        self.status(&request.policy)?;
        self.recovery_activity
            .invalidate_before_dispatch()
            .map_err(|error| self.unavailable(error))?;
        let input_state_changes = self.refresh_materialized_inputs()?;
        let mut command = self.exec_request(Vec::new());
        command.user = Some(AGENT_USER.into());
        command.cwd = Some(WORKSPACE_DATA_ROOT.into());
        for (name, default) in [("LANG", "C.UTF-8"), ("LC_ALL", "C.UTF-8"), ("TERM", "dumb")] {
            let value = request.env.get(name).map(String::as_str).unwrap_or(default);
            command.env.push(format!("{name}={value}"));
        }
        for (name, value) in [
            ("HOME", WORKSPACE_HOME),
            ("PATH", self.command_path.as_str()),
            ("TMPDIR", "/tmp"),
        ] {
            command.env.push(format!("{name}={value}"));
        }
        command.command.extend([
            "/usr/bin/timeout".to_string(),
            "--signal=TERM".to_string(),
            "--kill-after=1s".to_string(),
            coreutils_timeout_duration(request.timeout_ms),
            request.program,
        ]);
        command.command.extend(request.args);

        let mut child = command
            .spawn()
            .map_err(|error| ExecutionError::CancellationIndeterminate { reason: error })?;
        child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| self.unavailable("docker exec stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| self.unavailable("docker exec stderr is unavailable"))?;
        // Drain both channels, retaining at most 8 MiB each and exact raw byte counts.
        let stdout_reader = thread::spawn(move || capture_command_output(stdout, 8 * 1024 * 1024));
        let stderr_reader = thread::spawn(move || capture_command_output(stderr, 8 * 1024 * 1024));
        let deadline =
            Instant::now() + Duration::from_millis(request.timeout_ms.saturating_add(5_000));
        let (exit_code, timed_out, cancelled) = loop {
            if let Some(probe) = cancellation_probe {
                if probe()
                    .map_err(|error| {
                        self.unavailable(format!("execution cancellation probe failed: {error}"))
                    })?
                    .is_some()
                {
                    match Self::teardown(self.agent_run_id.as_str()) {
                        Ok(()) => {
                            child.disconnect();
                            break (None, false, true);
                        }
                        Err(error) => {
                            child.disconnect();
                            let _ = child.wait();
                            return Err(ExecutionError::CancellationIndeterminate {
                                reason: error,
                            });
                        }
                    }
                }
            }
            match child.try_wait() {
                Ok(Some(status)) if status.code() == Some(124) => break (None, true, false),
                Ok(Some(status)) => break (status.code(), false, false),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                Ok(None) => {
                    let teardown = Self::teardown(self.agent_run_id.as_str());
                    child.disconnect();
                    let _ = child.wait();
                    return Err(ExecutionError::CancellationIndeterminate {
                        reason: teardown.err().unwrap_or_else(|| {
                            "docker exec exceeded its confirmed deadline".to_string()
                        }),
                    });
                }
                Err(error) => {
                    child.disconnect();
                    let _ = child.wait();
                    return Err(ExecutionError::CancellationIndeterminate {
                        reason: format!("poll docker exec failed: {error}"),
                    });
                }
            }
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| self.unavailable("exec stdout reader panicked"))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| self.unavailable("exec stderr reader panicked"))?;
        if !cancelled {
            if let Some(error) = stdout.error.as_ref().or(stderr.error.as_ref()) {
                return Err(ExecutionError::CancellationIndeterminate {
                    reason: error.clone(),
                });
            }
        }
        let stdout = stdout.output;
        let stderr = stderr.output;
        let mut stdout_decoded = decode_process_output(stdout.bytes.as_slice());
        let mut stderr_decoded = decode_process_output(stderr.bytes.as_slice());
        stdout_decoded.summary.raw_byte_length = stdout.total_bytes;
        stderr_decoded.summary.raw_byte_length = stderr.total_bytes;
        let mut diagnostics = Vec::new();
        for (stream, captured, decoded) in [
            ("stdout", &stdout, &mut stdout_decoded),
            ("stderr", &stderr, &mut stderr_decoded),
        ] {
            if captured.total_bytes > captured.bytes.len() {
                decoded.text.push_str("\n[output truncated at 8 MiB]");
                diagnostics.push(centaeris_core::execution::RuntimeOutputDiagnostic {
                    source: "execution_host".into(), stream: stream.into(), severity: "warning".into(),
                    code: "output_truncated".into(), message: "Output exceeded the per-stream retention limit".into(),
                    details: Some(serde_json::json!({"retainedBytes":captured.bytes.len(), "rawBytes":captured.total_bytes})),
                });
            }
        }
        if timed_out {
            stderr_decoded.text = format!(
                "sandboxed process timed out after {}ms\n{}",
                request.timeout_ms, stderr_decoded.text
            );
        }
        let failure_kind = if cancelled {
            ExecutionHostFailureKind::Cancelled
        } else {
            classify_execution_host_failure(
                exit_code,
                timed_out,
                stdout_decoded.text.as_str(),
                stderr_decoded.text.as_str(),
            )
        };
        Ok(ExecutionHostCommandOutput {
            process: ExecutionProcessOutput {
                exit_code,
                stdout: stdout_decoded.text,
                stderr: stderr_decoded.text,
                stdout_decode: stdout_decoded.summary,
                stderr_decode: stderr_decoded.summary,
                timed_out,
                attempt: ExecutionAttempt {
                    transition_reason: self.oci_runtime.transition_reason().to_string(),
                    policy: policy_summary(&request.policy),
                },
                runtime_diagnostics: diagnostics,
            },
            failure_kind,
            input_state_changes,
        })
    }
}

fn coreutils_timeout_duration(timeout_ms: u64) -> String {
    format!("{}.{:03}s", timeout_ms / 1_000, timeout_ms % 1_000)
}

fn authorized_plugin_path(
    plugin_mounts: &[PluginMount],
    model_path: &str,
) -> Result<bool, ExecutionFileSystemError> {
    if !model_path.starts_with(&format!("{PLUGIN_CONTAINER_ROOT}/")) {
        return Ok(false);
    }
    if model_path.contains('\\')
        || model_path.chars().any(char::is_control)
        || model_path
            .split('/')
            .any(|component| matches!(component, "." | ".."))
    {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "Plugin path is invalid",
        ));
    }
    if plugin_mounts.iter().any(|mount| {
        model_path == mount.destination
            || model_path.starts_with(&format!("{}/", mount.destination))
    }) {
        return Ok(true);
    }
    Err(ExecutionFileSystemError::new(
        ExecutionFileSystemErrorKind::PermissionDenied,
        "Plugin package is not activated for this AgentRun",
    ))
}

fn authorized_system_skill_path(model_path: &str) -> Result<bool, ExecutionFileSystemError> {
    if model_path == SYSTEM_SKILL_CONTAINER_ROOT {
        return Ok(true);
    }
    let Some(relative) = model_path.strip_prefix(&format!("{SYSTEM_SKILL_CONTAINER_ROOT}/")) else {
        return Ok(false);
    };
    if model_path.contains('\\')
        || model_path.chars().any(char::is_control)
        || relative
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "System Skill path is invalid",
        ));
    }
    Ok(true)
}

fn ensure_container(
    expected: &ContainerExpectation<'_>,
    has_execution_fact: bool,
) -> Result<(), String> {
    crate::observations::timed("sandboxEnsure", || {
        ensure_container_inner(expected, has_execution_fact)
    })
}

fn ensure_container_inner(
    expected: &ContainerExpectation<'_>,
    has_execution_fact: bool,
) -> Result<(), String> {
    if let Some(facts) = inspect_container(expected.name)? {
        if has_execution_fact {
            if !facts.has_identity(expected.agent_run_id, expected.execution_id) {
                return Err("sandbox container identity mismatch; refusing replacement".to_string());
            }
            if !facts.matches_configuration(expected) {
                return Err(
                    "sandbox container configuration mismatch; refusing replacement".to_string(),
                );
            }
            if !facts.running {
                return Err("execution_environment_lost:container_stopped".to_string());
            }
            if workspace_execution_sentinel_matches(
                expected.name,
                expected.agent_run_id,
                expected.execution_id,
                expected.authorization_digest,
            )? {
                return Ok(());
            }
            return Err("sandbox workspace identity mismatch; refusing replacement".to_string());
        }
        if !facts.has_identity(expected.agent_run_id, expected.execution_id) {
            return Err("sandbox container identity mismatch; refusing replacement".to_string());
        }
        if !facts.matches_configuration(expected) {
            return Err(
                "sandbox container configuration mismatch; refusing replacement".to_string(),
            );
        }
    } else if has_execution_fact {
        return Err("execution_environment_lost:container_missing".to_string());
    }

    let id = crate::docker_engine::ensure_created(expected.name, container_create_body(expected)?)?;
    let prepared = (|| {
        crate::docker_engine::start(&id)?;
        let facts =
            inspect_container(&id)?.ok_or("sandbox container disappeared after creation")?;
        if !facts.matches(expected) {
            return Err("sandbox container creation verification failed".to_string());
        }
        write_workspace_execution_sentinel(
            &id,
            expected.agent_run_id,
            expected.execution_id,
            expected.authorization_digest,
        )
    })();
    if let Err(error) = prepared {
        // No Execution fact or user tool dispatch exists at this preparation
        // boundary. Remove only the verified immutable container we prepared.
        let cleanup = crate::docker_engine::remove(&id, false);
        return Err(match cleanup {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; sandbox preparation cleanup failed: {cleanup}"),
        });
    }
    Ok(())
}

fn container_create_body(expected: &ContainerExpectation<'_>) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let cpu_quota = u64::from(expected.resources.cpu_milli)
        .checked_mul(100)
        .ok_or("sandbox CPU quota overflow")?;
    let mut mounts = expected
        .plugin_mounts
        .iter()
        .map(|mount| {
            json!({
                "Type":"volume", "Source":expected.plugin_volume_name, "Target":mount.destination,
                "ReadOnly":true, "VolumeOptions":{"Subpath":mount.package_name},
            })
        })
        .collect::<Vec<_>>();
    mounts.push(
        json!({"Type":"volume", "Source":expected.memory_mount.volume_name,
        "Target":MEMORY_CONTAINER_ROOT, "ReadOnly":false,
        "VolumeOptions":{"Subpath":expected.memory_mount.scope_key}}),
    );
    Ok(json!({
        "Image":expected.image_digest,
        "Labels":{"centaeris.managed":"true", "centaeris.agent_run_id":expected.agent_run_id,
            "centaeris.execution_id":expected.execution_id},
        "HostConfig":{
            "Runtime":expected.oci_runtime.docker_runtime_name(), "NetworkMode":SANDBOX_NETWORK_MODE,
            "ReadonlyRootfs":true, "CapDrop":["ALL"], "CapAdd":["CHOWN"],
            "SecurityOpt":["no-new-privileges"], "Memory":expected.resources.memory_bytes,
            "CpuPeriod":100000, "CpuQuota":cpu_quota, "PidsLimit":expected.resources.pids_limit,
            "Tmpfs":{
                "/mnt/data":format!("rw,exec,nosuid,nodev,size={},uid=0,gid=0,mode=1777",expected.resources.data_tmpfs_bytes),
                "/tmp":"rw,exec,nosuid,nodev,size=268435456,mode=1777",
                "/home/agent":"rw,nosuid,nodev,size=134217728,uid=10001,gid=10001,mode=0700",
                "/run/centaeris":"rw,noexec,nosuid,nodev,size=8388608,uid=0,gid=0,mode=0700"
            }, "Mounts":mounts
        }
    }))
}

fn container_name(execution_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(execution_id.as_bytes()));
    format!("centaeris-agent-run-{}", &digest[..24])
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceExecutionSentinel {
    agent_run_id: String,
    execution_id: String,
    authorization_digest: String,
}

fn expected_workspace_execution_sentinel(
    agent_run_id: &str,
    execution_id: &str,
    authorization_digest: &str,
) -> WorkspaceExecutionSentinel {
    WorkspaceExecutionSentinel {
        agent_run_id: agent_run_id.to_string(),
        execution_id: execution_id.to_string(),
        authorization_digest: authorization_digest.to_string(),
    }
}

fn write_workspace_execution_sentinel(
    name: &str,
    agent_run_id: &str,
    execution_id: &str,
    authorization_digest: &str,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&expected_workspace_execution_sentinel(
        agent_run_id,
        execution_id,
        authorization_digest,
    ))
    .map_err(|error| format!("encode workspace execution sentinel failed: {error}"))?;
    let mut request = ExecRequest::owned(
        name,
        agent_run_id,
        execution_id,
        vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("umask 077; cat > {WORKSPACE_EXECUTION_SENTINEL}"),
        ],
    );
    request.user = Some("0:0".into());
    let output = exec_with_input(&request, &bytes, DOCKER_DIAGNOSTIC_LIMIT)?;
    if !output.status.success() {
        return Err(format!(
            "write workspace execution sentinel failed: {}",
            bounded_diagnostic(output.stderr.as_slice())
        ));
    }
    Ok(())
}

fn workspace_execution_sentinel_matches(
    name: &str,
    agent_run_id: &str,
    execution_id: &str,
    authorization_digest: &str,
) -> Result<bool, String> {
    let mut request = ExecRequest::owned(
        name,
        agent_run_id,
        execution_id,
        vec!["cat".into(), WORKSPACE_EXECUTION_SENTINEL.into()],
    );
    request.user = Some("0:0".into());
    let output = exec_with_input(&request, &[], DOCKER_DIAGNOSTIC_LIMIT)?;
    if !output.status.success() {
        let diagnostic = bounded_diagnostic(output.stderr.as_slice());
        if diagnostic.to_ascii_lowercase().contains("no such file") {
            return Ok(false);
        }
        return Err(format!(
            "read workspace execution sentinel failed: {diagnostic}"
        ));
    }
    let actual = serde_json::from_slice::<WorkspaceExecutionSentinel>(output.stdout.as_slice())
        .map_err(|error| format!("decode workspace execution sentinel failed: {error}"))?;
    Ok(actual
        == expected_workspace_execution_sentinel(agent_run_id, execution_id, authorization_digest))
}

fn container_ids_for_agent_run(agent_run_id: &str) -> Result<Vec<String>, String> {
    crate::docker_engine::list_owned(agent_run_id)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerFacts {
    id: String,
    #[serde(skip)]
    raw: serde_json::Value,
    running: bool,
    image: String,
    runtime: String,
    labels: HashMap<String, String>,
    memory: u64,
    cpu_period: i64,
    cpu_quota: i64,
    pids_limit: Option<i64>,
    readonly_rootfs: bool,
    network_mode: String,
    mounts: Vec<ContainerMountFacts>,
}

#[derive(Deserialize)]
struct ContainerMountFacts {
    #[serde(rename = "Type")]
    mount_type: String,
    #[serde(rename = "Source")]
    source: String,
    #[serde(rename = "Target")]
    target: String,
    #[serde(rename = "ReadOnly", default)]
    read_only: bool,
    #[serde(rename = "VolumeOptions")]
    volume_options: ContainerVolumeOptions,
}

#[derive(Deserialize)]
struct ContainerVolumeOptions {
    #[serde(rename = "Subpath")]
    subpath: String,
}

impl ContainerFacts {
    fn has_agent_run_identity(&self, agent_run_id: &str) -> bool {
        self.labels.get("centaeris.managed").map(String::as_str) == Some("true")
            && self
                .labels
                .get("centaeris.agent_run_id")
                .map(String::as_str)
                == Some(agent_run_id)
    }

    fn has_identity(&self, agent_run_id: &str, execution_id: &str) -> bool {
        self.has_agent_run_identity(agent_run_id)
            && self
                .labels
                .get("centaeris.execution_id")
                .map(String::as_str)
                == Some(execution_id)
    }

    fn matches(&self, expected: &ContainerExpectation<'_>) -> bool {
        self.running && self.matches_configuration(expected)
    }

    fn matches_configuration(&self, expected: &ContainerExpectation<'_>) -> bool {
        container_create_body(expected)
            .is_ok_and(|body| crate::docker_engine::verify_created(&self.raw, &body).is_ok())
            && self.has_identity(expected.agent_run_id, expected.execution_id)
            && self.image == expected.image_digest
            && self.runtime == expected.oci_runtime.docker_runtime_name()
            && self.memory == expected.resources.memory_bytes
            && self.cpu_period == 100_000
            && self.cpu_quota == i64::from(expected.resources.cpu_milli) * 100
            && self.pids_limit == Some(i64::from(expected.resources.pids_limit))
            && self.readonly_rootfs
            && self.network_mode == SANDBOX_NETWORK_MODE
            && plugin_mounts_match(
                self.mounts.as_slice(),
                expected.plugin_volume_name,
                expected.plugin_mounts,
            )
            && memory_mount_matches(self.mounts.as_slice(), expected.memory_mount)
    }
}

fn plugin_mounts_match(
    actual: &[ContainerMountFacts],
    plugin_volume_name: &str,
    expected: &[PluginMount],
) -> bool {
    let actual = actual
        .iter()
        .filter(|mount| {
            mount
                .target
                .starts_with(&format!("{PLUGIN_CONTAINER_ROOT}/"))
        })
        .collect::<Vec<_>>();
    actual.len() == expected.len()
        && expected.iter().all(|expected| {
            actual.iter().any(|mount| {
                mount.mount_type == "volume"
                    && mount.source == plugin_volume_name
                    && mount.read_only
                    && mount.target == expected.destination
                    && mount.volume_options.subpath == expected.package_name
            })
        })
}

fn memory_mount_matches(actual: &[ContainerMountFacts], expected: &MemoryMount) -> bool {
    let mounts = actual
        .iter()
        .filter(|mount| mount.target == MEMORY_CONTAINER_ROOT)
        .collect::<Vec<_>>();
    mounts.len() == 1
        && mounts[0].mount_type == "volume"
        && mounts[0].source == expected.volume_name
        && !mounts[0].read_only
        && mounts[0].volume_options.subpath == expected.scope_key
}

fn inspect_container(name: &str) -> Result<Option<ContainerFacts>, String> {
    crate::docker_engine::inspect(name)?.map(|facts| {
        let host = &facts["HostConfig"];
        let mut parsed: ContainerFacts = serde_json::from_value(serde_json::json!({
            "Id":facts["Id"], "Running":facts["State"]["Running"], "Image":facts["Image"],
            "Runtime":host["Runtime"], "Labels":facts["Config"]["Labels"],
            "Memory":host["Memory"], "CpuPeriod":host["CpuPeriod"], "CpuQuota":host["CpuQuota"],
            "PidsLimit":host["PidsLimit"], "ReadonlyRootfs":host["ReadonlyRootfs"],
            "NetworkMode":host["NetworkMode"], "Mounts":host["Mounts"].as_array().cloned().unwrap_or_default(),
        })).map_err(|error| format!("decode sandbox container facts failed: {error}"))?;
        parsed.raw = facts;
        Ok(parsed)
    }).transpose()
}

struct ExecOutput {
    status: crate::docker_engine::ExecStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn filesystem_response_limit(
    operation: &ExecutionFileSystemOperation,
) -> Result<usize, ExecutionFileSystemError> {
    match operation {
        ExecutionFileSystemOperation::ReadFile { max_bytes } => {
            // serde_json encodes Vec<u8> as decimal numbers: at most "255,"
            // per input byte. Keep the normal budget for identity/error metadata.
            // The helper still enforces Core's requested raw-file byte limit.
            max_bytes
                .checked_mul(4)
                .and_then(|bytes| bytes.checked_add(HELPER_JSON_LIMIT))
                .ok_or_else(|| {
                    ExecutionFileSystemError::new(
                        ExecutionFileSystemErrorKind::TooLarge,
                        "filesystem response budget overflow",
                    )
                })
        }
        _ => Ok(HELPER_JSON_LIMIT),
    }
}

fn exec_with_input(
    request: &ExecRequest,
    input: &[u8],
    output_limit: usize,
) -> Result<ExecOutput, String> {
    let mut child = request.spawn()?;
    let stdout = child.stdout.take().ok_or("exec stdout missing")?;
    let stderr = child.stderr.take().ok_or("exec stderr missing")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, output_limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, DOCKER_DIAGNOSTIC_LIMIT));
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).map_err(|e| e.to_string())?;
    }
    let status = child.wait()?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| "exec stdout reader panicked")??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "exec stderr reader panicked")??;
    if stdout.total_bytes > output_limit {
        return Err("exec helper exceeded output limit".into());
    }
    Ok(ExecOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

pub(crate) fn bounded_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(DOCKER_DIAGNOSTIC_LIMIT)])
        .trim()
        .to_string()
}

fn helper_failure(mode: &str, exit_code: Option<i32>, stderr: &[u8]) -> String {
    let code = exit_code
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".into());
    format!(
        "sandbox helper {mode} failed (exitCode={code}): {}",
        bounded_diagnostic(stderr)
    )
}

fn policy_summary(policy: &ExecutionPolicy) -> ExecutionPolicySummary {
    ExecutionPolicySummary {
        enforced: true,
        network: policy.network.clone(),
        workspace_root: WORKSPACE_DATA_ROOT.to_string(),
        read_only_root_count: policy.filesystem.read_only_roots.len(),
        writable_root_count: policy.filesystem.writable_roots.len(),
        denied_read_path_count: policy.filesystem.denied_read_paths.len(),
        denied_write_path_count: policy.filesystem.denied_write_paths.len(),
    }
}

struct BoundedRead {
    bytes: Vec<u8>,
    total_bytes: usize,
}

#[cfg(test)]
fn read_all(mut reader: impl Read) -> Result<BoundedRead, String> {
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read docker exec output failed: {error}"))?;
    let total_bytes = bytes.len();
    Ok(BoundedRead { bytes, total_bytes })
}

struct CapturedOutput {
    output: BoundedRead,
    error: Option<String>,
}

fn capture_command_output(mut reader: impl Read, limit: usize) -> CapturedOutput {
    let mut output = BoundedRead {
        bytes: Vec::new(),
        total_bytes: 0,
    };
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                return CapturedOutput {
                    output,
                    error: None,
                };
            }
            Ok(count) => {
                output.total_bytes = output.total_bytes.saturating_add(count);
                let keep = count.min(limit.saturating_sub(output.bytes.len()));
                output.bytes.extend_from_slice(&buffer[..keep]);
            }
            Err(error) => {
                return CapturedOutput {
                    output,
                    error: Some(error.to_string()),
                };
            }
        }
    }
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<BoundedRead, String> {
    let mut bytes = Vec::new();
    let mut total_bytes = 0usize;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read docker exec output failed: {error}"))?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read);
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(BoundedRead { bytes, total_bytes })
}

#[cfg(test)]
fn encode_frame<T: Serialize>(metadata: &T, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let metadata = serde_json::to_vec(metadata)
        .map_err(|error| format!("encode sandbox helper frame failed: {error}"))?;
    let metadata_len = u32::try_from(metadata.len())
        .map_err(|_| "sandbox helper frame metadata is too large".to_string())?;
    let mut frame = Vec::with_capacity(4 + metadata.len() + bytes.len());
    frame.extend_from_slice(&metadata_len.to_be_bytes());
    frame.extend_from_slice(metadata.as_slice());
    frame.extend_from_slice(bytes);
    Ok(frame)
}

fn decode_frame<T: serde::de::DeserializeOwned>(frame: &[u8]) -> Result<(T, &[u8]), String> {
    if frame.len() < 4 {
        return Err("sandbox helper frame is truncated".to_string());
    }
    let metadata_len = u32::from_be_bytes(
        frame[..4]
            .try_into()
            .map_err(|_| "sandbox helper frame is invalid".to_string())?,
    ) as usize;
    if metadata_len == 0 || metadata_len > HELPER_JSON_LIMIT || frame.len() < 4 + metadata_len {
        return Err("sandbox helper frame metadata is invalid".to_string());
    }
    let metadata = serde_json::from_slice(&frame[4..4 + metadata_len])
        .map_err(|error| format!("decode sandbox helper frame failed: {error}"))?;
    Ok((metadata, &frame[4 + metadata_len..]))
}

fn normalize_model_path(
    path: &str,
    operation: &ExecutionFileSystemOperation,
) -> Result<String, ExecutionFileSystemError> {
    if matches!(
        operation,
        ExecutionFileSystemOperation::ListDirectory { .. }
    ) && matches!(path, "." | WORKSPACE_DATA_ROOT)
    {
        return Ok(".".to_string());
    }
    let path = path.strip_prefix("/mnt/data/").unwrap_or(path);
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::InvalidPath,
            "sandbox path is invalid",
        ));
    }
    Ok(path.to_string())
}

fn filesystem_unavailable(message: impl Into<String>) -> ExecutionFileSystemError {
    ExecutionFileSystemError::new(ExecutionFileSystemErrorKind::HostUnavailable, message)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionWorkspaceLeaseRequest<'a> {
    schema: &'a str,
    job_id: &'a str,
    lease_owner: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
}

impl<'a> SessionWorkspaceLeaseRequest<'a> {
    fn new(
        schema: &'a str,
        runner: &'a DockerExecutionHostRunner,
        lease: &'a SessionWorkspaceLease,
    ) -> Self {
        Self {
            schema,
            job_id: lease.job_id.as_str(),
            lease_owner: lease.lease_owner.as_str(),
            agent_run_id: runner.agent_run_id.as_str(),
            authorization_digest: runner.authorization_digest.as_str(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionWorkspaceCommitRequest<'a> {
    schema: &'a str,
    job_id: &'a str,
    lease_owner: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    snapshot_sha256: &'a str,
    snapshot_size_bytes: u64,
    expanded_size_bytes: u64,
    file_count: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionWorkspaceStageRequest<'a> {
    schema: &'a str,
    job_id: &'a str,
    lease_owner: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    checkpoint_id: &'a str,
    snapshot_sha256: &'a str,
    snapshot_size_bytes: u64,
    expanded_size_bytes: u64,
    file_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutionWorkspaceStageResponse {
    schema: String,
    object_ref: Option<String>,
    snapshot_sha256: String,
    snapshot_size_bytes: u64,
    expanded_size_bytes: u64,
    file_count: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionWorkspaceDownloadRequest<'a> {
    schema: &'a str,
    job_id: &'a str,
    lease_owner: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    checkpoint_id: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionWorkspaceResolveResponse {
    schema: String,
    disposition: String,
    session_workspace: SessionWorkspace,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionWorkspaceCommitResponse {
    schema: String,
    disposition: String,
    session_workspace: SessionWorkspace,
}

#[derive(Clone)]
struct WorkspaceSnapshotDescriptor {
    sha256: String,
    size_bytes: u64,
    expanded_size_bytes: u64,
    file_count: u32,
}

struct ImmutableWorkspaceSnapshot {
    file: fs::File,
    descriptor: WorkspaceSnapshotDescriptor,
}

pub(crate) struct PreparedRecoveryWorkspace {
    content: PreparedRecoveryContent,
    evidence: WorkspaceReuseEvidence,
}

impl PreparedRecoveryWorkspace {
    pub(crate) fn evidence(&self) -> WorkspaceReuseEvidence {
        self.evidence
    }
}

enum PreparedRecoveryContent {
    Unchanged(RecoveryWorkspaceSnapshotV1),
    Upload {
        artifact: ImmutableWorkspaceSnapshot,
        lease: SessionWorkspaceLease,
        checkpoint_id: String,
    },
}

struct SnapshotSpoolReader<'a, R> {
    source: &'a mut R,
    file: &'a mut fs::File,
    remaining: u64,
}

impl<R: Read> Read for SnapshotSpoolReader<'_, R> {
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

fn capture_snapshot_artifact(
    source: &mut impl Read,
    limit: u64,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    let mut file =
        tempfile::tempfile().map_err(|error| format!("create snapshot spool failed: {error}"))?;
    let descriptor = inspect_workspace_snapshot(&mut SnapshotSpoolReader {
        source,
        file: &mut file,
        remaining: limit,
    })?;
    file.rewind()
        .map_err(|error| format!("rewind snapshot spool failed: {error}"))?;
    Ok(ImmutableWorkspaceSnapshot { file, descriptor })
}

pub(crate) fn resolve_workspace_image_digest() -> Result<String, String> {
    let image = env::var(WORKSPACE_GENERAL_IMAGE_ENV)
        .map_err(|_| format!("{WORKSPACE_GENERAL_IMAGE_ENV} is required"))?;
    if image.is_empty()
        || image.len() > 512
        || image.chars().any(|character| {
            character.is_control() || character.is_whitespace() || !character.is_ascii()
        })
    {
        return Err(format!(
            "{WORKSPACE_GENERAL_IMAGE_ENV} is not a canonical Docker image reference"
        ));
    }
    let digest = crate::docker_engine::image_id(&image)?;
    parse_docker_image_digest(digest.as_bytes())
}

fn parse_docker_image_digest(output: &[u8]) -> Result<String, String> {
    let raw = std::str::from_utf8(output)
        .map_err(|_| "Docker execution image digest is not UTF-8".to_string())?;
    let digest = raw.strip_suffix('\n').unwrap_or(raw);
    let digest = digest.strip_suffix('\r').unwrap_or(digest);
    let valid = digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if !valid {
        return Err("Docker execution image digest is invalid".to_string());
    }
    Ok(digest.to_string())
}

struct SessionWorkspaceUploadBody {
    prefix: Cursor<Vec<u8>>,
    snapshot: fs::File,
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

fn inspect_workspace_snapshot(
    source: &mut impl Read,
) -> Result<WorkspaceSnapshotDescriptor, String> {
    let mut header = [0_u8; 4];
    let first = source
        .read(&mut header)
        .map_err(|error| format!("read snapshot collect frame failed: {error}"))?;
    if first == 0 {
        return Ok(WorkspaceSnapshotDescriptor {
            sha256: String::new(),
            size_bytes: 0,
            expanded_size_bytes: 0,
            file_count: 0,
        });
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
    Ok(WorkspaceSnapshotDescriptor {
        sha256: format!("sha256:{:x}", digest.finalize()),
        size_bytes,
        expanded_size_bytes,
        file_count: u32::try_from(manifest.files.len())
            .map_err(|_| "snapshot file count overflow".to_string())?,
    })
}

fn snapshot_matches_frozen_workspace(
    descriptor: &WorkspaceSnapshotDescriptor,
    frozen: &SessionWorkspace,
) -> bool {
    descriptor.sha256 == frozen.snapshot_sha256
        && descriptor.size_bytes == frozen.snapshot_size_bytes
        && descriptor.expanded_size_bytes == frozen.expanded_size_bytes
        && descriptor.file_count == frozen.file_count
}

fn snapshot_matches_recovery_workspace(
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

fn validate_workspace_snapshot_manifest(
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

fn copy_exact(
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidateInputsRequest<'a> {
    schema: &'static str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    inputs: &'a [ProjectedInputValidation<'a>],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectedInputValidation<'a> {
    input_ref: &'a str,
    virtual_path: &'a str,
    size_bytes: u64,
    sha256: &'a str,
    source_version: &'a str,
}

impl<'a> From<&'a SandboxMaterializedInput> for ProjectedInputValidation<'a> {
    fn from(input: &'a SandboxMaterializedInput) -> Self {
        Self {
            input_ref: input.input_ref.as_str(),
            virtual_path: input.virtual_path.as_str(),
            size_bytes: input.size_bytes,
            sha256: input.sha256.as_str(),
            source_version: input.source_version.as_str(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ValidateInputsResponse {
    schema: String,
    inputs: Vec<ProjectedInputValidationResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectedInputValidationResult {
    input_ref: String,
    state: ProjectedInputState,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProjectedInputState {
    Active,
    AssetRemoved,
    AccessRevoked,
    SourceDeleted,
    StaleGeneration,
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_capture_bounds_retention_and_keeps_failure_evidence() {
        let captured = super::capture_command_output(&b"0123456789"[..], 4);
        assert_eq!(captured.output.bytes, b"0123");
        assert_eq!(captured.output.total_bytes, 10);
        assert!(captured.error.is_none());
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("transport lost"))
            }
        }
        let stream = std::io::Read::chain(&b"partial"[..], Broken);
        let captured = super::capture_command_output(stream, 64);
        assert_eq!(captured.output.bytes, b"partial");
        assert_eq!(captured.error.as_deref(), Some("transport lost"));
    }
    use super::*;
    use centaeris_core::execution::{
        ExecutionFileIdentity, ExecutionFileReadOutput, ExecutionHostBinding, ExecutionHostMode,
    };
    use centaeris_core::extension::skills::SkillCatalogLoadConfig;
    use centaeris_core::tool::inputs::{
        ResolvedInputManifest, ResolvedInputState, RESOLVED_INPUT_MANIFEST_SCHEMA,
    };
    use centaeris_core::tool::layer::{ToolInvocationRequest, ToolLayer};
    use std::path::PathBuf;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn docker_image_digest_parser_accepts_only_exact_lowercase_sha256() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            parse_docker_image_digest(format!("{digest}\n").as_bytes()).expect("digest"),
            digest
        );
        assert!(
            parse_docker_image_digest(format!("sha256:{}\n", "A".repeat(64)).as_bytes()).is_err()
        );
        assert!(
            parse_docker_image_digest(format!(" sha256:{}\n", "a".repeat(64)).as_bytes()).is_err()
        );
        assert!(parse_docker_image_digest(
            format!("sha256:{}\nextra\n", "a".repeat(64)).as_bytes()
        )
        .is_err());
    }

    #[test]
    fn container_names_do_not_embed_execution_ids() {
        let name = container_name("execution/with unsafe value");
        assert!(name.starts_with("centaeris-agent-run-"));
        assert_eq!(name.len(), "centaeris-agent-run-".len() + 24);
        assert!(!name.contains('/'));
    }

    #[test]
    fn docker_volume_names_reject_mount_option_injection() {
        assert!(
            validate_docker_volume_name("PLUGIN_VOLUME_NAME", "centaeris-e2e-plugin-data").is_ok()
        );
        assert!(validate_docker_volume_name("PLUGIN_VOLUME_NAME", "banana,ro").is_err());
    }

    #[cfg(windows)]
    fn generation_rpc_test_command(response: &str) -> Command {
        let request = String::from_utf8_lossy(SANDBOX_WORKSPACE_GENERATION_QUERY_LINE)
            .trim_end()
            .replace('\'', "''");
        let response = response.replace('\'', "''");
        let script = format!("$expected='{request}';$response='{response}';")
            + "while (($line=[Console]::In.ReadLine()) -ne $null) {"
            + "if ($line -ne $expected) { exit 2 };"
            + "[Console]::Out.Write($response+[char]10);[Console]::Out.Flush()}";
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()]);
        command
    }

    #[cfg(unix)]
    fn generation_rpc_test_command(response: &str) -> Command {
        let request = String::from_utf8_lossy(SANDBOX_WORKSPACE_GENERATION_QUERY_LINE);
        let script = format!(
            "while IFS= read -r line; do [ \"$line\" = '{}' ] || exit 2; printf '%s\\n' '{}'; done",
            request.trim_end(),
            response
        );
        let mut command = Command::new("sh");
        command.args(["-c", script.as_str()]);
        command
    }

    fn generation_rpc_response(epoch: &str, generation: u64) -> String {
        serde_json::json!({
            "schema": "workspace.generation.v1",
            "generation": {
                "instanceEpoch": epoch,
                "generation": generation,
            },
            "diagnostic": null,
        })
        .to_string()
    }

    #[cfg(windows)]
    fn generation_rpc_stalled_test_command(partial: bool) -> Command {
        let output = if partial { "{\"schema\":" } else { "" };
        let script = format!("$response='{output}';")
            + "while (($line=[Console]::In.ReadLine()) -ne $null) {"
            + "[Console]::Out.Write($response);[Console]::Out.Flush()}";
        let mut command = Command::new("powershell");
        command.args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()]);
        command
    }

    #[cfg(unix)]
    fn generation_rpc_stalled_test_command(partial: bool) -> Command {
        let output = if partial { "{\"schema\":" } else { "" };
        let script = format!("while IFS= read -r line; do printf '%s' '{output}'; done");
        let mut command = Command::new("sh");
        command.args(["-c", script.as_str()]);
        command
    }

    #[test]
    fn workspace_generation_rpc_reuses_one_process_for_42_queries_with_warm_metrics() {
        let mut command =
            generation_rpc_test_command(generation_rpc_response("watcher-test", 7).as_str());
        assert_generation_rpc_42_query_metrics(&mut command, "local_helper");
    }

    #[test]
    #[ignore = "requires CENTAERIS_GENERATION_BENCH_CONTAINER with an idle workspace watcher"]
    fn workspace_generation_rpc_docker_42_query_benchmark() {
        let container = std::env::var("CENTAERIS_GENERATION_BENCH_CONTAINER")
            .expect("Docker generation benchmark container");
        let cold_started = Instant::now();
        let mut request = ExecRequest::new(
            &container,
            vec![AGENT_BINARY.into(), "workspace-generation-rpc".into()],
        );
        request.cwd = Some(WORKSPACE_DATA_ROOT.into());
        let rpc =
            WorkspaceGenerationRpc::from_exec(request.spawn().expect("start Engine RPC")).unwrap();
        assert_generation_rpc_metrics(rpc, cold_started, "docker_engine");
    }

    fn assert_generation_rpc_42_query_metrics(command: &mut Command, transport: &str) {
        let cold_started = Instant::now();
        let rpc = WorkspaceGenerationRpc::spawn(command).expect("start test RPC");
        assert_generation_rpc_metrics(rpc, cold_started, transport);
    }

    fn assert_generation_rpc_metrics(
        mut rpc: WorkspaceGenerationRpc,
        cold_started: Instant,
        transport: &str,
    ) {
        let process_id = rpc.child.id();
        let mut durations = Vec::new();
        let mut expected_generation = None;
        for index in 0..42 {
            let started = if index == 0 {
                cold_started
            } else {
                Instant::now()
            };
            let response = rpc
                .query(WORKSPACE_GENERATION_RPC_RESPONSE_TIMEOUT)
                .expect("query persistent RPC");
            durations.push(started.elapsed());
            assert_eq!(rpc.child.id(), process_id);
            let generation = response.generation.expect("known generation");
            if let Some(expected) = expected_generation.as_ref() {
                assert_eq!(&generation, expected);
            } else {
                expected_generation = Some(generation);
            }
        }
        drop(rpc);
        let total_ms = cold_started.elapsed().as_secs_f64() * 1_000.0;
        let cold_ms = durations[0].as_secs_f64() * 1_000.0;
        let mut warm_ms = durations[1..]
            .iter()
            .map(|duration| duration.as_secs_f64() * 1_000.0)
            .collect::<Vec<_>>();
        warm_ms.sort_by(f64::total_cmp);
        let warm_p50_ms = warm_ms[warm_ms.len() / 2];
        let warm_p95_ms = warm_ms[(warm_ms.len() * 95 / 100).min(warm_ms.len() - 1)];
        eprintln!(
            "WORKSPACE_02_GENERATION_RPC transport={transport} queries=42 processes=1 coldMs={cold_ms:.3} warmP50Ms={warm_p50_ms:.3} warmP95Ms={warm_p95_ms:.3} totalMs={total_ms:.3}"
        );
    }

    #[test]
    fn workspace_generation_rpc_failure_clears_process_and_next_query_rebuilds() {
        let mut slot = None;
        let mut invalid = Some(generation_rpc_test_command(
            generation_rpc_response("", 1).as_str(),
        ));
        let error = query_workspace_generation_rpc(
            &mut slot,
            WORKSPACE_GENERATION_RPC_RESPONSE_TIMEOUT,
            || WorkspaceGenerationRpc::spawn(invalid.as_mut().expect("invalid command")),
        )
        .expect_err("invalid epoch must fail");
        assert!(error.contains("execution_workspace_generation_epoch_invalid"));
        assert!(slot.is_none());

        let mut valid = Some(generation_rpc_test_command(
            generation_rpc_response("watcher-rebuilt", 2).as_str(),
        ));
        let response = query_workspace_generation_rpc(
            &mut slot,
            WORKSPACE_GENERATION_RPC_RESPONSE_TIMEOUT,
            || WorkspaceGenerationRpc::spawn(valid.as_mut().expect("valid command")),
        )
        .expect("next query rebuilds RPC");
        assert_eq!(response.generation.expect("known generation").generation, 2);
        assert!(slot.is_some());
    }

    #[test]
    fn workspace_generation_rpc_no_reply_or_partial_line_times_out_and_rebuilds() {
        for partial in [false, true] {
            let mut slot = None;
            let mut stalled = generation_rpc_stalled_test_command(partial);
            let started = Instant::now();
            let error = query_workspace_generation_rpc(&mut slot, Duration::from_secs(2), || {
                WorkspaceGenerationRpc::spawn(&mut stalled)
            })
            .expect_err("incomplete response must hit the host deadline");
            assert!(error.contains("workspace generation RPC response timed out"));
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(slot.is_none());

            let mut valid = generation_rpc_test_command(
                generation_rpc_response("watcher-after-timeout", 3).as_str(),
            );
            let response = query_workspace_generation_rpc(
                &mut slot,
                WORKSPACE_GENERATION_RPC_RESPONSE_TIMEOUT,
                || WorkspaceGenerationRpc::spawn(&mut valid),
            )
            .expect("query after timeout rebuilds the process");
            assert_eq!(response.generation.expect("known generation").generation, 3);
        }
    }

    #[test]
    fn workspace_sentinel_uses_only_durable_execution_identity() {
        let sentinel = expected_workspace_execution_sentinel(
            "agent_run_test",
            "execution_test",
            &format!("sha256:{}", "a".repeat(64)),
        );
        assert_eq!(
            serde_json::to_value(sentinel).expect("sentinel"),
            serde_json::json!({
                "agentRunId": "agent_run_test",
                "executionId": "execution_test",
                "authorizationDigest": format!("sha256:{}", "a".repeat(64)),
            })
        );
    }

    #[test]
    fn oci_runtime_runc_maps_to_oci_container_and_docker_runc() {
        let runtime = OciRuntime::parse("runc").expect("runc");
        assert_eq!(runtime.docker_runtime_name(), "runc");
        assert_eq!(runtime.transition_reason(), "docker_runc");
    }

    #[test]
    fn oci_runtime_runsc_maps_to_gvisor_and_docker_runsc() {
        let runtime = OciRuntime::parse("runsc").expect("runsc");
        assert_eq!(runtime.docker_runtime_name(), "runsc");
        assert_eq!(runtime.transition_reason(), "docker_runsc");
    }

    #[test]
    fn oci_runtime_rejects_unknown_empty_and_whitespace_values() {
        for value in [
            "banana",
            "",
            " runc ",
            " runc",
            "runc ",
            "RUNC",
            "runc\n",
            "runc,runsc",
        ] {
            assert!(
                OciRuntime::parse(value).is_err(),
                "OCI runtime {value:?} must be rejected"
            );
        }
    }

    #[test]
    fn policy_summary_preserves_enforcement_without_backend_identity() {
        let policy = ExecutionPolicy::workspace_write_no_network(std::path::PathBuf::from(
            WORKSPACE_DATA_ROOT,
        ));
        let summary = policy_summary(&policy);
        assert!(summary.enforced);
        assert_eq!(summary.workspace_root, WORKSPACE_DATA_ROOT);
        assert_eq!(summary.network, NetworkPolicy::Disabled);
        assert_eq!(SANDBOX_NETWORK_MODE, "none");
    }

    #[test]
    fn docker_timeout_uses_coreutils_seconds() {
        assert_eq!(coreutils_timeout_duration(1), "0.001s");
        assert_eq!(coreutils_timeout_duration(120_000), "120.000s");
    }

    #[test]
    fn docker_command_output_reader_preserves_all_bytes() {
        let expected = vec![b'x'; 1024 * 1024 + 17];
        let output = read_all(Cursor::new(expected.clone())).expect("complete command output");

        assert_eq!(output.bytes, expected);
        assert_eq!(output.total_bytes, 1024 * 1024 + 17);
    }

    #[test]
    fn helper_frames_round_trip_exactly() {
        let metadata = SandboxArtifactMetadata {
            filename: "report.txt".to_string(),
            size_bytes: 3,
            sha256: format!("sha256:{}", "a".repeat(64)),
        };
        let frame = encode_frame(&metadata, b"abc").expect("frame");
        let (decoded, bytes) =
            decode_frame::<SandboxArtifactMetadata>(frame.as_slice()).expect("decode");
        assert_eq!(decoded.filename, "report.txt");
        assert_eq!(bytes, b"abc");
    }

    #[test]
    fn filesystem_read_response_preserves_large_binary_bytes() {
        // A rendered page can be well below Core's file limit while its JSON
        // byte array exceeds the ordinary helper metadata budget.
        let bytes = vec![255; 400_000];
        let expected = ExecutionFileSystemOutput::ReadFile(ExecutionFileReadOutput {
            identity: ExecutionFileIdentity {
                key: "rendered-page".into(),
                display_path: "/mnt/data/page.png".into(),
            },
            file_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
            bytes,
        });
        let encoded = serde_json::to_vec(&SandboxFileSystemResult::from(Ok(expected.clone())))
            .expect("encoded response");
        assert!(encoded.len() > HELPER_JSON_LIMIT);
        let limit = filesystem_response_limit(&ExecutionFileSystemOperation::ReadFile {
            max_bytes: 400_000,
        })
        .expect("response budget");
        let captured = read_bounded(Cursor::new(&encoded), limit).expect("bounded read");
        assert_eq!(
            captured.bytes.len(),
            encoded.len(),
            "valid image response was truncated"
        );
        let decoded = serde_json::from_slice::<SandboxFileSystemResult>(&captured.bytes)
            .expect("complete JSON")
            .into_result()
            .expect("file response");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn filesystem_response_budget_rejects_overflow_and_keeps_metadata_bounded() {
        assert_eq!(
            filesystem_response_limit(&ExecutionFileSystemOperation::ReadFile {
                max_bytes: usize::MAX,
            })
            .expect_err("overflow must fail")
            .kind,
            ExecutionFileSystemErrorKind::TooLarge,
        );
        assert_eq!(
            filesystem_response_limit(&ExecutionFileSystemOperation::InspectMutationPath)
                .expect("metadata budget"),
            HELPER_JSON_LIMIT,
        );
    }

    fn snapshot_test_frame(bytes: &[u8]) -> Vec<u8> {
        let manifest = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.to_string(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "report.txt".to_string(),
                size_bytes: bytes.len() as u64,
                sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                executable: false,
            }],
        };
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let mut frame = (manifest.len() as u32).to_be_bytes().to_vec();
        frame.extend(manifest);
        frame.extend(bytes);
        frame
    }

    #[test]
    fn snapshot_upload_reads_captured_bytes_after_live_workspace_changes() {
        let captured = snapshot_test_frame(b"original");
        let mut live = Cursor::new(captured.clone());
        let artifact = capture_snapshot_artifact(&mut live, captured.len() as u64).unwrap();
        live.get_mut().fill(0);
        live.set_position(0);
        let mut upload = SessionWorkspaceUploadBody {
            prefix: Cursor::new(vec![1, 2]),
            snapshot: artifact.file,
        };
        let mut uploaded = Vec::new();
        upload.read_to_end(&mut uploaded).unwrap();
        assert_eq!(&uploaded[2..], captured.as_slice());
        assert_eq!(live.position(), 0, "upload never reads the workspace again");
        assert!(capture_snapshot_artifact(
            &mut Cursor::new(captured.clone()),
            captured.len() as u64 - 1
        )
        .is_err());
    }

    #[test]
    fn prepared_snapshot_releases_gate_and_requires_original_epoch_on_acceptance() {
        let runner = test_docker_execution_host_runner();
        let frame = snapshot_test_frame(b"original");
        let mut collections = 0;
        let prepared = runner
            .capture_prepared_recovery_workspace_with_observer(
                |_| Ok(0),
                |_| {
                    collections += 1;
                    let artifact = capture_snapshot_artifact(
                        &mut Cursor::new(frame.clone()),
                        frame.len() as u64,
                    )?;
                    Ok(PreparedRecoveryContent::Upload {
                        artifact,
                        lease: SessionWorkspaceLease {
                            job_id: "job".into(),
                            lease_owner: "owner".into(),
                        },
                        checkpoint_id: "checkpoint".into(),
                    })
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(collections, 1);
        let evidence = prepared.evidence();
        assert!(runner.recovery_workspace_evidence().unwrap().is_none());
        runner
            .with_helper_dispatch("filesystem-once", || {
                Ok(runner.recovery_activity.invalidate_before_dispatch()?)
            })
            .unwrap();
        assert!(!runner.accept_captured_recovery_workspace(evidence).unwrap());
        assert!(runner.recovery_workspace_evidence().unwrap().is_none());
        // Dropping an unused prepared checkpoint closes the anonymous spool.
        drop(prepared);
    }

    #[test]
    fn slow_snapshot_upload_does_not_hold_workspace_dispatch_gate() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut runner = test_docker_execution_host_runner();
        runner.api_url = format!("http://{}", listener.local_addr().unwrap());
        runner.api_client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let runner = Arc::new(runner);
        let frame = snapshot_test_frame(b"original");
        let prepared = runner
            .capture_prepared_recovery_workspace_with_observer(
                |_| Ok(0),
                |_| {
                    Ok(PreparedRecoveryContent::Upload {
                        artifact: capture_snapshot_artifact(
                            &mut Cursor::new(frame.clone()),
                            frame.len() as u64,
                        )?,
                        lease: SessionWorkspaceLease {
                            job_id: "job".into(),
                            lease_owner: "owner".into(),
                        },
                        checkpoint_id: "checkpoint".into(),
                    })
                },
            )
            .unwrap()
            .unwrap();
        let (started, observed) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            let header = String::from_utf8(header).unwrap();
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_string)
                })
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            let metadata_length = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
            let metadata: serde_json::Value =
                serde_json::from_slice(&body[4..4 + metadata_length]).unwrap();
            assert_eq!(&body[4 + metadata_length..], frame.as_slice());
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            let response = serde_json::json!({
                "schema": EXECUTION_WORKSPACE_STAGE_RESULT_SCHEMA, "objectRef": "object-1",
                "snapshotSha256": metadata["snapshotSha256"], "snapshotSizeBytes": metadata["snapshotSizeBytes"],
                "expandedSizeBytes": metadata["expandedSizeBytes"], "fileCount": metadata["fileCount"],
            }).to_string();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        });
        let worker = {
            let runner = runner.clone();
            thread::spawn(move || runner.upload_recovery_workspace(prepared))
        };
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let (dispatched, dispatch_observed) = mpsc::channel();
        let dispatcher = {
            let runner = runner.clone();
            thread::spawn(move || {
                runner.with_helper_dispatch("filesystem-once", || {
                    dispatched.send(()).unwrap();
                    Ok(())
                })
            })
        };
        let admitted = dispatch_observed.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        worker.join().unwrap().unwrap();
        dispatcher.join().unwrap().unwrap();
        server.join().unwrap();
        assert!(
            admitted.is_ok(),
            "network upload must not delay workspace dispatch"
        );
    }

    #[test]
    fn immutable_snapshot_capture_is_bounded_and_preserves_collected_bytes() {
        let snapshot = capture_snapshot_artifact(&mut Cursor::new(Vec::<u8>::new()), 8).unwrap();
        assert_eq!(snapshot.descriptor.size_bytes, 0);
        let mut uploaded = Vec::new();
        snapshot.file.take(8).read_to_end(&mut uploaded).unwrap();
        assert!(uploaded.is_empty());
        assert!(capture_snapshot_artifact(&mut Cursor::new(vec![0_u8; 9]), 8).is_err());
    }

    #[test]
    fn workspace_snapshot_stream_requires_canonical_file_hashes() {
        let bytes = b"abc";
        let manifest = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.to_string(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "report.txt".to_string(),
                size_bytes: bytes.len() as u64,
                sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                executable: false,
            }],
        };
        let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest");
        let mut frame = Vec::new();
        frame.extend_from_slice(&(manifest_bytes.len() as u32).to_be_bytes());
        frame.extend_from_slice(manifest_bytes.as_slice());
        frame.extend_from_slice(bytes);
        let descriptor = inspect_workspace_snapshot(&mut Cursor::new(frame.clone()))
            .expect("workspace snapshot");
        assert_eq!(descriptor.expanded_size_bytes, bytes.len() as u64);
        assert_eq!(descriptor.file_count, 1);
        assert_eq!(descriptor.size_bytes, frame.len() as u64);

        let mut corrupt = manifest;
        corrupt.files[0].sha256 = format!("sha256:{}", "b".repeat(64));
        let corrupt_manifest = serde_json::to_vec(&corrupt).expect("corrupt manifest");
        let mut corrupt_frame = Vec::new();
        corrupt_frame.extend_from_slice(&(corrupt_manifest.len() as u32).to_be_bytes());
        corrupt_frame.extend_from_slice(corrupt_manifest.as_slice());
        corrupt_frame.extend_from_slice(bytes);
        assert!(inspect_workspace_snapshot(&mut Cursor::new(corrupt_frame)).is_err());
    }

    #[test]
    fn snapshot_descriptor_match_requires_every_frozen_field() {
        let descriptor = WorkspaceSnapshotDescriptor {
            sha256: format!("sha256:{}", "a".repeat(64)),
            size_bytes: 12,
            expanded_size_bytes: 8,
            file_count: 1,
        };
        let frozen = SessionWorkspace {
            generation: 7,
            snapshot_sha256: descriptor.sha256.clone(),
            snapshot_size_bytes: descriptor.size_bytes,
            expanded_size_bytes: descriptor.expanded_size_bytes,
            file_count: descriptor.file_count,
        };
        assert!(snapshot_matches_frozen_workspace(&descriptor, &frozen));
        let recovery = RecoveryWorkspaceSnapshotV1 {
            object_ref: Some("object-1".to_string()),
            snapshot_sha256: descriptor.sha256.clone(),
            snapshot_size_bytes: descriptor.size_bytes,
            expanded_size_bytes: descriptor.expanded_size_bytes,
            file_count: descriptor.file_count,
        };
        assert!(snapshot_matches_recovery_workspace(&descriptor, &recovery));

        let different = WorkspaceSnapshotDescriptor {
            file_count: 2,
            ..descriptor
        };
        assert!(!snapshot_matches_frozen_workspace(&different, &frozen));
        assert!(!snapshot_matches_recovery_workspace(&different, &recovery));
    }

    #[test]
    fn activated_plugin_mount_is_read_only_and_cli_precedes_base_path() {
        let activation = PluginActivationSnapshotV1 {
            schema: "plugin_activation_snapshot_v1".to_string(),
            digest: format!("sha256:{}", "a".repeat(64)),
            packages: vec![centaeris_core::extension::ActivatedPluginPackageV1 {
                name: "demo".to_string(),
                version: "1.0.0".to_string(),
                package_digest: format!("sha256:{}", "b".repeat(64)),
                skills: Vec::new(),
                cli: vec![centaeris_core::extension::PluginResourceDigestV1 {
                    path: "bin/demo".to_string(),
                    digest: format!("sha256:{}", "c".repeat(64)),
                }],
                mcp_servers: vec![],
                hooks: vec![],
            }],
        };
        let mounts = plugin_mounts(&activation);
        assert_eq!(mounts[0].destination, "/opt/centaeris/plugins/demo");
        assert_eq!(
            plugin_command_path(&activation).expect("plugin PATH"),
            format!("/opt/centaeris/plugins/demo/bin:{BASE_COMMAND_PATH}")
        );
        assert!(plugin_mounts_match(
            &[ContainerMountFacts {
                mount_type: "volume".to_string(),
                source: "centaeris-workspace-plugin-data".to_string(),
                target: mounts[0].destination.clone(),
                read_only: true,
                volume_options: ContainerVolumeOptions {
                    subpath: "demo".to_string(),
                },
            }],
            "centaeris-workspace-plugin-data",
            mounts.as_slice(),
        ));
        assert!(authorized_plugin_path(
            mounts.as_slice(),
            "/opt/centaeris/plugins/demo/skills/demo/SKILL.md",
        )
        .expect("activated path"));
        assert!(authorized_plugin_path(
            mounts.as_slice(),
            "/opt/centaeris/plugins/banana/SKILL.md",
        )
        .is_err());
        assert!(
            !authorized_plugin_path(mounts.as_slice(), "/mnt/data/report.docx")
                .expect("workspace path")
        );
        assert!(
            authorized_system_skill_path("/opt/centaeris/system-skills/memory/SKILL.md")
                .expect("System Skill path")
        );
        assert!(
            authorized_system_skill_path("/opt/centaeris/system-skills/../banana/SKILL.md")
                .is_err()
        );
    }

    struct ActivatedPluginReadRunner {
        mounts: Vec<PluginMount>,
        reads: AtomicUsize,
    }

    impl ExecutionHostRunner for ActivatedPluginReadRunner {
        fn status(&self, _policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
            Ok(ExecutionHostStatus::remote(
                true,
                ExecutionHostHealth::Ready,
                None,
            ))
        }

        fn run_file_system_operation(
            &self,
            request: ExecutionFileSystemRequest,
        ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
            assert!(authorized_plugin_path(
                self.mounts.as_slice(),
                request.model_path.as_str()
            )?);
            assert!(matches!(
                request.operation,
                ExecutionFileSystemOperation::ReadFile { .. }
            ));
            self.reads.fetch_add(1, Ordering::SeqCst);
            let bytes = b"# Banana Skill\n".to_vec();
            Ok(ExecutionFileSystemOutput::ReadFile(
                ExecutionFileReadOutput {
                    identity: ExecutionFileIdentity {
                        key: "activated-plugin-skill".to_string(),
                        display_path: request.model_path,
                    },
                    file_hash: format!("sha256:{:x}", Sha256::digest(bytes.as_slice())),
                    bytes,
                },
            ))
        }

        fn run_host_command(
            &self,
            _operation_id: Option<&str>,
            _request: ExecutionCommandRequest,
            _cancellation_probe: Option<&ExecutionCancellationProbe>,
        ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
            unreachable!("plugin Skill read must not invoke Bash")
        }
    }

    #[tokio::test]
    async fn empty_manifest_allows_activated_plugin_absolute_path_read() {
        let activation = PluginActivationSnapshotV1 {
            schema: "plugin_activation_snapshot_v1".to_string(),
            digest: format!("sha256:{}", "a".repeat(64)),
            packages: vec![centaeris_core::extension::ActivatedPluginPackageV1 {
                name: "banana".to_string(),
                version: "1.0.0".to_string(),
                package_digest: format!("sha256:{}", "b".repeat(64)),
                skills: vec![centaeris_core::extension::PluginResourceDigestV1 {
                    path: "skills/banana-skill/SKILL.md".to_string(),
                    digest: format!("sha256:{}", "c".repeat(64)),
                }],
                cli: Vec::new(),
                mcp_servers: Vec::new(),
                hooks: Vec::new(),
            }],
        };
        let runner = Arc::new(ActivatedPluginReadRunner {
            mounts: plugin_mounts(&activation),
            reads: AtomicUsize::new(0),
        });
        let workspace = std::env::temp_dir();
        let binding = Arc::new(
            ExecutionHostBinding::new(
                ExecutionHostMode::Remote,
                runner.clone(),
                workspace.clone(),
                ExecutionPolicy::workspace_write_no_network(workspace),
            )
            .expect("remote plugin execution host"),
        );
        let authorization_digest = format!("sha256:{}", "d".repeat(64));
        let manifest = Arc::new(
            ResolvedInputState::new(
                "agent_run_1".to_string(),
                authorization_digest.clone(),
                Vec::new(),
                ResolvedInputManifest {
                    schema: RESOLVED_INPUT_MANIFEST_SCHEMA.to_string(),
                    agent_run_id: "agent_run_1".to_string(),
                    authorization_digest,
                    inputs: Vec::new(),
                },
                None,
            )
            .expect("empty resolved input manifest"),
        );
        let layer = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
            SkillCatalogLoadConfig::default(),
            binding,
        )
        .expect("tool layer")
        .with_resolved_input_manifest(manifest);
        let path = "/opt/centaeris/plugins/banana/skills/banana-skill/SKILL.md";

        let output = layer
            .execute_async(ToolInvocationRequest {
                tool_call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                args_json: serde_json::json!({"path": path}).to_string(),
            })
            .await;

        assert_eq!(output.status, "ok", "{output:#?}");
        assert!(output.content.contains("Banana Skill"));
        assert_eq!(runner.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn agent_memory_scope_is_private_stable_and_exactly_mounted() {
        let volume_name = "centaeris-workspace-agent-memory".to_string();
        let mount = memory_mount("user_1", "centaeris", volume_name.clone()).expect("Memory mount");
        assert_eq!(
            mount,
            memory_mount("user_1", "centaeris", volume_name.clone()).expect("stable Memory mount")
        );
        assert_ne!(
            mount,
            memory_mount("user_1", "agent-banana", volume_name.clone()).expect("other Agent mount")
        );
        assert_ne!(
            mount,
            memory_mount("user_2", "centaeris", volume_name).expect("other user mount")
        );
        let docker_rw_mount: ContainerMountFacts = serde_json::from_value(serde_json::json!({
            "Type": "volume",
            "Source": mount.volume_name,
            "Target": MEMORY_CONTAINER_ROOT,
            "VolumeOptions": { "Subpath": mount.scope_key },
        }))
        .expect("Docker RW mount without ReadOnly field");
        assert!(memory_mount_matches(&[docker_rw_mount], &mount,));
        assert!(!memory_mount_matches(
            &[ContainerMountFacts {
                mount_type: "volume".to_string(),
                source: mount.volume_name.clone(),
                target: MEMORY_CONTAINER_ROOT.to_string(),
                read_only: true,
                volume_options: ContainerVolumeOptions {
                    subpath: mount.scope_key.clone(),
                },
            }],
            &mount,
        ));
    }

    #[test]
    fn engine_creation_preserves_sandbox_limits_and_mount_isolation() {
        let mut runner = test_docker_execution_host_runner();
        runner.resources.cpu_milli = 4000;
        runner.resources.memory_bytes = 8 * 1024 * 1024 * 1024;
        let body = container_create_body(&runner.container_expectation()).unwrap();
        assert_eq!(body["Image"], runner.image_digest);
        assert_eq!(
            body["Labels"]["centaeris.execution_id"],
            runner.execution_id
        );
        let host = &body["HostConfig"];
        assert_eq!(host["CpuPeriod"], 100000);
        assert_eq!(host["CpuQuota"], 400000);
        assert_eq!(host["Memory"], 8u64 * 1024 * 1024 * 1024);
        assert_eq!(host["PidsLimit"], runner.resources.pids_limit);
        assert_eq!(host["NetworkMode"], "none");
        assert_eq!(host["ReadonlyRootfs"], true);
        assert_eq!(host["CapDrop"], serde_json::json!(["ALL"]));
        assert_eq!(host["CapAdd"], serde_json::json!(["CHOWN"]));
        assert_eq!(
            host["SecurityOpt"],
            serde_json::json!(["no-new-privileges"])
        );
        assert_eq!(host["Mounts"][0]["ReadOnly"], true);
        assert_eq!(host["Mounts"][0]["VolumeOptions"]["Subpath"], "banana");
        assert_eq!(host["Mounts"][1]["ReadOnly"], false);
        assert_eq!(host["Mounts"][1]["VolumeOptions"]["Subpath"], "memory-test");
        // Verify the actual SDK request retains all configured fields before
        // any daemon-dependent gate is run.
        let encoded = serde_json::to_value(
            serde_json::from_value::<bollard::models::ContainerCreateBody>(body.clone()).unwrap(),
        )
        .unwrap();
        for (key, value) in host.as_object().unwrap() {
            // Models may add null defaults inside nested objects; security
            // verification itself permits only those extra defaults.
            let facts =
                serde_json::json!({"Id":"test", "HostConfig":{key:encoded["HostConfig"][key]}});
            let requested = serde_json::json!({"HostConfig":{key:value}});
            assert!(
                crate::docker_engine::verify_created(&facts, &requested).is_ok(),
                "{key}"
            );
        }
    }

    fn empty_recovery_snapshot() -> RecoveryWorkspaceSnapshotV1 {
        RecoveryWorkspaceSnapshotV1 {
            object_ref: None,
            snapshot_sha256: String::new(),
            snapshot_size_bytes: 0,
            expanded_size_bytes: 0,
            file_count: 0,
        }
    }

    #[test]
    fn recovery_snapshot_http_lifecycle_is_safe_inside_async_safe_point() {
        use std::net::TcpListener;
        for multi_thread in [false, true] {
            for status in [200, 503] {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let endpoint = format!("http://{}/stage", listener.local_addr().unwrap());
                let body = serde_json::to_vec(&empty_recovery_snapshot()).unwrap();
                let server = std::thread::spawn(move || {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        socket.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                    }
                    assert!(request.starts_with(b"POST /stage HTTP/1.1\r\n"));
                    write!(socket, "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    socket.write_all(&body).unwrap();
                });
                // The host and its blocking client are created/dropped outside Tokio.
                let mut runner = test_docker_execution_host_runner();
                runner.api_client = reqwest::blocking::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap();
                let runtime = if multi_thread {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .enable_all()
                        .build()
                        .unwrap()
                } else {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                };
                let mut calls = 0;
                let result = runtime.block_on(async {
                    runner.test_capture_recovery_workspace(|_| {
                        calls += 1;
                        let response = runner
                            .api_client
                            .post(&endpoint)
                            .send()
                            .map_err(|e| e.to_string())?;
                        if !response.status().is_success() {
                            return Err(format!(
                                "stage execution workspace returned {}",
                                response.status().as_u16()
                            ));
                        }
                        response
                            .json::<RecoveryWorkspaceSnapshotV1>()
                            .map_err(|e| e.to_string())
                    })
                });
                server.join().unwrap();
                assert_eq!(calls, 1, "no internal staging retry");
                if status == 200 {
                    assert_eq!(result.unwrap(), Some(empty_recovery_snapshot()));
                    assert!(runner.recovery_workspace_evidence().unwrap().is_some());
                } else {
                    assert_eq!(result, Err("stage execution workspace returned 503".into()));
                    assert!(runner.recovery_workspace_evidence().unwrap().is_none());
                }
            }
        }
    }

    #[test]
    fn recovery_snapshot_activity_defers_without_reusing_old_witness_or_retrying() {
        let runner = test_docker_execution_host_runner();
        let previous = empty_recovery_snapshot();
        assert!(runner
            .test_capture_recovery_workspace(|_| Ok(previous.clone()))
            .unwrap()
            .is_some());
        assert!(runner.recovery_workspace_evidence().unwrap().is_some());
        let mut calls = 0;
        let candidate = runner
            .test_capture_recovery_workspace(|_| {
                calls += 1;
                runner
                    .recovery_activity
                    .invalidate_before_dispatch()
                    .unwrap();
                Ok(previous.clone())
            })
            .expect("concurrent activity defers checkpoint rather than failing the parent");
        assert!(candidate.is_none());
        assert_eq!(calls, 1, "no internal collection or tool retry");
        assert!(runner.recovery_workspace_evidence().unwrap().is_none());
        assert_eq!(
            runner
                .test_capture_recovery_workspace(|_| Ok(previous.clone()))
                .unwrap(),
            Some(previous)
        );
        let fresh = runner.recovery_workspace_evidence().unwrap().unwrap();
        assert_eq!(fresh.snapshot_activity_epoch, fresh.current_activity_epoch);
        assert_eq!(fresh.snapshot_activity_epoch, 1);
    }

    #[test]
    fn recovery_snapshot_collection_error_still_fails_and_clears_witness() {
        let runner = test_docker_execution_host_runner();
        runner
            .test_capture_recovery_workspace(|_| Ok(empty_recovery_snapshot()))
            .unwrap();
        let result =
            runner.test_capture_recovery_workspace(|_| Err("snapshot upload failed".to_string()));
        assert_eq!(result, Err("snapshot upload failed".to_string()));
        assert!(runner.recovery_workspace_evidence().unwrap().is_none());
    }

    #[test]
    fn active_filesystem_helper_defers_capture_before_collect_or_quiesce() {
        let runner = Arc::new(test_docker_execution_host_runner());
        for mode in ["filesystem-once", "input-inventory", "read-artifact"] {
            let (started, observed) = std::sync::mpsc::channel();
            let (release, wait) = std::sync::mpsc::channel();
            let helper = {
                let runner = runner.clone();
                thread::spawn(move || {
                    runner.with_helper_dispatch(mode, || {
                        started.send(()).unwrap();
                        wait.recv().unwrap();
                        Ok(())
                    })
                })
            };
            observed.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut collected = false;
            let result = runner.test_capture_recovery_workspace(|_| {
                collected = true;
                Ok(empty_recovery_snapshot())
            });
            release.send(()).unwrap();
            helper.join().unwrap().unwrap();
            assert_eq!(result.unwrap(), None);
            assert!(
                !collected,
                "active helper must not reach destructive snapshot quiescence"
            );
        }
    }

    #[test]
    fn background_agent_processes_defer_capture_and_constructor_without_signals() {
        let runner = test_docker_execution_host_runner();
        let mut collected = false;
        let result = runner
            .capture_recovery_workspace_with_observer(
                |_| Ok(2),
                |_| {
                    collected = true;
                    Ok(empty_recovery_snapshot())
                },
            )
            .unwrap();
        assert!(result.is_none());
        assert!(!collected);
        let mut signaled = false;
        let quiesced = runner
            .with_idle_quiescence(
                |_| Ok(2),
                |_| {
                    signaled = true;
                    Ok(())
                },
            )
            .unwrap();
        assert!(!quiesced);
        assert!(!signaled);
        assert!(matches!(
            runner.restore_activity_with_observer(|_| Ok(2)),
            Err(SessionWorkspaceApiError::Unavailable(_))
        ));
    }

    #[test]
    fn terminal_collection_returns_pending_instead_of_waiting_for_active_helper() {
        let runner = Arc::new(test_docker_execution_host_runner());
        let active = runner.recovery_activity.begin_dispatch_activity().unwrap();
        let (finished, observed) = std::sync::mpsc::channel();
        let collector = {
            let runner = runner.clone();
            thread::spawn(move || {
                let result = runner
                    .with_terminal_collection(|_| Ok(SessionWorkspaceCommitOutcome::Unchanged));
                finished.send(result.clone()).unwrap();
                result
            })
        };
        let early = observed.recv_timeout(Duration::from_millis(100));
        drop(active);
        let result = collector.join().unwrap().unwrap();
        assert_eq!(early.ok(), Some(Ok(SessionWorkspaceCommitOutcome::Pending)));
        assert_eq!(result, SessionWorkspaceCommitOutcome::Pending);
    }

    #[test]
    fn agent_process_observation_requires_exact_nonnegative_integer_contract() {
        let idle: AgentProcessState = serde_json::from_str(r#"{"activeProcessCount":0}"#).unwrap();
        assert_eq!(idle.active_process_count, 0);
        for bad in [
            r#"{}"#,
            r#"{"activeProcessCount":-1}"#,
            r#"{"activeProcessCount":"0"}"#,
            r#"{"activeProcessCount":0,"unknown":true}"#,
            r#"{"active_process_count":0}"#,
        ] {
            assert!(serde_json::from_str::<AgentProcessState>(bad).is_err());
        }
    }

    #[test]
    fn helper_failure_with_empty_stderr_still_reports_observed_exit_code() {
        let error = helper_failure("filesystem-once", Some(143), b"");
        assert!(error.contains("exitCode=143"));
        assert!(error.contains("filesystem-once"));
        assert!(
            !error.contains("signal"),
            "exit status does not identify who terminated it"
        );
        assert!(helper_failure("filesystem-once", None, b"").contains("exitCode=unavailable"));
    }

    #[test]
    fn capture_holds_dispatch_until_complete_then_helper_proceeds_normally() {
        let runner = Arc::new(test_docker_execution_host_runner());
        let (capturing, observed_capture) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let capture = {
            let runner = runner.clone();
            thread::spawn(move || {
                runner.test_capture_recovery_workspace(move |_| {
                    capturing.send(()).unwrap();
                    wait.recv().unwrap();
                    Ok(empty_recovery_snapshot())
                })
            })
        };
        observed_capture
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let (started, observed) = std::sync::mpsc::channel();
        let helper = {
            let runner = runner.clone();
            thread::spawn(move || {
                runner.with_helper_dispatch("filesystem-once", || {
                    started.send(()).unwrap();
                    Ok(())
                })
            })
        };
        let early = observed.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        capture.join().unwrap().unwrap();
        helper.join().unwrap().unwrap();
        assert!(
            early.is_err(),
            "dispatch must wait while capture is exclusive"
        );
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn rebuilt_runner_shares_activity_for_the_same_execution() {
        let key = format!(
            "shared-helper-{}",
            NEXT_EXECUTION_HOST_INSTANCE.fetch_add(1, Ordering::Relaxed)
        );
        let first = ExecutionActivityWitness::for_execution(1, &key).unwrap();
        let second = ExecutionActivityWitness::for_execution(2, &key).unwrap();
        let _helper = first.begin_dispatch_activity().unwrap();
        assert!(second.try_snapshot_activity().unwrap().is_none());
        let isolated = ExecutionActivityWitness::for_execution(3, &format!("{key}-other")).unwrap();
        assert!(isolated.try_snapshot_activity().unwrap().is_some());
        first.invalidate_before_dispatch().unwrap();
        let token = second.begin_snapshot_capture();
        first.invalidate_before_dispatch().unwrap();
        assert_eq!(
            second.complete_quiesced_snapshot(token, true, true),
            Err(SnapshotCaptureError::ActivityChanged)
        );
    }

    #[test]
    fn rebuilt_constructor_and_restore_defer_while_existing_helper_is_active() {
        let key = format!(
            "active-restore-{}",
            NEXT_EXECUTION_HOST_INSTANCE.fetch_add(1, Ordering::Relaxed)
        );
        let first = ExecutionActivityWitness::for_execution(1, &key).unwrap();
        let mut runner = test_docker_execution_host_runner();
        runner.recovery_activity = ExecutionActivityWitness::for_execution(2, &key).unwrap();
        let activity = first.begin_dispatch_activity().unwrap();
        let mut quiesced = false;
        assert!(!runner
            .with_idle_quiescence(
                |_| Ok(0),
                |_| {
                    quiesced = true;
                    Ok(())
                }
            )
            .unwrap());
        assert!(!quiesced);
        assert!(matches!(
            runner.restore_activity_with_observer(|_| Ok(0)),
            Err(SessionWorkspaceApiError::Unavailable(_))
        ));
        drop(activity);
        assert!(runner
            .with_idle_quiescence(
                |_| Ok(0),
                |_| {
                    quiesced = true;
                    Ok(())
                }
            )
            .unwrap());
        assert!(quiesced);
        assert!(runner.restore_activity_with_observer(|_| Ok(0)).is_ok());
    }

    #[test]
    fn maintenance_capture_excludes_activity_without_relocking_for_internal_helper() {
        let runner = Arc::new(test_docker_execution_host_runner());
        let activity = runner.recovery_activity.begin_dispatch_activity().unwrap();
        let (started, observed) = std::sync::mpsc::channel();
        let collector = {
            let runner = runner.clone();
            thread::spawn(move || {
                let _capture = runner.recovery_activity.wait_snapshot_activity().unwrap();
                runner.with_helper_dispatch("quiesce-agent-processes", || {
                    started.send(()).unwrap();
                    Ok(())
                })
            })
        };
        let early = observed.recv_timeout(Duration::from_millis(100));
        drop(activity);
        collector.join().unwrap().unwrap();
        assert!(early.is_err());
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn collection_failure_and_panic_release_activity_gate_for_new_helpers() {
        let runner = test_docker_execution_host_runner();
        assert!(runner
            .test_capture_recovery_workspace(|_| Err("collection failed".into()))
            .is_err());
        runner
            .with_helper_dispatch("filesystem-once", || Ok(()))
            .unwrap();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runner.test_capture_recovery_workspace(|_| panic!("injected collector panic"))
        }));
        assert!(failed.is_err());
        runner
            .with_helper_dispatch("filesystem-once", || Ok(()))
            .unwrap();
        assert!(runner
            .test_capture_recovery_workspace(|_| Ok(empty_recovery_snapshot()))
            .unwrap()
            .is_some());
    }

    pub(super) fn test_docker_execution_host_runner() -> DockerExecutionHostRunner {
        DockerExecutionHostRunner {
            transcript_capture: Mutex::new(Default::default()),
            transcript_publisher: Arc::new(Default::default()),
            recovery_activity: ExecutionActivityWitness::new(
                NEXT_EXECUTION_HOST_INSTANCE.fetch_add(1, Ordering::Relaxed),
            ),
            recovery_snapshot: Mutex::new(None),
            pre_dispatch_loss: AtomicBool::new(false),
            deferred_extension_dispatch: AtomicBool::new(false),
            container_name: "centaeris-agent-run-test".to_string(),
            agent_run_id: "agent_run_test".to_string(),
            execution_id: "execution_test".to_string(),
            authorization_digest: format!("sha256:{}", "a".repeat(64)),
            image_digest: format!("sha256:{}", "b".repeat(64)),
            oci_runtime: OciRuntime::Runc,
            resources: SandboxResources {
                memory_bytes: 1,
                cpu_milli: 1,
                pids_limit: 1,
                data_tmpfs_bytes: 1,
            },
            plugin_volume_name: "centaeris-workspace-plugin-data".to_string(),
            plugin_mounts: vec![PluginMount {
                package_name: "banana".to_string(),
                destination: "/opt/centaeris/plugins/banana".to_string(),
            }],
            memory_mount: MemoryMount {
                volume_name: "centaeris-workspace-agent-memory".to_string(),
                scope_key: "memory-test".to_string(),
            },
            command_path: "/opt/centaeris/plugins/banana/bin:/usr/bin".to_string(),
            api_url: "http://api:8000".to_string(),
            api_token: "token".to_string(),
            api_client: reqwest::blocking::Client::new(),
            input_lock: Mutex::new(()),
            materialized_inputs: Mutex::new(BTreeMap::new()),
            workspace_generation_rpc: Mutex::new(None),
        }
    }

    #[test]
    fn read_root_workspace_accepts_only_directory_listing() {
        let listing = ExecutionFileSystemOperation::ListDirectory {
            recursive: false,
            max_entries: 10,
        };
        for root in [".", WORKSPACE_DATA_ROOT] {
            assert_eq!(normalize_model_path(root, &listing).unwrap(), ".");
            for operation in [
                ExecutionFileSystemOperation::ReadFile { max_bytes: 10 },
                ExecutionFileSystemOperation::InspectMutationPath,
                ExecutionFileSystemOperation::WriteFile {
                    content: vec![],
                    observed_file_hash: None,
                    create_only: true,
                },
                ExecutionFileSystemOperation::DeleteFile {
                    expected_file_hash: format!("sha256:{}", "a".repeat(64)),
                },
            ] {
                assert!(normalize_model_path(root, &operation).is_err());
            }
        }
        for path in [
            "",
            "/mnt",
            "/opt/centaeris",
            "/mnt/data/",
            "/mnt/data/..",
            "./",
            "../private",
            "a//b",
            "a\\b",
        ] {
            assert!(normalize_model_path(path, &listing).is_err(), "{path}");
        }
        assert_eq!(
            normalize_model_path("/mnt/data/reports/a.txt", &listing).unwrap(),
            "reports/a.txt"
        );
    }

    #[test]
    fn read_root_preserves_protected_input_guards() {
        let runner = test_docker_execution_host_runner();
        runner.materialized_inputs.lock().unwrap().insert(
            "input_test".to_string(),
            SandboxMaterializedInput {
                input_ref: "input_test".to_string(),
                virtual_path: "reports/input.txt".to_string(),
                size_bytes: 1,
                sha256: format!("sha256:{}", "a".repeat(64)),
                source_version: "1".to_string(),
                state: Some(ExecutionInputState::AccessRevoked),
            },
        );
        let listing = ExecutionFileSystemOperation::ListDirectory {
            recursive: true,
            max_entries: 10,
        };
        for root in [".", WORKSPACE_DATA_ROOT] {
            assert!(runner
                .protected_input_error(root, &listing)
                .unwrap()
                .is_none());
        }
        let read = ExecutionFileSystemOperation::ReadFile { max_bytes: 10 };
        assert_eq!(
            runner
                .protected_input_error("/mnt/data/reports/input.txt", &read)
                .unwrap()
                .unwrap()
                .kind,
            ExecutionFileSystemErrorKind::AccessRevoked
        );
        runner
            .materialized_inputs
            .lock()
            .unwrap()
            .get_mut("input_test")
            .unwrap()
            .state = None;
        assert!(runner
            .protected_input_error("reports/input.txt", &read)
            .unwrap()
            .is_none());
        let write = ExecutionFileSystemOperation::WriteFile {
            content: vec![],
            observed_file_hash: None,
            create_only: false,
        };
        assert_eq!(
            runner
                .protected_input_error("reports/input.txt", &write)
                .unwrap()
                .unwrap()
                .kind,
            ExecutionFileSystemErrorKind::PermissionDenied
        );
    }

    #[test]
    fn read_root_system_catalog_is_not_parent_namespace_grant() {
        assert!(authorized_system_skill_path(SYSTEM_SKILL_CONTAINER_ROOT).unwrap());
        for parent in ["/opt", "/opt/centaeris", "/mnt", "/opt/centaeris/plugins"] {
            assert!(!authorized_system_skill_path(parent).unwrap());
        }
        for path in [
            "/opt/centaeris/system-skills/",
            "/opt/centaeris/system-skills/..",
            "/opt/centaeris/system-skills//memory",
            "/opt/centaeris/system-skills/memory\\SKILL.md",
        ] {
            assert!(authorized_system_skill_path(path).is_err());
        }
        assert!(
            authorized_plugin_path(&[], "/opt/centaeris/plugins/legal-research/SKILL.md").is_err()
        );
    }

    #[test]
    fn docker_filesystem_router_rejects_unknown_uri_scheme() {
        let error = test_docker_execution_host_runner()
            .run_file_system_operation(ExecutionFileSystemRequest {
                operation_id: None,
                cwd: PathBuf::from(WORKSPACE_DATA_ROOT),
                policy: ExecutionPolicy::workspace_write_no_network(WORKSPACE_DATA_ROOT),
                model_path: "banana://workspace/file.md".to_string(),
                operation: ExecutionFileSystemOperation::InspectMutationPath,
            })
            .expect_err("unknown URI scheme must loud-fail before Docker helper dispatch");

        assert_eq!(error.kind, ExecutionFileSystemErrorKind::InvalidPath);
        assert_eq!(error.message, "filesystem URI scheme is unsupported");
    }

    #[test]
    fn mcp_exec_is_bound_to_the_agent_run_container() {
        let runner = test_docker_execution_host_runner();
        let request = runner
            .mcp_exec_request(
                "banana",
                "bin/banana-mcp",
                &["--mode".into(), "stdio".into()],
            )
            .unwrap();
        assert_eq!(request.container, "centaeris-agent-run-test");
        assert_eq!(request.user.as_deref(), Some("10001:10001"));
        assert_eq!(request.cwd.as_deref(), Some("/mnt/data"));
        assert_eq!(
            request.command,
            [
                "/opt/centaeris/plugins/banana/bin/banana-mcp",
                "--mode",
                "stdio"
            ]
        );
        assert_eq!(
            request.env,
            [
                "HOME=/home/agent",
                "PATH=/opt/centaeris/plugins/banana/bin:/usr/bin",
                "TMPDIR=/tmp"
            ]
        );

        let hook_args = runner
            .lifecycle_hook_exec_request(&LifecycleHookHandlerV1 {
                id: "banana:guard".to_string(),
                event: centaeris_core::extension::hooks::LifecycleHookEventNameV1::PreToolUse,
                matcher: Some("write".to_string()),
                source: centaeris_core::extension::hooks::LifecycleHookSourceV1 {
                    kind: LifecycleHookSourceKindV1::Plugin,
                    name: "banana".to_string(),
                },
                trusted: true,
                program: "node".to_string(),
                args: vec!["hooks/guard-write.mjs".to_string()],
                cwd: Some("ignored-host-package-root".to_string()),
                timeout_ms: 5_000,
            })
            .expect("Hook command");
        assert_eq!(hook_args.container, "centaeris-agent-run-test");
        assert_eq!(hook_args.user.as_deref(), Some("10001:10001"));
        assert_eq!(
            hook_args.cwd.as_deref(),
            Some("/opt/centaeris/plugins/banana")
        );
        assert_eq!(
            hook_args.command,
            [
                "/usr/bin/timeout",
                "--signal=TERM",
                "--kill-after=1s",
                "5.000s",
                "node",
                "hooks/guard-write.mjs"
            ]
        );
        assert_eq!(
            hook_args.env,
            [
                "LANG=C.UTF-8",
                "LC_ALL=C.UTF-8",
                "TERM=dumb",
                "HOME=/home/agent",
                "PATH=/opt/centaeris/plugins/banana/bin:/usr/bin",
                "TMPDIR=/tmp"
            ]
        );
        assert!(!hook_args.env.iter().any(|arg| arg.contains("token")));
    }

    #[cfg(unix)]
    #[test]
    fn agent_memory_scope_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "centaeris-memory-scope-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let outside = root.with_extension("outside");
        fs::create_dir(&root).expect("volume root");
        fs::create_dir(&outside).expect("outside root");
        let mount = memory_mount(
            "user_1",
            "centaeris",
            "centaeris-workspace-agent-memory".to_string(),
        )
        .expect("Memory mount");
        symlink(&outside, root.join(&mount.scope_key)).expect("scope symlink");

        assert!(prepare_memory_scope_at(&root, &mount).is_err());
        assert!(!outside.join("topics").exists());

        fs::remove_dir_all(&root).expect("remove volume root");
        fs::remove_dir_all(&outside).expect("remove outside root");
    }
}
