use super::*;
use futures::poll;
use std::collections::HashMap;

struct Reply(u16);
impl JsonHttpTransport for Reply {
    fn execute_json<'a>(&'a self, _: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            Ok(JsonHttpResponse {
                status_code: self.0,
                headers: if self.0 == 429 {
                    HashMap::from([("retry-after".into(), "5".into())])
                } else {
                    HashMap::new()
                },
                body_json: "{}".into(),
            })
        })
    }
    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        _: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        self.execute_json(request)
    }
}

#[tokio::test(start_paused = true)]
async fn local_retry_after_does_not_pause_unrelated_transports() {
    let request = JsonHttpRequest {
        method: "POST".into(),
        url: "https://unused.invalid".into(),
        headers: HashMap::new(),
        timeout_ms: 1000,
        sse_idle_timeout_ms: 1000,
        max_retries: 0,
        retry_backoff_ms: 1000,
        body_json: "{}".into(),
    };
    let provider_a = admit_runtime_transport(Reply(429), "cooldown-a".into());
    let provider_b = admit_runtime_transport(Reply(200), "cooldown-b".into());
    assert_eq!(
        provider_a.execute_json(&request).await.unwrap().status_code,
        429
    );
    let mut unrelated = provider_b.execute_json(&request);
    assert!(
        poll!(&mut unrelated).is_ready(),
        "one provider must not pause an unrelated transport"
    );
    let mut retry = provider_a.execute_json(&request);
    assert!(
        poll!(&mut retry).is_pending(),
        "the original transport must still respect Retry-After"
    );
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(retry.await.unwrap().status_code, 429);
}
