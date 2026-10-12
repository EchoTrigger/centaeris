//! JSON-RPC framing shared by every Runtime Server local connection.

use crate::errors::RuntimeHostError;
use crate::runtime_rpc::{RuntimeRpcNotification, RuntimeRpcResponse};
use crate::runtime_server::{
    ActiveAgentRun, AgentRunLease, AgentRunRegistry, RuntimeClientKind, StartAgentRunError,
};
use centaeris_core::runtime::TurnControl;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeClientRegistration {
    pub(crate) kind: RuntimeClientKind,
    pub(crate) viewer_id: String,
}

pub(crate) const RPC_INPUT_FRAME_MAX_BYTES: usize = 16 * 1024 * 1024;
const RPC_OUTPUT_FRAME_MAX_BYTES: usize = 160 * 1024 * 1024;
const RPC_OUTBOUND_MESSAGES: usize = 128;
const RPC_CLIENT_OUTBOUND_BYTES: usize = 256 * 1024 * 1024;
const RPC_GLOBAL_OUTBOUND_BYTES: usize = 512 * 1024 * 1024;
const RPC_MAX_CONNECTIONS: usize = 32;
const RPC_CLIENT_REQUESTS: usize = 8;
const RPC_GLOBAL_REQUESTS: usize = 32;
const RPC_CLIENT_CONTROL_REQUESTS: usize = 4;
const RPC_GLOBAL_CONTROL_REQUESTS: usize = 8;

// Byte permits remain owned until the complete frame has been written or dropped.
pub(crate) struct OutboundFrame {
    text: String,
    _client_bytes: OwnedSemaphorePermit,
    _global_bytes: OwnedSemaphorePermit,
}
impl std::ops::Deref for OutboundFrame {
    type Target = String;
    fn deref(&self) -> &String {
        &self.text
    }
}

#[derive(Clone)]
struct OutboundSender {
    sender: mpsc::Sender<OutboundFrame>,
    bytes: Arc<Semaphore>,
    global_bytes: Arc<Semaphore>,
    disconnected: CancellationToken,
}
impl OutboundSender {
    fn send(&self, text: String) -> Result<(), RuntimeHostError> {
        let enqueue = || {
            if self.disconnected.is_cancelled() || text.len() > RPC_OUTPUT_FRAME_MAX_BYTES {
                return Err(());
            }
            let bytes = text.len() as u32;
            let client = self
                .bytes
                .clone()
                .try_acquire_many_owned(bytes)
                .map_err(|_| ())?;
            let global = self
                .global_bytes
                .clone()
                .try_acquire_many_owned(bytes)
                .map_err(|_| ())?;
            self.sender
                .try_send(OutboundFrame {
                    text,
                    _client_bytes: client,
                    _global_bytes: global,
                })
                .map_err(|_| ())
        };
        if enqueue().is_err() {
            self.disconnected.cancel();
            return Err(RuntimeHostError::transport(
                "runtime client outbound capacity exhausted or closed; reconnect and reconcile",
            ));
        }
        Ok(())
    }
}

pub(crate) struct RequestPermit {
    _client: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

struct ConnectedClient {
    outbound: OutboundSender,
    requests: Arc<Semaphore>,
    controls: Arc<Semaphore>,
    registration: Option<RuntimeClientRegistration>,
    exiting: bool,
}

pub(crate) struct RuntimeServerClientHub {
    model_http: Mutex<Option<reqwest::Client>>,
    clients: Mutex<HashMap<u64, ConnectedClient>>,
    next_client_id: AtomicU64,
    activity_generation: AtomicU64,
    active_actions: AtomicU64,
    agent_runs: AgentRunRegistry,
    draining: AtomicBool,
    service_shutdown: Notify,
    outbound_bytes: Arc<Semaphore>,
    requests: Arc<Semaphore>,
    controls: Arc<Semaphore>,
}

impl Default for RuntimeServerClientHub {
    fn default() -> Self {
        Self {
            model_http: Mutex::new(None),
            clients: Mutex::new(HashMap::new()),
            next_client_id: AtomicU64::new(0),
            activity_generation: AtomicU64::new(0),
            active_actions: AtomicU64::new(0),
            agent_runs: AgentRunRegistry::default(),
            draining: AtomicBool::new(false),
            service_shutdown: Notify::new(),
            outbound_bytes: Arc::new(Semaphore::new(RPC_GLOBAL_OUTBOUND_BYTES)),
            requests: Arc::new(Semaphore::new(RPC_GLOBAL_REQUESTS)),
            controls: Arc::new(Semaphore::new(RPC_GLOBAL_CONTROL_REQUESTS)),
        }
    }
}

impl RuntimeServerClientHub {
    pub(crate) fn model_http_client(&self) -> Result<reqwest::Client, String> {
        self.model_http_client_with(crate::http_transport::build_model_http_client)
    }

