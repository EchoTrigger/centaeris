use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::env;
use std::io::Read;
#[cfg(unix)]
use std::io::Write;
#[cfg(any(unix, test))]
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(windows)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

const JSON_RPC_VERSION: &str = "2.0";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RUNTIME_EXE_ENV: &str = "CENTAERIS_RUNTIME_EXE";
const RUNTIME_CONNECTION_CLOSED: &str = "Runtime Server connection closed";
const RUNTIME_SERVER_STDERR_TAIL_CHARS: usize = 8_192;
const SUPPORTED_CORE_PROTOCOL_VERSION: &str = "1.0.0";
type PendingRuntimeResponses = Arc<Mutex<HashMap<String, mpsc::Sender<Result<Value, String>>>>>;

#[derive(Debug)]
pub(crate) enum RuntimeEvent {
    SessionUpdate(Value),
    RuntimeConfigChanged,
    Error(String),
}

pub(crate) struct RuntimeClient {
    write_tx: mpsc::Sender<String>,
    event_rx: mpsc::Receiver<RuntimeEvent>,
    pending: PendingRuntimeResponses,
    connected: Arc<AtomicBool>,
    expected_build_id: String,
    #[cfg(unix)]
    shutdown_stream: Option<std::os::unix::net::UnixStream>,
    #[cfg(windows)]
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    next_id: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeInitializeDescriptor {
    status: String,
    runtime: String,
    protocol: String,
    protocol_version: u32,
    capabilities: Vec<String>,
    events: Vec<String>,
    projections: Vec<String>,
    build_id: String,
    core_protocol_version: String,
    profile_id: String,
    store_id: String,
    store_schema_version: i64,
    layout_schema_version: u32,
}

pub(crate) struct RuntimeResponse {
    response_rx: mpsc::Receiver<Result<Value, String>>,
    id: String,
    method: String,
    pending: PendingRuntimeResponses,
    started_at: Instant,
}

impl RuntimeClient {
    pub(crate) fn start() -> Result<Self, String> {
        let executable = resolve_runtime_executable()?;
        let expected_build_id = executable_build_id(executable.as_path())?;

        let (write_tx, write_rx) = mpsc::channel::<String>();
        let (event_tx, event_rx) = mpsc::channel::<RuntimeEvent>();
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(AtomicBool::new(true));
        #[cfg(windows)]
        let shutdown_tx = start_windows_runtime_io(
            executable.as_path(),
            write_rx,
            write_tx.clone(),
            event_tx.clone(),
            Arc::clone(&pending),
            Arc::clone(&connected),
        )?;
        #[cfg(unix)]
        let shutdown_stream = {
            let endpoint = runtime_server_endpoint(executable.as_path())?;
            let stream = connect_or_start_runtime_server(executable.as_path(), endpoint.as_str())?;
            let shutdown_stream = stream
                .try_clone()
                .map_err(|error| format!("clone runtime shutdown connection failed: {error}"))?;
            let reader = stream
                .try_clone()
                .map_err(|error| format!("clone runtime server connection failed: {error}"))?;
            spawn_writer(
                stream,
                write_rx,
                event_tx.clone(),
                Arc::clone(&pending),
                Arc::clone(&connected),
            );
            spawn_reader(
                reader,
                write_tx.clone(),
                event_tx.clone(),
                Arc::clone(&pending),
                Arc::clone(&connected),
            );
            shutdown_stream
        };

        Ok(Self {
            write_tx,
            event_rx,
            pending,
            connected,
            expected_build_id,
            #[cfg(unix)]
            shutdown_stream: Some(shutdown_stream),
            #[cfg(windows)]
            shutdown_tx: Some(shutdown_tx),
            next_id: 1,
        })
    }

    pub(crate) fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request_with_timeout(method, params, REQUEST_TIMEOUT)
    }

