//! JSONL framing for stdio and local sockets.

use crate::app_service::{dispatch_host_command, uses_control_admission};
use crate::handlers::RuntimeHostState;
use crate::local_host::log_runtime_server;
use crate::protocol::HostCommandRequest;
use crate::runtime_rpc::{
    decode_jsonl_frame, RuntimeRpcFrame, RuntimeRpcRequest, RuntimeRpcResponse,
};
use crate::runtime_rpc_transport::{
    EventWriter, OutboundFrame, RequestPermit, RuntimeServerClientHub, RPC_INPUT_FRAME_MAX_BYTES,
};
use futures::StreamExt;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::codec::{FramedRead, LinesCodec};

#[cfg(all(test, feature = "native-job-store"))]
mod storage_tests;

/// An admitted RPC connection ready for a transport to attach a byte stream.
pub struct RpcConnection {
    state: Arc<Mutex<RuntimeHostState>>,
    clients: Arc<RuntimeServerClientHub>,
    writer: EventWriter,
    outbound: tokio::sync::mpsc::Receiver<OutboundFrame>,
}

impl RpcConnection {
    pub(crate) fn new(
        state: Arc<Mutex<RuntimeHostState>>,
        clients: Arc<RuntimeServerClientHub>,
        writer: EventWriter,
        outbound: tokio::sync::mpsc::Receiver<OutboundFrame>,
    ) -> Self {
        Self {
            state,
            clients,
            writer,
            outbound,
        }
    }

    /// Serves public JSON-RPC frames until the client disconnects.
    pub async fn serve(self, stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static) {
        serve_connection(stream, self.state, self.clients, self.writer, self.outbound).await;
    }
}

async fn serve_connection<TStream>(
    stream: TStream,
    state: Arc<Mutex<RuntimeHostState>>,
    clients: Arc<RuntimeServerClientHub>,
    event_writer: EventWriter,
    outbound: tokio::sync::mpsc::Receiver<OutboundFrame>,
) where
    TStream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    serve_connection_with_dispatch(
        stream,
        state,
        clients,
        event_writer,
        outbound,
        spawn_request_handler,
    )
    .await;
}

