use std::io::{Cursor, Read};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::docker_execution_host::{DockerExecutionHostRunner, PreparedArtifact};

const API_STATUS_SCHEMA: &str = "artifact.publication.status.v1";
const API_METADATA_SCHEMA: &str = "artifact.publication.v1";
const API_RESULT_SCHEMA: &str = "artifact.publication.result.v1";
const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactPublicationRequest {
    pub(crate) tool_call_id: String,
    pub(crate) path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtifactPublicationReceipt {
    pub(crate) publication_id: String,
    pub(crate) artifact_ref: String,
    pub(crate) filename: String,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

pub(crate) struct WorkspaceArtifactPublicationPort {
    sandbox: Arc<DockerExecutionHostRunner>,
    api_url: String,
    api_token: String,
    agent_run_id: String,
    authorization_digest: String,
    client: reqwest::blocking::Client,
}

impl WorkspaceArtifactPublicationPort {
    pub(crate) fn new(
        sandbox: Arc<DockerExecutionHostRunner>,
        api_url: String,
        api_token: String,
        agent_run_id: String,
        authorization_digest: String,
    ) -> Result<Self, String> {
        if api_url.is_empty()
            || api_url.trim_end_matches('/') != api_url
            || api_token.trim().is_empty()
            || agent_run_id.trim().is_empty()
            || authorization_digest.trim().is_empty()
        {
            return Err("artifact publication binding is invalid".to_string());
        }
        Ok(Self {
            sandbox,
            api_url,
            api_token,
            agent_run_id,
            authorization_digest,
            client: reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|error| format!("build artifact publisher failed: {error}"))?,
        })
    }

    fn published(
        &self,
        publication_id: &str,
        tool_call_id: &str,
        expected: Option<&ApiPublicationMetadata<'_>>,
    ) -> Result<Option<ArtifactPublicationReceipt>, String> {
        let query = ApiPublicationStatusQuery {
            schema: API_STATUS_SCHEMA,
            publication_id,
            agent_run_id: self.agent_run_id.as_str(),
            authorization_digest: self.authorization_digest.as_str(),
            tool_call_id,
        };
        let mut last_error = None;
        for _ in 0..MAX_ATTEMPTS {
            match self
                .client
                .post(format!("{}/internal/artifacts/status", self.api_url))
                .header("X-Internal-Token", self.api_token.as_str())
                .json(&query)
                .timeout(Duration::from_secs(10))
                .send()
            {
                Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => {
                    return Ok(None)
                }
                Ok(response) if response.status().is_success() => {
                    match response.json::<ApiPublicationResult>() {
                        Ok(value) => {
                            return receipt_from_api(value, publication_id, expected).map(Some)
                        }
                        Err(error) => {
                            last_error = Some(format!("decode artifact status failed: {error}"));
                        }
                    }
                }
                Ok(response) if response.status().is_server_error() => {
                    last_error = Some(api_error(response, "artifact status"));
                }
                Ok(response) => return Err(api_error(response, "artifact status")),
                Err(error) => last_error = Some(format!("artifact status unavailable: {error}")),
            }
        }
        Err(last_error.unwrap_or_else(|| "artifact status unavailable".to_string()))
    }

    fn upload(
        &self,
        metadata: &ApiPublicationMetadata,
        artifact: &PreparedArtifact,
    ) -> Result<ArtifactPublicationReceipt, String> {
        let metadata_bytes = serde_json::to_vec(metadata)
            .map_err(|error| format!("encode artifact metadata failed: {error}"))?;
        let metadata_len = u32::try_from(metadata_bytes.len())
            .map_err(|_| "artifact metadata is too large".to_string())?;
        let mut prefix = Vec::with_capacity(4 + metadata_bytes.len());
        prefix.extend_from_slice(&metadata_len.to_be_bytes());
        prefix.extend_from_slice(&metadata_bytes);
        let length = prefix.len() as u64 + metadata.size_bytes;

        let mut last_error = None;
        for _ in 0..MAX_ATTEMPTS {
            let body = Cursor::new(prefix.clone()).chain(artifact.reader()?);
            match self
                .client
                .post(format!("{}/internal/artifacts/publish", self.api_url))
                .header("X-Internal-Token", self.api_token.as_str())
                .header("Content-Type", "application/octet-stream")
                .body(reqwest::blocking::Body::sized(body, length))
                .timeout(Duration::from_secs(45))
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    match response.json::<ApiPublicationResult>() {
                        Ok(value) => {
                            match receipt_from_api(value, metadata.publication_id, Some(metadata)) {
                                Ok(receipt) => return Ok(receipt),
                                Err(error) => last_error = Some(error),
                            }
                        }
                        Err(error) => {
                            last_error =
                                Some(format!("decode artifact publication failed: {error}"));
                        }
                    }
                }
                Ok(response) if response.status().is_server_error() => {
                    last_error = Some(api_error(response, "artifact publication"));
                }
                Ok(response) => return Err(api_error(response, "artifact publication")),
                Err(error) => {
                    last_error = Some(format!("artifact publication unavailable: {error}"));
                }
            }
            // The server may have committed before losing its reply. Recover
            // that exact receipt before resending any bytes or reporting failure.
            if let Some(receipt) = self.published(
                metadata.publication_id,
                metadata.tool_call_id,
                Some(metadata),
            )? {
                return Ok(receipt);
            }
        }
        Err(last_error.unwrap_or_else(|| "artifact publication unavailable".to_string()))
    }

    pub(crate) fn publish_artifact(
        &self,
        request: ArtifactPublicationRequest,
    ) -> Result<ArtifactPublicationReceipt, String> {
        let publication_id = stable_artifact_publication_id(
            self.agent_run_id.as_str(),
            request.tool_call_id.as_str(),
        );
        if let Some(receipt) =
            self.published(publication_id.as_str(), request.tool_call_id.as_str(), None)?
        {
            return Ok(receipt);
        }
        let artifact = self.sandbox.read_artifact(request.path.as_str())?;
        let metadata = ApiPublicationMetadata {
            schema: API_METADATA_SCHEMA,
            publication_id: publication_id.as_str(),
            agent_run_id: self.agent_run_id.as_str(),
            authorization_digest: self.authorization_digest.as_str(),
            tool_call_id: request.tool_call_id.as_str(),
            filename: artifact.metadata.filename.as_str(),
            size_bytes: artifact.metadata.size_bytes,
            sha256: artifact.metadata.sha256.as_str(),
        };
        self.upload(&metadata, &artifact)
    }
}

