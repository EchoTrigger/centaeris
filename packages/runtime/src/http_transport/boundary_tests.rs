use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BODY_LIMIT_BYTES: usize = 16 * 1024 * 1024;

fn request(url: String) -> JsonHttpRequest {
    JsonHttpRequest {
        method: "POST".into(),
        url,
        headers: HashMap::from([("x-api-key".into(), "fake-secret".into())]),
        timeout_ms: 5000,
        sse_idle_timeout_ms: 5000,
        max_retries: 0,
        retry_backoff_ms: 0,
        body_json: "private-test-body".into(),
    }
}
async fn response_server(status: u16, content_type: &str, body: String, extra: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let content_type = content_type.to_string();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut wire = Vec::new();
        while !wire.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut chunk = [0; 1024];
            let count = socket.read(&mut chunk).await.unwrap();
            if count == 0 {
                return;
            }
            wire.extend_from_slice(&chunk[..count]);
        }
        let header_end = wire
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        let content_length = String::from_utf8_lossy(&wire[..header_end])
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().parse::<usize>().unwrap())
            .unwrap_or(0);
        while wire.len() < header_end + content_length {
            let mut chunk = [0; 1024];
            let count = socket.read(&mut chunk).await.unwrap();
            if count == 0 {
                return;
            }
            wire.extend_from_slice(&chunk[..count]);
        }
        let headers = format!("HTTP/1.1 {status} test\r\nContent-Type: {content_type}\r\nConnection: close\r\n{extra}\r\n");
        if socket.write_all(headers.as_bytes()).await.is_ok() {
            let _ = socket.write_all(body.as_bytes()).await;
        }
    });
    format!("http://{address}/model")
}

#[tokio::test]
async fn redirects_never_reach_another_server_with_model_credentials() {
    for status in [302, 307, 308] {
        for streaming in [false, true] {
            let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = target.local_addr().unwrap();
            let url = response_server(
                status,
                "text/plain",
                "redirect".into(),
                format!("Location: http://{address}/capture\r\n"),
            )
            .await;
            let transport = ReqwestJsonHttpTransport::for_session(
                "redirect-test".into(),
                "test-session",
                "opencode-go.default",
            )
            .unwrap();
            let req = request(url);
            // Observe the forbidden connection directly, rather than inferring it
            // from a short response deadline (which can include admission wait).
            let exchange = async {
                if streaming {
                    transport.execute_sse(&req, &mut |_| {}).await
                } else {
                    transport.execute_json(&req).await
                }
            };
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    biased;
                    connection = target.accept() => {
                        panic!("redirect connected to target: status={status}, streaming={streaming}, connection={connection:?}");
                    }
                    response = exchange => response,
                }
            })
            .await
            .unwrap_or_else(|_| panic!("model response hung: status={status}, streaming={streaming}"))
            .unwrap_or_else(|error| panic!("model response failed: status={status}, streaming={streaming}, error={error}"));
            assert_eq!(result.status_code, status);
            assert_eq!(result.body_json, "redirect");
            assert!(
                tokio::time::timeout(Duration::from_millis(20), target.accept())
                    .await
                    .is_err(),
                "redirect target received a connection"
            );
        }
    }
}

#[tokio::test]
async fn sse_non_success_status_preserves_error_body() {
    for status in [302, 307, 308, 400, 429, 500] {
        let url =
            response_server(status, "text/plain", "upstream error".into(), String::new()).await;
        let response = execute_sse_async(reqwest::Client::new(), request(url), &mut |_| {
            panic!("error response is not an event")
        })
        .await
        .unwrap();
        assert_eq!(response.status_code, status);
        assert_eq!(response.body_json, "upstream error");
    }
}

