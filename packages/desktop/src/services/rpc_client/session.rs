//! Session operations over the existing shared-host transport.

use super::{RuntimeClient, RuntimeRequestError, RuntimeResponse};
use crate::commands::RuntimeHostCommand;
use serde_json::{json, Value};

/// Runtime Session operations available to connected SDK and CLI callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionCommand {
    Create,
    Load,
    Send,
    Subscribe,
    Unsubscribe,
    ListRuns,
    LiveSnapshot,
    Cancel,
    Supplement,
    AnswerNow,
    AnswerQuestion,
    CompactContext,
}

impl SessionCommand {
    const fn method(self) -> &'static str {
        let command = match self {
            Self::Create => RuntimeHostCommand::SessionCreate,
            Self::Load => RuntimeHostCommand::SessionGet,
            Self::Send => RuntimeHostCommand::AgentInput,
            Self::Subscribe => RuntimeHostCommand::AgentRunAttach,
            Self::Unsubscribe => RuntimeHostCommand::AgentRunDetach,
            Self::ListRuns => RuntimeHostCommand::AgentRunList,
            Self::LiveSnapshot => RuntimeHostCommand::AgentRunLiveSnapshot,
            Self::Cancel => RuntimeHostCommand::AgentRunCancel,
            Self::Supplement => RuntimeHostCommand::AgentSupplement,
            Self::AnswerNow => RuntimeHostCommand::AgentAnswerNow,
            Self::AnswerQuestion => RuntimeHostCommand::AgentQuestionAnswer,
            Self::CompactContext => RuntimeHostCommand::AgentContextCompact,
        };
        command.command()
    }
}

impl RuntimeClient {
    /// Sends a Session operation using the transport's existing deadline and
    /// outcome classification. The caller retains its operation identity.
    pub fn session_request(
        &mut self,
        command: SessionCommand,
        request: Value,
    ) -> Result<Value, RuntimeRequestError> {
        self.request(command.method(), json!({ "request": request }))
    }

    /// Sends a Session operation whose result can be polled by an interactive
    /// caller through the existing RuntimeResponse lifecycle.
    pub fn session_request_async(
        &mut self,
        command: SessionCommand,
        request: Value,
    ) -> Result<RuntimeResponse, String> {
        self.request_async(command.method(), json!({ "request": request }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn session_operations_preserve_canonical_methods_and_request_identity() {
        let mut client = RuntimeClient::from_test_request_handler(|frame| frame);
        let request = json!({
            "operationId": "session-request-1",
            "sessionId": "session-1",
            "message": "原生会话\ninput",
        });
        for (command, method) in [
            (SessionCommand::Create, "session/new"),
            (SessionCommand::Load, "session/load"),
            (SessionCommand::Send, "session/prompt"),
            (
                SessionCommand::Subscribe,
                "_centaeris/session/agent-runs/attach",
            ),
            (
                SessionCommand::Unsubscribe,
                "_centaeris/session/agent-runs/detach",
            ),
            (SessionCommand::ListRuns, "_centaeris/session/agent-runs"),
            (
                SessionCommand::LiveSnapshot,
                "_centaeris/session/agent-runs/live-snapshot",
            ),
            (
                SessionCommand::Cancel,
                "_centaeris/session/agent-runs/cancel",
            ),
            (SessionCommand::Supplement, "_centaeris/session/supplement"),
            (SessionCommand::AnswerNow, "_centaeris/session/answer_now"),
            (
                SessionCommand::AnswerQuestion,
                "_centaeris/session/answer_question",
            ),
            (
                SessionCommand::CompactContext,
                "_centaeris/session/compact_context",
            ),
        ] {
            let response = client.session_request(command, request.clone()).unwrap();
            assert_eq!(response["method"], method);
            assert_eq!(response["params"], json!({ "request": request }));
        }
        let response = client
            .session_request_async(SessionCommand::Supplement, request.clone())
            .unwrap()
            .recv_timeout("_centaeris/session/supplement", Duration::from_secs(1))
            .unwrap();
        assert_eq!(response["method"], "_centaeris/session/supplement");
        assert_eq!(response["params"], json!({ "request": request }));
    }
}
