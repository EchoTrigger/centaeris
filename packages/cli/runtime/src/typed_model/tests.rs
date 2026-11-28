use super::*;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn input(operation: &str) -> EvaluateTypedRequest {
    serde_json::from_value(json!({
        "operationId":operation,"sessionId":"session-typed","state":"{\"task\":\"inspect\"}",
        "questions":{"relation":{"type":"choice","instructions":"Compare with task","criteria":{"withinTask":"Within requested scope","outsideTask":"Outside requested scope"}}}
    })).unwrap()
}

async fn fixture(body: Value) -> (TypedModelConfig, tokio::task::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/native-evaluate", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let header_end;
        loop {
            let mut chunk = [0; 4096];
            let size = stream.read(&mut chunk).await.unwrap();
            assert!(size > 0, "HTTP request closed before headers");
            bytes.extend_from_slice(&chunk[..size]);
            if let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                header_end = offset + 4;
                break;
            }
        }
        let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .unwrap();
        while bytes.len() < header_end + length {
            let mut chunk = [0; 4096];
            let size = stream.read(&mut chunk).await.unwrap();
            assert!(size > 0, "HTTP request closed before body");
            bytes.extend_from_slice(&chunk[..size]);
        }
        let request = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
        let body = body.to_string();
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Request-Id: upstream-fixture-1\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(response.as_bytes()).await.unwrap();
        (headers, request)
    });
    (
        TypedModelConfig {
            url,
            key: "fixture-key".into(),
            model: "jev-fixture".into(),
        },
        server,
    )
}

#[tokio::test]
async fn typed_model_http_fixture_persists_usage_identity_and_replays_without_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let native = json!({"id":"native-response-1","model":"jev-fixture","answers":{"relation":{"type":"choice","choice":"withinTask","confidence":0.9}},"usage":{"input_tokens":53,"output_tokens":8,"total_tokens":61,"provider_extra":{"cached":3}}});
    let (config, server) = fixture(native.clone()).await;
    let transport = ReqwestJsonHttpTransport::for_session("session-typed", "typed-model", || {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| e.to_string())
    })
    .unwrap();
    let receipt = evaluate_with(input("operation-1"), &config, root.path(), &transport)
        .await
        .unwrap();
    let (headers, sent) = server.await.unwrap();
    assert!(headers.starts_with("POST /native-evaluate HTTP/1.1"));
    assert!(headers
        .to_ascii_lowercase()
        .contains(&format!("x-request-id: {}", receipt.request_id)));
    assert!(headers
        .to_ascii_lowercase()
        .contains("authorization: bearer fixture-key"));
    assert_eq!(sent, serde_json::to_value(&receipt.request).unwrap());
    assert!(!sent.as_object().unwrap().contains_key("messages"));
    assert_eq!(receipt.response_id.as_deref(), Some("native-response-1"));
    assert_eq!(
        receipt.upstream_request_id.as_deref(),
        Some("upstream-fixture-1")
    );
    assert_eq!(receipt.usage, Some(native["usage"].clone()));
    let directory = PathBuf::from(&receipt.record_path);
    let stored: TypedAttemptRequest = read_record(&directory.join("request.json")).unwrap();
    let raw: Value = read_record(&directory.join("raw-response.json")).unwrap();
    assert_eq!(stored.request_id, receipt.request_id);
    assert_eq!(
        serde_json::from_str::<Value>(raw["body"].as_str().unwrap()).unwrap(),
        native
    );
    assert!(!fs::read_to_string(directory.join("request.json"))
        .unwrap()
        .contains("fixture-key"));
    // The fixture listener has gone away: a replay must resolve only from CET data.
    let replay = evaluate_with(input("operation-1"), &config, root.path(), &transport)
        .await
        .unwrap();
    assert_eq!(replay.request_id, receipt.request_id);
    assert_eq!(replay.attempt_id, receipt.attempt_id);
    let mut changed = input("operation-1");
    changed.state = json!({"task":"changed"});
    assert_eq!(
        evaluate_with(changed, &config, root.path(), &transport)
            .await
            .unwrap_err()
            .code(),
        "typed_model_operation_conflict"
    );
}

