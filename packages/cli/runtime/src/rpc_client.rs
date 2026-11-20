//! In-process CLI access to the local Agent runtime.
mod embedded;
mod session;
mod subagents;

pub use crate::sessions::SessionItemResponse;
pub use embedded::run_embedded_rpc_stdio;
pub use session::SessionCommand;
pub use subagents::NativeSubagentResponse;

use centaeris_core::session::store::AgentRuntimeSnapshotStorePort;
use serde_json::Value;
use std::fmt;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RUNTIME_CLOSED: &str = "local runtime is closed";

#[derive(Debug)]
pub enum RuntimeEvent {
    SessionUpdate(Value),
    RuntimeConfigChanged,
    Error(String),
}

pub struct TerminalTokenUsage {
    pub context_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeRequestError {
    code: String,
    message: String,
    outcome_unknown: bool,
}

impl RuntimeRequestError {
    fn domain(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            outcome_unknown: false,
        }
    }

    fn timeout(method: &str) -> Self {
        Self {
            code: "runtime_request_timeout".into(),
            message: format!("runtime request timed out: {method}"),
            outcome_unknown: true,
        }
    }

    fn response_channel_closed() -> Self {
        Self {
            code: "runtime_response_channel_closed".into(),
            message: "runtime response channel closed".into(),
            outcome_unknown: true,
        }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn outcome_unknown(&self) -> bool {
        self.outcome_unknown
    }
}

impl fmt::Display for RuntimeRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.outcome_unknown() {
            write!(
                formatter,
                "{} (code: {}; outcome unknown)",
                self.message(),
                self.code()
            )
        } else {
            write!(formatter, "{} (code: {})", self.message(), self.code())
        }
    }
}

impl std::error::Error for RuntimeRequestError {}

impl From<RuntimeRequestError> for String {
    fn from(error: RuntimeRequestError) -> Self {
        error.to_string()
    }
}

type ResponseSender = mpsc::Sender<Result<Value, RuntimeRequestError>>;

#[cfg(any(test, feature = "test-support"))]
type TestRequests = mpsc::Sender<(Value, ResponseSender)>;

enum Runtime {
    Embedded(embedded::EmbeddedRuntime),
    #[cfg(any(test, feature = "test-support"))]
    Test {
        requests: Option<TestRequests>,
        session_ownership: bool,
    },
}

pub struct RuntimeClient {
    runtime: Runtime,
    event_rx: mpsc::Receiver<RuntimeEvent>,
    connected: bool,
}

pub struct RuntimeResponse {
    response_rx: mpsc::Receiver<Result<Value, RuntimeRequestError>>,
    method: String,
    started_at: Instant,
}

pub type RetainedImageResponse = mpsc::Receiver<Result<Option<Vec<u8>>, String>>;
pub use crate::message_log::transcript_video_paths;

pub type TranscriptToolMediaRefs = Vec<(
    String,
    Vec<centaeris_core::model::prepared_prompt::ModelInputImageSourceRefV1>,
    Vec<String>,
)>;

impl RuntimeClient {
    pub fn transcript_tool_media_refs_async(
        &self,
        session_id: String,
        call_ids: Vec<String>,
    ) -> Result<mpsc::Receiver<Result<TranscriptToolMediaRefs, String>>, String> {
        if !self.connected {
            return Err(RUNTIME_CLOSED.into());
        }
        let (tx, rx) = mpsc::channel();
        match &self.runtime {
            Runtime::Embedded(_) => {
                std::thread::Builder::new()
                    .name("centaeris-transcript-media-refs".into())
                    .spawn(move || {
                        let refs = crate::message_log::read_transcript_tool_media_refs(
                            &session_id,
                            &call_ids,
                        );
                        let _ = tx.send(refs);
                    })
                    .map_err(|error| {
                        format!("start transcript media reference read failed: {error}")
                    })?;
            }
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { .. } => {
                let _ = tx.send(Ok(Vec::new()));
            }
        }
        Ok(rx)
    }

