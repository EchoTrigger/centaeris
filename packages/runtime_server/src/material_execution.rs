//! First-party original-file delivery into the current Session execution host.
use std::sync::Arc;
use std::time::Duration;

use centaeris_core::execution::MAX_EXECUTION_INPUT_BYTES;
use centaeris_core::tool::inputs::{
    DeclaredInput, InputIdentityV1, ResolvedInput, ResolvedInputState, DECLARED_INPUT_SCHEMA,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::docker_execution_host::DockerExecutionHostRunner;

// Matches the API's protected-input admission and validate-inputs batch limit.
const MAX_SESSION_MATERIAL_INPUTS: usize = 128;

pub(crate) struct MaterialExecutionDelivery {
    api_url: String,
    token: String,
    run_id: String,
    digest: String,
    http: reqwest::Client,
    host: Arc<DockerExecutionHostRunner>,
    inputs: Arc<ResolvedInputState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Context {
    schema: String,
    personal_library: bool,
    inputs: Vec<ResolvedInput>,
    pending_inputs: Vec<PendingInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PendingInput {
    pub(crate) resolved_input: ResolvedInput,
    pub(crate) state: String,
    pub(crate) reservations: Vec<Reservation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Reservation {
    pub(crate) admission_id: i64,
    pub(crate) origin_run_id: String,
    pub(crate) call_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Binding {
    schema: String,
    resolved_input: ResolvedInput,
}

impl MaterialExecutionDelivery {
    pub(crate) fn new(
        api_url: String,
        token: String,
        run_id: String,
        digest: String,
        host: Arc<DockerExecutionHostRunner>,
        inputs: Arc<ResolvedInputState>,
    ) -> Result<Self, String> {
        Ok(Self {
            api_url,
            token,
            run_id,
            digest,
            host,
            inputs,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|_| "material delivery client unavailable")?,
        })
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: Value) -> Result<T, String> {
        let mut response = self
            .http
            .post(format!(
                "{}/internal/agent-runs/{path}",
                self.api_url.trim_end_matches('/')
            ))
            .header("X-Internal-Token", &self.token)
            .json(&body)
            .send()
            .await
            .map_err(|_| "material delivery API unavailable")?;
        if !response.status().is_success() {
            return Err("material delivery authorization rejected".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "material delivery response failed")?
        {
            if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
                return Err("material delivery response oversized".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| "material delivery response invalid".into())
    }

    async fn read_context(&self) -> Result<Context, String> {
        let context: Context = self
            .post(
                "materials/context",
                json!({
            "schema":"workspace.material.context.read.v1", "agentRunId":self.run_id,
            "authorizationDigest":self.digest}),
            )
            .await?;
        if context.schema != "workspace.material.context.result.v1"
            || context.inputs.len() > MAX_SESSION_MATERIAL_INPUTS
            || context.pending_inputs.len() > MAX_SESSION_MATERIAL_INPUTS
        {
            return Err("material context invalid".into());
        }
        Ok(context)
    }

    pub(crate) async fn restore(&self) -> Result<bool, String> {
        let mut context = self.read_context().await?;
        // Validate retained inputs before admitting them into the new Run.
        if !context.pending_inputs.is_empty() {
            let host = self.host.clone();
            let pending = context.pending_inputs;
            tokio::task::spawn_blocking(move || host.reconcile_material_inputs(&pending))
                .await
                .map_err(|_| "material input reconciliation interrupted")??;
            context = self.read_context().await?;
            if !context.pending_inputs.is_empty() {
                return Err("material reconciled context invalid".into());
            }
        }
        for input in context.inputs {
            self.deliver(input.clone()).await?;
            self.confirm_outputs(input).await?;
        }
        Ok(context.personal_library)
    }

    pub(crate) async fn fetch(&self, input_ref: &str, call_id: &str) -> Result<Value, String> {
        let prepared: Binding = self
            .post(
                "materials/prepare",
                json!({
            "schema":"workspace.material.prepare.v1", "agentRunId":self.run_id,
            "authorizationDigest":self.digest,"inputRef":input_ref,"callId":call_id}),
            )
            .await?;
        if prepared.schema != "workspace.material.prepare.result.v1" {
            return Err("material preparation invalid".into());
        }
        let input = prepared.resolved_input;
        let path = self.deliver(input.clone()).await?;
        let confirmed: Binding = self
            .post(
                "materials/confirm",
                json!({
            "schema":"workspace.material.confirm.v1","agentRunId":self.run_id,
            "authorizationDigest":self.digest,"inputRef":input.input_ref,
            "sourceVersion":input.source_version,"sha256":input.sha256,"callId":call_id}),
            )
            .await?;
        if confirmed.schema != "workspace.material.confirm.result.v1"
            || confirmed.resolved_input != input
        {
            return Err("material confirmation binding mismatch".into());
        }
        self.confirm_outputs(input.clone()).await?;
        Ok(
            json!({"kind":"execution_file","delivery":"execution_file","inputRef":input.input_ref,
            "path":path,"displayName":input.display_name,"contentType":input.content_type,
            "sizeBytes":input.size_bytes,"sha256":input.sha256,"sourceVersion":input.source_version}),
        )
    }

    async fn confirm_outputs(&self, input: ResolvedInput) -> Result<(), String> {
        let host = self.host.clone();
        tokio::task::spawn_blocking(move || host.confirm_material_input_outputs(&input))
            .await
            .map_err(|_| "material output confirmation interrupted".into())
    }

    async fn deliver(&self, input: ResolvedInput) -> Result<String, String> {
        input.validate()?;
        if input.size_bytes > MAX_EXECUTION_INPUT_BYTES {
            return Err("material exceeds execution input limit".into());
        }
        let declared = declared_input(&input)?;
        let host = self.host.clone();
        let lookup = input.clone();
        let existing = tokio::task::spawn_blocking(move || host.execution_input_path(&lookup))
            .await
            .map_err(|_| "material input lookup interrupted")??;
        let path = if let Some(path) = existing {
            path
        } else {
            let bytes = self.read_bytes(&input).await?;
            let host = self.host.clone();
            let materialized = input.clone();
            tokio::task::spawn_blocking(move || {
                host.materialize_execution_input(&materialized, &bytes)
            })
            .await
            .map_err(|_| "material installation interrupted")??
        };
        self.inputs.admit_declared_inputs(&[declared])?;
        // Existing resolver rechecks current authorization and the immutable version.
        let inputs = self.inputs.clone();
        let expected = input.clone();
        let resolved =
            tokio::task::spawn_blocking(move || inputs.resolve_input(&expected.input_ref))
                .await
                .map_err(|_| "material admission interrupted")?
                .map_err(|_| "material admission rejected")?;
        if resolved != input {
            return Err("material admission binding mismatch".into());
        }
        Ok(path)
    }

    async fn read_bytes(&self, input: &ResolvedInput) -> Result<Vec<u8>, String> {
        let mut response = self.http.post(format!("{}/internal/agent-runs/read-input", self.api_url.trim_end_matches('/')))
            .header("X-Internal-Token", &self.token).json(&json!({"schema":"runtime.deferred_input.read.v1",
                "agentRunId":self.run_id,"authorizationDigest":self.digest,"inputRef":input.input_ref,
                "sourceVersion":input.source_version,"sha256":input.sha256})).send().await
            .map_err(|_| "material original unavailable")?;
        if !response.status().is_success()
            || response
                .headers()
                .get("X-Content-Sha256")
                .and_then(|v| v.to_str().ok())
                != Some(input.sha256.as_str())
            || response
                .headers()
                .get("X-Source-Version")
                .and_then(|v| v.to_str().ok())
                != Some(input.source_version.as_str())
        {
            return Err("material original binding rejected".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "material original read failed")?
        {
            if (bytes.len() as u64).saturating_add(chunk.len() as u64) > input.size_bytes {
                return Err("material original size mismatch".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() as u64 != input.size_bytes
            || format!("sha256:{:x}", Sha256::digest(&bytes)) != input.sha256
        {
            return Err("material original identity mismatch".into());
        }
        Ok(bytes)
    }
}

fn declared_input(input: &ResolvedInput) -> Result<DeclaredInput, String> {
    let declared = DeclaredInput {
        schema: DECLARED_INPUT_SCHEMA.into(),
        input_ref: input.input_ref.clone(),
        display_name: input.display_name.clone(),
        content_type: input.content_type.clone(),
        size_bytes: input.size_bytes,
        input_identity: InputIdentityV1 {
            owner_kind: input.owner_kind.clone(),
            owner_id: input.object_ref.clone(),
            generation: input
                .source_version
                .parse()
                .map_err(|_| "material source version invalid")?,
            sha256: input.sha256.clone(),
        },
    };
    declared.validate()?;
    Ok(declared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::tool::inputs::ResolvedInputManifest;
    use std::io::{Read, Write};

    fn input() -> ResolvedInput {
        serde_json::from_value(json!({"schema":"runtime.resolved_input.v1","inputRef":"library-file",
            "objectRef":"file-one","ownerKind":"userLibraryObject","virtualPath":"inputs/library-file/file.zip",
            "displayName":"file.zip","contentType":"application/zip","sizeBytes":3,
            "sha256":format!("sha256:{:x}",Sha256::digest(b"zip")),"sourceVersion":"1",
            "evidenceKind":"uploadedFile","citationAllowed":false})).unwrap()
    }

    fn delivery(url: String) -> MaterialExecutionDelivery {
        let digest = format!("sha256:{}", "a".repeat(64));
        let inputs = Arc::new(
            ResolvedInputState::new(
                "run-one".into(),
                digest.clone(),
                vec![],
                ResolvedInputManifest {
                    schema: "runtime.resolved_input_manifest.v1".into(),
                    agent_run_id: "run-one".into(),
                    authorization_digest: digest.clone(),
                    inputs: vec![],
                },
                None,
            )
            .unwrap(),
        );
        let mut delivery = MaterialExecutionDelivery::new(
            url,
            "test-token".into(),
            "run-one".into(),
            digest,
            Arc::new(crate::docker_execution_host::material_delivery_test_host()),
            inputs,
        )
        .unwrap();
        delivery.http = reqwest::Client::builder().no_proxy().build().unwrap();
        delivery
    }

    #[test]
    fn context_accepts_more_than_direct_attachments_and_rejects_over_protected_input_limit() {
        for field in ["inputs", "pendingInputs"] {
            for (count, accepted) in [(65, true), (129, false)] {
                let mut values = Vec::new();
                for index in 0..count {
                    let mut original = input();
                    original.input_ref = format!("library-{index}");
                    original.object_ref = format!("file-{index}");
                    original.virtual_path = format!("inputs/library-{index}/file.zip");
                    values.push(if field == "inputs" {
                        serde_json::to_value(original).unwrap()
                    } else {
                        json!({"resolvedInput":original,"state":"access_revoked","reservations":[]})
                    });
                }
                let mut response = json!({"schema":"workspace.material.context.result.v1",
                    "personalLibrary":true,"inputs":[],"pendingInputs":[]});
                response[field] = Value::Array(values);
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let url = format!("http://{}", listener.local_addr().unwrap());
                let server = std::thread::spawn(move || {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut request = Vec::new();
                    loop {
                        let mut chunk = [0_u8; 4096];
                        let count = stream.read(&mut chunk).unwrap();
                        assert!(count > 0, "fixture request must complete");
                        request.extend_from_slice(&chunk[..count]);
                        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                        {
                            let headers = String::from_utf8_lossy(&request[..end]);
                            let size: usize = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse().unwrap())
                                })
                                .unwrap();
                            if request.len() >= end + 4 + size {
                                break;
                            }
                        }
                    }
                    assert!(String::from_utf8_lossy(&request)
                        .starts_with("POST /internal/agent-runs/materials/context "));
                    let body = serde_json::to_vec(&response).unwrap();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                });
                let reader = delivery(url);
                let runtime = tokio::runtime::Runtime::new().unwrap();
                let result = runtime.block_on(reader.read_context());
                assert_eq!(result.is_ok(), accepted, "{field} count {count}");
                if !accepted {
                    assert_eq!(result.err().unwrap(), "material context invalid");
                }
                server.join().unwrap();
            }
        }
    }

    #[test]
    fn original_download_rejects_changed_version_digest_and_length() {
        for (version, bytes, expected) in [
            ("2", b"zip".as_slice(), "material original binding rejected"),
            (
                "1",
                b"bad".as_slice(),
                "material original identity mismatch",
            ),
            ("1", b"zips".as_slice(), "material original size mismatch"),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let body = bytes.to_vec();
            let hash = input().sha256;
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0_u8; 4096];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0, "fixture request must complete");
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let size: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        if request.len() >= end + 4 + size {
                            break;
                        }
                    }
                }
                assert!(String::from_utf8_lossy(&request)
                    .starts_with("POST /internal/agent-runs/read-input "));
                write!(stream,"HTTP/1.1 200 OK\r\nX-Content-Sha256: {hash}\r\nX-Source-Version: {version}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                stream.write_all(&body).unwrap();
            });
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let reader = delivery(url);
            assert_eq!(
                runtime.block_on(reader.read_bytes(&input())).unwrap_err(),
                expected
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn original_admission_rejects_oversized_inputs_before_host_dispatch() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let reader = delivery("http://127.0.0.1:1".into());
        let mut original = input();
        original.size_bytes = MAX_EXECUTION_INPUT_BYTES + 1;
        assert_eq!(
            runtime.block_on(reader.deliver(original)).unwrap_err(),
            "material exceeds execution input limit"
        );
    }
}
