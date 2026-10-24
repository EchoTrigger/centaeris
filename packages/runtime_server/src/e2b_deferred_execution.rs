//! One step-owned deferred E2B execution. Selection and signed managed-file
//! eligibility are checked by the caller before constructing this shared port.
use crate::agent_run_authorization::SessionWorkspace;
use crate::artifact_publication::ArtifactReadPort;
use crate::hosted_execution_context::{HostedExecutionContext, SessionWorkspacePort};
use crate::session_workspace::{
    SessionWorkspaceApi, SessionWorkspaceApiError, SessionWorkspaceCommitOutcome,
    SessionWorkspaceLease, SessionWorkspaceResolution,
};
use centaeris_core::execution::{
    ExecutionCancellationProbe, ExecutionCommandRequest, ExecutionError, ExecutionFileSystemError,
    ExecutionFileSystemErrorKind, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
    ExecutionHostCommandOutput, ExecutionHostCommandRecovery, ExecutionHostKind,
    ExecutionHostRunner, ExecutionHostStatus, ExecutionPolicy, ExecutionWorkspaceGeneration,
};
use hosted_execution::protocol::SandboxArtifactMetadata;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

type Factory = Box<dyn FnOnce() -> Result<HostedExecutionContext, String> + Send>;
const CONTROL_POLL: Duration = Duration::from_millis(50);

pub(crate) struct DeferredWorkspaceBinding {
    pub(crate) client: reqwest::blocking::Client,
    pub(crate) api_url: String,
    pub(crate) api_token: String,
    pub(crate) agent_run_id: String,
    pub(crate) authorization_digest: String,
    pub(crate) lease: SessionWorkspaceLease,
    pub(crate) frozen: SessionWorkspace,
}

enum Phase {
    Deferred,
    CheckingWorkspace,
    Preparing,
    Ready(Arc<HostedExecutionContext>),
    Failed(String),
}
struct State {
    phase: Phase,
    factory: Option<Factory>,
}

pub(crate) struct DeferredE2bExecution {
    state: Mutex<State>,
    changed: Condvar,
    workspace: DeferredWorkspaceBinding,
    cancellation_probe: Arc<ExecutionCancellationProbe>,
}