    pub(crate) fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.request_async(method, params)?
            .recv_timeout(method, timeout)
    }

    pub(crate) fn request_async(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<RuntimeResponse, String> {
        if !self.is_connected() {
            return Err(RUNTIME_CONNECTION_CLOSED.to_string());
        }
        let id = format!("tui-{}", self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let (tx, rx) = mpsc::channel::<Result<Value, String>>();
        self.pending
            .lock()
            .map_err(|_| "runtime response table lock poisoned".to_string())?
            .insert(id.clone(), tx);
        if !self.is_connected() {
            let _ = self
                .pending
                .lock()
                .map(|mut pending| pending.remove(id.as_str()));
            return Err(RUNTIME_CONNECTION_CLOSED.to_string());
        }

        let frame = request_frame(id.as_str(), method, params)?;
        if let Err(error) = self.write_line(frame) {
            let _ = self
                .pending
                .lock()
                .map(|mut pending| pending.remove(id.as_str()));
            return Err(error);
        }

        Ok(RuntimeResponse {
            response_rx: rx,
            id,
            method: method.to_string(),
            pending: Arc::clone(&self.pending),
            started_at: Instant::now(),
        })
    }

    pub(crate) fn try_recv_event(&self) -> Option<RuntimeEvent> {
        match self.event_rx.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(RuntimeEvent::Error(
                "runtime event channel closed".to_string(),
            )),
        }
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub(crate) fn validate_initialize_descriptor(&self, value: &Value) -> Result<(), String> {
        validate_initialize_descriptor(value, self.expected_build_id.as_str())
    }

    fn write_line(&self, frame: Value) -> Result<(), String> {
        if !self.is_connected() {
            return Err(RUNTIME_CONNECTION_CLOSED.to_string());
        }
        let encoded = serde_json::to_string(&frame)
            .map_err(|error| format!("serialize runtime frame failed: {error}"))?;
        self.write_tx
            .send(format!("{encoded}\n"))
            .map_err(|error| format!("runtime writer is closed: {error}"))
    }

    #[cfg(test)]
    pub(crate) fn from_test_event_receiver(
        event_rx: mpsc::Receiver<RuntimeEvent>,
        connected: bool,
    ) -> Self {
        let (write_tx, _write_rx) = mpsc::channel();
        Self {
            write_tx,
            event_rx,
            pending: Arc::new(Mutex::new(HashMap::new())),
            connected: Arc::new(AtomicBool::new(connected)),
            expected_build_id: "sha256:test-runtime".to_string(),
            #[cfg(unix)]
            shutdown_stream: None,
            #[cfg(windows)]
            shutdown_tx: None,
            next_id: 1,
        }
    }
}

impl Drop for RuntimeClient {
    fn drop(&mut self) {
        invalidate_runtime_connection(
            &self.connected,
            &self.pending,
            RUNTIME_CONNECTION_CLOSED.to_string(),
        );
        #[cfg(unix)]
        if let Some(stream) = self.shutdown_stream.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        #[cfg(windows)]
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }
    }
}

fn executable_build_id(executable: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(executable)
        .map_err(|error| format!("open Runtime executable for build identity failed: {error}"))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            format!("read Runtime executable for build identity failed: {error}")
        })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn validate_initialize_descriptor(value: &Value, expected_build_id: &str) -> Result<(), String> {
    let descriptor: RuntimeInitializeDescriptor = serde_json::from_value(value.clone())
        .map_err(|error| format!("runtime initialize returned invalid descriptor: {error}"))?;
    if descriptor.status != "ok"
        || descriptor.runtime != "centaeris-runtime"
        || descriptor.protocol != "centaeris.runtime"
        || descriptor.protocol_version != 1
    {
        return Err("runtime initialize returned unsupported protocol".to_string());
    }
    if !descriptor_contains(
        descriptor.capabilities.as_slice(),
        &["json_rpc_2_over_jsonl"],
    ) || !descriptor_contains(
        descriptor.events.as_slice(),
        &["session/update", "runtime/config-changed"],
    ) || !descriptor_contains(
        descriptor.projections.as_slice(),
        &["session_event", "headless_transcript"],
    ) {
        return Err("runtime initialize returned incomplete protocol descriptor".to_string());
    }
    if descriptor.core_protocol_version != SUPPORTED_CORE_PROTOCOL_VERSION {
        return Err(format!(
            "runtime initialize returned unsupported coreProtocolVersion: {}",
            descriptor.core_protocol_version
        ));
    }
    if descriptor.profile_id.trim().is_empty() || descriptor.store_id.trim().is_empty() {
        return Err("runtime initialize returned empty profile/store identity".to_string());
    }
    if descriptor.store_schema_version <= 0 || descriptor.layout_schema_version == 0 {
        return Err("runtime initialize returned invalid storage schema version".to_string());
    }
    if descriptor.build_id != expected_build_id {
        return Err(
            "Runtime Server build does not match this TUI package; fully exit other Centaeris Desktop/TUI hosts and retry"
                .to_string(),
        );
    }
    Ok(())
}

fn descriptor_contains(values: &[String], required: &[&str]) -> bool {
    values.iter().all(|value| !value.trim().is_empty())
        && required
            .iter()
            .all(|required| values.iter().any(|value| value == required))
}