#[tokio::test]
async fn sse_success_requires_event_stream_media_type() {
    for content_type in ["text/html", "application/json", "text/event-streamish"] {
        let url = response_server(200, content_type, "data: bad\n\n".into(), String::new()).await;
        assert!(
            execute_sse_async(reqwest::Client::new(), request(url), &mut |_| {})
                .await
                .unwrap_err()
                .contains("Content-Type")
        );
    }
    let url = response_server(
        200,
        "text/event-stream; charset=utf-8",
        "data: good\n\n".into(),
        String::new(),
    )
    .await;
    let mut events = Vec::new();
    execute_sse_async(reqwest::Client::new(), request(url), &mut |event| {
        events.push(event)
    })
    .await
    .unwrap();
    assert_eq!(events, ["good"]);
}

#[tokio::test]
async fn model_json_and_sse_error_bodies_are_bounded() {
    for streaming in [false, true] {
        for bytes in [BODY_LIMIT_BYTES, BODY_LIMIT_BYTES + 1] {
            let url = response_server(
                if streaming { 500 } else { 200 },
                "application/json",
                "x".repeat(bytes),
                String::new(),
            )
            .await;
            let result = if streaming {
                execute_sse_async(reqwest::Client::new(), request(url), &mut |_| {}).await
            } else {
                execute_json_async(reqwest::Client::new(), request(url)).await
            };
            if bytes == BODY_LIMIT_BYTES {
                assert_eq!(result.unwrap().body_json.len(), bytes);
            } else {
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("oversized body must be rejected"),
                };
                assert!(error.contains("body exceeds"));
            }
        }
    }
}

#[tokio::test]
async fn declared_oversized_body_is_rejected_without_waiting_for_body_bytes() {
    let url = response_server(
        200,
        "application/json",
        String::new(),
        format!("Content-Length: {}\r\n", BODY_LIMIT_BYTES + 1),
    )
    .await;
    let error = execute_json_async(reqwest::Client::new(), request(url))
        .await
        .unwrap_err();
    assert!(error.contains("body exceeds"));
}

#[test]
fn sse_limits_unterminated_lines() {
    let mut decoder = SseDecoder::default();
    assert!(decoder
        .push(&vec![b'x'; 16 * 1024 * 1024 + 1], &mut |_| {})
        .unwrap_err()
        .contains("line exceeds"));
}

#[test]
fn sse_limits_multiline_event_without_blank_delimiter() {
    let mut decoder = SseDecoder::default();
    let line = format!("data: {}\n", "x".repeat(64 * 1024));
    let mut result = Ok(());
    for _ in 0..257 {
        result = decoder.push(line.as_bytes(), &mut |_| {});
        if result.is_err() {
            break;
        }
    }
    assert!(result.unwrap_err().contains("event exceeds"));
}

#[test]
fn sse_ignores_one_leading_bom_at_every_byte_split() {
    let wire = "\u{feff}data: hello\n\n".as_bytes();
    for split in 0..=wire.len() {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        decoder
            .push(&wire[..split], &mut |event| events.push(event))
            .unwrap();
        decoder
            .push(&wire[split..], &mut |event| events.push(event))
            .unwrap();
        decoder.finish(&mut |event| events.push(event)).unwrap();
        assert_eq!(events, ["hello"], "split={split}");
    }
}

fn push_line_with_split_delimiters(
    decoder: &mut SseDecoder,
    line: &[u8],
    delimiter: &str,
    on_data: &mut (dyn FnMut(String) + Send),
) -> Result<(), String> {
    for chunk in line.chunks(4093) {
        decoder.push(chunk, on_data)?;
    }
    for byte in delimiter.as_bytes() {
        decoder.push(std::slice::from_ref(byte), on_data)?;
    }
    Ok(())
}

