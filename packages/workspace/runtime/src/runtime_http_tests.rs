//! Transport characterization independent of database, Docker and token configuration.
use super::*;
use crate::http::dispatch_http_operation;
use axum::{extract::State, Router};
use std::convert::Infallible;

fn http_state(runtime: Arc<tokio::runtime::Runtime>) -> HttpServerState {
    let job_store = PostgresRuntimeStore::rejecting_http_test_store();
    HttpServerState {
        runtime,
        store: Arc::new(RuntimeStoreActor::start(job_store.clone()).unwrap()),
        job_store: Arc::new(job_store),
        tool_layers: Default::default(),
        tool_concurrency_registry: Default::default(),
        execution_profile: Arc::new(RuntimeExecutionProfile {
            schema: RUNTIME_EXECUTION_PROFILE_SCHEMA,
            image_digest: "sha256:synthetic-http-fixture".into(),
        }),
        native_model: None,
        request_policy: request_policy::RequestPolicy::for_http_tests(Duration::from_secs(2)),
    }
}

#[test]
fn service_tool_budgets_survive_request_state_drop_and_isolate_server_instances() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let (server, independent) = {
        let _guard = runtime.enter();
        (http_state(runtime.clone()), http_state(runtime.clone()))
    };
    let request = server.clone();
    let parent = server
        .tool_concurrency_registry
        .for_scope("session:parent", 1)
        .unwrap();
    let child = request
        .tool_concurrency_registry
        .for_scope("session:parent", 1)
        .unwrap();
    let other = independent
        .tool_concurrency_registry
        .for_scope("session:parent", 1)
        .unwrap();
    drop(request);

    runtime.block_on(async {
        let permit = parent.acquire_async().await;
        let waiting = child.acquire_async();
        tokio::pin!(waiting);
        assert!(futures::poll!(&mut waiting).is_pending());
        assert!(futures::poll!(std::pin::pin!(other.acquire_async())).is_ready());
        drop(permit);
        let _child_permit = waiting.await;
    });
}

#[test]
fn retained_hosted_context_preserves_tool_budget_across_execution_steps() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let registry = centaeris_core::runtime::ToolConcurrencyRegistry::default();
        let first_executor = registry.for_scope("session:parent", 1).unwrap();
        let mut context = crate::tests::hosted_context();
        context.tool_concurrency = first_executor.clone();
        let mut contexts = HashMap::from([("parent", context)]);
        drop(first_executor);

        // A resumed step finds the budget retained after the previous executor ended.
        let resumed = registry.for_scope("session:parent", 1).unwrap();
        let worker = contexts.get("parent").unwrap().clone();
        let permit = worker.tool_concurrency.acquire_async().await;
        {
            let waiting = resumed.acquire_async();
            tokio::pin!(waiting);
            assert!(futures::poll!(&mut waiting).is_pending());

            // Replacing/retiring the retained context does not end an in-flight clone.
            contexts.remove("parent");
            assert!(registry.for_scope("session:parent", 2).is_err());
            assert!(futures::poll!(&mut waiting).is_pending());
            drop(permit);
            drop(waiting.await);
        }
        drop(worker);
        drop(resumed);

        // Once all execution/context owners finish, the scope may be configured anew.
        let replacement = registry.for_scope("session:parent", 2).unwrap();
        assert_eq!(replacement.capacity(), 2);
    });
}

