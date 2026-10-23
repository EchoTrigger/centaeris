//! Provider selection for one existing Core execution binding.
//! The facade owns ports; it does not interpret commands, Core events or recovery.
use crate::agent_run_authorization::SessionWorkspace;
use crate::artifact_publication::ArtifactReadPort;
use crate::docker_execution_host::DockerExecutionHostRunner;
use crate::e2b_execution_host::E2bExecutionHostRunner;
use crate::e2b_session_workspace::E2bSessionWorkspace;
use crate::session_workspace::{
    SessionWorkspaceApiError, SessionWorkspaceCommitOutcome, SessionWorkspaceLease,
    SessionWorkspaceResolution,
};
use centaeris_core::execution::{ExecutionHostRunner, ExecutionPolicy};
use centaeris_core::extension::hooks::{
    LifecycleHookCommandResultV1, LifecycleHookEventV1, LifecycleHookHandlerV1, LifecycleHookRunner,
};
use centaeris_core::extension::{validate_plugin_activation_snapshot, PluginActivationSnapshotV1};
use std::sync::Arc;

pub(crate) trait SessionWorkspacePort: Send + Sync {
    fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError>;
    fn restore(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError>;
    fn collect_and_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String>;
}

impl SessionWorkspacePort for DockerExecutionHostRunner {
    fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.resolve_session_workspace(lease, frozen)
    }
    fn restore(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.restore_session_workspace(lease, frozen, inputs)
    }
    fn collect_and_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        self.collect_and_commit_session_workspace(lease, frozen, inputs)
    }
}

impl SessionWorkspacePort for E2bSessionWorkspace {
    fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        E2bSessionWorkspace::resolve(self, lease, frozen)
    }
    fn restore(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        E2bSessionWorkspace::restore(self, lease, frozen, inputs)
    }
    fn collect_and_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        E2bSessionWorkspace::collect_and_commit(self, lease, frozen, inputs)
    }
}

pub(crate) struct HostedExecutionContext {
    runner: Arc<dyn ExecutionHostRunner>,
    artifact_reader: Arc<dyn ArtifactReadPort>,
    hook_runner: Arc<dyn LifecycleHookRunner + Send + Sync>,
    workspace: Arc<dyn SessionWorkspacePort>,
    execution_id: String,
    workspace_root: String,
    policy: ExecutionPolicy,
    legacy: Option<Arc<DockerExecutionHostRunner>>,
}

impl HostedExecutionContext {
    pub(crate) fn from_legacy(host: Arc<DockerExecutionHostRunner>) -> Self {
        Self {
            execution_id: host.execution_id().into(),
            workspace_root: host.workspace_root().into(),
            policy: host.execution_policy(),
            runner: host.clone(),
            artifact_reader: host.clone(),
            hook_runner: host.clone(),
            workspace: host.clone(),
            legacy: Some(host),
        }
    }

    pub(crate) fn e2b(
        host: Arc<E2bExecutionHostRunner>,
        workspace: Arc<E2bSessionWorkspace>,
        logical_execution_id: &str,
        activation: &PluginActivationSnapshotV1,
    ) -> Result<Self, String> {
        Self::require_no_lifecycle_hooks(activation)?;
        Ok(Self {
            execution_id: logical_execution_id.into(),
            workspace_root: host.workspace_root().into(),
            policy: host.execution_policy(),
            runner: host.clone(),
            artifact_reader: host,
            hook_runner: Arc::new(UnsupportedLifecycleHooks),
            workspace,
            legacy: None,
        })
    }

    /// Call during provider preflight, before any physical creation.
    pub(crate) fn require_no_lifecycle_hooks(
        activation: &PluginActivationSnapshotV1,
    ) -> Result<(), String> {
        validate_plugin_activation_snapshot(activation)?;
        if activation
            .packages
            .iter()
            .any(|package| !package.hooks.is_empty())
        {
            return Err("e2b_lifecycle_hooks_unsupported".into());
        }
        Ok(())
    }

    pub(crate) fn runner(&self) -> Arc<dyn ExecutionHostRunner> {
        self.runner.clone()
    }
    pub(crate) fn artifact_reader(&self) -> Arc<dyn ArtifactReadPort> {
        self.artifact_reader.clone()
    }
    pub(crate) fn hook_runner(&self) -> Arc<dyn LifecycleHookRunner + Send + Sync> {
        self.hook_runner.clone()
    }
    pub(crate) fn execution_id(&self) -> &str {
        &self.execution_id
    }
    pub(crate) fn workspace_root(&self) -> &str {
        &self.workspace_root
    }
    pub(crate) fn execution_policy(&self) -> ExecutionPolicy {
        self.policy.clone()
    }
    pub(crate) fn legacy(&self) -> Option<&Arc<DockerExecutionHostRunner>> {
        self.legacy.as_ref()
    }

    pub(crate) fn resolve_session_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.workspace.resolve(lease, frozen)
    }
    pub(crate) fn restore_session_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.workspace.restore(lease, frozen, inputs)
    }
    pub(crate) fn collect_and_commit_session_workspace(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        self.workspace.collect_and_commit(lease, frozen, inputs)
    }
}

struct UnsupportedLifecycleHooks;
impl LifecycleHookRunner for UnsupportedLifecycleHooks {
    fn run_hook(
        &self,
        _handler: &LifecycleHookHandlerV1,
        _event: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1 {
        LifecycleHookCommandResultV1 {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            spawn_error: Some("e2b_lifecycle_hooks_unsupported".into()),
        }
    }
}

#[cfg(test)]
mod tests;