#[tokio::test]
async fn typed_model_rejects_invalid_answers_and_never_redispatches_incomplete_operation() {
    let root = tempfile::tempdir().unwrap();
    let (config, server) = fixture(json!({"model":"jev-fixture","answers":{"relation":{"type":"choice","choice":"free-text-invention"}},"usage":{"total_tokens":9}})).await;
    let transport = ReqwestJsonHttpTransport::for_session("session-typed", "typed-model", || {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| e.to_string())
    })
    .unwrap();
    let error = evaluate_with(input("operation-bad"), &config, root.path(), &transport)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "typed_model_invalid_response");
    server.await.unwrap();
    let directory = fs::read_dir(root.path().join("runtime/typed-model"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(directory.join("raw-response.json").exists());
    assert!(directory.join("failure.json").exists());
    assert!(!directory.join("receipt.json").exists());
    let operation = read_operation_at(
        &ReadTypedOperationRequest {
            operation_id: "operation-bad".into(),
            session_id: "session-typed".into(),
        },
        root.path(),
    )
    .unwrap()
    .unwrap();
    let stored: TypedAttemptRequest = read_record(&directory.join("request.json")).unwrap();
    assert_eq!(operation.attempt_id, stored.attempt_id);
    assert_eq!(operation.request_id, stored.request_id);
    assert_eq!(operation.request, stored.request);
    assert_eq!(PathBuf::from(&operation.record_path), directory);
    assert!(operation.receipt.is_none());
    assert_eq!(operation.failure.as_ref().unwrap()["stage"], "validation");
    let raw = operation.raw_response.as_ref().unwrap();
    assert_eq!(raw.status_code, 200);
    let original: Value = serde_json::from_str(&raw.body).unwrap();
    assert_eq!(original["usage"], json!({"total_tokens":9}));
    assert_eq!(
        original["answers"]["relation"]["choice"],
        "free-text-invention"
    );
    assert!(read_operation_at(
        &ReadTypedOperationRequest {
            operation_id: "never-dispatched".into(),
            session_id: "session-typed".into()
        },
        root.path()
    )
    .unwrap()
    .is_none());
    assert_eq!(
        evaluate_with(input("operation-bad"), &config, root.path(), &transport)
            .await
            .unwrap_err()
            .code(),
        "typed_model_operation_indeterminate"
    );
    for bad in [
        json!({"operationId":"a","sessionId":"s","state":{},"questions":{"q":{"type":"score","instructions":"score","criteria":{"a":"A"}}}}),
        json!({"operationId":"a","sessionId":"s","state":{},"questions":{},"old_field":true}),
    ] {
        assert!(serde_json::from_value::<EvaluateTypedRequest>(bad).is_err());
    }
}

#[tokio::test]
async fn typed_model_admits_one_dispatch_and_exposes_complete_in_flight_state() {
    use centaeris_core::model::{JsonHttpFuture, JsonHttpRequest};
    use std::time::Duration;
    use tokio::sync::Notify;

    #[derive(Default)]
    struct PausedTransport {
        dispatched: Notify,
        resume: Notify,
    }

    impl JsonHttpTransport for PausedTransport {
        fn execute_json<'a>(&'a self, _request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
            Box::pin(async move {
                self.dispatched.notify_one();
                self.resume.notified().await;
                Ok(JsonHttpResponse {
                    timings: None,
                    status_code: 200,
                    headers: Default::default(),
                    body_json: json!({
                        "model": "jev-fixture",
                        "answers": {"relation": {"type": "choice", "choice": "withinTask"}}
                    })
                    .to_string(),
                })
            })
        }
    }

    let root = tempfile::tempdir().unwrap();
    let config = TypedModelConfig {
        url: "https://example.invalid/evaluate".into(),
        key: String::new(),
        model: "jev-fixture".into(),
    };
    let transport = PausedTransport::default();
    let input = input("operation-pending");
    let read = ReadTypedOperationRequest {
        session_id: input.session_id.clone(),
        operation_id: input.operation_id.clone(),
    };
    let inspect = async {
        transport.dispatched.notified().await;
        let pending = read_operation_at(&read, root.path()).unwrap().unwrap();
        assert!(pending.receipt.is_none());
        assert!(pending.raw_response.is_none());
        let duplicate = evaluate_with(input.clone(), &config, root.path(), &transport)
            .await
            .unwrap_err();
        assert_eq!(duplicate.code(), "typed_model_operation_indeterminate");
        transport.resume.notify_one();
        pending.request_id
    };
    let (receipt, pending_request_id) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            evaluate_with(input.clone(), &config, root.path(), &transport),
            inspect
        )
    })
    .await
    .unwrap();
    let receipt = receipt.unwrap();
    assert_eq!(receipt.request_id, pending_request_id);
    let completed = read_operation_at(&read, root.path()).unwrap().unwrap();
    assert_eq!(completed.raw_response.unwrap().status_code, 200);
    assert_eq!(completed.receipt.unwrap().request_id, pending_request_id);
}

#[test]
fn typed_rpc_envelope_rejects_unknown_fields() {
    let request = serde_json::to_value(input("strict-operation")).unwrap();
    assert!(parse_request(json!({"request":request})).is_ok());
    assert!(parse_request(json!({"request":request,"legacy":true})).is_err());
    assert!(parse_request(request).is_err());
    assert!(parse_read_request(
        json!({"request":{"operationId":"strict-operation","sessionId":"session-typed"}})
    )
    .is_ok());
    assert!(parse_read_request(json!({"request":{"operationId":"strict-operation","sessionId":"session-typed","model":"old"}})).is_err());
    assert!(parse_read_request(json!({"request":{"operationId":"strict-operation","sessionId":"session-typed"},"legacy":true})).is_err());
}