#[test]
fn sse_accepts_exact_line_budget_and_rejects_overflow_with_split_delimiters() {
    for delimiter in ["\n", "\r\n", "\r"] {
        for line_bytes in [BODY_LIMIT_BYTES, BODY_LIMIT_BYTES + 1] {
            let line = format!("data: {}", "x".repeat(line_bytes - "data: ".len()));
            let mut decoder = SseDecoder::default();
            let mut delivered = 0;
            let mut on_data = |event: String| {
                assert_eq!(event.len(), line_bytes - "data: ".len());
                delivered += 1;
            };
            let result = push_line_with_split_delimiters(
                &mut decoder,
                line.as_bytes(),
                delimiter,
                &mut on_data,
            )
            .and_then(|()| {
                push_line_with_split_delimiters(&mut decoder, b"", delimiter, &mut on_data)
            })
            .and_then(|()| decoder.finish(&mut on_data));
            if line_bytes == BODY_LIMIT_BYTES {
                result.unwrap();
                assert_eq!(delivered, 1, "delimiter={delimiter:?}");
            } else {
                assert!(result.unwrap_err().contains("line exceeds"));
                assert_eq!(delivered, 0, "oversized line must never be delivered");
            }
        }
    }
}

#[test]
fn sse_accepts_exact_multiline_event_budget_and_rejects_overflow_before_delivery() {
    let first_data_bytes = BODY_LIMIT_BYTES / 2;
    let first_line = format!("data: {}", "x".repeat(first_data_bytes));
    for delimiter in ["\n", "\r\n", "\r"] {
        for event_bytes in [BODY_LIMIT_BYTES, BODY_LIMIT_BYTES + 1] {
            // One decoded joining newline is included in the event budget.
            let second_line = format!("data: {}", "y".repeat(event_bytes - first_data_bytes - 1));
            let mut decoder = SseDecoder::default();
            let mut delivered = 0;
            let mut on_data = |event: String| {
                assert_eq!(event.len(), event_bytes);
                assert_eq!(event.as_bytes()[first_data_bytes], b'\n');
                delivered += 1;
            };
            let result = push_line_with_split_delimiters(
                &mut decoder,
                first_line.as_bytes(),
                delimiter,
                &mut on_data,
            )
            .and_then(|()| {
                push_line_with_split_delimiters(
                    &mut decoder,
                    second_line.as_bytes(),
                    delimiter,
                    &mut on_data,
                )
            })
            .and_then(|()| {
                push_line_with_split_delimiters(&mut decoder, b"", delimiter, &mut on_data)
            })
            .and_then(|()| decoder.finish(&mut on_data));
            if event_bytes == BODY_LIMIT_BYTES {
                result.unwrap();
                assert_eq!(delivered, 1, "delimiter={delimiter:?}");
            } else {
                assert!(result.unwrap_err().contains("event exceeds"));
                assert_eq!(delivered, 0, "oversized event must never be delivered");
            }
        }
    }
}

#[test]
fn sse_allows_cumulative_stream_larger_than_individual_event_budget() {
    let event_bytes = 5 * 1024 * 1024;
    let line = format!("data: {}", "x".repeat(event_bytes));
    let mut decoder = SseDecoder::default();
    let mut delivered_bytes = 0;
    let mut delivered_events = 0;
    for _ in 0..4 {
        let mut on_data = |event: String| {
            assert_eq!(event.len(), event_bytes);
            delivered_bytes += event.len();
            delivered_events += 1;
        };
        push_line_with_split_delimiters(&mut decoder, line.as_bytes(), "\r\n", &mut on_data)
            .unwrap();
        push_line_with_split_delimiters(&mut decoder, b"", "\r\n", &mut on_data).unwrap();
    }
    decoder
        .finish(&mut |_| panic!("all complete events already delivered"))
        .unwrap();
    assert_eq!(delivered_events, 4);
    assert_eq!(delivered_bytes, 20 * 1024 * 1024);
    assert!(delivered_bytes > BODY_LIMIT_BYTES);
}
#[tokio::test(start_paused = true)]
async fn done_without_eof_keeps_existing_idle_timeout_contract() {
    let chunks =
        futures::stream::once(async { Ok("data: [DONE]\n\n") }).chain(futures::stream::pending());
    let error = read_sse_chunks(chunks, 150, &mut |_| panic!("DONE is not data"))
        .await
        .unwrap_err();
    assert!(error.contains("idle timeout"));
}
