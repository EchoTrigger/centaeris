use centaeris_core::model::{
    HttpTimings, JsonHttpFuture, JsonHttpRequest, JsonHttpResponse, JsonHttpTransport,
    TransportError, MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::time::Duration;

// Wire-buffer safety ceilings, independently bounded per response/line/event.
const MODEL_BODY_MAX_BYTES: usize = 16 * 1024 * 1024;
const SSE_LINE_MAX_BYTES: usize = 16 * 1024 * 1024;
const SSE_EVENT_MAX_BYTES: usize = 16 * 1024 * 1024;
pub(crate) struct ReqwestJsonHttpTransport {
    inner: ReqwestHttpClient,
}

impl ReqwestJsonHttpTransport {
    pub(crate) fn for_session(
        session_id: &str,
        provider_id: &str,
        client: impl FnOnce() -> Result<reqwest::Client, String>,
    ) -> Result<Self, String> {
        let session_header = if provider_id == "opencode-go.default" {
            if session_id.trim().is_empty() {
                return Err("OpenCode requests require a conversation session ID".into());
            }
            reqwest::header::HeaderValue::from_str(session_id)
                .map_err(|_| "invalid conversation session ID for OpenCode".to_string())?;
            Some(session_id.to_owned())
        } else {
            None
        };
        // Preserve provider/session validation before lazy Client construction.
        let client = client()?;
        Ok(Self {
            inner: ReqwestHttpClient {
                client,
                session_header,
            },
        })
    }
}

impl JsonHttpTransport for ReqwestJsonHttpTransport {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        self.inner.execute_json(request)
    }
    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        self.inner.execute_sse(request, on_data)
    }
}

pub(crate) fn build_model_http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("centaeris/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("build reqwest client failed: {error}"))
}

struct ReqwestHttpClient {
    client: reqwest::Client,
    session_header: Option<String>,
}

impl ReqwestHttpClient {
    fn request(&self, request: &JsonHttpRequest) -> JsonHttpRequest {
        let mut request = request.clone();
        if let Some(session) = &self.session_header {
            // Client defaults previously filled only absent fields. Preserve all
            // explicitly supplied case variants, including multiple values.
            if !request
                .headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("x-opencode-session"))
            {
                request
                    .headers
                    .insert("x-opencode-session".into(), session.clone());
            }
        }
        request
    }
}
impl JsonHttpTransport for ReqwestHttpClient {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(execute_json_async(
            self.client.clone(),
            self.request(request),
        ))
    }
    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        Box::pin(execute_sse_async(
            self.client.clone(),
            self.request(request),
            on_data,
        ))
    }
}

