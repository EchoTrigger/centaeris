use crate::api_model_client::set_native_failure;
use crate::contract::AgentRunStart;
use crate::postgres_store::PostgresRuntimeStore;
use base64::Engine as _;
use centaeris_core::model::{
    AuthSpec, JsonHttpFuture, JsonHttpRequest, JsonHttpResponse, JsonHttpTransport, ModelClient,
    ModelClientError, ModelClientErrorKind, ModelClientRequest, ModelClientResponse,
    ModelClientStreamEvent, ModelProviderRegistry, ModelSessionConfig, OpenAiResponsesModelClient,
    RetryAfter, TransportError,
};
use centaeris_model_http::{build_model_http_client, ReqwestJsonHttpTransport};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::Duration;

const AUTHORITY_SQL: &str = include_str!("../../api/app_core/native_model_authority.sql");
const GRANT_REQUEST_SCHEMA: &str = "runtime.native_model_grant.request.v1";
const GRANT_SCHEMA: &str = "workspace.native_model_grant.v1";
const SEALED_GRANT_SCHEMA: &str = "workspace.native_model_grant.sealed.v1";
const REPLY_DOMAIN: &[u8] = b"workspace:native-model-grant:reply:v1\0";
const GRANT_DOMAIN: &[u8] = b"workspace:native-model-grant:v1\0";
const GRANT_MAX_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub(crate) struct NativeModelGateway {
    provider_id: String,
    http: reqwest::Client,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantRequest<'a> {
    schema: &'static str,
    agent_run_id: &'a str,
    authorization_ref: &'a str,
    authorization_digest: &'a str,
    lease_owner: &'a str,
    provider_id: &'a str,
    proof: String,
}

impl GrantRequest<'_> {
    fn sign(&mut self, key: &[u8]) -> Result<(), String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| "native model grant signing key invalid")?;
        mac.update(GRANT_DOMAIN);
        mac.update(
            [
                self.agent_run_id,
                self.authorization_ref,
                self.authorization_digest,
                self.lease_owner,
                self.provider_id,
            ]
            .join("\0")
            .as_bytes(),
        );
        self.proof = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SealedGrant {
    schema: String,
    nonce: String,
    ciphertext: String,
}

impl SealedGrant {
    fn open(self, key: &[u8], proof: &str) -> Result<Grant, String> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};

        let invalid = || "native model grant response invalid".to_owned();
        if self.schema != SEALED_GRANT_SCHEMA {
            return Err(invalid());
        }
        let nonce = URL_SAFE_NO_PAD.decode(self.nonce).map_err(|_| invalid())?;
        let nonce = Nonce::try_assume_unique_for_key(&nonce).map_err(|_| invalid())?;
        let mut body = URL_SAFE_NO_PAD
            .decode(self.ciphertext)
            .map_err(|_| invalid())?;
        let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| invalid())?;
        mac.update(REPLY_DOMAIN);
        mac.update(proof.as_bytes());
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, mac.finalize().into_bytes().as_slice())
                .map_err(|_| invalid())?,
        );
        let plaintext = key
            .open_in_place(nonce, Aad::from(proof.as_bytes()), &mut body)
            .map_err(|_| invalid())?;
        serde_json::from_slice(plaintext).map_err(|_| invalid())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Grant {
    schema: String,
    authority_digest: String,
    api_base: String,
    model: String,
    max_output_tokens: u32,
    thinking_mode: Option<String>,
    provider_timeout_ms: u64,
    bearer_token: String,
}

