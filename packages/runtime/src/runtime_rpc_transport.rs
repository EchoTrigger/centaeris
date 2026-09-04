//! JSON-RPC framing shared by every Runtime Server local connection.

use crate::errors::RuntimeHostError;
use crate::runtime_rpc::{encode_jsonl_value, RuntimeRpcNotification, RuntimeRpcResponse};
use crate::runtime_server::{
    ActiveAgentRun, AgentRunLease, AgentRunRegistry, OwnerExitDisposition, RuntimeClientKind,
    StartAgentRunError,
};
use centaeris_core::runtime::TurnControl;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeClientRegistration {
    pub(crate) kind: RuntimeClientKind,
    pub(crate) viewer_id: String,
}

struct ConnectedClient {
    outbound: mpsc::UnboundedSender<String>,
    registration: Option<RuntimeClientRegistration>,
    exiting: bool,
}

#[derive(Default)]
pub(crate) struct RuntimeServerClientHub {
    clients: Mutex<HashMap<u64, ConnectedClient>>,
    next_client_id: AtomicU64,
    activity_generation: AtomicU64,
    agent_runs: AgentRunRegistry,
    draining: AtomicBool,
}

impl RuntimeServerClientHub {
    pub(crate) fn connect(
        self: &Arc<Self>,
    ) -> Result<Option<(EventWriter, mpsc::UnboundedReceiver<String>)>, RuntimeHostError> {
        let client_id = self.next_client_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (outbound, receiver) = mpsc::unbounded_channel();
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        if self.draining.load(Ordering::Acquire) {
            return Ok(None);
        }
        clients.insert(
            client_id,
            ConnectedClient {
                outbound,
                registration: None,
                exiting: false,
            },
        );
        self.activity_generation.fetch_add(1, Ordering::Release);
        Ok(Some((
            EventWriter::new(Arc::clone(self), client_id),
            receiver,
        )))
    }

    pub(crate) fn broadcaster(self: &Arc<Self>) -> EventWriter {
        EventWriter::new(Arc::clone(self), 0)
    }

    pub(crate) fn disconnect(
        &self,
        event_writer: &EventWriter,
    ) -> Result<Vec<OwnerExitDisposition>, RuntimeHostError> {
        let client = self.remove_client(event_writer.client_id)?;
        if client.is_some() {
            self.activity_generation.fetch_add(1, Ordering::Release);
        }
        if let Some(registration) = client.and_then(|client| client.registration) {
            detach_viewer(registration.viewer_id.as_str())?;
        }
        self.owner_exited(event_writer.client_id)
    }

    pub(crate) fn has_clients_or_active_agent_runs(&self) -> Result<bool, RuntimeHostError> {
        let has_clients = !self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?
            .is_empty();
        let active_agent_run_count = self
            .agent_runs
            .active_agent_run_count()
            .map_err(RuntimeHostError::transport)?;
        Ok(has_clients || active_agent_run_count > 0)
    }

    pub(crate) fn activity_generation(&self) -> u64 {
        self.activity_generation.load(Ordering::Acquire)
    }

    pub(crate) fn begin_idle_shutdown(
        &self,
        expected_activity_generation: u64,
    ) -> Result<bool, RuntimeHostError> {
        let clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        if self.activity_generation() != expected_activity_generation
            || !clients.is_empty()
            || self
                .agent_runs
                .active_agent_run_count()
                .map_err(RuntimeHostError::transport)?
                > 0
        {
            return Ok(false);
        }
        self.draining.store(true, Ordering::Release);
        Ok(true)
    }

    fn broadcast<TValue: serde::Serialize>(&self, value: &TValue) -> Result<(), RuntimeHostError> {
        let encoded = encode(value)?;
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        clients.retain(|_, client| {
            client.registration.is_none()
                || client.exiting
                || client.outbound.send(encoded.clone()).is_ok()
        });
        Ok(())
    }

    fn send<TValue: serde::Serialize>(
        &self,
        client_id: u64,
        value: &TValue,
    ) -> Result<(), RuntimeHostError> {
        let encoded = encode(value)?;
        let sender = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?
            .get(&client_id)
            .map(|client| client.outbound.clone())
            .ok_or_else(|| RuntimeHostError::transport("runtime request owner is disconnected"))?;
        sender
            .send(encoded)
            .map_err(|_| RuntimeHostError::transport("runtime request owner write channel closed"))
    }

