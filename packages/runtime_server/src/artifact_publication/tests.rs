use super::*;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::sync::Mutex;

#[derive(Default)]
struct PublicationFacts {
    statuses: Vec<Value>,
    uploads: Vec<(Value, Vec<u8>)>,
    receipt: Option<Value>,
}

struct ApiFixture {
    url: String,
    facts: Arc<Mutex<PublicationFacts>>,
    server: tokio::task::JoinHandle<()>,
    _runtime: tokio::runtime::Runtime,
}

impl Drop for ApiFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn publication_status(
    State(facts): State<Arc<Mutex<PublicationFacts>>>,
    body: Bytes,
) -> axum::response::Response {
    let query: Value = serde_json::from_slice(&body).unwrap();
    let mut facts = facts.lock().unwrap();
    facts.statuses.push(query);
    match facts.receipt.clone() {
        Some(receipt) => json_response(receipt),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

async fn publish_bytes(
    State(facts): State<Arc<Mutex<PublicationFacts>>>,
    body: Bytes,
) -> axum::response::Response {
    let header_size = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
    let metadata: Value = serde_json::from_slice(&body[4..4 + header_size]).unwrap();
    let data = body[4 + header_size..].to_vec();
    let receipt = json!({
        "schema": API_RESULT_SCHEMA,
        "publicationId": metadata["publicationId"],
        "artifactRef": "artifact:published-file",
        "filename": metadata["filename"],
        "contentType": "application/octet-stream",
        "sizeBytes": metadata["sizeBytes"],
        "sha256": metadata["sha256"],
    });
    let mut facts = facts.lock().unwrap();
    facts.uploads.push((metadata, data));
    facts.receipt = Some(receipt.clone());
    json_response(receipt)
}

fn json_response(value: Value) -> axum::response::Response {
    (
        [("Content-Type", "application/json")],
        serde_json::to_string(&value).unwrap(),
    )
        .into_response()
}

fn api_fixture() -> ApiFixture {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let address = listener.local_addr().unwrap();
    let facts = Arc::new(Mutex::new(PublicationFacts::default()));
    let router = Router::new()
        .route("/internal/artifacts/status", post(publication_status))
        .route("/internal/artifacts/publish", post(publish_bytes))
        .with_state(facts.clone());
    let server = runtime.spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    ApiFixture {
        url: format!("http://{address}"),
        facts,
        server,
        _runtime: runtime,
    }
}

struct ArtifactReader {
    metadata: SandboxArtifactMetadata,
    bytes: Vec<u8>,
    paths: Mutex<Vec<String>>,
    read_error: Option<&'static str>,
}

impl ArtifactReader {
    fn new(bytes: &[u8]) -> Self {
        Self {
            metadata: SandboxArtifactMetadata {
                filename: "result.bin".into(),
                size_bytes: bytes.len() as u64,
                sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
            },
            bytes: bytes.to_vec(),
            paths: Mutex::new(Vec::new()),
            read_error: None,
        }
    }
}

impl ArtifactReadPort for ArtifactReader {
    fn read_artifact(&self, path: &str) -> Result<(SandboxArtifactMetadata, Vec<u8>), String> {
        self.paths.lock().unwrap().push(path.into());
        if let Some(error) = self.read_error {
            return Err(error.into());
        }
        Ok((self.metadata.clone(), self.bytes.clone()))
    }
}

fn publisher(reader: Arc<ArtifactReader>, api: &ApiFixture) -> WorkspaceArtifactPublicationPort {
    let mut port = WorkspaceArtifactPublicationPort::new(
        reader,
        api.url.clone(),
        "fixture-token".into(),
        "run-fixture".into(),
        "authorization-fixture".into(),
    )
    .unwrap();
    // The fixture must stay local even on developer machines with HTTP proxies.
    port.client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    port
}

fn artifact_request() -> ArtifactPublicationRequest {
    ArtifactPublicationRequest {
        tool_call_id: "tool-call".into(),
        path: "/mnt/data/result.bin".into(),
    }
}

fn api_result(publication_id: &str) -> ApiPublicationResult {
    ApiPublicationResult {
        schema: API_RESULT_SCHEMA.into(),
        publication_id: publication_id.into(),
        artifact_ref: "artifact:published-file".into(),
        filename: "result.bin".into(),
        content_type: "application/octet-stream".into(),
        size_bytes: 5,
        sha256: "sha256:expected".into(),
    }
}

#[test]
fn publication_receipt_preserves_exact_api_identity_and_uploaded_metadata() {
    let metadata = ApiPublicationMetadata {
        schema: API_METADATA_SCHEMA,
        publication_id: "publication",
        agent_run_id: "run",
        authorization_digest: "authorization",
        tool_call_id: "call",
        filename: "result.bin",
        size_bytes: 5,
        sha256: "sha256:expected",
    };
    let receipt =
        receipt_from_api(api_result("publication"), "publication", Some(&metadata)).unwrap();
    assert_eq!(receipt.publication_id, "publication");
    assert_eq!(receipt.artifact_ref, "artifact:published-file");
    assert_eq!(receipt.filename, "result.bin");
    assert_eq!(receipt.size_bytes, 5);
    assert_eq!(receipt.sha256, "sha256:expected");

    for mismatch in ["identity", "filename", "length", "digest", "artifactRef"] {
        let mut result = api_result("publication");
        match mismatch {
            "identity" => result.publication_id = "another-publication".into(),
            "filename" => result.filename = "another.bin".into(),
            "length" => result.size_bytes += 1,
            "digest" => result.sha256 = "sha256:different".into(),
            "artifactRef" => result.artifact_ref = "file:foreign".into(),
            _ => unreachable!(),
        }
        assert!(
            receipt_from_api(result, "publication", Some(&metadata)).is_err(),
            "{mismatch}"
        );
    }
}

#[test]
fn publication_identity_replays_within_the_original_run_and_tool_call() {
    let first = stable_artifact_publication_id("run-a", "call-a");
    assert_eq!(first, stable_artifact_publication_id("run-a", "call-a"));
    assert_ne!(first, stable_artifact_publication_id("run-b", "call-a"));
    assert_ne!(first, stable_artifact_publication_id("run-a", "call-b"));
    assert!(receipt_from_api(api_result(&first), &first, None).is_ok());
}

#[test]
fn backend_neutral_reader_publishes_exact_binary_and_replays_without_reading_again() {
    let api = api_fixture();
    let bytes = b"\x00\xffabc";
    let reader = Arc::new(ArtifactReader::new(bytes));
    let port = publisher(reader.clone(), &api);
    let first = port.publish_artifact(artifact_request()).unwrap();
    let replay = port.publish_artifact(artifact_request()).unwrap();
    assert_eq!(first, replay);
    assert_eq!(
        reader.paths.lock().unwrap().as_slice(),
        ["/mnt/data/result.bin"]
    );
    let facts = api.facts.lock().unwrap();
    assert_eq!(facts.uploads.len(), 1);
    let (metadata, actual) = &facts.uploads[0];
    assert_eq!(actual, bytes);
    assert_eq!(metadata["schema"], API_METADATA_SCHEMA);
    assert_eq!(metadata["agentRunId"], "run-fixture");
    assert_eq!(metadata["authorizationDigest"], "authorization-fixture");
    assert_eq!(metadata["toolCallId"], "tool-call");
    assert_eq!(metadata["sizeBytes"], bytes.len());
    assert_eq!(
        metadata["sha256"],
        format!("sha256:{:x}", Sha256::digest(actual))
    );
    assert_eq!(first.sha256, reader.metadata.sha256);
    assert_eq!(facts.statuses.len(), 3);
}

#[test]
fn artifact_reader_failure_cannot_upload_or_manufacture_a_receipt() {
    let api = api_fixture();
    let mut reader = ArtifactReader::new(b"bytes");
    reader.read_error = Some("artifact_unavailable");
    let port = publisher(Arc::new(reader), &api);
    assert_eq!(
        port.publish_artifact(artifact_request()),
        Err("artifact_unavailable".into())
    );
    assert!(api.facts.lock().unwrap().uploads.is_empty());
}

#[test]
fn corrupt_reader_length_or_digest_is_rejected_before_publication() {
    for mismatch in ["length", "digest"] {
        let api = api_fixture();
        let mut reader = ArtifactReader::new(b"bytes");
        match mismatch {
            "length" => reader.metadata.size_bytes += 1,
            "digest" => reader.metadata.sha256 = "sha256:wrong".into(),
            _ => unreachable!(),
        }
        let reader = Arc::new(reader);
        let port = publisher(reader.clone(), &api);
        let result = port.publish_artifact(artifact_request());
        assert_eq!(reader.paths.lock().unwrap().len(), 1, "{mismatch}");
        assert!(!api.facts.lock().unwrap().statuses.is_empty(), "{mismatch}");
        assert!(result.is_err(), "{mismatch}");
        assert!(api.facts.lock().unwrap().uploads.is_empty(), "{mismatch}");
    }
}
