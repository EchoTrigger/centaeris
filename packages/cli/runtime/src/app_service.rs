//! Shared request dispatch and client admission for the local app service.

use crate::agent_runtime;
use crate::commands::RuntimeHostCommand;
use crate::errors::RuntimeHostError;
use crate::handlers::{handle_request_async, handle_stateless_request_async, RuntimeHostState};
use crate::protocol::HostCommandRequest;
use crate::rpc_stream::RpcConnection;
use crate::runtime_rpc_transport::{EventWriter, RuntimeServerClientHub};
use centaeris_core::session::reliability::{ListRuntimeJobsRequest, RuntimeJobStatus};
use std::sync::{Arc, Mutex};

/// One local host's request state and client registry.
#[derive(Clone)]
pub struct LocalAppService {
    clients: Arc<RuntimeServerClientHub>,
    state: Arc<Mutex<RuntimeHostState>>,
}

impl LocalAppService {
    pub(crate) fn new(clients: Arc<RuntimeServerClientHub>) -> Self {
        Self {
            clients,
            state: Arc::new(Mutex::new(RuntimeHostState::default())),
        }
    }

    /// Runs a framed RPC host under the exclusive local profile writer lock.
    pub async fn run_exclusive<F, Fut>(serve: F) -> Result<(), RuntimeHostError>
    where
        F: FnOnce(Self) -> Fut,
        Fut: std::future::Future<Output = Result<(), RuntimeHostError>>,
    {
        crate::local_host::run(serve).await
    }

    pub(crate) fn clients(&self) -> &Arc<RuntimeServerClientHub> {
        &self.clients
    }

    pub(crate) async fn request(
        &self,
        writer: EventWriter,
        request: HostCommandRequest,
    ) -> Result<serde_json::Value, RuntimeHostError> {
        let command = RuntimeHostCommand::parse(&request.command)?;
        let _permit = writer.request_permit(uses_control_admission(command))?;
        dispatch_host_command(Arc::clone(&self.state), writer, request).await
    }

    /// Reserves a client slot before the transport starts serving the stream.
    pub fn accept_rpc(&self) -> Result<Option<RpcConnection>, RuntimeHostError> {
        Ok(self.clients.connect()?.map(|(writer, outbound)| {
            RpcConnection::new(
                Arc::clone(&self.state),
                Arc::clone(&self.clients),
                writer,
                outbound,
            )
        }))
    }

    /// Waits until the host closes admission for shutdown.
    pub async fn shutdown_requested(&self) {
        self.clients.service_shutdown_requested().await;
    }

    /// Returns the generation used to detect activity during a listener idle window.
    pub fn activity_generation(&self) -> u64 {
        self.clients.activity_generation()
    }

    /// Checks for outstanding local work that keeps a listener alive.
    pub async fn is_idle(&self) -> Result<bool, RuntimeHostError> {
        runtime_server_is_idle(&self.clients).await
    }

    /// Closes admission when no clients or runs have arrived since the observed generation.
    pub fn begin_idle_shutdown(&self, generation: u64) -> Result<bool, RuntimeHostError> {
        self.clients.begin_idle_shutdown(generation)
    }
}

pub(crate) async fn dispatch_host_command(
    state: Arc<Mutex<RuntimeHostState>>,
    event_writer: EventWriter,
    host_request: HostCommandRequest,
) -> Result<serde_json::Value, RuntimeHostError> {
    let command = RuntimeHostCommand::parse(&host_request.command)?;
    if command != RuntimeHostCommand::Initialize {
        event_writer.require_registered()?;
    }
    if event_writer.is_draining() && !allowed_while_draining(command) {
        return Err(RuntimeHostError::new(
            "runtime_server_draining",
            "Runtime is shutting down; new work is not accepted",
        ));
    }
    let _action_permit =
        if command == RuntimeHostCommand::Initialize || allowed_while_draining(command) {
            None
        } else {
            Some(event_writer.start_action()?)
        };
    if let Some(value) = handle_stateless_request_async(&host_request).await? {
        return Ok(value);
    }
    handle_request_async(state, host_request, event_writer).await
}

// Transport headroom is reserved for lifecycle control, independently of which
// operations remain legal during shutdown. Ordinary reads use ordinary capacity.
pub(crate) fn uses_control_admission(command: RuntimeHostCommand) -> bool {
    matches!(
        command,
        RuntimeHostCommand::Initialize
            | RuntimeHostCommand::AgentRunCancel
            | RuntimeHostCommand::AgentRunDetach
            | RuntimeHostCommand::AgentRunDetachViewer
            | RuntimeHostCommand::AppExit
            | RuntimeHostCommand::RuntimeShutdown
            | RuntimeHostCommand::SidecarStop
    )
}

pub(crate) fn allowed_while_draining(command: RuntimeHostCommand) -> bool {
    command.operation_kind() == crate::runtime_command_registry::RuntimeOperationKind::Read
        || matches!(
            command,
            RuntimeHostCommand::AgentRunCancel
                | RuntimeHostCommand::AgentRunDetach
                | RuntimeHostCommand::AgentRunDetachViewer
                | RuntimeHostCommand::AppExit
                | RuntimeHostCommand::RuntimeShutdown
                | RuntimeHostCommand::SidecarStop
        )
}

async fn runtime_server_is_idle(
    clients: &RuntimeServerClientHub,
) -> Result<bool, RuntimeHostError> {
    if crate::schedules::keep_alive()
        || clients.has_clients_or_active_agent_runs()?
        || crate::terminal_sessions::manager().has_active()
        || crate::process_sessions::manager().has_active()
        || crate::process_sessions::manager().has_pending_notifications()
    {
        return Ok(false);
    }
    let store = agent_runtime::agent_runtime_job_store()
        .map_err(|error| RuntimeHostError::new("runtime_server_idle_state_failed", error))?;
    let active_jobs = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![
                RuntimeJobStatus::Queued,
                RuntimeJobStatus::Leased,
                RuntimeJobStatus::Running,
            ],
            job_kind: None,
            session_id: None,
            branch_id: None,
            limit: 1,
            offset: 0,
        })
        .await
        .map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_idle_state_failed",
                error.diagnostic_message(),
            )
        })?;
    Ok(active_jobs.is_empty())
}
