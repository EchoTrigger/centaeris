use super::*;
use std::sync::Arc;

/// A JSON document encoded once; clones share the same owned text allocation.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedJsonBody(Arc<String>);

impl EncodedJsonBody {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl std::fmt::Debug for EncodedJsonBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedJsonBody")
            .field("bytes", &self.0.len())
            .finish()
    }
}
impl From<String> for EncodedJsonBody {
    fn from(value: String) -> Self {
        Self(Arc::new(value))
    }
}
impl From<&str> for EncodedJsonBody {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}
impl std::ops::Deref for EncodedJsonBody {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl AsRef<[u8]> for EncodedJsonBody {
    fn as_ref(&self) -> &[u8] {
        self.as_str().as_bytes()
    }
}

/// Transport failures retain their category before model-protocol interpretation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("{0}")]
    Timeout(String),
    #[error("{0}")]
    Connection(String),
    #[error("{0}")]
    Network(String),
    #[error("{0}")]
    Build(String),
    #[error("{0}")]
    InvalidResponse(String),
    #[error("response body exceeds {max_bytes} bytes")]
    ResponseTooLarge { max_bytes: usize },
    #[error("{0}")]
    StreamInterrupted(String),
}
impl TransportError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_) | Self::Connection(_) | Self::Network(_) | Self::StreamInterrupted(_)
        )
    }
    fn model_kind(&self) -> ModelClientErrorKind {
        match self {
            Self::Timeout(_) => ModelClientErrorKind::Timeout,
            Self::Build(_) => ModelClientErrorKind::InvalidRequest,
            Self::InvalidResponse(_)
            | Self::ResponseTooLarge { .. }
            | Self::StreamInterrupted(_) => ModelClientErrorKind::ProviderResponseInterrupted,
            Self::Connection(_) | Self::Network(_) => ModelClientErrorKind::Network,
        }
    }
    pub(super) fn into_model_error(self) -> ModelClientError {
        ModelClientError::new(self.model_kind(), self.to_string(), self.retryable())
    }
}

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
    pub body_json: EncodedJsonBody,
}

/// Local transport observations for one completed HTTP attempt. All values are elapsed milliseconds.
/// First event means a complete SSE data/comment event, not a semantic model token.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HttpTimings {
    pub headers_wait_ms: u64,
    pub first_event_ms: Option<u64>,
    pub response_body_ms: u64,
    pub total_ms: u64,
}

/// One transport invocation, including failed invocations that received no HTTP status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAttemptTimings {
    pub elapsed_ms: u64,
    pub status_code: Option<u16>,
    pub error_kind: Option<ModelClientErrorKind>,
    pub transport: Option<HttpTimings>,
}

/// The model-client call boundary, including preparation, all attempts and retry waits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelRequestTimings {
    pub prepare_ms: u64,
    pub retry_wait_ms: u64,
    pub total_ms: u64,
    pub attempts: Vec<HttpAttemptTimings>,
}

struct RequestTimings {
    started: tokio::time::Instant,
    data: ModelRequestTimings,
}
impl RequestTimings {
    fn new(started: tokio::time::Instant) -> Self {
        Self {
            started,
            data: ModelRequestTimings {
                prepare_ms: elapsed_ms(started),
                ..Default::default()
            },
        }
    }
    fn record(
        &mut self,
        started: tokio::time::Instant,
        result: &Result<JsonHttpResponse, TransportError>,
    ) {
        let (status_code, error_kind, transport) = match result {
            Ok(response) => (Some(response.status_code), None, response.timings),
            Err(error) => (None, Some(error.model_kind()), None),
        };
        self.data.attempts.push(HttpAttemptTimings {
            elapsed_ms: elapsed_ms(started),
            status_code,
            error_kind,
            transport,
        });
    }
    fn snapshot(&self) -> ModelRequestTimings {
        ModelRequestTimings {
            total_ms: elapsed_ms(self.started),
            ..self.data.clone()
        }
    }
}
fn elapsed_ms(started: tokio::time::Instant) -> u64 {
    started.elapsed().as_millis() as u64
}
pub(super) fn preparation_failure(
    mut error: ModelClientError,
    started: tokio::time::Instant,
) -> ModelClientError {
    let elapsed = elapsed_ms(started);
    error.request_timings = Some(Box::new(ModelRequestTimings {
        prepare_ms: elapsed,
        total_ms: elapsed,
        ..Default::default()
    }));
    error
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonHttpResponse {
    pub timings: Option<HttpTimings>,
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body_json: String,
}

pub type JsonHttpFuture<'a> =
    Pin<Box<dyn Future<Output = Result<JsonHttpResponse, TransportError>> + Send + 'a>>;

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

/// Convert server retry advice to a deadline once, without sharing cooldown across requests.
#[derive(Debug, Clone, Copy)]
pub struct RetryAfter(tokio::time::Instant);
impl RetryAfter {
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let delay = if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
            std::time::Duration::from_secs(value.parse().ok()?)
        } else {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(std::time::SystemTime::now())
                .unwrap_or_default()
        };
        tokio::time::Instant::now().checked_add(delay).map(Self)
    }
    pub fn from_headers(headers: &HashMap<String, String>) -> Option<Self> {
        let value = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))?
            .1;
        Self::parse(value)
    }
    pub async fn wait(self) {
        tokio::time::sleep_until(self.0).await;
    }
}