impl NativeModelGateway {
    pub(crate) fn from_env() -> Result<Option<Self>, String> {
        let provider_id = match std::env::var("RUNTIME_NATIVE_RESPONSES_PROVIDER_ID") {
            Ok(value) if value.is_empty() => return Ok(None),
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(_) => return Err("native Responses provider identity is invalid".into()),
        };
        if provider_id.len() > 64
            || !provider_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err("native Responses provider identity is invalid".into());
        }
        Ok(Some(Self {
            provider_id,
            http: build_model_http_client()?,
        }))
    }

    pub(crate) async fn authorize(
        &self,
        api_url: &str,
        token: &str,
        start: &AgentRunStart,
        lease_owner: &str,
        store: Arc<PostgresRuntimeStore>,
    ) -> Result<Option<NativeResponsesClient>, String> {
        let mut request = GrantRequest {
            schema: GRANT_REQUEST_SCHEMA,
            agent_run_id: &start.agent_run_id,
            authorization_ref: &start.authorization.id,
            authorization_digest: &start.authorization_digest,
            lease_owner,
            provider_id: &self.provider_id,
            proof: String::new(),
        };
        let key = std::env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
            .map_err(|_| "native model grant signing key is required")?;
        request.sign(key.as_bytes())?;
        let mut response = self
            .http
            .post(format!(
                "{}/internal/model-runs/native-grant",
                api_url.trim_end_matches('/')
            ))
            .header("X-Internal-Token", token)
            .timeout(Duration::from_secs(10))
            .json(&request)
            .send()
            .await
            .map_err(|_| "native_model_grant_unavailable")?;
        match response.status() {
            reqwest::StatusCode::NO_CONTENT => return Ok(None),
            reqwest::StatusCode::OK => {}
            status if status.is_server_error() => {
                return Err("native_model_grant_unavailable".into())
            }
            _ => return Err("native model grant rejected".into()),
        }
        let mut body = Vec::new();
        while let Some(bytes) = response
            .chunk()
            .await
            .map_err(|_| "native_model_grant_unavailable")?
        {
            if bytes.len() > GRANT_MAX_BYTES - body.len() {
                return Err("native model grant response too large".into());
            }
            body.extend_from_slice(&bytes);
        }
        let sealed: SealedGrant =
            serde_json::from_slice(&body).map_err(|_| "native model grant response invalid")?;
        let grant = sealed.open(key.as_bytes(), &request.proof)?;
        let endpoint = reqwest::Url::parse(&grant.api_base)
            .map_err(|_| "native model grant endpoint invalid")?;
        let digest = grant.authority_digest.strip_prefix("sha256:").unwrap_or("");
        if grant.schema != GRANT_SCHEMA
            || grant.model.trim().is_empty()
            || grant.max_output_tokens != start.model_max_output_tokens
            || grant.thinking_mode != start.authorization.thinking_mode
            || grant.provider_timeout_ms == 0
            || grant.bearer_token.trim().is_empty()
            || endpoint.scheme() != "https"
            || !endpoint.has_host()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("native model grant response invalid".into());
        }
        let bearer = format!("Bearer {}", grant.bearer_token);
        reqwest::header::HeaderValue::from_str(&bearer)
            .map_err(|_| "native model grant bearer invalid")?;
        let mut registry = ModelProviderRegistry::new();
        let mut provider = registry
            .get("openai.default")
            .ok_or("Responses provider adapter missing")?
            .clone();
        provider.provider_key = self.provider_id.clone();
        provider.base_url = None;
        provider.auth = AuthSpec::None;
        provider.env_http_headers.clear();
        provider.default_max_retries = Some(0);
        let config = ModelSessionConfig {
            provider_id: provider.provider_key.clone(),
            provider_kind: provider.provider_kind.clone(),
            api_base: Some(grant.api_base.clone()),
            model: grant.model,
            thinking_mode: grant.thinking_mode,
            max_output_tokens: Some(grant.max_output_tokens),
            timeout_ms: grant.provider_timeout_ms,
            max_retries: 0,
            ..ModelSessionConfig::default()
        };
        registry.insert_user_defined(provider);
        let transport = ReqwestJsonHttpTransport::for_session(
            &start.authorization.session_id,
            &self.provider_id,
            || Ok(self.http.clone()),
        )?;
        Ok(Some(NativeResponsesClient(Arc::new(NativeRun {
            registry: Arc::new(registry),
            config,
            transport,
            bearer,
            url: format!("{}/responses", grant.api_base.trim_end_matches('/')),
            authority: Authority {
                store,
                agent_run_id: start.agent_run_id.clone(),
                authorization_ref: start.authorization.id.clone(),
                authorization_digest: start.authorization_digest.clone(),
                lease_owner: lease_owner.to_owned(),
                provider_id: self.provider_id.clone(),
                digest: grant.authority_digest,
                model_config_ref: start.authorization.model_config_ref.clone(),
            },
        }))))
    }
}

#[derive(Clone)]
struct Authority {
    store: Arc<PostgresRuntimeStore>,
    agent_run_id: String,
    authorization_ref: String,
    authorization_digest: String,
    lease_owner: String,
    provider_id: String,
    digest: String,
    model_config_ref: String,
}

impl Authority {
    fn verify(&self) -> Result<(), String> {
        self.store.with_execution_control_client(|client| {
            let row = client
                .query_opt(
                    AUTHORITY_SQL,
                    &[
                        &self.agent_run_id,
                        &self.authorization_ref,
                        &self.authorization_digest,
                        &self.lease_owner,
                        &self.provider_id,
                    ],
                )
                .map_err(|_| "native_model_authority_unavailable")?;
            let stamp: String = row.ok_or("native_model_authority_denied")?.get(0);
            if format!("sha256:{:x}", Sha256::digest(stamp.as_bytes())) != self.digest {
                return Err("native_model_authority_denied".into());
            }
            Ok(())
        })
    }

