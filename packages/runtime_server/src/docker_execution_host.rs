mod environment;
mod environment_container;
mod maintenance;
mod network;
mod retained;
mod retained_commands;
mod snapshot_capture;
pub(crate) use maintenance::{
    maintenance_helper, maintenance_task, observe_environment, provision_environment,
    remove_cleaned_environment,
};

use crate::execution_recovery_coordinator::OwnedExecutionContinuity;
use crate::execution_recovery_policy::{
    ExecutionActivityWitness, HostFailureEvidence, SnapshotActivityGuard, SnapshotCaptureError,
    WorkspaceReuseEvidence,
};
use crate::observations::{
    MeasuredWriter, ProtectionMetric, ProtectionTrigger, WorkspaceProtection,
};
use snapshot_capture::{SnapshotCaptureCache, SnapshotCaptureResult};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{Cursor, Read, Seek, Write};
use std::path::Path;
#[cfg(test)]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

static NEXT_EXECUTION_HOST_INSTANCE: AtomicU64 = AtomicU64::new(1);
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use centaeris_core::execution::NetworkPolicy;
use centaeris_core::execution::{
    ExecutionCancellationProbe, ExecutionFileSystemError, ExecutionFileSystemErrorKind,
    ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
    ExecutionHostCommandOutput, ExecutionHostCommandRecovery, ExecutionHostHealth,
    ExecutionHostRunner, ExecutionHostStatus, ExecutionInputState, ExecutionInputStateChange,
    ExecutionWorkspaceGeneration, MAX_PUBLISHED_ARTIFACT_BYTES, WORKSPACE_DATA_ROOT,
    WORKSPACE_HOME,
};
use centaeris_core::execution::{
    ExecutionCommandRequest, ExecutionError, ExecutionPolicy, ExecutionPolicySummary,
};
use centaeris_core::extension::hooks::{
    LifecycleHookCommandResultV1, LifecycleHookEventV1, LifecycleHookHandlerV1,
    LifecycleHookRunner, LifecycleHookSourceKindV1,
};
use centaeris_core::extension::{validate_plugin_activation_snapshot, PluginActivationSnapshotV1};
use centaeris_core::runtime::contracts::RecoveryWorkspaceSnapshotV1;
#[cfg(test)]
use hosted_execution::memory::MEMORY_CONTAINER_ROOT;
use hosted_execution::memory::{is_memory_uri, MemoryPath};
#[cfg(test)]
use hosted_execution::protocol::SandboxFileSystemResult;
use hosted_execution::protocol::{
    SandboxArtifactMetadata, SandboxArtifactRequest, SandboxFileSystemRequest,
    SandboxInputInventory, SandboxInputRevokeRequest, SandboxMaterializedInput,
    SandboxWorkspaceGeneration, SandboxWorkspaceSnapshotFile, SandboxWorkspaceSnapshotManifest,
    SANDBOX_INPUT_INVENTORY_SCHEMA, SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::agent_run_authorization::SessionWorkspace;
use crate::agent_run_authorization::{EnvironmentProfile, ExecutionNetwork, SandboxResources};
use crate::postgres_store::PostgresRuntimeStore;
use crate::resident_capacity::{declared_usage, ObservedResidentSandbox, ResidentCapacity};

pub(crate) fn environment_policy(profile: &EnvironmentProfile) -> ExecutionPolicy {
    let mut policy = network::policy(profile.network, &profile.workspace_root);
    policy.filesystem.denied_read_paths =
        vec!["/run/centaeris".into(), "/run/centaeris-host".into()];
    policy.filesystem.denied_write_paths = policy.filesystem.denied_read_paths.clone();
    policy
}

const VALIDATE_INPUTS_SCHEMA: &str = "runtime.projected_input.validate.v1";
const AGENT_BINARY: &str = "/opt/centaeris/bin/execution_agent";
const AGENT_USER: &str = "10001:10001";
const HELPER_JSON_LIMIT: usize = 1024 * 1024;
const DOCKER_DIAGNOSTIC_LIMIT: usize = 64 * 1024;
const PLUGIN_VOLUME_NAME_ENV: &str = "PLUGIN_VOLUME_NAME";
const PLUGIN_CONTAINER_ROOT: &str = "/opt/centaeris/plugins";
const SYSTEM_SKILL_CONTAINER_ROOT: &str = "/opt/centaeris/system-skills";
const AGENT_MEMORY_VOLUME_NAME_ENV: &str = "AGENT_MEMORY_VOLUME_NAME";
const WORKSPACE_GENERAL_IMAGE_ENV: &str = "WORKSPACE_GENERAL_IMAGE";
const MEMORY_RUNTIME_VOLUME_ROOT: &str = "/var/lib/centaeris-agent-memory";
const BASE_COMMAND_PATH: &str =
    "/opt/centaeris/bin:/opt/centaeris/venv/bin:/usr/local/bin:/usr/bin:/bin";
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

#[derive(Debug, Clone)]
pub(crate) struct SessionWorkspaceLease {
    pub job_id: String,
    pub lease_owner: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionWorkspaceResolution {
    RetainedLive,
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
    ProtectionDeferred,
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
    network: ExecutionNetwork,
    environment_profile: Option<EnvironmentProfile>,
    benchmark_helper: Option<String>,
    pub(crate) transcript_capture: Mutex<crate::transcript_capture::CaptureBuffer>,
    pub(crate) transcript_publisher: Arc<crate::transcript_capture::CapturePublisher>,
    recovery_activity: ExecutionActivityWitness,
    recovery_snapshot: Mutex<Option<WorkspaceReuseEvidence>>,
    snapshot_capture_cache: Mutex<Option<SnapshotCaptureCache>>,
    pre_dispatch_loss: AtomicBool,
    deferred_extension_dispatch: AtomicBool,
    container_name: String,
    agent_run_id: String,
    execution_id: String,
    authorization_digest: String,
    environment_id: String,
    instance_id: String,
    binding_digest: String,
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
    resident_store: Option<Arc<PostgresRuntimeStore>>,
    resident_lease: Option<centaeris_core::session::RuntimeJobLeaseFence>,
    retained_allocation:
        Option<crate::postgres_store::execution_instances::RetainedInstanceAllocation>,
    environment_authorization: Option<crate::environment_authorization::EnvironmentAuthorization>,
    retained_workspace_id: Option<String>,
}

fn decode_workspace_generation(bytes: &[u8]) -> Result<SandboxWorkspaceGeneration, String> {
    let generation: SandboxWorkspaceGeneration =
        serde_json::from_slice(bytes).map_err(|_| "workspace_generation_invalid")?;
    generation.validate()?;
    Ok(generation)
}

pub struct DockerExecutionHostRequest<'a> {
    pub network: ExecutionNetwork,
    pub environment_profile: Option<EnvironmentProfile>,
    pub workspace_id: String,
    pub lifecycle_job_id: String,
    pub lifecycle_lease_owner: String,
    pub resident_store: Arc<PostgresRuntimeStore>,
    pub agent_run_id: String,
    pub execution_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub authorization_digest: String,
    pub environment_authorization: crate::environment_authorization::EnvironmentAuthorization,
    pub image_digest: String,
    pub resources: SandboxResources,
    pub has_execution_fact: bool,
    pub api_url: String,
    pub api_token: String,
    pub plugin_activation: &'a PluginActivationSnapshotV1,
}

#[derive(Clone, Copy)]
struct ContainerExpectation<'a> {
    network: ExecutionNetwork,
    environment_profile: Option<&'a EnvironmentProfile>,
    benchmark_helper: Option<&'a str>,
    profile_digest: Option<&'a str>,
    name: &'a str,
    agent_run_id: &'a str,
    allocation_owner: Option<&'a centaeris_core::execution::EnvironmentTaskOwner>,
    execution_id: &'a str,
    environment_id: &'a str,
    instance_id: &'a str,
    binding_digest: &'a str,
    image_digest: &'a str,
    oci_runtime: OciRuntime,
    resources: SandboxResources,
    plugin_volume_name: &'a str,
    plugin_mounts: &'a [PluginMount],
}

impl DockerExecutionHostRunner {
    pub(crate) fn keeps_instance(&self) -> bool {
        self.environment_authorization
            .as_ref()
            .is_some_and(|a| a.payload.execution_mode == "retained")
    }
    pub(crate) fn workspace_root(&self) -> &str {
        self.environment_profile
            .as_ref()
            .map_or(WORKSPACE_DATA_ROOT, |profile| {
                profile.workspace_root.as_str()
            })
    }
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

