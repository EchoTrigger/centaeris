//! Typed local Session operations shared by host entry points.
//!
//! The existing Runtime owns admission, execution, receipts and event delivery.
//! This facade keeps those owners behind one Session API.

use crate::agent_runs::{
    self, AgentRunAttachRequest, AgentRunAttachResponse, AgentRunCancelRequest,
    AgentRunCancelResponse,
};
use crate::agent_runtime::{self, AgentInputRequest, AgentInputResponse};
use crate::errors::RuntimeHostError;
use crate::runtime_rpc_transport::EventWriter;
use crate::sessions::{self, SessionCreateCommandRequest, SessionItemResponse};

pub(crate) struct LocalSessionApi {
    events: EventWriter,
}

impl LocalSessionApi {
    pub(crate) fn new(events: EventWriter) -> Self {
        Self { events }
    }

    pub(crate) fn create(
        &self,
        request: SessionCreateCommandRequest,
    ) -> Result<SessionItemResponse, RuntimeHostError> {
        sessions::create_command(request)
            .map_err(|error| RuntimeHostError::new(error.code(), error.message()))
    }

    pub(crate) fn send(
        &self,
        request: AgentInputRequest,
    ) -> Result<AgentInputResponse, RuntimeHostError> {
        agent_runtime::input(self.events.clone(), request)
            .map_err(|error| RuntimeHostError::new(error.code(), error.message()))
    }

    /// Attach this connection's registered viewer to a Run. Events continue on
    /// the connection's existing `session/update` channel.
    pub(crate) fn subscribe(
        &self,
        request: AgentRunAttachRequest,
    ) -> Result<AgentRunAttachResponse, RuntimeHostError> {
        let viewer_id = request.viewer_id.clone().unwrap_or_default();
        self.events.with_registered_viewer(&viewer_id, || {
            agent_runs::attach(request)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))
        })
    }

    pub(crate) fn cancel(
        &self,
        request: AgentRunCancelRequest,
    ) -> Result<AgentRunCancelResponse, RuntimeHostError> {
        agent_runtime::cancel_agent_run(self.events.clone(), request)
            .map_err(|error| RuntimeHostError::new("agent_task_failed", error))
    }
}