/// Storage regressions exercise the production Router with their owned database.
pub(crate) fn stored_request(
    store: PostgresRuntimeStore,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    runtime.block_on(async {
        let response = stored_response(runtime.clone(), store, method, path, body).await;
        (
            response.status().as_u16(),
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
    })
}

pub(crate) async fn stored_response(
    runtime: Arc<tokio::runtime::Runtime>,
    store: PostgresRuntimeStore,
    method: &str,
    path: &str,
    body: &[u8],
) -> Response<Body> {
    use hyper::service::Service;
    let mut state = http_state(runtime);
    state.store = Arc::new(RuntimeStoreActor::start(store.clone()).unwrap());
    state.job_store = Arc::new(store);
    state.request_policy = request_policy::RequestPolicy::from_env().unwrap();
    TowerToHyperService::new(http_router(state))
        .call(
            Request::builder()
                .method(method)
                .uri(path)
                .header(
                    "x-internal-token",
                    env::var("INTERNAL_API_TOKEN").expect("isolated fixture token"),
                )
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[test]
fn runtime_http_timeout_and_disconnect_do_not_cancel_blocking_work() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    runtime.block_on(async {
        for disconnect in [false, true] {
            let mut state = http_state(runtime.clone());
            state.request_policy =
                request_policy::RequestPolicy::for_http_tests(Duration::from_millis(100));
            let (entered, entered_rx) = tokio::sync::oneshot::channel();
            let (finished, mut finished_rx) = tokio::sync::oneshot::channel();
            let (release, release_rx) = std::sync::mpsc::channel();
            let task = tokio::spawn(dispatch_http_operation(
                State(state),
                Request::builder()
                    .method("POST")
                    .uri("/skills/catalog")
                    .body(Body::empty())
                    .unwrap(),
                move |_, _| {
                    entered.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    finished.send(()).unwrap();
                    Ok(http_response(200, "text/plain", b"completed".to_vec()))
                },
            ));
            tokio::time::timeout(Duration::from_secs(3), entered_rx)
                .await
                .unwrap()
                .unwrap();
            if disconnect {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                assert_eq!(task.await.unwrap().status(), 504);
            }
            assert!(matches!(
                finished_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(3), finished_rx)
                .await
                .unwrap()
                .unwrap();
        }
    });
}

#[test]
fn runtime_http_concurrent_handlers_progress_independently() {
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    runtime.block_on(async {
        let state = http_state(runtime.clone());
        let entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let changed = Arc::new(tokio::sync::Notify::new());
        let (release, receiver) = std::sync::mpsc::channel();
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let entered = entered.clone();
            let changed = changed.clone();
            let receiver = receiver.clone();
            tasks.push(tokio::spawn(dispatch_http_operation(
                State(state.clone()),
                Request::builder()
                    .method("POST")
                    .uri("/agent-runs/step")
                    .body(Body::empty())
                    .unwrap(),
                move |_, _| {
                    entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    changed.notify_one();
                    receiver
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap();
                    Ok(http_response(200, "text/plain", b"completed".to_vec()))
                },
            )));
        }
        let all_entered = tokio::time::timeout(Duration::from_secs(3), async {
            while entered.load(std::sync::atomic::Ordering::SeqCst) < 32 {
                changed.notified().await;
            }
        })
        .await
        .is_ok();
        for _ in 0..32 {
            release.send(()).unwrap();
        }
        assert!(
            all_entered,
            "concurrent HTTP work must not be rejected by a local request quota"
        );
        for task in tasks {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .status(),
                200
            );
        }
    });
}

#[test]
fn runtime_http_accepts_new_requests_with_many_incomplete_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/", axum::routing::get(|| async { "ready" }));
        let server = tokio::spawn(accept_http_connections(listener, router));
        let mut pending = Vec::new();
        for _ in 0..128 {
            pending.push(tokio::net::TcpStream::connect(address).await.unwrap());
        }
        let mut request = tokio::net::TcpStream::connect(address).await.unwrap();
        request
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        let result =
            tokio::time::timeout(HTTP_HEADER_TIMEOUT / 2, request.read_to_end(&mut response)).await;
        drop(pending);
        server.abort();
        result
            .expect("a complete request must progress independently of idle connections")
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert!(response.ends_with(b"ready"));
    });
}

async fn router_request(router: &Router, request: Request<Body>) -> (u16, serde_json::Value) {
    use hyper::service::Service;
    let method = request.method().to_string();
    let target = request.uri().to_string();
    let response = TowerToHyperService::new(router.clone())
        .call(request)
        .await
        .unwrap();
    let status = response.status().as_u16();
    assert_eq!(
        response.headers()[header::CONNECTION],
        "close",
        "{method} {target}"
    );
    assert!(
        !response.headers().contains_key(header::ALLOW),
        "{method} {target}"
    );
    let headers: std::collections::BTreeMap<_, _> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.to_str().unwrap().to_owned()))
        .collect();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        json!({"method": method, "target": target, "status": status,
        "headers": headers, "body": String::from_utf8(body.to_vec()).unwrap()}),
    )
}