async fn sleep_before_http_retry(
    attempt: u32,
    retry_backoff_ms: u64,
    retry_after: Option<RetryAfter>,
) -> u64 {
    let started = tokio::time::Instant::now();
    if let Some(advice) = retry_after {
        advice.wait().await;
    } else {
        let delay_ms = retry_backoff_ms.saturating_mul(u64::from(attempt.max(1)));
        if delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
    }
    elapsed_ms(started)
}

pub(super) fn is_retryable_http_status(status_code: u16) -> bool {
    status_code == 408 || status_code == 429 || status_code >= 500
}

async fn execute_json_with_retries<T: JsonHttpTransport>(
    transport: &T,
    request: &JsonHttpRequest,
    clock: &mut RequestTimings,
) -> Result<AttemptedHttpResponse, AttemptedTransportError> {
    let mut attempt = 0_u32;
    loop {
        let started = tokio::time::Instant::now();
        let result = transport.execute_json(request).await;
        clock.record(started, &result);
        match result {
            Ok(response)
                if is_retryable_http_status(response.status_code)
                    && attempt < request.max_retries =>
            {
                attempt = attempt.saturating_add(1);
                clock.data.retry_wait_ms += sleep_before_http_retry(
                    attempt,
                    request.retry_backoff_ms,
                    RetryAfter::from_headers(&response.headers),
                )
                .await;
            }
            Ok(response) => {
                return Ok(AttemptedHttpResponse {
                    request_timings: clock.snapshot(),
                    response,
                    attempts: attempt.saturating_add(1),
                })
            }
            Err(error) if error.retryable() && attempt < request.max_retries => {
                attempt = attempt.saturating_add(1);
                clock.data.retry_wait_ms +=
                    sleep_before_http_retry(attempt, request.retry_backoff_ms, None).await;
            }
            Err(error) => {
                return Err(AttemptedTransportError {
                    request_timings: clock.snapshot(),
                    message: error.to_string(),
                    attempts: attempt.saturating_add(1),
                    provider_error: Some(error.into_model_error()),
                })
            }
        }
    }
}

