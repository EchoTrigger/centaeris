//! Profile-scoped local Runtime Server process and socket transport.

use crate::agent_runtime;
use crate::errors::RuntimeHostError;
use crate::handlers::{handle_request_async, handle_stateless_request_async, RuntimeHostState};
use crate::protocol::HostCommandRequest;
use crate::runtime_rpc::{
    decode_jsonl_frame, RuntimeRpcFrame, RuntimeRpcRequest, RuntimeRpcResponse,
};
use crate::runtime_rpc_transport::{EventWriter, RuntimeServerClientHub};
use crate::subagent_scheduler;
use centaeris_core::session::reliability::{ListRuntimeJobsRequest, RuntimeJobStatus};
use fs2::FileExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

const ENDPOINT_PREFIX: &str = "centaeris-runtime";
const RUNTIME_SERVER_IDLE_CHECK_INTERVAL_MS: u64 = 1_000;
const RUNTIME_SERVER_IDLE_TIMEOUT_MS: u64 = 5_000;
const RUNTIME_SERVER_SHUTDOWN_DRAIN_MS: u64 = 5_000;
const RUNTIME_SERVER_SHUTDOWN_CLEANUP_MS: u64 = 5_000;

fn log_runtime_server(message: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr(), "{message}");
}

pub(crate) struct RuntimeServerEndpoint {
    pub(crate) endpoint: String,
    writer_lock_path: PathBuf,
}

#[derive(Debug)]
struct RuntimeServerSingleton {
    _file: File,
}

// Tokio shutdown may leave blocking host calls alive until the process exits.
// Keep the profile writer lock for that entire lifetime, not only the listener.
static PROCESS_SINGLETON: OnceLock<RuntimeServerSingleton> = OnceLock::new();

pub(crate) fn print_endpoint() -> Result<(), RuntimeHostError> {
    crate::user_data_layout::ensure_runtime_endpoint_layout()
        .map_err(|error| RuntimeHostError::new("runtime_endpoint_layout_init_failed", error))?;
    let endpoint = current_endpoint()?;
    write_json_line(&serde_json::json!({ "endpoint": endpoint.endpoint }))
}

pub(crate) async fn run_server() -> Result<(), RuntimeHostError> {
    crate::user_data_layout::ensure_runtime_endpoint_layout()
        .map_err(|error| RuntimeHostError::new("runtime_endpoint_layout_init_failed", error))?;
    let endpoint = current_endpoint()?;
    let singleton = RuntimeServerSingleton::acquire(endpoint.writer_lock_path.as_path())?;
    PROCESS_SINGLETON.set(singleton).map_err(|_| {
        RuntimeHostError::new(
            "runtime_server_already_running",
            "Runtime Server already initialized in this process",
        )
    })?;
    crate::user_data_layout::ensure_user_data_layout()
        .map_err(|error| RuntimeHostError::new("user_data_layout_init_failed", error))?;
    crate::system_skills_deployment::deploy();
    agent_runtime::agent_runtime_store_actor()
        .map_err(|error| RuntimeHostError::new("runtime_server_store_init_failed", error))?;
    agent_runtime::recover_unsealed_live_text_journals().map_err(|error| {
        RuntimeHostError::new("runtime_server_live_text_recovery_failed", error)
    })?;
    subagent_scheduler::reconcile_stopped_jobs()
        .await
        .map_err(|error| RuntimeHostError::new("runtime_server_job_recovery_failed", error))?;
    let state = Arc::new(Mutex::new(RuntimeHostState::default()));
    let clients = Arc::new(RuntimeServerClientHub::default());
    let shutdown_jobs = Arc::new(subagent_scheduler::ShutdownJobs::default());
    let background_worker = tokio::spawn(subagent_scheduler::run_background_worker(
        clients.broadcaster(),
        Arc::clone(&clients),
        Arc::clone(&shutdown_jobs),
    ));
    let shutdown = Arc::new(Notify::new());
    let idle_monitor = tokio::spawn(agent_run_idle_shutdown_monitor(
        Arc::clone(&clients),
        Arc::clone(&shutdown),
    ));
    #[cfg(windows)]
    let serve = serve_windows(
        endpoint.endpoint.as_str(),
        state,
        Arc::clone(&clients),
        shutdown,
    );
    #[cfg(unix)]
    let serve = serve_unix(
        endpoint.endpoint.as_str(),
        state,
        Arc::clone(&clients),
        shutdown,
    );
    let result = tokio::select! {
        result = serve => result,
        _ = clients.service_shutdown_requested() => Ok(()),
    };
    // The listener may finish because a new connection observes draining at
    // the same instant as the shutdown notification. Both exits must drain.
    let result = if clients.is_draining() {
        let drained = drain_service_shutdown(
            &clients,
            &background_worker,
            &shutdown_jobs,
            Duration::from_millis(RUNTIME_SERVER_SHUTDOWN_DRAIN_MS),
            Duration::from_millis(RUNTIME_SERVER_SHUTDOWN_CLEANUP_MS),
        )
        .await;
        result.and(drained)
    } else {
        result
    };
    background_worker.abort();
    idle_monitor.abort();
    result
}

