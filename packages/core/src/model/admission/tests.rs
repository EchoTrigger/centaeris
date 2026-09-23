use super::*;
use crate::model::transport::execute_sse_with_retries;
use crate::model::{JsonHttpResponse, SseAttemptEvent, SseAttemptProgress};
use futures::poll;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

type Reply = oneshot::Sender<Result<JsonHttpResponse, String>>;
#[derive(Clone)]
struct Controlled(mpsc::UnboundedSender<Reply>);
impl JsonHttpTransport for Controlled {
    fn execute_json<'a>(&'a self, _: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            self.0.send(tx).unwrap();
            rx.await.unwrap()
        })
    }
    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            on_data("first chunk".into());
            self.execute_json(request).await
        })
    }
}
fn fixture(
    limit: usize,
) -> (
    ModelAdmission,
    Controlled,
    mpsc::UnboundedReceiver<Reply>,
    JsonHttpRequest,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        ModelAdmission::new(NonZeroUsize::new(limit).unwrap()),
        Controlled(tx),
        rx,
        JsonHttpRequest {
            method: "POST".into(),
            url: "https://unused.invalid".into(),
            headers: HashMap::new(),
            timeout_ms: 1000,
            sse_idle_timeout_ms: 1000,
            max_retries: 1,
            retry_backoff_ms: 1000,
            body_json: "{}".into(),
        },
    )
}
fn client(
    inner: &Controlled,
    admission: &ModelAdmission,
    run: &str,
) -> AdmittedJsonHttpTransport<Controlled> {
    AdmittedJsonHttpTransport::new(inner.clone(), admission.clone(), run.into())
}
fn response(status: u16, retry_after: Option<&str>) -> JsonHttpResponse {
    JsonHttpResponse {
        status_code: status,
        headers: retry_after
            .map(|v| HashMap::from([("Retry-After".into(), v.into())]))
            .unwrap_or_default(),
        body_json: "{}".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn public_attempt_ticket_holds_capacity_and_observes_cooldown() {
    let (admission, _, _, _) = fixture(1);
    let first = admission.acquire_attempt("first-run").await;
    let mut next = Box::pin(admission.acquire_attempt("next-run"));
    assert!(poll!(&mut next).is_pending());
    first.observe_http_status(429, Some("5"));
    drop(first);
    assert!(poll!(&mut next).is_pending());
    tokio::time::advance(Duration::from_secs(5)).await;
    let second = next.await;
    drop(second);
}

#[tokio::test]
async fn shared_limit_is_held_for_whole_stream_and_released_on_drop() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "main");
    let b = client(&raw, &domain, "child");
    let mut chunks = Vec::new();
    let mut on_data = |chunk| chunks.push(chunk);
    let mut stream = a.execute_sse(&req, &mut on_data);
    assert!(poll!(&mut stream).is_pending());
    let reply = calls.try_recv().unwrap();
    let mut queued = b.execute_json(&req);
    assert!(poll!(&mut queued).is_pending());
    assert!(
        calls.try_recv().is_err(),
        "queued request reached provider while stream is open"
    );
    drop(stream);
    assert_eq!(chunks, ["first chunk"]);
    assert!(reply.is_closed());
    assert!(poll!(&mut queued).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    assert_eq!(queued.await.unwrap().status_code, 200);
}

#[tokio::test]
async fn run_round_robin_prevents_one_run_draining_its_backlog() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut active = a.execute_json(&req);
    assert!(poll!(&mut active).is_pending());
    let first = calls.try_recv().unwrap();
    let mut a1 = a.execute_json(&req);
    let mut a2 = a.execute_json(&req);
    let mut b1 = b.execute_json(&req);
    assert!(poll!(&mut a1).is_pending());
    assert!(poll!(&mut a2).is_pending());
    assert!(poll!(&mut b1).is_pending());
    assert!(calls.try_recv().is_err());
    first.send(Ok(response(200, None))).unwrap();
    active.await.unwrap();
    assert!(poll!(&mut a1).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    a1.await.unwrap();
    // Poll A first deliberately; eligibility must not depend on executor order.
    assert!(poll!(&mut a2).is_pending());
    assert!(calls.try_recv().is_err());
    assert!(poll!(&mut b1).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    b1.await.unwrap();
    assert!(poll!(&mut a2).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    a2.await.unwrap();
}

#[tokio::test]
async fn cancelling_waiter_removes_it_without_consuming_or_leaking_capacity() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut active = a.execute_json(&req);
    assert!(poll!(&mut active).is_pending());
    let first = calls.try_recv().unwrap();
    let mut cancelled = b.execute_json(&req);
    let mut next = b.execute_json(&req);
    assert!(poll!(&mut cancelled).is_pending());
    assert!(poll!(&mut next).is_pending());
    assert!(calls.try_recv().is_err());
    drop(cancelled);
    first.send(Err("network error".into())).unwrap();
    assert!(active.await.is_err());
    assert!(poll!(&mut next).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    next.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn retries_release_before_backoff_and_reenter_admission() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut event = |_: SseAttemptEvent| SseAttemptProgress::default();
    let mut retrying = Box::pin(execute_sse_with_retries(&a, &req, &mut event));
    assert!(poll!(&mut retrying).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Err("disconnected".into()))
        .unwrap();
    assert!(poll!(&mut retrying).is_pending());
    let mut other = b.execute_json(&req);
    assert!(poll!(&mut other).is_pending());
    let other_reply = calls.try_recv().expect("backoff must not hold capacity");
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(poll!(&mut retrying).is_pending());
    assert!(calls.try_recv().is_err(), "retry bypassed admission");
    other_reply.send(Ok(response(200, None))).unwrap();
    other.await.unwrap();
    assert!(poll!(&mut retrying).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    // A stream without a terminal event exhausts its retry budget.
    assert!(retrying.await.is_err());
    let mut after = b.execute_json(&req);
    assert!(poll!(&mut after).is_pending());
    assert!(calls.try_recv().is_ok());
}

#[tokio::test(start_paused = true)]
async fn retry_after_blocks_shared_domain_even_when_no_retries_remain() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut active = a.execute_json(&req);
    assert!(poll!(&mut active).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(429, Some("5"))))
        .unwrap();
    assert_eq!(active.await.unwrap().status_code, 429);
    let mut waiting = b.execute_json(&req);
    assert!(poll!(&mut waiting).is_pending());
    assert!(
        calls.try_recv().is_err(),
        "Retry-After cooldown was ignored"
    );
    tokio::time::advance(Duration::from_secs(4)).await;
    assert!(poll!(&mut waiting).is_pending());
    assert!(calls.try_recv().is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(poll!(&mut waiting).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    waiting.await.unwrap();
}

#[tokio::test]
async fn independent_domains_do_not_share_capacity() {
    let (one, raw, mut calls, req) = fixture(1);
    let two = ModelAdmission::new(NonZeroUsize::new(1).unwrap());
    let a = client(&raw, &one, "same-run");
    let b = client(&raw, &two, "same-run");
    let mut a = a.execute_json(&req);
    let mut b = b.execute_json(&req);
    assert!(poll!(&mut a).is_pending());
    assert!(poll!(&mut b).is_pending());
    assert!(calls.try_recv().is_ok());
    assert!(calls.try_recv().is_ok());
}

#[test]
fn retry_after_accepts_seconds_and_http_dates_and_ignores_invalid_values() {
    let now = std::time::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let date = httpdate::fmt_http_date(now + Duration::from_secs(7));
    assert_eq!(retry_after_delay(&date, now), Some(Duration::from_secs(7)));
    assert_eq!(
        retry_after_delay(&date, now + Duration::from_secs(10)),
        Some(Duration::ZERO)
    );
    assert_eq!(
        retry_after_delay(" 12 ", now),
        Some(Duration::from_secs(12))
    );
    for invalid in ["", "-1", "+1", "1.5", "garbage", "18446744073709551616"] {
        assert_eq!(retry_after_delay(invalid, now), None, "{invalid}");
    }
}

#[tokio::test(start_paused = true)]
async fn later_shorter_cooldown_does_not_shorten_existing_domain_deadline() {
    let (domain, raw, mut calls, req) = fixture(2);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut one = a.execute_json(&req);
    let mut ignore = |_| {};
    let mut two = b.execute_sse(&req, &mut ignore);
    assert!(poll!(&mut one).is_pending());
    assert!(poll!(&mut two).is_pending());
    let first = calls.try_recv().unwrap();
    let second = calls.try_recv().unwrap();
    first.send(Ok(response(503, Some("10")))).unwrap();
    one.await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    second.send(Ok(response(429, Some("3")))).unwrap();
    two.await.unwrap();
    let mut next = a.execute_json(&req);
    assert!(poll!(&mut next).is_pending());
    assert!(calls.try_recv().is_err());
    tokio::time::advance(Duration::from_secs(7)).await;
    assert!(poll!(&mut next).is_pending());
    assert!(calls.try_recv().is_err());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(poll!(&mut next).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    next.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn json_network_http_and_parser_retries_each_reacquire_without_holding_backoff() {
    use crate::model::transport::execute_json_model_response_with_retries;
    use crate::model::{ModelClientError, ModelClientErrorKind};
    for mode in ["network", "http", "parser"] {
        let (domain, raw, mut calls, req) = fixture(1);
        let a = client(&raw, &domain, "a");
        let b = client(&raw, &domain, "b");
        let mut retrying = Box::pin(execute_json_model_response_with_retries(&a, &req, |_| {
            Err(ModelClientError::new(
                ModelClientErrorKind::Provider,
                "test parser error",
                true,
            ))
        }));
        assert!(poll!(&mut retrying).is_pending());
        let first = match mode {
            "network" => Err("network error".into()),
            "http" => Ok(response(503, None)),
            _ => Ok(response(200, None)),
        };
        calls.try_recv().unwrap().send(first).unwrap();
        assert!(poll!(&mut retrying).is_pending());
        let mut other = b.execute_json(&req);
        assert!(poll!(&mut other).is_pending());
        let other_reply = calls.try_recv().expect("backoff released slot");
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(poll!(&mut retrying).is_pending());
        assert!(calls.try_recv().is_err(), "{mode} retry bypassed limit");
        other_reply.send(Ok(response(200, None))).unwrap();
        other.await.unwrap();
        assert!(poll!(&mut retrying).is_pending());
        calls
            .try_recv()
            .unwrap()
            .send(Ok(response(200, None)))
            .unwrap();
        assert_eq!(retrying.await.unwrap_err().provider_attempts, 2);
    }
}

#[tokio::test]
async fn queued_stream_emits_nothing_and_releases_after_successful_completion() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut active = a.execute_json(&req);
    assert!(poll!(&mut active).is_pending());
    let first = calls.try_recv().unwrap();
    let chunks = Arc::new(Mutex::new(Vec::new()));
    let seen = chunks.clone();
    let mut on_data = move |chunk| seen.lock().unwrap().push(chunk);
    let mut stream = b.execute_sse(&req, &mut on_data);
    assert!(poll!(&mut stream).is_pending());
    assert!(chunks.lock().unwrap().is_empty());
    first.send(Ok(response(200, None))).unwrap();
    active.await.unwrap();
    assert!(poll!(&mut stream).is_pending());
    assert_eq!(chunks.lock().unwrap().len(), 1);
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    stream.await.unwrap();
    let mut next = a.execute_json(&req);
    assert!(poll!(&mut next).is_pending());
    assert!(calls.try_recv().is_ok());
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_cooldown_does_not_block_other_runs_at_expiry() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut active = a.execute_json(&req);
    assert!(poll!(&mut active).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(429, Some("5"))))
        .unwrap();
    active.await.unwrap();
    let mut cancelled = a.execute_json(&req);
    let mut next = b.execute_json(&req);
    assert!(poll!(&mut cancelled).is_pending());
    assert!(poll!(&mut next).is_pending());
    drop(cancelled);
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(poll!(&mut next).is_pending());
    calls
        .try_recv()
        .unwrap()
        .send(Ok(response(200, None)))
        .unwrap();
    next.await.unwrap();
}

#[tokio::test]
async fn release_wakes_a_spawned_waiter_without_manual_polling() {
    let (domain, raw, mut calls, req) = fixture(1);
    let a = client(&raw, &domain, "a");
    let b = client(&raw, &domain, "b");
    let mut first = a.execute_json(&req);
    assert!(poll!(&mut first).is_pending());
    let reply = calls.try_recv().unwrap();
    let queued_request = req.clone();
    let queued = tokio::spawn(async move { b.execute_json(&queued_request).await });
    tokio::task::yield_now().await;
    assert!(calls.try_recv().is_err());
    reply.send(Ok(response(200, None))).unwrap();
    first.await.unwrap();
    let next = tokio::time::timeout(Duration::from_secs(2), calls.recv())
        .await
        .unwrap()
        .unwrap();
    next.send(Ok(response(200, None))).unwrap();
    tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