async fn serve_connection_with_dispatch<TStream>(
    stream: TStream,
    state: Arc<Mutex<RuntimeHostState>>,
    clients: Arc<RuntimeServerClientHub>,
    event_writer: EventWriter,
    mut outbound: tokio::sync::mpsc::Receiver<OutboundFrame>,
    dispatch: impl Fn(Arc<Mutex<RuntimeHostState>>, EventWriter, RuntimeRpcRequest, RequestPermit)
        + Send,
) where
    TStream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Ok(disconnected) = event_writer.disconnect_token() else {
        return;
    };
    let (reader, mut writer) = tokio::io::split(stream);
    let mut writer_task = tokio::spawn(async move {
        while let Some(frame) = outbound.recv().await {
            if writer.write_all(frame.as_bytes()).await.is_err() || writer.flush().await.is_err() {
                break;
            }
        }
    });
    let mut lines = FramedRead::new(
        reader,
        LinesCodec::new_with_max_length(RPC_INPUT_FRAME_MAX_BYTES),
    );
    loop {
        let read_result = tokio::select! {
            _ = &mut writer_task => break,
            _ = disconnected.cancelled() => break,
            result = lines.next() => result,
        };
        let next_line = match read_result {
            Some(Ok(line)) => Some(line),
            None => None,
            Some(Err(error)) => {
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
                let control = crate::commands::RuntimeHostCommand::parse(&request.method)
                    .ok()
                    .is_some_and(uses_control_admission);
                let permit = match event_writer.request_permit(control) {
                    Ok(permit) => permit,
                    Err(error) => {
                        log_runtime_server(format_args!(
                            "runtime request dispatch rejected: {error}"
                        ));
                        break;
                    }
                };
                dispatch(Arc::clone(&state), event_writer.clone(), request, permit)
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
    if !writer_task.is_finished() {
        writer_task.abort();
        let _ = writer_task.await;
    }
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
    permit: RequestPermit,
) {
    tokio::spawn(async move {
        let _permit = permit;
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
    let result = match HostCommandRequest::try_from(request) {
        Ok(host_request) => dispatch_host_command(state, event_writer, host_request).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => RuntimeRpcResponse::success(id, value),
        Err(error) => RuntimeRpcResponse::failure(id, crate::errors::to_runtime_rpc_error(&error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime;
    use crate::app_service::{allowed_while_draining, LocalAppService};
    use crate::local_host::{drain_service_shutdown, RuntimeServerSingleton};
    use crate::subagent_scheduler;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};
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
    async fn slow_reads_fill_ordinary_capacity_without_consuming_cancel_headroom() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio_util::sync::CancellationToken;

        let clients = Arc::new(RuntimeServerClientHub::default());
        let (writer, outbound) = clients.connect().unwrap().unwrap();
        writer
            .register_client(
                crate::runtime_server::RuntimeClientKind::Desktop,
                "control-read-test",
            )
            .unwrap();
        let (server, mut peer) = tokio::io::duplex(4096);
        let (observed, mut observations) = tokio::sync::mpsc::channel(9);
        let release_reads = CancellationToken::new();
        let _release_on_failure = release_reads.clone().drop_guard();
        let release_for_dispatch = release_reads.clone();
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let handlers_for_dispatch = handlers.clone();
        // Isolate business handlers: exercise real framing, command classification,
        // admission and response writing while keeping reads deterministically active.
        let serving = tokio::spawn(serve_connection_with_dispatch(
            server,
            Arc::new(Mutex::new(RuntimeHostState::default())),
            clients,
            writer,
            outbound,
            move |_state, writer, request, permit| {
                let observed = observed.clone();
                let release = release_for_dispatch.clone();
                let handler = tokio::spawn(async move {
                    let _permit = permit;
                    let is_read = request.method == "session/list";
                    assert!(is_read || request.method == "_centaeris/session/agent-runs/cancel");
                    observed.send(request.method).await.unwrap();
                    drop(observed);
                    if is_read {
                        release.cancelled().await;
                    }
                    writer
                        .send_response(&RuntimeRpcResponse::success(
                            request.id,
                            serde_json::Value::Null,
                        ))
                        .unwrap();
                });
                handlers_for_dispatch.lock().unwrap().push(handler);
            },
        ));
        for id in 0..8 {
            let request =
                serde_json::json!({"jsonrpc":"2.0", "id":id, "method":"session/list", "params":{}});
            peer.write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let observed = tokio::time::timeout(Duration::from_secs(1), observations.recv())
                .await
                .unwrap();
            assert_eq!(
                observed.as_deref(),
                Some("session/list"),
                "all eight reads must enter before cancellation"
            );
        }
        let request = serde_json::json!({"jsonrpc":"2.0", "id":99, "method":"_centaeris/session/agent-runs/cancel", "params":{"agentRunId":"run"}});
        peer.write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), observations.recv())
                .await
                .unwrap()
                .as_deref(),
            Some("_centaeris/session/agent-runs/cancel")
        );
        let mut peer = BufReader::new(peer);
        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(1), peer.read_line(&mut response))
            .await
            .unwrap()
            .unwrap();
        let RuntimeRpcFrame::Response(response) = decode_jsonl_frame(&response).unwrap() else {
            panic!("cancel response expected")
        };
        assert_eq!(
            serde_json::to_value(&response.id).unwrap(),
            serde_json::json!(99)
        );
        assert_eq!(response.result, Some(serde_json::Value::Null));
        release_reads.cancel();
        // Drain all previously accepted reads before disconnecting their writer.
        for _ in 0..8 {
            let mut response = String::new();
            tokio::time::timeout(Duration::from_secs(1), peer.read_line(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                decode_jsonl_frame(&response).unwrap(),
                RuntimeRpcFrame::Response(_)
            ));
        }
        drop(peer);
        tokio::time::timeout(Duration::from_secs(1), serving)
            .await
            .unwrap()
            .unwrap();
        let handlers = std::mem::take(&mut *handlers.lock().unwrap());
        for handler in handlers {
            handler.await.unwrap();
        }
    }

    #[tokio::test]
    async fn saturated_request_admission_closes_and_capacity_recovers() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let (writer, outbound) = clients.connect().unwrap().unwrap();
        let permits = (0..8)
            .map(|_| writer.request_permit(false).unwrap())
            .collect::<Vec<_>>();
        let (server, mut peer) = tokio::io::duplex(4096);
        let serving = tokio::spawn(serve_connection(
            server,
            Arc::new(Mutex::new(RuntimeHostState::default())),
            clients.clone(),
            writer,
            outbound,
        ));
        peer.write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"session/prompt\",\"params\":{}}\n",
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), serving)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(clients.active_action_count(), 0);
        drop(permits);
        let (replacement, _events) = clients.connect().unwrap().unwrap();
        assert!(replacement.request_permit(false).is_ok());
    }

    #[tokio::test]
    async fn nonreading_client_disconnect_keeps_run_and_fast_client_alive() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let (slow, outbound) = clients.connect().unwrap().unwrap();
        let (fast, mut fast_events) = clients.connect().unwrap().unwrap();
        slow.register_client(
            crate::runtime_server::RuntimeClientKind::Desktop,
            "slow-write",
        )
        .unwrap();
        fast.register_client(crate::runtime_server::RuntimeClientKind::Tui, "fast-write")
            .unwrap();
        let lease = slow
            .start_agent_run(
                "slow-session",
                "slow-run",
                "slow-turn",
                centaeris_core::runtime::TurnControl::new(),
            )
            .unwrap();
        let (server, _peer) = tokio::io::duplex(1);
        let serving = tokio::spawn(serve_connection(
            server,
            Arc::new(Mutex::new(RuntimeHostState::default())),
            clients.clone(),
            slow,
            outbound,
        ));
        for _ in 0..130 {
            clients
                .broadcaster()
                .emit("runtime.progress", serde_json::json!({}))
                .unwrap();
            fast_events.recv().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), serving)
            .await
            .unwrap()
            .unwrap();
        assert!(fast.require_registered().is_ok());
        let active = fast.active_agent_run("slow-run").unwrap().unwrap();
        assert_eq!(active.lease, lease);
        assert_eq!(active.cancellation_reason().unwrap(), None);
        fast.finish_agent_run(&lease.lease_id).unwrap();
    }

    #[tokio::test]
    async fn unterminated_oversized_input_closes_connection() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let (writer, outbound) = clients.connect().unwrap().unwrap();
        let (server, mut peer) = tokio::io::duplex(64 * 1024);
        let serving = tokio::spawn(serve_connection(
            server,
            Arc::new(Mutex::new(RuntimeHostState::default())),
            clients.clone(),
            writer,
            outbound,
        ));
        let sending = tokio::spawn(async move {
            let _ = peer.write_all(&vec![b'x'; 16 * 1024 * 1024 + 1]).await;
            std::future::pending::<()>().await;
        });
        tokio::time::timeout(Duration::from_secs(3), serving)
            .await
            .expect("oversized frame must terminate without LF")
            .unwrap();
        assert!(!clients.has_clients_or_active_agent_runs().unwrap());
        sending.abort();
    }

    #[tokio::test]
    async fn broken_writer_detaches_the_client_even_when_read_half_never_closes() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let app = LocalAppService::new(Arc::clone(&clients));
        let connection = app.accept_rpc().unwrap().unwrap();
        connection
            .writer
            .register_client(
                crate::runtime_server::RuntimeClientKind::Desktop,
                "broken-writer",
            )
            .unwrap();
        connection
            .writer
            .emit("runtime/config-changed", serde_json::json!({}))
            .unwrap();
        tokio::time::timeout(
            Duration::from_millis(250),
            connection.serve(ReadPendingWriteBroken),
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
            let root = root.canonicalize().unwrap();
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
    fn session_fork_rpc_keeps_the_prefix_and_recovers_a_published_child() {
        let _guard = crate::message_log::test_env_mutex().lock().unwrap();
        let profile = TestProfile::enter("native-session-fork");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(check_session_fork_rpc(&profile));
    }

    async fn check_session_fork_rpc(profile: &TestProfile) {
        use crate::message_log;
        use crate::operation_receipts;
        use crate::session_files::SessionFiles;
        use centaeris_core::runtime::contracts::current_timestamp_ms;
        use centaeris_core::session::recovery::recover_session_records;
        use serde_json::{json, Value};
        use tokio::io::{AsyncBufReadExt, BufReader};

        const METHOD: &str = "_centaeris/session/fork";
        crate::user_data_layout::ensure_user_data_layout().unwrap();
        crate::sqlite_store::SqliteRuntimeStore::new(
            crate::user_data_layout::runtime_store_db_path(),
        )
        .unwrap();
        let sessions = SessionFiles::new(profile.root.join("sessions"));
        let now = current_timestamp_ms();
        let source = sessions
            .create(
                Some("source"),
                profile.root.join("workspace").to_str().unwrap(),
                now,
            )
            .unwrap();
        let source_path = profile.root.join(&source.session_path);
        let commit_turn = |operation_id: &str, user_text: &str, reply: &str, at_ms| {
            let run = operation_receipts::deterministic_identity(
                "agent-run-",
                "session/prompt",
                operation_id,
            );
            let turn =
                operation_receipts::deterministic_identity("turn-", "session/prompt", operation_id);
            operation_receipts::write_prompt(
                operation_id,
                "fixture-digest".into(),
                json!({"sessionId":source.id, "agentRunId":run, "turnId":turn, "streamItems":[]}),
                true,
            )
            .unwrap();
            message_log::append_agent_run_started(&source.id, &turn, &run, user_text, at_ms)
                .unwrap();
            message_log::append_assistant_message(
                &source.id,
                &turn,
                Some(&run),
                reply,
                "done",
                at_ms + 1,
            )
            .unwrap();
            message_log::append_agent_run_terminal(
                &source.id,
                &turn,
                &run,
                "succeeded",
                None,
                at_ms + 2,
            )
            .unwrap();
            turn
        };
        let first = commit_turn("first", "first prompt", "first reply", now + 1);
        let middle = commit_turn("middle", "middle prompt", "middle reply", now + 4);
        let latest = commit_turn("latest", "latest prompt", "latest reply", now + 7);
        let original = fs::read(&source_path).unwrap();
        let request = |operation: &str, target: &str| {
            json!({"request": {
                "operationId": operation, "sessionId": source.id,
                "beforeTurnId": target,
            }})
        };
        let recover_only = |operation: &str, target: &str| {
            let mut params = request(operation, target);
            params["recoverOnly"] = json!(true);
            params
        };

        let clients = Arc::new(RuntimeServerClientHub::default());
        let (writer, outbound) = clients.connect().unwrap().unwrap();
        let controller = writer.clone();
        let (server, peer) = tokio::io::duplex(8192);
        let serving = tokio::spawn(serve_connection(
            server,
            Arc::new(Mutex::new(RuntimeHostState::default())),
            clients,
            writer,
            outbound,
        ));
        let mut peer = BufReader::new(peer);
        async fn call(
            peer: &mut BufReader<tokio::io::DuplexStream>,
            id: u64,
            method: &str,
            params: Value,
        ) -> Value {
            let request = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
            peer.write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(10), peer.read_line(&mut line))
                .await
                .expect("fork RPC response timeout")
                .unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(response["id"], id, "{response}");
            response
        }
        let initialize = call(
            &mut peer,
            1,
            "initialize",
            json!({"request":{
                "clientKind":"desktop", "viewerId":"native-session-fork"
            }}),
        )
        .await;
        assert!(
            initialize["result"]["capabilities"]
                .as_array()
                .is_some_and(|capabilities| { capabilities.contains(&json!("session_fork")) }),
            "{initialize}"
        );
        let mut invalid = request("fork-operation", &latest);
        invalid["request"]["draft"] = json!("may not be supplied");
        let invalid = call(&mut peer, 2, METHOD, invalid).await;
        assert_eq!(
            invalid["error"]["data"]["code"], "invalid_request",
            "{invalid}"
        );
        let missing = call(
            &mut peer,
            3,
            METHOD,
            request("fork-missing", "missing-turn"),
        )
        .await;
        assert_eq!(
            missing["error"]["data"]["code"], "session_fork_stale",
            "{missing}"
        );
        let lease = controller
            .start_agent_run(
                &source.id,
                "active-lease",
                "pending-turn",
                centaeris_core::runtime::TurnControl::new(),
            )
            .unwrap();
        let active = call(&mut peer, 4, METHOD, request("fork-active", &middle)).await;
        assert_eq!(
            active["error"]["data"]["code"], "session_fork_active",
            "{active}"
        );
        controller.finish_agent_run(&lease.lease_id).unwrap();
        let unpublished = call(
            &mut peer,
            5,
            METHOD,
            recover_only("fork-operation", &latest),
        )
        .await;
        assert_eq!(
            unpublished["error"]["data"]["code"], "session_fork_stale",
            "{unpublished}"
        );
        assert!(operation_receipts::read(METHOD, "fork-operation")
            .unwrap()
            .is_none());
        assert_eq!(sessions.list().unwrap().len(), 1);

        let cutoffs: [(u64, &str, &str, &[&str]); 3] = [
            (6, "fork-first", &first, &[]),
            (7, "fork-middle", &middle, &["first prompt", "first reply"]),
            (
                8,
                "fork-operation",
                &latest,
                &[
                    "first prompt",
                    "first reply",
                    "middle prompt",
                    "middle reply",
                ],
            ),
        ];
        for (id, operation, turn, expected_messages) in cutoffs {
            let response = call(&mut peer, id, METHOD, request(operation, turn)).await;
            let child_id = response["result"]["sessionId"]
                .as_str()
                .expect("fork child id");
            assert_ne!(child_id, source.id);
            assert_eq!(
                response["result"],
                json!({
                    "operationId": operation, "sourceSessionId": source.id,
                    "sourceTurnId": turn, "sessionId": child_id,
                })
            );
            let child = sessions.get(child_id).unwrap();
            let child_path = profile.root.join(&child.session_path);
            let document = message_log::read_session_document(&child_path).unwrap();
            let restored = recover_session_records(child_id, &document.records).unwrap();
            let messages = restored
                .snapshot
                .messages
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>();
            assert_eq!(messages, expected_messages);
        }
        assert_eq!(fs::read(&source_path).unwrap(), original);
        let published = call(
            &mut peer,
            9,
            METHOD,
            recover_only("fork-operation", &latest),
        )
        .await;
        let expected = &published["result"];
        let child = sessions
            .get(expected["sessionId"].as_str().unwrap())
            .unwrap();
        let child_path = profile.root.join(&child.session_path);
        let document = message_log::read_session_document(&child_path).unwrap();

        let stored = operation_receipts::read(METHOD, "fork-operation")
            .unwrap()
            .unwrap();
        operation_receipts::write(
            METHOD,
            "fork-operation",
            stored.request_digest,
            json!({"state":"prepared", "createdAtMs":document.manifest.created_at_ms}),
        )
        .unwrap();
        let messages = message_log::project_chat_messages(&source.id).unwrap();
        let latest_user = messages
            .iter()
            .rfind(|message| message.role == "user")
            .unwrap();
        message_log::rewrite_last_user_input(message_log::RewriteLastUserInputRequest {
            session_id: &source.id,
            target_chat_message_id: &latest_user.id,
            expected_tail_chat_message_id: &messages.last().unwrap().id,
            new_turn_id: "replacement",
            new_agent_run_id: "replacement-run",
            new_content: "updated since publication",
            reason: "rewrite_last_user_input",
            at_ms: now + 10,
        })
        .unwrap();
        message_log::append_assistant_message(
            &source.id,
            "replacement",
            Some("replacement-run"),
            "new reply",
            "done",
            now + 12,
        )
        .unwrap();
        message_log::append_agent_run_terminal(
            &source.id,
            "replacement",
            "replacement-run",
            "succeeded",
            None,
            now + 13,
        )
        .unwrap();
        let changed_source = fs::read(&source_path).unwrap();
        let stale = call(&mut peer, 10, METHOD, request("fork-stale", &latest)).await;
        assert_eq!(
            stale["error"]["data"]["code"], "session_fork_stale",
            "{stale}"
        );
        let conflict = call(&mut peer, 11, METHOD, request("fork-operation", &middle)).await;
        assert_eq!(
            conflict["error"]["data"]["code"], "operation_id_conflict",
            "{conflict}"
        );
        let recovered = call(
            &mut peer,
            12,
            METHOD,
            recover_only("fork-operation", &latest),
        )
        .await;
        assert_eq!(&recovered["result"], expected, "{recovered}");
        let replay = call(&mut peer, 13, METHOD, request("fork-operation", &latest)).await;
        assert_eq!(&replay["result"], expected, "{replay}");
        assert_eq!(sessions.list().unwrap().len(), 4);
        assert_eq!(fs::read(source_path).unwrap(), changed_source);
        drop(peer);
        serving.await.unwrap();
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
                #[cfg(feature = "native-job-store")]
                if !finishes { assert!(drained.unwrap_err().to_string().contains("runtime_server_business_shutdown_unconfirmed")); } else { drained.unwrap(); }
                #[cfg(not(feature = "native-job-store"))]
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
                #[cfg(feature = "native-job-store")]
                if outcome == "unknown" { assert!(drained.unwrap_err().to_string().contains("runtime_server_business_shutdown_unconfirmed")); } else { drained.unwrap(); }
                #[cfg(not(feature = "native-job-store"))]
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
        let app = LocalAppService::new(clients);
        for method in [
            "session/prompt",
            "process_capture",
            "sidecar_start",
            "agent_dead_letter_replay",
            "_centaeris/session/answer_now",
        ] {
            let request = HostCommandRequest {
                command: method.into(),
                payload: serde_json::json!({}),
            };
            let error = app.request(writer.clone(), request).await.unwrap_err();
            assert_eq!(error.code(), "runtime_server_draining", "{method}: {error}");

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
                assert!(!uses_control_admission(command), "{}", descriptor.command);
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
    fn singleton_lock_rejects_a_second_server() {
        let root = unique_test_root("lock");
        fs::create_dir_all(root.join("runtime")).expect("create root");
        let lock_path = crate::local_host::writer_lock_path(&root).expect("profile lock path");
        let first = RuntimeServerSingleton::acquire(&lock_path).expect("first lock");
        let error = RuntimeServerSingleton::acquire(&lock_path).expect_err("second lock must fail");
        assert!(error.to_string().contains("runtime_server_already_running"));
        drop(first);
        RuntimeServerSingleton::acquire(&lock_path).expect("lock after release");
        fs::remove_dir_all(root).expect("remove root");
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