impl RuntimeResponse {
    pub(crate) fn try_recv(&self) -> Result<Option<Value>, String> {
        match self.response_rx.try_recv() {
            Ok(response) => response.map(Some),
            Err(mpsc::TryRecvError::Empty) if self.started_at.elapsed() < REQUEST_TIMEOUT => {
                Ok(None)
            }
            Err(mpsc::TryRecvError::Empty) => {
                Err(format!("runtime request timed out: {}", self.method))
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("runtime response channel closed".to_string())
            }
        }
    }

    fn recv_timeout(&self, method: &str, timeout: Duration) -> Result<Value, String> {
        match self.response_rx.recv_timeout(timeout) {
            Ok(response) => response,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("runtime request timed out: {method}"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("runtime response channel closed".to_string())
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn from_response_receiver(
        response_rx: mpsc::Receiver<Result<Value, String>>,
    ) -> Self {
        Self {
            response_rx,
            id: "test".to_string(),
            method: "test".to_string(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            started_at: Instant::now(),
        }
    }
}

impl Drop for RuntimeResponse {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(self.id.as_str());
        }
    }
}

fn fail_runtime_connection(
    connected: &AtomicBool,
    pending: &PendingRuntimeResponses,
    event_tx: &mpsc::Sender<RuntimeEvent>,
    message: String,
) {
    if invalidate_runtime_connection(connected, pending, message.as_str()) {
        let _ = event_tx.send(RuntimeEvent::Error(message));
    }
}

fn invalidate_runtime_connection(
    connected: &AtomicBool,
    pending: &PendingRuntimeResponses,
    message: impl Into<String>,
) -> bool {
    if !connected.swap(false, Ordering::AcqRel) {
        return false;
    }
    let message = message.into();
    let response_txs = pending
        .lock()
        .map(|mut pending| pending.drain().map(|(_, tx)| tx).collect::<Vec<_>>())
        .unwrap_or_default();
    for response_tx in response_txs {
        let _ = response_tx.send(Err(message.clone()));
    }
    true
}

#[cfg(unix)]
fn spawn_writer<TStream>(
    mut stream: TStream,
    write_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<RuntimeEvent>,
    pending: PendingRuntimeResponses,
    connected: Arc<AtomicBool>,
) where
    TStream: Write + Send + 'static,
{
    thread::spawn(move || {
        for line in write_rx {
            if let Err(error) = stream
                .write_all(line.as_bytes())
                .and_then(|_| stream.flush())
            {
                fail_runtime_connection(
                    &connected,
                    &pending,
                    &event_tx,
                    format!("write runtime frame failed: {error}"),
                );
                break;
            }
        }
    });
}

#[cfg(any(unix, test))]
fn spawn_reader<TStream>(
    stream: TStream,
    write_tx: mpsc::Sender<String>,
    event_tx: mpsc::Sender<RuntimeEvent>,
    pending: PendingRuntimeResponses,
    connected: Arc<AtomicBool>,
) where
    TStream: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut close_reason = RUNTIME_CONNECTION_CLOSED.to_string();
        for line_result in BufReader::new(stream).lines() {
            let line = match line_result {
                Ok(line) => line,
                Err(error) => {
                    close_reason = format!("read runtime frame failed: {error}");
                    break;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Err(error) = handle_runtime_line(line.as_str(), &write_tx, &event_tx, &pending) {
                close_reason = error;
                break;
            }
        }
        fail_runtime_connection(&connected, &pending, &event_tx, close_reason);
    });
}

#[cfg(windows)]
fn start_windows_runtime_io(
    executable: &Path,
    write_rx: mpsc::Receiver<String>,
    write_tx: mpsc::Sender<String>,
    event_tx: mpsc::Sender<RuntimeEvent>,
    pending: PendingRuntimeResponses,
    connected: Arc<AtomicBool>,
) -> Result<tokio::sync::oneshot::Sender<()>, String> {
    let executable = executable.to_path_buf();
    let endpoint = runtime_server_endpoint(executable.as_path())?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    thread::Builder::new()
        .name("centaeris-tui-runtime-io".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(format!(
                        "create Windows Runtime Server I/O runtime failed: {error}"
                    )));
                    return;
                }
            };
            let stream = match runtime.block_on(async {
                connect_or_start_runtime_server(executable.as_path(), endpoint.as_str())
            }) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            if ready_tx.send(Ok(())).is_err() {
                return;
            }
            let (async_write_tx, mut async_write_rx) = tokio::sync::mpsc::unbounded_channel();
            thread::spawn(move || {
                for line in write_rx {
                    if async_write_tx.send(line).is_err() {
                        break;
                    }
                }
            });
            runtime.block_on(async move {
                let (reader, mut writer) = tokio::io::split(stream);
                let writer_events = event_tx.clone();
                let writer_pending = Arc::clone(&pending);
                let writer_connected = Arc::clone(&connected);
                let writer_task = tokio::spawn(async move {
                    while let Some(line) = async_write_rx.recv().await {
                        if let Err(error) = writer.write_all(line.as_bytes()).await {
                            fail_runtime_connection(
                                &writer_connected,
                                &writer_pending,
                                &writer_events,
                                format!("write runtime frame failed: {error}"),
                            );
                            return;
                        }
                        if let Err(error) = writer.flush().await {
                            fail_runtime_connection(
                                &writer_connected,
                                &writer_pending,
                                &writer_events,
                                format!("flush runtime frame failed: {error}"),
                            );
                            return;
                        }
                    }
                });
                let mut lines = TokioBufReader::new(reader).lines();
                let mut close_reason = RUNTIME_CONNECTION_CLOSED.to_string();
                loop {
                    let line = match tokio::select! {
                        _ = &mut shutdown_rx => break,
                        result = lines.next_line() => result,
                    } {
                        Ok(Some(line)) => line,
                        Ok(None) => break,
                        Err(error) => {
                            close_reason = format!("read runtime frame failed: {error}");
                            break;
                        }
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Err(error) =
                        handle_runtime_line(line.as_str(), &write_tx, &event_tx, &pending)
                    {
                        close_reason = error;
                        break;
                    }
                }
                fail_runtime_connection(&connected, &pending, &event_tx, close_reason);
                writer_task.abort();
            });
        })
        .map_err(|error| format!("start Windows Runtime Server I/O worker failed: {error}"))?;
    ready_rx
        .recv()
        .map_err(|_| "Windows Runtime Server I/O worker stopped during startup".to_string())??;
    Ok(shutdown_tx)
}

