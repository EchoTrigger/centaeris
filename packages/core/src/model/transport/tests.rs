use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn request() -> JsonHttpRequest {
    JsonHttpRequest {
        method: "POST".into(),
        url: "https://synthetic.invalid".into(),
        headers: HashMap::new(),
        timeout_ms: 1000,
        sse_idle_timeout_ms: 1000,
        max_retries: 3,
        retry_backoff_ms: 0,
        body_json: "{}".into(),
    }
}
struct Failure {
    error: TransportError,
    calls: AtomicUsize,
}
impl JsonHttpTransport for Failure {
    fn execute_json<'a>(&'a self, _: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(self.error.clone())
        })
    }
}
#[tokio::test]
async fn permanent_transport_failures_never_retry_json_or_sse() {
    for error in [
        TransportError::Build("invalid header".into()),
        TransportError::InvalidResponse("wrong media type".into()),
        TransportError::ResponseTooLarge { max_bytes: 16 },
    ] {
        for streaming in [false, true] {
            let transport = Failure {
                error: error.clone(),
                calls: AtomicUsize::new(0),
            };
            let error = if streaming {
                execute_sse_with_retries(&transport, &request(), &mut |_| {
                    SseAttemptProgress::default()
                })
                .await
                .err()
                .unwrap()
            } else {
                execute_json_with_retries(&transport, &request())
                    .await
                    .err()
                    .unwrap()
            };
            assert!(!map_attempted_transport_error(error).retryable);
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        }
    }
}
#[tokio::test]
async fn transient_transport_failures_use_only_the_configured_attempt_budget() {
    for error in [
        TransportError::Timeout("timeout".into()),
        TransportError::Connection("refused".into()),
        TransportError::StreamInterrupted("closed".into()),
    ] {
        let transport = Failure {
            error,
            calls: AtomicUsize::new(0),
        };
        let error = execute_json_with_retries(&transport, &request())
            .await
            .err()
            .unwrap();
        assert_eq!(error.attempts, 4);
        assert_eq!(transport.calls.load(Ordering::SeqCst), 4);
    }
}
#[test]
fn encoded_body_clones_share_large_request_storage_and_preserve_bytes() {
    for size in [1024 * 1024, 4 * 1024 * 1024, 8 * 1024 * 1024] {
        let body = EncodedJsonBody::from("x".repeat(size));
        for _ in 0..5 {
            let cloned = body.clone();
            assert_eq!(body.as_str().as_ptr(), cloned.as_str().as_ptr());
            assert_eq!(body.as_ref(), cloned.as_ref());
        }
    }
}
#[tokio::test(start_paused = true)]
async fn retry_after_uses_the_original_deadline() {
    let advice = RetryAfter::parse("5").unwrap();
    tokio::time::advance(std::time::Duration::from_secs(3)).await;
    let started = tokio::time::Instant::now();
    advice.wait().await;
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(2));
}
#[tokio::test(start_paused = true)]
async fn retry_after_delays_each_rejected_request_without_blocking_unrelated_calls() {
    struct Busy(AtomicUsize);
    impl JsonHttpTransport for Busy {
        fn execute_json<'a>(&'a self, _: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
            Box::pin(async move {
                let attempt = self.0.fetch_add(1, Ordering::SeqCst);
                Ok(JsonHttpResponse {
                    timings: None,
                    status_code: if attempt == 0 { 429 } else { 200 },
                    headers: HashMap::from([("Retry-After".into(), "5".into())]),
                    body_json: "{}".into(),
                })
            })
        }
    }
    use futures::poll;
    let busy = Busy(AtomicUsize::new(0));
    let req = request();
    let mut waiting = Box::pin(execute_json_with_retries(&busy, &req));
    assert!(poll!(&mut waiting).is_pending());
    let unrelated = Busy(AtomicUsize::new(1));
    assert_eq!(unrelated.execute_json(&req).await.unwrap().status_code, 200);
    assert_eq!(busy.0.load(Ordering::SeqCst), 1);
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    assert_eq!(waiting.await.unwrap().attempts, 2);
}