impl DeferredE2bExecution {
    pub(crate) fn new(
        factory: impl FnOnce() -> Result<HostedExecutionContext, String> + Send + 'static,
        workspace: DeferredWorkspaceBinding,
        cancellation_probe: Arc<ExecutionCancellationProbe>,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                phase: Phase::Deferred,
                factory: Some(Box::new(factory)),
            }),
            changed: Condvar::new(),
            workspace,
            cancellation_probe,
        }
    }

    fn require_current(&self) -> Result<(), String> {
        match (self.cancellation_probe)() {
            Ok(None) => Ok(()),
            Ok(Some(_)) => Err("e2b_deferred_execution_cancelled".into()),
            Err(_) => Err("e2b_deferred_control_unavailable".into()),
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, String> {
        self.state
            .lock()
            .map_err(|_| "e2b_deferred_state_unavailable".into())
    }

    fn wait(&self, state: MutexGuard<'_, State>) -> Result<(), String> {
        let (state, _) = self
            .changed
            .wait_timeout(state, CONTROL_POLL)
            .map_err(|_| "e2b_deferred_state_unavailable".to_string())?;
        drop(state);
        Ok(())
    }

    fn require_empty_binding(&self) -> Result<(), String> {
        let frozen = &self.workspace.frozen;
        frozen
            .validate()
            .map_err(|_| "e2b_deferred_workspace_binding_rejected")?;
        if frozen.snapshot_size_bytes != 0
            || !frozen.snapshot_sha256.is_empty()
            || frozen.expanded_size_bytes != 0
            || frozen.file_count != 0
        {
            return Err("e2b_deferred_workspace_binding_rejected".into());
        }
        Ok(())
    }

    fn require_workspace_scope(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<(), SessionWorkspaceApiError> {
        self.require_empty_binding()
            .map_err(SessionWorkspaceApiError::Rejected)?;
        if lease.job_id != self.workspace.lease.job_id
            || lease.lease_owner != self.workspace.lease.lease_owner
            || frozen != &self.workspace.frozen
            || inputs != 0
        {
            return Err(SessionWorkspaceApiError::Rejected(
                "e2b_deferred_workspace_binding_rejected".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn ensure_ready(&self) -> Result<Arc<HostedExecutionContext>, String> {
        self.require_empty_binding()?;
        loop {
            self.require_current()?;
            let mut state = self.lock()?;
            match &state.phase {
                Phase::Ready(execution) => return Ok(execution.clone()),
                Phase::Failed(reason) => return Err(reason.clone()),
                Phase::Preparing | Phase::CheckingWorkspace => {
                    self.wait(state)?;
                }
                Phase::Deferred => {
                    let factory = state
                        .factory
                        .take()
                        .ok_or("e2b_deferred_factory_unavailable")?;
                    state.phase = Phase::Preparing;
                    drop(state);
                    // The factory may perform irreversible provider work. Store its
                    // result before observing cancellation again; a late cancellation
                    // cannot erase a prepared context or permit another invocation.
                    let result = catch_unwind(AssertUnwindSafe(factory))
                        .unwrap_or_else(|_| Err("e2b_deferred_prepare_panicked".into()));
                    let mut state = self.lock()?;
                    state.phase = match result {
                        Ok(execution) => Phase::Ready(Arc::new(execution)),
                        Err(reason) => Phase::Failed(reason),
                    };
                    self.changed.notify_all();
                }
            }
        }
    }

    /// None is a serialized, current API proof that this deferred workspace is
    /// still empty. Physical preparation cannot race that proof. No mutex is held
    /// across HTTP, and demand waiters continue checking cancellation.
    fn live_or_empty(
        &self,
    ) -> Result<Option<Arc<HostedExecutionContext>>, SessionWorkspaceApiError> {
        loop {
            self.require_current()
                .map_err(SessionWorkspaceApiError::Unavailable)?;
            let mut state = self.lock().map_err(SessionWorkspaceApiError::Unavailable)?;
            match &state.phase {
                Phase::Ready(execution) => return Ok(Some(execution.clone())),
                Phase::Failed(reason) => {
                    return Err(SessionWorkspaceApiError::Unavailable(reason.clone()))
                }
                Phase::Preparing => {
                    drop(state);
                    return self
                        .ensure_ready()
                        .map(Some)
                        .map_err(SessionWorkspaceApiError::Unavailable);
                }
                Phase::CheckingWorkspace => {
                    self.wait(state)
                        .map_err(SessionWorkspaceApiError::Unavailable)?;
                }
                Phase::Deferred => {
                    state.phase = Phase::CheckingWorkspace;
                    drop(state);
                    let resolved = catch_unwind(AssertUnwindSafe(|| {
                        self.api()
                            .resolve(&self.workspace.lease, &self.workspace.frozen)
                    }))
                    .unwrap_or_else(|_| {
                        Err(SessionWorkspaceApiError::Unavailable(
                            "e2b_deferred_workspace_read_panicked".into(),
                        ))
                    });
                    let mut state = self.lock().map_err(SessionWorkspaceApiError::Unavailable)?;
                    state.phase = Phase::Deferred;
                    self.changed.notify_all();
                    drop(state);
                    self.require_current()
                        .map_err(SessionWorkspaceApiError::Unavailable)?;
                    return match resolved {
                        Ok(SessionWorkspaceResolution::Empty) => Ok(None),
                        Ok(_) => Err(SessionWorkspaceApiError::Rejected(
                            "e2b_deferred_workspace_not_empty".into(),
                        )),
                        Err(error) => Err(error),
                    };
                }
            }
        }
    }

    fn api(&self) -> SessionWorkspaceApi<'_> {
        SessionWorkspaceApi {
            client: &self.workspace.client,
            api_url: &self.workspace.api_url,
            api_token: &self.workspace.api_token,
            agent_run_id: &self.workspace.agent_run_id,
            authorization_digest: &self.workspace.authorization_digest,
        }
    }
}

impl ExecutionHostRunner for DeferredE2bExecution {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::RemoteHost
    }

    fn status(&self, policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        self.ensure_ready()
            .map_err(host_error)?
            .runner()
            .status(policy)
    }

    fn workspace_generation(&self) -> ExecutionWorkspaceGeneration {
        if let Ok(state) = self.lock() {
            if let Phase::Ready(execution) = &state.phase {
                let execution = execution.clone();
                drop(state);
                return execution.runner().workspace_generation();
            }
        }
        ExecutionWorkspaceGeneration::Unknown {
            reason: "e2b_deferred_execution_not_materialized".into(),
        }
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        self.ensure_ready()
            .map_err(file_error)?
            .runner()
            .run_file_system_operation(request)
    }

    fn run_host_command(
        &self,
        operation_id: Option<&str>,
        request: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        self.ensure_ready()
            .map_err(host_error)?
            .runner()
            .run_host_command(operation_id, request, cancellation_probe)
    }

    fn recover_host_command(
        &self,
        operation_id: &str,
        request: ExecutionCommandRequest,
        cancellation_probe: Option<&ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandRecovery, ExecutionError> {
        self.require_current().map_err(host_error)?;
        let state = self.lock().map_err(host_error)?;
        let execution = match &state.phase {
            Phase::Ready(execution) => execution.clone(),
            Phase::Preparing => {
                drop(state);
                return self
                    .ensure_ready()
                    .map_err(host_error)?
                    .runner()
                    .recover_host_command(operation_id, request, cancellation_probe);
            }
            Phase::Deferred | Phase::CheckingWorkspace | Phase::Failed(_) => {
                return Ok(ExecutionHostCommandRecovery::Unknown {
                    reason: "e2b_deferred_original_execution_unavailable".into(),
                });
            }
        };
        drop(state);
        execution
            .runner()
            .recover_host_command(operation_id, request, cancellation_probe)
    }
}

impl ArtifactReadPort for DeferredE2bExecution {
    fn read_artifact(&self, path: &str) -> Result<(SandboxArtifactMetadata, Vec<u8>), String> {
        self.ensure_ready()?.artifact_reader().read_artifact(path)
    }
}

impl SessionWorkspacePort for DeferredE2bExecution {
    fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.require_workspace_scope(lease, frozen, 0)?;
        match self.live_or_empty()? {
            Some(execution) => execution.resolve_session_workspace(lease, frozen),
            None => Ok(SessionWorkspaceResolution::Empty),
        }
    }

    fn restore(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.require_workspace_scope(lease, frozen, inputs)?;
        match self.live_or_empty()? {
            Some(execution) => execution.restore_session_workspace(lease, frozen, inputs),
            None => Ok(SessionWorkspaceResolution::Empty),
        }
    }

    fn collect_and_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        inputs: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        if let Err(error) = self.require_workspace_scope(lease, frozen, inputs) {
            return match error {
                SessionWorkspaceApiError::Rejected(reason) => {
                    Ok(SessionWorkspaceCommitOutcome::Rejected(reason))
                }
                SessionWorkspaceApiError::Unavailable(_) => {
                    Ok(SessionWorkspaceCommitOutcome::Pending)
                }
            };
        }
        match self.live_or_empty() {
            Ok(Some(execution)) => {
                execution.collect_and_commit_session_workspace(lease, frozen, inputs)
            }
            Ok(None) => Ok(SessionWorkspaceCommitOutcome::Unchanged),
            Err(SessionWorkspaceApiError::Unavailable(_)) => {
                Ok(SessionWorkspaceCommitOutcome::Pending)
            }
            Err(SessionWorkspaceApiError::Rejected(reason)) => {
                Ok(SessionWorkspaceCommitOutcome::Rejected(reason))
            }
        }
    }
}

fn host_error(reason: String) -> ExecutionError {
    ExecutionError::HostUnavailable { reason }
}
fn file_error(reason: String) -> ExecutionFileSystemError {
    ExecutionFileSystemError::new(ExecutionFileSystemErrorKind::HostUnavailable, reason)
}

#[cfg(test)]
mod tests;
