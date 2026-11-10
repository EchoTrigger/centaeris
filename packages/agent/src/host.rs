//! Host integration for the native Session API.

use crate::{
    AgentAnswerNowResponse, AgentContextCompactResponse, AgentInputResponse, AgentRun,
    AgentRunInterventionV1, AgentSupplementResponse, CancelReceipt, QuestionAnswer,
    RunSubscription, RuntimeHostError, SessionCreateCommandRequest, SessionDataResponse,
    SessionPrompt,
};
use futures::future::BoxFuture;

/// Application-owned admission, persistence and execution assembly for Sessions.
/// Implementations delegate runtime decisions to Core and retain their own
/// request admission, viewer identity and accepted-work lifecycle.
/// Operations apply to the supplied Session. A known Run owned by another
/// Session is rejected, while missing Runs retain the host's receipt semantics.
pub trait SessionHost: Send + Sync {
    fn create_session(
        &self,
        request: SessionCreateCommandRequest,
    ) -> Result<String, RuntimeHostError>;

    fn load_session(&self, session_id: &str) -> Result<SessionDataResponse, RuntimeHostError>;

    fn send(
        &self,
        session_id: &str,
        prompt: SessionPrompt,
    ) -> Result<AgentInputResponse, RuntimeHostError>;

    fn subscribe(
        &self,
        session_id: &str,
        run_id: Option<&str>,
    ) -> Result<RunSubscription, RuntimeHostError>;

    fn cancel(
        &self,
        session_id: &str,
        run_id: &str,
        reason: Option<String>,
    ) -> Result<CancelReceipt, RuntimeHostError>;

    fn supplement(
        &self,
        session_id: &str,
        run_id: &str,
        message: String,
    ) -> Result<AgentSupplementResponse, RuntimeHostError>;

    fn answer_now(
        &self,
        session_id: &str,
        intervention: AgentRunInterventionV1,
    ) -> Result<AgentAnswerNowResponse, RuntimeHostError>;

    fn answer_question<'a>(
        &'a self,
        session_id: &'a str,
        answer: QuestionAnswer,
    ) -> BoxFuture<'a, Result<AgentInputResponse, RuntimeHostError>>;

    fn compact_context<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<AgentContextCompactResponse, RuntimeHostError>>;

    fn list_runs(
        &self,
        session_id: &str,
        include_terminal: bool,
    ) -> Result<Vec<AgentRun>, RuntimeHostError>;

    fn run(&self, run_id: &str) -> Result<AgentRun, RuntimeHostError>;

    /// Detaches this client's viewer while accepted Runs remain host-owned.
    fn disconnect(&self) -> Result<(), RuntimeHostError>;
}