async fn drain_service_shutdown(
    clients: &RuntimeServerClientHub,
    background_worker: &tokio::task::JoinHandle<()>,
    shutdown_jobs: &subagent_scheduler::ShutdownJobs,
    drain: Duration,
    cleanup: Duration,
) -> Result<(), RuntimeHostError> {
    // The shutdown reply shares the normal asynchronous writer. Yield before
    // checking an empty server, without making delivery a completion condition.
    tokio::task::yield_now().await;
    let drained =
        tokio::time::timeout(drain, wait_for_runtime_work(clients, background_worker)).await;
    if let Ok(result) = drained {
        return result;
    }
    // Cancellation closes admission; the owner remains responsible for tool
    // receipts and its terminal fact. A timeout never fabricates that fact.
    let result = tokio::time::timeout(cleanup, async {
        let active_runs = clients.active_agent_runs()?;
        tokio::task::spawn_blocking(move || {
            for active in active_runs {
                active.close_with_shutdown(|| Ok(()))?;
            }
            Ok::<(), String>(())
        })
        .await
        .map_err(|error| {
            RuntimeHostError::new("runtime_server_shutdown_failed", error.to_string())
        })?
        .map_err(|error| RuntimeHostError::new("runtime_server_shutdown_failed", error))?;
        subagent_scheduler::stop_pending_jobs(shutdown_jobs)
            .await
            .map_err(|error| RuntimeHostError::new("runtime_server_shutdown_failed", error))?;
        wait_for_runtime_work(clients, background_worker).await
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            log_runtime_server(format_args!("runtime server shutdown cleanup timed out; unfinished outcomes will be reconciled on restart"));
            Ok(())
        }
    }
}

async fn wait_for_runtime_work(
    clients: &RuntimeServerClientHub,
    background_worker: &tokio::task::JoinHandle<()>,
) -> Result<(), RuntimeHostError> {
    loop {
        let store = agent_runtime::agent_runtime_store_actor()
            .map_err(|error| RuntimeHostError::new("runtime_server_shutdown_failed", error))?;
        let jobs = store
            .list_runtime_jobs(ListRuntimeJobsRequest {
                statuses: vec![
                    RuntimeJobStatus::Queued,
                    RuntimeJobStatus::Leased,
                    RuntimeJobStatus::Running,
                ],
                limit: 1,
                ..Default::default()
            })
            .await
            .map_err(|error| RuntimeHostError::new("runtime_server_shutdown_failed", error))?;
        if clients.active_agent_runs()?.is_empty()
            && clients.active_action_count() == 0
            && jobs.is_empty()
            && background_worker.is_finished()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn agent_run_idle_shutdown_monitor(
    clients: Arc<RuntimeServerClientHub>,
    shutdown: Arc<Notify>,
) {
    let mut idle_since = None;
    let mut observed_activity_generation = clients.activity_generation();
    loop {
        tokio::time::sleep(Duration::from_millis(RUNTIME_SERVER_IDLE_CHECK_INTERVAL_MS)).await;
        let activity_generation = clients.activity_generation();
        if activity_generation != observed_activity_generation {
            idle_since = None;
            observed_activity_generation = activity_generation;
        }
        let idle = match runtime_server_is_idle(&clients).await {
            Ok(idle) => idle,
            Err(error) => {
                idle_since = None;
                log_runtime_server(format_args!(
                    "centaeris runtime server idle state check failed: {error}"
                ));
                continue;
            }
        };
        if !idle_timeout_elapsed(
            &mut idle_since,
            idle,
            Instant::now(),
            Duration::from_millis(RUNTIME_SERVER_IDLE_TIMEOUT_MS),
        ) {
            continue;
        }
        match clients.begin_idle_shutdown(activity_generation) {
            Ok(true) => {
                shutdown.notify_one();
                log_runtime_server(format_args!(
                    "centaeris runtime server exiting after idle timeout"
                ));
                return;
            }
            Ok(false) => idle_since = None,
            Err(error) => {
                idle_since = None;
                log_runtime_server(format_args!(
                    "centaeris runtime server final idle state check failed: {error}"
                ));
            }
        }
    }
}

async fn runtime_server_is_idle(
    clients: &RuntimeServerClientHub,
) -> Result<bool, RuntimeHostError> {
    if clients.has_clients_or_active_agent_runs()? {
        return Ok(false);
    }
    let store = agent_runtime::agent_runtime_store_actor()
        .map_err(|error| RuntimeHostError::new("runtime_server_idle_state_failed", error))?;
    let active_jobs = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![
                RuntimeJobStatus::Queued,
                RuntimeJobStatus::Leased,
                RuntimeJobStatus::Running,
            ],
            job_kind: None,
            session_id: None,
            branch_id: None,
            limit: 1,
            offset: 0,
        })
        .await
        .map_err(|error| RuntimeHostError::new("runtime_server_idle_state_failed", error))?;
    Ok(active_jobs.is_empty())
}

