use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    /// Total timeout for JSON requests and response-header timeout for SSE requests.
    pub timeout_ms: u64,
    /// Maximum silence between raw network chunks after an SSE response is established.
    pub sse_idle_timeout_ms: u64,
    pub max_retries: u32,
    pub retry_backoff_ms: u64,
    pub body_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonHttpResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body_json: String,
}

pub type JsonHttpFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsonHttpResponse, String>> + Send + 'a>>;

pub trait JsonHttpTransport: Send + Sync {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a>;

    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        _on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        self.execute_json(request)
    }
}

async fn sleep_before_http_retry(attempt: u32, retry_backoff_ms: u64) {
    if retry_backoff_ms == 0 {
        return;
    }
    let delay_ms = retry_backoff_ms.saturating_mul(u64::from(attempt.max(1)));
    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
}

fn is_retryable_http_status(status_code: u16) -> bool {
    status_code == 408 || status_code == 429 || status_code >= 500
}

async fn execute_json_with_retries<T: JsonHttpTransport>(
    transport: &T,
    request: &JsonHttpRequest,
) -> Result<AttemptedHttpResponse, AttemptedTransportError> {
    let mut attempt = 0_u32;
    loop {
        match transport.execute_json(request).await {
            Ok(response)
                if is_retryable_http_status(response.status_code)
                    && attempt < request.max_retries =>
            {
                attempt = attempt.saturating_add(1);
                sleep_before_http_retry(attempt, request.retry_backoff_ms).await;
            }
            Ok(response) => {
                return Ok(AttemptedHttpResponse {
                    response,
                    attempts: attempt.saturating_add(1),
                })
            }
            Err(_) if attempt < request.max_retries => {
                attempt = attempt.saturating_add(1);
                sleep_before_http_retry(attempt, request.retry_backoff_ms).await;
            }
            Err(message) => {
                return Err(AttemptedTransportError {
                    message,
                    attempts: attempt.saturating_add(1),
                    provider_error: None,
                })
            }
        }
    }
}

pub(super) async fn execute_json_model_response_with_retries<T, F>(
    transport: &T,
    request: &JsonHttpRequest,
    mut parse_response: F,
) -> Result<ModelClientResponse, ModelClientError>
where
    T: JsonHttpTransport,
    F: FnMut(JsonHttpResponse) -> Result<ModelClientResponse, ModelClientError>,
{
    let maximum_attempts = request.max_retries.saturating_add(1);
    let mut provider_attempts = 0_u32;
    loop {
        let mut attempt_request = request.clone();
        attempt_request.max_retries = maximum_attempts
            .saturating_sub(provider_attempts)
            .saturating_sub(1);
        let attempted = execute_json_with_retries(transport, &attempt_request)
            .await
            .map_err(|mut error| {
                error.attempts = error.attempts.saturating_add(provider_attempts);
                map_attempted_transport_error(error)
            })?;
        provider_attempts = provider_attempts.saturating_add(attempted.attempts);
        match parse_response(attempted.response) {
            Ok(mut response) => {
                response.provider_attempts = provider_attempts;
                return Ok(response);
            }
            Err(error)
                if provider_attempts < maximum_attempts
                    && (error.retryable
                        || error.provider_code.as_deref()
                            == Some("malformed_tool_call_arguments")) =>
            {
                sleep_before_http_retry(provider_attempts, request.retry_backoff_ms).await;
            }
            Err(mut error) => {
                if error.provider_code.as_deref() == Some("malformed_tool_call_arguments") {
                    error.retryable = false;
                }
                return Err(error.with_provider_attempts(provider_attempts));
            }
        }
    }
}

pub(super) struct AttemptedHttpResponse {
    pub(super) response: JsonHttpResponse,
    pub(super) attempts: u32,
}

pub(super) struct AttemptedTransportError {
    message: String,
    attempts: u32,
    provider_error: Option<ModelClientError>,
}

pub(super) fn map_attempted_transport_error(error: AttemptedTransportError) -> ModelClientError {
    match error.provider_error {
        Some(provider_error) => provider_error.with_provider_attempts(error.attempts),
        None => {
            map_transport_model_client_error(error.message).with_provider_attempts(error.attempts)
        }
    }
}

pub(super) enum SseAttemptEvent {
    Start { attempt: u32 },
    Data { attempt: u32, frame: String },
}

#[derive(Debug, Clone, Default)]
pub(super) struct SseAttemptProgress {
    pub(super) terminal: bool,
    pub(super) terminal_error: Option<ModelClientError>,
}

pub(super) async fn execute_sse_with_retries<T: JsonHttpTransport>(
    transport: &T,
    request: &JsonHttpRequest,
    on_event: &mut (dyn FnMut(SseAttemptEvent) -> SseAttemptProgress + Send),
) -> Result<AttemptedHttpResponse, AttemptedTransportError> {
    let mut attempt = 0_u32;
    loop {
        let _ = on_event(SseAttemptEvent::Start { attempt });
        let mut terminal = false;
        let mut terminal_error = None;
        let result = transport
            .execute_sse(request, &mut |chunk| {
                if terminal_error.is_some() {
                    return;
                }
                let progress = on_event(SseAttemptEvent::Data {
                    attempt: attempt.saturating_add(1),
                    frame: chunk,
                });
                terminal |= progress.terminal;
                if terminal_error.is_none() && progress.terminal_error.is_some() {
                    terminal_error = progress.terminal_error;
                }
            })
            .await;
        match result {
            Ok(_)
                if terminal_error.as_ref().is_some_and(|error| error.retryable)
                    && attempt < request.max_retries =>
            {
                attempt = attempt.saturating_add(1);
                sleep_before_http_retry(attempt, request.retry_backoff_ms).await;
            }
            Ok(_) if terminal_error.is_some() => {
                let mut provider_error = terminal_error.expect("checked terminal error");
                if matches!(
                    provider_error.provider_code.as_deref(),
                    Some("malformed_tool_call_arguments" | "malformed_sse_frame")
                ) {
                    provider_error.retryable = false;
                }
                return Err(AttemptedTransportError {
                    message: provider_error.message.clone(),
                    attempts: attempt.saturating_add(1),
                    provider_error: Some(provider_error),
                });
            }
            Ok(response)
                if is_retryable_http_status(response.status_code)
                    && attempt < request.max_retries =>
            {
                attempt = attempt.saturating_add(1);
                sleep_before_http_retry(attempt, request.retry_backoff_ms).await;
            }
            Ok(response)
                if response.status_code < 400 && !terminal && attempt < request.max_retries =>
            {
                attempt = attempt.saturating_add(1);
                sleep_before_http_retry(attempt, request.retry_backoff_ms).await;
            }
            Ok(response) if response.status_code < 400 && !terminal => {
                return Err(AttemptedTransportError {
                    message: "SSE stream ended before a terminal response event".to_string(),
                    attempts: attempt.saturating_add(1),
                    provider_error: None,
                });
            }
            Ok(response) => {
                return Ok(AttemptedHttpResponse {
                    response,
                    attempts: attempt.saturating_add(1),
                })
            }
            Err(_) if attempt < request.max_retries => {
                attempt = attempt.saturating_add(1);
                sleep_before_http_retry(attempt, request.retry_backoff_ms).await;
            }
            Err(message) => {
                return Err(AttemptedTransportError {
                    message,
                    attempts: attempt.saturating_add(1),
                    provider_error: None,
                })
            }
        }
    }
}