fn handle_runtime_line(
    line: &str,
    write_tx: &mpsc::Sender<String>,
    event_tx: &mpsc::Sender<RuntimeEvent>,
    pending: &PendingRuntimeResponses,
) -> Result<(), String> {
    let message = serde_json::from_str::<Value>(line)
        .map_err(|error| format!("invalid runtime JSONL frame: {error}"))?;
    if message.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION) {
        return Err("runtime frame missing JSON-RPC 2.0 envelope".to_string());
    }
    let method = message.get("method").and_then(Value::as_str);
    let has_id = message.get("id").is_some();
    if let Some(method) = method {
        if has_id {
            handle_runtime_request(&message, method, write_tx)
        } else {
            handle_runtime_notification(&message, method, event_tx)
        }
    } else {
        handle_runtime_response(&message, pending)
    }
}

fn handle_runtime_request(
    message: &Value,
    method: &str,
    write_tx: &mpsc::Sender<String>,
) -> Result<(), String> {
    let id = message
        .get("id")
        .cloned()
        .ok_or_else(|| "runtime request id is required".to_string())?;
    let response = json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("runtime request method not supported by TUI: {method}")
            }
    });
    let encoded = serde_json::to_string(&response)
        .map_err(|error| format!("serialize runtime response failed: {error}"))?;
    write_tx
        .send(format!("{encoded}\n"))
        .map_err(|error| format!("runtime writer is closed: {error}"))
}

fn handle_runtime_notification(
    message: &Value,
    method: &str,
    event_tx: &mpsc::Sender<RuntimeEvent>,
) -> Result<(), String> {
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "session/update" => event_tx
            .send(RuntimeEvent::SessionUpdate(params))
            .map_err(|error| format!("runtime event receiver closed: {error}")),
        "runtime/config-changed" => {
            if !params.as_object().is_some_and(serde_json::Map::is_empty) {
                return Err("runtime/config-changed params must be an empty object".to_string());
            }
            event_tx
                .send(RuntimeEvent::RuntimeConfigChanged)
                .map_err(|error| format!("runtime event receiver closed: {error}"))
        }
        other => Err(format!("unsupported runtime notification: {other}")),
    }
}

fn handle_runtime_response(
    message: &Value,
    pending: &PendingRuntimeResponses,
) -> Result<(), String> {
    let id = message
        .get("id")
        .map(runtime_id_to_string)
        .ok_or_else(|| "runtime response id is required".to_string())?;
    let tx = pending
        .lock()
        .map_err(|_| "runtime response table lock poisoned".to_string())?
        .remove(id.as_str())
        .ok_or_else(|| format!("runtime response has no pending request: {id}"))?;
    let result = if let Some(result) = message.get("result") {
        Ok(result.clone())
    } else {
        Err(message
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("runtime request failed")
            .to_string())
    };
    tx.send(result)
        .map_err(|_| "runtime response receiver dropped".to_string())
}