fn idle_timeout_elapsed(
    idle_since: &mut Option<Instant>,
    idle: bool,
    now: Instant,
    idle_timeout: Duration,
) -> bool {
    if !idle {
        *idle_since = None;
        return false;
    }
    let started_at = idle_since.get_or_insert(now);
    now.duration_since(*started_at) >= idle_timeout
}

fn current_endpoint() -> Result<RuntimeServerEndpoint, RuntimeHostError> {
    RuntimeServerEndpoint::for_data_root(crate::user_data_layout::desktop_data_root_dir().as_path())
}

impl RuntimeServerEndpoint {
    fn for_data_root(data_root: &Path) -> Result<Self, RuntimeHostError> {
        Self::for_data_root_and_protocol(data_root, centaeris_core::runtime::CORE_PROTOCOL_VERSION)
    }

    fn for_data_root_and_protocol(
        data_root: &Path,
        protocol_version: &str,
    ) -> Result<Self, RuntimeHostError> {
        let canonical_root = data_root.canonicalize().map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_data_root_invalid",
                format!(
                    "canonicalize runtime server data root {} failed: {error}",
                    data_root.display()
                ),
            )
        })?;
        let profile_id = crate::user_data_layout::profile_identity_for(canonical_root.as_path())
            .map_err(|error| {
                RuntimeHostError::new("runtime_server_profile_identity_failed", error)
            })?;
        let endpoint_identity = runtime_endpoint_identity(profile_id.as_str(), protocol_version);
        let runtime_dir = canonical_root.join("runtime");
        let writer_lock_path =
            runtime_dir.join(format!("{ENDPOINT_PREFIX}-{profile_id}.writer.lock"));
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\{ENDPOINT_PREFIX}-{endpoint_identity}");
        #[cfg(unix)]
        let endpoint = runtime_dir
            .join(format!("{ENDPOINT_PREFIX}-{endpoint_identity}.sock"))
            .to_string_lossy()
            .to_string();
        Ok(Self {
            endpoint,
            writer_lock_path,
        })
    }
}

impl RuntimeServerSingleton {
    fn acquire(lock_path: &Path) -> Result<Self, RuntimeHostError> {
        let parent = lock_path.parent().ok_or_else(|| {
            RuntimeHostError::new(
                "runtime_server_lock_invalid",
                format!("runtime server lock has no parent: {}", lock_path.display()),
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_lock_directory_failed",
                format!(
                    "create runtime server lock directory {} failed: {error}",
                    parent.display()
                ),
            )
        })?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|error| {
                RuntimeHostError::new(
                    "runtime_server_lock_open_failed",
                    format!(
                        "open runtime server lock {} failed: {error}",
                        lock_path.display()
                    ),
                )
            })?;
        file.try_lock_exclusive().map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_already_running",
                format!(
                    "runtime server singleton lock {} is held: {error}",
                    lock_path.display()
                ),
            )
        })?;
        Ok(Self { _file: file })
    }
}

fn runtime_endpoint_identity(profile_id: &str, protocol_version: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(profile_id.as_bytes());
    digest.update([0]);
    digest.update(protocol_version.as_bytes());
    let hex = format!("{:x}", digest.finalize());
    hex[..16].to_string()
}