    fn remove_client(&self, client_id: u64) -> Result<Option<ConnectedClient>, RuntimeHostError> {
        self.clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))
            .map(|mut clients| clients.remove(&client_id))
    }

    fn register_client(
        &self,
        client_id: u64,
        kind: RuntimeClientKind,
        viewer_id: &str,
    ) -> Result<(), RuntimeHostError> {
        let viewer_id = required_identifier(viewer_id, "viewerId")?;
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        if clients.iter().any(|(candidate_id, client)| {
            *candidate_id != client_id
                && client
                    .registration
                    .as_ref()
                    .is_some_and(|registration| registration.viewer_id == viewer_id)
        }) {
            return Err(RuntimeHostError::invalid_request(format!(
                "runtime viewerId is already connected: {viewer_id}"
            )));
        }
        let client = clients.get_mut(&client_id).ok_or_else(|| {
            RuntimeHostError::transport("runtime client disconnected before initialize")
        })?;
        let registration = RuntimeClientRegistration { kind, viewer_id };
        match client.registration.as_ref() {
            Some(existing) if existing == &registration && !client.exiting => Ok(()),
            Some(_) => Err(RuntimeHostError::invalid_request(
                "runtime client registration cannot change",
            )),
            None if client.exiting => Err(RuntimeHostError::invalid_request(
                "exiting runtime client cannot initialize",
            )),
            None => {
                client.registration = Some(registration);
                Ok(())
            }
        }
    }

    fn registration(&self, client_id: u64) -> Result<RuntimeClientRegistration, RuntimeHostError> {
        let clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        let client = clients.get(&client_id).ok_or_else(|| {
            RuntimeHostError::transport("runtime client disconnected before request")
        })?;
        if client.exiting {
            return Err(RuntimeHostError::invalid_request(
                "runtime client is exiting",
            ));
        }
        client
            .registration
            .clone()
            .ok_or_else(|| RuntimeHostError::invalid_request("runtime client is not initialized"))
    }

    fn mark_exiting(&self, client_id: u64) -> Result<(), RuntimeHostError> {
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        let client = clients.get_mut(&client_id).ok_or_else(|| {
            RuntimeHostError::transport("runtime client disconnected before owner exit")
        })?;
        client.exiting = true;
        Ok(())
    }

    fn desktop_owner_ids(&self) -> Result<Vec<String>, RuntimeHostError> {
        let mut owner_ids = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?
            .iter()
            .filter(|&(_, client)| {
                !client.exiting
                    && client
                        .registration
                        .as_ref()
                        .is_some_and(|registration| registration.kind == RuntimeClientKind::Desktop)
            })
            .map(|(client_id, _)| owner_id(*client_id))
            .collect::<Vec<_>>();
        owner_ids.sort();
        Ok(owner_ids)
    }

    fn owner_exited(&self, client_id: u64) -> Result<Vec<OwnerExitDisposition>, RuntimeHostError> {
        let dispositions = self
            .agent_runs
            .owner_exited(
                owner_id(client_id).as_str(),
                self.desktop_owner_ids()?.as_slice(),
            )
            .map_err(RuntimeHostError::transport)?;
        Ok(dispositions)
    }
}

#[derive(Clone)]
pub(crate) struct EventWriter {
    hub: Arc<RuntimeServerClientHub>,
    client_id: u64,
}

impl EventWriter {
    fn new(hub: Arc<RuntimeServerClientHub>, client_id: u64) -> Self {
        Self { hub, client_id }
    }

    pub(crate) fn for_agent_run(&self, lease_id: &str) -> Result<Self, String> {
        let lease_id =
            required_identifier(lease_id, "leaseId").map_err(|error| error.to_string())?;
        self.hub.agent_runs.owner_id_for_lease(lease_id.as_str())?;
        Ok(Self {
            hub: Arc::clone(&self.hub),
            client_id: self.client_id,
        })
    }

    pub(crate) fn emit(
        &self,
        event_name: impl Into<String>,
        payload: serde_json::Value,
    ) -> Result<(), RuntimeHostError> {
        self.hub
            .broadcast(&RuntimeRpcNotification::new(event_name, payload))
    }

    pub(crate) fn start_agent_run(
        &self,
        session_id: &str,
        agent_run_id: &str,
        turn_id: &str,
        control: TurnControl,
    ) -> Result<AgentRunLease, String> {
        let lease = self
            .hub
            .agent_runs
            .start(
                session_id,
                agent_run_id,
                turn_id,
                self.owner_id().as_str(),
                self.registration().map_err(|error| error.to_string())?.kind,
                control,
            )
            .map_err(format_start_agent_run_error)?;
        self.hub.activity_generation.fetch_add(1, Ordering::Release);
        Ok(lease)
    }

