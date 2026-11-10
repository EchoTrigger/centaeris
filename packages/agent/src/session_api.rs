//! Native Agent SDK over an application-owned Session host.

pub use crate::errors::RuntimeHostError;
pub use crate::host::SessionHost;
pub use crate::operation::OperationId;
pub use crate::types::*;
pub use centaeris_core::runtime::contracts::AgentRunInterventionV1;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// A registered SDK client. Events from its Sessions share one bounded stream.
/// Dropping the client detaches its viewer. Accepted Runs remain host-owned.
pub struct AgentClient {
    host: Arc<dyn SessionHost>,
    events: mpsc::Receiver<SessionEvent>,
    disconnected: CancellationToken,
}

impl AgentClient {
    /// Connects an application-registered host and its canonical event stream.
    pub fn new(
        host: Arc<dyn SessionHost>,
        events: mpsc::Receiver<SessionEvent>,
        disconnected: CancellationToken,
    ) -> Self {
        Self {
            host,
            events,
            disconnected,
        }
    }

    /// Creates a durable Session using the caller's operation identity.
    pub async fn create_session(
        &self,
        request: SessionCreateCommandRequest,
    ) -> Result<AgentSession, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        run_blocking(move || {
            let id = host.create_session(request)?;
            Ok(AgentSession { id, host })
        })
        .await
    }

    /// Opens an existing durable Session for subsequent operations.
    pub async fn open_session(&self, session_id: &str) -> Result<AgentSession, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = session_id.to_owned();
        run_blocking(move || {
            let session = host.load_session(&session_id)?;
            Ok(AgentSession {
                id: session.id,
                host,
            })
        })
        .await
    }

    /// Loads existing history and reattaches this client's viewer to its current
    /// Run. The host owns execution recovery and the existing Run identity.
    pub async fn resume_session(
        &self,
        session_id: &str,
    ) -> Result<(AgentSession, RunSubscription), RuntimeHostError> {
        let session = self.open_session(session_id).await?;
        let subscription = session.subscribe(None).await?;
        Ok((session, subscription))
    }

    /// Receives the next canonical notification. After closure, reconcile
    /// durable state through a newly connected client.
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        tokio::select! {
            biased;
            _ = self.disconnected.cancelled() => None,
            event = self.events.recv() => event,
        }
    }

    /// Consumes this client's notifications until the Run has a durable outcome.
    pub async fn wait_for_run(&mut self, agent_run_id: &str) -> Result<AgentRun, RuntimeHostError> {
        loop {
            let host = Arc::clone(&self.host);
            let run_id = agent_run_id.to_owned();
            let run = run_blocking(move || host.run(&run_id)).await?;
            if run.status != "running" && run.status != "stalled" {
                return Ok(run);
            }
            self.next_event().await.ok_or_else(|| {
                RuntimeHostError::new(
                    "agent_events_closed",
                    "Event connection closed; reconcile the durable Run before continuing",
                )
            })?;
        }
    }
}

impl Drop for AgentClient {
    fn drop(&mut self) {
        if let Err(error) = self.host.disconnect() {
            eprintln!("Agent client disconnect failed: {error}");
        }
    }
}

/// A Session bound to its creating SDK client's lifetime and identity.
/// Application hosts own configuration, startup and product catalog state.
pub struct AgentSession {
    id: String,
    host: Arc<dyn SessionHost>,
}

impl AgentSession {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn load(&self) -> Result<SessionDataResponse, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        run_blocking(move || host.load_session(&session_id)).await
    }

    /// Returns admission identities. Observe events or wait for the durable Run
    /// outcome separately. Reuse operationId to reconcile an uncertain result.
    pub async fn send(
        &self,
        prompt: SessionPrompt,
    ) -> Result<AgentInputResponse, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        run_blocking(move || host.send(&session_id, prompt)).await
    }

    /// Attaches this client's viewer to the Session or a specific Run.
    pub async fn subscribe(
        &self,
        agent_run_id: Option<&str>,
    ) -> Result<RunSubscription, RuntimeHostError> {
        if agent_run_id.is_some_and(|run_id| run_id.trim().is_empty()) {
            return Err(RuntimeHostError::invalid_request(
                "agentRunId must not be empty",
            ));
        }
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        let agent_run_id = agent_run_id.map(str::to_owned);
        run_blocking(move || host.subscribe(&session_id, agent_run_id.as_deref())).await
    }

    pub async fn cancel(
        &self,
        agent_run_id: &str,
        reason: Option<String>,
    ) -> Result<CancelReceipt, RuntimeHostError> {
        if agent_run_id.trim().is_empty() {
            return Err(RuntimeHostError::invalid_request(
                "agentRunId must not be empty",
            ));
        }
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        let agent_run_id = agent_run_id.to_owned();
        run_blocking(move || host.cancel(&session_id, &agent_run_id, reason)).await
    }

    pub async fn supplement(
        &self,
        agent_run_id: &str,
        message: String,
    ) -> Result<AgentSupplementResponse, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        let agent_run_id = agent_run_id.to_owned();
        run_blocking(move || host.supplement(&session_id, &agent_run_id, message)).await
    }

    pub async fn answer_now(
        &self,
        intervention: AgentRunInterventionV1,
    ) -> Result<AgentAnswerNowResponse, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        run_blocking(move || host.answer_now(&session_id, intervention)).await
    }

    pub async fn answer_question(
        &self,
        answer: QuestionAnswer,
    ) -> Result<AgentInputResponse, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        run_async(async move { host.answer_question(&session_id, answer).await }).await
    }

    pub async fn compact_context(&self) -> Result<AgentContextCompactResponse, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        run_async(async move { host.compact_context(&session_id).await }).await
    }

    pub async fn list_runs(
        &self,
        include_terminal: bool,
    ) -> Result<Vec<AgentRun>, RuntimeHostError> {
        let host = Arc::clone(&self.host);
        let session_id = self.id.clone();
        run_blocking(move || host.list_runs(&session_id, include_terminal)).await
    }
}

async fn run_blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, RuntimeHostError> + Send + 'static,
) -> Result<T, RuntimeHostError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| RuntimeHostError::new("agent_operation_failed", error.to_string()))?
}

async fn run_async<T: Send + 'static>(
    operation: impl std::future::Future<Output = Result<T, RuntimeHostError>> + Send + 'static,
) -> Result<T, RuntimeHostError> {
    tokio::spawn(operation)
        .await
        .map_err(|error| RuntimeHostError::new("agent_operation_failed", error.to_string()))?
}
