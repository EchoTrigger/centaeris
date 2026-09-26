//! JSON-RPC framing shared by every Runtime Server local connection.

use crate::errors::RuntimeHostError;
use crate::runtime_rpc::{encode_jsonl_value, RuntimeRpcNotification, RuntimeRpcResponse};
use crate::runtime_server::{
    ActiveAgentRun, AgentRunLease, AgentRunRegistry, RuntimeClientKind, StartAgentRunError,
};
use centaeris_core::runtime::TurnControl;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Notify};

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
    active_actions: AtomicU64,
    agent_runs: AgentRunRegistry,
    draining: AtomicBool,
    service_shutdown: Notify,
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

    pub(crate) fn disconnect(&self, event_writer: &EventWriter) -> Result<(), RuntimeHostError> {
        self.disconnect_with_detach(event_writer, detach_viewer)
    }

    fn disconnect_with_detach(
        &self,
        event_writer: &EventWriter,
        detach: impl FnOnce(&str) -> Result<(), RuntimeHostError>,
    ) -> Result<(), RuntimeHostError> {
        // Keep the viewer identity reserved until its old binding is removed.
        // Reconnecting with the same viewerId cannot race this cleanup.
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        let Some(client) = clients.get_mut(&event_writer.client_id) else {
            return Ok(());
        };
        client.exiting = true;
        if let Some(registration) = &client.registration {
            detach(registration.viewer_id.as_str())?;
        }
        clients.remove(&event_writer.client_id);
        self.activity_generation.fetch_add(1, Ordering::Release);
        Ok(())
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
        Ok(has_clients || active_agent_run_count > 0 || self.active_action_count() > 0)
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
            || self.active_action_count() > 0
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
        for client in clients.values_mut() {
            if client.registration.is_some()
                && !client.exiting
                && client.outbound.send(encoded.clone()).is_err()
            {
                // The connection task owns removal and viewer cleanup. Retain
                // the registration even when its write channel has closed.
                client.exiting = true;
            }
        }
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

    fn client_exited(&self, client_id: u64) -> Result<(), RuntimeHostError> {
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        let client = clients.get_mut(&client_id).ok_or_else(|| {
            RuntimeHostError::transport("runtime client disconnected before client exit")
        })?;
        let registration = client
            .registration
            .as_ref()
            .filter(|_| !client.exiting)
            .ok_or_else(|| {
                RuntimeHostError::invalid_request("runtime client is not initialized or is exiting")
            })?;
        client.exiting = true;
        detach_viewer(&registration.viewer_id)?;
        Ok(())
    }

    fn with_registered_viewer<T>(
        &self,
        client_id: u64,
        viewer_id: &str,
        operation: impl FnOnce() -> Result<T, RuntimeHostError>,
    ) -> Result<T, RuntimeHostError> {
        let viewer_id = required_identifier(viewer_id, "viewerId")?;
        let clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        let registration = clients
            .get(&client_id)
            .filter(|client| !client.exiting)
            .and_then(|client| client.registration.as_ref())
            .ok_or_else(|| {
                RuntimeHostError::invalid_request("runtime client is not initialized or is exiting")
            })?;
        if registration.viewer_id != viewer_id {
            return Err(RuntimeHostError::invalid_request(
                "viewerId does not match the initialized runtime client",
            ));
        }
        // Viewer mutations and cleanup share the clients -> viewer-registry
        // lock order. An old in-flight attach cannot outlive its registration.
        operation()
    }

    fn start_agent_run(
        &self,
        client_id: u64,
        session_id: &str,
        agent_run_id: &str,
        turn_id: &str,
        control: TurnControl,
    ) -> Result<AgentRunLease, String> {
        // Admission and draining share this lock: a client disappearing or the
        // service closing admission cannot race an unregistered execution.
        let clients = self
            .clients
            .lock()
            .map_err(|_| "runtime server client hub lock poisoned".to_string())?;
        if self.draining.load(Ordering::Acquire) {
            return Err("runtime_server_draining".to_string());
        }
        let client = clients
            .get(&client_id)
            .ok_or_else(|| "runtime client disconnected before admission".to_string())?;
        if client.exiting || client.registration.is_none() {
            return Err("runtime client is not initialized or is exiting".to_string());
        }
        let lease = self
            .agent_runs
            .start(session_id, agent_run_id, turn_id, control)
            .map_err(format_start_agent_run_error)?;
        self.activity_generation.fetch_add(1, Ordering::Release);
        Ok(lease)
    }

    fn request_service_shutdown(&self) -> Result<(), RuntimeHostError> {
        let _clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        self.draining.store(true, Ordering::Release);
        self.service_shutdown.notify_one();
        Ok(())
    }

    fn start_action(
        self: &Arc<Self>,
        client_id: u64,
    ) -> Result<RuntimeActionPermit, RuntimeHostError> {
        let clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        if self.is_draining() {
            return Err(RuntimeHostError::new(
                "runtime_server_draining",
                "Runtime is shutting down; new work is not accepted",
            ));
        }
        if !clients
            .get(&client_id)
            .is_some_and(|client| !client.exiting && client.registration.is_some())
        {
            return Err(RuntimeHostError::invalid_request(
                "runtime client is not initialized or is exiting",
            ));
        }
        self.active_actions.fetch_add(1, Ordering::AcqRel);
        self.activity_generation.fetch_add(1, Ordering::Release);
        Ok(RuntimeActionPermit {
            hub: Arc::clone(self),
        })
    }

    pub(crate) fn active_action_count(&self) -> u64 {
        self.active_actions.load(Ordering::Acquire)
    }

    pub(crate) async fn service_shutdown_requested(&self) {
        self.service_shutdown.notified().await;
    }

    pub(crate) fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    pub(crate) fn active_agent_runs(&self) -> Result<Vec<ActiveAgentRun>, RuntimeHostError> {
        self.agent_runs
            .active_agent_runs()
            .map_err(RuntimeHostError::transport)
    }
}

