use centaeris_core::tool::inputs::{
    DeclaredInput, DeferredInputResolutionError, DeferredInputResolutionFailureKind,
    DeferredInputResolverPort, ResolvedInput,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

const RESOLVE_DEFERRED_INPUT_SCHEMA: &str = "runtime.deferred_input.resolve.v1";

pub struct ApiDeferredInputResolver {
    api_internal_url: String,
    internal_api_token: String,
    agent_run_id: String,
    authorization_digest: String,
    client: reqwest::blocking::Client,
}

impl ApiDeferredInputResolver {
    pub fn new(
        api_internal_url: String,
        internal_api_token: String,
        agent_run_id: String,
        authorization_digest: String,
    ) -> Self {
        Self {
            api_internal_url,
            internal_api_token,
            agent_run_id,
            authorization_digest,
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl DeferredInputResolverPort for ApiDeferredInputResolver {
    fn resolve_deferred_input(
        &self,
        reference: &DeclaredInput,
    ) -> Result<ResolvedInput, DeferredInputResolutionError> {
        let url = format!(
            "{}/internal/agent-runs/resolve-input",
            self.api_internal_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Internal-Token", self.internal_api_token.as_str())
            .json(&ResolveDeferredInputRequest {
                schema: RESOLVE_DEFERRED_INPUT_SCHEMA,
                agent_run_id: self.agent_run_id.as_str(),
                authorization_digest: self.authorization_digest.as_str(),
                input_ref: reference.input_ref.as_str(),
            })
            .send()
            .map_err(|error| {
                host_error(format!("request deferred input resolution failed: {error}"))
            })?;
        let status = response.status();
        let body = response.text().map_err(|error| {
            host_error(format!("read deferred input resolution failed: {error}"))
        })?;
        if !status.is_success() {
            let kind = serde_json::from_str::<ResolveDeferredInputFailure>(body.as_str())
                .ok()
                .and_then(|failure| match failure.error.as_str() {
                    "asset_removed" => Some(DeferredInputResolutionFailureKind::AssetRemoved),
                    "access_revoked" => Some(DeferredInputResolutionFailureKind::AccessRevoked),
                    "source_deleted" => Some(DeferredInputResolutionFailureKind::SourceDeleted),
                    "stale_generation" => Some(DeferredInputResolutionFailureKind::StaleGeneration),
                    "asset_unavailable" => {
                        Some(DeferredInputResolutionFailureKind::AssetUnavailable)
                    }
                    _ => None,
                })
                .unwrap_or(DeferredInputResolutionFailureKind::HostUnavailable);
            return Err(DeferredInputResolutionError::new(
                kind,
                format!("deferred input resolution returned {}", status.as_u16()),
            ));
        }
        let payload = serde_json::from_str::<ResolveDeferredInputResponse>(body.as_str()).map_err(
            |error| host_error(format!("decode deferred input resolution failed: {error}")),
        )?;
        if payload.schema != RESOLVE_DEFERRED_INPUT_SCHEMA {
            return Err(host_error("deferred input resolution schema mismatch"));
        }
        Ok(payload.resolved_input)
    }
}

fn host_error(message: impl Into<String>) -> DeferredInputResolutionError {
    DeferredInputResolutionError::new(DeferredInputResolutionFailureKind::HostUnavailable, message)
}

pub(crate) struct ApiModelInputImageResolver {
    inputs: Arc<centaeris_core::tool::inputs::ResolvedInputState>,
    api_url: String,
    token: String,
    agent_run_id: String,
    authorization_digest: String,
    client: reqwest::blocking::Client,
}

impl ApiModelInputImageResolver {
    pub(crate) fn new(
        inputs: Arc<centaeris_core::tool::inputs::ResolvedInputState>,
        api_url: String,
        token: String,
        agent_run_id: String,
        authorization_digest: String,
    ) -> Result<Self, String> {
        Ok(Self {
            inputs,
            api_url,
            token,
            agent_run_id,
            authorization_digest,
            client: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| "model_input_image_client_failed")?,
        })
    }
}

impl centaeris_core::model::prepared_prompt::ModelInputImageResolverPort
    for ApiModelInputImageResolver
{
    fn resolve(&self, input_ref: &str, content_type: &str) -> Result<Vec<u8>, String> {
        crate::postgres_store::run_postgres_blocking(|| {
            self.resolve_blocking(input_ref, content_type)
        })
    }
}

impl ApiModelInputImageResolver {
    fn resolve_blocking(&self, input_ref: &str, content_type: &str) -> Result<Vec<u8>, String> {
        use centaeris_core::model::prepared_prompt::MODEL_INPUT_IMAGE_MAX_BYTES;
        let captured = self
            .inputs
            .resolve_input(input_ref)
            .map_err(|_| "model_input_image_not_authorized")?;
        if captured.content_type != content_type
            || !matches!(content_type, "image/png" | "image/jpeg" | "image/webp")
            || captured.size_bytes > MODEL_INPUT_IMAGE_MAX_BYTES as u64
        {
            return Err("model_input_image_binding_invalid".into());
        }
        let response = self.client.post(format!("{}/internal/agent-runs/read-input", self.api_url.trim_end_matches('/')))
            .header("X-Internal-Token", &self.token)
            .json(&serde_json::json!({"schema":"runtime.deferred_input.read.v1", "agentRunId":self.agent_run_id,
                "authorizationDigest":self.authorization_digest, "inputRef":input_ref,
                "sourceVersion":captured.source_version, "sha256":captured.sha256}))
            .send().map_err(|_| "model_input_image_read_unavailable")?;
        if !response.status().is_success()
            || response
                .headers()
                .get("X-Content-Sha256")
                .and_then(|value| value.to_str().ok())
                != Some(captured.sha256.as_str())
            || response
                .headers()
                .get("X-Source-Version")
                .and_then(|value| value.to_str().ok())
                != Some(captured.source_version.as_str())
            || response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.split(';').next().unwrap_or_default())
                != Some(content_type)
        {
            return Err("model_input_image_read_binding_invalid".into());
        }
        let mut bytes = Vec::new();
        response
            .take((MODEL_INPUT_IMAGE_MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "model_input_image_read_failed")?;
        if bytes.len() > MODEL_INPUT_IMAGE_MAX_BYTES
            || bytes.len() as u64 != captured.size_bytes
            || format!("sha256:{:x}", Sha256::digest(&bytes)) != captured.sha256
        {
            return Err("model_input_image_content_identity_mismatch".into());
        }
        if self
            .inputs
            .resolve_input(input_ref)
            .map_err(|_| "model_input_image_not_authorized")?
            != captured
        {
            return Err("model_input_image_binding_changed".into());
        }
        Ok(bytes)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolveDeferredInputRequest<'a> {
    schema: &'static str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    input_ref: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveDeferredInputResponse {
    schema: String,
    resolved_input: ResolvedInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveDeferredInputFailure {
    error: String,
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use centaeris_core::model::prepared_prompt::ModelInputImageResolverPort;
    use centaeris_core::tool::inputs::{ResolvedInputManifest, ResolvedInputState};
    use std::io::Write;

    fn state(bytes: &[u8], size: u64) -> Arc<ResolvedInputState> {
        let sha = format!("sha256:{:x}", Sha256::digest(bytes));
        let declared = serde_json::from_value(serde_json::json!({
            "schema":"runtime.declared_input.v1","inputRef":"image-one","displayName":"one.png",
            "contentType":"image/png","sizeBytes":size,
            "inputIdentity":{"ownerKind":"userLibraryObject","ownerId":"library-one","generation":1,"sha256":sha}
        })).unwrap();
        let resolved = serde_json::from_value(serde_json::json!({
            "schema":"runtime.resolved_input.v1","inputRef":"image-one","displayName":"one.png",
            "contentType":"image/png","sizeBytes":size,"ownerKind":"userLibraryObject","objectRef":"library-one",
            "virtualPath":"inputs/image-one/one.png","sha256":sha,"sourceVersion":"1",
            "evidenceKind":"uploadedFile","citationAllowed":true
        })).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        Arc::new(
            ResolvedInputState::new(
                "run-one".into(),
                digest.clone(),
                vec![declared],
                ResolvedInputManifest {
                    schema: "runtime.resolved_input_manifest.v1".into(),
                    agent_run_id: "run-one".into(),
                    authorization_digest: digest,
                    inputs: vec![resolved],
                },
                None,
            )
            .unwrap(),
        )
    }

    #[test]
    fn image_reader_rejects_undeclared_and_oversized_captures_before_http() {
        let inputs = state(b"png", 3);
        let reader = ApiModelInputImageResolver::new(
            inputs,
            "http://127.0.0.1:1".into(),
            "test-token".into(),
            "run-one".into(),
            format!("sha256:{}", "a".repeat(64)),
        )
        .unwrap();
        assert_eq!(
            reader.resolve("undeclared", "image/png").unwrap_err(),
            "model_input_image_not_authorized"
        );
        assert_eq!(
            reader.resolve("image-one", "image/jpeg").unwrap_err(),
            "model_input_image_binding_invalid"
        );
        let reader = ApiModelInputImageResolver::new(
            state(b"png", 10 * 1024 * 1024 + 1),
            "http://127.0.0.1:1".into(),
            "test-token".into(),
            "run-one".into(),
            format!("sha256:{}", "a".repeat(64)),
        )
        .unwrap();
        assert_eq!(
            reader.resolve("image-one", "image/png").unwrap_err(),
            "model_input_image_binding_invalid"
        );
    }

    #[test]
    fn image_reader_checks_captured_headers_and_actual_bytes() {
        for (version, response_bytes, expected) in [
            (
                "2",
                b"png".as_slice(),
                "model_input_image_read_binding_invalid",
            ),
            (
                "1",
                b"bad".as_slice(),
                "model_input_image_content_identity_mismatch",
            ),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let sha = format!("sha256:{:x}", Sha256::digest(b"png"));
            let body = response_bytes.to_vec();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let length = stream.read(&mut request).unwrap();
                assert!(String::from_utf8_lossy(&request[..length])
                    .starts_with("POST /internal/agent-runs/read-input "));
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nX-Content-Sha256: {sha}\r\nX-Source-Version: {version}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                stream.write_all(&body).unwrap();
            });
            let reader = ApiModelInputImageResolver::new(
                state(b"png", 3),
                url,
                "test-token".into(),
                "run-one".into(),
                format!("sha256:{}", "a".repeat(64)),
            )
            .unwrap();
            assert_eq!(
                reader.resolve("image-one", "image/png").unwrap_err(),
                expected
            );
            server.join().unwrap();
        }
    }
}
