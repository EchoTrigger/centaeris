use super::tests::{model_client, model_request, read_http_request};
use super::*;
use crate::hosted_model_admission::HostedModelAdmission;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

struct ObservedRequest {
    wire: serde_json::Value,
    stream: TcpStream,
}

struct Server {
    url: String,
    requests: mpsc::Receiver<ObservedRequest>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (send, requests) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            let mut readers = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let send = send.clone();
                readers.spawn(async move {
                    let wire = read_http_request(&mut stream).await;
                    let _ = send.send(ObservedRequest { wire, stream }).await;
                });
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
    async fn next(&mut self) -> ObservedRequest {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn assert_no_dispatch(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(100), self.requests.recv())
                .await
                .is_err(),
            "a queued request dispatched before capacity was returned"
        );
    }
}

fn client(
    server: &Server,
    domains: &HostedModelAdmission,
    domain: &str,
    run: &str,
) -> ApiModelClient {
    let mut client =
        model_client(server.url.clone(), None, 1_024).with_admission(domains.for_domain(domain));
    client.agent_run_id = run.to_owned();
    client
}
fn start(
    client: ApiModelClient,
    streaming: bool,
    retry_backoff_ms: u64,
) -> JoinHandle<Result<ModelClientResponse, ModelClientError>> {
    tokio::spawn(async move {
        let mut request = model_request();
        request.session_config.max_retries = u32::from(retry_backoff_ms > 0);
        request.session_config.retry_backoff_ms = retry_backoff_ms;
        if streaming {
            client.generate_stream(&request, &mut |_| {}).await
        } else {
            client.generate(&request).await
        }
    })
}
fn body(streaming: bool) -> String {
    if streaming {
        concat!(
            "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"delta\",\"delta\":\"ok\"}\n\n",
            "data: {\"schema\":\"api.model.stream.v1\",\"type\":\"result\",\"text\":\"ok\",\"toolCalls\":[],\"usage\":null}\n\n"
        ).to_owned()
    } else {
        r#"{"text":"ok","toolCalls":[],"usage":null}"#.to_owned()
    }
}
async fn finish(mut request: ObservedRequest, streaming: bool) {
    let body = body(streaming);
    request.stream.write_all(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        if streaming { "text/event-stream" } else { "application/json" }, body.len()
    ).as_bytes()).await.unwrap();
}
async fn completed(task: JoinHandle<Result<ModelClientResponse, ModelClientError>>, attempts: u32) {
    let response = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(response.provider_attempts, attempts);
    assert_eq!(response.generate_result.content, "ok");
}

#[tokio::test]
async fn hosted_model_admission_limits_distinct_runs_and_shared_clones() {
    let mut server = Server::start().await;
    let domains = HostedModelAdmission::configured(&[("shared", 2)]).unwrap();
    let a = client(&server, &domains, "shared", "run-a");
    let mut b = a.clone();
    b.agent_run_id = "run-b".into();
    let first = start(a, false, 0);
    let second = start(b, false, 0);
    let request_a = server.next().await;
    let request_b = server.next().await;
    assert_ne!(request_a.wire["agentRunId"], request_b.wire["agentRunId"]);
    let third = start(client(&server, &domains, "shared", "run-c"), false, 0);
    server.assert_no_dispatch().await;
    finish(request_a, false).await;
    let request_c = server.next().await;
    assert_eq!(request_c.wire["agentRunId"], "run-c");
    finish(request_b, false).await;
    finish(request_c, false).await;
    completed(first, 1).await;
    completed(second, 1).await;
    completed(third, 1).await;
}

#[tokio::test]
async fn hosted_model_admission_explicit_domains_do_not_block_each_other() {
    let mut server = Server::start().await;
    let domains = HostedModelAdmission::configured(&[("alpha", 1), ("beta", 1)]).unwrap();
    let first = start(client(&server, &domains, "alpha", "run-a"), false, 0);
    let a = server.next().await;
    let second = start(client(&server, &domains, "beta", "run-b"), false, 0);
    let b = server.next().await;
    finish(a, false).await;
    finish(b, false).await;
    completed(first, 1).await;
    completed(second, 1).await;
}

#[tokio::test]
async fn hosted_model_admission_dropped_queue_never_dispatches_or_leaks_capacity() {
    let mut server = Server::start().await;
    let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
    let first = start(client(&server, &domains, "shared", "run-a"), false, 0);
    let a = server.next().await;
    let queued = start(
        client(&server, &domains, "shared", "never-dispatch"),
        false,
        0,
    );
    server.assert_no_dispatch().await;
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    finish(a, false).await;
    completed(first, 1).await;
    let next = start(client(&server, &domains, "shared", "run-c"), false, 0);
    let c = server.next().await;
    assert_eq!(c.wire["agentRunId"], "run-c");
    finish(c, false).await;
    completed(next, 1).await;
    server.assert_no_dispatch().await;
}

