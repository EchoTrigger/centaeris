//! Bounded synthetic loopback measurements of the actual runtime HTTP transport.
#[expect(
    dead_code,
    reason = "probe supplies a no_proxy client rather than calling the production client factory"
)]
#[rustfmt::skip]
#[path = "../src/http_transport.rs"]
mod http_transport;

use centaeris_core::model::{JsonHttpRequest, JsonHttpTransport};
use http_transport::ReqwestJsonHttpTransport;
use serde_json::json;
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const SAMPLES: usize = 30;
const WARMUP: usize = 3;
const CLONES: usize = 100;
const WIDTH: usize = 8;
const SERVICE_MS: u64 = 25;

fn request(url: &str, bytes: usize) -> JsonHttpRequest {
    let body = if bytes == 2 {
        "{}".to_owned()
    } else {
        format!("\"{}\"", "x".repeat(bytes - 2))
    };
    JsonHttpRequest {
        method: "POST".into(),
        url: url.into(),
        headers: HashMap::from([("content-type".into(), "application/json".into())]),
        timeout_ms: 30_000,
        sse_idle_timeout_ms: 30_000,
        max_retries: 0,
        retry_backoff_ms: 0,
        body_json: body.into(),
    }
}

fn transport() -> ReqwestJsonHttpTransport {
    ReqwestJsonHttpTransport::for_session("probe-session", "openai.default", || {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())
    })
    .expect("build synthetic loopback transport")
}

fn percentiles(values: &[f64]) -> (f64, f64) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |percent: usize| sorted[(sorted.len() * percent).div_ceil(100) - 1];
    (percentile(50), percentile(95))
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