    pub fn new_with_admitted(
        request: DockerExecutionHostRequest<'_>,
        before_create: &mut (dyn FnMut(&mut postgres::Client) -> Result<(), String> + Send),
    ) -> Result<Self, String> {
        if request.environment_authorization.payload.agent_run_id != request.agent_run_id
            || request
                .environment_authorization
                .payload
                .agent_run_authorization_digest
                != request.authorization_digest
        {
            return Err("Docker environment binding is invalid".into());
        }
        if request
            .environment_profile
            .as_ref()
            .is_some_and(|profile| profile.network != request.network)
        {
            return Err("benchmark network differs from signed authorization".into());
        }
        #[cfg(test)]
        if crate::native_consumption_tests::owns_identity(&request.user_id, &request.agent_id) {
            assert!(
                !request.has_execution_fact,
                "fresh execution required by native fixture"
            );
            validate_plugin_activation_snapshot(request.plugin_activation)?;
            assert!(plugin_mounts(request.plugin_activation).is_empty());
            let allocation =
                crate::native_consumption_tests::prepare_owned_allocation(&request, before_create)?;
            crate::native_consumption_tests::register_owned_run(&request.agent_run_id);
            let mut runner = tests::test_docker_execution_host_runner();
            runner.container_name = format!("native-consume-test:{}", request.execution_id);
            runner.agent_run_id = request.agent_run_id;
            runner.execution_id = request.execution_id;
            runner.environment_id = request
                .environment_authorization
                .payload
                .environment_id
                .clone();
            runner.binding_digest = request.environment_authorization.payload.digest()?;
            runner.instance_id = allocation.instance_id.clone();
            runner.authorization_digest = request.authorization_digest;
            runner.image_digest = request.image_digest;
            runner.resources = request.resources;
            runner.network = request.network;
            runner.plugin_mounts.clear();
            runner.command_path = "/usr/bin".into();
            runner.api_url = request.api_url;
            runner.api_token = request.api_token;
            runner.environment_profile = request.environment_profile;
            runner.environment_authorization = Some(request.environment_authorization);
            runner.resident_store = Some(request.resident_store);
            runner.resident_lease = Some(centaeris_core::session::RuntimeJobLeaseFence {
                job_id: request.lifecycle_job_id,
                job_kind: "agent_run.lifecycle".into(),
                lease_owner: request.lifecycle_lease_owner,
            });
            runner.retained_workspace_id = Some(request.workspace_id);
            runner.retained_allocation = Some(allocation);
            return Ok(runner);
        }
        let DockerExecutionHostRequest {
            network,
            environment_profile,
            workspace_id,
            lifecycle_job_id,
            lifecycle_lease_owner,
            resident_store,
            agent_run_id,
            execution_id,
            user_id,
            agent_id,
            authorization_digest,
            environment_authorization,
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
        let environment_binding = &environment_authorization.payload;
        let retained = environment_binding.controller.is_some();
        if !retained || environment_profile.is_none() {
            return Err("environment_profile_and_controller_required".into());
        }
        let benchmark_helper = environment_profile
            .as_ref()
            .map(|_| environment_container::helper_path())
            .transpose()?;
        let environment_id = environment_binding.environment_id.clone();
        let binding_digest = environment_binding.digest()?;
        let mut instance_id =
            crate::postgres_store::execution_instances::instance_id(&execution_id);
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
        let mut container_name = container_name(execution_id.as_str());
        let expected = ContainerExpectation {
            network,
            environment_profile: environment_profile.as_ref(),
            benchmark_helper: benchmark_helper.as_deref(),
            profile_digest: None,
            name: container_name.as_str(),
            agent_run_id: agent_run_id.as_str(),
            allocation_owner: None,
            execution_id: execution_id.as_str(),
            environment_id: &environment_id,
            instance_id: &instance_id,
            binding_digest: &binding_digest,
            image_digest: image_digest.as_str(),
            oci_runtime,
            resources,
            plugin_volume_name: plugin_volume_name.as_str(),
            plugin_mounts: plugin_mounts.as_slice(),
        };
        let (resident_host_id, resident_limits) = resident_host_context()?;
        let lease = centaeris_core::session::RuntimeJobLeaseFence {
            job_id: lifecycle_job_id.clone(),
            job_kind: centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
            lease_owner: lifecycle_lease_owner.clone(),
        };
        let retained_allocation = {
            let key = retained::signing_key()?;
            let profile = retained::profile_digest(&expected, plugin_activation)?;
            let request = crate::postgres_store::execution_instances::RetainedInstanceRequest {
                environment_authorization: &environment_authorization,
                signing_key: key.as_bytes(),
                execution_id: &execution_id,
                workspace_id: &workspace_id,
                host_id: &resident_host_id,
                image_digest: &image_digest,
                profile_digest: &profile,
                resources: declared_usage(resources),
                lease: &lease,
            };
            let allocation = resident_store.prepare_retained_instance(
                &request,
                resident_limits,
                observed_resident_sandboxes,
                |client, _| before_create(client),
                |allocation| {
                    prepare_memory_scope(&memory_mount)?;
                    let name = self::container_name(&allocation.allocation_execution_id);
                    let physical = retained::origin_expectation(&expected, allocation, &name);
                    ensure_container(&physical, allocation.reused || has_execution_fact)?;
                    let facts =
                        inspect_container(&name)?.ok_or("retained container disappeared")?;
                    if allocation
                        .container_id
                        .as_ref()
                        .is_some_and(|id| id != &facts.id)
                    {
                        return Err("retained container incarnation mismatch".into());
                    }
                    let fence =
                        retained::fence(&environment_authorization, &allocation.instance_id)?;
                    retained::takeover(&physical, &facts.id, &fence)?;
                    Ok(facts.id)
                },
            )?;
            instance_id = allocation.instance_id.clone();
            container_name = self::container_name(&allocation.allocation_execution_id);
            Some(allocation)
        };
        let runner = Self {
            network,
            environment_profile,
            benchmark_helper,
            transcript_capture: Mutex::new(Default::default()),
            transcript_publisher: Arc::new(Default::default()),
            recovery_activity: ExecutionActivityWitness::for_execution(
                NEXT_EXECUTION_HOST_INSTANCE.fetch_add(1, Ordering::Relaxed),
                &container_name,
            )?,
            recovery_snapshot: Mutex::new(None),
            snapshot_capture_cache: Mutex::new(None),
            pre_dispatch_loss: AtomicBool::new(false),
            deferred_extension_dispatch: AtomicBool::new(false),
            container_name,
            agent_run_id,
            execution_id,
            authorization_digest,
            environment_id,
            instance_id,
            binding_digest,
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
            resident_store: Some(resident_store),
            resident_lease: Some(centaeris_core::session::RuntimeJobLeaseFence {
                job_id: lifecycle_job_id,
                job_kind: centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
                lease_owner: lifecycle_lease_owner,
            }),
            retained_allocation,
            environment_authorization: retained.then_some(environment_authorization),
            retained_workspace_id: retained.then_some(workspace_id),
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
        let origin = self.retained_allocation.as_ref();
        ContainerExpectation {
            network: self.network,
            environment_profile: self.environment_profile.as_ref(),
            benchmark_helper: self.benchmark_helper.as_deref(),
            profile_digest: origin.map(|a| a.profile_digest.as_str()),
            name: self.container_name.as_str(),
            allocation_owner: origin.map(|a| &a.allocation_owner),
            agent_run_id: origin.map_or(self.agent_run_id.as_str(), |a| {
                a.allocation_agent_run_id.as_deref().unwrap_or("")
            }),
            execution_id: origin.map_or(self.execution_id.as_str(), |a| {
                a.allocation_execution_id.as_str()
            }),
            environment_id: &self.environment_id,
            instance_id: origin.map_or(self.instance_id.as_str(), |a| a.instance_id.as_str()),
            binding_digest: origin
                .map_or(self.binding_digest.as_str(), |a| a.binding_digest.as_str()),
            image_digest: self.image_digest.as_str(),
            oci_runtime: self.oci_runtime,
            resources: self.resources,
            plugin_volume_name: self.plugin_volume_name.as_str(),
            plugin_mounts: self.plugin_mounts.as_slice(),
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

    #[cfg(test)]
    fn exec_request(&self, command: Vec<String>) -> ContainerObservation {
        ContainerObservation {
            container: self.container_name.clone(),
            labels: [
                ("centaeris.managed".into(), "true".into()),
                ("centaeris.agent_run_id".into(), self.agent_run_id.clone()),
                ("centaeris.execution_id".into(), self.execution_id.clone()),
                ("centaeris.instance_id".into(), self.instance_id.clone()),
                (
                    "centaeris.environment_id".into(),
                    self.environment_id.clone(),
                ),
            ]
            .into(),
            command,
            user: None,
        }
    }

    pub(crate) fn mcp_exec_request(
        self: &Arc<Self>,
        plugin_name: &str,
        program: &str,
        program_args: &[String],
    ) -> Result<crate::environment_stdio::EnvironmentStdioRequest, String> {
        let mount = self
            .plugin_mounts
            .iter()
            .find(|m| m.package_name == plugin_name)
            .ok_or("mcp_plugin_not_activated")?;
        let program = format!("{}/{program}", mount.destination);
        if !authorized_plugin_path(&self.plugin_mounts, &program)
            .map_err(|_| "mcp_program_rejected")?
        {
            return Err("mcp_program_rejected".into());
        }
        let fence = self.environment_task_fence()?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "mcp_identity_invalid")?
            .as_nanos();
        let runner = Arc::clone(self);
        let dispatch = self.recovery_activity.begin_dispatch_activity()?;
        dispatch.register_persistent_dispatch()?;
        self.deferred_extension_dispatch
            .store(true, Ordering::SeqCst);
        Ok(crate::environment_stdio::EnvironmentStdioRequest {
            operation_id: fence.operation_id(&format!("mcp:{}:{stamp}", std::process::id())),
            fence,
            program,
            args: program_args.into(),
            cwd: self.workspace_root().into(),
            env: BTreeMap::from([
                ("HOME".into(), WORKSPACE_HOME.into()),
                ("PATH".into(), self.command_path.clone()),
                ("TMPDIR".into(), "/tmp".into()),
            ]),
            rpc: Arc::new(move |request| runner.environment_task_rpc(request)),
        })
    }

    fn lifecycle_hook_request(
        &self,
        handler: &LifecycleHookHandlerV1,
    ) -> Result<ExecutionCommandRequest, String> {
        if handler.source.kind != LifecycleHookSourceKindV1::Plugin {
            return Err("lifecycle_hook_source_rejected".into());
        }
        let mount = self
            .plugin_mounts
            .iter()
            .find(|m| m.package_name == handler.source.name)
            .ok_or("lifecycle_hook_plugin_not_activated")?;
        let bare = !handler.program.is_empty()
            && !handler.program.contains('/')
            && !handler.program.contains('\\')
            && !handler.program.chars().any(char::is_control);
        if !bare
            && (!authorized_plugin_path(&self.plugin_mounts, &handler.program)
                .map_err(|_| "lifecycle_hook_program_rejected")?
                || !handler
                    .program
                    .starts_with(&format!("{}/", mount.destination)))
        {
            return Err("lifecycle_hook_program_rejected".into());
        }
        Ok(ExecutionCommandRequest {
            program: handler.program.clone(),
            args: handler.args.clone(),
            cwd: mount.destination.clone().into(),
            env: Default::default(),
            timeout_ms: handler.timeout_ms,
            policy: self.execution_policy(),
        })
    }

    fn run_lifecycle_hook_command(
        &self,
        handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> Result<LifecycleHookCommandResultV1, String> {
        let request = self.lifecycle_hook_request(handler)?;
        let _activity = self.recovery_activity.begin_dispatch_activity()?;
        self.recovery_activity.invalidate_before_dispatch()?;
        let mut input = serde_json::to_vec(event).map_err(|_| "lifecycle_hook_event_invalid")?;
        input.push(b'\n');
        let fence = self.environment_task_fence()?;
        let identity = format!(
            "hook:{:x}",
            Sha256::digest(
                serde_json::to_vec(&(handler, event))
                    .map_err(|_| "lifecycle_hook_event_invalid")?
            )
        );
        let output = retained_commands::run_command_with_stdin(
            &fence,
            &identity,
            request,
            &input,
            &self.command_path,
            |request| self.environment_task_rpc(request),
        )
        .map_err(|_| "lifecycle_hook_outcome_unknown")?;
        let mut stdout = output.process.stdout;
        let mut stderr = output.process.stderr;
        let stdout_truncated = output.process.stdout_decode.raw_byte_length > stdout.len()
            || stdout.len() > DOCKER_DIAGNOSTIC_LIMIT;
        let stderr_truncated = output.process.stderr_decode.raw_byte_length > stderr.len()
            || stderr.len() > DOCKER_DIAGNOSTIC_LIMIT;
        for text in [&mut stdout, &mut stderr] {
            let mut limit = DOCKER_DIAGNOSTIC_LIMIT.min(text.len());
            while !text.is_char_boundary(limit) {
                limit -= 1;
            }
            text.truncate(limit);
        }
        Ok(LifecycleHookCommandResultV1 {
            exit_code: output.process.exit_code,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            timed_out: output.process.timed_out,
            spawn_error: None,
        })
    }

    pub fn teardown(
        agent_run_id: &str,
        resident_store: &PostgresRuntimeStore,
        fence: &centaeris_core::session::RuntimeJobLeaseFence,
    ) -> Result<(), String> {
        #[cfg(test)]
        if crate::native_consumption_tests::owns_run(agent_run_id) {
            return Ok(());
        }
        crate::observations::timed("sandboxTeardown", || {
            resident_store.remove_resident_sandboxes(
                &resident_host_id()?,
                agent_run_id,
                fence,
                || Self::teardown_inner(agent_run_id),
                || observed_resident_sandboxes_for_run(agent_run_id),
            )
        })
    }

    #[cfg(test)]
    fn teardown_owned(&self) -> Result<(), String> {
        if self.retained_allocation.is_some() {
            return Err("retained instance removal requires explicit cleanup authority".into());
        }
        if let Some(store) = self.resident_store.as_ref() {
            return Self::teardown(
                &self.agent_run_id,
                store,
                self.resident_lease
                    .as_ref()
                    .ok_or("resident removal owner binding missing")?,
            );
        }
        #[cfg(test)]
        return Self::teardown_inner(&self.agent_run_id);
        #[cfg(not(test))]
        Err("resident sandbox accounting binding missing".into())
    }

    pub(crate) fn reconcile_residency(resident_store: &PostgresRuntimeStore) -> Result<(), String> {
        let (host_id, _) = resident_host_context()?;
        resident_store.reconcile_resident_sandboxes(&host_id, observed_resident_sandboxes)
    }

    pub(crate) fn retire_recovery_execution(
        agent_run_id: &str,
        execution_id: &str,
        resident_store: &PostgresRuntimeStore,
        fence: &centaeris_core::session::RuntimeJobLeaseFence,
    ) -> Result<(), String> {
        resident_store.remove_resident_execution(
            &resident_host_id()?,
            agent_run_id,
            execution_id,
            fence,
            || {
                let name = container_name(execution_id);
                if let Some(facts) = inspect_container(&name)? {
                    if !facts.has_identity(agent_run_id, execution_id) {
                        return Err(
                            "sandbox container identity mismatch; refusing recovery retirement"
                                .into(),
                        );
                    }
                    crate::docker_engine::remove_stopped(&facts.id)?;
                    if inspect_container(&name)?.is_some() {
                        return Err("sandbox recovery retirement was not confirmed".into());
                    }
                }
                Ok(())
            },
            || observed_resident_sandboxes_for_run(agent_run_id),
        )
    }

    pub(crate) fn discard_unstarted_recovery_execution(
        agent_run_id: &str,
        execution_id: &str,
        resident_store: &PostgresRuntimeStore,
        fence: &centaeris_core::session::RuntimeJobLeaseFence,
        authorize: impl FnOnce(&mut postgres::Transaction<'_>) -> Result<(), String> + Send,
    ) -> Result<(), String> {
        resident_store.discard_unstarted_resident_execution(
            &resident_host_id()?,
            agent_run_id,
            execution_id,
            fence,
            authorize,
            |known_container_id| {
                if known_container_id.is_some_and(|id| {
                    id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                    return Err("pending recovery known container identity invalid".into());
                }
                let name = container_name(execution_id);
                let Some(facts) = inspect_container(&name)? else {
                    return if known_container_id.is_some() {
                        Ok(())
                    } else {
                        Err("pending recovery creation outcome remains unknown".into())
                    };
                };
                if !facts.has_identity(agent_run_id, execution_id)
                    || known_container_id.is_some_and(|id| id != facts.id)
                {
                    return Err(
                        "pending recovery container identity mismatch; refusing discard".into(),
                    );
                }
                // This is a proven unstarted setup, not an ended live Execution.
                // Force removes its orphaned setup processes while retaining volumes.
                crate::docker_engine::remove(&facts.id, false)?;
                if inspect_container(&name)?.is_some() {
                    return Err("pending recovery discard was not confirmed".into());
                }
                Ok(())
            },
            || observed_resident_sandboxes_for_run(agent_run_id),
        )
    }

    pub(crate) fn restore_prepared_recovery_workspace(
        &self,
        prepared: &mut PreparedRecoveryWorkspaceRestore,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        if let Some(artifact) = prepared.artifact.as_mut() {
            self.require_workspace_capacity(
                artifact.descriptor.expanded_size_bytes,
                input_upper_bound_bytes,
            )
            .map_err(SessionWorkspaceApiError::Rejected)?;
            let capture = self.restore_activity()?;
            artifact
                .file
                .rewind()
                .map_err(|error| SessionWorkspaceApiError::Unavailable(error.to_string()))?;
            // The sealed helper authorizes every start, query and result against
            // the current controller and exact physical instance.
            self.restore_snapshot_stream(
                &mut artifact.file,
                artifact.descriptor.size_bytes,
                &artifact.descriptor.sha256,
                &capture,
                &artifact.observation,
            )
            .map_err(SessionWorkspaceApiError::Rejected)?;
        }
        Ok(prepared.resolution)
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
        resolved_resolution(&resolved, frozen)
    }

    pub(crate) fn restore_session_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        if self.is_hot_reuse() {
            return Ok(SessionWorkspaceResolution::RetainedLive);
        }
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
        let observation = WorkspaceProtection::new().with_context(
            ProtectionTrigger::SessionRestore,
            &self.agent_run_id,
            Some(&lease.job_id),
        );
        observation
            .download(&mut response, |source| {
                self.restore_snapshot_stream(
                    source,
                    frozen.snapshot_size_bytes,
                    frozen.snapshot_sha256.as_str(),
                    &capture,
                    &observation,
                )
            })
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
            let artifact = match self.capture_snapshot(
                capture,
                &WorkspaceSnapshotDescriptor::from(frozen),
                ProtectionTrigger::TerminalCommit,
                &lease.job_id,
            )? {
                SnapshotCaptureResult::Snapshot(artifact) => artifact,
                SnapshotCaptureResult::Unchanged => {
                    self.require_workspace_capacity(
                        frozen.expanded_size_bytes,
                        input_upper_bound_bytes,
                    )?;
                    return Ok(SessionWorkspaceCommitOutcome::Unchanged);
                }
            };
            let descriptor = &artifact.descriptor;
            self.require_workspace_capacity(
                descriptor.expanded_size_bytes,
                input_upper_bound_bytes,
            )?;
            if snapshot_matches_frozen_workspace(descriptor, frozen) {
                artifact
                    .observation
                    .count(ProtectionMetric::UploadAttempts, 0, true);
                artifact
                    .observation
                    .count(ProtectionMetric::UploadedBytes, 0, true);
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
            let observation = &artifact.observation;
            let length = u64::try_from(prefix.len())
                .map_err(|_| "session workspace commit length overflow".to_string())?
                .checked_add(descriptor.size_bytes)
                .ok_or_else(|| "session workspace commit length overflow".to_string())?;
            let response = observation.timed(ProtectionMetric::UploadElapsedMs, || {
                observation.count(ProtectionMetric::UploadAttempts, 1, true);
                if descriptor.size_bytes == 0 {
                    self.api_client
                        .post(format!(
                            "{}/internal/agent-runs/session-workspace/commit",
                            self.api_url
                        ))
                        .header("X-Internal-Token", self.api_token.as_str())
                        .header("Content-Length", prefix.len().to_string())
                        .header("Content-Type", "application/octet-stream")
                        .body(reqwest::blocking::Body::sized(
                            observation.upload_reader(Cursor::new(prefix), length),
                            length,
                        ))
                        .timeout(Duration::from_secs(30))
                        .send()
                } else {
                    let snapshot = artifact.file;
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
                        .body(reqwest::blocking::Body::sized(
                            observation.upload_reader(body, length),
                            length,
                        ))
                        .timeout(Duration::from_secs(300))
                        .send()
                }
            });
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
        if self.keeps_instance() {
            return Ok(SessionWorkspaceCommitOutcome::ProtectionDeferred);
        }
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
                let capture_result =
                    crate::observations::timed("workspaceSnapshotInspect", || {
                        self.capture_snapshot(
                            capture,
                            &WorkspaceSnapshotDescriptor::from(previous),
                            ProtectionTrigger::RecoveryCheckpoint,
                            &lease.job_id,
                        )
                    })?;
                let artifact = match capture_result {
                    SnapshotCaptureResult::Snapshot(artifact) => artifact,
                    SnapshotCaptureResult::Unchanged => {
                        self.require_workspace_capacity(
                            previous.expanded_size_bytes,
                            input_upper_bound_bytes,
                        )?;
                        return Ok(PreparedRecoveryContent::Unchanged(previous.clone()));
                    }
                };
                self.require_workspace_capacity(
                    artifact.descriptor.expanded_size_bytes,
                    input_upper_bound_bytes,
                )?;
                if snapshot_matches_recovery_workspace(&artifact.descriptor, previous) {
                    artifact
                        .observation
                        .count(ProtectionMetric::UploadAttempts, 0, true);
                    artifact
                        .observation
                        .count(ProtectionMetric::UploadedBytes, 0, true);
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
        if self.keeps_instance() {
            return Err("retained_consistent_capture_unavailable".into());
        }
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
        let observation = artifact.observation.clone().with_context(
            ProtectionTrigger::RecoveryCheckpoint,
            &self.agent_run_id,
            Some(&lease.job_id),
        );
        observation.timed(ProtectionMetric::UploadElapsedMs, || {
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
                observation.count(ProtectionMetric::UploadAttempts, 1, true);
                let response = if descriptor.size_bytes == 0 {
                    let length = prefix.len() as u64;
                    self.api_client
                        .post(format!(
                            "{}/internal/agent-runs/execution-workspace/stage",
                            self.api_url
                        ))
                        .header("X-Internal-Token", self.api_token.as_str())
                        .header("Content-Length", prefix.len().to_string())
                        .header("Content-Type", "application/octet-stream")
                        .body(reqwest::blocking::Body::sized(
                            observation.upload_reader(Cursor::new(prefix), length),
                            length,
                        ))
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
                            observation.upload_reader(
                                SessionWorkspaceUploadBody {
                                    prefix: Cursor::new(prefix),
                                    snapshot,
                                },
                                length,
                            ),
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
        let observation = WorkspaceProtection::new().with_context(
            ProtectionTrigger::RecoveryRestore,
            &self.agent_run_id,
            Some(&lease.job_id),
        );
        observation
            .download(&mut response, |source| {
                self.restore_snapshot_stream(
                    source,
                    snapshot.snapshot_size_bytes,
                    snapshot.snapshot_sha256.as_str(),
                    &capture,
                    &observation,
                )
            })
            .map_err(SessionWorkspaceApiError::Rejected)
    }

    fn reconcile_session_workspace_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        match self.resolve_session_workspace(lease, frozen) {
            Ok(SessionWorkspaceResolution::RetainedLive) => {
                Ok(SessionWorkspaceCommitOutcome::ProtectionDeferred)
            }
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
        require_workspace_capacity_for_limit(
            expanded_size_bytes,
            input_upper_bound_bytes,
            self.resources.data_tmpfs_bytes,
        )
    }

    fn restore_snapshot_stream(
        &self,
        source: &mut impl Read,
        expected_size_bytes: u64,
        expected_sha256: &str,
        capture: &SnapshotActivityGuard,
        observation: &WorkspaceProtection,
    ) -> Result<(), String> {
        observation.timed(ProtectionMetric::RestoreApplyElapsedMs, || {
            self.begin_snapshot_restore()?;
            let _ = capture;
            if expected_size_bytes > hosted_execution::host_supervisor::HELPER_STREAM_LIMIT {
                return Err("snapshot_restore_limit_exceeded".into());
            }
            let mut input =
                tempfile::tempfile().map_err(|_| "snapshot_restore_spool_unavailable")?;
            copy_exact(source, &mut input, expected_size_bytes, expected_sha256)?;
            let (_, _, exit) = self.supervised_helper_file(
                hosted_execution::host_supervisor::TrustedHelperKind::SnapshotRestore,
                &mut input,
                expected_size_bytes,
                expected_sha256,
            )?;
            if exit != Some(0) {
                return Err("snapshot_restore_failed".into());
            }
            Ok(())
        })
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
        let base = network::policy(self.network, self.workspace_root());
        if policy != &base && policy != &self.execution_policy() {
            return Err(ExecutionError::Denied {
                reason: "environment policy differs from signed profile".into(),
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
        if user.is_some_and(|user| {
            user != AGENT_USER
                && self
                    .environment_profile
                    .as_ref()
                    .is_none_or(|profile| user != profile.user)
        }) {
            return Err("environment_helper_user_override_rejected".into());
        }
        use hosted_execution::host_supervisor::TrustedHelperKind;
        let kind = match mode {
            "filesystem-once" => TrustedHelperKind::FileSystem,
            "input-inventory" => TrustedHelperKind::InputInventory,
            "materialize-input" => TrustedHelperKind::MaterializeInput,
            "revoke-input" => TrustedHelperKind::RevokeInput,
            "read-artifact" => TrustedHelperKind::ReadArtifact,
            "snapshot-collect" => TrustedHelperKind::SnapshotCollect,
            "snapshot-capture" => TrustedHelperKind::SnapshotCapture,
            "snapshot-restore" => TrustedHelperKind::SnapshotRestore,
            "workspace-generation" => TrustedHelperKind::WorkspaceGeneration,
            "workspace-generation-rpc" => TrustedHelperKind::WorkspaceGenerationRpc,
            "agent-process-state" => TrustedHelperKind::AgentProcessState,
            "quiesce-agent-processes" => TrustedHelperKind::QuiesceAgentProcesses,
            _ => return Err("environment_helper_kind_rejected".into()),
        };
        self.supervised_helper(kind, input, output_limit)
    }

    fn protected_input_error(
        &self,
        model_path: &str,
        operation: &ExecutionFileSystemOperation,
    ) -> Result<Option<ExecutionFileSystemError>, ExecutionFileSystemError> {
        let path = normalize_model_path_at(self.workspace_root(), model_path, operation)?;
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

        let fence = self
            .environment_task_fence()
            .map_err(|error| self.unavailable(error))?;
        self.environment_task_rpc(centaeris_core::execution::EnvironmentTaskRequest::List {
            fence,
        })
        .map_err(|error| self.unavailable(error))?;
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
            .helper("workspace-generation", None, &[], HELPER_JSON_LIMIT)
            .and_then(|bytes| decode_workspace_generation(&bytes));
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
        if request.cwd != Path::new(self.workspace_root()) {
            return Err(filesystem_unavailable(
                "environment filesystem cwd differs from profile",
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
        self.validate_policy(&request.policy).map_err(|error| {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                error.to_string(),
            )
        })?;
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
        if memory_path {
            prepare_memory_scope(&self.memory_mount).map_err(filesystem_unavailable)?;
            return hosted_execution::memory::run_memory_file_system_operation_at(
                &Path::new(MEMORY_RUNTIME_VOLUME_ROOT).join(&self.memory_mount.scope_key),
                SandboxFileSystemRequest {
                    path: request.model_path,
                    operation: request.operation,
                },
            );
        }
        let mut request = request;
        request.policy = self.execution_policy();
        if read_only_path {
            request
                .policy
                .filesystem
                .read_only_roots
                .push(request.model_path.clone().into());
        }
        let operation = request.operation.clone();
        let output_limit = usize::try_from(
            hosted_execution::host_supervisor::TrustedHelperKind::FileSystem.output_limit(),
        )
        .map_err(|_| filesystem_unavailable("filesystem budget invalid"))?;
        let mut body = Vec::new();
        hosted_execution::protocol::write_filesystem_request(&mut body, request)
            .map_err(filesystem_unavailable)?;
        let output = self
            .helper("filesystem-once", None, &body, output_limit)
            .map_err(filesystem_unavailable)?;
        let mut result =
            hosted_execution::protocol::read_filesystem_result(&mut Cursor::new(output))
                .map_err(filesystem_unavailable)?
                .into_result()?;
        if !memory_path && !read_only_path {
            if let Ok(mut buffer) = self.transcript_capture.lock() {
                buffer.observe_write(
                    &operation,
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

    fn recover_host_command(
        &self,
        operation_id: &str,
        request: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandRecovery, ExecutionError> {
        self.validate_policy(&request.policy)?;
        let fence = self
            .environment_task_fence()
            .map_err(|error| self.unavailable(error))?;
        retained_commands::recover_command(
            &fence,
            operation_id,
            request,
            cancellation_probe,
            &self.command_path,
            |request| self.environment_task_rpc(request),
        )
        .map(|output| ExecutionHostCommandRecovery::Completed(Box::new(output)))
    }

    fn run_host_command(
        &self,
        operation_id: Option<&str>,
        request: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        self.validate_policy(&request.policy)?;
        let operation_id = operation_id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| self.unavailable("retained command operation identity required"))?;
        let fence = self
            .environment_task_fence()
            .map_err(|error| self.unavailable(error))?;
        retained_commands::run_command(
            &fence,
            operation_id,
            request,
            cancellation_probe,
            &self.command_path,
            |request| self.environment_task_rpc(request),
        )
    }
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
    let body = container_create_body(expected)?;
    if let Some(facts) = inspect_container(expected.name)? {
        if !facts.matches_configuration(expected) {
            return Err("sandbox container configuration mismatch; refusing replacement".into());
        }
        if has_execution_fact {
            return if facts.running {
                Ok(())
            } else {
                Err("execution_environment_lost:container_stopped".into())
            };
        }
    } else if has_execution_fact {
        return Err("execution_environment_lost:container_missing".into());
    }

    let id = crate::docker_engine::ensure_created(expected.name, body)?;
    let prepared = (|| {
        crate::docker_engine::start(&id)?;
        let facts =
            inspect_container(&id)?.ok_or("sandbox container disappeared after creation")?;
        if !facts.matches(expected) {
            return Err("sandbox container creation verification failed".to_string());
        }
        Ok(())
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
    environment_container::create_body(
        expected,
        expected
            .environment_profile
            .ok_or("environment_profile_required")?,
        expected
            .benchmark_helper
            .ok_or("environment_helper_required")?,
    )
}

fn container_name(execution_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(execution_id.as_bytes()));
    format!("centaeris-agent-run-{}", &digest[..24])
}

fn container_ids_for_agent_run(agent_run_id: &str) -> Result<Vec<String>, String> {
    crate::docker_engine::list_owned(agent_run_id)
}

fn resident_host_id() -> Result<String, String> {
    crate::docker_engine::info()?["ID"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Docker daemon identity missing; resident admission blocked".into())
}

pub(crate) fn resident_host_context() -> Result<(String, ResidentCapacity), String> {
    let info = crate::docker_engine::info()?;
    let id = info["ID"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or("Docker daemon identity missing; resident admission blocked")?
        .to_string();
    let memory = info["MemTotal"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or("Docker daemon memory capacity missing")?;
    let cpus = info["NCPU"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or("Docker daemon CPU capacity missing")?;
    let memory_headroom = crate::positive_u64_env(
        "RESIDENT_HOST_MEMORY_HEADROOM_BYTES",
        (memory / 5).max(512 * 1024 * 1024),
    )?;
    let cpu_headroom = crate::positive_u64_env(
        "RESIDENT_HOST_CPU_HEADROOM_MILLI",
        cpus.checked_mul(100)
            .ok_or("resident CPU headroom overflow")?,
    )?;
    Ok((
        id,
        ResidentCapacity::from_env()?.bound_to_host(memory, cpus, memory_headroom, cpu_headroom)?,
    ))
}

pub(crate) fn observed_resident_sandboxes() -> Result<Vec<ObservedResidentSandbox>, String> {
    observe_resident_container_ids(crate::docker_engine::list_managed(None)?)
}

fn observed_resident_sandboxes_for_run(
    agent_run_id: &str,
) -> Result<Vec<ObservedResidentSandbox>, String> {
    observe_resident_container_ids(crate::docker_engine::list_owned(agent_run_id)?)
}

fn observe_resident_container_ids(
    ids: Vec<String>,
) -> Result<Vec<ObservedResidentSandbox>, String> {
    ids.into_iter()
        .filter_map(|id| match inspect_container(&id) {
            Ok(None) => None,
            Ok(Some(facts)) => Some(observed_resident_sandbox(&facts)),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn observed_resident_sandbox(facts: &ContainerFacts) -> Result<ObservedResidentSandbox, String> {
    let label = |name: &str| {
        facts
            .labels
            .get(name)
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .ok_or_else(|| format!("managed sandbox {name} missing; resident admission blocked"))
    };
    if facts.labels.get("centaeris.managed").map(String::as_str) != Some("true")
        || facts.memory == 0
        || facts.cpu_period != 100_000
        || facts.cpu_quota <= 0
        || facts.cpu_quota % 100 != 0
        || facts.pids_limit.is_none_or(|value| value <= 0)
    {
        return Err(
            "managed sandbox has unbounded or unknown resources; resident admission blocked".into(),
        );
    }
    let benchmark_budget = facts
        .labels
        .get("centaeris.environment_profile_digest")
        .and_then(|_| facts.labels.get("centaeris.data_budget"))
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0);
    let tmpfs = benchmark_budget
        .or_else(|| {
            facts.raw["HostConfig"]["Tmpfs"][WORKSPACE_DATA_ROOT]
                .as_str()
                .and_then(|options| {
                    options
                        .split(',')
                        .find_map(|option| option.strip_prefix("size="))
                })
                .and_then(|size| size.parse::<u64>().ok())
                .filter(|value| *value > 0)
        })
        .ok_or("managed sandbox workspace resource limit unknown; resident admission blocked")?;
    Ok(ObservedResidentSandbox {
        execution_id: label("centaeris.execution_id")?,
        allocation_owner: serde_json::from_str(&label("centaeris.allocation_owner")?)
            .map_err(|_| "managed sandbox owner invalid")?,
        container_id: facts.id.clone(),
        resources: centaeris_core::execution::ResidentResourceUsage {
            sandbox_count: 1,
            memory_bytes: facts.memory,
            cpu_milli: (facts.cpu_quota as u64) / 100,
            pids: facts.pids_limit.unwrap_or_default() as u64,
            workspace_bytes: tmpfs,
        },
    })
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
    volume_options: Option<ContainerVolumeOptions>,
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
            && (if let Some(owner) = expected.allocation_owner {
                self.labels
                    .get("centaeris.allocation_owner")
                    .and_then(|v| {
                        serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(v)
                            .ok()
                    })
                    .as_ref()
                    == Some(owner)
            } else {
                self.has_identity(expected.agent_run_id, expected.execution_id)
            })
            && self.image == expected.image_digest
            && self.runtime == expected.oci_runtime.docker_runtime_name()
            && self.memory == expected.resources.memory_bytes
            && self.cpu_period == 100_000
            && self.cpu_quota == i64::from(expected.resources.cpu_milli) * 100
            && self.pids_limit == Some(i64::from(expected.resources.pids_limit))
            && !self.readonly_rootfs
            && self.network_mode == network::mode(expected.network)
            && plugin_mounts_match(
                self.mounts.as_slice(),
                expected.plugin_volume_name,
                expected.plugin_mounts,
            )
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
                    && mount
                        .volume_options
                        .as_ref()
                        .is_some_and(|options| options.subpath == expected.package_name)
            })
        })
}

#[cfg(test)]
fn memory_mount_matches(actual: &[ContainerMountFacts], expected: &MemoryMount) -> bool {
    let mounts = actual
        .iter()
        .filter(|mount| mount.target == MEMORY_CONTAINER_ROOT)
        .collect::<Vec<_>>();
    mounts.len() == 1
        && mounts[0].mount_type == "volume"
        && mounts[0].source == expected.volume_name
        && !mounts[0].read_only
        && mounts[0]
            .volume_options
            .as_ref()
            .is_some_and(|options| options.subpath == expected.scope_key)
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

#[cfg(test)]
struct ContainerObservation {
    container: String,
    labels: BTreeMap<String, String>,
    command: Vec<String>,
    user: Option<String>,
}

#[cfg(test)]
struct ExecOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
}

#[cfg(test)]
fn exec_with_input(
    request: &ContainerObservation,
    input: &[u8],
    output_limit: usize,
) -> Result<ExecOutput, String> {
    // This fixture only seeds or observes explicitly owned physical test state.
    // Bind immutable daemon identity before the CLI touches that container.
    let facts =
        crate::docker_engine::inspect(&request.container)?.ok_or("observation container absent")?;
    if facts["State"]["Running"] != true
        || !request.labels.iter().all(|(key, expected)| {
            facts["Config"]["Labels"][key].as_str() == Some(expected.as_str())
        })
    {
        return Err("observation container owner mismatch".into());
    }
    let id = facts["Id"]
        .as_str()
        .filter(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or("observation container identity missing")?;
    let mut command = Command::new("docker");
    command.args(["exec", "--interactive"]);
    if let Some(user) = &request.user {
        command.args(["--user", user.as_str()]);
    }
    let mut child = command
        .arg(id)
        .args(&request.command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let stdout = child.stdout.take().ok_or("exec stdout missing")?;
    let stderr = child.stderr.take().ok_or("exec stderr missing")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, output_limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, DOCKER_DIAGNOSTIC_LIMIT));
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).map_err(|e| e.to_string())?;
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    let stdout = stdout_reader
        .join()
        .map_err(|_| "exec stdout reader panicked")??;
    stderr_reader
        .join()
        .map_err(|_| "exec stderr reader panicked")??;
    if stdout.total_bytes > output_limit {
        return Err("exec helper exceeded output limit".into());
    }
    Ok(ExecOutput {
        status,
        stdout: stdout.bytes,
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
        workspace_root: policy
            .filesystem
            .workspace_root
            .to_string_lossy()
            .into_owned(),
        read_only_root_count: policy.filesystem.read_only_roots.len(),
        writable_root_count: policy.filesystem.writable_roots.len(),
        denied_read_path_count: policy.filesystem.denied_read_paths.len(),
        denied_write_path_count: policy.filesystem.denied_write_paths.len(),
    }
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
fn normalize_model_path(
    path: &str,
    operation: &ExecutionFileSystemOperation,
) -> Result<String, ExecutionFileSystemError> {
    normalize_model_path_at(WORKSPACE_DATA_ROOT, path, operation)
}
fn normalize_model_path_at(
    root: &str,
    path: &str,
    operation: &ExecutionFileSystemOperation,
) -> Result<String, ExecutionFileSystemError> {
    if matches!(
        operation,
        ExecutionFileSystemOperation::ListDirectory { .. }
    ) && (path == "." || path == root)
    {
        return Ok(".".into());
    }
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let path = path.strip_prefix(&prefix).unwrap_or(path);
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceSnapshotDescriptor {
    sha256: String,
    size_bytes: u64,
    expanded_size_bytes: u64,
    file_count: u32,
}

struct ImmutableWorkspaceSnapshot {
    file: fs::File,
    descriptor: WorkspaceSnapshotDescriptor,
    observation: WorkspaceProtection,
    manifest: Option<SandboxWorkspaceSnapshotManifest>,
}

pub(crate) struct PreparedRecoveryWorkspaceRestore {
    artifact: Option<ImmutableWorkspaceSnapshot>,
    resolution: SessionWorkspaceResolution,
}

impl PreparedRecoveryWorkspaceRestore {
    pub(crate) fn resolution(&self) -> SessionWorkspaceResolution {
        self.resolution
    }
}

pub(crate) struct RecoveryRestoreBinding<'a> {
    pub api_url: &'a str,
    pub api_token: &'a str,
    pub agent_run_id: &'a str,
    pub authorization_digest: &'a str,
    pub lease: &'a SessionWorkspaceLease,
    pub checkpoint_id: &'a str,
    pub data_tmpfs_bytes: u64,
    pub input_upper_bound_bytes: u64,
}

pub(crate) fn prepare_recovery_workspace_restore(
    binding: RecoveryRestoreBinding<'_>,
    snapshot: &RecoveryWorkspaceSnapshotV1,
    session_workspace: Option<&SessionWorkspace>,
) -> Result<PreparedRecoveryWorkspaceRestore, SessionWorkspaceApiError> {
    snapshot
        .validate()
        .map_err(SessionWorkspaceApiError::Rejected)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| SessionWorkspaceApiError::Unavailable(error.to_string()))?;
    let session_request = |schema| SessionWorkspaceLeaseRequest {
        schema,
        job_id: &binding.lease.job_id,
        lease_owner: &binding.lease.lease_owner,
        agent_run_id: binding.agent_run_id,
        authorization_digest: binding.authorization_digest,
    };
    if let Some(frozen) = session_workspace {
        let response = client
            .post(format!(
                "{}/internal/agent-runs/session-workspace/resolve",
                binding.api_url
            ))
            .header("X-Internal-Token", binding.api_token)
            .json(&session_request(SESSION_WORKSPACE_RESOLVE_SCHEMA))
            .timeout(Duration::from_secs(10))
            .send()
            .map_err(|error| SessionWorkspaceApiError::Unavailable(error.to_string()))?;
        if !response.status().is_success() {
            return Err(SessionWorkspaceApiError::Unavailable(format!(
                "recovery session workspace resolve returned {}",
                response.status()
            )));
        }
        let resolved = response
            .json::<SessionWorkspaceResolveResponse>()
            .map_err(|error| SessionWorkspaceApiError::Rejected(error.to_string()))?;
        let resolution = resolved_resolution(&resolved, frozen)?;
        if resolution == SessionWorkspaceResolution::Advanced {
            return Ok(PreparedRecoveryWorkspaceRestore {
                artifact: None,
                resolution,
            });
        }
    }
    require_workspace_capacity_for_limit(
        snapshot.expanded_size_bytes,
        binding.input_upper_bound_bytes,
        binding.data_tmpfs_bytes,
    )
    .map_err(SessionWorkspaceApiError::Rejected)?;
    if snapshot.object_ref.is_none() {
        return Ok(PreparedRecoveryWorkspaceRestore {
            artifact: None,
            resolution: SessionWorkspaceResolution::Empty,
        });
    }
    let request = if session_workspace.is_some() {
        client
            .post(format!(
                "{}/internal/agent-runs/session-workspace/download",
                binding.api_url
            ))
            .json(&session_request(SESSION_WORKSPACE_DOWNLOAD_SCHEMA))
    } else {
        client
            .post(format!(
                "{}/internal/agent-runs/execution-workspace/download",
                binding.api_url
            ))
            .json(&ExecutionWorkspaceDownloadRequest {
                schema: EXECUTION_WORKSPACE_DOWNLOAD_SCHEMA,
                job_id: &binding.lease.job_id,
                lease_owner: &binding.lease.lease_owner,
                agent_run_id: binding.agent_run_id,
                authorization_digest: binding.authorization_digest,
                checkpoint_id: binding.checkpoint_id,
            })
    };
    let mut response = request
        .header("X-Internal-Token", binding.api_token)
        .timeout(Duration::from_secs(30))
        .send()
        .map_err(|error| SessionWorkspaceApiError::Unavailable(error.to_string()))?;
    if !response.status().is_success() {
        return Err(SessionWorkspaceApiError::Unavailable(format!(
            "recovery workspace download returned {}",
            response.status()
        )));
    }
    if response.content_length() != Some(snapshot.snapshot_size_bytes)
        || response
            .headers()
            .get("x-content-sha256")
            .and_then(|value| value.to_str().ok())
            != Some(snapshot.snapshot_sha256.as_str())
    {
        return Err(SessionWorkspaceApiError::Rejected(
            "recovery workspace download binding mismatch".into(),
        ));
    }
    let observation = WorkspaceProtection::new().with_context(
        ProtectionTrigger::RecoveryRestore,
        binding.agent_run_id,
        Some(&binding.lease.job_id),
    );
    let artifact = prepare_recovery_restore_artifact(&mut response, snapshot, observation)?;
    Ok(PreparedRecoveryWorkspaceRestore {
        artifact: Some(artifact),
        resolution: SessionWorkspaceResolution::Download,
    })
}

fn prepare_recovery_restore_artifact(
    source: &mut impl Read,
    snapshot: &RecoveryWorkspaceSnapshotV1,
    observation: WorkspaceProtection,
) -> Result<ImmutableWorkspaceSnapshot, SessionWorkspaceApiError> {
    let artifact = observation
        .download(source, |source| {
            capture_snapshot_artifact_observed(
                source,
                snapshot.snapshot_size_bytes,
                observation.clone(),
            )
        })
        .map_err(SessionWorkspaceApiError::Unavailable)?;
    if !snapshot_matches_recovery_workspace(&artifact.descriptor, snapshot) {
        return Err(SessionWorkspaceApiError::Rejected(
            "recovery workspace artifact binding mismatch".into(),
        ));
    }
    Ok(artifact)
}

fn require_workspace_capacity_for_limit(
    expanded_size_bytes: u64,
    input_upper_bound_bytes: u64,
    limit: u64,
) -> Result<(), String> {
    if expanded_size_bytes
        .checked_add(input_upper_bound_bytes)
        .and_then(|value| value.checked_add(SESSION_WORKSPACE_RESTORE_OVERHEAD_BYTES))
        .is_none_or(|value| value > limit)
    {
        return Err("session workspace exceeds sandbox dataTmpfsBytes".into());
    }
    Ok(())
}

fn resolved_resolution(
    resolved: &SessionWorkspaceResolveResponse,
    frozen: &SessionWorkspace,
) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
    if resolved.schema != SESSION_WORKSPACE_RESOLVED_SCHEMA {
        return Err(SessionWorkspaceApiError::Rejected(
            "session workspace resolve schema mismatch".into(),
        ));
    }
    match resolved.disposition.as_str() {
        "empty" if &resolved.session_workspace == frozen && frozen.snapshot_size_bytes == 0 => {
            Ok(SessionWorkspaceResolution::Empty)
        }
        "download" if &resolved.session_workspace == frozen && frozen.snapshot_size_bytes != 0 => {
            Ok(SessionWorkspaceResolution::Download)
        }
        "advanced"
            if Some(resolved.session_workspace.generation) == frozen.generation.checked_add(1)
                && resolved.session_workspace.validate().is_ok() =>
        {
            Ok(SessionWorkspaceResolution::Advanced)
        }
        _ => Err(SessionWorkspaceApiError::Rejected(
            "session workspace resolve binding mismatch".into(),
        )),
    }
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

struct SnapshotSpoolReader<'a, R, W> {
    source: &'a mut R,
    file: &'a mut W,
    remaining: u64,
}

impl<R: Read, W: Write> Read for SnapshotSpoolReader<'_, R, W> {
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

#[cfg(test)]
fn capture_snapshot_artifact(
    source: &mut impl Read,
    limit: u64,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    capture_snapshot_artifact_observed(source, limit, WorkspaceProtection::new())
}

fn capture_snapshot_artifact_observed(
    source: &mut impl Read,
    limit: u64,
    observation: WorkspaceProtection,
) -> Result<ImmutableWorkspaceSnapshot, String> {
    let mut file =
        tempfile::tempfile().map_err(|error| format!("create snapshot spool failed: {error}"))?;
    let mut measured = MeasuredWriter {
        inner: &mut file,
        written: 0,
    };
    let descriptor = inspect_workspace_snapshot_with_manifest(&mut SnapshotSpoolReader {
        source,
        file: &mut measured,
        remaining: limit,
    });
    observation.count(
        ProtectionMetric::SpoolWrittenBytes,
        measured.written,
        descriptor.is_ok(),
    );
    let (descriptor, manifest) = descriptor?;
    file.rewind()
        .map_err(|error| format!("rewind snapshot spool failed: {error}"))?;
    Ok(ImmutableWorkspaceSnapshot {
        file,
        descriptor,
        observation,
        manifest,
    })
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

#[cfg(test)]
fn inspect_workspace_snapshot(
    source: &mut impl Read,
) -> Result<WorkspaceSnapshotDescriptor, String> {
    inspect_workspace_snapshot_with_manifest(source).map(|(descriptor, _)| descriptor)
}

#[cfg(test)]
pub(crate) fn inspect_workspace_snapshot_manifest_for_test(
    source: &mut impl Read,
) -> Result<Option<SandboxWorkspaceSnapshotManifest>, String> {
    inspect_workspace_snapshot_with_manifest(source).map(|(_, manifest)| manifest)
}

fn inspect_workspace_snapshot_with_manifest(
    source: &mut impl Read,
) -> Result<
    (
        WorkspaceSnapshotDescriptor,
        Option<SandboxWorkspaceSnapshotManifest>,
    ),
    String,
> {
    let mut header = [0_u8; 4];
    let first = source
        .read(&mut header)
        .map_err(|error| format!("read snapshot collect frame failed: {error}"))?;
    if first == 0 {
        return Ok((
            WorkspaceSnapshotDescriptor {
                sha256: String::new(),
                size_bytes: 0,
                expanded_size_bytes: 0,
                file_count: 0,
            },
            None,
        ));
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
    Ok((
        WorkspaceSnapshotDescriptor {
            sha256: format!("sha256:{:x}", digest.finalize()),
            size_bytes,
            expanded_size_bytes,
            file_count: u32::try_from(manifest.files.len())
                .map_err(|_| "snapshot file count overflow".to_string())?,
        },
        Some(manifest),
    ))
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

pub(crate) fn validate_workspace_snapshot_manifest(
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
    pub(super) fn retained_test_runner() -> DockerExecutionHostRunner {
        let mut runner = test_docker_execution_host_runner();
        let origin_execution = "execution-origin";
        let allocation = crate::postgres_store::execution_instances::RetainedInstanceAllocation {
            allocation_execution_id: origin_execution.into(),
            allocation_agent_run_id: Some("run-origin".into()),
            allocation_owner: centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                run_id: "run-origin".into(),
            },
            instance_id: crate::postgres_store::execution_instances::instance_id(origin_execution),
            environment_id: runner.environment_id.clone(),
            host_id: "host".into(),
            image_digest: runner.image_digest.clone(),
            authorization_digest: runner.authorization_digest.clone(),
            binding_digest: runner.binding_digest.clone(),
            profile_digest: format!("sha256:{}", "e".repeat(64)),
            container_id: Some("origin-container".into()),
            reused: true,
        };
        runner.container_name = container_name(origin_execution);
        runner.agent_run_id = "run-controller".into();
        runner.execution_id = "execution-controller".into();
        runner.authorization_digest = format!("sha256:{}", "d".repeat(64));
        let mut authority: crate::environment_authorization::EnvironmentAuthorization =
            serde_json::from_str(include_str!(
                "../../../tests/workspace/fixtures/environment_authorization/v1/retained.json"
            ))
            .unwrap();
        authority.payload.environment_id = runner.environment_id.clone();
        authority.payload.agent_run_id = runner.agent_run_id.clone();
        authority.payload.agent_run_authorization_digest = runner.authorization_digest.clone();
        authority.payload.controller.as_mut().unwrap().controller_id = runner.agent_run_id.clone();
        runner.environment_authorization = Some(authority);
        runner.retained_allocation = Some(allocation);
        runner
    }

    #[test]
    fn container_payload_keeps_allocation_owner_after_controller_takeover() {
        let runner = retained_test_runner();
        let expected = runner.container_expectation();
        let body = container_create_body(&expected).unwrap();
        assert_eq!(body["Labels"]["centaeris.agent_run_id"], "run-origin");
        assert_eq!(body["Labels"]["centaeris.execution_id"], "execution-origin");
        assert_eq!(
            body["Labels"]["centaeris.instance_id"],
            runner.retained_allocation.as_ref().unwrap().instance_id
        );
        assert_eq!(body["Cmd"][1], "environment-keeper");
        assert_eq!(
            serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(
                body["Labels"]["centaeris.allocation_owner"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                run_id: "run-origin".into()
            }
        );
        assert_eq!(runner.agent_run_id, "run-controller");
        assert_eq!(runner.execution_id, "execution-controller");
    }

    #[test]
    fn retained_profile_preserves_configuration_without_controller_identity() {
        let mut runner = retained_test_runner();
        let original: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
            serde_json::from_str(include_str!(
                "../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
            ))
            .unwrap();
        runner.environment_profile = Some(original.environment_profile.clone());
        let expected = runner.container_expectation();
        let first = retained::profile_digest(&expected, &original.plugin_activation).unwrap();
        let mut next = expected;
        next.agent_run_id = "another-run";
        next.execution_id = "another-execution";
        next.binding_digest = "another-binding";
        assert_eq!(
            first,
            retained::profile_digest(&next, &original.plugin_activation).unwrap()
        );
        let mut changed = original.environment_profile.clone();
        changed.resources.memory_bytes += 1;
        next.environment_profile = Some(&changed);
        assert_ne!(
            first,
            retained::profile_digest(&next, &original.plugin_activation).unwrap()
        );
        let mut invalid = original.plugin_activation;
        invalid.schema = "unsupported".into();
        assert!(retained::profile_digest(&expected, &invalid).is_err());
    }

    #[test]
    fn retained_old_dispatch_and_container_removal_are_rejected_before_io() {
        let runner = retained_test_runner();
        assert!(runner.keeps_instance());
        assert!(runner.is_hot_reuse());
        let request = Arc::new(retained_test_runner())
            .mcp_exec_request("banana", "main", &[])
            .unwrap();
        assert_eq!(request.program, "/opt/centaeris/plugins/banana/main");
        assert!(
            runner.helper("input-inventory", None, b"{}", 100).is_err(),
            "helper requires current persistent authority"
        );
        assert_eq!(
            runner.teardown_owned().unwrap_err(),
            "retained instance removal requires explicit cleanup authority"
        );
    }

    #[test]
    fn retained_commands_reject_unsupported_policy_before_controller_or_rpc_access() {
        // This runner has no authorization/store and cannot perform any RPC.
        // Policy denial must precede even attempting to obtain that authority.
        let runner = retained_test_runner();
        for policy in [
            ExecutionPolicy::workspace_write_public_internet(WORKSPACE_DATA_ROOT),
            ExecutionPolicy::workspace_write_no_network("/another-workspace"),
        ] {
            let request = ExecutionCommandRequest {
                program: "/usr/bin/true".into(),
                args: Vec::new(),
                cwd: WORKSPACE_DATA_ROOT.into(),
                env: HashMap::new(),
                timeout_ms: 1_000,
                policy,
            };
            assert!(matches!(
                runner.run_host_command(Some("policy-check"), request, None),
                Err(ExecutionError::Denied { reason }) if reason == "environment policy differs from signed profile"
            ));
        }
    }

    #[test]
    fn model_policy_rejects_roots_outside_signed_workspace() {
        let runner = retained_test_runner();
        let mut policy = runner.execution_policy();
        policy.filesystem.writable_roots.push("/".into());
        assert!(matches!(
            runner.validate_policy(&policy),
            Err(ExecutionError::Denied { .. })
        ));
    }
    #[test]
    fn protected_input_path_uses_the_signed_workspace_root() {
        let mut runner = test_docker_execution_host_runner();
        let mut profile: EnvironmentProfile = serde_json::from_value(
            serde_json::from_str::<serde_json::Value>(include_str!(
                "../../../tests/workspace/fixtures/agent_run_authorization/v1/custom.json"
            ))
            .unwrap()["environmentProfile"]
                .clone(),
        )
        .unwrap();
        profile.workspace_root = "/app".into();
        runner.environment_profile = Some(profile);
        let read = ExecutionFileSystemOperation::ReadFile { max_bytes: 32 };
        assert!(runner
            .protected_input_error("/app/reports/input.txt", &read)
            .unwrap()
            .is_none());
        assert!(runner
            .protected_input_error("/outside/input.txt", &read)
            .is_err());
    }

    #[test]
    fn retained_terminal_collection_defers_without_quiescing_environment_tasks() {
        let runner = retained_test_runner();
        assert_eq!(
            runner
                .with_terminal_collection(|_| panic!("retained final collection must not run"))
                .unwrap(),
            SessionWorkspaceCommitOutcome::ProtectionDeferred
        );
    }

    #[test]
    fn retained_hot_workspace_uses_live_files_and_recovery_capture_is_explicitly_unavailable() {
        let runner = retained_test_runner();
        let lease = SessionWorkspaceLease {
            job_id: "job".into(),
            lease_owner: "owner".into(),
        };
        let frozen = SessionWorkspace {
            generation: 99,
            snapshot_sha256: "old-snapshot".into(),
            snapshot_size_bytes: 1,
            expanded_size_bytes: 1,
            file_count: 1,
        };
        assert_eq!(
            runner
                .restore_session_workspace(&lease, &frozen, 0)
                .unwrap(),
            SessionWorkspaceResolution::RetainedLive
        );
        assert_eq!(
            runner
                .capture_sealed_recovery_workspace_with_observer::<()>(
                    |_| panic!("retained capture observer must not run"),
                    |_| panic!("retained capture collector must not run")
                )
                .unwrap_err(),
            "retained_consistent_capture_unavailable"
        );
    }

    #[test]
    #[ignore = "requires explicitly isolated Postgres, dedicated Docker volumes and an execution_agent image"]
    fn real_docker_checkpoint_recovery_releases_only_confirmed_old_residency_and_restores_one_slot()
    {
        use centaeris_core::runtime::contracts::{
            CheckpointKindV1, CheckpointRecord, RuntimeRecoveryCheckpointV1,
            RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1,
        };
        use centaeris_core::session::{store::RuntimeStore, AgentRunSessionState};
        use postgres::{Client, NoTls};
        use std::net::TcpListener;
        assert_eq!(
            env::var("CENTAERIS_RECOVERY_DOCKER_TEST").as_deref(),
            Ok("isolated")
        );
        let url = env::var("CENTAERIS_RECOVERY_DOCKER_POSTGRES_URL")
            .expect("dedicated Postgres required");
        let mut db = Client::connect(&url, NoTls).unwrap();
        assert_eq!(
            db.query_one("SELECT current_database()", &[])
                .unwrap()
                .get::<_, String>(0),
            "centaeris_recovery_docker_test"
        );
        for name in [PLUGIN_VOLUME_NAME_ENV, AGENT_MEMORY_VOLUME_NAME_ENV] {
            assert!(
                docker_volume_name_from_environment(name)
                    .unwrap()
                    .starts_with("centaeris-recovery-test-"),
                "dedicated test volumes required"
            );
        }
        let marker = Path::new(MEMORY_RUNTIME_VOLUME_ROOT).join(".centaeris-recovery-test-volume");
        assert!(!fs::symlink_metadata(&marker)
            .expect("dedicated memory root marker required")
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(marker).unwrap().trim(),
            docker_volume_name_from_environment(AGENT_MEMORY_VOLUME_NAME_ENV).unwrap()
        );
        let image = parse_docker_image_digest(
            env::var("CENTAERIS_RECOVERY_DOCKER_IMAGE_DIGEST")
                .expect("local execution_agent image digest required")
                .as_bytes(),
        )
        .unwrap();
        let run = "centaeris-recovery-test-run";
        let old_id = "centaeris-recovery-test-old";
        let digest = format!("sha256:{}", "a".repeat(64));
        let checkpoint_id = "centaeris-recovery-test-checkpoint";
        let mut start = crate::tests::agent_run_start();
        start.agent_run_id = run.into();
        start.authorization.workspace_id = "centaeris-recovery-test-tenant".into();
        start.authorization.session_id = "centaeris-recovery-test-session".into();
        start.authorization_digest = digest.clone();
        let replacement_key = crate::replacement_execution_id(&start, checkpoint_id, 1);
        let replacement_id = replacement_key.as_str();
        let late_id = "centaeris-recovery-test-late";
        assert!(
            container_ids_for_agent_run(run).unwrap().is_empty(),
            "test run identity already occupied"
        );
        for execution in [old_id, replacement_id, late_id] {
            assert!(
                inspect_container(&container_name(execution))
                    .unwrap()
                    .is_none(),
                "test execution identity already occupied"
            );
        }
        let observed = observed_resident_sandboxes().unwrap();
        db.batch_execute("DROP SCHEMA IF EXISTS runtime CASCADE; CREATE TABLE IF NOT EXISTS public.app_core_agentrun(id text PRIMARY KEY,session_id text NOT NULL,workspace_id text NOT NULL,status text NOT NULL); DELETE FROM public.app_core_agentrun WHERE id='centaeris-recovery-test-run'; INSERT INTO public.app_core_agentrun(id,session_id,workspace_id,status) VALUES('centaeris-recovery-test-run','centaeris-recovery-test-session','centaeris-recovery-test-tenant','running');").unwrap();
        db.batch_execute("ALTER TABLE public.app_core_agentrun ADD COLUMN IF NOT EXISTS environment_id text; UPDATE public.app_core_agentrun SET environment_id='environment_test' WHERE id='centaeris-recovery-test-run'; CREATE TABLE IF NOT EXISTS public.app_core_agentrunauthorization(agent_run_id text PRIMARY KEY,digest text NOT NULL);").unwrap();
        db.execute("INSERT INTO public.app_core_agentrunauthorization VALUES($1,$2) ON CONFLICT(agent_run_id) DO UPDATE SET digest=EXCLUDED.digest", &[&run,&digest]).unwrap();
        let store = Arc::new(PostgresRuntimeStore::new(&url).unwrap());
        let owner = "centaeris-recovery-test-owner";
        let job = centaeris_core::session::reliability::agent_run_lifecycle_job_id(run).unwrap();
        let now = db
            .query_one(
                "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
                &[],
            )
            .unwrap()
            .get::<_, i64>(0);
        let backoff = serde_json::to_string(
            &centaeris_core::session::reliability::RuntimeBackoffPolicy::default(),
        )
        .unwrap();
        let mut bind = |run: &str, tenant: &str| {
            let job =
                centaeris_core::session::reliability::agent_run_lifecycle_job_id(run).unwrap();
            db.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,run_at_ms,lease_owner,lease_expires_at_ms,backoff_policy_json,idempotency_key,created_at_ms,updated_at_ms) VALUES($1,'agent_run.lifecycle','running',$2::bigint,$3,$2::bigint+600000,$4,$1,$2::bigint,$2::bigint) ON CONFLICT(job_id) DO NOTHING", &[&job, &now, &owner, &backoff]).unwrap();
            db.execute("INSERT INTO runtime.execution_job_tenants VALUES($1,$2) ON CONFLICT(job_id) DO NOTHING", &[&job, &tenant]).unwrap();
        };
        // Other daemon holdings are observed and charged only in this isolated database.
        for sandbox in observed {
            bind(
                &match &sandbox.allocation_owner {
                    centaeris_core::execution::EnvironmentTaskOwner::AgentRun { run_id } => {
                        run_id.clone()
                    }
                    _ => panic!("expected Run fixture"),
                },
                "centaeris-recovery-test-observed-tenant",
            );
        }
        bind(run, "centaeris-recovery-test-tenant");
        db.execute(
            "UPDATE runtime.runtime_jobs SET session_id=$2 WHERE job_id=$1",
            &[&job, &start.authorization.session_id],
        )
        .unwrap();
        db.batch_execute("DROP TABLE IF EXISTS public.app_core_sessionevent; DROP TABLE IF EXISTS public.app_core_session CASCADE; CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL); CREATE TABLE public.app_core_sessionevent(\"eventId\" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,\"createdAtMs\" bigint NOT NULL,\"insertedAt\" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence)); INSERT INTO public.app_core_session VALUES('centaeris-recovery-test-session','centaeris-recovery-test-tenant');").unwrap();
        let defaults = crate::resident_capacity::ResidentCapacity::default();
        let configured = crate::resident_capacity::ResidentCapacity::from_env().unwrap();
        assert_eq!(configured.global, defaults.global);
        assert_eq!(configured.tenant, defaults.tenant);
        let example = include_str!("../../../.env.example");
        let setting = |name: &str| {
            example
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}=")))
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        let resources = SandboxResources {
            memory_bytes: setting("SANDBOX_MEMORY_BYTES"),
            cpu_milli: setting("SANDBOX_CPU_MILLI") as u32,
            pids_limit: setting("SANDBOX_PIDS_LIMIT") as u32,
            data_tmpfs_bytes: setting("SANDBOX_DATA_TMPFS_BYTES"),
        };
        let activation = centaeris_core::extension::build_plugin_activation_snapshot(&[]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api_url = format!("http://{}", listener.local_addr().unwrap());
        let create =
            |execution: &str,
             lease_owner: &str,
             resident_store: Arc<PostgresRuntimeStore>,
             before: &mut (dyn FnMut(&mut Client) -> Result<(), String> + Send)| {
                DockerExecutionHostRunner::new_with_admitted(
                    DockerExecutionHostRequest {
                        network: ExecutionNetwork::Disabled,
                        environment_profile: None,
                        workspace_id: "centaeris-recovery-test-tenant".into(),
                        lifecycle_job_id: job.clone(),
                        lifecycle_lease_owner: lease_owner.into(),
                        resident_store,
                        agent_run_id: run.into(),
                        execution_id: execution.into(),
                        user_id: "centaeris-recovery-test-user".into(),
                        agent_id: "centaeris-recovery-test-agent".into(),
                        authorization_digest: digest.clone(),
                        environment_authorization:
                            crate::environment_authorization::EnvironmentAuthorization {
                                signature: "fixture".into(),
                                payload: crate::environment_authorization::EnvironmentBinding {
                                    schema: "workspace.environment_authorization.v1".into(),
                                    environment_id: "environment_test".into(),
                                    session_id: "centaeris-recovery-test-session".into(),
                                    agent_run_id: run.into(),
                                    agent_run_authorization_digest: digest.clone(),
                                    execution_mode: "perRun".into(),
                                    controller: None,
                                },
                            },
                        image_digest: image.clone(),
                        resources,
                        has_execution_fact: false,
                        api_url: api_url.clone(),
                        api_token: "centaeris-recovery-test-token".into(),
                        plugin_activation: &activation,
                    },
                    before,
                )
            };
        let charged = || {
            Client::connect(&url, NoTls)
                .unwrap()
                .query_one(
                    "SELECT COUNT(*) FROM runtime.resident_sandboxes WHERE agent_run_id=$1",
                    &[&run],
                )
                .unwrap()
                .get::<_, i64>(0)
        };
        let old = create(old_id, owner, store.clone(), &mut |_| Ok(())).unwrap();
        assert_eq!(charged(), 1);
        assert_eq!(
            create(replacement_id, owner, store.clone(), &mut |_| Ok(()))
                .err()
                .unwrap(),
            crate::resident_capacity::RESIDENT_CAPACITY_WAIT
        );
        let mut write_request = old.exec_request(vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf 'checkpoint contents' > /mnt/data/report.txt".into(),
        ]);
        write_request.user = Some(AGENT_USER.into());
        let written = exec_with_input(&write_request, &[], 1024).unwrap();
        assert!(written.status.success());
        let capture = old.recovery_activity.wait_snapshot_activity().unwrap();
        let SnapshotCaptureResult::Snapshot(mut artifact) = old
            .capture_snapshot(
                &capture,
                &WorkspaceSnapshotDescriptor::from(&empty_recovery_snapshot()),
                ProtectionTrigger::RecoveryCheckpoint,
                &job,
            )
            .unwrap()
        else {
            panic!("a fresh runner must capture a complete snapshot");
        };
        drop(capture);
        let snapshot = RecoveryWorkspaceSnapshotV1 {
            object_ref: Some("centaeris-recovery-test-checkpoint-object".into()),
            snapshot_sha256: artifact.descriptor.sha256.clone(),
            snapshot_size_bytes: artifact.descriptor.size_bytes,
            expanded_size_bytes: artifact.descriptor.expanded_size_bytes,
            file_count: artifact.descriptor.file_count,
        };
        let mut body = Vec::new();
        artifact.file.read_to_end(&mut body).unwrap();
        let checkpoint = RuntimeRecoveryCheckpointV1 {
            schema: RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1.into(),
            checkpoint_id: checkpoint_id.into(),
            session_id: start.authorization.session_id.clone(),
            agent_run_id: run.into(),
            execution_id: old_id.into(),
            authorization_digest: digest.clone(),
            session_sequence: 3,
            model_request_id: "centaeris-recovery-test-request".into(),
            workspace_snapshot: snapshot.clone(),
            workspace_generation: ExecutionWorkspaceGeneration::Unknown {
                reason: "explicit test capture".into(),
            },
            created_at_ms: 3,
        };
        let checkpoint_record = CheckpointRecord {
            checkpoint_id: checkpoint_id.into(),
            kind: CheckpointKindV1::Recovery,
            session_id: start.authorization.session_id.clone(),
            turn_id: start.turn_id.clone(),
            status: "committed".into(),
            done_reason: None,
            updated_at_ms: 3,
            payload_json: serde_json::to_string(&checkpoint).unwrap(),
        };
        store.save_checkpoint(checkpoint_record.clone()).unwrap();
        let fence = centaeris_core::session::RuntimeJobLeaseFence {
            job_id: job.clone(),
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: owner.into(),
        };
        let log = store.session_log(
            start.authorization.workspace_id.clone(),
            start.authorization.session_id.clone(),
            "recover".into(),
        );
        let mut sequence = AgentRunSessionState::new(&start.authorization.session_id, run).unwrap();
        let mut records = sequence
            .start(&start.turn_id, "recover", Vec::new(), 1)
            .unwrap();
        records.push(
            sequence
                .start_execution(&start.turn_id, old_id, &digest, None, 2)
                .unwrap(),
        );
        records.push(sequence.checkpoint_ref(&checkpoint_record).unwrap());
        records.push(
            sequence
                .end_execution(
                    &start.turn_id,
                    old_id,
                    "lost",
                    "execution_environment_lost",
                    true,
                    Some(checkpoint_id),
                    Vec::new(),
                    4,
                )
                .unwrap(),
        );
        log.append_session_records_with_runtime_job_lease_blocking(run, &records, &fence)
            .unwrap();
        sequence = crate::load_existing_session_sequence(store.as_ref(), &start).unwrap();
        assert_eq!(
            crate::next_recovery_execution_id(&start, &sequence, checkpoint_id),
            replacement_id
        );
        let hash = snapshot.snapshot_sha256.clone();
        let server = thread::spawn(move || {
            for _ in 0..2 {
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
                assert!(request.starts_with(
                    b"POST /internal/agent-runs/execution-workspace/download HTTP/1.1\r\n"
                ));
                let length = String::from_utf8(request)
                    .unwrap()
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                let mut request_body = vec![0; length];
                socket.read_exact(&mut request_body).unwrap();
                write!(socket, "HTTP/1.1 200 fixture\r\nContent-Length: {}\r\nX-Content-Sha256: {hash}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.write_all(&body).unwrap();
            }
        });
        let lease = SessionWorkspaceLease {
            job_id: job.clone(),
            lease_owner: owner.into(),
        };
        let mut prepared = prepare_recovery_workspace_restore(
            RecoveryRestoreBinding {
                api_url: &api_url,
                api_token: "centaeris-recovery-test-token",
                agent_run_id: run,
                authorization_digest: &digest,
                lease: &lease,
                checkpoint_id: "centaeris-recovery-test-checkpoint",
                data_tmpfs_bytes: resources.data_tmpfs_bytes,
                input_upper_bound_bytes: 0,
            },
            &snapshot,
            None,
        )
        .unwrap();
        let before_retirement = inspect_container(&container_name(old_id)).unwrap().unwrap();
        let old_pid = before_retirement.raw["State"]["Pid"].as_i64().unwrap();
        assert!(old_pid > 0 && before_retirement.running);
        assert!(
            DockerExecutionHostRunner::retire_recovery_execution(
                run,
                old_id,
                store.as_ref(),
                &fence
            )
            .is_err(),
            "running old container must be retained"
        );
        assert_eq!(charged(), 1);
        let old_facts = inspect_container(&container_name(old_id)).unwrap().unwrap();
        assert!(old_facts.running && old_facts.has_identity(run, old_id));
        assert_eq!(old_facts.id, before_retirement.id);
        assert_eq!(old_facts.raw["State"]["Pid"].as_i64(), Some(old_pid));
        assert!(Command::new("docker")
            .args(["stop", "-t", "2", &old_facts.id])
            .status()
            .unwrap()
            .success());
        let host_id = resident_host_id().unwrap();
        let unknown_delete = store.remove_resident_execution(
            &host_id,
            run,
            old_id,
            &fence,
            || {
                crate::docker_engine::remove_stopped(&old_facts.id)?;
                Err("reply lost after physical removal".into())
            },
            || observed_resident_sandboxes_for_run(run),
        );
        assert_eq!(
            unknown_delete.unwrap_err(),
            "reply lost after physical removal"
        );
        assert!(inspect_container(&container_name(old_id))
            .unwrap()
            .is_none());
        assert_eq!(charged(), 1, "lost delete reply must retain the charge");
        let unknown_inventory = store.remove_resident_execution(
            &host_id,
            run,
            old_id,
            &fence,
            || Ok(()),
            || Err("inventory unavailable after physical removal".into()),
        );
        assert_eq!(
            unknown_inventory.unwrap_err(),
            "inventory unavailable after physical removal"
        );
        assert_eq!(charged(), 1, "unknown inventory must retain the charge");
        DockerExecutionHostRunner::retire_recovery_execution(run, old_id, store.as_ref(), &fence)
            .unwrap();
        assert!(inspect_container(&container_name(old_id))
            .unwrap()
            .is_none());
        assert_eq!(charged(), 0);
        let retirement_sequence = sequence.clone();
        let replacement = crate::prepare_execution_host_with_recovery(
            || {
                crate::retire_recovery_preparation(
                    &start,
                    &retirement_sequence,
                    checkpoint_id,
                    old_id,
                    |execution| {
                        DockerExecutionHostRunner::retire_recovery_execution(
                            run,
                            execution,
                            store.as_ref(),
                            &fence,
                        )
                    },
                    |_, _| Err("no pending preparation before the first attempt".into()),
                )
            },
            |mut client| {
                crate::commit_execution_recovery_attempt(
                    &mut sequence,
                    &start.turn_id,
                    checkpoint_id,
                    5,
                    now,
                    |_, record| {
                        log.append_session_records_with_runtime_job_lease_on_client(
                            client.as_deref_mut().unwrap(),
                            run,
                            std::slice::from_ref(record),
                            &fence,
                        )
                        .map(|_| ())
                    },
                )
            },
            |admitted| {
                create(replacement_id, owner, store.clone(), &mut |client| {
                    admitted(Some(client))
                })
            },
        )
        .unwrap();
        assert_eq!(charged(), 1);
        assert_eq!(
            replacement
                .restore_prepared_recovery_workspace(&mut prepared, 0)
                .unwrap(),
            SessionWorkspaceResolution::Download
        );
        let restored = exec_with_input(
            &replacement.exec_request(vec!["/bin/cat".into(), "/mnt/data/report.txt".into()]),
            &[],
            1024,
        )
        .unwrap();
        assert!(restored.status.success());
        assert_eq!(restored.stdout, b"checkpoint contents");
        // The worker ends after physical restore but before execution_started.
        drop(replacement);
        drop(prepared);
        drop(sequence);
        drop(log);
        let resumed_owner = "centaeris-recovery-test-resumed-owner";
        assert_eq!(
            db.execute(
                "UPDATE runtime.runtime_jobs SET lease_owner=$2 WHERE job_id=$1 AND lease_owner=$3",
                &[&job, &resumed_owner, &owner]
            )
            .unwrap(),
            1
        );
        let resumed_store = Arc::new(PostgresRuntimeStore::new(&url).unwrap());
        let resumed_log = resumed_store.session_log(
            start.authorization.workspace_id.clone(),
            start.authorization.session_id.clone(),
            "recover".into(),
        );
        let mut resumed_sequence =
            crate::load_existing_session_sequence(resumed_store.as_ref(), &start).unwrap();
        let (persisted_record, persisted_checkpoint) = crate::select_agent_run_recovery_checkpoint(
            resumed_store.as_ref(),
            &start,
            &resumed_sequence,
        )
        .unwrap()
        .unwrap();
        assert_eq!(persisted_record.checkpoint_id, checkpoint_id);
        assert_eq!(
            resumed_sequence
                .pending_recovery_attempt(checkpoint_id)
                .unwrap()
                .number,
            1
        );
        assert_eq!(resumed_sequence.active_execution_id(), None);
        let resumed_lease = SessionWorkspaceLease {
            job_id: job.clone(),
            lease_owner: resumed_owner.into(),
        };
        let mut resumed_prepared = prepare_recovery_workspace_restore(
            RecoveryRestoreBinding {
                api_url: &api_url,
                api_token: "centaeris-recovery-test-token",
                agent_run_id: run,
                authorization_digest: &digest,
                lease: &resumed_lease,
                checkpoint_id: &persisted_checkpoint.checkpoint_id,
                data_tmpfs_bytes: resources.data_tmpfs_bytes,
                input_upper_bound_bytes: 0,
            },
            &persisted_checkpoint.workspace_snapshot,
            None,
        )
        .unwrap();
        server.join().unwrap();
        let resumed_fence = centaeris_core::session::RuntimeJobLeaseFence {
            lease_owner: resumed_owner.into(),
            ..fence.clone()
        };
        let next_execution =
            crate::next_recovery_execution_id(&start, &resumed_sequence, checkpoint_id);
        assert_ne!(next_execution, replacement_id);
        let retirement_sequence = resumed_sequence.clone();
        let reentry = crate::prepare_execution_host_with_recovery(
            || {
                let retire = || {
                    crate::retire_recovery_preparation(
                        &start,
                        &retirement_sequence,
                        checkpoint_id,
                        &persisted_checkpoint.execution_id,
                        |execution| {
                            DockerExecutionHostRunner::retire_recovery_execution(
                                run,
                                execution,
                                resumed_store.as_ref(),
                                &resumed_fence,
                            )
                        },
                        |execution, pending| {
                            assert_eq!(pending.number, 1);
                            assert_eq!(execution, replacement_id);
                            crate::discard_unstarted_recovery_execution(
                                resumed_store.as_ref(),
                                &start,
                                &resumed_fence,
                                execution,
                                pending,
                            )
                        },
                    )
                };
                retire()?;
                assert_eq!(charged(), 0);
                // Re-enter after discard committed, before the next attempt exists.
                retire()
            },
            |mut client| {
                crate::commit_execution_recovery_attempt(
                    &mut resumed_sequence,
                    &start.turn_id,
                    checkpoint_id,
                    5,
                    now + 1,
                    |_, record| {
                        resumed_log
                            .append_session_records_with_runtime_job_lease_on_client(
                                client.as_deref_mut().unwrap(),
                                run,
                                std::slice::from_ref(record),
                                &resumed_fence,
                            )
                            .map(|_| ())
                    },
                )
            },
            |admitted| {
                create(
                    &next_execution,
                    resumed_owner,
                    resumed_store.clone(),
                    &mut |client| admitted(Some(client)),
                )
            },
        );
        let replacement = match reentry {
            Ok(replacement) => replacement,
            Err(error) => {
                if let Some(facts) = inspect_container(&container_name(replacement_id)).unwrap() {
                    assert!(facts.has_identity(run, replacement_id));
                    resumed_store
                        .remove_resident_execution(
                            &resident_host_id().unwrap(),
                            run,
                            replacement_id,
                            &resumed_fence,
                            || crate::docker_engine::remove(&facts.id, false),
                            || observed_resident_sandboxes_for_run(run),
                        )
                        .unwrap();
                }
                assert_eq!(charged(), 0);
                panic!("replayed pending attempt blocked reentry: {error}");
            }
        };
        assert_eq!(
            replacement
                .restore_prepared_recovery_workspace(&mut resumed_prepared, 0)
                .unwrap(),
            SessionWorkspaceResolution::Download
        );
        let started = resumed_sequence
            .start_execution(
                &start.turn_id,
                &next_execution,
                &digest,
                Some(checkpoint_id),
                now + 2,
            )
            .unwrap();
        resumed_log
            .append_session_records_with_runtime_job_lease_blocking(run, &[started], &resumed_fence)
            .unwrap();
        let replayed =
            crate::load_existing_session_sequence(resumed_store.as_ref(), &start).unwrap();
        assert_eq!(
            replayed.active_execution_id(),
            Some(next_execution.as_str())
        );
        assert!(replayed.has_used_recovery_checkpoint(checkpoint_id));
        assert!(replayed.pending_recovery_attempt(checkpoint_id).is_none());
        let restored = exec_with_input(
            &replacement.exec_request(vec!["/bin/cat".into(), "/mnt/data/report.txt".into()]),
            &[],
            1024,
        )
        .unwrap();
        assert!(restored.status.success());
        assert_eq!(restored.stdout, b"checkpoint contents");
        let ended = resumed_sequence
            .end_execution(
                &start.turn_id,
                &next_execution,
                "completed",
                "completed",
                false,
                None,
                Vec::new(),
                now + 3,
            )
            .unwrap();
        resumed_log
            .append_session_records_with_runtime_job_lease_blocking(run, &[ended], &resumed_fence)
            .unwrap();
        let replacement_facts = inspect_container(&container_name(&next_execution))
            .unwrap()
            .unwrap();
        assert!(replacement_facts.has_identity(run, &next_execution));
        assert!(Command::new("docker")
            .args(["stop", "-t", "2", &replacement_facts.id])
            .status()
            .unwrap()
            .success());
        DockerExecutionHostRunner::retire_recovery_execution(
            run,
            &next_execution,
            resumed_store.as_ref(),
            &resumed_fence,
        )
        .unwrap();
        assert_eq!(charged(), 0);
        let late = create(late_id, resumed_owner, resumed_store.clone(), &mut |_| {
            Ok(())
        })
        .unwrap();
        assert_eq!(charged(), 1);
        let absence_request = late.exec_request(vec![
            "/bin/sh".into(),
            "-c".into(),
            "test ! -e /mnt/data/report.txt".into(),
        ]);
        assert!(
            exec_with_input(&absence_request, &[], 1024)
                .unwrap()
                .status
                .success(),
            "late execution must start empty"
        );
        let new_owner = "centaeris-recovery-test-new-owner";
        assert_eq!(
            db.execute(
                "UPDATE runtime.runtime_jobs SET lease_owner=$2 WHERE job_id=$1 AND lease_owner=$3",
                &[&resumed_fence.job_id, &new_owner, &resumed_owner]
            )
            .unwrap(),
            1
        );
        let late_restore = late.restore_prepared_recovery_workspace(&mut resumed_prepared, 0);
        let remained_empty = exec_with_input(&absence_request, &[], 1024).unwrap();
        let current_fence = centaeris_core::session::RuntimeJobLeaseFence {
            lease_owner: new_owner.into(),
            ..resumed_fence
        };
        let facts = inspect_container(&container_name(late_id))
            .unwrap()
            .unwrap();
        assert!(facts.has_identity(run, late_id));
        assert!(Command::new("docker")
            .args(["stop", "-t", "2", &facts.id])
            .status()
            .unwrap()
            .success());
        DockerExecutionHostRunner::retire_recovery_execution(
            run,
            late_id,
            resumed_store.as_ref(),
            &current_fence,
        )
        .unwrap();
        assert_eq!(charged(), 0);
        assert!(
            late_restore.is_err(),
            "stale first restore outcome={late_restore:?}; report absent={}",
            remained_empty.status.success()
        );
        assert!(
            remained_empty.status.success(),
            "stale worker must leave the empty workspace untouched"
        );
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

    #[test]
    fn workspace_generation_response_preserves_known_unknown_and_invalid_states() {
        let known = serde_json::json!({
            "schema": "workspace.generation.v1",
            "generation": {"instanceEpoch": "watcher-current", "generation": 7},
            "diagnostic": null,
        });
        assert_eq!(
            decode_workspace_generation(&serde_json::to_vec(&known).unwrap())
                .unwrap()
                .generation
                .unwrap()
                .generation,
            7
        );
        let unknown = serde_json::json!({
            "schema": "workspace.generation.v1",
            "generation": null,
            "diagnostic": "workspace watcher unavailable",
        });
        let decoded = decode_workspace_generation(&serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert!(decoded.generation.is_none());
        assert_eq!(
            decoded.diagnostic.as_deref(),
            Some("workspace watcher unavailable")
        );
        let mut invalid = known.clone();
        invalid["generation"]["instanceEpoch"] = "".into();
        assert!(decode_workspace_generation(&serde_json::to_vec(&invalid).unwrap()).is_err());
        assert!(decode_workspace_generation(b"{\"schema\":").is_err());
        let mut extra = known;
        extra["unexpected"] = true.into();
        assert!(decode_workspace_generation(&serde_json::to_vec(&extra).unwrap()).is_err());
    }

    #[test]
    fn oci_runtime_runc_maps_to_oci_container_and_docker_runc() {
        let runtime = OciRuntime::parse("runc").expect("runc");
        assert_eq!(runtime.docker_runtime_name(), "runc");
        assert_eq!(runtime.transition_reason(), "docker_runc");
    }

    #[test]
    fn environment_container_keeps_host_control_outside_workload_mounts() {
        let runner = test_docker_execution_host_runner();
        let mut expected = runner.container_expectation();
        let body = container_create_body(&expected).unwrap();
        assert_eq!(
            body["Cmd"],
            serde_json::json!([AGENT_BINARY, "environment-keeper"])
        );
        assert_eq!(body["User"], "10001:10001");
        let host = &body["HostConfig"];
        assert_eq!(host["CapDrop"], serde_json::json!(["ALL"]));
        assert_eq!(host["CapAdd"], serde_json::json!([]));
        assert!(host["CgroupParent"].is_null());
        assert!(host["Binds"].is_null());
        assert!(host["Tmpfs"].is_null());
        let mounts = host["Mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0]["Target"], "/opt/centaeris/plugins/banana");
        assert_eq!(mounts[1]["Target"], AGENT_BINARY);
        assert!(mounts.iter().all(|mount| mount["ReadOnly"] == true));
        assert_eq!(host["Memory"], expected.resources.memory_bytes);
        assert_eq!(host["PidsLimit"], expected.resources.pids_limit);
        assert_eq!(host["NetworkMode"], "none");
        expected.oci_runtime = OciRuntime::Runsc;
        assert!(
            container_create_body(&expected).is_err(),
            "backend requires its own verified supervisor adapter"
        );
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
        assert_eq!(network::mode(ExecutionNetwork::Disabled), "none");
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
        let bytes = vec![255; 400_000];
        let expected = ExecutionFileSystemOutput::ReadFile(ExecutionFileReadOutput {
            identity: ExecutionFileIdentity {
                key: "rendered-page".into(),
                display_path: "/mnt/data/page.png".into(),
            },
            file_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
            bytes,
        });
        let mut encoded = Vec::new();
        hosted_execution::protocol::write_filesystem_result(
            &mut encoded,
            SandboxFileSystemResult::from(Ok(expected.clone())),
        )
        .unwrap();
        let header = u32::from_be_bytes(encoded[..4].try_into().unwrap()) as usize;
        assert!(header < 4096);
        assert_eq!(&encoded[header + 4..], vec![255; 400_000]);
        let decoded = hosted_execution::protocol::read_filesystem_result(&mut Cursor::new(encoded))
            .expect("complete binary frame")
            .into_result()
            .expect("file response");
        assert_eq!(decoded, expected);
    }

    #[test]
    fn filesystem_response_budget_rejects_overflow_and_keeps_metadata_bounded() {
        let result = SandboxFileSystemResult::from(Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::NotFound,
            "absent",
        )));
        let (mut metadata, raw) =
            hosted_execution::protocol::FileSystemResponseFrameMetadata::from_result(result)
                .unwrap();
        assert!(raw.is_empty());
        metadata.validate().unwrap();
        assert!(serde_json::to_vec(&metadata).unwrap().len() < 4096);
        metadata.content_length = u64::MAX;
        assert!(
            metadata.validate().is_err(),
            "oversize result cannot enter framed transport"
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
    fn snapshot_protection_observations_count_actual_spool_bytes_on_success_and_failure() {
        let frame = snapshot_test_frame(b"protected payload");
        let (result, events) = crate::observations::capture_protection(|| {
            capture_snapshot_artifact(&mut Cursor::new(frame.clone()), frame.len() as u64)
        });
        assert!(result.is_ok());
        let bytes = events
            .iter()
            .find(|event| event["metric"] == "runtimeSpoolWrittenBytes")
            .expect("actual spool write observation");
        assert_eq!(bytes["value"], frame.len() as u64);
        assert_eq!(bytes["outcome"], "ok");

        let mut corrupted = frame.clone();
        *corrupted.last_mut().unwrap() = b'!';
        let (result, events) = crate::observations::capture_protection(|| {
            capture_snapshot_artifact(&mut Cursor::new(corrupted), frame.len() as u64)
        });
        assert!(result.is_err());
        let bytes = events
            .iter()
            .find(|event| event["metric"] == "runtimeSpoolWrittenBytes")
            .expect("failed captures still account for their writes");
        assert_eq!(bytes["value"], frame.len() as u64);
        assert_eq!(bytes["outcome"], "error");
        assert!(!serde_json::to_string(&events)
            .unwrap()
            .contains("protected payload"));
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
        let (expected_bytes, events) = crate::observations::capture_protection(|| {
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
                started.send(length).unwrap();
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
            let expected_bytes = observed.recv_timeout(Duration::from_secs(5)).unwrap();
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
            expected_bytes
        });
        let attempt = events
            .iter()
            .find(|event| event["metric"] == "uploadAttempts")
            .expect("actual HTTP upload attempt");
        let bytes = events
            .iter()
            .find(|event| event["metric"] == "uploadedBytes")
            .expect("consumed request body bytes");
        assert_eq!(attempt["value"], 1);
        assert_eq!(bytes["value"], expected_bytes);
        assert_eq!(bytes["outcome"], "ok");
        assert_eq!(attempt["operationId"], bytes["operationId"]);
        for event in [attempt, bytes] {
            assert_eq!(event["runId"], "agent_run_test");
            assert_eq!(event["jobId"], "job");
            assert_eq!(event["trigger"], "recoveryCheckpoint");
        }
        assert!(
            !events
                .iter()
                .any(|event| event["metric"] == "scannedEntries"),
            "no sandbox scan was observed by this HTTP-only fixture"
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
    fn prepared_recovery_restore_holds_the_verified_immutable_source() {
        let bytes = b"checkpoint contents";
        let manifest = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "restored.txt".into(),
                size_bytes: bytes.len() as u64,
                sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
                executable: false,
            }],
        };
        let frame = encode_frame(&manifest, bytes).unwrap();
        let snapshot = RecoveryWorkspaceSnapshotV1 {
            object_ref: Some("immutable-checkpoint".into()),
            snapshot_sha256: format!("sha256:{:x}", Sha256::digest(&frame)),
            snapshot_size_bytes: frame.len() as u64,
            expanded_size_bytes: bytes.len() as u64,
            file_count: 1,
        };
        let mut source = Cursor::new(frame.clone());
        let (result, events) = crate::observations::capture_protection(|| {
            prepare_recovery_restore_artifact(
                &mut source,
                &snapshot,
                WorkspaceProtection::new().with_context(
                    ProtectionTrigger::RecoveryRestore,
                    "run-restored",
                    Some("job-restored"),
                ),
            )
        });
        let mut artifact = result.unwrap();
        assert_eq!(events.len(), 2);
        for event in &events {
            assert_eq!(event["runId"], "run-restored");
            assert_eq!(event["jobId"], "job-restored");
            assert_eq!(event["trigger"], "recoveryRestore");
            assert_eq!(event["operationId"], events[0]["operationId"]);
        }
        source.get_mut().fill(0);
        let mut held = Vec::new();
        artifact.file.read_to_end(&mut held).unwrap();
        assert_eq!(held, frame);
        assert!(prepare_recovery_restore_artifact(
            &mut source,
            &snapshot,
            WorkspaceProtection::new()
        )
        .is_err());
        let mut wrong_binding = snapshot.clone();
        wrong_binding.file_count = 2;
        assert!(prepare_recovery_restore_artifact(
            &mut Cursor::new(frame),
            &wrong_binding,
            WorkspaceProtection::new()
        )
        .is_err());
    }

    #[test]
    fn recovery_session_resolution_preserves_advanced_completion_without_download() {
        let frozen = SessionWorkspace {
            generation: 0,
            snapshot_sha256: String::new(),
            snapshot_size_bytes: 0,
            expanded_size_bytes: 0,
            file_count: 0,
        };
        let mut resolved = SessionWorkspaceResolveResponse {
            schema: SESSION_WORKSPACE_RESOLVED_SCHEMA.into(),
            disposition: "empty".into(),
            session_workspace: frozen.clone(),
        };
        assert_eq!(
            resolved_resolution(&resolved, &frozen).unwrap(),
            SessionWorkspaceResolution::Empty
        );
        resolved.disposition = "advanced".into();
        resolved.session_workspace.generation = 1;
        assert_eq!(
            resolved_resolution(&resolved, &frozen).unwrap(),
            SessionWorkspaceResolution::Advanced
        );
        resolved.session_workspace.generation = 2;
        assert!(resolved_resolution(&resolved, &frozen).is_err());
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
                volume_options: Some(ContainerVolumeOptions {
                    subpath: "demo".to_string(),
                }),
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
                volume_options: Some(ContainerVolumeOptions {
                    subpath: mount.scope_key.clone(),
                }),
            }],
            &mount,
        ));
    }

    #[test]
    fn adoption_rejects_missing_or_foreign_environment_instance_and_grant_labels() {
        let runner = test_docker_execution_host_runner();
        let request = container_create_body(&runner.container_expectation()).unwrap();
        let mut config = request.clone();
        config.as_object_mut().unwrap().remove("Image");
        config.as_object_mut().unwrap().remove("HostConfig");
        let facts = serde_json::json!({"Id":"test", "Image": request["Image"], "Config": config,
            "HostConfig":request["HostConfig"]});
        assert!(crate::docker_engine::verify_created(&facts, &request).is_ok());
        for label in [
            "centaeris.environment_id",
            "centaeris.instance_id",
            "centaeris.environment_authorization_digest",
        ] {
            let mut missing = facts.clone();
            missing["Config"]["Labels"]
                .as_object_mut()
                .unwrap()
                .remove(label);
            assert!(
                crate::docker_engine::verify_created(&missing, &request).is_err(),
                "{label}"
            );
            let mut foreign = facts.clone();
            foreign["Config"]["Labels"][label] = "foreign".into();
            assert!(
                crate::docker_engine::verify_created(&foreign, &request).is_err(),
                "{label}"
            );
        }
    }

    #[test]
    fn engine_creation_preserves_sandbox_limits_and_mount_isolation() {
        let mut runner = test_docker_execution_host_runner();
        runner.resources.cpu_milli = 4000;
        runner.resources.memory_bytes = 8 * 1024 * 1024 * 1024;
        let profile = runner.environment_profile.as_mut().unwrap();
        profile.resources.cpu_milli = runner.resources.cpu_milli;
        profile.resources.memory_bytes = runner.resources.memory_bytes;
        let body = container_create_body(&runner.container_expectation()).unwrap();
        assert_eq!(body["Image"], runner.image_digest);
        assert_eq!(
            body["Labels"]["centaeris.execution_id"],
            runner.execution_id
        );
        assert_eq!(
            body["Labels"]["centaeris.environment_id"],
            "environment_test"
        );
        let instance = body["Labels"]["centaeris.instance_id"]
            .as_str()
            .expect("physical instance identity");
        assert!(instance.starts_with("instance_"));
        assert_ne!(instance, runner.execution_id);
        let host = &body["HostConfig"];
        assert_eq!(host["CpuPeriod"], 100000);
        assert_eq!(host["CpuQuota"], 400000);
        assert_eq!(host["Memory"], 8u64 * 1024 * 1024 * 1024);
        assert_eq!(host["PidsLimit"], runner.resources.pids_limit);
        assert_eq!(host["NetworkMode"], "none");
        assert_eq!(host["ReadonlyRootfs"], false);
        assert_eq!(host["CapDrop"], serde_json::json!(["ALL"]));
        assert_eq!(host["CapAdd"], serde_json::json!([]));
        assert_eq!(
            host["SecurityOpt"],
            serde_json::json!(["no-new-privileges"])
        );
        assert_eq!(host["Mounts"][0]["ReadOnly"], true);
        assert_eq!(host["Mounts"][0]["VolumeOptions"]["Subpath"], "banana");
        assert_eq!(host["Mounts"][1]["Type"], "bind");
        assert_eq!(host["Mounts"][1]["ReadOnly"], true);
        assert_eq!(host["Mounts"][1]["Source"], "/tmp/execution_agent");
        assert_eq!(host["Mounts"][1]["Target"], AGENT_BINARY);
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

    #[test]
    fn ordinary_network_capability_controls_engine_and_core_policy() {
        let mut runner = test_docker_execution_host_runner();
        for (network, mode, policy) in [
            (ExecutionNetwork::Disabled, "none", NetworkPolicy::Disabled),
            (
                ExecutionNetwork::PublicInternet,
                "bridge",
                NetworkPolicy::PublicInternet,
            ),
        ] {
            runner.network = network;
            runner.environment_profile.as_mut().unwrap().network = network;
            let body = container_create_body(&runner.container_expectation()).unwrap();
            assert_eq!(body["HostConfig"]["NetworkMode"], mode);
            assert_eq!(body["HostConfig"]["ReadonlyRootfs"], false);
            let execution = runner.execution_policy();
            assert_eq!(execution.network, policy);
            assert_eq!(
                execution.filesystem.workspace_root,
                Path::new(WORKSPACE_DATA_ROOT)
            );
            assert_eq!(
                execution.filesystem,
                environment_policy(runner.environment_profile.as_ref().unwrap()).filesystem
            );
            runner.validate_policy(&execution).unwrap();
            let mut unauthorized = execution.clone();
            unauthorized.network = if policy == NetworkPolicy::Disabled {
                NetworkPolicy::PublicInternet
            } else {
                NetworkPolicy::Disabled
            };
            assert!(matches!(
                runner.validate_policy(&unauthorized),
                Err(ExecutionError::Denied { .. })
            ));
        }
    }

    #[test]
    fn retained_network_change_rejects_container_reuse_and_changes_profile() {
        let mut runner = retained_test_runner();
        let original: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
            serde_json::from_str(include_str!(
                "../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
            ))
            .unwrap();
        runner.resources = original.environment_profile.resources.sandbox_resources();
        runner.image_digest = original.environment_profile.image_digest.clone();
        runner.retained_allocation.as_mut().unwrap().image_digest = runner.image_digest.clone();
        runner.environment_profile = Some(original.environment_profile.clone());
        runner.benchmark_helper = Some("/tmp/execution_agent".into());
        let expected = runner.container_expectation();
        let body = container_create_body(&expected).unwrap();
        let host = &body["HostConfig"];
        let mut config = body.clone();
        config.as_object_mut().unwrap().remove("HostConfig");
        config.as_object_mut().unwrap().remove("Image");
        let facts = ContainerFacts {
            id: "a".repeat(64),
            raw: serde_json::json!({"Id":"a".repeat(64), "Config":config,
                "Image":body["Image"], "HostConfig":host, "Mounts":host["Mounts"]}),
            running: true,
            image: runner.image_digest.clone(),
            runtime: "runc".into(),
            labels: serde_json::from_value(body["Labels"].clone()).unwrap(),
            memory: host["Memory"].as_u64().unwrap(),
            cpu_period: host["CpuPeriod"].as_i64().unwrap(),
            cpu_quota: host["CpuQuota"].as_i64().unwrap(),
            pids_limit: host["PidsLimit"].as_i64(),
            readonly_rootfs: false,
            network_mode: "none".into(),
            mounts: serde_json::from_value(host["Mounts"].clone()).unwrap(),
        };
        assert!(facts.matches(&expected), "original retained configuration");
        let changed = ContainerExpectation {
            network: ExecutionNetwork::PublicInternet,
            ..expected
        };
        assert!(
            !facts.matches(&changed),
            "a new network grant cannot reuse the old physical mode"
        );
        let mut changed_profile = original.environment_profile.clone();
        changed_profile.network = ExecutionNetwork::PublicInternet;
        let changed = ContainerExpectation {
            environment_profile: Some(&changed_profile),
            ..changed
        };
        assert_ne!(
            retained::profile_digest(&expected, &original.plugin_activation).unwrap(),
            retained::profile_digest(&changed, &original.plugin_activation).unwrap()
        );
    }

    #[test]
    fn resident_inventory_counts_declared_limits_and_rejects_unknown_managed_resources() {
        let runner = test_docker_execution_host_runner();
        let body = container_create_body(&runner.container_expectation()).unwrap();
        let host = &body["HostConfig"];
        let mut facts = ContainerFacts {
            id: "a".repeat(64),
            raw: serde_json::json!({"HostConfig": host}),
            running: false,
            image: runner.image_digest.clone(),
            runtime: "runc".into(),
            labels: serde_json::from_value(body["Labels"].clone()).unwrap(),
            memory: host["Memory"].as_u64().unwrap(),
            cpu_period: host["CpuPeriod"].as_i64().unwrap(),
            cpu_quota: host["CpuQuota"].as_i64().unwrap(),
            pids_limit: host["PidsLimit"].as_i64(),
            readonly_rootfs: true,
            network_mode: "none".into(),
            mounts: Vec::new(),
        };
        facts.labels.insert(
            "centaeris.allocation_owner".into(),
            serde_json::to_string(&centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                run_id: runner.agent_run_id.clone(),
            })
            .unwrap(),
        );
        let observed = observed_resident_sandbox(&facts).unwrap();
        assert_eq!(observed.resources, declared_usage(runner.resources));
        assert_eq!(
            observed.allocation_owner,
            centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
                run_id: runner.agent_run_id.clone()
            }
        );
        assert_eq!(observed.execution_id, runner.execution_id);
        facts.labels.remove("centaeris.execution_id");
        assert!(observed_resident_sandbox(&facts).is_err());
        facts
            .labels
            .insert("centaeris.execution_id".into(), runner.execution_id.clone());
        facts.memory = 0;
        assert!(observed_resident_sandbox(&facts).is_err());
        facts.memory = runner.resources.memory_bytes;
        facts.labels.remove("centaeris.data_budget");
        assert!(observed_resident_sandbox(&facts).is_err());
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
            network: ExecutionNetwork::Disabled,
            transcript_capture: Mutex::new(Default::default()),
            transcript_publisher: Arc::new(Default::default()),
            recovery_activity: ExecutionActivityWitness::new(
                NEXT_EXECUTION_HOST_INSTANCE.fetch_add(1, Ordering::Relaxed),
            ),
            recovery_snapshot: Mutex::new(None),
            snapshot_capture_cache: Mutex::new(None),
            pre_dispatch_loss: AtomicBool::new(false),
            deferred_extension_dispatch: AtomicBool::new(false),
            container_name: "centaeris-agent-run-test".to_string(),
            agent_run_id: "agent_run_test".to_string(),
            execution_id: "execution_test".to_string(),
            environment_id: "environment_test".into(),
            instance_id: crate::postgres_store::execution_instances::instance_id("execution_test"),
            binding_digest: format!("sha256:{}", "c".repeat(64)),
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
            resident_store: None,
            resident_lease: None,
            retained_allocation: None,
            environment_authorization: None,
            retained_workspace_id: None,
            environment_profile: Some(EnvironmentProfile {
                schema: "runtime.environment_profile.v1".into(),
                image_digest: format!("sha256:{}", "b".repeat(64)),
                workspace_root: WORKSPACE_DATA_ROOT.into(),
                user: AGENT_USER.into(),
                network: ExecutionNetwork::Disabled,
                resources: crate::agent_run_authorization::EnvironmentResources {
                    cpu_milli: 1,
                    memory_bytes: 1,
                    pids_limit: 1,
                    data_tmpfs_bytes: 1,
                    storage_bytes: 1,
                },
            }),
            benchmark_helper: Some("/tmp/execution_agent".into()),
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
        let mcp_runner = Arc::new(retained_test_runner());
        let request = mcp_runner
            .mcp_exec_request(
                "banana",
                "bin/banana-mcp",
                &["--mode".into(), "stdio".into()],
            )
            .unwrap();
        assert!(
            mcp_runner
                .recovery_activity
                .try_snapshot_activity()
                .unwrap()
                .is_none(),
            "live persistent MCP defers recovery capture"
        );
        assert_eq!(
            request.program,
            "/opt/centaeris/plugins/banana/bin/banana-mcp"
        );
        assert_eq!(request.cwd, "/mnt/data");
        assert_eq!(request.args, ["--mode", "stdio"]);
        assert_eq!(
            request.env["PATH"],
            "/opt/centaeris/plugins/banana/bin:/usr/bin"
        );

        let hook_args = runner
            .lifecycle_hook_request(&LifecycleHookHandlerV1 {
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
        assert_eq!(hook_args.cwd, Path::new("/opt/centaeris/plugins/banana"));
        assert_eq!(hook_args.program, "node");
        assert_eq!(hook_args.args, ["hooks/guard-write.mjs"]);
        assert_eq!(hook_args.timeout_ms, 5000);
        assert!(hook_args.env.is_empty());
        assert_eq!(hook_args.policy, runner.execution_policy());
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