#[tokio::test]
async fn hosted_model_admission_json_capacity_covers_the_complete_body() {
    let mut server = Server::start().await;
    let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
    let first = start(client(&server, &domains, "shared", "run-a"), false, 0);
    let mut a = server.next().await;
    let response = body(false);
    a.stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{",
                response.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let second = start(client(&server, &domains, "shared", "run-b"), false, 0);
    server.assert_no_dispatch().await;
    a.stream.write_all(&response.as_bytes()[1..]).await.unwrap();
    completed(first, 1).await;
    finish(server.next().await, false).await;
    completed(second, 1).await;
}

#[tokio::test]
async fn hosted_model_admission_sse_capacity_covers_eof_after_result() {
    let mut server = Server::start().await;
    let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
    let first = start(client(&server, &domains, "shared", "run-a"), true, 0);
    let mut a = server.next().await;
    let response = body(true);
    a.stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n{response}\r\n", response.len()).as_bytes()).await.unwrap();
    let second = start(client(&server, &domains, "shared", "run-b"), true, 0);
    server.assert_no_dispatch().await;
    a.stream.write_all(b"0\r\n\r\n").await.unwrap();
    completed(first, 1).await;
    finish(server.next().await, true).await;
    completed(second, 1).await;
}

#[tokio::test]
async fn hosted_model_admission_releases_before_json_and_sse_retry_backoff() {
    for streaming in [false, true] {
        let mut server = Server::start().await;
        let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
        let first = start(
            client(&server, &domains, "shared", "run-a"),
            streaming,
            1_000,
        );
        let mut a = server.next().await;
        a.stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        drop(a);
        tokio::time::sleep(Duration::from_millis(40)).await;
        let second = start(client(&server, &domains, "shared", "run-b"), streaming, 0);
        let b = tokio::time::timeout(Duration::from_millis(500), server.next())
            .await
            .expect("another Run must advance during backoff");
        assert_eq!(b.wire["agentRunId"], "run-b");
        finish(b, streaming).await;
        completed(second, 1).await;
        let retry = server.next().await;
        assert_eq!(retry.wire["agentRunId"], "run-a");
        finish(retry, streaming).await;
        completed(first, 2).await;
    }
}

#[tokio::test]
async fn hosted_model_admission_inflight_cancel_and_error_return_capacity() {
    for cancel in [true, false] {
        let mut server = Server::start().await;
        let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
        let first = start(client(&server, &domains, "shared", "run-a"), false, 0);
        let mut a = server.next().await;
        if cancel {
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
        } else {
            a.stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            let error = first.await.unwrap().unwrap_err();
            assert_eq!(error.provider_attempts, 1);
            assert!(!error.retryable);
        }
        let second = start(client(&server, &domains, "shared", "run-b"), false, 0);
        finish(server.next().await, false).await;
        completed(second, 1).await;
    }
}

#[tokio::test]
async fn hosted_model_admission_disabled_preserves_wire_stream_and_attempts() {
    for streaming in [false, true] {
        let mut server = Server::start().await;
        let domains = HostedModelAdmission::configured(&[("not-selected", 1)]).unwrap();
        let admission = domains.for_domain("not-selected").unwrap();
        let _held = admission.acquire_attempt("other-run").await;
        let client = model_client(server.url.clone(), None, 1_024).with_admission(None);
        let expected =
            serde_json::to_value(client.model_run_request(&model_request()).unwrap()).unwrap();
        let task = start(client, streaming, 0);
        let request = server.next().await;
        assert_eq!(request.wire, expected);
        finish(request, streaming).await;
        completed(task, 1).await;
    }
}

fn capture_model_timings<F: std::future::Future>(
    enabled: bool,
    future: F,
) -> (F::Output, Vec<serde_json::Value>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    crate::observations::capture_timings(enabled, || runtime.block_on(future))
}

fn model_timing_records(records: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    records
        .iter()
        .filter(|row| {
            matches!(
                row["phase"].as_str(),
                Some("modelAdmissionWait" | "modelRpcAttempt" | "modelRetryBackoff")
            )
        })
        .collect()
}

#[test]
fn hosted_model_timing_covers_complete_json_body_and_sse_eof() {
    for streaming in [false, true] {
        let (_, records) = capture_model_timings(true, async {
            let _context = crate::observations::Context::enter(
                br#"{"jobId":"agent_run.lifecycle:timed-run"}"#,
            );
            let mut server = Server::start().await;
            let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
            let task = start(
                client(&server, &domains, "shared", "timed-run"),
                streaming,
                0,
            );
            let mut request = server.next().await;
            let response = body(streaming);
            if streaming {
                request.stream.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n{response}\r\n",
                    response.len()
                ).as_bytes()).await.unwrap();
            } else {
                request.stream.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{{",
                    response.len()
                ).as_bytes()).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(
                !task.is_finished(),
                "response headers/result must not close the RPC attempt"
            );
            {
                let mut boundary = crate::observations::Span::new("testModelBodyBoundary");
                boundary.finished(true);
            }
            if streaming {
                request.stream.write_all(b"0\r\n\r\n").await.unwrap();
            } else {
                request
                    .stream
                    .write_all(&response.as_bytes()[1..])
                    .await
                    .unwrap();
            }
            completed(task, 1).await;
        });
        let timings = model_timing_records(&records);
        assert_eq!(
            timings.len(),
            2,
            "one admission and one complete-body RPC span"
        );
        assert_eq!(timings[0]["phase"], "modelAdmissionWait");
        assert_eq!(timings[1]["phase"], "modelRpcAttempt");
        assert!(timings.iter().all(|row| row["outcome"] == "ok"));
        let boundary = records
            .iter()
            .position(|row| row["phase"] == "testModelBodyBoundary")
            .unwrap();
        let rpc = records
            .iter()
            .position(|row| row["phase"] == "modelRpcAttempt")
            .unwrap();
        assert!(
            boundary < rpc,
            "RPC timing must remain open through body/EOF consumption"
        );
    }
}