fn request_frame(id: &str, method: &str, params: Value) -> Result<Value, String> {
    if method.trim().is_empty() {
        return Err("runtime method is required".to_string());
    }
    Ok(json!({
        "jsonrpc": JSON_RPC_VERSION,
        "id": id,
        "method": method,
        "params": params,
    }))
}

fn runtime_id_to_string(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn connect_or_start_runtime_server(
    executable: &Path,
    endpoint: &str,
) -> Result<RuntimeServerStream, String> {
    if let Ok(stream) = connect_runtime_server(endpoint) {
        return Ok(stream);
    }
    let mut child = Command::new(executable)
        .arg("--runtime-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "start Runtime Server failed for {}: {error}",
                executable.display()
            )
        })?;
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let mut stderr_reader = child.stderr.take().map(|stderr| {
        let tail = Arc::clone(&stderr_tail);
        thread::spawn(move || capture_runtime_stderr_tail(stderr, &tail))
    });
    let mut last_error = String::from("runtime server did not accept a connection");
    for _ in 0..50 {
        match connect_runtime_server(endpoint) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = error,
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("read Runtime Server startup status failed: {error}"))?
        {
            if let Some(reader) = stderr_reader.take() {
                let _ = reader.join();
            }
            return Err(runtime_server_exit_message(
                status.to_string().as_str(),
                &stderr_tail,
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
    let detail = sanitized_runtime_stderr_tail(&stderr_tail);
    Err(if detail.is_empty() {
        format!("Runtime Server did not become reachable at {endpoint}: {last_error}")
    } else {
        format!(
            "Runtime Server did not become reachable at {endpoint}: {last_error}; startup diagnostic: {detail}"
        )
    })
}

fn capture_runtime_stderr_tail(mut stderr: impl Read, tail: &Mutex<String>) {
    let mut buffer = [0_u8; 1_024];
    loop {
        let count = match stderr.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        let chunk = String::from_utf8_lossy(&buffer[..count]);
        if let Ok(mut tail) = tail.lock() {
            tail.push_str(chunk.as_ref());
            let excess = tail
                .chars()
                .count()
                .saturating_sub(RUNTIME_SERVER_STDERR_TAIL_CHARS);
            if excess > 0 {
                *tail = tail.chars().skip(excess).collect();
            }
        }
    }
}

fn runtime_server_exit_message(status: &str, stderr_tail: &Mutex<String>) -> String {
    let detail = sanitized_runtime_stderr_tail(stderr_tail);
    if detail.is_empty() {
        format!("Runtime Server exited before accepting a connection ({status})")
    } else {
        format!(
            "Runtime Server exited before accepting a connection ({status}); startup diagnostic: {detail}"
        )
    }
}

fn sanitized_runtime_stderr_tail(stderr_tail: &Mutex<String>) -> String {
    stderr_tail
        .lock()
        .map(|tail| sanitize_runtime_stderr(tail.as_str()))
        .unwrap_or_default()
}

fn sanitize_runtime_stderr(raw: &str) -> String {
    raw.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let lower = line.to_ascii_lowercase();
            let sensitive = [
                "authorization",
                "bearer",
                "credential",
                "password",
                "api-key",
                "api_key",
                "secret",
                "token",
                "://",
            ]
            .iter()
            .any(|marker| lower.contains(marker));
            Some(if sensitive {
                "[redacted sensitive Runtime diagnostic]".to_string()
            } else {
                line.chars().take(1_024).collect()
            })
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn runtime_server_endpoint(executable: &Path) -> Result<String, String> {
    let output = Command::new(executable)
        .arg("--runtime-server-endpoint")
        .output()
        .map_err(|error| {
            format!(
                "resolve Runtime Server endpoint failed for {}: {error}",
                executable.display()
            )
        })?;
    if !output.status.success() {
        let detail =
            sanitize_runtime_stderr(String::from_utf8_lossy(output.stderr.as_slice()).as_ref());
        return Err(format!(
            "Runtime Server endpoint discovery failed: {}",
            detail
        ));
    }
    let descriptor: Value = serde_json::from_slice(output.stdout.as_slice())
        .map_err(|error| format!("invalid Runtime Server endpoint descriptor: {error}"))?;
    descriptor
        .get("endpoint")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "Runtime Server endpoint descriptor is missing endpoint".to_string())
}

#[cfg(windows)]
type RuntimeServerStream = NamedPipeClient;

#[cfg(windows)]
fn connect_runtime_server(endpoint: &str) -> Result<RuntimeServerStream, String> {
    ClientOptions::new()
        .open(endpoint)
        .map_err(|error| format!("connect Runtime Server named pipe {endpoint} failed: {error}"))
}

#[cfg(unix)]
type RuntimeServerStream = std::os::unix::net::UnixStream;

#[cfg(unix)]
fn connect_runtime_server(endpoint: &str) -> Result<RuntimeServerStream, String> {
    std::os::unix::net::UnixStream::connect(endpoint)
        .map_err(|error| format!("connect Runtime Server socket {endpoint} failed: {error}"))
}

fn resolve_runtime_executable() -> Result<PathBuf, String> {
    if let Some(raw) = env::var_os(RUNTIME_EXE_ENV) {
        return validate_runtime_executable(PathBuf::from(raw));
    }
    let current_exe = env::current_exe().ok();
    let executable_directory = current_exe.as_deref().and_then(Path::parent);
    let cwd = env::current_dir().map_err(|error| format!("read cwd failed: {error}"))?;
    runtime_executable_candidates(executable_directory, cwd.as_path())
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!("Runtime executable not found; build packages/runtime or set {RUNTIME_EXE_ENV}")
        })
}