    fn model_http_client_with(
        &self,
        build: impl FnOnce() -> Result<reqwest::Client, String>,
    ) -> Result<reqwest::Client, String> {
        let mut cached = self
            .model_http
            .lock()
            .map_err(|_| "runtime model HTTP client lock poisoned".to_string())?;
        if let Some(client) = cached.as_ref() {
            return Ok(client.clone());
        }
        let client = build()?;
        *cached = Some(client.clone());
        Ok(client)
    }
    pub(crate) fn connect(
        self: &Arc<Self>,
    ) -> Result<Option<(EventWriter, mpsc::Receiver<OutboundFrame>)>, RuntimeHostError> {
        let client_id = self.next_client_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = mpsc::channel(RPC_OUTBOUND_MESSAGES);
        let outbound = OutboundSender {
            sender,
            bytes: Arc::new(Semaphore::new(RPC_CLIENT_OUTBOUND_BYTES)),
            global_bytes: self.outbound_bytes.clone(),
            disconnected: CancellationToken::new(),
        };
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        if self.draining.load(Ordering::Acquire) {
            return Ok(None);
        }
        if clients.len() >= RPC_MAX_CONNECTIONS {
            return Err(RuntimeHostError::transport(
                "runtime client connection capacity exhausted",
            ));
        }
        clients.insert(
            client_id,
            ConnectedClient {
                outbound,
                requests: Arc::new(Semaphore::new(RPC_CLIENT_REQUESTS)),
                controls: Arc::new(Semaphore::new(RPC_CLIENT_CONTROL_REQUESTS)),
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

    fn request_permit(
        &self,
        client_id: u64,
        control: bool,
    ) -> Result<RequestPermit, RuntimeHostError> {
        let clients = self
            .clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?;
        let client = clients
            .get(&client_id)
            .filter(|client| !client.exiting && !client.outbound.disconnected.is_cancelled())
            .ok_or_else(|| {
                RuntimeHostError::transport("runtime client disconnected before dispatch")
            })?;
        let (local, global) = if control {
            (&client.controls, &self.controls)
        } else {
            (&client.requests, &self.requests)
        };
        let local = local.clone().try_acquire_owned().map_err(|_| {
            RuntimeHostError::transport("runtime client request capacity exhausted")
        })?;
        let global = global.clone().try_acquire_owned().map_err(|_| {
            RuntimeHostError::transport("runtime global request capacity exhausted")
        })?;
        Ok(RequestPermit {
            _client: local,
            _global: global,
        })
    }

    fn disconnect_token(&self, client_id: u64) -> Result<CancellationToken, RuntimeHostError> {
        self.clients
            .lock()
            .map_err(|_| RuntimeHostError::transport("runtime server client hub lock poisoned"))?
            .get(&client_id)
            .map(|client| client.outbound.disconnected.clone())
            .ok_or_else(|| RuntimeHostError::transport("runtime client disconnected"))
    }

    pub(crate) fn broadcaster(self: &Arc<Self>) -> EventWriter {
        EventWriter::new(Arc::clone(self), 0)
    }
    pub(crate) fn process_scheduler(self: &Arc<Self>) -> EventWriter {
        EventWriter {
            hub: Arc::clone(self),
            client_id: 0,
            internal_admission: true,
        }
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
        client.outbound.disconnected.cancel();
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
        Ok(has_clients
            || active_agent_run_count > 0
            || self.active_action_count() > 0
            || self.agent_runs.background_work_count() > 0)
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
            || self.agent_runs.background_work_count() > 0
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
        let encoded = encode(value).map_err(RpcEncodeError::into_host_error)?;
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

    fn send(&self, client_id: u64, response: &RuntimeRpcResponse) -> Result<(), RuntimeHostError> {
        let encoded = match encode(response) {
            Ok(encoded) => encoded,
            Err(RpcEncodeError::TooLarge) => {
                // A typed failure preserves the request ID without truncation or disconnect.
                let error = RpcEncodeError::TooLarge.into_host_error();
                encode(&RuntimeRpcResponse::failure(
                    response.id.clone(),
                    error.to_runtime_rpc_error(),
                ))
                .map_err(RpcEncodeError::into_host_error)?
            }
            Err(error) => return Err(error.into_host_error()),
        };
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
        client_id: Option<u64>,
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
        if let Some(client_id) = client_id {
            let client = clients
                .get(&client_id)
                .ok_or_else(|| "runtime client disconnected before admission".to_string())?;
            if client.exiting || client.registration.is_none() {
                return Err("runtime client is not initialized or is exiting".to_string());
            }
        }
        let lease = self
            .agent_runs
            .start(session_id, agent_run_id, turn_id, control)
            .map_err(format_start_agent_run_error)?;
        self.activity_generation.fetch_add(1, Ordering::Release);
        Ok(lease)
    }

    pub(crate) fn request_service_shutdown(&self) -> Result<(), RuntimeHostError> {
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

    #[cfg(feature = "native-job-store")]
    pub(crate) async fn drain_background_work(&self) -> Result<(), RuntimeHostError> {
        if !self.is_draining() {
            return Err(RuntimeHostError::transport(
                "background drain requires stopped service admission",
            ));
        }
        self.agent_runs
            .drain_background_work()
            .await
            .map_err(RuntimeHostError::transport)
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
    internal_admission: bool,
}

impl EventWriter {
    pub(crate) fn model_http_client(&self) -> Result<reqwest::Client, String> {
        self.hub.model_http_client()
    }
    fn new(hub: Arc<RuntimeServerClientHub>, client_id: u64) -> Self {
        Self {
            hub,
            client_id,
            internal_admission: false,
        }
    }

    pub(crate) fn for_agent_run(&self, lease_id: &str) -> Result<Self, String> {
        let lease_id =
            required_identifier(lease_id, "leaseId").map_err(|error| error.to_string())?;
        self.hub.agent_runs.require_lease(lease_id.as_str())?;
        Ok(Self {
            hub: Arc::clone(&self.hub),
            client_id: 0,
            internal_admission: false,
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
        self.hub.start_agent_run(
            (!self.internal_admission).then_some(self.client_id),
            session_id,
            agent_run_id,
            turn_id,
            control,
        )
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

    #[cfg(feature = "native-job-store")]
    pub(crate) fn begin_session_deletion(
        &self,
        sessions: &[String],
    ) -> Result<crate::runtime_server::SessionDeletionRegistration, String> {
        self.hub.agent_runs.begin_session_deletion(sessions)
    }

    #[cfg(feature = "native-job-store")]
    pub(crate) fn begin_background_work(
        &self,
        session: &str,
    ) -> Result<tokio_util::task::task_tracker::TaskTrackerToken, String> {
        let _clients = self
            .hub
            .clients
            .lock()
            .map_err(|_| "runtime server client hub lock poisoned")?;
        if self.hub.is_draining() {
            return Err("runtime_server_draining".into());
        }
        self.hub.agent_runs.begin_background_work(session)
    }

    #[cfg(feature = "native-job-store")]
    pub(crate) async fn wait_for_background_work(&self, sessions: &[String]) -> Result<(), String> {
        self.hub.agent_runs.wait_for_background_work(sessions).await
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

    pub(crate) fn request_permit(&self, control: bool) -> Result<RequestPermit, RuntimeHostError> {
        self.hub.request_permit(self.client_id, control)
    }
    pub(crate) fn disconnect_token(&self) -> Result<CancellationToken, RuntimeHostError> {
        self.hub.disconnect_token(self.client_id)
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

#[derive(Debug)]
enum RpcEncodeError {
    TooLarge,
    Serialize(serde_json::Error),
}
impl RpcEncodeError {
    fn into_host_error(self) -> RuntimeHostError {
        match self {
            Self::TooLarge => RuntimeHostError::new(
                "runtime_rpc_response_too_large",
                format!("Runtime response exceeds {RPC_OUTPUT_FRAME_MAX_BYTES} encoded bytes; use bounded transcript reads for history"),
            ),
            Self::Serialize(error) => RuntimeHostError::transport(format!("runtime response encode failed: {error}")),
        }
    }
}

struct RpcFrameBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for RpcFrameBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self.bytes.len().checked_add(bytes.len());
        let Some(required) = required.filter(|required| *required <= self.limit) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RPC output frame limit exceeded",
            ));
        };
        if required > self.bytes.capacity() {
            // Grow geometrically for many tiny escapes, but never beyond the cap.
            let capacity = self
                .bytes
                .capacity()
                .max(1024)
                .saturating_mul(2)
                .min(self.limit)
                .max(required);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode<TValue: serde::Serialize>(value: &TValue) -> Result<String, RpcEncodeError> {
    encode_with_limit(value, RPC_OUTPUT_FRAME_MAX_BYTES)
}

fn encode_with_limit<TValue: serde::Serialize>(
    value: &TValue,
    limit: usize,
) -> Result<String, RpcEncodeError> {
    let mut buffer = RpcFrameBuffer {
        bytes: Vec::new(),
        limit,
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut buffer, value) {
        return Err(if buffer.exceeded {
            RpcEncodeError::TooLarge
        } else {
            RpcEncodeError::Serialize(error)
        });
    }
    buffer
        .write_all(b"\n")
        .map_err(|_| RpcEncodeError::TooLarge)?;
    Ok(String::from_utf8(buffer.bytes).expect("JSON serializer writes valid UTF-8"))
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

    static LARGE_FRAME_TEST: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn capped_json_encoding_includes_lf_and_preserves_utf8_and_escapes() {
        assert_eq!(
            encode_with_limit(&serde_json::Value::Null, 5).unwrap(),
            "null\n"
        );
        assert!(matches!(
            encode_with_limit(&serde_json::Value::Null, 4),
            Err(RpcEncodeError::TooLarge)
        ));
        let value = json!({"text":"中文\r\n"});
        let expected = format!("{}\n", serde_json::to_string(&value).unwrap());
        assert_eq!(encode_with_limit(&value, expected.len()).unwrap(), expected);
        assert!(matches!(
            encode_with_limit(&value, expected.len() - 1),
            Err(RpcEncodeError::TooLarge)
        ));
    }

    #[test]
    fn capped_writer_never_appends_over_limit_and_serializer_stops_early() {
        let mut buffer = RpcFrameBuffer {
            bytes: Vec::new(),
            limit: 4,
            exceeded: false,
        };
        assert_eq!(buffer.write(b"1234").unwrap(), 4);
        assert!(buffer.write(b"5").is_err());
        assert_eq!(buffer.bytes, b"1234");
        assert!(buffer.bytes.capacity() <= 4);
        struct Counted<'a>(&'a AtomicU64);
        impl serde::Serialize for Counted<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut sequence = serializer.serialize_seq(None)?;
                for _ in 0..1000 {
                    self.0.fetch_add(1, Ordering::Relaxed);
                    sequence.serialize_element("abcdefgh")?;
                }
                sequence.end()
            }
        }
        let serialized = AtomicU64::new(0);
        assert!(matches!(
            encode_with_limit(&Counted(&serialized), 32),
            Err(RpcEncodeError::TooLarge)
        ));
        assert!(serialized.load(Ordering::Relaxed) < 10);
    }

    #[test]
    fn serialization_failure_remains_distinct_from_capacity_failure() {
        struct Failed;
        impl serde::Serialize for Failed {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("deliberate failure"))
            }
        }
        assert!(matches!(
            encode_with_limit(&Failed, 32),
            Err(RpcEncodeError::Serialize(_))
        ));
    }

    fn connect(hub: &Arc<RuntimeServerClientHub>) -> (EventWriter, mpsc::Receiver<OutboundFrame>) {
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
    fn dispatch_limits_release_and_keep_control_capacity_after_disconnect() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (origin, _events) = connect(&hub);
        let permits = (0..RPC_CLIENT_REQUESTS)
            .map(|_| origin.request_permit(false).unwrap())
            .collect::<Vec<_>>();
        assert!(origin.request_permit(false).is_err());
        let control = origin.request_permit(true).unwrap();
        hub.disconnect(&origin).unwrap();
        assert!(origin.request_permit(true).is_err());
        // Accepted requests own capacity after detach; a replacement still has control headroom.
        let (replacement, _events) = connect(&hub);
        let replacement_control = replacement.request_permit(true).unwrap();
        assert_eq!(
            hub.requests.available_permits(),
            RPC_GLOBAL_REQUESTS - RPC_CLIENT_REQUESTS
        );
        drop(permits);
        drop(control);
        drop(replacement_control);
        assert_eq!(hub.requests.available_permits(), RPC_GLOBAL_REQUESTS);
        assert_eq!(
            hub.controls.available_permits(),
            RPC_GLOBAL_CONTROL_REQUESTS
        );
    }

    #[test]
    fn global_dispatch_limit_has_reserved_control_and_releases_on_drop() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let writers = (0..5).map(|_| connect(&hub).0).collect::<Vec<_>>();
        let permits = writers[..4]
            .iter()
            .flat_map(|writer| {
                (0..RPC_CLIENT_REQUESTS).map(move |_| writer.request_permit(false).unwrap())
            })
            .collect::<Vec<_>>();
        assert!(writers[4].request_permit(false).is_err());
        assert!(writers[4].request_permit(true).is_ok());
        drop(permits);
        assert!(writers[4].request_permit(false).is_ok());
    }

    #[tokio::test]
    async fn outbound_bytes_are_owned_through_write_and_release_after_drop() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (writer, mut output) = connect(&hub);
        let response =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"result":"x"})).unwrap();
        writer.send_response(&response).unwrap();
        let frame = output.recv().await.unwrap();
        assert_eq!(
            hub.outbound_bytes.available_permits(),
            RPC_GLOBAL_OUTBOUND_BYTES - frame.len()
        );
        drop(frame);
        assert_eq!(
            hub.outbound_bytes.available_permits(),
            RPC_GLOBAL_OUTBOUND_BYTES
        );
    }