#[test]
fn hosted_model_timing_separates_json_and_sse_retry_backoff() {
    for streaming in [false, true] {
        let (_, records) = capture_model_timings(true, async {
            let mut server = Server::start().await;
            let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
            let task = start(
                client(&server, &domains, "shared", "timed-run"),
                streaming,
                50,
            );
            let mut first = server.next().await;
            first
                .stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            drop(first);
            let retry = server.next().await;
            assert_eq!(retry.wire["agentRunId"], "timed-run");
            finish(retry, streaming).await;
            completed(task, 2).await;
        });
        let actual: Vec<_> = model_timing_records(&records)
            .into_iter()
            .map(|row| {
                (
                    row["phase"].as_str().unwrap(),
                    row["outcome"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            actual,
            [
                ("modelAdmissionWait", "ok"),
                ("modelRpcAttempt", "error"),
                ("modelRetryBackoff", "ok"),
                ("modelAdmissionWait", "ok"),
                ("modelRpcAttempt", "ok"),
            ],
            "failed RPC closes before backoff; a retry has its own admission and RPC timing"
        );
    }
}

#[test]
fn hosted_model_timing_closes_queued_and_active_cancellation_and_error() {
    for mode in ["queued", "activeCancel", "activeError"] {
        let (_, records) = capture_model_timings(true, async {
            let mut server = Server::start().await;
            let domains = HostedModelAdmission::configured(&[("shared", 1)]).unwrap();
            let admission = domains.for_domain("shared").unwrap();
            let held = if mode == "queued" {
                Some(admission.acquire_attempt("occupier").await)
            } else {
                None
            };
            let task = start(client(&server, &domains, "shared", "timed-run"), false, 0);
            if mode == "queued" {
                server.assert_no_dispatch().await;
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                let mut request = server.next().await;
                if mode == "activeCancel" {
                    request
                        .stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .unwrap();
                    task.abort();
                    assert!(task.await.unwrap_err().is_cancelled());
                } else {
                    request.stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    let error = task.await.unwrap().unwrap_err();
                    assert_eq!(error.provider_attempts, 1);
                    assert!(!error.retryable);
                }
            }
            drop(held);
        });
        let actual: Vec<_> = model_timing_records(&records)
            .into_iter()
            .map(|row| {
                (
                    row["phase"].as_str().unwrap(),
                    row["outcome"].as_str().unwrap(),
                )
            })
            .collect();
        let expected = match mode {
            "queued" => vec![("modelAdmissionWait", "interrupted")],
            "activeCancel" => vec![
                ("modelAdmissionWait", "ok"),
                ("modelRpcAttempt", "interrupted"),
            ],
            _ => vec![("modelAdmissionWait", "ok"), ("modelRpcAttempt", "error")],
        };
        assert_eq!(
            actual, expected,
            "cancellation must not fabricate a successful attempt"
        );
    }
}

#[test]
fn hosted_model_timing_disabled_preserves_wire_and_results_without_private_fields() {
    for enabled in [false, true] {
        for streaming in [false, true] {
            let (_, records) = capture_model_timings(enabled, async {
                let _context = crate::observations::Context::enter(
                    br#"{"jobId":"agent_run.lifecycle:timed-run"}"#,
                );
                let mut server = Server::start().await;
                let client = model_client(server.url.clone(), None, 1_024);
                let expected =
                    serde_json::to_value(client.model_run_request(&model_request()).unwrap())
                        .unwrap();
                let task = start(client, streaming, 0);
                let request = server.next().await;
                assert_eq!(request.wire, expected);
                finish(request, streaming).await;
                completed(task, 1).await;
            });
            let timings = model_timing_records(&records);
            assert_eq!(timings.len(), usize::from(enabled));
            for row in timings {
                let mut keys: Vec<_> = row
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    [
                        "atUnixMs",
                        "elapsedMs",
                        "jobId",
                        "outcome",
                        "phase",
                        "schema",
                        "source"
                    ]
                );
                assert_eq!(row["phase"], "modelRpcAttempt");
                assert_eq!(row["jobId"], "agent_run.lifecycle:timed-run");
                assert_eq!(row["schema"], "workspace.perf.v1");
                assert_eq!(row["source"], "runtime");
                assert_eq!(row["outcome"], "ok");
                assert!(row["elapsedMs"].as_f64().unwrap() >= 0.0);
            }
        }
    }
}
