use centaeris_core::tool::limits::MAX_TOOL_CONTRACT_BYTES;
use futures::{stream::BoxStream, StreamExt};
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, JsonRpcMessage, ServerJsonRpcMessage};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use rmcp::transport::Transport;
use rmcp::RoleClient;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

fn oversize() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "MCP response exceeds 4 MiB")
}

/// Bounds each raw stdio line before the SDK buffers or decodes JSON.
struct BoundedLines<R> {
    inner: R,
    bytes: usize,
    failed: bool,
}

impl<R> BoundedLines<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes: 0,
            failed: false,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BoundedLines<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.failed {
            return Poll::Ready(Err(oversize()));
        }
        let mut scratch = [0_u8; 8192];
        let length = output.remaining().min(scratch.len());
        let mut input = ReadBuf::new(&mut scratch[..length]);
        match Pin::new(&mut self.inner).poll_read(cx, &mut input) {
            Poll::Ready(Ok(())) => {
                for byte in input.filled() {
                    if *byte == b'\n' {
                        self.bytes = 0;
                    } else {
                        self.bytes += 1;
                        if self.bytes > MAX_TOOL_CONTRACT_BYTES {
                            self.failed = true;
                            return Poll::Ready(Err(oversize()));
                        }
                    }
                }
                output.put_slice(input.filled());
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

pub struct BoundedStdioTransport {
    child: Child,
    transport: AsyncRwTransport<RoleClient, BoundedLines<ChildStdout>, ChildStdin>,
}

/// Spawns the already configured command with bounded stdout and kill-on-drop cleanup.
pub fn bounded_stdio_transport(mut command: Command) -> io::Result<BoundedStdioTransport> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("MCP stdout missing"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("MCP stdin missing"))?;
    Ok(BoundedStdioTransport {
        child,
        transport: AsyncRwTransport::new_client(BoundedLines::new(stdout), stdin),
    })
}

impl Transport<RoleClient> for BoundedStdioTransport {
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        self.transport.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        self.transport.receive().await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.transport.close().await?;
        match tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait()).await {
            Ok(result) => {
                result?;
            }
            Err(_) => self.child.kill().await?,
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct BoundedHttpClient(pub reqwest::Client);

type HttpError = StreamableHttpError<reqwest::Error>;

impl StreamableHttpClient for BoundedHttpClient {
    type Error = reqwest::Error;

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<sse_stream::Sse, sse_stream::Error>>, HttpError> {
        self.0
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                MAX_TOOL_CONTRACT_BYTES,
            )
            .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        self.0
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let mut request = self
            .0
            .post(uri.as_ref())
            .header("accept", "application/json, text/event-stream");
        if let Some(token) = auth_header {
            request = request.bearer_auth(token);
        }
        let session_attached = session_id.is_some();
        if let Some(session) = session_id {
            request = request.header("mcp-session-id", session.as_ref());
        }
        for (name, value) in custom_headers {
            if matches!(name.as_str(), "accept" | "mcp-session-id" | "last-event-id") {
                return Err(StreamableHttpError::ReservedHeaderConflict(
                    name.to_string(),
                ));
            }
            request = request.header(name, value);
        }
        let response = request
            .json(&message)
            .send()
            .await
            .map_err(StreamableHttpError::Client)?;
        let status = response.status();
        if matches!(
            status,
            reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == reqwest::StatusCode::NOT_FOUND && session_attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TOOL_CONTRACT_BYTES as u64)
        {
            return Err(StreamableHttpError::UnexpectedServerResponse(
                "MCP response exceeds 4 MiB".into(),
            ));
        }
        let mut remaining = Some(MAX_TOOL_CONTRACT_BYTES);
        let stream = response.bytes_stream().map(move |chunk| {
            let chunk = chunk.map_err(io::Error::other)?;
            remaining = remaining.and_then(|left| left.checked_sub(chunk.len()));
            remaining.ok_or_else(oversize)?;
            Ok::<_, io::Error>(chunk)
        });
        if status.is_success()
            && content_type
                .as_deref()
                .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            return Ok(StreamableHttpPostResponse::Sse(
                sse_stream::SseStream::from_bytes_stream(stream).boxed(),
                session,
            ));
        }
        let mut body = Vec::new();
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                StreamableHttpError::UnexpectedServerResponse(
                    "MCP response read failed or exceeds 4 MiB".into(),
                )
            })?;
            body.extend_from_slice(&chunk);
        }
        if status.is_success()
            && body.is_empty()
            && !matches!(message, ClientJsonRpcMessage::Request(_))
        {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if content_type
            .as_deref()
            .is_some_and(|v| v.starts_with("application/json"))
        {
            let decoded = serde_json::from_slice::<ServerJsonRpcMessage>(&body).map_err(|_| {
                StreamableHttpError::UnexpectedServerResponse("invalid MCP JSON response".into())
            })?;
            if status.is_success() || matches!(decoded, JsonRpcMessage::Error(_)) {
                return Ok(StreamableHttpPostResponse::Json(decoded, session));
            }
        }
        Err(StreamableHttpError::UnexpectedServerResponse(
            "unexpected MCP HTTP response".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn stdio_lines_bound_before_decode_and_reset_at_newline() {
        let bytes = vec![b'x'; MAX_TOOL_CONTRACT_BYTES];
        let mut reader = BoundedLines::new(bytes.as_slice());
        let mut output = Vec::new();
        reader.read_to_end(&mut output).await.expect("exact limit");
        let bytes = [bytes.as_slice(), b"\n", bytes.as_slice()].concat();
        let mut reader = BoundedLines::new(bytes.as_slice());
        reader
            .read_to_end(&mut Vec::new())
            .await
            .expect("separate lines");
        let bytes = vec![b'x'; MAX_TOOL_CONTRACT_BYTES + 1];
        let mut reader = BoundedLines::new(bytes.as_slice());
        assert_eq!(
            reader
                .read_to_end(&mut Vec::new())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn stdio_child_fixture() {
        if std::env::var_os("CENTAERIS_MCP_OVERSIZE_FIXTURE").is_some() {
            use std::io::Write;
            let _ = io::stdout().write_all(&vec![b'x'; MAX_TOOL_CONTRACT_BYTES + 1]);
            let _ = io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }

    #[tokio::test]
    async fn actual_stdio_transport_rejects_oversize_unterminated_child_output() {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "transport::tests::stdio_child_fixture",
                "--nocapture",
            ])
            .env("CENTAERIS_MCP_OVERSIZE_FIXTURE", "1");
        let mut transport = bounded_stdio_transport(command).expect("spawn fixture");
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), transport.receive())
                .await
                .expect("bounded read")
                .is_none()
        );
        transport.close().await.expect("kill oversized child");
        assert!(transport.child.try_wait().expect("child status").is_some());
    }

    #[tokio::test]
    async fn http_json_error_and_sse_raw_bodies_are_bounded() {
        use axum::{
            body::{Body, Bytes},
            http::{Response, StatusCode},
            routing::any,
            Router,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");
        let router = Router::new().route(
            "/{kind}",
            any(
                |axum::extract::Path(kind): axum::extract::Path<String>| async move {
                    let content_type = if kind.contains("sse") {
                        "text/event-stream"
                    } else {
                        "application/json"
                    };
                    let status = if kind == "error" {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::OK
                    };
                    let chunk = Bytes::from(vec![b'x'; 8192]);
                    // No Content-Length: the byte counter must reject while streaming, before JSON/SSE parsing.
                    let body = Body::from_stream(futures::stream::iter(
                        (0..514).map(move |_| Ok::<_, io::Error>(chunk.clone())),
                    ));
                    Response::builder()
                        .status(status)
                        .header("content-type", content_type)
                        .body(body)
                        .expect("response")
                },
            ),
        );
        let server =
            tokio::spawn(async move { axum::serve(listener, router).await.expect("serve") });
        let client = BoundedHttpClient(reqwest::Client::new());
        for kind in ["json", "error", "sse"] {
            let message = ClientJsonRpcMessage::request(
                rmcp::model::ClientRequest::PingRequest(Default::default()),
                rmcp::model::RequestId::Number(1),
            );
            let result = client
                .post_message(
                    format!("http://{address}/{kind}").into(),
                    message,
                    None,
                    None,
                    HashMap::new(),
                )
                .await;
            if kind == "sse" {
                let StreamableHttpPostResponse::Sse(mut stream, _) = result.expect("SSE headers")
                else {
                    panic!("SSE expected")
                };
                assert!(stream
                    .next()
                    .await
                    .expect("SSE event error")
                    .unwrap_err()
                    .to_string()
                    .contains("4 MiB"));
            } else {
                assert!(result.unwrap_err().to_string().contains("4 MiB"));
            }
        }
        let mut stream = client
            .get_stream(
                format!("http://{address}/sse").into(),
                None,
                None,
                None,
                HashMap::new(),
            )
            .await
            .expect("GET SSE");
        assert!(stream.next().await.expect("GET error").is_err());
        server.abort();
    }
}