    #[test]
    fn connection_limit_releases_after_disconnect() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let clients = (0..RPC_MAX_CONNECTIONS)
            .map(|_| connect(&hub))
            .collect::<Vec<_>>();
        assert!(hub.connect().is_err());
        hub.disconnect(&clients[0].0).unwrap();
        assert!(hub.connect().unwrap().is_some());
    }

    #[tokio::test]
    async fn maximum_desktop_preview_remains_deliverable_after_json_expansion() {
        let _guard = LARGE_FRAME_TEST.lock().await;
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "centaeris-rpc-preview-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        let chunk = vec![0; 1024 * 1024];
        for _ in 0..25 {
            file.write_all(&chunk).unwrap();
        }
        drop(file);
        let preview = crate::desktop_file_preview::read(
            crate::desktop_file_preview::DesktopFilePreviewReadRequest {
                path: path.to_string_lossy().into_owned(),
                workspace_root: None,
                base_path: None,
            },
        )
        .unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(preview.byte_len, 25 * 1024 * 1024);
        let id = serde_json::from_value::<crate::runtime_rpc::RuntimeRpcRequest>(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        )
        .unwrap()
        .id;
        let response = RuntimeRpcResponse::success(id, serde_json::to_value(preview).unwrap());
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (writer, mut output) = connect(&hub);
        writer.send_response(&response).unwrap();
        let frame = output.recv().await.unwrap();
        assert!(frame.len() > 150 * 1024 * 1024);
        assert!(frame.len() < RPC_OUTPUT_FRAME_MAX_BYTES);
    }