    fn write_log(&self, log: AttemptOutcome) -> Result<(), String> {
        self.store.with_client(|client| {
            client.execute(
                r#"INSERT INTO app_core_modelrunlog
                    (id,"agentRunId","modelConfig_id",status,"promptTokens","completionTokens",
                    "totalTokens","promptCacheHitTokens","promptCacheMissTokens",error,"createdAt")
                    VALUES ('model_run_' || replace(gen_random_uuid()::text,'-',''),$1,$2,$3,$4,$5,$6,$7,$8,$9,clock_timestamp())"#,
                &[
                    &self.agent_run_id, &self.model_config_ref, &log.status, &log.input, &log.output,
                    &log.total, &log.cache_hit, &log.cache_miss, &log.error,
                ],
            ).map_err(|_| "native_model_log_unavailable".to_owned())?;
            Ok(())
        })
    }
}

#[derive(Clone)]
pub(crate) struct NativeResponsesClient(Arc<NativeRun>);

struct NativeRun {
    registry: Arc<ModelProviderRegistry>,
    config: ModelSessionConfig,
    transport: ReqwestJsonHttpTransport,
    bearer: String,
    url: String,
    authority: Authority,
}

impl NativeResponsesClient {
    pub(crate) fn config(&self) -> &ModelSessionConfig {
        &self.0.config
    }

    pub(crate) async fn generate(
        &self,
        request: &ModelClientRequest,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let attempt = Attempt::new(&self.0.authority);
        let result = self.client(&attempt).generate(request).await;
        attempt.finish(result).await
    }

    pub(crate) async fn generate_stream(
        &self,
        request: &ModelClientRequest,
        sink: &mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> Result<ModelClientResponse, ModelClientError> {
        let attempt = Attempt::new(&self.0.authority);
        let mut completion = Vec::new();
        let result = self
            .client(&attempt)
            .generate_stream(request, &mut |event| {
                if matches!(
                    event,
                    ModelClientStreamEvent::Done { .. }
                        | ModelClientStreamEvent::ToolCallReady { .. }
                ) {
                    completion.push(event);
                } else {
                    sink(event);
                }
            })
            .await;
        let response = attempt.finish(result).await?;
        for event in completion {
            sink(event);
        }
        Ok(response)
    }

    fn client<'a>(
        &'a self,
        attempt: &'a Attempt<'a>,
    ) -> OpenAiResponsesModelClient<FencedTransport<'a>> {
        let output_tokens = attempt.output_tokens.clone();
        OpenAiResponsesModelClient::with_shared_registry(
            self.0.registry.clone(),
            FencedTransport {
                run: &self.0,
                attempt,
            },
        )
        .with_output_tokens_observer(move |tokens| {
            let _ = output_tokens.set(tokens);
        })
    }
}

struct FencedTransport<'a> {
    run: &'a NativeRun,
    attempt: &'a Attempt<'a>,
}

impl FencedTransport<'_> {
    async fn authorize(
        &self,
        request: &JsonHttpRequest,
    ) -> Result<JsonHttpRequest, TransportError> {
        if request.url != self.run.url || request.method != "POST" {
            return Err(TransportError::Build("native_model_route_mismatch".into()));
        }
        let authority = self.run.authority.clone();
        tokio::task::spawn_blocking(move || authority.verify())
            .await
            .map_err(|_| TransportError::Build("native_model_authority_unavailable".into()))?
            .map_err(TransportError::Build)?;
        let mut request = request.clone();
        request
            .headers
            .insert("authorization".into(), self.run.bearer.clone());
        self.attempt.dispatched.store(true, Ordering::Relaxed);
        Ok(request)
    }

    fn observed(
        &self,
        result: Result<JsonHttpResponse, TransportError>,
    ) -> Result<JsonHttpResponse, TransportError> {
        match result {
            Ok(response) => {
                if let Ok(mut http) = self.attempt.http.lock() {
                    *http = Some((
                        response.status_code,
                        RetryAfter::from_headers(&response.headers),
                    ));
                }
                Ok(response)
            }
            Err(
                TransportError::Network(_)
                | TransportError::Timeout(_)
                | TransportError::StreamInterrupted(_),
            ) => Err(TransportError::InvalidResponse(
                "model_provider_outcome_unknown".into(),
            )),
            Err(error) => Err(error),
        }
    }
}

impl JsonHttpTransport for FencedTransport<'_> {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            let request = self.authorize(request).await?;
            self.observed(self.run.transport.execute_json(&request).await)
        })
    }

    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            let request = self.authorize(request).await?;
            self.observed(self.run.transport.execute_sse(&request, on_data).await)
        })
    }
}

struct Attempt<'a> {
    authority: &'a Authority,
    dispatched: AtomicBool,
    http: Mutex<Option<(u16, Option<RetryAfter>)>>,
    output_tokens: Arc<OnceLock<i64>>,
}