/// Keeps an accepted host action visible to service shutdown after its client
/// disconnects. The request owner holds it until the awaited action completes.
pub(crate) struct RuntimeActionPermit {
    hub: Arc<RuntimeServerClientHub>,
}

impl Drop for RuntimeActionPermit {
    fn drop(&mut self) {
        self.hub.active_actions.fetch_sub(1, Ordering::AcqRel);
        self.hub.activity_generation.fetch_add(1, Ordering::Release);
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
        self.hub.agent_runs.require_lease(lease_id.as_str())?;
        Ok(Self {
            hub: Arc::clone(&self.hub),
            client_id: 0,
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
        self.hub
            .start_agent_run(self.client_id, session_id, agent_run_id, turn_id, control)
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

    pub(crate) fn client_exited(&self) -> Result<(), RuntimeHostError> {
        self.hub.client_exited(self.client_id)
    }

    pub(crate) fn request_service_shutdown(&self) -> Result<(), RuntimeHostError> {
        self.hub.request_service_shutdown()
    }

    pub(crate) fn is_draining(&self) -> bool {
        self.hub.is_draining()
    }

    pub(crate) fn start_action(&self) -> Result<RuntimeActionPermit, RuntimeHostError> {
        self.hub.start_action(self.client_id)
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

    pub(crate) fn with_registered_viewer<T>(
        &self,
        viewer_id: &str,
        operation: impl FnOnce() -> Result<T, RuntimeHostError>,
    ) -> Result<T, RuntimeHostError> {
        self.hub
            .with_registered_viewer(self.client_id, viewer_id, operation)
    }
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
            "session already has an active AgentRun: agentRunId={}",
            busy.active_agent_run_id
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

    #[tokio::test]
    async fn run_events_broadcast_after_the_initiating_connection_is_removed() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _origin_events) = connect(&hub);
        let (observer, mut events) = connect(&hub);
        origin
            .register_client(RuntimeClientKind::Desktop, "origin")
            .unwrap();
        observer
            .register_client(RuntimeClientKind::Tui, "observer")
            .unwrap();
        let lease = start_agent_run(&origin, "session", "run");
        let run_writer = origin.for_agent_run(&lease.lease_id).unwrap();
        hub.disconnect(&origin).unwrap();
        run_writer
            .emit("session/update", json!({"agentRunId": "run"}))
            .unwrap();
        let RuntimeRpcFrame::Notification(event) =
            decode_jsonl_frame(&events.recv().await.unwrap()).unwrap()
        else {
            panic!("expected run notification");
        };
        assert_eq!(event.params["agentRunId"], "run");
        assert_eq!(
            observer.active_agent_run("run").unwrap().unwrap().lease,
            lease
        );
        run_writer.finish_agent_run(&lease.lease_id).unwrap();
        assert!(observer.active_agent_run("run").unwrap().is_none());
    }

    #[test]
    fn disconnect_preserves_the_same_runtime_lease_without_transferring_to_an_observer() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        let (observer, _observer_events) = connect(&hub);
        origin
            .register_client(RuntimeClientKind::Tui, "origin")
            .unwrap();
        observer
            .register_client(RuntimeClientKind::Desktop, "observer")
            .unwrap();
        let lease = start_agent_run(&origin, "session", "run");
        hub.disconnect(&origin).unwrap();
        let active = observer.active_agent_run("run").unwrap().unwrap();
        assert_eq!(active.lease, lease);
        assert_eq!(active.cancellation_reason().unwrap(), None);
        assert!(active
            .control
            .enqueue_supplement_with("still running".into(), || Ok(()))
            .is_ok());
        assert!(origin
            .start_agent_run("other", "other", "other", TurnControl::new())
            .is_err());
    }

    #[test]
    fn app_exit_detaches_without_transferring_or_stopping_the_run() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        let (observer, _observer_events) = connect(&hub);
        origin
            .register_client(RuntimeClientKind::Tui, "exit-origin")
            .unwrap();
        observer
            .register_client(RuntimeClientKind::Desktop, "exit-observer")
            .unwrap();
        let lease = start_agent_run(&origin, "session-exit", "run-exit");
        let result = crate::handlers::handle_request(
            &mut crate::handlers::RuntimeHostState::default(),
            crate::protocol::HostCommandRequest {
                command: "app_exit".into(),
                payload: json!({}),
            },
            origin.clone(),
        )
        .unwrap();
        assert_eq!(result, json!({"ok": true}));
        let active = observer.active_agent_run("run-exit").unwrap().unwrap();
        assert_eq!(active.lease, lease);
        assert_eq!(active.cancellation_reason().unwrap(), None);
        assert!(origin.require_registered().is_err());
    }

