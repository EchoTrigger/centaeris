//! Owned per-Run E2B SDK contexts. Registry and lifecycle routing live above this factory.
use crate::agent_run_authorization::{EnvironmentProfile, WorkspaceAgentRunAuthorization};
use crate::e2b_adapter::bulk::{SpoolingAdapterProcess, MAX_TRANSFER_BYTES, TRANSFER_CHUNK_BYTES};
use crate::e2b_adapter::{AdapterFailure, AdapterOutcome, AdapterResponse};
use crate::e2b_creation::{E2bCreationAdapter, E2bCreationDriver, E2bCreationRequest};
use crate::e2b_execution_host::authority::PostgresE2bDispatchAuthority;
use crate::e2b_execution_host::sdk_transport::E2bSdkTransport;
use crate::e2b_execution_host::{
    E2bDispatchAuthority, E2bDispatchResult, E2bExecutionHostRunner, E2bHostError,
    E2bSandboxBinding,
};
use crate::postgres_store::controller_authority::{ControllerAuthorityRequest, ControllerPurpose};
use crate::postgres_store::provider_operations::cleanup::{
    expected_metadata, ProviderExecutionDispatchError, ProviderVerifiedTarget,
};
use crate::postgres_store::provider_operations::{
    ProviderCreationDispatchState, ProviderCreationIdentity, ProviderCreationOperation,
};
use crate::postgres_store::PostgresRuntimeStore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

mod cleanup;
pub(crate) mod maintenance;
pub(crate) mod observation;
mod power;
mod power_adapter;
pub(crate) use power::with_power_environment;
mod provision;
mod retained;
mod retirement;
pub(crate) mod rotation;
#[cfg(test)]
mod tests;
pub(crate) use cleanup::E2bCleanupOutcome;
#[cfg(test)]
pub(crate) use cleanup::E2bCleanupPendingStage;
pub(crate) use provision::E2bProvisionOutcome;