    pub(crate) fn finish_agent_run(&self, lease_id: &str) -> Result<(), String> {
        let result = self.hub.agent_runs.finish(lease_id).map(|_| ());
        self.hub.activity_generation.fetch_add(1, Ordering::Release);
        result
    }

    pub(crate) fn active_agent_run(
        &self,
        agent_run_id: &str,
    ) -> Result<Option<ActiveAgentRun>, String> {
        self.hub.agent_runs.active(agent_run_id)
    }

    pub(crate) fn active_agent_run_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<ActiveAgentRun>, String> {
        self.hub.agent_runs.active_for_session(session_id)
    }

    pub(crate) fn agent_run_cancellation_reason(
        &self,
        agent_run_id: &str,
    ) -> Result<Option<String>, String> {
        self.active_agent_run(agent_run_id)?
            .map(|active| active.cancellation_reason())
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn with_session_deletion<TResult>(
        &self,
        session_ids: &[String],
        delete: impl FnOnce() -> Result<TResult, String>,
    ) -> Result<TResult, String> {
        self.hub
            .agent_runs
            .with_session_deletion(session_ids, delete)
    }

    pub(crate) fn wait_until_sessions_inactive(
        &self,
        session_ids: &[String],
    ) -> Result<(), String> {
        self.hub
            .agent_runs
            .wait_until_sessions_inactive(session_ids)
    }

    pub(crate) fn owner_exited(&self) -> Result<Vec<OwnerExitDisposition>, String> {
        self.hub
            .mark_exiting(self.client_id)
            .map_err(|error| error.to_string())?;
        self.hub
            .owner_exited(self.client_id)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn owner_id(&self) -> String {
        owner_id(self.client_id)
    }

    pub(crate) fn send_response(
        &self,
        response: &RuntimeRpcResponse,
    ) -> Result<(), RuntimeHostError> {
        self.hub.send(self.client_id, response)
    }

    pub(crate) fn register_client(
        &self,
        kind: RuntimeClientKind,
        viewer_id: &str,
    ) -> Result<(), RuntimeHostError> {
        self.hub.register_client(self.client_id, kind, viewer_id)
    }

    pub(crate) fn registration(&self) -> Result<RuntimeClientRegistration, RuntimeHostError> {
        self.hub.registration(self.client_id)
    }

    pub(crate) fn require_registered(&self) -> Result<(), RuntimeHostError> {
        self.registration().map(|_| ())
    }

    pub(crate) fn require_viewer_id(&self, viewer_id: &str) -> Result<(), RuntimeHostError> {
        let viewer_id = required_identifier(viewer_id, "viewerId")?;
        if self.registration()?.viewer_id != viewer_id {
            return Err(RuntimeHostError::invalid_request(
                "viewerId does not match the initialized runtime client",
            ));
        }
        Ok(())
    }

    pub(crate) fn detach_registered_viewer(&self) -> Result<(), RuntimeHostError> {
        detach_viewer(self.registration()?.viewer_id.as_str())
    }
}

fn owner_id(client_id: u64) -> String {
    format!("runtime-client-{client_id}")
}

fn required_identifier(value: &str, field: &str) -> Result<String, RuntimeHostError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(RuntimeHostError::invalid_request(format!(
            "{field} must not be empty"
        )));
    }
    Ok(value.to_string())
}

fn detach_viewer(viewer_id: &str) -> Result<(), RuntimeHostError> {
    crate::agent_runs::detach_viewer(crate::agent_runs::AgentRunDetachViewerRequest {
        viewer_id: viewer_id.to_string(),
    })
    .map(|_| ())
    .map_err(|error| RuntimeHostError::new("agent_task_failed", error))
}

fn encode<TValue: serde::Serialize>(value: &TValue) -> Result<String, RuntimeHostError> {
    encode_jsonl_value(value).map_err(|error| {
        RuntimeHostError::transport(format!("runtime response encode failed: {error}"))
    })
}