async fn serve_connection(socket: TcpStream, arrivals: mpsc::UnboundedSender<Instant>) {
    socket
        .set_nodelay(true)
        .expect("disable server Nagle buffering");
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    let mut scratch = [0_u8; 65_536];
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .await
            .expect("read request line")
            == 0
        {
            return;
        }
        let delayed = line.contains(" /delayed ");
        let mut remaining = None;
        loop {
            line.clear();
            assert!(
                reader.read_line(&mut line).await.expect("read header") > 0,
                "headers must complete"
            );
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    remaining = Some(
                        value
                            .trim()
                            .parse::<usize>()
                            .expect("numeric content length"),
                    );
                }
            }
        }
        let mut remaining = remaining.expect("probe request includes content length");
        while remaining > 0 {
            let count = remaining.min(scratch.len());
            reader
                .read_exact(&mut scratch[..count])
                .await
                .expect("read complete request body");
            remaining -= count;
        }
        if delayed {
            arrivals
                .send(Instant::now())
                .expect("record delayed request arrival");
            tokio::time::sleep(Duration::from_millis(SERVICE_MS)).await;
        }
        reader.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n{}").await.expect("write synthetic response");
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let label = std::env::args().nth(1).expect("measurement label argument");
    println!(
        "{}",
        json!({"kind":"parameters","label":label,"profile":"dev-debug0","requestsPerCase":SAMPLES,"warmup":WARMUP,"cloneCount":CLONES,"parallelWidth":WIDTH,"syntheticServiceMs":SERVICE_MS,"percentile":"nearest-rank","serverInProcess":true})
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback only");
    let address = listener.local_addr().expect("read loopback address");
    let (arrivals_tx, mut arrivals_rx) = mpsc::unbounded_channel();
    let connections = Arc::new(AtomicUsize::new(0));
    let accepted = connections.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.expect("accept loopback socket");
            accepted.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(serve_connection(socket, arrivals_tx.clone()));
        }
    });
    let url = format!("http://{address}/json");
    for bytes in [1024 * 1024, 4 * 1024 * 1024, 8 * 1024 * 1024] {
        let request = request(&url, bytes);
        let mut durations = Vec::with_capacity(CLONES);
        let total = Instant::now();
        for _ in 0..CLONES {
            let started = Instant::now();
            let copy = black_box(black_box(&request).clone());
            durations.push(started.elapsed().as_secs_f64() * 1_000_000.0);
            drop(copy);
        }
        let total_ms = milliseconds(total.elapsed());
        let (p50, p95) = percentiles(&durations);
        println!(
            "{}",
            json!({"kind":"clone","label":label,"bytes":bytes,"count":CLONES,"p50Us":p50,"p95Us":p95,"totalMsIncludingDrop":total_ms})
        );
    }
    let client = transport();
    for bytes in [2, 1024 * 1024, 4 * 1024 * 1024, 8 * 1024 * 1024] {
        let request = request(&url, bytes);
        let mut durations = Vec::with_capacity(SAMPLES - WARMUP);
        for index in 0..SAMPLES {
            let started = Instant::now();
            let response = client
                .execute_json(&request)
                .await
                .expect("successful loopback request");
            assert_eq!(response.status_code, 200, "synthetic success status");
            assert_eq!(response.body_json, "{}", "synthetic body is complete");
            if index >= WARMUP {
                durations.push(milliseconds(started.elapsed()));
            }
        }
        let (p50, p95) = percentiles(&durations);
        println!(
            "{}",
            json!({"kind":"sequential","label":label,"bytes":bytes,"measured":durations.len(),"p50Ms":p50,"p95Ms":p95,"connectionsAccepted":connections.load(Ordering::Relaxed)})
        );
    }
    drop(client);
    let clients: Vec<_> = (0..WIDTH).map(|_| transport()).collect();
    let parallel_request = request(&format!("http://{address}/delayed"), 2);
    let mut requests = Vec::new();
    let mut batches = Vec::new();
    let mut gaps = Vec::new();
    let mut spans = Vec::new();
    for batch in 0..SAMPLES {
        let batch_started = Instant::now();
        let durations = futures::future::join_all(clients.iter().map(|client| {
            let request = &parallel_request;
            async move {
                let started = Instant::now();
                let response = client
                    .execute_json(request)
                    .await
                    .expect("successful parallel request");
                assert_eq!(response.status_code, 200, "parallel synthetic success");
                milliseconds(started.elapsed())
            }
        }))
        .await;
        let batch_ms = milliseconds(batch_started.elapsed());
        let mut arrived = Vec::with_capacity(WIDTH);
        for _ in 0..WIDTH {
            arrived.push(
                arrivals_rx
                    .try_recv()
                    .expect("server recorded each completed request"),
            );
        }
        arrived.sort();
        let offsets: Vec<_> = arrived
            .iter()
            .map(|arrival| milliseconds(arrival.duration_since(batch_started)))
            .collect();
        let arrival_gaps: Vec<_> = arrived
            .windows(2)
            .map(|pair| milliseconds(pair[1].duration_since(pair[0])))
            .collect();
        if batch >= WARMUP {
            requests.extend_from_slice(&durations);
            batches.push(batch_ms);
            gaps.extend_from_slice(&arrival_gaps);
            spans.push(offsets[WIDTH - 1] - offsets[0]);
            println!(
                "{}",
                json!({"kind":"parallelBatch","label":label,"batch":batch - WARMUP,"requestMs":durations,"batchMs":batch_ms,"arrivalOffsetMs":offsets,"arrivalGapMs":arrival_gaps})
            );
        }
    }
    let (request_p50, request_p95) = percentiles(&requests);
    let (batch_p50, batch_p95) = percentiles(&batches);
    let (gap_p50, gap_p95) = percentiles(&gaps);
    let (span_p50, span_p95) = percentiles(&spans);
    println!(
        "{}",
        json!({"kind":"parallelSummary","label":label,"measuredRequests":requests.len(),"measuredBatches":batches.len(),"requestP50Ms":request_p50,"requestP95Ms":request_p95,"batchP50Ms":batch_p50,"batchP95Ms":batch_p95,"arrivalGapP50Ms":gap_p50,"arrivalGapP95Ms":gap_p95,"arrivalSpanP50Ms":span_p50,"arrivalSpanP95Ms":span_p95})
    );
    server.abort();
}