pub(super) async fn execute_json_model_response_with_retries<T, F>(
    transport: &T,
    request: &JsonHttpRequest,
    started: tokio::time::Instant,
    mut parse_response: F,
) -> Result<ModelClientResponse, ModelClientError>
where
    T: JsonHttpTransport,
    F: FnMut(JsonHttpResponse) -> Result<ModelClientResponse, ModelClientError>,
{
    let mut clock = RequestTimings::new(started);
    let maximum_attempts = request.max_retries.saturating_add(1);
    let mut provider_attempts = 0_u32;
    loop {
        let mut attempt_request = request.clone();
        attempt_request.max_retries = maximum_attempts
            .saturating_sub(provider_attempts)
            .saturating_sub(1);
        let attempted = execute_json_with_retries(transport, &attempt_request, &mut clock)
            .await
            .map_err(|mut error| {
                error.attempts = error.attempts.saturating_add(provider_attempts);
                map_attempted_transport_error(error)
            })?;
        provider_attempts = provider_attempts.saturating_add(attempted.attempts);
        match parse_response(attempted.response) {
            Ok(mut response) => {
                response.provider_attempts = provider_attempts;
                response.request_timings = Some(clock.snapshot());
                return Ok(response);
            }
            Err(error)
                if provider_attempts < maximum_attempts
                    && (error.retryable
                        || error.provider_code.as_deref()
                            == Some("malformed_tool_call_arguments")) =>
            {
                clock.data.retry_wait_ms +=
                    sleep_before_http_retry(provider_attempts, request.retry_backoff_ms, None)
                        .await;
            }
            Err(mut error) => {
                if error.provider_code.as_deref() == Some("malformed_tool_call_arguments") {
                    error.retryable = false;
                }
                error.request_timings = Some(Box::new(clock.snapshot()));
                return Err(error.with_provider_attempts(provider_attempts));
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct AttemptedHttpResponse {
    pub(super) response: JsonHttpResponse,
    pub(super) attempts: u32,
    pub(super) request_timings: ModelRequestTimings,
}

#[derive(Debug)]
pub(super) struct AttemptedTransportError {
    message: String,
    attempts: u32,
    provider_error: Option<ModelClientError>,
    request_timings: ModelRequestTimings,
}

pub(super) fn map_attempted_transport_error(error: AttemptedTransportError) -> ModelClientError {
    let mut mapped = match error.provider_error {
        Some(provider_error) => provider_error.with_provider_attempts(error.attempts),
        None => TransportError::StreamInterrupted(error.message)
            .into_model_error()
            .with_provider_attempts(error.attempts),
    };
    mapped.request_timings = Some(Box::new(error.request_timings));
    mapped
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
    started: tokio::time::Instant,
    on_event: &mut (dyn FnMut(SseAttemptEvent) -> SseAttemptProgress + Send),
) -> Result<AttemptedHttpResponse, AttemptedTransportError> {
    let mut clock = RequestTimings::new(started);
    let mut attempt = 0_u32;
    loop {
        let _ = on_event(SseAttemptEvent::Start { attempt });
        let mut terminal = false;
        let mut terminal_error = None;
        let attempt_started = tokio::time::Instant::now();
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
        clock.record(attempt_started, &result);
        match result {
            Ok(_)
                if terminal_error.as_ref().is_some_and(|error| error.retryable)
                    && attempt < request.max_retries =>
            {
                attempt = attempt.saturating_add(1);
                clock.data.retry_wait_ms +=
                    sleep_before_http_retry(attempt, request.retry_backoff_ms, None).await;
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
                    request_timings: clock.snapshot(),
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
                clock.data.retry_wait_ms += sleep_before_http_retry(
                    attempt,
                    request.retry_backoff_ms,
                    RetryAfter::from_headers(&response.headers),
                )
                .await;
            }
            Ok(response)
                if response.status_code < 400 && !terminal && attempt < request.max_retries =>
            {
                attempt = attempt.saturating_add(1);
                clock.data.retry_wait_ms +=
                    sleep_before_http_retry(attempt, request.retry_backoff_ms, None).await;
            }
            Ok(response) if response.status_code < 400 && !terminal => {
                return Err(AttemptedTransportError {
                    request_timings: clock.snapshot(),
                    message: "SSE stream ended before a terminal response event".to_string(),
                    attempts: attempt.saturating_add(1),
                    provider_error: None,
                });
            }
            Ok(response) => {
                return Ok(AttemptedHttpResponse {
                    request_timings: clock.snapshot(),
                    response,
                    attempts: attempt.saturating_add(1),
                })
            }
            Err(error) if error.retryable() && attempt < request.max_retries => {
                attempt = attempt.saturating_add(1);
                clock.data.retry_wait_ms +=
                    sleep_before_http_retry(attempt, request.retry_backoff_ms, None).await;
            }
            Err(error) => {
                return Err(AttemptedTransportError {
                    request_timings: clock.snapshot(),
                    message: error.to_string(),
                    attempts: attempt.saturating_add(1),
                    provider_error: Some(error.into_model_error()),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests;