fn elapsed_ms(start: tokio::time::Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

async fn execute_json_async(
    client: reqwest::Client,
    request: JsonHttpRequest,
) -> Result<JsonHttpResponse, TransportError> {
    let started = tokio::time::Instant::now();
    let response = send_with_total_timeout(client, request).await?;
    let headers_wait_ms = elapsed_ms(started);
    let mut response = response_to_json(response).await?;
    let total_ms = elapsed_ms(started);
    response.timings = Some(HttpTimings {
        headers_wait_ms,
        first_event_ms: None,
        response_body_ms: total_ms.saturating_sub(headers_wait_ms),
        total_ms,
    });
    Ok(response)
}

async fn execute_sse_async(
    client: reqwest::Client,
    request: JsonHttpRequest,
    on_data: &mut (dyn FnMut(String) + Send),
) -> Result<JsonHttpResponse, TransportError> {
    let started = tokio::time::Instant::now();
    let sse_idle_timeout_ms = request.sse_idle_timeout_ms.max(1);
    let response = send_sse_headers(client, request).await?;
    let headers_wait_ms = elapsed_ms(started);
    let mut first_event_ms = None;
    let status_code = response.status().as_u16();
    let headers = response_headers(&response);
    if !response.status().is_success() {
        let body_json = tokio::time::timeout(
            Duration::from_millis(sse_idle_timeout_ms),
            read_bounded_body(response, "SSE error"),
        )
        .await
        .map_err(|_| {
            TransportError::Timeout(format!(
                "read SSE error body idle timeout after {sse_idle_timeout_ms}ms"
            ))
        })??;
        let total_ms = elapsed_ms(started);
        return Ok(JsonHttpResponse {
            timings: Some(HttpTimings {
                headers_wait_ms,
                first_event_ms: None,
                response_body_ms: total_ms.saturating_sub(headers_wait_ms),
                total_ms,
            }),
            status_code,
            headers,
            body_json,
        });
    }
    let media_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next());
    if !media_type.is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream")) {
        return Err(TransportError::InvalidResponse(
            "successful model SSE response requires Content-Type text/event-stream".into(),
        ));
    }
    read_sse_chunks(
        futures::stream::try_unfold(response, |mut response| async move {
            let bytes = response
                .chunk()
                .await
                .map_err(|error| format_reqwest_error("read SSE chunk failed", error))?;
            Ok(bytes.map(|bytes| (bytes, response)))
        }),
        sse_idle_timeout_ms,
        &mut |event| {
            first_event_ms.get_or_insert_with(|| elapsed_ms(started));
            on_data(event);
        },
    )
    .await?;

    let total_ms = elapsed_ms(started);
    Ok(JsonHttpResponse {
        timings: Some(HttpTimings {
            headers_wait_ms,
            first_event_ms,
            response_body_ms: total_ms.saturating_sub(headers_wait_ms),
            total_ms,
        }),
        status_code,
        headers,
        body_json: String::new(),
    })
}

async fn read_sse_chunks<B: AsRef<[u8]>>(
    chunks: impl futures::Stream<Item = Result<B, TransportError>>,
    sse_idle_timeout_ms: u64,
    on_data: &mut (dyn FnMut(String) + Send),
) -> Result<(), TransportError> {
    let mut decoder = SseDecoder::default();
    futures::pin_mut!(chunks);

    loop {
        let next_chunk =
            tokio::time::timeout(Duration::from_millis(sse_idle_timeout_ms), chunks.next())
                .await
                .map_err(|_| {
                    TransportError::Timeout(format!(
                        "read SSE chunk idle timeout after {sse_idle_timeout_ms}ms"
                    ))
                })?;
        let Some(bytes) = next_chunk else {
            break;
        };
        decoder.push(bytes?.as_ref(), on_data)?;
    }
    decoder.finish(on_data)
}

async fn send_with_total_timeout(
    client: reqwest::Client,
    request: JsonHttpRequest,
) -> Result<reqwest::Response, TransportError> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|error| {
        TransportError::Build(format!("invalid http method {}: {error}", request.method))
    })?;
    let mut builder = client
        .request(method, request.url.as_str())
        .timeout(Duration::from_millis(request.timeout_ms.max(1)));
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(bytes::Bytes::from_owner(request.body_json))
        .send()
        .await
        .map_err(|error| format_reqwest_error("http send failed", error))
}

async fn send_sse_headers(
    client: reqwest::Client,
    request: JsonHttpRequest,
) -> Result<reqwest::Response, TransportError> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|error| {
        TransportError::Build(format!("invalid http method {}: {error}", request.method))
    })?;
    let mut builder = client.request(method, request.url.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    tokio::time::timeout(
        Duration::from_millis(request.timeout_ms.max(1)),
        builder
            .body(bytes::Bytes::from_owner(request.body_json))
            .send(),
    )
    .await
    .map_err(|_| {
        TransportError::Timeout(format!(
            "http send timed out while waiting for SSE headers after {}ms",
            request.timeout_ms.max(1)
        ))
    })?
    .map_err(|error| format_reqwest_error("http send failed", error))
}