impl<'a> Attempt<'a> {
    fn new(authority: &'a Authority) -> Self {
        Self {
            authority,
            dispatched: AtomicBool::new(false),
            http: Mutex::new(None),
            output_tokens: Arc::new(OnceLock::new()),
        }
    }

    async fn finish(
        self,
        result: Result<ModelClientResponse, ModelClientError>,
    ) -> Result<ModelClientResponse, ModelClientError> {
        let result = result.and_then(|response| {
            let log = AttemptOutcome::success(&response, self.output_tokens.get().copied())
                .ok_or_else(|| {
                    ModelClientError::new(
                        ModelClientErrorKind::ProviderResponseInterrupted,
                        "provider_usage_invalid",
                        false,
                    )
                })?;
            Ok((response, log))
        });
        let (outcome, log) =
            match result {
                Ok((response, log)) => (Ok(response), log),
                Err(mut error) => {
                    let reason = failure_reason(&error);
                    if reason == "model_provider_outcome_unknown" {
                        error.retryable = false;
                    }
                    let status = self.http.lock().ok().and_then(|http| *http).and_then(
                        |(status, advice)| {
                            error.retry_after = advice.or(error.retry_after);
                            (400..=599).contains(&status).then_some(status)
                        },
                    );
                    set_native_failure(&mut error, reason, status);
                    (Err(error), AttemptOutcome::error(reason))
                }
            };
        if self.dispatched.swap(false, Ordering::Relaxed) {
            let authority = self.authority.clone();
            let _ = tokio::task::spawn_blocking(move || record(authority, log)).await;
        }
        outcome
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if self.dispatched.swap(false, Ordering::Relaxed) {
            let authority = self.authority.clone();
            tokio::task::spawn_blocking(move || {
                record(
                    authority,
                    AttemptOutcome::error("provider_stream_cancelled"),
                );
            });
        }
    }
}

fn record(authority: Authority, log: AttemptOutcome) {
    if authority.write_log(log).is_err() {
        eprintln!(
            "Native model attempt log unavailable; transitionReason=native_model_log_unavailable"
        );
    }
}

struct AttemptOutcome {
    status: &'static str,
    error: &'static str,
    input: Option<i32>,
    output: Option<i32>,
    total: Option<i32>,
    cache_hit: Option<i32>,
    cache_miss: Option<i32>,
}

impl AttemptOutcome {
    fn error(error: &'static str) -> Self {
        Self {
            status: "error",
            error,
            input: None,
            output: None,
            total: None,
            cache_hit: None,
            cache_miss: None,
        }
    }

    fn success(response: &ModelClientResponse, output_tokens: Option<i64>) -> Option<Self> {
        fn count(tokens: i64) -> Option<i32> {
            i32::try_from(tokens).ok().filter(|value| *value >= 0)
        }
        let result = &response.generate_result;
        let input = count(result.input_tokens?)?;
        let total = count(result.total_tokens?)?;
        let output = count(output_tokens?)?;
        let optional = |value: Option<i64>| match value {
            Some(value) => count(value).map(Some),
            None => Some(None),
        };
        Some(Self {
            status: "success",
            error: "",
            input: Some(input),
            output: Some(output),
            total: Some(total),
            cache_hit: optional(result.prompt_cache_hit_tokens)?,
            cache_miss: optional(result.prompt_cache_miss_tokens)?,
        })
    }
}

fn failure_reason(error: &ModelClientError) -> &'static str {
    match error.provider_code.as_deref() {
        Some("incomplete_output_token_limit") => return "incomplete_output_token_limit",
        Some("incomplete_response") => return "incomplete_response",
        _ => {}
    }
    match error.message.as_str() {
        "native_model_authority_denied" => return "native_model_authority_denied",
        "native_model_authority_unavailable" => return "native_model_authority_unavailable",
        "model_provider_outcome_unknown" | "SSE stream ended before a terminal response event" => {
            return "model_provider_outcome_unknown";
        }
        "provider_usage_invalid" => return "provider_usage_invalid",
        _ => {}
    }
    match error.kind {
        ModelClientErrorKind::Timeout => "provider_timeout",
        ModelClientErrorKind::Network => "provider_unreachable",
        ModelClientErrorKind::ProviderBusyOrRateLimited => "provider_rate_limited",
        ModelClientErrorKind::ProviderUnavailable | ModelClientErrorKind::ModelUnavailable => {
            "provider_unavailable"
        }
        ModelClientErrorKind::AuthFailed => "provider_authentication_failed",
        ModelClientErrorKind::ProviderResponseInterrupted => "provider_response_invalid",
        ModelClientErrorKind::InvalidRequest => "provider_request_rejected",
        ModelClientErrorKind::Cancelled => "model_run_cancelled",
        _ => "model_adapter_failed",
    }
}

#[cfg(test)]
mod tests;