fn runtime_executable_candidates(exe_dir: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    let exe_name = runtime_exe_name();
    let mut candidates = Vec::with_capacity(4);
    if let Some(exe_dir) = exe_dir {
        if let Some(standalone_root) = exe_dir.parent() {
            candidates.push(standalone_root.join("current").join(exe_name));
        }
        candidates.push(exe_dir.join(exe_name));
    }
    candidates.push(cwd.join("target/release").join(exe_name));
    candidates.push(cwd.join("target/debug").join(exe_name));
    candidates
}

fn runtime_exe_name() -> &'static str {
    if cfg!(windows) {
        "centaeris-runtime.exe"
    } else {
        "centaeris-runtime"
    }
}

#[cfg(test)]
fn first_existing_candidate(exe_dir: &std::path::Path, cwd: &std::path::Path) -> Option<PathBuf> {
    runtime_executable_candidates(Some(exe_dir), cwd)
        .into_iter()
        .find(|path| path.is_file())
}

fn validate_runtime_executable(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_file() {
        return Err(format!(
            "Runtime executable is not a file: {}",
            path.display()
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failure_invalidates_client_and_completes_every_pending_request() {
        let (first_tx, first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let pending = Arc::new(Mutex::new(HashMap::from([
            ("tui-1".to_string(), first_tx),
            ("tui-2".to_string(), second_tx),
        ])));
        let connected = AtomicBool::new(true);
        let (event_tx, event_rx) = mpsc::channel();

        fail_runtime_connection(
            &connected,
            &pending,
            &event_tx,
            RUNTIME_CONNECTION_CLOSED.to_string(),
        );

        assert!(!connected.load(Ordering::Acquire));
        assert!(pending.lock().expect("pending lock").is_empty());
        assert_eq!(
            first_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("first response"),
            Err(RUNTIME_CONNECTION_CLOSED.to_string())
        );
        assert_eq!(
            second_rx
                .recv_timeout(Duration::from_millis(50))
                .expect("second response"),
            Err(RUNTIME_CONNECTION_CLOSED.to_string())
        );
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_millis(50)).expect("disconnect event"),
            RuntimeEvent::Error(message) if message == RUNTIME_CONNECTION_CLOSED
        ));

        fail_runtime_connection(
            &connected,
            &pending,
            &event_tx,
            "duplicate failure".to_string(),
        );
        assert!(
            event_rx.try_recv().is_err(),
            "disconnect must be emitted once"
        );
    }

    #[test]
    fn reader_eof_completes_pending_request_without_request_timeout() {
        let (response_tx, response_rx) = mpsc::channel();
        let pending = Arc::new(Mutex::new(HashMap::from([(
            "tui-eof".to_string(),
            response_tx,
        )])));
        let connected = Arc::new(AtomicBool::new(true));
        let (write_tx, _write_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        spawn_reader(
            std::io::Cursor::new(Vec::<u8>::new()),
            write_tx,
            event_tx,
            Arc::clone(&pending),
            Arc::clone(&connected),
        );

        assert_eq!(
            response_rx
                .recv_timeout(Duration::from_millis(250))
                .expect("EOF response"),
            Err(RUNTIME_CONNECTION_CLOSED.to_string())
        );
        assert!(matches!(
            event_rx.recv_timeout(Duration::from_millis(250)).expect("EOF event"),
            RuntimeEvent::Error(message) if message == RUNTIME_CONNECTION_CLOSED
        ));
        assert!(!connected.load(Ordering::Acquire));
        assert!(pending.lock().expect("pending lock").is_empty());
    }

    #[test]
    fn runtime_stderr_diagnostic_is_bounded_and_redacts_sensitive_lines() {
        let long_line = "x".repeat(1_200);
        let diagnostic = sanitize_runtime_stderr(
            format!(
                "ordinary startup failure\nAuthorization: Bearer private-value\nhttps://internal.invalid/path\n{long_line}"
            )
            .as_str(),
        );

        assert!(diagnostic.contains("ordinary startup failure"));
        assert_eq!(
            diagnostic
                .matches("[redacted sensitive Runtime diagnostic]")
                .count(),
            2
        );
        assert!(!diagnostic.contains("private-value"));
        assert!(!diagnostic.contains("internal.invalid"));
        assert!(!diagnostic.contains(&"x".repeat(1_025)));

        let tail = Mutex::new(String::new());
        capture_runtime_stderr_tail(std::io::Cursor::new(vec![b'x'; 9_000]), &tail);
        assert_eq!(
            tail.lock().expect("tail lock").chars().count(),
            RUNTIME_SERVER_STDERR_TAIL_CHARS
        );
    }

    #[test]
    fn initialize_descriptor_requires_exact_complete_identity() {
        let descriptor = json!({
            "status": "ok",
            "runtime": "centaeris-runtime",
            "protocol": "centaeris.runtime",
            "protocolVersion": 1,
            "capabilities": ["json_rpc_2_over_jsonl"],
            "events": ["session/update", "runtime/config-changed"],
            "projections": ["session_event", "headless_transcript"],
            "buildId": "sha256:expected",
            "coreProtocolVersion": "1.0.0",
            "profileId": "profile-1",
            "storeId": "store-1",
            "storeSchemaVersion": 1,
            "layoutSchemaVersion": 1
        });
        validate_initialize_descriptor(&descriptor, "sha256:expected")
            .expect("complete descriptor");

        let mut missing = descriptor.clone();
        missing
            .as_object_mut()
            .expect("descriptor object")
            .remove("storeId");
        assert!(validate_initialize_descriptor(&missing, "sha256:expected").is_err());

        let mut unknown = descriptor.clone();
        unknown
            .as_object_mut()
            .expect("descriptor object")
            .insert("legacyField".to_string(), Value::Bool(true));
        assert!(validate_initialize_descriptor(&unknown, "sha256:expected").is_err());

        let mismatch = validate_initialize_descriptor(&descriptor, "sha256:different")
            .expect_err("build mismatch");
        assert!(mismatch.contains("fully exit other Centaeris Desktop/TUI hosts"));

        for (field, required) in [
            ("capabilities", "json_rpc_2_over_jsonl"),
            ("events", "session/update"),
            ("projections", "headless_transcript"),
        ] {
            let mut incomplete = descriptor.clone();
            incomplete[field]
                .as_array_mut()
                .expect("descriptor list")
                .retain(|value| value.as_str() != Some(required));
            assert!(
                validate_initialize_descriptor(&incomplete, "sha256:expected").is_err(),
                "missing {field} dependency must fail"
            );
        }
    }

    #[test]
    fn executable_build_id_is_sha256_prefixed() {
        let root = temp_test_dir("build-id");
        let executable = root.join("runtime.bin");
        std::fs::write(executable.as_path(), b"abc").expect("write fixture");

        assert_eq!(
            executable_build_id(executable.as_path()).expect("build id"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn request_frame_uses_json_rpc_envelope() {
        let frame = request_frame("tui-1", "initialize", json!({})).expect("frame");
        assert_eq!(frame["jsonrpc"], "2.0");
        assert_eq!(frame["id"], "tui-1");
        assert_eq!(frame["method"], "initialize");
    }

    #[test]
    fn dropped_async_response_removes_its_pending_request() {
        let (response_tx, response_rx) = mpsc::channel();
        let (pending_tx, _pending_rx) = mpsc::channel();
        let pending = Arc::new(Mutex::new(HashMap::from([(
            "tui-1".to_string(),
            pending_tx,
        )])));
        drop(response_tx);
        {
            let _response = RuntimeResponse {
                response_rx,
                id: "tui-1".to_string(),
                method: "agent_runtime_config_get".to_string(),
                pending: Arc::clone(&pending),
                started_at: Instant::now(),
            };
        }
        assert!(pending.lock().expect("pending lock").is_empty());
    }

    #[test]
    fn runtime_resolution_prefers_exe_dir_then_target_release_then_target_debug() {
        let root = temp_test_dir("runtime-resolution");
        let exe_dir = root.join("exe");
        let release_dir = root.join("target/release");
        let debug_dir = root.join("target/debug");
        std::fs::create_dir_all(exe_dir.as_path()).expect("exe dir");
        std::fs::create_dir_all(release_dir.as_path()).expect("release dir");
        std::fs::create_dir_all(debug_dir.as_path()).expect("debug dir");
        let name = runtime_exe_name();
        let exe_runtime = exe_dir.join(name);
        let release_runtime = release_dir.join(name);
        let debug_runtime = debug_dir.join(name);
        std::fs::write(exe_runtime.as_path(), b"exe").expect("exe runtime");
        std::fs::write(release_runtime.as_path(), b"release").expect("release runtime");
        std::fs::write(debug_runtime.as_path(), b"debug").expect("debug runtime");

        assert_eq!(
            first_existing_candidate(exe_dir.as_path(), root.as_path()),
            Some(exe_runtime.clone())
        );

        std::fs::remove_file(exe_runtime.as_path()).expect("remove exe runtime");
        assert_eq!(
            first_existing_candidate(exe_dir.as_path(), root.as_path()),
            Some(release_runtime.clone())
        );

        std::fs::remove_file(release_runtime.as_path()).expect("remove release runtime");
        assert_eq!(
            first_existing_candidate(exe_dir.as_path(), root.as_path()),
            Some(debug_runtime.clone())
        );

        std::fs::remove_file(debug_runtime.as_path()).expect("remove debug runtime");
        assert_eq!(
            first_existing_candidate(exe_dir.as_path(), root.as_path()),
            None
        );
        std::fs::remove_dir_all(root.as_path()).expect("cleanup");
    }

    #[test]
    fn installed_runtime_resolution_prefers_current_release_over_stale_bin_sidecar() {
        let root = temp_test_dir("installed-runtime-resolution");
        let standalone_root = root.join("packages/standalone");
        let exe_dir = standalone_root.join("bin");
        let current_dir = standalone_root.join("current");
        std::fs::create_dir_all(exe_dir.as_path()).expect("bin dir");
        std::fs::create_dir_all(current_dir.as_path()).expect("current dir");
        let stale_runtime = exe_dir.join(runtime_exe_name());
        let current_runtime = current_dir.join(runtime_exe_name());
        std::fs::write(stale_runtime.as_path(), b"stale").expect("stale runtime");
        std::fs::write(current_runtime.as_path(), b"current").expect("current runtime");

        assert_eq!(
            first_existing_candidate(exe_dir.as_path(), root.as_path()),
            Some(current_runtime.clone())
        );
        std::fs::remove_file(current_runtime).expect("remove current runtime");
        assert_eq!(
            first_existing_candidate(exe_dir.as_path(), root.as_path()),
            Some(stale_runtime)
        );
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    fn temp_test_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "centaeris-tui-runtime-client-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.as_path()).expect("create temp dir");
        dir
    }

    #[test]
    fn unsolicited_runtime_request_is_rejected() {
        let (write_tx, write_rx) = mpsc::channel::<String>();
        let message = json!({
            "jsonrpc": "2.0",
            "id": "runtime-1",
            "method": "runtime/unsupported-request",
            "params": {}
        });

        handle_runtime_request(&message, "runtime/unsupported-request", &write_tx)
            .expect("handle request");
        let response = serde_json::from_str::<Value>(&write_rx.recv().expect("response"))
            .expect("response json");
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn runtime_config_changed_requires_exact_empty_params() {
        let (event_tx, event_rx) = mpsc::channel::<RuntimeEvent>();
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "runtime/config-changed",
            "params": {}
        });
        handle_runtime_notification(&notification, "runtime/config-changed", &event_tx)
            .expect("handle config notification");
        assert!(matches!(
            event_rx.recv().expect("event"),
            RuntimeEvent::RuntimeConfigChanged
        ));

        let invalid = json!({
            "jsonrpc": "2.0",
            "method": "runtime/config-changed",
            "params": {"banana": true}
        });
        assert!(
            handle_runtime_notification(&invalid, "runtime/config-changed", &event_tx).is_err()
        );
    }
}