#[cfg(windows)]
async fn serve_windows(
    endpoint: &str,
    state: Arc<Mutex<RuntimeHostState>>,
    clients: Arc<RuntimeServerClientHub>,
    shutdown: Arc<Notify>,
) -> Result<(), RuntimeHostError> {
    let mut server = crate::runtime_server_windows_security::create_server(endpoint, true)
        .map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_bind_failed",
                format!("create runtime server named pipe {endpoint} failed: {error}"),
            )
        })?;
    loop {
        tokio::select! {
            biased;
            _ = shutdown.notified() => return Ok(()),
            result = server.connect() => result.map_err(|error| {
                RuntimeHostError::new(
                    "runtime_server_accept_failed",
                    format!("accept runtime server named pipe {endpoint} failed: {error}"),
                )
            })?,
        }
        let connected = server;
        server = crate::runtime_server_windows_security::create_server(endpoint, false).map_err(
            |error| {
                RuntimeHostError::new(
                    "runtime_server_bind_failed",
                    format!("replace runtime server named pipe {endpoint} failed: {error}"),
                )
            },
        )?;
        let state = Arc::clone(&state);
        let clients = Arc::clone(&clients);
        let Some((event_writer, outbound)) = clients.connect()? else {
            return Ok(());
        };
        tokio::spawn(async move {
            serve_connection(connected, state, clients, event_writer, outbound).await;
        });
    }
}

#[cfg(unix)]
async fn serve_unix(
    endpoint: &str,
    state: Arc<Mutex<RuntimeHostState>>,
    clients: Arc<RuntimeServerClientHub>,
    shutdown: Arc<Notify>,
) -> Result<(), RuntimeHostError> {
    use tokio::net::UnixListener;

    let endpoint_path = Path::new(endpoint);
    if endpoint_path.exists() {
        fs::remove_file(endpoint_path).map_err(|error| {
            RuntimeHostError::new(
                "runtime_server_stale_endpoint_failed",
                format!("remove stale runtime server endpoint {endpoint} failed: {error}"),
            )
        })?;
    }
    let listener = UnixListener::bind(endpoint_path).map_err(|error| {
        RuntimeHostError::new(
            "runtime_server_bind_failed",
            format!("bind runtime server socket {endpoint} failed: {error}"),
        )
    })?;
    loop {
        let (connected, _) = tokio::select! {
            biased;
            _ = shutdown.notified() => return Ok(()),
            result = listener.accept() => result.map_err(|error| {
                RuntimeHostError::new(
                    "runtime_server_accept_failed",
                    format!("accept runtime server socket {endpoint} failed: {error}"),
                )
            })?,
        };
        let state = Arc::clone(&state);
        let clients = Arc::clone(&clients);
        let Some((event_writer, outbound)) = clients.connect()? else {
            return Ok(());
        };
        tokio::spawn(async move {
            serve_connection(connected, state, clients, event_writer, outbound).await;
        });
    }
}

async fn serve_connection<TStream>(
    stream: TStream,
    state: Arc<Mutex<RuntimeHostState>>,
    clients: Arc<RuntimeServerClientHub>,
    event_writer: EventWriter,
    mut outbound: tokio::sync::mpsc::UnboundedReceiver<String>,
) where
    TStream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut writer_task = tokio::spawn(async move {
        while let Some(frame) = outbound.recv().await {
            if writer.write_all(frame.as_bytes()).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    });
    let mut lines = BufReader::new(reader).lines();
    loop {
        let read_result = tokio::select! {
            _ = &mut writer_task => break,
            result = lines.next_line() => result,
        };
        let next_line = match read_result {
            Ok(line) => line,
            Err(error) => {
                log_runtime_server(format_args!(
                    "centaeris runtime server client read failed: {error}"
                ));
                break;
            }
        };
        let Some(line) = next_line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        match decode_jsonl_frame(line.as_str()) {
            Ok(RuntimeRpcFrame::Request(request)) => {
                spawn_request_handler(Arc::clone(&state), event_writer.clone(), request)
            }
            Ok(RuntimeRpcFrame::Response(_)) => {
                log_runtime_server(format_args!(
                    "centaeris runtime server inbound response is not accepted"
                ));
                break;
            }
            Ok(RuntimeRpcFrame::Notification(notification)) => {
                log_runtime_server(format_args!(
                    "centaeris runtime server inbound notification is not accepted: {}",
                    notification.method
                ));
                break;
            }
            Err(error) => {
                if event_writer.send_response(&error.response()).is_err() {
                    break;
                }
            }
        }
    }
    writer_task.abort();
    if let Err(error) = clients.disconnect(&event_writer) {
        log_runtime_server(format_args!(
            "centaeris runtime server client disconnect failed: {error}"
        ));
    }
}