    /// Reads an image retained by a local tool without blocking the terminal thread.
    pub fn retained_execution_image_async(
        &self,
        session_id: String,
        digest: String,
    ) -> Result<RetainedImageResponse, String> {
        if !self.connected {
            return Err(RUNTIME_CLOSED.into());
        }
        let (tx, rx) = mpsc::channel();
        match &self.runtime {
            Runtime::Embedded(_) => {
                std::thread::Builder::new()
                    .name("centaeris-retained-image".into())
                    .spawn(move || {
                        let image = crate::agent_runtime::agent_runtime_store_actor()
                            .and_then(|actor| actor.load_model_input_image(&session_id, &digest));
                        let _ = tx.send(image);
                    })
                    .map_err(|error| format!("start retained image read failed: {error}"))?;
            }
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { .. } => {
                let _ = tx.send(Ok(None));
            }
        }
        Ok(rx)
    }

    pub fn terminal_token_usage(
        &self,
        session_id: &str,
        agent_run_id: Option<&str>,
        include_context: bool,
    ) -> Option<mpsc::Receiver<Result<TerminalTokenUsage, String>>> {
        match &self.runtime {
            Runtime::Embedded(runtime) => {
                runtime.terminal_token_usage(session_id, agent_run_id, include_context)
            }
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { .. } => None,
        }
    }

    /// Starts the runtime and its event stream inside the calling CLI process.
    pub fn start_embedded(viewer_id: &str) -> Result<Self, String> {
        let (event_tx, event_rx) = mpsc::channel();
        let runtime = Runtime::Embedded(embedded::EmbeddedRuntime::start(viewer_id, event_tx)?);
        Ok(Self {
            runtime,
            event_rx,
            connected: true,
        })
    }

    pub fn shutdown_embedded(&mut self) -> Result<(), String> {
        self.connected = false;
        match &mut self.runtime {
            Runtime::Embedded(runtime) => runtime.shutdown(),
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { .. } => Ok(()),
        }
    }

    pub fn claim_session_writer(&mut self, session_id: &str) -> Result<bool, String> {
        match &self.runtime {
            Runtime::Embedded(runtime) => runtime.claim_session_writer(session_id),
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test {
                session_ownership: false,
                ..
            } => Ok(true),
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test {
                session_ownership: true,
                ..
            } => {
                let response = self.request(
                    "_centaeris/session/claim-writer",
                    serde_json::json!({"request": {"sessionId": session_id}}),
                )?;
                response["claimed"]
                    .as_bool()
                    .ok_or_else(|| "Claim response missing claimed".to_string())
            }
        }
    }

    pub fn release_session_writer(&mut self, session_id: &str) -> Result<(), String> {
        match &self.runtime {
            Runtime::Embedded(runtime) => runtime.release_session_writer(session_id),
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test {
                session_ownership: false,
                ..
            } => Ok(()),
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test {
                session_ownership: true,
                ..
            } => {
                self.request(
                    "_centaeris/session/release-writer",
                    serde_json::json!({"request": {"sessionId": session_id}}),
                )?;
                Ok(())
            }
        }
    }

