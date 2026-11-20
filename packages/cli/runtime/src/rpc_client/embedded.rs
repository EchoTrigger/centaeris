use super::{
    runtime_notification_event, ResponseSender, RuntimeEvent, RuntimeRequestError,
    TerminalTokenUsage,
};
use crate::app_service::LocalAppService;
use crate::local_host::LocalAgentHost;
use crate::protocol::HostCommandRequest;
use crate::runtime_rpc_transport::EventWriter;
use crate::runtime_server::RuntimeClientKind;
use serde_json::Value;
use std::sync::mpsc;
use std::time::Duration;
use tokio::runtime::Runtime;

pub(super) struct EmbeddedRuntime {
    pub(super) owner: Option<(Runtime, LocalAgentHost)>,
    pub(super) writer: EventWriter,
}

impl EmbeddedRuntime {
    pub(super) fn terminal_token_usage(
        &self,
        session_id: &str,
        agent_run_id: Option<&str>,
        include_context: bool,
    ) -> Option<mpsc::Receiver<Result<TerminalTokenUsage, String>>> {
        let (executor, _) = self.owner.as_ref()?;
        let session_id = session_id.to_owned();
        let agent_run_id = agent_run_id.map(str::to_owned);
        let (tx, rx) = mpsc::channel();
        executor.spawn_blocking(move || {
            let snapshot = (|| {
                let context_tokens = if include_context {
                    crate::message_log::project_agent_context_state(&session_id)?
                        .context_token_estimate
                } else {
                    None
                };
                let output_tokens = if let Some(agent_run_id) = agent_run_id {
                    crate::message_log::agent_run_session_state(&session_id, &agent_run_id)?
                        .reported_output_tokens()
                } else {
                    None
                };
                Ok(TerminalTokenUsage {
                    context_tokens,
                    output_tokens,
                })
            })();
            let _ = tx.send(snapshot);
        });
        Some(rx)
    }

    pub(super) fn start(
        viewer_id: &str,
        event_tx: mpsc::Sender<RuntimeEvent>,
    ) -> Result<Self, String> {
        let executor = executor()?;
        let host = executor
            .block_on(LocalAgentHost::start_for_cli())
            .map_err(|error| error.to_string())?;
        let (writer, mut events) = host
            .app()
            .clients()
            .connect_native()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Local runtime is shutting down".to_string())?;
        writer
            .register_client(RuntimeClientKind::Tui, viewer_id)
            .map_err(|error| error.to_string())?;
        executor.spawn(async move {
            while let Some(event) = events.recv().await {
                let event = runtime_notification_event(&event.method, event.params)
                    .unwrap_or_else(RuntimeEvent::Error);
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            owner: Some((executor, host)),
            writer,
        })
    }

    pub(super) fn request(
        &self,
        method: &str,
        payload: Value,
        response: ResponseSender,
    ) -> Result<(), String> {
        let (executor, host) = self
            .owner
            .as_ref()
            .ok_or_else(|| "Local runtime is closed".to_string())?;
        let writer = self.writer.clone();
        let app = host.app().clone();
        let request = HostCommandRequest {
            command: method.to_owned(),
            payload,
        };
        executor.spawn(async move {
            let result = app
                .request(writer, request)
                .await
                .map_err(|error| RuntimeRequestError::domain(error.code(), error.message()));
            let _ = response.send(result);
        });
        Ok(())
    }

    pub(super) fn claim_session_writer(&self, session_id: &str) -> Result<bool, String> {
        let (executor, _) = self.owner.as_ref().ok_or("Local runtime is closed")?;
        executor.block_on(async {
            let was_owned = self.writer.owns_session(session_id)?;
            let claimed = self.writer.claim_session_writer(session_id)?;
            if claimed && !was_owned {
                if let Err(error) =
                    crate::subagent_scheduler::reconcile_stopped_jobs_for_session(session_id).await
                {
                    self.writer.release_session_writer(session_id)?;
                    return Err(error);
                }
            }
            Ok(claimed)
        })
    }

    pub(super) fn release_session_writer(&self, session_id: &str) -> Result<(), String> {
        self.writer.release_session_writer(session_id)
    }

    pub(super) fn fork_session_before_user_turn(&self, session_id: &str) -> Result<Value, String> {
        crate::message_log::fork::fork_before_user_turn(session_id)
    }

    pub(super) fn shutdown(&mut self) -> Result<(), String> {
        let Some((executor, host)) = self.owner.take() else {
            return Ok(());
        };
        let result = executor
            .block_on(host.shutdown())
            .map_err(|error| error.to_string());
        executor.shutdown_timeout(Duration::from_millis(100));
        result
    }
}

impl Drop for EmbeddedRuntime {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("Centaeris local runtime shutdown failed: {error}");
        }
    }
}

fn executor() -> Result<Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("centaeris-cli-runtime")
        .build()
        .map_err(|error| format!("start CLI runtime executor failed: {error}"))
}

/// Serves the public JSONL protocol over this process's standard input and output.
pub fn run_embedded_rpc_stdio() -> Result<(), String> {
    let executor = executor()?;
    let result = executor.block_on(LocalAppService::run_exclusive(|app| async move {
        let Some(connection) = app.accept_rpc()? else {
            return Ok(());
        };
        let stream = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
        tokio::select! {
            _ = connection.serve(stream) => {},
            _ = app.shutdown_requested() => {},
        }
        Ok(())
    }));
    executor.shutdown_timeout(Duration::from_millis(100));
    result.map_err(|error| error.to_string())
}