    #[test]
    fn detached_active_run_prevents_idle_shutdown_until_finished() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        origin
            .register_client(RuntimeClientKind::Desktop, "idle-origin")
            .unwrap();
        let lease = start_agent_run(&origin, "idle-session", "idle-run");
        let run_writer = origin.for_agent_run(&lease.lease_id).unwrap();
        hub.disconnect(&origin).unwrap();
        assert!(hub.has_clients_or_active_agent_runs().unwrap());
        assert!(!hub.begin_idle_shutdown(hub.activity_generation()).unwrap());
        run_writer.finish_agent_run(&lease.lease_id).unwrap();
        assert!(!hub.has_clients_or_active_agent_runs().unwrap());
        assert!(hub.begin_idle_shutdown(hub.activity_generation()).unwrap());
        assert!(hub.connect().unwrap().is_none());
    }

    #[tokio::test]
    async fn service_shutdown_closes_admission_but_keeps_active_runs_queryable() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        origin
            .register_client(RuntimeClientKind::Desktop, "shutdown-origin")
            .unwrap();
        let lease = start_agent_run(&origin, "shutdown-session", "shutdown-run");
        origin.request_service_shutdown().unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            hub.service_shutdown_requested(),
        )
        .await
        .unwrap();
        assert!(hub.is_draining());
        assert!(hub.connect().unwrap().is_none());
        assert_eq!(
            origin
                .start_agent_run("next-session", "next-run", "next-turn", TurnControl::new())
                .unwrap_err(),
            "runtime_server_draining"
        );
        assert!(origin.require_registered().is_ok());
        assert_eq!(
            origin
                .active_agent_run("shutdown-run")
                .unwrap()
                .unwrap()
                .lease,
            lease
        );
        assert_eq!(hub.active_agent_runs().unwrap().len(), 1);
        let active = hub.active_agent_runs().unwrap().pop().unwrap();
        active
            .close_with_cancellation("user_cancelled", || Ok(()))
            .unwrap();
        assert_eq!(
            active.cancellation_reason().unwrap().as_deref(),
            Some("user_cancelled")
        );
        origin.finish_agent_run(&lease.lease_id).unwrap();
        assert!(hub.active_agent_runs().unwrap().is_empty());
    }

    #[test]
    fn concurrent_service_shutdown_never_admits_work_after_the_drain_boundary() {
        for index in 0..32 {
            let hub = Arc::new(RuntimeServerClientHub::default());
            let (origin, _events) = connect(&hub);
            origin
                .register_client(RuntimeClientKind::Tui, &format!("race-{index}"))
                .unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|scope| {
                let barrier_for_start = Arc::clone(&barrier);
                let writer = origin.clone();
                let start = scope.spawn(move || {
                    barrier_for_start.wait();
                    writer.start_agent_run("session", "run", "turn", TurnControl::new())
                });
                barrier.wait();
                origin.request_service_shutdown().unwrap();
                let accepted = start.join().unwrap();
                assert_eq!(
                    hub.active_agent_runs().unwrap().len(),
                    usize::from(accepted.is_ok())
                );
                assert!(origin
                    .start_agent_run("late-session", "late-run", "late-turn", TurnControl::new())
                    .is_err());
            });
        }
    }

    #[test]
    fn failed_broadcast_retains_viewer_registration_until_disconnect_cleanup() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, events) = connect(&hub);
        let (reconnected, _events) = connect(&hub);
        origin
            .register_client(RuntimeClientKind::Desktop, "broadcast-viewer")
            .unwrap();
        drop(events);
        hub.broadcaster().emit("session/update", json!({})).unwrap();
        assert!(reconnected
            .register_client(RuntimeClientKind::Desktop, "broadcast-viewer")
            .is_err());
        let mut detached = None;
        hub.disconnect_with_detach(&origin, |viewer| {
            detached = Some(viewer.to_string());
            Ok(())
        })
        .unwrap();
        assert_eq!(detached.as_deref(), Some("broadcast-viewer"));
        reconnected
            .register_client(RuntimeClientKind::Desktop, "broadcast-viewer")
            .unwrap();
    }

    #[test]
    fn admitted_host_action_survives_disconnect_and_blocks_idle_until_completion() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        assert!(origin.start_action().is_err());
        origin
            .register_client(RuntimeClientKind::Desktop, "action-origin")
            .unwrap();
        let first = origin.start_action().unwrap();
        let second = origin.start_action().unwrap();
        assert_eq!(hub.active_action_count(), 2);
        hub.disconnect(&origin).unwrap();
        assert!(origin.start_action().is_err());
        assert!(hub.has_clients_or_active_agent_runs().unwrap());
        assert!(!hub.begin_idle_shutdown(hub.activity_generation()).unwrap());
        drop(first);
        assert_eq!(hub.active_action_count(), 1);
        drop(second);
        assert_eq!(hub.active_action_count(), 0);
        assert!(hub.begin_idle_shutdown(hub.activity_generation()).unwrap());
    }

    #[test]
    fn concurrent_host_action_admission_is_accounted_for_before_service_shutdown() {
        for index in 0..32 {
            let hub = Arc::new(RuntimeServerClientHub::default());
            let (origin, _events) = connect(&hub);
            origin
                .register_client(RuntimeClientKind::Desktop, &format!("action-race-{index}"))
                .unwrap();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|scope| {
                let writer = origin.clone();
                let worker_barrier = Arc::clone(&barrier);
                let admitting = scope.spawn(move || {
                    worker_barrier.wait();
                    writer.start_action()
                });
                barrier.wait();
                origin.request_service_shutdown().unwrap();
                let accepted = admitting.join().unwrap();
                assert_eq!(hub.active_action_count(), u64::from(accepted.is_ok()));
                assert!(origin.start_action().is_err());
                drop(accepted);
                assert_eq!(hub.active_action_count(), 0);
            });
        }
    }

    #[test]
    fn registered_viewer_operations_require_the_current_connection_identity() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        assert!(origin
            .with_registered_viewer::<()>("viewer", || panic!("uninitialized mutation"))
            .is_err());
        origin
            .register_client(RuntimeClientKind::Tui, "viewer")
            .unwrap();
        assert!(origin
            .with_registered_viewer::<()>("wrong-viewer", || panic!("wrong-viewer mutation"))
            .is_err());
        assert_eq!(
            origin.with_registered_viewer("viewer", || Ok(7)).unwrap(),
            7
        );
        origin.client_exited().unwrap();
        assert!(origin
            .with_registered_viewer::<()>("viewer", || panic!("exited mutation"))
            .is_err());
    }

    #[test]
    fn disconnect_cannot_overtake_an_in_flight_registered_viewer_operation() {
        for graceful_exit in [true, false] {
            let hub = Arc::new(RuntimeServerClientHub::default());
            let (origin, _events) = connect(&hub);
            origin
                .register_client(RuntimeClientKind::Desktop, "in-flight-viewer")
                .unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (attempting_tx, attempting_rx) = std::sync::mpsc::channel();
            let (cleanup_done_tx, cleanup_done_rx) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                let exiting_writer = origin.clone();
                let clients = Arc::clone(&hub);
                let exit = scope.spawn(move || {
                    entered_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    attempting_tx.send(()).unwrap();
                    if graceful_exit {
                        exiting_writer.client_exited().unwrap();
                    } else {
                        clients.disconnect(&exiting_writer).unwrap();
                    }
                    cleanup_done_tx.send(()).unwrap();
                });
                let cleanup_overtook_operation = origin
                    .with_registered_viewer("in-flight-viewer", || {
                        entered_tx.send(()).unwrap();
                        attempting_rx
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        // This is the operation's final mutation checkpoint. Cleanup
                        // must remain blocked until this closure finishes.
                        Ok(cleanup_done_rx
                            .recv_timeout(std::time::Duration::from_millis(100))
                            .is_ok())
                    })
                    .unwrap();
                exit.join().unwrap();
                assert!(!cleanup_overtook_operation, "cleanup overtook the registered viewer mutation; graceful_exit={graceful_exit}");
                assert!(origin
                    .with_registered_viewer::<()>("in-flight-viewer", || panic!("late mutation"))
                    .is_err());
            });
        }
    }

    #[test]
    fn reconnect_cannot_reuse_a_viewer_until_old_binding_cleanup_finishes() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        let (reconnected, _events) = connect(&hub);
        origin
            .register_client(RuntimeClientKind::Desktop, "cleanup-viewer")
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (attempting_tx, attempting_rx) = std::sync::mpsc::channel();
        let (registered_tx, registered_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let register = scope.spawn(move || {
                entered_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                attempting_tx.send(()).unwrap();
                let result =
                    reconnected.register_client(RuntimeClientKind::Desktop, "cleanup-viewer");
                registered_tx.send(result).unwrap();
            });
            let mut registered_before_cleanup = false;
            hub.disconnect_with_detach(&origin, |viewer| {
                assert_eq!(viewer, "cleanup-viewer");
                entered_tx.send(()).unwrap();
                attempting_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                registered_before_cleanup = registered_rx
                    .recv_timeout(std::time::Duration::from_millis(100))
                    .is_ok();
                Ok(())
            })
            .unwrap();
            register.join().unwrap();
            assert!(
                !registered_before_cleanup,
                "a replacement viewer registered while old cleanup could still erase its binding"
            );
            registered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
                .unwrap();
        });
    }
}