/// Host-selected paths and public provider identity. Credentials stay inherited
/// by the SDK child and this type deliberately implements no Debug.
pub(crate) struct E2bRuntimeConfig {
    profile: EnvironmentProfile,
    provider_config_digest: String,
    python: PathBuf,
    adapter_package_root: PathBuf,
    request_timeout: Duration,
    running_limit: Option<u32>,
    idle_pause_timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum E2bRuntimeError {
    Rejected(&'static str),
    Unavailable(&'static str),
    Unknown(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct E2bAttemptKey {
    pub attempt_id: String,
    pub idempotency_key: String,
}

fn make_attempt_key(
    environment_id: &str,
    authorization_digest: &str,
    profile_digest: &str,
    provider_config_digest: &str,
    execution_id: &str,
) -> E2bAttemptKey {
    let mut digest = Sha256::new();
    digest.update(b"runtime:e2b-business-create:v1\0");
    for value in [
        environment_id,
        authorization_digest,
        profile_digest,
        provider_config_digest,
        execution_id,
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    let hash = format!("{:x}", digest.finalize());
    E2bAttemptKey {
        attempt_id: format!("e2b-create-{hash}"),
        idempotency_key: format!("centaeris-create-{hash}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum E2bAdapterLane {
    Ordinary,
    Control,
    Cleanup,
}

pub(crate) trait E2bAdapterLauncher: Send + Sync {
    fn launch(
        &self,
        config: &E2bRuntimeConfig,
        lane: E2bAdapterLane,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError>;

    fn launch_observed(
        &self,
        config: &E2bRuntimeConfig,
        lane: E2bAdapterLane,
        _observation: crate::observations::power::PowerObservation,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError> {
        self.launch(config, lane)
    }
}

struct InstalledAdapterLauncher;

impl InstalledAdapterLauncher {
    fn launch_with_observation(
        &self,
        config: &E2bRuntimeConfig,
        lane: E2bAdapterLane,
        observation: Option<crate::observations::power::PowerObservation>,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError> {
        // Launch readiness is measured separately by require_capabilities.
        // This span includes private spool and host IO-thread setup.
        let mut span = crate::observations::enabled().then(|| {
            crate::observations::Span::new(match lane {
                E2bAdapterLane::Ordinary => "e2bAdapterSpawnOrdinary",
                E2bAdapterLane::Control => "e2bAdapterSpawnControl",
                E2bAdapterLane::Cleanup => "e2bAdapterSpawnCleanup",
            })
        });
        // The program is fixed; the trusted package root is a separate argv
        // value, never interpolated into Python or a shell command. -I removes
        // ambient Python search paths and the process working directory.
        const BOOTSTRAP:&str="import runpy,sys;sys.path.insert(0,sys.argv.pop(1));runpy.run_module('centaeris_e2b_adapter',run_name='__main__')";
        let mut command = Command::new(&config.python);
        command
            .args(["-I", "-u", "-c", BOOTSTRAP])
            .arg(&config.adapter_package_root)
            .args(["--lane", lane.as_str()])
            .current_dir(&config.adapter_package_root);
        let process = match observation {
            Some(observation) => SpoolingAdapterProcess::spawn_observed(command, observation),
            None => SpoolingAdapterProcess::spawn(command),
        };
        let result = process
            .map(Arc::new)
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_adapter_unavailable"));
        if let Some(span) = &mut span {
            span.finished(result.is_ok());
        }
        result
    }
}

impl E2bAdapterLauncher for InstalledAdapterLauncher {
    fn launch(
        &self,
        config: &E2bRuntimeConfig,
        lane: E2bAdapterLane,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError> {
        self.launch_with_observation(config, lane, None)
    }
    fn launch_observed(
        &self,
        config: &E2bRuntimeConfig,
        lane: E2bAdapterLane,
        observation: crate::observations::power::PowerObservation,
    ) -> Result<Arc<SpoolingAdapterProcess>, E2bRuntimeError> {
        self.launch_with_observation(config, lane, Some(observation))
    }
}

impl E2bAdapterLane {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Control => "control",
            Self::Cleanup => "cleanup",
        }
    }
}

impl E2bRuntimeConfig {
    pub(crate) fn new(
        profile: EnvironmentProfile,
        provider_config_digest: String,
        python: PathBuf,
        adapter_package_root: PathBuf,
        request_timeout: Duration,
    ) -> Result<Self, E2bRuntimeError> {
        profile
            .validate()
            .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_profile_invalid"))?;
        let EnvironmentProfile::E2b(selected) = &profile else {
            return Err(E2bRuntimeError::Rejected("e2b_runtime_profile_unsupported"));
        };
        if !matches!(selected.lifetime.as_str(), "perRun" | "retained")
            || (!profile.task_image()
                && (selected.workspace_root != "/mnt/data" || selected.user != "agent"))
            || selected.provider_config_digest != provider_config_digest
        {
            return Err(E2bRuntimeError::Rejected("e2b_runtime_profile_unsupported"));
        }
        if request_timeout.is_zero() || request_timeout > Duration::from_secs(60) {
            return Err(E2bRuntimeError::Rejected("e2b_runtime_timeout_invalid"));
        }
        if !python.is_absolute() || !adapter_package_root.is_absolute() {
            return Err(E2bRuntimeError::Rejected("e2b_runtime_paths_invalid"));
        }
        // Keep the selected launch alias: resolving a venv's Python symlink
        // changes its prefix and drops the SDK installed in that environment.
        let canonical_python = python
            .canonicalize()
            .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_paths_invalid"))?;
        let adapter_package_root = adapter_package_root
            .canonicalize()
            .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_paths_invalid"))?;
        if !canonical_python.is_file() || !adapter_package_root.is_dir() {
            return Err(E2bRuntimeError::Rejected("e2b_runtime_paths_invalid"));
        }
        for module in [
            "centaeris_e2b_adapter/__init__.py",
            "centaeris_e2b_adapter/__main__.py",
        ] {
            let path = adapter_package_root
                .join(module)
                .canonicalize()
                .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_package_invalid"))?;
            if !path.is_file() || !path.starts_with(&adapter_package_root) {
                return Err(E2bRuntimeError::Rejected("e2b_runtime_package_invalid"));
            }
        }
        Ok(Self {
            profile,
            provider_config_digest,
            python,
            adapter_package_root,
            request_timeout,
            running_limit: None,
            idle_pause_timeout: None,
        })
    }

    pub(crate) fn with_running_limit(
        mut self,
        limit: Option<u32>,
    ) -> Result<Self, E2bRuntimeError> {
        crate::postgres_store::provider_operations::capacity::validate_limit(limit)
            .map_err(|_| E2bRuntimeError::Rejected("e2b_running_limit_invalid"))?;
        if limit.is_some()
            && !matches!(&self.profile,EnvironmentProfile::E2b(value) if value.lifetime=="retained")
        {
            return Err(E2bRuntimeError::Rejected(
                "e2b_rotation_requires_retained_profile",
            ));
        }
        self.running_limit = limit;
        Ok(self)
    }
    pub(crate) fn with_idle_pause_timeout(
        mut self,
        timeout: Option<Duration>,
    ) -> Result<Self, E2bRuntimeError> {
        if let Some(timeout) = timeout {
            if self.running_limit.is_none() || crate::postgres_store::provider_operations::retained::idle::policy::IdlePausePolicy::new(timeout).is_err() {
                return Err(E2bRuntimeError::Rejected("e2b_idle_pause_configuration_invalid"));
            }
        }
        self.idle_pause_timeout = timeout;
        Ok(self)
    }
    pub(crate) fn profile(&self) -> &EnvironmentProfile {
        &self.profile
    }
}

pub(crate) enum E2bPrepareOutcome {
    Prepared(Arc<E2bPreparedHost>),
    Pending {
        operation: Box<ProviderCreationOperation>,
        reason: E2bPendingReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum E2bPendingReason {
    CreationUnconfirmed,
    OwnedContextUnavailable,
    CreationNotDispatched,
}

pub(crate) struct E2bRuntime {
    store: PostgresRuntimeStore,
    config: E2bRuntimeConfig,
    launcher: Arc<dyn E2bAdapterLauncher>,
    rotation_started: AtomicBool,
    power_adapter: power_adapter::PowerAdapterCache,
}

impl E2bRuntime {
    pub(crate) fn new(store: PostgresRuntimeStore, config: E2bRuntimeConfig) -> Self {
        Self {
            store,
            config,
            launcher: Arc::new(InstalledAdapterLauncher),
            rotation_started: AtomicBool::new(false),
            power_adapter: power_adapter::PowerAdapterCache::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_launcher(
        store: PostgresRuntimeStore,
        config: E2bRuntimeConfig,
        launcher: Arc<dyn E2bAdapterLauncher>,
    ) -> Self {
        Self {
            store,
            config,
            launcher,
            rotation_started: AtomicBool::new(false),
            power_adapter: power_adapter::PowerAdapterCache::default(),
        }
    }

    pub(crate) fn start_rotation(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Runtime,
        registry: Arc<crate::e2b_run_registry::E2bRunRegistry>,
    ) -> Option<rotation::RotationTask> {
        if self.config.idle_pause_timeout.is_none()
            || self.rotation_started.swap(true, Ordering::AcqRel)
        {
            return None;
        }
        rotation::start(self.clone(), registry, runtime)
    }

    /// Allow the in-flight capabilities handshake and provider call their existing
    /// request deadlines, plus bounded bookkeeping. Expiry never settles a duty.
    pub(crate) fn rotation_shutdown_timeout(&self) -> Duration {
        self.config.request_timeout.saturating_mul(2) + Duration::from_secs(10)
    }

    pub(crate) fn attempt_key(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        authorization: &WorkspaceAgentRunAuthorization,
        execution_id: &str,
    ) -> Result<E2bAttemptKey, E2bRuntimeError> {
        if authority.purpose != ControllerPurpose::Execute
            || authorization.environment_profile != self.config.profile
            || authorization.environment_profile_digest
                != self
                    .config
                    .profile
                    .digest()
                    .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_profile_invalid"))?
            || authorization.workspace_id != authority.workspace_id
        {
            return Err(E2bRuntimeError::Rejected(
                "e2b_runtime_authorization_rejected",
            ));
        }
        if execution_id.is_empty()
            || execution_id.len() > 256
            || !execution_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            return Err(E2bRuntimeError::Rejected("e2b_runtime_execution_invalid"));
        }
        let binding = &authority.environment_authorization.payload;
        authorization
            .checked_binding(&binding.agent_run_id)
            .and_then(|checked| {
                checked.verify_digest_binding(&binding.agent_run_authorization_digest)
            })
            .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_authorization_rejected"))?;
        authority
            .environment_authorization
            .verify(
                authorization,
                &binding.agent_run_authorization_digest,
                authority.signing_key,
            )
            .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_authorization_rejected"))?;
        Ok(make_attempt_key(
            &binding.environment_id,
            &binding.agent_run_authorization_digest,
            &authorization.environment_profile_digest,
            &self.config.provider_config_digest,
            execution_id,
        ))
    }

    #[cfg(test)]
    pub(crate) fn prepare(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        authorization: &WorkspaceAgentRunAuthorization,
        execution_id: &str,
    ) -> Result<E2bPrepareOutcome, E2bRuntimeError> {
        self.prepare_with_environment(authority, authorization, execution_id, None)
    }

    pub(crate) fn prepare_with_environment(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        authorization: &WorkspaceAgentRunAuthorization,
        execution_id: &str,
        previous: Option<&E2bPreparedHost>,
    ) -> Result<E2bPrepareOutcome, E2bRuntimeError> {
        if matches!(&self.config.profile, EnvironmentProfile::E2b(profile) if profile.lifetime == "retained")
        {
            self.prepare_retained(authority, authorization, execution_id, previous)
        } else {
            self.prepare_new(authority, authorization, execution_id)
        }
    }

    fn prepare_new(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        authorization: &WorkspaceAgentRunAuthorization,
        execution_id: &str,
    ) -> Result<E2bPrepareOutcome, E2bRuntimeError> {
        // Frozen provider/profile identity is checked before any process launch.
        let key = self.attempt_key(authority, authorization, execution_id)?;
        let request = E2bCreationRequest {
            authorization,
            execution_id,
            attempt_id: &key.attempt_id,
            idempotency_key: &key.idempotency_key,
            configured_provider_config_digest: &self.config.provider_config_digest,
            transport_timeout: self.config.request_timeout,
        };
        if self
            .store
            .provider_creation_operation(&key.attempt_id)
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_ledger_unavailable"))?
            .is_some_and(|operation| {
                operation.dispatch_state != ProviderCreationDispatchState::Prepared
            })
        {
            // Let the existing driver validate the entire immutable invocation.
            // This adapter has no SDK process and cannot send a loaded attempt.
            let operation = E2bCreationDriver::new(&self.store, &NeverCreateAdapter)
                .create_or_reopen(authority, &request)
                .map_err(|_| E2bRuntimeError::Unknown("e2b_runtime_creation_unconfirmed"))?;
            return Ok(pending_existing(operation));
        }
        let ordinary = self
            .launcher
            .launch(&self.config, E2bAdapterLane::Ordinary)?;
        let control = self
            .launcher
            .launch(&self.config, E2bAdapterLane::Control)?;
        require_capabilities(
            &ordinary,
            E2bAdapterLane::Ordinary,
            self.config.request_timeout,
        )?;
        require_capabilities_for_profile(
            &control,
            E2bAdapterLane::Control,
            self.config.request_timeout,
            &self.config.profile,
        )?;
        let adapter = RecordingCreationAdapter {
            ordinary: &ordinary,
            sent: AtomicBool::new(false),
            succeeded: AtomicBool::new(false),
        };
        let operation = E2bCreationDriver::new(&self.store, &adapter)
            .with_running_limit(self.config.running_limit)
            .create_or_reopen(authority, &request)
            .map_err(|error| match error {
                crate::e2b_creation::E2bCreationError::Store(reason)
                    if reason == crate::postgres_store::provider_operations::capacity::WAIT =>
                {
                    E2bRuntimeError::Unavailable(
                        crate::postgres_store::provider_operations::capacity::WAIT,
                    )
                }
                _ => E2bRuntimeError::Unknown("e2b_runtime_creation_unconfirmed"),
            })?;
        if !adapter.sent.load(Ordering::Acquire) {
            return Ok(pending_existing(operation));
        }
        if !adapter.succeeded.load(Ordering::Acquire)
            || operation.dispatch_state != ProviderCreationDispatchState::Dispatched
            || operation.reported_sandbox_id.is_none()
        {
            return Ok(E2bPrepareOutcome::Pending {
                operation: Box::new(operation),
                reason: E2bPendingReason::CreationUnconfirmed,
            });
        }
        if matches!(&self.config.profile, EnvironmentProfile::E2b(profile) if profile.lifetime == "retained")
        {
            return self.finish_retained_creation(
                authority,
                authorization,
                execution_id,
                &operation,
                ordinary,
                control,
            );
        }
        let target = self
            .store
            .with_provider_execution_dispatch(
                authority,
                &operation.identity,
                &self.config.provider_config_digest,
                |target| Ok::<_, E2bHostError>(target.clone()),
            )
            .map_err(|_| E2bRuntimeError::Unknown("e2b_runtime_target_unconfirmed"))?;
        let EnvironmentProfile::E2b(profile) = &self.config.profile else {
            return Err(E2bRuntimeError::Rejected("e2b_runtime_profile_unsupported"));
        };
        let binding = E2bSandboxBinding {
            sandbox_id: target.sandbox_id.clone(),
            template_id: profile.template_ref.template_id.clone(),
            metadata: expected_metadata(&operation.identity),
        };
        let dispatch = Arc::new(RebindableAuthority::new(
            self.store.clone(),
            authority,
            &target,
            &self.config.provider_config_digest,
        )?);
        let transport = Arc::new(E2bSdkTransport::new(
            ordinary,
            control,
            self.config.profile.clone(),
        ));
        let runner = Arc::new(
            E2bExecutionHostRunner::new(
                self.config.profile.clone(),
                binding.clone(),
                transport.clone(),
                dispatch.clone(),
            )
            .map_err(|_| E2bRuntimeError::Unknown("e2b_runtime_host_unconfirmed"))?,
        );
        Ok(E2bPrepareOutcome::Prepared(Arc::new(E2bPreparedHost {
            runner,
            authority: PreparedAuthority::PerRun(dispatch),
            logical_execution_id: execution_id.into(),
            workspace_reused_live: false,
        })))
    }
}

/// Retain this complete object across yield/reclaim. Constructing a new runner
/// for an existing operation would discard command and mutating-helper evidence.
pub(crate) struct E2bPreparedHost {
    runner: Arc<E2bExecutionHostRunner>,
    authority: PreparedAuthority,
    logical_execution_id: String,
    workspace_reused_live: bool,
}

enum PreparedAuthority {
    PerRun(Arc<RebindableAuthority>),
    Retained(Arc<retained::RetainedRunAuthority>),
}

impl E2bPreparedHost {
    #[cfg(test)]
    pub(crate) fn identity(&self) -> &ProviderCreationIdentity {
        match &self.authority {
            PreparedAuthority::PerRun(authority) => &authority.identity,
            PreparedAuthority::Retained(authority) => &authority.use_binding.target.identity,
        }
    }

    #[cfg(test)]
    pub(crate) fn binding(&self) -> &E2bSandboxBinding {
        self.runner.binding()
    }

    pub(crate) fn runner(&self) -> Arc<E2bExecutionHostRunner> {
        self.runner.clone()
    }

    pub(crate) fn rebind(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
    ) -> Result<(), E2bRuntimeError> {
        match &self.authority {
            PreparedAuthority::PerRun(current) => current.replace(authority),
            PreparedAuthority::Retained(current) => current.replace(authority),
        }
    }

    pub(crate) fn logical_execution_id(&self) -> &str {
        &self.logical_execution_id
    }
    pub(crate) fn workspace_reused_live(&self) -> bool {
        self.workspace_reused_live
    }
    pub(crate) fn retained_use(
        &self,
    ) -> Option<&crate::postgres_store::provider_operations::retained::RetainedProviderRunUse> {
        match &self.authority {
            PreparedAuthority::PerRun(_) => None,
            PreparedAuthority::Retained(current) => Some(&current.use_binding),
        }
    }
}

struct NeverCreateAdapter;
impl E2bCreationAdapter for NeverCreateAdapter {
    fn create(&self, _: &str, _: Value, _: Duration) -> Result<AdapterResponse, AdapterFailure> {
        Err(AdapterFailure::NotDispatched(
            "e2b_runtime_loaded_attempt_cannot_send",
        ))
    }
}

struct RecordingCreationAdapter<'a> {
    ordinary: &'a SpoolingAdapterProcess,
    sent: AtomicBool,
    succeeded: AtomicBool,
}
impl E2bCreationAdapter for RecordingCreationAdapter<'_> {
    fn create(
        &self,
        request_id: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<AdapterResponse, AdapterFailure> {
        self.sent.store(true, Ordering::Release);
        let response = self.ordinary.create(request_id, payload, timeout)?;
        self.succeeded.store(
            response.outcome == AdapterOutcome::Succeeded,
            Ordering::Release,
        );
        Ok(response)
    }
}

fn pending_existing(operation: ProviderCreationOperation) -> E2bPrepareOutcome {
    let reason = if operation.dispatch_state == ProviderCreationDispatchState::NotDispatched {
        E2bPendingReason::CreationNotDispatched
    } else {
        E2bPendingReason::OwnedContextUnavailable
    };
    E2bPrepareOutcome::Pending {
        operation: Box::new(operation),
        reason,
    }
}

struct RebindableAuthority {
    store: PostgresRuntimeStore,
    identity: ProviderCreationIdentity,
    sandbox_id: String,
    configured_digest: String,
    current: RwLock<Arc<PostgresE2bDispatchAuthority>>,
}

impl RebindableAuthority {
    fn new(
        store: PostgresRuntimeStore,
        authority: &ControllerAuthorityRequest<'_>,
        target: &ProviderVerifiedTarget,
        configured_digest: &str,
    ) -> Result<Self, E2bRuntimeError> {
        let current = PostgresE2bDispatchAuthority::new(
            store.clone(),
            authority,
            target.identity.clone(),
            configured_digest,
            &target.sandbox_id,
        )
        .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_authority_rejected"))?;
        Ok(Self {
            store,
            identity: target.identity.clone(),
            sandbox_id: target.sandbox_id.clone(),
            configured_digest: configured_digest.into(),
            current: RwLock::new(Arc::new(current)),
        })
    }

    fn replace(&self, authority: &ControllerAuthorityRequest<'_>) -> Result<(), E2bRuntimeError> {
        let replacement = PostgresE2bDispatchAuthority::new(
            self.store.clone(),
            authority,
            self.identity.clone(),
            &self.configured_digest,
            &self.sandbox_id,
        )
        .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_authority_rejected"))?;
        self.store
            .with_provider_execution_dispatch(
                authority,
                &self.identity,
                &self.configured_digest,
                |target| {
                    if target.identity != self.identity || target.sandbox_id != self.sandbox_id {
                        return Err(E2bHostError::NotDispatched("e2b_runtime_target_rejected"));
                    }
                    Ok(())
                },
            )
            .map_err(|error| match error {
                ProviderExecutionDispatchError::AfterDispatch(_) => {
                    E2bRuntimeError::Unknown("e2b_runtime_rebind_unconfirmed")
                }
                _ => E2bRuntimeError::Rejected("e2b_runtime_authority_rejected"),
            })?;
        // No lock is held during validation or an SDK wait. An installed stale
        // grant still fails the Store fence before any subsequent physical start.
        *self
            .current
            .write()
            .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_authority_unavailable"))? =
            Arc::new(replacement);
        Ok(())
    }
}

impl E2bDispatchAuthority for RebindableAuthority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        let current = self
            .current
            .read()
            .map_err(|_| E2bHostError::NotDispatched("e2b_runtime_authority_unavailable"))?
            .clone();
        current.dispatch(operation)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdapterCapabilities {
    sdk_version: String,
    lane: String,
    create_idempotency_configured: bool,
    read_only_attach: bool,
    command_start_idempotency: bool,
    run_process_tree_cancellation: bool,
    control_command_cancellation: bool,
    command_output_bytes: bool,
    command_output_restart_recovery: bool,
    command_output_recovery: String,
    wait_budget_must_cover_original_stream: bool,
    max_command_evidence: u32,
    fixed_control_helpers: Vec<String>,
    bulk_file_transfer: bool,
    max_transfer_bytes: u64,
    transfer_chunk_bytes: usize,
    storage_quota: bool,
    full_memory_pause_verified: bool,
    max_file_bytes: u64,
    max_output_bytes: u64,
}

fn require_capabilities(
    process: &SpoolingAdapterProcess,
    lane: E2bAdapterLane,
    timeout: Duration,
) -> Result<(), E2bRuntimeError> {
    require_capabilities_with_requirement(process, lane, timeout, false)
}

fn require_capabilities_for_profile(
    process: &SpoolingAdapterProcess,
    lane: E2bAdapterLane,
    timeout: Duration,
    profile: &EnvironmentProfile,
) -> Result<(), E2bRuntimeError> {
    require_capabilities_with_requirement(
        process,
        lane,
        timeout,
        managed_workspace_required(profile),
    )
}

fn managed_workspace_required(profile: &EnvironmentProfile) -> bool {
    matches!(profile, EnvironmentProfile::E2b(value)
        if value.workspace_initialization == Some(crate::agent_run_authorization::WorkspaceInitialization::ManagedFiles))
}

fn require_capabilities_with_requirement(
    process: &SpoolingAdapterProcess,
    lane: E2bAdapterLane,
    timeout: Duration,
    managed_workspace_required: bool,
) -> Result<(), E2bRuntimeError> {
    let mut span = crate::observations::enabled().then(|| {
        crate::observations::Span::new(match lane {
            E2bAdapterLane::Ordinary => "e2bAdapterHandshakeOrdinary",
            E2bAdapterLane::Control => "e2bAdapterHandshakeControl",
            E2bAdapterLane::Cleanup => "e2bAdapterHandshakeCleanup",
        })
    });
    let result = require_capabilities_inner(process, lane, timeout, managed_workspace_required);
    if let Some(span) = &mut span {
        span.finished(result.is_ok());
    }
    result
}

fn require_capabilities_inner(
    process: &SpoolingAdapterProcess,
    lane: E2bAdapterLane,
    timeout: Duration,
    managed_workspace_required: bool,
) -> Result<(), E2bRuntimeError> {
    let response = process
        .call(
            &format!("e2b-runtime-capabilities-{}", lane.as_str()),
            "capabilities",
            json!({}),
            timeout,
        )
        .map_err(|_| E2bRuntimeError::Unavailable("e2b_runtime_adapter_unavailable"))?;
    if response.outcome != AdapterOutcome::Succeeded {
        return Err(E2bRuntimeError::Unavailable(
            "e2b_runtime_adapter_unavailable",
        ));
    }
    let capabilities: AdapterCapabilities = serde_json::from_value(response.result.ok_or(
        E2bRuntimeError::Rejected("e2b_runtime_capabilities_invalid"),
    )?)
    .map_err(|_| E2bRuntimeError::Rejected("e2b_runtime_capabilities_invalid"))?;
    validate_capabilities(capabilities, lane, managed_workspace_required)
}

fn validate_capabilities(
    capabilities: AdapterCapabilities,
    lane: E2bAdapterLane,
    managed_workspace_required: bool,
) -> Result<(), E2bRuntimeError> {
    // A configured idempotency declaration is never used as permission to send
    // or retry. The durable creation CAS remains the only such authority.
    let _configured_idempotency = capabilities.create_idempotency_configured;
    let mut helpers: Vec<_> = capabilities
        .fixed_control_helpers
        .iter()
        .map(String::as_str)
        .collect();
    helpers.sort_unstable();
    // Accept only the two complete known advertisements, never an arbitrary
    // superset or a deduplicated list that could conceal a malformed contract.
    let helpers_supported = match lane {
        E2bAdapterLane::Ordinary | E2bAdapterLane::Cleanup => helpers.is_empty(),
        E2bAdapterLane::Control => {
            helpers.as_slice()
                == [
                    "agent-process-state",
                    "prepare-managed-workspace",
                    "prepare-workspace",
                    "quiesce-agent-processes",
                ]
                || (!managed_workspace_required
                    && helpers.as_slice()
                        == [
                            "agent-process-state",
                            "prepare-workspace",
                            "quiesce-agent-processes",
                        ])
        }
    };
    if capabilities.sdk_version != "2.58.0"
        || capabilities.lane != lane.as_str()
        || !capabilities.read_only_attach
        || capabilities.command_start_idempotency
        || capabilities.run_process_tree_cancellation
        || capabilities.control_command_cancellation
        || !capabilities.command_output_bytes
        || capabilities.command_output_restart_recovery
        || capabilities.command_output_recovery != "adapterLifetime"
        || !capabilities.wait_budget_must_cover_original_stream
        || capabilities.max_command_evidence != 128
        || !helpers_supported
        || !capabilities.bulk_file_transfer
        || capabilities.max_transfer_bytes != MAX_TRANSFER_BYTES
        || capabilities.transfer_chunk_bytes != TRANSFER_CHUNK_BYTES
        || capabilities.storage_quota
        || capabilities.full_memory_pause_verified
        || capabilities.max_file_bytes != 256 * 1024
        || capabilities.max_output_bytes != 64 * 1024
    {
        return Err(E2bRuntimeError::Rejected(
            "e2b_runtime_capabilities_unsupported",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    const ORIGINAL_HELPERS: [&str; 3] = [
        "agent-process-state",
        "prepare-workspace",
        "quiesce-agent-processes",
    ];
    const EXTENDED_HELPERS: [&str; 4] = [
        "agent-process-state",
        "prepare-workspace",
        "prepare-managed-workspace",
        "quiesce-agent-processes",
    ];

    fn capabilities(lane: E2bAdapterLane, helpers: &[&str]) -> AdapterCapabilities {
        serde_json::from_value(json!({
            "sdkVersion":"2.58.0", "lane":lane.as_str(),
            "createIdempotencyConfigured":false, "readOnlyAttach":true,
            "commandStartIdempotency":false, "runProcessTreeCancellation":false,
            "controlCommandCancellation":false, "commandOutputBytes":true,
            "commandOutputRestartRecovery":false, "commandOutputRecovery":"adapterLifetime",
            "waitBudgetMustCoverOriginalStream":true, "maxCommandEvidence":128,
            "fixedControlHelpers":helpers, "bulkFileTransfer":true,
            "maxTransferBytes":MAX_TRANSFER_BYTES, "transferChunkBytes":TRANSFER_CHUNK_BYTES,
            "storageQuota":false, "fullMemoryPauseVerified":false,
            "maxFileBytes":256 * 1024, "maxOutputBytes":64 * 1024,
        }))
        .unwrap()
    }

    fn unsupported() -> Result<(), E2bRuntimeError> {
        Err(E2bRuntimeError::Rejected(
            "e2b_runtime_capabilities_unsupported",
        ))
    }

    #[test]
    fn legacy_control_accepts_original_or_known_extended_helpers() {
        for helpers in [ORIGINAL_HELPERS.as_slice(), EXTENDED_HELPERS.as_slice()] {
            let mut reordered = helpers.to_vec();
            reordered.reverse();
            for values in [helpers, reordered.as_slice()] {
                assert_eq!(
                    validate_capabilities(
                        capabilities(E2bAdapterLane::Control, values),
                        E2bAdapterLane::Control,
                        false
                    ),
                    Ok(())
                );
            }
        }
    }

    #[test]
    fn managed_control_requires_the_exact_managed_workspace_extension() {
        let mut profile: EnvironmentProfile = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        )))
        .unwrap();
        assert!(!managed_workspace_required(&profile));
        let EnvironmentProfile::E2b(value) = &mut profile else {
            unreachable!()
        };
        value.workspace_initialization =
            Some(crate::agent_run_authorization::WorkspaceInitialization::ManagedFiles);
        assert!(managed_workspace_required(&profile));
        assert_eq!(
            validate_capabilities(
                capabilities(E2bAdapterLane::Control, &ORIGINAL_HELPERS),
                E2bAdapterLane::Control,
                true
            ),
            unsupported()
        );
        assert_eq!(
            validate_capabilities(
                capabilities(E2bAdapterLane::Control, &EXTENDED_HELPERS),
                E2bAdapterLane::Control,
                true
            ),
            Ok(())
        );
    }

    #[test]
    fn control_rejects_unknown_missing_and_duplicate_helpers() {
        let cases = [
            vec!["agent-process-state", "prepare-workspace"],
            vec![
                "agent-process-state",
                "prepare-workspace",
                "quiesce-agent-processes",
                "unknown-helper",
            ],
            vec![
                "agent-process-state",
                "prepare-workspace",
                "quiesce-agent-processes",
                "prepare-workspace",
            ],
            vec![
                "agent-process-state",
                "prepare-workspace",
                "quiesce-agent-processes",
                "prepare-managed-workspace",
                "prepare-managed-workspace",
            ],
        ];
        for helpers in cases {
            for managed in [false, true] {
                assert_eq!(
                    validate_capabilities(
                        capabilities(E2bAdapterLane::Control, &helpers),
                        E2bAdapterLane::Control,
                        managed
                    ),
                    unsupported()
                );
            }
        }
    }

    #[test]
    fn noncontrol_lanes_require_an_empty_helper_advertisement() {
        for lane in [E2bAdapterLane::Ordinary, E2bAdapterLane::Cleanup] {
            for managed in [false, true] {
                assert_eq!(
                    validate_capabilities(capabilities(lane, &[]), lane, managed),
                    Ok(())
                );
                for helpers in [ORIGINAL_HELPERS.as_slice(), EXTENDED_HELPERS.as_slice()] {
                    assert_eq!(
                        validate_capabilities(capabilities(lane, helpers), lane, managed),
                        unsupported()
                    );
                }
            }
        }
    }

    #[test]
    fn helper_negotiation_preserves_the_other_capability_requirements() {
        for changed in ["sdk", "lane", "attach", "output", "bulk", "quota", "pause"] {
            let mut value = capabilities(E2bAdapterLane::Control, &ORIGINAL_HELPERS);
            match changed {
                "sdk" => value.sdk_version = "unsupported".into(),
                "lane" => value.lane = "ordinary".into(),
                "attach" => value.read_only_attach = false,
                "output" => value.command_output_bytes = false,
                "bulk" => value.bulk_file_transfer = false,
                "quota" => value.storage_quota = true,
                "pause" => value.full_memory_pause_verified = true,
                _ => unreachable!(),
            }
            assert_eq!(
                validate_capabilities(value, E2bAdapterLane::Control, false),
                unsupported(),
                "{changed}"
            );
        }
    }
}
