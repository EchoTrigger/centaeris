//! Rust Agent SDK with application-owned host integration.

mod errors;
mod host;
mod operation;
mod types;

pub mod local_execution_host;
pub mod openssh;
pub mod session_api;

pub use centaeris_core::runtime::contracts::AgentRunInterventionV1;
pub use errors::RuntimeHostError;
pub use host::SessionHost;
pub use operation::OperationId;
pub use session_api::{AgentClient, AgentSession};
pub use types::{
    AgentAnswerNowResponse, AgentContextCompactResponse, AgentInputResponse, AgentRun,
    AgentSupplementResponse, CancelReceipt, LocalImageInputRequest, PersistedChatMessageResponse,
    QuestionAnswer, RunSubscription, SessionCreateCommandRequest, SessionDataResponse,
    SessionEvent, SessionPrompt,
};