async fn response_to_json(response: reqwest::Response) -> Result<JsonHttpResponse, TransportError> {
    let status_code = response.status().as_u16();
    let headers = response_headers(&response);
    let body_json = read_bounded_body(response, "http").await?;
    Ok(JsonHttpResponse {
        timings: None,
        status_code,
        headers,
        body_json,
    })
}

async fn read_bounded_body(
    mut response: reqwest::Response,
    context: &str,
) -> Result<String, TransportError> {
    let too_large = || TransportError::ResponseTooLarge {
        max_bytes: MODEL_BODY_MAX_BYTES,
    };
    if response
        .content_length()
        .is_some_and(|length| length > MODEL_BODY_MAX_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format_reqwest_error(&format!("read {context} body failed"), error))?
    {
        if chunk.len() > MODEL_BODY_MAX_BYTES - body.len() {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    // reqwest's charset feature is disabled; preserve its text() UTF-8-lossy policy.
    Ok(String::from_utf8_lossy(&body).into_owned())
}
fn response_headers(response: &reqwest::Response) -> HashMap<String, String> {
    response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    scan_from: usize,
    data: String,
    has_data: bool,
    read_first_line: bool,
}

impl SseDecoder {
    fn push(
        &mut self,
        bytes: &[u8],
        on_data: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), TransportError> {
        // Drain incrementally: a large network chunk must not create an unbounded
        // pending buffer before the line limit is checked.
        for chunk in bytes.chunks(8192) {
            self.pending.extend_from_slice(chunk);
            self.drain(false, on_data)?;
            let trailing_cr = usize::from(self.pending.last() == Some(&b'\r'));
            if self.pending.len() > SSE_LINE_MAX_BYTES + trailing_cr {
                return Err(TransportError::ResponseTooLarge {
                    max_bytes: SSE_LINE_MAX_BYTES,
                });
            }
        }
        Ok(())
    }

    fn finish(&mut self, on_data: &mut (dyn FnMut(String) + Send)) -> Result<(), TransportError> {
        self.drain(true, on_data)?;
        if self.pending.is_empty() && !self.has_data {
            Ok(())
        } else {
            Err(TransportError::StreamInterrupted(
                "SSE stream ended with an incomplete event".to_string(),
            ))
        }
    }

    fn drain(
        &mut self,
        end_of_stream: bool,
        on_data: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), TransportError> {
        loop {
            let Some(index) = self
                .pending
                .get(self.scan_from..)
                .expect("scan stays within pending line")
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .map(|offset| self.scan_from + offset)
            else {
                self.scan_from = self.pending.len();
                return Ok(());
            };
            if self.pending[index] == b'\r' && index + 1 == self.pending.len() && !end_of_stream {
                self.scan_from = index;
                return Ok(());
            }
            let delimiter_bytes =
                if self.pending[index] == b'\r' && self.pending.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            if index > SSE_LINE_MAX_BYTES {
                return Err(TransportError::ResponseTooLarge {
                    max_bytes: SSE_LINE_MAX_BYTES,
                });
            }
            let line = self.pending[..index].to_vec();
            self.pending.drain(..index + delimiter_bytes);
            self.scan_from = 0;
            self.process_line(line.as_slice(), on_data)?;
        }
    }

    fn process_line(
        &mut self,
        line: &[u8],
        on_data: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), TransportError> {
        let line = if !self.read_first_line {
            self.read_first_line = true;
            line.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(line)
        } else {
            line
        };
        let line = std::str::from_utf8(line).map_err(|error| {
            TransportError::InvalidResponse(format!("SSE event contains invalid UTF-8: {error}"))
        })?;
        if line.is_empty() {
            if self.has_data {
                let data = std::mem::take(&mut self.data);
                self.has_data = false;
                if !data.is_empty() && data != "[DONE]" {
                    on_data(data);
                }
            }
            return Ok(());
        }
        if line.starts_with(':') {
            on_data(provider_waiting_chunk());
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        if field == "data" {
            let value = value.strip_prefix(' ').unwrap_or(value);
            let separator = usize::from(self.has_data);
            if value.len() + separator > SSE_EVENT_MAX_BYTES - self.data.len() {
                return Err(TransportError::ResponseTooLarge {
                    max_bytes: SSE_EVENT_MAX_BYTES,
                });
            }
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(value);
            self.has_data = true;
        }
        Ok(())
    }
}

fn provider_waiting_chunk() -> String {
    serde_json::json!({
        "type": MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE,
    })
    .to_string()
}

fn format_reqwest_error(context: &str, error: reqwest::Error) -> TransportError {
    let mut message = format!("{context}: {error}");
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str("; caused by: ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    if error.is_timeout() {
        TransportError::Timeout(message)
    } else if error.is_builder() {
        TransportError::Build(message)
    } else if error.is_connect() {
        TransportError::Connection(message)
    } else {
        TransportError::Network(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[tokio::test]
    async fn model_requests_send_product_user_agent_and_preserve_explicit_overrides() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for streaming in [false, true] {
            for (header_name, provider, session) in [
                (None, "opencode-go.default", "conversation-a"),
                (Some("User-Agent"), "opencode-go.default", "conversation-a"),
                (Some("uSeR-aGeNt"), "opencode-go.default", "conversation-b"),
                (None, "openai.default", "conversation-c"),
            ] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut wire = Vec::new();
                    while !wire.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let mut chunk = [0; 1024];
                        let count = socket.read(&mut chunk).await.unwrap();
                        assert!(count > 0 && wire.len() < 65_536);
                        wire.extend_from_slice(&chunk[..count]);
                    }
                    let (content_type, body) = if streaming {
                        ("text/event-stream", "data: [DONE]\n\n")
                    } else {
                        ("application/json", "{}")
                    };
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                    String::from_utf8(wire).unwrap()
                });
                let request = JsonHttpRequest {
                    method: "POST".into(),
                    url: format!("http://{address}/model"),
                    headers: header_name
                        .map(|name| HashMap::from([(name.into(), "custom-agent/2".into())]))
                        .unwrap_or_default(),
                    timeout_ms: 5000,
                    sse_idle_timeout_ms: 5000,
                    max_retries: 0,
                    retry_backoff_ms: 0,
                    body_json: "{}".into(),
                };
                let client = ReqwestJsonHttpTransport::for_session(
                    session,
                    provider,
                    build_model_http_client,
                )
                .unwrap();
                let response = if streaming {
                    client.execute_sse(&request, &mut |_| {}).await
                } else {
                    client.execute_json(&request).await
                }
                .unwrap();
                assert_eq!(response.status_code, 200);
                let wire = tokio::time::timeout(Duration::from_secs(5), server)
                    .await
                    .unwrap()
                    .unwrap();
                let session_headers: Vec<_> = wire
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .filter(|(name, _)| name.eq_ignore_ascii_case("x-opencode-session"))
                    .map(|(_, value)| value.trim())
                    .collect();
                assert_eq!(
                    session_headers,
                    if provider == "opencode-go.default" {
                        vec![session]
                    } else {
                        vec![]
                    }
                );
                let values: Vec<_> = wire
                    .split("\r\n\r\n")
                    .next()
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                    .map(|(_, value)| value.trim())
                    .collect();
                let expected = if header_name.is_some() {
                    "custom-agent/2"
                } else {
                    concat!("centaeris/", env!("CARGO_PKG_VERSION"))
                };
                assert_eq!(values, vec![expected], "streaming={streaming}");
            }
        }
    }

    #[tokio::test]
    async fn independent_sessions_dispatch_eight_requests_before_any_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::sync::mpsc;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let (arrived, mut requests) = mpsc::channel(8);
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let server_release = release.clone();
        let server = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            for _ in 0..8 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let arrived = arrived.clone();
                let release = server_release.clone();
                connections.spawn(async move {
                    let wait = release.notified();
                    tokio::pin!(wait);
                    wait.as_mut().enable();
                    let mut wire = Vec::new();
                    while !wire.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let mut chunk = [0; 1024];
                        let n = socket.read(&mut chunk).await.unwrap();
                        assert!(n > 0);
                        wire.extend_from_slice(&chunk[..n]);
                    }
                    arrived.send(()).await.unwrap();
                    wait.await;
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        )
                        .await;
                });
            }
            while let Some(done) = connections.join_next().await {
                done.unwrap();
            }
        });
        let mut tasks = tokio::task::JoinSet::new();
        for n in 0..8 {
            let url = url.clone();
            tasks.spawn(async move {
                let transport = ReqwestJsonHttpTransport::for_session(
                    &format!("session-{n}"),
                    "custom.provider",
                    || {
                        reqwest::Client::builder()
                            .no_proxy()
                            .build()
                            .map_err(|e| e.to_string())
                    },
                )
                .unwrap();
                let request = JsonHttpRequest {
                    method: "POST".into(),
                    url,
                    headers: HashMap::new(),
                    timeout_ms: 5000,
                    sse_idle_timeout_ms: 5000,
                    max_retries: 0,
                    retry_backoff_ms: 0,
                    body_json: "{}".into(),
                };
                transport.execute_json(&request).await
            });
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..8 {
                requests.recv().await.expect("request reached server");
            }
        })
        .await
        .expect("all eight requests must dispatch before a response releases anything");
        release.notify_waiters();
        while let Some(done) = tasks.join_next().await {
            assert_eq!(done.unwrap().unwrap().status_code, 200);
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn model_http_client_routes_socks5h_proxy_scheme_to_connector() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind unused proxy port");
        let proxy_address = listener.local_addr().expect("proxy address");
        drop(listener);
        let client = reqwest::Client::builder()
            .proxy(
                reqwest::Proxy::all(format!("socks5h://{proxy_address}"))
                    .expect("accept socks5h proxy scheme"),
            )
            .build()
            .expect("build model HTTP client with socks5h proxy");
        let error = client
            .get("http://example.invalid")
            .send()
            .await
            .expect_err("closed local proxy must reject the connection");
        let message = format_reqwest_error("proxy request failed", error);

        assert!(
            !message.to_string().contains("unsupported scheme socks5h"),
            "{message}"
        );
    }

    #[test]
    fn sse_decoder_handles_keep_alive_and_complete_data_events() {
        let mut events = Vec::new();
        let mut decoder = SseDecoder::default();

        decoder
            .push(
                b": keep-alive\n\ndata:\n\ndata: {\"id\":\"completion\",\"choices\":[]}\n\ndata: [DONE]\n\n",
                &mut |event| events.push(event),
            )
            .expect("decode SSE events");
        decoder.finish(&mut |event| events.push(event)).unwrap();

        assert_eq!(events.len(), 2);
        assert!(events[0].contains(MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE));
        assert_eq!(events[1], "{\"id\":\"completion\",\"choices\":[]}");
    }

    #[test]
    fn sse_decoder_is_lossless_at_every_byte_boundary() {
        for newline in ["\n", "\r", "\r\n"] {
            let wire = format!(
                "data: {{\"text\":\"你\",{newline}data: \"next\":\"好\"}}{newline}{newline}"
            );
            for boundary in 0..=wire.len() {
                let mut events = Vec::new();
                let mut decoder = SseDecoder::default();
                decoder
                    .push(&wire.as_bytes()[..boundary], &mut |event| {
                        events.push(event)
                    })
                    .expect("decode first byte segment");
                decoder
                    .push(&wire.as_bytes()[boundary..], &mut |event| {
                        events.push(event)
                    })
                    .expect("decode second byte segment");
                decoder.finish(&mut |event| events.push(event)).unwrap();
                assert_eq!(
                    events,
                    vec![format!("{{\"text\":\"你\",\n\"next\":\"好\"}}")],
                    "newline={newline:?} boundary={boundary}"
                );
            }
        }
    }

    #[test]
    fn sse_decoder_rejects_invalid_utf8_and_incomplete_event() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: \xff\n\n", &mut |_| {}).is_err());

        let mut decoder = SseDecoder::default();
        decoder.push(b"data: {}", &mut |_| {}).unwrap();
        assert!(decoder.finish(&mut |_| {}).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn sse_timeout_is_idle_based_and_resets_on_keep_alive_chunks() {
        let started = tokio::time::Instant::now();
        let chunks = std::collections::VecDeque::from([
            ": keep-alive\n\n",
            ": keep-alive\n\n",
            ": keep-alive\n\n",
            "data: done\n\ndata: [DONE]\n\n",
        ]);
        let mut events = Vec::new();
        read_sse_chunks(
            futures::stream::unfold(chunks, |mut chunks| async move {
                let next = chunks.pop_front()?;
                tokio::time::sleep(Duration::from_millis(60)).await;
                Some((Ok(next), chunks))
            }),
            150,
            &mut |event| events.push(event),
        )
        .await
        .expect("each chunk resets the idle timeout");
        assert_eq!(started.elapsed(), Duration::from_millis(240));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.contains(MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE))
                .count(),
            3
        );
        assert_eq!(events.last().map(String::as_str), Some("done"));
    }

    #[tokio::test(start_paused = true)]
    async fn sse_timeout_expires_when_the_next_chunk_is_idle() {
        let started = tokio::time::Instant::now();
        let error = read_sse_chunks(
            futures::stream::once(async {
                tokio::time::sleep(Duration::from_millis(151)).await;
                Ok("data: late\n\n")
            }),
            150,
            &mut |_| {},
        )
        .await
        .expect_err("an idle stream must time out");
        assert_eq!(started.elapsed(), Duration::from_millis(150));
        assert_eq!(
            error,
            TransportError::Timeout("read SSE chunk idle timeout after 150ms".into())
        );
    }

    #[tokio::test]
    async fn sse_http_delivers_keep_alive_and_terminal_events() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local SSE server");
        let address = listener.local_addr().expect("local SSE address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept SSE request");
            stream
                .set_nodelay(true)
                .expect("disable test-server buffering");
            let mut request_bytes = [0_u8; 4096];
            let _ = stream.read(&mut request_bytes).expect("read SSE request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .expect("write SSE headers");
            stream.flush().expect("flush SSE headers");
            for _ in 0..3 {
                stream
                    .write_all(b": keep-alive\n\n")
                    .expect("write keep-alive");
                stream.flush().expect("flush keep-alive");
            }
            stream
                .write_all(
                    b"data: {\"id\":\"done\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                )
                .expect("write terminal SSE event");
            stream.flush().expect("flush terminal SSE event");
        });
        let request = JsonHttpRequest {
            method: "POST".to_string(),
            url: format!("http://{address}/chat/completions"),
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            timeout_ms: 5000,
            sse_idle_timeout_ms: 5000,
            max_retries: 0,
            retry_backoff_ms: 0,
            body_json: "{}".into(),
        };
        let mut events = Vec::new();

        let response = execute_sse_async(reqwest::Client::new(), request, &mut |event| {
            events.push(event)
        })
        .await
        .expect("deliver keep-alive and terminal events over HTTP");
        server.join().expect("join local SSE server");

        assert_eq!(response.status_code, 200);
        assert!(response.body_json.is_empty());
        assert_eq!(
            events
                .iter()
                .filter(|event| event.contains(MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE))
                .count(),
            3
        );
        assert!(events.iter().any(|event| event.contains("finish_reason")));
    }
}

#[cfg(test)]
mod boundary_tests;

#[cfg(test)]
mod client_tests;