#[test]
fn runtime_http_router_contract() {
    let mode = match env::var("CENTAERIS_HTTP_CONTRACT_CHILD") {
        Ok(mode) => mode,
        Err(_) => {
            // Configuration is process-wide. Each case runs alone without changing
            // environment variables under other parallel Rust tests.
            for mode in ["configured", "missing", "empty"] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "runtime_http_tests::runtime_http_router_contract",
                        "--nocapture",
                    ])
                    .env("CENTAERIS_HTTP_CONTRACT_CHILD", mode);
                match mode {
                    "missing" => {
                        command.env_remove("INTERNAL_API_TOKEN");
                    }
                    "empty" => {
                        command.env("INTERNAL_API_TOKEN", "");
                    }
                    _ => {
                        command.env("INTERNAL_API_TOKEN", "synthetic-http-token");
                    }
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "{mode}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }
    };
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    runtime.block_on(async {
        let state = http_state(runtime.clone());
        let router = http_router(state.clone());
        let mut records = Vec::new();
        // Exercise the documented surface instead of maintaining a second route list.
        let routes: Vec<_> = include_str!("../../../../docs/workspace/reference/RuntimeHTTP.md")
            .lines()
            .filter(|line| line.starts_with("| GET |") || line.starts_with("| POST |"))
            .map(|line| {
                let fields: Vec<_> = line.split('|').collect();
                (
                    fields[1].trim(),
                    fields[2]
                        .trim()
                        .trim_matches('`')
                        .replace("{jobId}", "bad%2Fid"),
                )
            })
            .collect();
        assert_eq!(routes.len(), 31);
        for (method, path) in routes {
            let jobs =
                path.starts_with("/internal/jobs") || path.starts_with("/internal/job-outbox");
            for token in [
                None,
                Some("wrong-token"),
                Some(if mode == "empty" {
                    ""
                } else {
                    "synthetic-http-token"
                }),
            ] {
                let mut request = Request::builder().method(method).uri(&path);
                if let Some(token) = token {
                    request = request.header("x-internal-token", token);
                }
                let (status, record) =
                    router_request(&router, request.body(Body::from("{")).unwrap()).await;
                let expected = if mode == "missing" || (jobs && mode == "empty") {
                    500
                } else if token
                    != Some(if mode == "empty" {
                        ""
                    } else {
                        "synthetic-http-token"
                    })
                {
                    401
                } else if method == "POST" || jobs {
                    400
                } else {
                    200
                };
                assert_eq!(status, expected, "{method} {path}: {record}");
                records.push(record);
            }
            for (other_method, target) in [
                ("HEAD", path.clone()),
                ("OPTIONS", path.clone()),
                ("PUT", path.clone()),
                (method, format!("{path}?ignored=1")),
                (method, format!("{path}/")),
            ] {
                let (status, record) = router_request(
                    &router,
                    Request::builder()
                        .method(other_method)
                        .uri(&target)
                        .header(
                            "x-internal-token",
                            if mode == "empty" {
                                ""
                            } else {
                                "synthetic-http-token"
                            },
                        )
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await;
                let expected = if jobs && mode != "configured" {
                    500
                } else if jobs && other_method == "GET" {
                    400
                } else {
                    404
                };
                assert_eq!(status, expected, "{other_method} {target}: {record}");
                records.push(record);
            }
        }
        for (method, path) in [
            ("GET", "/banana"),
            ("POST", "/internal/jobsXYZ"),
            ("POST", "/internal/job-outboxXYZ"),
            ("GET", "/internal/jobs/"),
            ("GET", "/internal/jobs/internal/jobs/bad%2Fid"),
            ("GET", "/internal/%6dodel-catalog"),
            ("POST", "/skills%2Fcatalog"),
        ] {
            let (status, record) = router_request(
                &router,
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("x-internal-token", "synthetic-http-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            let jobs =
                path.starts_with("/internal/jobs") || path.starts_with("/internal/job-outbox");
            let expected = if jobs && mode != "configured" {
                500
            } else if jobs && method == "GET" && path.starts_with("/internal/jobs/") {
                400
            } else {
                404
            };
            assert_eq!(status, expected, "{method} {path}: {record}");
            records.push(record);
        }
        // Admission and body validation still precede routing and authentication.
        for (body, expected) in [(vec![], 400), (vec![0; MAX_HTTP_BODY_BYTES + 1], 413)] {
            let (status, record) = router_request(
                &router,
                Request::builder()
                    .method("POST")
                    .uri("/banana")
                    .header("x-test", "one")
                    .header("X-Test", "two")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await;
            assert_eq!(status, expected);
            records.push(record);
        }
        if let Ok(directory) = env::var("CENTAERIS_HTTP_CONTRACT_RECORD_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                Path::new(&directory).join(format!("{mode}.json")),
                serde_json::to_vec_pretty(&records).unwrap(),
            )
            .unwrap();
        }
    });
}

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
                    http_response(status, content_type, body.to_vec()),
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
                bounded_json_error_response(503, error),
                503,
                "application/json",
                &expected,
            )
            .await;
            assert_response(
                json_error_response(400, error).unwrap(),
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
            agent_run_step_failure_response("run_test", "execution", true, "test_reason").unwrap(),
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
                .unwrap_err();
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
            read_axum_request(request).await.unwrap_err(),
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
        assert_eq!(
            parsed.uri().path_and_query().unwrap().as_str(),
            "/internal/transcript/page?cursor=1"
        );
        assert_eq!(parsed.body().len(), MAX_HTTP_BODY_BYTES);
        let parsed = read_axum_request(
            Request::builder()
                .method("GET")
                .uri("/internal/model-catalog")
                .header("X-Internal-Token", "synthetic-token")
                .header("Content-Type", "text/plain")
                .body(Body::from("unchanged body"))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(parsed.method(), "GET");
        assert_eq!(
            parsed.headers().get("x-internal-token").unwrap(),
            "synthetic-token"
        );
        assert_eq!(parsed.headers().get("content-type").unwrap(), "text/plain");
        assert_eq!(parsed.body().as_ref(), b"unchanged body");
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
                Ok::<_, Infallible>(bounded_json_error_response(503, "runtime_busy"))
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
