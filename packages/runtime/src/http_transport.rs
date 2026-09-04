use centaeris_core::model::{
    JsonHttpFuture, JsonHttpRequest, JsonHttpResponse, JsonHttpTransport,
    MODEL_PROVIDER_WAITING_STREAM_EVENT_TYPE,
};
use std::collections::HashMap;
use std::time::Duration;

pub(crate) struct ReqwestJsonHttpTransport {
    client: reqwest::Client,
}

impl ReqwestJsonHttpTransport {
    pub(crate) fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| format!("build reqwest client failed: {error}"))?;
        Ok(Self { client })
    }
}

impl JsonHttpTransport for ReqwestJsonHttpTransport {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(execute_json_async(self.client.clone(), request.clone()))
    }

    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        Box::pin(execute_sse_async(
            self.client.clone(),
            request.clone(),
            on_data,
        ))
    }
}

async fn execute_json_async(
    client: reqwest::Client,
    request: JsonHttpRequest,
) -> Result<JsonHttpResponse, String> {
    let response = send_with_total_timeout(client, request).await?;
    response_to_json(response).await
}

async fn execute_sse_async(
    client: reqwest::Client,
    request: JsonHttpRequest,
    on_data: &mut (dyn FnMut(String) + Send),
) -> Result<JsonHttpResponse, String> {
    let sse_idle_timeout_ms = request.sse_idle_timeout_ms.max(1);
    let mut response = send_sse_headers(client, request).await?;
    let status_code = response.status().as_u16();
    let headers = response_headers(&response);
    if status_code >= 400 {
        let body_json =
            tokio::time::timeout(Duration::from_millis(sse_idle_timeout_ms), response.text())
                .await
                .map_err(|_| {
                    format!("read SSE error body idle timeout after {sse_idle_timeout_ms}ms")
                })?
                .map_err(|error| format_reqwest_error("read SSE error body failed", error))?;
        return Ok(JsonHttpResponse {
            status_code,
            headers,
            body_json,
        });
    }
    let mut decoder = SseDecoder::default();

    loop {
        let next_chunk =
            tokio::time::timeout(Duration::from_millis(sse_idle_timeout_ms), response.chunk())
                .await
                .map_err(|_| format!("read SSE chunk idle timeout after {sse_idle_timeout_ms}ms"))?
                .map_err(|error| format_reqwest_error("read SSE chunk failed", error))?;
        let Some(bytes) = next_chunk else {
            break;
        };
        decoder.push(bytes.as_ref(), on_data)?;
    }
    decoder.finish(on_data)?;

    Ok(JsonHttpResponse {
        status_code,
        headers,
        body_json: String::new(),
    })
}

async fn send_with_total_timeout(
    client: reqwest::Client,
    request: JsonHttpRequest,
) -> Result<reqwest::Response, String> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| format!("invalid http method {}: {error}", request.method))?;
    let mut builder = client
        .request(method, request.url.as_str())
        .timeout(Duration::from_millis(request.timeout_ms.max(1)));
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(request.body_json)
        .send()
        .await
        .map_err(|error| format_reqwest_error("http send failed", error))
}

async fn send_sse_headers(
    client: reqwest::Client,
    request: JsonHttpRequest,
) -> Result<reqwest::Response, String> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| format!("invalid http method {}: {error}", request.method))?;
    let mut builder = client.request(method, request.url.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    tokio::time::timeout(
        Duration::from_millis(request.timeout_ms.max(1)),
        builder.body(request.body_json).send(),
    )
    .await
    .map_err(|_| {
        format!(
            "http send timed out while waiting for SSE headers after {}ms",
            request.timeout_ms.max(1)
        )
    })?
    .map_err(|error| format_reqwest_error("http send failed", error))
}

async fn response_to_json(response: reqwest::Response) -> Result<JsonHttpResponse, String> {
    let status_code = response.status().as_u16();
    let headers = response_headers(&response);
    let body_json = response
        .text()
        .await
        .map_err(|error| format_reqwest_error("read http body failed", error))?;
    Ok(JsonHttpResponse {
        status_code,
        headers,
        body_json,
    })
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
    data_lines: Vec<String>,
}

impl SseDecoder {
    fn push(
        &mut self,
        bytes: &[u8],
        on_data: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), String> {
        self.pending.extend_from_slice(bytes);
        self.drain(false, on_data)
    }

    fn finish(&mut self, on_data: &mut (dyn FnMut(String) + Send)) -> Result<(), String> {
        self.drain(true, on_data)?;
        if self.pending.is_empty() && self.data_lines.is_empty() {
            Ok(())
        } else {
            Err("SSE stream ended with an incomplete event".to_string())
        }
    }

    fn drain(
        &mut self,
        end_of_stream: bool,
        on_data: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), String> {
        loop {
            let Some(index) = self
                .pending
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
            else {
                return Ok(());
            };
            if self.pending[index] == b'\r' && index + 1 == self.pending.len() && !end_of_stream {
                return Ok(());
            }
            let delimiter_bytes =
                if self.pending[index] == b'\r' && self.pending.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            let line = self.pending[..index].to_vec();
            self.pending.drain(..index + delimiter_bytes);
            self.process_line(line.as_slice(), on_data)?;
        }
    }

    fn process_line(
        &mut self,
        line: &[u8],
        on_data: &mut (dyn FnMut(String) + Send),
    ) -> Result<(), String> {
        let line = std::str::from_utf8(line)
            .map_err(|error| format!("SSE event contains invalid UTF-8: {error}"))?;
        if line.is_empty() {
            if !self.data_lines.is_empty() {
                let data = self.data_lines.join("\n");
                self.data_lines.clear();
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
            self.data_lines
                .push(value.strip_prefix(' ').unwrap_or(value).to_string());
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

fn format_reqwest_error(context: &str, error: reqwest::Error) -> String {
    let mut message = format!("{context}: {error}");
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str("; caused by: ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

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

    #[tokio::test]
    async fn sse_timeout_is_idle_based_and_resets_on_keep_alive_chunks() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local SSE server");
        let address = listener.local_addr().expect("local SSE address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept SSE request");
            let mut request_bytes = [0_u8; 4096];
            let _ = stream.read(&mut request_bytes).expect("read SSE request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .expect("write SSE headers");
            stream.flush().expect("flush SSE headers");
            for _ in 0..3 {
                thread::sleep(Duration::from_millis(60));
                stream
                    .write_all(b": keep-alive\n\n")
                    .expect("write keep-alive");
                stream.flush().expect("flush keep-alive");
            }
            thread::sleep(Duration::from_millis(60));
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
            timeout_ms: 50,
            sse_idle_timeout_ms: 150,
            max_retries: 0,
            retry_backoff_ms: 0,
            body_json: "{}".to_string(),
        };
        let mut events = Vec::new();

        let response = execute_sse_async(reqwest::Client::new(), request, &mut |event| {
            events.push(event)
        })
        .await
        .expect("keep-alive chunks should reset the idle timeout");
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