    pub fn fork_session_before_user_turn(&mut self, session_id: &str) -> Result<Value, String> {
        match &self.runtime {
            Runtime::Embedded(runtime) => runtime.fork_session_before_user_turn(session_id),
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { .. } => self
                .request(
                    "_centaeris/session/fork-before-user-turn",
                    serde_json::json!({"request": {"sessionId": session_id}}),
                )
                .map_err(Into::into),
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, RuntimeRequestError> {
        self.request_async(method, params)
            .map_err(|message| RuntimeRequestError::domain("runtime_request_not_sent", message))?
            .recv_timeout(method, REQUEST_TIMEOUT)
    }

    pub fn request_async(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<RuntimeResponse, String> {
        if !self.connected {
            return Err(RUNTIME_CLOSED.to_string());
        }
        let (tx, response_rx) = mpsc::channel();
        match &self.runtime {
            Runtime::Embedded(runtime) => runtime.request(method, params, tx)?,
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test {
                requests: Some(request_tx),
                ..
            } => request_tx
                .send((
                    serde_json::json!({ "method": method, "params": params }),
                    tx,
                ))
                .map_err(|error| error.to_string())?,
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { requests: None, .. } => {
                return Err("test client has no request handler".to_string())
            }
        }
        Ok(RuntimeResponse {
            response_rx,
            method: method.to_owned(),
            started_at: Instant::now(),
        })
    }

    pub fn try_recv_event(&self) -> Option<RuntimeEvent> {
        match self.event_rx.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(RuntimeEvent::Error(RUNTIME_CLOSED.into()))
            }
        }
    }

    pub fn recv_event_timeout(&self, timeout: Duration) -> Result<Option<RuntimeEvent>, String> {
        match self.event_rx.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(RUNTIME_CLOSED.into()),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_test_request_handler(handler: impl Fn(Value) -> Value + Send + 'static) -> Self {
        Self::from_test_handler(handler, false)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_test_request_handler_with_session_ownership(
        handler: impl Fn(Value) -> Value + Send + 'static,
    ) -> Self {
        Self::from_test_handler(handler, true)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn from_test_handler(
        handler: impl Fn(Value) -> Value + Send + 'static,
        session_ownership: bool,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let (request_tx, request_rx) = mpsc::channel::<(Value, ResponseSender)>();
        std::thread::spawn(move || {
            let _event_tx = event_tx;
            for (request, response) in request_rx {
                let _ = response.send(Ok(handler(request)));
            }
        });
        Self {
            runtime: Runtime::Test {
                requests: Some(request_tx),
                session_ownership,
            },
            event_rx,
            connected: true,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_test_event_receiver(
        event_rx: mpsc::Receiver<RuntimeEvent>,
        connected: bool,
    ) -> Self {
        Self {
            runtime: Runtime::Test {
                requests: None,
                session_ownership: false,
            },
            event_rx,
            connected,
        }
    }
}

impl RuntimeResponse {
    pub fn try_recv(&self) -> Result<Option<Value>, RuntimeRequestError> {
        match self.response_rx.try_recv() {
            Ok(response) => response.map(Some),
            Err(mpsc::TryRecvError::Empty) if self.started_at.elapsed() < REQUEST_TIMEOUT => {
                Ok(None)
            }
            Err(mpsc::TryRecvError::Empty) => Err(RuntimeRequestError::timeout(&self.method)),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(RuntimeRequestError::response_channel_closed())
            }
        }
    }

    fn recv_timeout(&self, method: &str, timeout: Duration) -> Result<Value, RuntimeRequestError> {
        match self.response_rx.recv_timeout(timeout) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RuntimeRequestError::timeout(method)),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(RuntimeRequestError::response_channel_closed())
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_response_receiver(
        response_rx: mpsc::Receiver<Result<Value, RuntimeRequestError>>,
    ) -> Self {
        Self {
            response_rx,
            method: "test".to_string(),
            started_at: Instant::now(),
        }
    }
}

fn runtime_notification_event(method: &str, params: Value) -> Result<RuntimeEvent, String> {
    match method {
        "session/update" => Ok(RuntimeEvent::SessionUpdate(params)),
        "runtime/config-changed" if params.as_object().is_some_and(serde_json::Map::is_empty) => {
            Ok(RuntimeEvent::RuntimeConfigChanged)
        }
        "runtime/config-changed" => {
            Err("runtime/config-changed params must be an empty object".to_string())
        }
        other => Err(format!("unsupported runtime notification: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn runtime_config_changed_requires_exact_empty_params() {
        assert!(matches!(
            runtime_notification_event("runtime/config-changed", json!({})),
            Ok(RuntimeEvent::RuntimeConfigChanged)
        ));
        assert!(
            runtime_notification_event("runtime/config-changed", json!({"extra": true})).is_err()
        );
        assert!(runtime_notification_event("runtime/config-changed", Value::Null).is_err());
    }
}