fn stable_artifact_publication_id(agent_run_id: &str, tool_call_id: &str) -> String {
    let encoded = serde_json::to_vec(&[agent_run_id, tool_call_id])
        .expect("serialize artifact publication identity");
    format!("pub_{:x}", Sha256::digest(encoded))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiPublicationStatusQuery<'a> {
    schema: &'static str,
    publication_id: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    tool_call_id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiPublicationMetadata<'a> {
    schema: &'static str,
    publication_id: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    tool_call_id: &'a str,
    filename: &'a str,
    size_bytes: u64,
    sha256: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApiPublicationResult {
    schema: String,
    publication_id: String,
    artifact_ref: String,
    filename: String,
    content_type: String,
    size_bytes: u64,
    sha256: String,
}

fn receipt_from_api(
    value: ApiPublicationResult,
    publication_id: &str,
    expected: Option<&ApiPublicationMetadata<'_>>,
) -> Result<ArtifactPublicationReceipt, String> {
    let artifact_id = value
        .artifact_ref
        .strip_prefix("artifact:")
        .unwrap_or_default();
    if value.schema != API_RESULT_SCHEMA
        || value.publication_id != publication_id
        || artifact_id.is_empty()
        || value.content_type.is_empty()
        || value.content_type.len() > 160
        || expected.is_some_and(|metadata| {
            metadata.filename != value.filename
                || metadata.size_bytes != value.size_bytes
                || metadata.sha256 != value.sha256
        })
    {
        return Err("artifact publication result binding mismatch".to_string());
    }
    Ok(ArtifactPublicationReceipt {
        publication_id: value.publication_id,
        artifact_ref: value.artifact_ref,
        filename: value.filename,
        size_bytes: value.size_bytes,
        sha256: value.sha256,
    })
}

fn api_error(response: reqwest::blocking::Response, operation: &str) -> String {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let code = serde_json::from_str::<serde_json::Value>(body.as_str())
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "invalid_error_response".to_string());
    format!("{operation} returned {}: {code}", status.as_u16())
}
