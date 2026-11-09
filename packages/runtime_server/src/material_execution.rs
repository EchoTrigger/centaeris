//! First-party original-file delivery into the current Session execution host.
use std::fs::File;
use std::sync::Arc;
use std::time::Duration;

use centaeris_core::execution::MAX_EXECUTION_INPUT_BYTES;
use centaeris_core::tool::inputs::{
    DeclaredInput, InputIdentityV1, ResolvedInput, ResolvedInputState, DECLARED_INPUT_SCHEMA,
};
use hosted_execution::protocol::SandboxMaterializedInput;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::docker_execution_host::DockerExecutionHostRunner;

// Matches the API's protected-input admission and validate-inputs batch limit.
const MAX_SESSION_MATERIAL_INPUTS: usize = 128;

/// A verified, rewindable frame for the Host helper. The temporary file stays
/// private and can be streamed again if Host admission asks us to wait.
#[derive(Debug)]
pub(crate) struct VerifiedMaterialInputFrame {
    input: ResolvedInput,
    file: File,
    descriptor: SandboxMaterializedInput,
    length: u64,
    digest: String,
}

impl VerifiedMaterialInputFrame {
    pub(crate) fn into_parts(self) -> (ResolvedInput, SandboxMaterializedInput, File, u64, String) {
        (
            self.input,
            self.descriptor,
            self.file,
            self.length,
            self.digest,
        )
    }

    async fn read(input: &ResolvedInput, response: &mut reqwest::Response) -> Result<Self, String> {
        let descriptor = SandboxMaterializedInput {
            input_ref: input.input_ref.clone(),
            virtual_path: input.virtual_path.clone(),
            size_bytes: input.size_bytes,
            sha256: input.sha256.clone(),
            source_version: input.source_version.clone(),
            state: None,
        };
        let metadata =
            serde_json::to_vec(&descriptor).map_err(|_| "execution input metadata invalid")?;
        let metadata_length =
            u32::try_from(metadata.len()).map_err(|_| "execution input metadata oversized")?;
        let header = metadata_length.to_be_bytes();
        let file = tokio::task::spawn_blocking(tempfile::tempfile)
            .await
            .map_err(|_| "material input spool interrupted")?
            .map_err(|_| "material input spool unavailable")?;
        let mut file = tokio::fs::File::from_std(file);
        let mut frame_hash = Sha256::new();
        for prefix in [header.as_slice(), metadata.as_slice()] {
            file.write_all(prefix)
                .await
                .map_err(|_| "material input spool write failed")?;
            frame_hash.update(prefix);
        }
        let mut body_hash = Sha256::new();
        let mut size = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "material original read failed")?
        {
            size = size.saturating_add(chunk.len() as u64);
            if size > input.size_bytes {
                return Err("material original size mismatch".into());
            }
            body_hash.update(&chunk);
            frame_hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|_| "material input spool write failed")?;
        }
        if size != input.size_bytes || format!("sha256:{:x}", body_hash.finalize()) != input.sha256
        {
            return Err("material original identity mismatch".into());
        }
        file.flush()
            .await
            .map_err(|_| "material input spool write failed")?;
        Ok(Self {
            input: input.clone(),
            file: file.into_std().await,
            descriptor,
            length: header.len() as u64 + u64::from(metadata_length) + size,
            digest: format!("sha256:{:x}", frame_hash.finalize()),
        })
    }
}

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
            self.deliver(&input).await?;
            // Context describes retained bindings; resolve still checks current
            // authority and storage before registering one in this Run.
            let inputs = self.inputs.clone();
            let input_ref = input.input_ref.clone();
            let resolved = tokio::task::spawn_blocking(move || inputs.resolve_input(&input_ref))
                .await
                .map_err(|_| "material admission interrupted")?
                .map_err(|_| "material admission rejected")?;
            if resolved != input {
                return Err("material admission binding mismatch".into());
            }
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
        let path = self.deliver(&input).await?;
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
        self.inputs
            .accept_authorized_resolution(confirmed.resolved_input)
            .map_err(|_| "material admission rejected")?;
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

    async fn deliver(&self, input: &ResolvedInput) -> Result<String, String> {
        input.validate()?;
        if input.size_bytes > MAX_EXECUTION_INPUT_BYTES {
            return Err("material exceeds execution input limit".into());
        }
        let declared = declared_input(input)?;
        let host = self.host.clone();
        let lookup = input.clone();
        let existing = tokio::task::spawn_blocking(move || host.execution_input_path(&lookup))
            .await
            .map_err(|_| "material input lookup interrupted")??;
        let path = if let Some(path) = existing {
            path
        } else {
            let frame = self.read_frame(input).await?;
            let host = self.host.clone();
            tokio::task::spawn_blocking(move || host.materialize_execution_input(frame))
                .await
                .map_err(|_| "material installation interrupted")??
        };
        self.inputs.admit_declared_inputs(&[declared])?;
        Ok(path)
    }

    async fn read_frame(
        &self,
        input: &ResolvedInput,
    ) -> Result<VerifiedMaterialInputFrame, String> {
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
        VerifiedMaterialInputFrame::read(input, &mut response).await
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
    use std::io::{Read, Seek, Write};

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
    fn original_download_produces_a_verified_frame_or_rejects_changed_content() {
        for (version, bytes, expected) in [
            ("1", b"zip".as_slice(), None),
            (
                "2",
                b"zip".as_slice(),
                Some("material original binding rejected"),
            ),
            (
                "1",
                b"bad".as_slice(),
                Some("material original identity mismatch"),
            ),
            (
                "1",
                b"zi".as_slice(),
                Some("material original identity mismatch"),
            ),
            (
                "1",
                b"zips".as_slice(),
                Some("material original size mismatch"),
            ),
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
            let result = runtime.block_on(reader.read_frame(&input()));
            if let Some(error) = expected {
                assert_eq!(result.unwrap_err(), error);
            } else {
                let (binding, descriptor, mut file, length, digest) = result.unwrap().into_parts();
                assert_eq!(binding, input());
                file.rewind().unwrap();
                let mut frame = Vec::new();
                file.read_to_end(&mut frame).unwrap();
                assert_eq!(length, frame.len() as u64);
                assert_eq!(digest, format!("sha256:{:x}", Sha256::digest(&frame)));
                let header_len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
                let decoded: SandboxMaterializedInput =
                    serde_json::from_slice(&frame[4..4 + header_len]).unwrap();
                assert_eq!(decoded, descriptor);
                assert_eq!(&frame[4 + header_len..], bytes);
            }
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
            runtime.block_on(reader.deliver(&original)).unwrap_err(),
            "material exceeds execution input limit"
        );
    }
}