fn spawn_request_handler(
    state: Arc<Mutex<RuntimeHostState>>,
    event_writer: EventWriter,
    request: RuntimeRpcRequest,
) {
    tokio::spawn(async move {
        let response = handle_rpc_request(state, event_writer.clone(), request).await;
        if let Err(error) = event_writer.send_response(&response) {
            log_runtime_server(format_args!(
                "centaeris runtime server response write failed: {error}"
            ));
        }
    });
}

async fn handle_rpc_request(
    state: Arc<Mutex<RuntimeHostState>>,
    event_writer: EventWriter,
    request: RuntimeRpcRequest,
) -> RuntimeRpcResponse {
    let id = request.id.clone();
    let result = async move {
        let host_request = HostCommandRequest::try_from(request)?;
        let command = crate::commands::RuntimeHostCommand::parse(&host_request.command)?;
        if command != crate::commands::RuntimeHostCommand::Initialize {
            event_writer.require_registered()?;
        }
        if event_writer.is_draining() && !allowed_while_draining(command) {
            return Err(RuntimeHostError::new(
                "runtime_server_draining",
                "Runtime is shutting down; new work is not accepted",
            ));
        }
        let _action_permit = if command == crate::commands::RuntimeHostCommand::Initialize
            || allowed_while_draining(command)
        {
            None
        } else {
            Some(event_writer.start_action()?)
        };
        if let Some(value) = handle_stateless_request_async(&host_request).await? {
            return Ok(value);
        }
        handle_request_async(state, host_request, event_writer).await
    }
    .await;
    match result {
        Ok(value) => RuntimeRpcResponse::success(id, value),
        Err(error) => RuntimeRpcResponse::failure(id, error.to_runtime_rpc_error()),
    }
}

fn allowed_while_draining(command: crate::commands::RuntimeHostCommand) -> bool {
    use crate::commands::RuntimeHostCommand;
    command.operation_kind() == crate::runtime_command_registry::RuntimeOperationKind::Read
        || matches!(
            command,
            RuntimeHostCommand::AgentRunCancel
                | RuntimeHostCommand::AgentRunDetach
                | RuntimeHostCommand::AgentRunDetachViewer
                | RuntimeHostCommand::AppExit
                | RuntimeHostCommand::RuntimeShutdown
                | RuntimeHostCommand::SidecarStop
        )
}