    #[tokio::test]
    async fn output_frame_boundary_is_inclusive_and_bytes_release() {
        let _guard = LARGE_FRAME_TEST.lock().await;
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (writer, mut output) = connect(&hub);
        let sender = hub
            .clients
            .lock()
            .unwrap()
            .get(&writer.client_id)
            .unwrap()
            .outbound
            .clone();
        sender.send("x".repeat(RPC_OUTPUT_FRAME_MAX_BYTES)).unwrap();
        let frame = output.recv().await.unwrap();
        assert_eq!(frame.len(), RPC_OUTPUT_FRAME_MAX_BYTES);
        drop(frame);
        assert_eq!(
            hub.outbound_bytes.available_permits(),
            RPC_GLOBAL_OUTBOUND_BYTES
        );
        assert!(sender
            .send("x".repeat(RPC_OUTPUT_FRAME_MAX_BYTES + 1))
            .is_err());
    }

    #[tokio::test]
    async fn oversized_response_returns_typed_error_without_closing_client() {
        let _guard = LARGE_FRAME_TEST.lock().await;
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (writer, mut output) = connect(&hub);
        let response: RuntimeRpcResponse = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":1,"result":"\0".repeat(27 * 1024 * 1024)}),
        )
        .unwrap();
        writer.send_response(&response).unwrap();
        let frame = output.recv().await.unwrap();
        assert!(frame.len() < 1024);
        let RuntimeRpcFrame::Response(response) = decode_jsonl_frame(&frame).unwrap() else {
            panic!("expected response");
        };
        assert!(response.result.is_none());
        assert_eq!(
            response.error.unwrap().data.unwrap()["code"],
            "runtime_rpc_response_too_large"
        );
        assert!(!writer.disconnect_token().unwrap().is_cancelled());
    }

    #[tokio::test]
    async fn outbound_byte_backlog_rejects_a_large_response_before_message_limit() {
        let _guard = LARGE_FRAME_TEST.lock().await;
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (writer, _output) = connect(&hub);
        let response = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":1,"result":"\0".repeat(15 * 1024 * 1024)}),
        )
        .unwrap();
        for _ in 0..2 {
            writer.send_response(&response).unwrap();
        }
        assert!(writer.send_response(&response).is_err());
    }

    #[tokio::test]
    async fn outbound_backlog_disconnects_only_slow_client() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (slow, _slow_output) = connect(&hub);
        let (fast, mut fast_output) = connect(&hub);
        slow.register_client(RuntimeClientKind::Desktop, "slow")
            .unwrap();
        fast.register_client(RuntimeClientKind::Tui, "fast")
            .unwrap();
        for _ in 0..129 {
            hub.broadcaster()
                .emit("runtime.progress", json!({}))
                .unwrap();
            assert!(fast_output.recv().await.is_some());
        }
        assert!(slow
            .send_response(
                &serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"result":{}})).unwrap()
            )
            .is_err());
        assert!(fast.require_registered().is_ok());
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
    fn process_scheduler_admission_is_client_independent_but_not_fence_independent() {
        let hub = Arc::new(RuntimeServerClientHub::default());
        let writer = hub.process_scheduler();
        assert!(hub
            .broadcaster()
            .start_agent_run("s", "broadcast-run", "t", TurnControl::new())
            .is_err());
        let lease = writer
            .start_agent_run("s", "internal-run", "t", TurnControl::new())
            .unwrap();
        assert!(writer
            .start_agent_run("s", "other-run", "t2", TurnControl::new())
            .is_err());
        assert!(hub.has_clients_or_active_agent_runs().unwrap());
        writer.finish_agent_run(&lease.lease_id).unwrap();
        writer
            .with_session_deletion(&["s".into()], || {
                assert!(writer
                    .start_agent_run("s", "deleted-run", "t3", TurnControl::new())
                    .is_err());
                Ok(())
            })
            .unwrap();
        hub.request_service_shutdown().unwrap();
        assert_eq!(
            writer
                .start_agent_run("s", "shutdown-run", "t4", TurnControl::new())
                .unwrap_err(),
            "runtime_server_draining"
        );
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

#[cfg(test)]
mod model_client_tests;
