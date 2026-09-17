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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

const ENDPOINT_PREFIX: &str = "centaeris-runtime";
const RUNTIME_SERVER_IDLE_CHECK_INTERVAL_MS: u64 = 1_000;
const RUNTIME_SERVER_IDLE_TIMEOUT_MS: u64 = 5_000;

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
    let _singleton = RuntimeServerSingleton::acquire(endpoint.writer_lock_path.as_path())?;
    crate::user_data_layout::ensure_user_data_layout()
        .map_err(|error| RuntimeHostError::new("user_data_layout_init_failed", error))?;
    crate::system_skills_deployment::deploy();
    agent_runtime::agent_runtime_store_actor()
        .map_err(|error| RuntimeHostError::new("runtime_server_store_init_failed", error))?;
    agent_runtime::recover_unsealed_live_text_journals().map_err(|error| {
        RuntimeHostError::new("runtime_server_live_text_recovery_failed", error)
    })?;
    let state = Arc::new(Mutex::new(RuntimeHostState::default()));
    let clients = Arc::new(RuntimeServerClientHub::default());
    let background_worker = tokio::spawn(subagent_scheduler::run_background_worker(
        clients.broadcaster(),
    ));
    let shutdown = Arc::new(Notify::new());
    let idle_monitor = tokio::spawn(agent_run_idle_shutdown_monitor(
        Arc::clone(&clients),
        Arc::clone(&shutdown),
    ));
    #[cfg(windows)]
    let result = serve_windows(endpoint.endpoint.as_str(), state, clients, shutdown).await;
    #[cfg(unix)]
    let result = serve_unix(endpoint.endpoint.as_str(), state, clients, shutdown).await;
    background_worker.abort();
    idle_monitor.abort();
    result
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
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = outbound.recv().await {
            if writer.write_all(frame.as_bytes()).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    });
    let mut lines = BufReader::new(reader).lines();
    loop {
        let next_line = match lines.next_line().await {
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
    match clients.disconnect(&event_writer) {
        Ok(dispositions) => {
            if let Err(error) =
                agent_runtime::interrupt_owner_agent_runs(event_writer.clone(), dispositions)
            {
                log_runtime_server(format_args!(
                    "centaeris runtime server owner exit interrupt failed: {error}"
                ));
            }
        }
        Err(error) => {
            log_runtime_server(format_args!(
                "centaeris runtime server client disconnect failed: {error}"
            ));
        }
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
        if host_request.command != "initialize" {
            event_writer.require_registered()?;
        }
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