fn write_json_line<TValue: Serialize>(value: &TValue) -> Result<(), RuntimeHostError> {
    let encoded = crate::runtime_rpc::encode_jsonl_value(value).map_err(|error| {
        RuntimeHostError::transport(format!("runtime endpoint encode failed: {error}"))
    })?;
    print!("{encoded}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ReadPendingWriteBroken;

    impl AsyncRead for ReadPendingWriteBroken {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }
    impl AsyncWrite for ReadPendingWriteBroken {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buffer: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn broken_writer_detaches_the_client_even_when_read_half_never_closes() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let (writer, outbound) = clients.connect().unwrap().unwrap();
        writer
            .register_client(
                crate::runtime_server::RuntimeClientKind::Desktop,
                "broken-writer",
            )
            .unwrap();
        writer
            .emit("runtime/config-changed", serde_json::json!({}))
            .unwrap();
        tokio::time::timeout(
            Duration::from_millis(250),
            serve_connection(
                ReadPendingWriteBroken,
                Arc::new(Mutex::new(RuntimeHostState::default())),
                Arc::clone(&clients),
                writer,
                outbound,
            ),
        )
        .await
        .expect("broken outbound must end the connection without waiting for inbound EOF");
        assert!(!clients.has_clients_or_active_agent_runs().unwrap());
    }

    struct TestProfile {
        root: PathBuf,
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl TestProfile {
        fn enter(label: &str) -> Self {
            let root = unique_test_root(label);
            fs::create_dir_all(root.join("workspace")).unwrap();
            let previous = [
                ("CENTAERIS_DESKTOP_DATA_DIR", root.clone()),
                ("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions")),
                (
                    "CENTAERIS_AGENT_RUNTIME_DB_PATH",
                    root.join("runtime").join("runtime.sqlite3"),
                ),
            ]
            .into_iter()
            .map(|(name, value)| {
                let previous = std::env::var_os(name);
                std::env::set_var(name, value);
                (name, previous)
            })
            .collect();
            Self { root, previous }
        }
    }

    impl Drop for TestProfile {
        fn drop(&mut self) {
            for (name, previous) in &self.previous {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn service_shutdown_drains_completed_work_stops_cooperative_work_and_preserves_unknown_work() {
        let _guard = crate::message_log::test_env_mutex()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let profile = TestProfile::enter("shutdown-lifecycle");
        crate::user_data_layout::ensure_user_data_layout().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            agent_runtime::agent_runtime_store_actor().unwrap();
            let initializing_clients = Arc::new(RuntimeServerClientHub::default());
            let (initializing_writer, _outbound) = initializing_clients.connect().unwrap().unwrap();
            let initialize = || serde_json::from_value(serde_json::json!({
                "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"request":{"clientKind":"desktop", "viewerId":"initialize-before-action"}}
            })).unwrap();
            let response = handle_rpc_request(Arc::new(Mutex::new(RuntimeHostState::default())), initializing_writer.clone(), initialize()).await;
            let wire = serde_json::to_value(response).unwrap();
            assert_eq!(wire["result"]["status"], "ok", "initialize cannot require an existing registration: {wire}");
            initializing_writer.request_service_shutdown().unwrap();
            let response = handle_rpc_request(Arc::new(Mutex::new(RuntimeHostState::default())), initializing_writer, initialize()).await;
            assert_eq!(serde_json::to_value(response).unwrap()["error"]["data"]["code"], "runtime_server_draining");
            for finishes in [true, false] {
                let clients = Arc::new(RuntimeServerClientHub::default());
                let (writer, _outbound) = clients.connect().unwrap().unwrap();
                writer
                    .register_client(
                        crate::runtime_server::RuntimeClientKind::Desktop,
                        "action-owner",
                    )
                    .unwrap();
                let permit = writer.start_action().unwrap();
                let (ready, ready_wait) = tokio::sync::oneshot::channel();
                let (release, release_wait) = tokio::sync::oneshot::channel();
                let action = tokio::spawn(async move {
                    ready.send(()).unwrap();
                    release_wait.await.unwrap();
                    if !finishes {
                        std::future::pending::<()>().await;
                    }
                    drop(permit);
                });
                ready_wait.await.unwrap();
                let worker = tokio::spawn(async {});
                writer.request_service_shutdown().unwrap();
                let started = Instant::now();
                let shutdown_jobs = subagent_scheduler::ShutdownJobs::default();
                let drain = drain_service_shutdown(
                    &clients,
                    &worker,
                    &shutdown_jobs,
                    if finishes { Duration::from_secs(5) } else { Duration::from_millis(100) },
                    if finishes { Duration::from_secs(5) } else { Duration::from_millis(100) },
                );
                let (drained, ()) = tokio::join!(drain, async {
                    tokio::task::yield_now().await;
                    release.send(()).unwrap();
                });
                drained.unwrap();
                if !finishes {
                    assert!(started.elapsed() >= Duration::from_millis(200));
                }
                assert_eq!(clients.active_action_count(), u64::from(!finishes));
                action.abort();
                let _ = action.await;
            }
            for outcome in ["succeeded", "shutdown", "cancelled", "unknown"] {
                let clients = Arc::new(RuntimeServerClientHub::default());
                let (writer, _outbound) = clients.connect().unwrap().unwrap();
                writer
                    .register_client(crate::runtime_server::RuntimeClientKind::Desktop, outcome)
                    .unwrap();
                let session = crate::sessions::create(crate::sessions::SessionCreateRequest {
                    title: Some(outcome.to_string()),
                    cwd: profile.root.join("workspace").to_string_lossy().to_string(),
                })
                .unwrap();
                let run_id = format!("run-{outcome}");
                crate::message_log::append_agent_run_started(
                    &session.id,
                    &run_id,
                    &run_id,
                    "test",
                    1,
                )
                .unwrap();
                let control = centaeris_core::runtime::TurnControl::new();
                let lease = writer
                    .start_agent_run(&session.id, &run_id, &run_id, control.clone())
                    .unwrap();
                let active = writer.active_agent_run(&run_id).unwrap().unwrap();
                let run_writer = writer.for_agent_run(&lease.lease_id).unwrap();
                let task_run_id = run_id.clone();
                let task_session_id = session.id.clone();
                if outcome == "cancelled" {
                    active
                        .close_with_cancellation("user_interrupt", || Ok(()))
                        .unwrap();
                }
                let (ready, ready_wait) = tokio::sync::oneshot::channel();
                let (release, release_wait) = tokio::sync::oneshot::channel();
                let owner = tokio::spawn(async move {
                    ready.send(()).unwrap();
                    release_wait.await.unwrap();
                    if outcome == "succeeded" {
                        crate::message_log::append_assistant_message(
                            &task_session_id,
                            &task_run_id,
                            Some(&task_run_id),
                            "completed",
                            "done",
                            2,
                        )
                        .unwrap();
                        crate::message_log::append_agent_run_terminal(
                            &task_session_id,
                            &task_run_id,
                            &task_run_id,
                            "succeeded",
                            None,
                            2,
                        )
                        .unwrap();
                    } else if outcome == "unknown" {
                        std::future::pending::<()>().await;
                    } else {
                        assert!(!control.wait_for_pending_or_close().await.unwrap());
                        let current = crate::message_log::project_agent_run(&task_run_id)
                            .unwrap()
                            .unwrap();
                        agent_runtime::stop_agent_run_after_tool_closure(
                            &current,
                            active.cancellation_reason_type().unwrap().unwrap(),
                            &active.cancellation_reason().unwrap().unwrap(),
                        )
                        .unwrap();
                    }
                    run_writer.finish_agent_run(&lease.lease_id).unwrap();
                });
                ready_wait.await.unwrap();
                let worker = tokio::spawn(async {});
                writer.request_service_shutdown().unwrap();
                let started = Instant::now();
                let shutdown_jobs = subagent_scheduler::ShutdownJobs::default();
                let budget = if outcome == "unknown" { Duration::from_millis(100) } else { Duration::from_secs(5) };
                let drain = drain_service_shutdown(
                    &clients,
                    &worker,
                    &shutdown_jobs,
                    budget,
                    budget,
                );
                let (drained, ()) = tokio::join!(drain, async {
                    tokio::task::yield_now().await;
                    release.send(()).unwrap();
                });
                drained.unwrap();
                if outcome == "unknown" {
                    assert!(started.elapsed() < Duration::from_secs(2), "unresponsive owner shutdown must remain bounded");
                }
                let projected = crate::message_log::project_agent_run(&run_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    projected.status,
                    match outcome {
                        "shutdown" => "stopped",
                        "unknown" => "running",
                        other => other,
                    }
                );
                if outcome == "unknown" {
                    owner.abort();
                    let _ = owner.await;
                    agent_runtime::recover_unsealed_live_text_journals().unwrap();
                    assert_eq!(
                        crate::message_log::project_agent_run(&run_id)
                            .unwrap()
                            .unwrap()
                            .status,
                        "stopped"
                    );
                } else {
                    owner.await.unwrap();
                }
                let path = crate::user_data_layout::find_session_log_file_path(&session.id)
                    .unwrap()
                    .unwrap();
                let records = crate::message_log::read_session_document(&path)
                    .unwrap()
                    .records;
                let terminal = records.last().unwrap();
                if outcome == "shutdown" {
                    assert_eq!(terminal.payload["reasonType"], "shutdown");
                }
                if outcome == "cancelled" {
                    assert_eq!(terminal.payload["reasonType"], "cancelled");
                }
            }
        });
    }

    #[tokio::test]
    async fn service_shutdown_blocks_new_actions_but_keeps_observation_and_cancel_routes() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let (writer, _outbound) = clients.connect().unwrap().unwrap();
        writer
            .register_client(
                crate::runtime_server::RuntimeClientKind::Desktop,
                "draining-test",
            )
            .unwrap();
        writer.request_service_shutdown().unwrap();
        for method in [
            "session/prompt",
            "process_capture",
            "sidecar_start",
            "agent_dead_letter_replay",
            "_centaeris/session/answer_now",
        ] {
            let response = handle_rpc_request(
                Arc::new(Mutex::new(RuntimeHostState::default())),
                writer.clone(),
                serde_json::from_value(
                    serde_json::json!({"jsonrpc":"2.0", "id":1, "method":method, "params":{}}),
                )
                .unwrap(),
            )
            .await;
            let encoded = serde_json::to_value(response).unwrap();
            assert_eq!(
                encoded["error"]["data"]["code"], "runtime_server_draining",
                "{method}: {encoded}"
            );
        }
        for descriptor in crate::host_protocol::RUNTIME_COMMANDS {
            let command = crate::commands::RuntimeHostCommand::parse(descriptor.command).unwrap();
            if command.operation_kind()
                == crate::runtime_command_registry::RuntimeOperationKind::Read
            {
                assert!(allowed_while_draining(command), "{}", descriptor.command);
            }
        }
        for method in [
            "_centaeris/session/agent-runs/cancel",
            "_centaeris/session/agent-runs/detach",
            "app_exit",
            "runtime/shutdown",
        ] {
            assert!(allowed_while_draining(
                crate::commands::RuntimeHostCommand::parse(method).unwrap()
            ));
        }
    }

    fn unique_test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("centaeris-runtime-server-{label}-{nonce}"))
    }

    #[test]
    fn endpoint_is_protocol_scoped_but_writer_lock_is_profile_global() {
        let first_root = unique_test_root("first");
        let second_root = unique_test_root("second");
        fs::create_dir_all(first_root.join("runtime")).expect("create first root");
        fs::create_dir_all(second_root.join("runtime")).expect("create second root");
        let first =
            RuntimeServerEndpoint::for_data_root(first_root.as_path()).expect("first endpoint");
        let second =
            RuntimeServerEndpoint::for_data_root(second_root.as_path()).expect("second endpoint");
        assert_ne!(first.endpoint, second.endpoint);
        assert_ne!(first.writer_lock_path, second.writer_lock_path);
        let next_protocol = RuntimeServerEndpoint::for_data_root_and_protocol(
            first_root.as_path(),
            "centaeris.core.v2",
        )
        .expect("next protocol endpoint");
        assert_ne!(first.endpoint, next_protocol.endpoint);
        assert_eq!(first.writer_lock_path, next_protocol.writer_lock_path);
        fs::remove_dir_all(first_root).expect("remove first root");
        fs::remove_dir_all(second_root).expect("remove second root");
    }

    #[test]
    fn singleton_lock_rejects_a_second_server() {
        let root = unique_test_root("lock");
        fs::create_dir_all(root.join("runtime")).expect("create root");
        let endpoint = RuntimeServerEndpoint::for_data_root(root.as_path()).expect("endpoint");
        let first = RuntimeServerSingleton::acquire(endpoint.writer_lock_path.as_path())
            .expect("first lock");
        let error = RuntimeServerSingleton::acquire(endpoint.writer_lock_path.as_path())
            .expect_err("second lock must fail");
        assert!(error.to_string().contains("runtime_server_already_running"));
        drop(first);
        RuntimeServerSingleton::acquire(endpoint.writer_lock_path.as_path())
            .expect("lock after release");
        fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn idle_timeout_requires_one_continuous_idle_window() {
        let started_at = Instant::now();
        let timeout = Duration::from_millis(RUNTIME_SERVER_IDLE_TIMEOUT_MS);
        let mut idle_since = None;

        assert!(!idle_timeout_elapsed(
            &mut idle_since,
            true,
            started_at,
            timeout,
        ));
        assert!(!idle_timeout_elapsed(
            &mut idle_since,
            true,
            started_at + timeout - Duration::from_millis(1),
            timeout,
        ));
        assert!(!idle_timeout_elapsed(
            &mut idle_since,
            false,
            started_at + timeout,
            timeout,
        ));
        assert!(!idle_timeout_elapsed(
            &mut idle_since,
            true,
            started_at + timeout,
            timeout,
        ));
        assert!(idle_timeout_elapsed(
            &mut idle_since,
            true,
            started_at + timeout + timeout,
            timeout,
        ));
    }

    #[test]
    fn idle_shutdown_stops_accepting_new_clients() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let (event_writer, _outbound) = clients
            .connect()
            .expect("connect")
            .expect("accepted client");
        let connected_generation = clients.activity_generation();
        assert!(!clients
            .begin_idle_shutdown(connected_generation)
            .expect("client blocks idle"));
        clients
            .disconnect(&event_writer)
            .expect("disconnect client");
        assert!(!clients
            .begin_idle_shutdown(connected_generation)
            .expect("stale generation blocks idle"));
        assert!(clients
            .begin_idle_shutdown(clients.activity_generation())
            .expect("begin idle shutdown"));
        assert!(clients.connect().expect("connect while draining").is_none());
    }
}
