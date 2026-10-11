//! Transport characterization independent of database, Docker and token configuration.
use super::*;
use std::convert::Infallible;

async fn assert_response(response: Response<Body>, status: u16, content_type: &str, body: &[u8]) {
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
    assert_eq!(response.headers()[header::CONNECTION], "close");
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|v| v.to_str().unwrap()),
        matches!(status, 429 | 503).then_some("5"),
    );
    assert_eq!(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        body
    );
}

#[test]
fn runtime_http_response_contract() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for status in [200, 202, 400, 401, 404, 409, 413, 429, 500, 503, 504] {
            for (content_type, body) in [
                ("text/plain", b"not_found".as_slice()),
                ("application/json", br#"{"schema":"test.v1"}"#.as_slice()),
            ] {
                assert_response(
                    http_response(status, content_type, body.to_vec()).into_axum_response(),
                    status,
                    content_type,
                    body,
                )
                .await;
            }
        }
        for error in ["unauthorized", "runtime_busy", "quote\" newline\n 中文"] {
            let expected = serde_json::to_vec(&json!({"error": error})).unwrap();
            assert_response(
                bounded_json_error_response(503, error).into_axum_response(),
                503,
                "application/json",
                &expected,
            )
            .await;
            assert_response(
                json_error_response(400, error)
                    .unwrap()
                    .into_axum_response(),
                400,
                "application/json",
                &expected,
            )
            .await;
        }
        let expected = serde_json::to_vec(&json!({
            "schema": "runtime.agent_run.step.failure.v1",
            "agentRunId": "run_test", "failureClass": "execution",
            "retryable": true, "transitionReason": "test_reason",
            "error": "runtime_step_failed",
        }))
        .unwrap();
        assert_response(
            agent_run_step_failure_response("run_test", "execution", true, "test_reason")
                .unwrap()
                .into_axum_response(),
            500,
            "application/json",
            &expected,
        )
        .await;
    });
}

#[test]
fn runtime_http_body_and_header_rejection_order() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let duplicate_headers = || {
            Request::builder()
                .method("POST")
                .uri("/internal/transcript/page?cursor=1")
                .header("x-internal-token", "first")
                .header("X-Internal-Token", "second")
        };
        for (body, status, error) in [
            (
                vec![0; MAX_HTTP_BODY_BYTES + 1],
                413,
                "request_body_too_large",
            ),
            (vec![], 400, "request_header_duplicate"),
        ] {
            let response = read_axum_request(duplicate_headers().body(Body::from(body)).unwrap())
                .await
                .unwrap_err()
                .into_axum_response();
            assert_response(
                response,
                status,
                "application/json",
                &serde_json::to_vec(&json!({"error": error})).unwrap(),
            )
            .await;
        }
        let request = Request::builder()
            .header("x-invalid", [0xff].as_slice())
            .body(Body::empty())
            .unwrap();
        assert_response(
            read_axum_request(request)
                .await
                .unwrap_err()
                .into_axum_response(),
            400,
            "application/json",
            br#"{"error":"request_header_invalid"}"#,
        )
        .await;
        // No Content-Type is required and the complete target is retained for dispatch.
        let parsed = read_axum_request(
            Request::builder()
                .method("POST")
                .uri("/internal/transcript/page?cursor=1")
                .body(Body::from(vec![0; MAX_HTTP_BODY_BYTES]))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(parsed.path, "/internal/transcript/page?cursor=1");
        assert_eq!(parsed.body.len(), MAX_HTTP_BODY_BYTES);
    });
}

#[test]
fn runtime_http_error_response_crosses_hyper_with_retry_headers() {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(|_| async {
                Ok::<_, Infallible>(bounded_json_error_response(503, "runtime_busy").into_axum_response())
            });
            runtime_http1_builder(HTTP_HEADER_TIMEOUT)
                .serve_connection(TokioIo::new(stream), service).await.unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client.write_all(b"POST /internal/jobs/claim HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n").await.unwrap();
        let mut wire = String::new();
        tokio::time::timeout(Duration::from_secs(5), client.read_to_string(&mut wire)).await.unwrap().unwrap();
        server.await.unwrap();
        let (headers, body) = wire.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        let headers = headers.to_ascii_lowercase();
        assert!(headers.contains("\r\nconnection: close"));
        assert!(headers.contains("\r\nretry-after: 5"));
        assert!(headers.contains("\r\ncontent-type: application/json"));
        assert_eq!(body, r#"{"error":"runtime_busy"}"#);
    });
}