fn format_start_agent_run_error(error: StartAgentRunError) -> String {
    match error {
        StartAgentRunError::Invalid(message) => {
            format!("invalid runtime AgentRun lease: {message}")
        }
        StartAgentRunError::SessionBusy(busy) => format!(
            "session already has an active AgentRun: agentRunId={} ownerId={}",
            busy.active_agent_run_id, busy.origin_owner_id
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_rpc::{decode_jsonl_frame, RuntimeRpcFrame};
    use serde_json::json;

    fn connect(
        hub: &Arc<RuntimeServerClientHub>,
    ) -> (EventWriter, mpsc::UnboundedReceiver<String>) {
        hub.connect()
            .expect("connect")
            .expect("runtime server accepts client")
    }

    fn start_agent_run(
        writer: &EventWriter,
        session_id: &str,
        agent_run_id: &str,
    ) -> AgentRunLease {
        writer
            .start_agent_run(
                session_id,
                agent_run_id,
                format!("turn:{agent_run_id}").as_str(),
                TurnControl::new(),
            )
            .expect("start AgentRun")
    }

    #[tokio::test]
    async fn session_updates_broadcast_only_to_initialized_clients() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (writer, mut first) = connect(&hub);
        let (second_writer, mut second) = connect(&hub);
        let (uninitialized_writer, mut uninitialized) = connect(&hub);
        writer
            .register_client(RuntimeClientKind::Desktop, "desktop-first")
            .expect("register first client");
        second_writer
            .register_client(RuntimeClientKind::Tui, "tui-second")
            .expect("register second client");
        writer
            .emit(
                "session/update",
                json!({"sessionId": "session-1", "agentRunId": "agent-run-1", "payload": {}}),
            )
            .expect("emit update");
        assert!(matches!(
            decode_jsonl_frame(first.recv().await.expect("first event").as_str()),
            Ok(RuntimeRpcFrame::Notification(_))
        ));
        assert!(matches!(
            decode_jsonl_frame(second.recv().await.expect("second event").as_str()),
            Ok(RuntimeRpcFrame::Notification(_))
        ));
        assert!(matches!(
            uninitialized.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(uninitialized_writer);
    }

    #[tokio::test]
    async fn runtime_config_changes_broadcast_exact_empty_params_to_every_client() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (writer, mut first) = connect(&hub);
        let (second_writer, mut second) = connect(&hub);
        writer
            .register_client(RuntimeClientKind::Desktop, "desktop-config")
            .expect("register first client");
        second_writer
            .register_client(RuntimeClientKind::Tui, "tui-config")
            .expect("register second client");
        writer
            .emit("runtime/config-changed", json!({}))
            .expect("emit config change");
        for receiver in [&mut first, &mut second] {
            let RuntimeRpcFrame::Notification(notification) =
                decode_jsonl_frame(receiver.recv().await.expect("config event").as_str())
                    .expect("decode config event")
            else {
                panic!("expected config notification");
            };
            assert_eq!(notification.method, "runtime/config-changed");
            assert_eq!(notification.params, json!({}));
        }
    }

    #[test]
    fn disconnect_only_returns_the_disconnected_client_agent_run_leases() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (first, _first_events) = connect(&hub);
        let (second, _second_events) = connect(&hub);
        first
            .register_client(RuntimeClientKind::Desktop, "desktop-first")
            .expect("register first");
        second
            .register_client(RuntimeClientKind::Tui, "tui-second")
            .expect("register second");
        start_agent_run(&first, "session-first", "agent-run-first");
        start_agent_run(&second, "session-second", "agent-run-second");

        let dispositions = hub.disconnect(&first).expect("disconnect first");
        assert!(matches!(
            dispositions.as_slice(),
            [OwnerExitDisposition::Interrupt(lease)] if lease.agent_run_id == "agent-run-first"
        ));
        start_agent_run(&second, "session-third", "agent-run-third");
    }

    #[test]
    fn client_registration_is_required_unique_and_immutable() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (first, _first_events) = connect(&hub);
        let (second, _second_events) = connect(&hub);
        assert!(first.require_registered().is_err());
        first
            .register_client(RuntimeClientKind::Desktop, "desktop-main")
            .expect("register first client");
        first
            .register_client(RuntimeClientKind::Desktop, "desktop-main")
            .expect("same registration is idempotent");
        assert!(first
            .register_client(RuntimeClientKind::Tui, "desktop-main")
            .is_err());
        assert!(second
            .register_client(RuntimeClientKind::Tui, "desktop-main")
            .is_err());
    }

    #[test]
    fn tui_owner_exit_transfers_same_lease_to_desktop() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (tui, _tui_events) = connect(&hub);
        let (desktop, _desktop_events) = connect(&hub);
        tui.register_client(RuntimeClientKind::Tui, "tui-owner")
            .expect("register TUI");
        desktop
            .register_client(RuntimeClientKind::Desktop, "desktop-observer")
            .expect("register Desktop");
        let lease = start_agent_run(&tui, "session-transfer", "agent-run-transfer");

        let dispositions = tui.owner_exited().expect("TUI owner exit");
        assert!(matches!(
            dispositions.as_slice(),
            [OwnerExitDisposition::Transfer(transferred)]
                if transferred.lease_id == lease.lease_id
                    && transferred.agent_run_id == lease.agent_run_id
                    && transferred.owner_id == desktop.owner_id()
        ));
    }
}
