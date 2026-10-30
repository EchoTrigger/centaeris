use super::*;
use crate::runtime_rpc_transport::RuntimeServerClientHub;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

type Capture = (usize, Vec<(String, String)>);

async fn keepalive_server() -> (
    String,
    mpsc::Receiver<Capture>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel(32);
    let (shutdown, mut stop) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut id = 0;
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (socket, _) = tokio::select! {
                accepted = listener.accept() => accepted.unwrap(),
                completed = connections.join_next(), if !connections.is_empty() => {
                    completed.unwrap().expect("keepalive connection task failed");
                    continue;
                }
                _ = &mut stop => break,
            };
            id += 1;
            let tx = tx.clone();
            connections.spawn(async move {
                let mut socket = BufReader::new(socket);
                loop {
                    let mut line = String::new();
                    if socket.read_line(&mut line).await.unwrap() == 0 {
                        return;
                    }
                    let streaming = line.contains("/sse ");
                    let incomplete = line.contains("/incomplete ");
                    let pending = line.contains("/pending ");
                    let mut headers = Vec::new();
                    loop {
                        line.clear();
                        if socket.read_line(&mut line).await.unwrap() == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        let (name, value) =
                            line.trim_end_matches(['\r', '\n']).split_once(':').unwrap();
                        headers.push((
                            name.to_string(),
                            value.strip_prefix(' ').unwrap_or(value).to_string(),
                        ));
                    }
                    let length = headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.parse::<usize>().unwrap())
                        .unwrap_or(0);
                    socket.read_exact(&mut vec![0; length]).await.unwrap();
                    tx.send((id, headers)).await.unwrap();
                    if incomplete {
                        socket.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n{").await.unwrap();
                        return;
                    }
                    if pending {
                        // Cancellation may discard this connection; another request
                        // must remain able to obtain capacity and use the same Client.
                        let mut tail = Vec::new();
                        let _ = socket.read_to_end(&mut tail).await;
                        return;
                    }
                    let (media, body) = if streaming {
                        ("text/event-stream", "data: hello\n\ndata: [DONE]\n\n")
                    } else {
                        ("application/json", "{}")
                    };
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {media}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
                    if socket
                        .get_mut()
                        .write_all(response.as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        connections.abort_all();
        while let Some(completed) = connections.join_next().await {
            if let Err(error) = completed {
                assert!(
                    error.is_cancelled(),
                    "keepalive connection task panicked: {error}"
                );
            }
        }
    });
    (url, rx, shutdown, task)
}

fn values(headers: &[(String, String)], name: &str) -> Vec<String> {
    let mut values: Vec<_> = headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .collect();
    values.sort();
    values
}

async fn send(
    transport: &ReqwestJsonHttpTransport,
    url: &str,
    streaming: bool,
    headers: HashMap<String, String>,
) {
    let request = JsonHttpRequest {
        method: "POST".to_string(),
        url: format!("{url}/{}", if streaming { "sse" } else { "json" }),
        headers,
        timeout_ms: 5000,
        sse_idle_timeout_ms: 5000,
        max_retries: 0,
        retry_backoff_ms: 0,
        body_json: "{}".into(),
    };
    if streaming {
        let mut events = Vec::new();
        transport
            .execute_sse(&request, &mut |event| events.push(event))
            .await
            .unwrap();
        assert_eq!(events, ["hello"]);
    } else {
        assert_eq!(
            transport.execute_json(&request).await.unwrap().body_json,
            "{}"
        );
    }
}

#[test]
fn session_validation_is_provider_scoped() {
    for session in ["", "  ", "bad\nvalue"] {
        assert!(ReqwestJsonHttpTransport::for_session(
            session,
            "opencode-go.default",
            build_model_http_client,
        )
        .is_err());
        assert!(ReqwestJsonHttpTransport::for_session(
            session,
            "openai.default",
            build_model_http_client,
        )
        .is_ok());
    }
}

#[tokio::test]
async fn session_headers_preserve_raw_values_and_explicit_case_variants() {
    let hub = RuntimeServerClientHub::default();
    let (url, mut captures, shutdown, server) = keepalive_server().await;
    for streaming in [false, true] {
        for explicit in [false, true] {
            let transport = ReqwestJsonHttpTransport::for_session(
                "  session-a  ",
                "opencode-go.default",
                || hub.model_http_client(),
            )
            .unwrap();
            let headers = if explicit {
                HashMap::from([
                    ("x-opencode-session".to_string(), "first".to_string()),
                    ("X-OpenCode-Session".to_string(), "second".to_string()),
                    ("uSeR-aGeNt".to_string(), "custom-agent".to_string()),
                ])
            } else {
                HashMap::new()
            };
            send(&transport, &url, streaming, headers).await;
            let (_, wire) = captures.recv().await.unwrap();
            assert_eq!(
                values(&wire, "x-opencode-session"),
                if explicit {
                    vec!["first".to_string(), "second".to_string()]
                } else {
                    vec!["  session-a  ".to_string()]
                }
            );
            assert_eq!(
                values(&wire, "user-agent"),
                vec![if explicit {
                    "custom-agent".to_string()
                } else {
                    concat!("centaeris/", env!("CARGO_PKG_VERSION")).to_string()
                }]
            );
        }
    }
    shutdown.send(()).unwrap();
    server.await.expect("keepalive server failed");
}

#[tokio::test]
async fn different_session_transports_reuse_keepalive_connection_without_header_leaks() {
    let hub = RuntimeServerClientHub::default();
    let (url, mut captures, shutdown, server) = keepalive_server().await;
    let mut connection = None;
    for (session, provider, streaming, key) in [
        ("session-a", "opencode-go.default", false, "fake-a"),
        ("session-b", "opencode-go.default", true, "fake-b"),
        ("unused", "openai.default", false, "fake-c"),
    ] {
        let transport =
            ReqwestJsonHttpTransport::for_session(session, provider, || hub.model_http_client())
                .unwrap();
        send(
            &transport,
            &url,
            streaming,
            HashMap::from([("x-api-key".to_string(), key.to_string())]),
        )
        .await;
        let (id, headers) = captures.recv().await.unwrap();
        if let Some(expected) = connection {
            assert_eq!(
                id, expected,
                "new session must reuse the existing connection"
            );
        } else {
            connection = Some(id);
        }
        assert_eq!(values(&headers, "x-api-key"), vec![key]);
        assert_eq!(
            values(&headers, "x-opencode-session"),
            if provider == "opencode-go.default" {
                vec![session]
            } else {
                vec![]
            }
        );
        drop(transport);
    }
    let fresh_hub = RuntimeServerClientHub::default();
    let fresh = ReqwestJsonHttpTransport::for_session("session-c", "opencode-go.default", || {
        fresh_hub.model_http_client()
    })
    .unwrap();
    send(&fresh, &url, false, HashMap::new()).await;
    let (fresh_connection, headers) = captures.recv().await.unwrap();
    assert_ne!(
        Some(fresh_connection),
        connection,
        "a different Runtime owner needs an independent pool"
    );
    assert_eq!(values(&headers, "x-opencode-session"), vec!["session-c"]);
    shutdown.send(()).unwrap();
    server.await.expect("keepalive server failed");
}

#[test]
fn invalid_opencode_session_precedes_lazy_client_failure() {
    for (session, expected) in [
        ("", "OpenCode requests require a conversation session ID"),
        ("  ", "OpenCode requests require a conversation session ID"),
        ("bad\nvalue", "invalid conversation session ID for OpenCode"),
    ] {
        let result = ReqwestJsonHttpTransport::for_session(session, "opencode-go.default", || {
            panic!("client must not be built for an invalid session")
        });
        assert_eq!(result.err().expect("invalid session must fail"), expected);
    }
    let result = ReqwestJsonHttpTransport::for_session("session", "opencode-go.default", || {
        Err("injected build failure".into())
    });
    assert_eq!(result.err().unwrap(), "injected build failure");
    let result = ReqwestJsonHttpTransport::for_session("bad\nvalue", "openai.default", || {
        Err("injected build failure".into())
    });
    assert_eq!(result.err().unwrap(), "injected build failure");
}

#[tokio::test]
async fn concurrent_shared_pool_requests_keep_session_and_credentials_request_local() {
    let hub = RuntimeServerClientHub::default();
    let (url, mut captures, shutdown, server) = keepalive_server().await;
    let a = ReqwestJsonHttpTransport::for_session("a", "opencode-go.default", || {
        hub.model_http_client()
    })
    .unwrap();
    let b = ReqwestJsonHttpTransport::for_session("b", "opencode-go.default", || {
        hub.model_http_client()
    })
    .unwrap();
    let c = ReqwestJsonHttpTransport::for_session("unused", "openai.default", || {
        hub.model_http_client()
    })
    .unwrap();
    let d = ReqwestJsonHttpTransport::for_session("d", "opencode-go.default", || {
        hub.model_http_client()
    })
    .unwrap();
    tokio::join!(
        send(
            &a,
            &url,
            false,
            HashMap::from([("x-api-key".into(), "fake-a".into())])
        ),
        send(
            &b,
            &url,
            true,
            HashMap::from([("x-api-key".into(), "fake-b".into())])
        ),
        send(
            &c,
            &url,
            true,
            HashMap::from([("x-api-key".into(), "fake-c".into())])
        ),
        send(
            &d,
            &url,
            false,
            HashMap::from([
                ("x-api-key".into(), "fake-d".into()),
                ("x-opencode-session".into(), "first".into()),
                ("X-OpenCode-Session".into(), "second".into()),
            ])
        ),
    );
    let mut keys = Vec::new();
    for _ in 0..4 {
        let (_, headers) = captures.recv().await.unwrap();
        let key = values(&headers, "x-api-key");
        assert_eq!(key.len(), 1);
        let expected = match key[0].as_str() {
            "fake-a" => vec!["a"],
            "fake-b" => vec!["b"],
            "fake-c" => vec![],
            "fake-d" => vec!["first", "second"],
            other => panic!("unexpected fake credential: {other}"),
        };
        assert_eq!(values(&headers, "x-opencode-session"), expected);
        keys.push(key[0].clone());
    }
    keys.sort();
    assert_eq!(keys, ["fake-a", "fake-b", "fake-c", "fake-d"]);
    let _ = shutdown.send(());
    server.await.expect("keepalive server failed");
}

#[test]
fn runtime_client_samples_proxy_environment_only_on_first_success() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args([
        "--exact",
        "http_transport::client_tests::proxy_snapshot_child",
        "--ignored",
        "--test-threads=1",
        "--nocapture",
    ]);
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
        "REQUEST_METHOD",
    ] {
        child.env_remove(name);
    }
    child.env("CENTAERIS_MODEL_PROXY_TEST_CHILD", "1");
    let result = child.output().unwrap();
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("MODEL_PROXY_SNAPSHOT_OK"),
        "proxy child did not complete its assertions: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success(),
        "isolated proxy test failed: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
#[ignore = "runs in the isolated proxy-environment child process"]
async fn proxy_snapshot_child() {
    tokio::time::timeout(Duration::from_secs(20), async {
        assert_eq!(
            std::env::var("CENTAERIS_MODEL_PROXY_TEST_CHILD").unwrap(),
            "1"
        );
        let (proxy_a, mut a, shutdown_a, server_a) = keepalive_server().await;
        let (proxy_b, mut b, shutdown_b, server_b) = keepalive_server().await;
        // The destination is also loopback, but remains unopened: only the selected
        // fake proxy should receive requests. No public network or real key is used.
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", destination.local_addr().unwrap());
        std::env::set_var("HTTP_PROXY", proxy_a);
        let hub = RuntimeServerClientHub::default();
        let first = ReqwestJsonHttpTransport::for_session("first", "opencode-go.default", || {
            hub.model_http_client()
        })
        .unwrap();
        send(
            &first,
            &url,
            false,
            HashMap::from([("x-api-key".into(), "fake-first".into())]),
        )
        .await;
        let (connection, headers) = a.recv().await.unwrap();
        assert_eq!(values(&headers, "x-api-key"), vec!["fake-first"]);
        std::env::set_var("HTTP_PROXY", proxy_b);
        let same_owner =
            ReqwestJsonHttpTransport::for_session("same", "opencode-go.default", || {
                hub.model_http_client()
            })
            .unwrap();
        send(
            &same_owner,
            &url,
            true,
            HashMap::from([("x-api-key".into(), "fake-same".into())]),
        )
        .await;
        let (reused, headers) = a.recv().await.unwrap();
        assert_eq!(reused, connection);
        assert_eq!(values(&headers, "x-api-key"), vec!["fake-same"]);
        assert!(b.try_recv().is_err());
        let fresh_hub = RuntimeServerClientHub::default();
        let fresh = ReqwestJsonHttpTransport::for_session("fresh", "opencode-go.default", || {
            fresh_hub.model_http_client()
        })
        .unwrap();
        send(
            &fresh,
            &url,
            false,
            HashMap::from([("x-api-key".into(), "fake-fresh".into())]),
        )
        .await;
        let (_, headers) = b.recv().await.unwrap();
        assert_eq!(values(&headers, "x-api-key"), vec!["fake-fresh"]);
        assert_eq!(values(&headers, "x-opencode-session"), vec!["fresh"]);
        assert!(a.try_recv().is_err());
        assert!(
            futures::FutureExt::now_or_never(destination.accept()).is_none(),
            "destination must not be contacted directly"
        );
        let _ = shutdown_a.send(());
        let _ = shutdown_b.send(());
        server_a.await.unwrap();
        server_b.await.unwrap();
    })
    .await
    .expect("isolated proxy snapshot test timed out");
    println!("MODEL_PROXY_SNAPSHOT_OK");
}

#[tokio::test]
async fn cancelled_or_failed_request_does_not_poison_shared_client() {
    let hub = RuntimeServerClientHub::default();
    let transport = ReqwestJsonHttpTransport::for_session("session", "opencode-go.default", || {
        hub.model_http_client()
    })
    .unwrap();
    let (url, mut captures, shutdown, server) = keepalive_server().await;
    let mut request = JsonHttpRequest {
        method: "POST".into(),
        url: format!("{url}/incomplete"),
        headers: HashMap::new(),
        timeout_ms: 5000,
        sse_idle_timeout_ms: 5000,
        max_retries: 0,
        retry_backoff_ms: 0,
        body_json: "{}".into(),
    };
    assert!(transport.execute_json(&request).await.is_err());
    captures.recv().await.unwrap();
    request.url = format!("{url}/pending");
    {
        let pending = transport.execute_json(&request);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("pending request unexpectedly completed: {}", result.is_ok()),
            accepted = captures.recv() => { accepted.unwrap(); }
        }
        // Dropping the caller future cancels this attempt.
    }
    send(&transport, &url, false, HashMap::new()).await;
    let (_, headers) = captures.recv().await.unwrap();
    assert_eq!(values(&headers, "x-opencode-session"), vec!["session"]);
    let _ = shutdown.send(());
    server.await.unwrap();
}
