//! Local ownership of native typed-model dispatch and immutable attempt evidence.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;

use centaeris_core::model::typed::{TypedModelRequest, TypedModelResponse, TypedQuestion};
use centaeris_core::model::{JsonHttpResponse, JsonHttpTransport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempPath;

use crate::errors::RuntimeHostError;
use crate::http_transport::{build_model_http_client, ReqwestJsonHttpTransport};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EvaluateTypedRequest {
    operation_id: String,
    session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    state: Value,
    questions: BTreeMap<String, TypedQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TypedAttemptRequest {
    schema_version: u32,
    operation_id: String,
    session_id: String,
    attempt_id: String,
    request_id: String,
    endpoint: String,
    request: TypedModelRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TypedModelReceipt {
    operation_id: String,
    session_id: String,
    attempt_id: String,
    request_id: String,
    upstream_request_id: Option<String>,
    response_id: Option<String>,
    record_path: String,
    request: TypedModelRequest,
    response: TypedModelResponse,
    usage: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReadTypedOperationRequest {
    operation_id: String,
    session_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TypedRawResponse {
    status_code: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TypedOperation {
    operation_id: String,
    session_id: String,
    attempt_id: String,
    request_id: String,
    record_path: String,
    request: TypedModelRequest,
    raw_response: Option<TypedRawResponse>,
    failure: Option<Value>,
    receipt: Option<TypedModelReceipt>,
}
struct TypedModelConfig {
    url: String,
    key: String,
    model: String,
}

impl TypedModelConfig {
    fn from_env() -> Result<Self, RuntimeHostError> {
        let required = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    RuntimeHostError::new(
                        "typed_model_not_configured",
                        format!("{name} is required for the dedicated typed-model process"),
                    )
                })
        };
        Ok(Self {
            url: required("CENTAERIS_TYPED_MODEL_URL")?,
            key: std::env::var("CENTAERIS_TYPED_MODEL_KEY").unwrap_or_default(),
            model: required("CENTAERIS_TYPED_MODEL_MODEL")?,
        })
    }
}

pub(crate) fn parse_request(payload: Value) -> Result<EvaluateTypedRequest, RuntimeHostError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Envelope {
        request: EvaluateTypedRequest,
    }
    serde_json::from_value::<Envelope>(payload)
        .map(|envelope| envelope.request)
        .map_err(|error| {
            RuntimeHostError::invalid_request(format!("invalid typed-model request: {error}"))
        })
}
pub(crate) fn parse_read_request(
    payload: Value,
) -> Result<ReadTypedOperationRequest, RuntimeHostError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Envelope {
        request: ReadTypedOperationRequest,
    }
    serde_json::from_value::<Envelope>(payload)
        .map(|envelope| envelope.request)
        .map_err(|error| {
            RuntimeHostError::invalid_request(format!("invalid typed-operation request: {error}"))
        })
}

pub(crate) fn read_operation(
    request: ReadTypedOperationRequest,
) -> Result<Option<TypedOperation>, RuntimeHostError> {
    read_operation_at(&request, &crate::user_data_layout::profile_root())
}

fn operation_directory(
    profile_root: &Path,
    session_id: &str,
    operation_id: &str,
) -> Result<std::path::PathBuf, RuntimeHostError> {
    if session_id.trim().is_empty() || operation_id.trim().is_empty() {
        return Err(RuntimeHostError::invalid_request(
            "operationId and sessionId are required",
        ));
    }
    let identity = serde_json::to_vec(&(session_id, operation_id))
        .map_err(|e| failure("typed_model_record_failed", e.to_string()))?;
    Ok(profile_root
        .join("runtime")
        .join("typed-model")
        .join(format!("{:x}", Sha256::digest(identity))))
}

fn read_operation_at(
    request: &ReadTypedOperationRequest,
    profile_root: &Path,
) -> Result<Option<TypedOperation>, RuntimeHostError> {
    let directory = operation_directory(profile_root, &request.session_id, &request.operation_id)?;
    let Some(stored) =
        read_optional_record::<TypedAttemptRequest>(&directory.join("request.json"))?
    else {
        return Ok(None);
    };
    if stored.schema_version != 1
        || stored.operation_id != request.operation_id
        || stored.session_id != request.session_id
    {
        return Err(failure(
            "typed_model_record_failed",
            "typed-operation record identity or schema does not match",
        ));
    }
    let receipt = read_receipt(&directory)?;
    let failure = read_optional_record(&directory.join("failure.json"))?;
    let raw_response = read_optional_record(&directory.join("raw-response.json"))?;
    Ok(Some(TypedOperation {
        operation_id: stored.operation_id,
        session_id: stored.session_id,
        attempt_id: stored.attempt_id,
        request_id: stored.request_id,
        record_path: directory.to_string_lossy().into_owned(),
        request: stored.request,
        raw_response,
        failure,
        receipt,
    }))
}

fn read_optional_record<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, RuntimeHostError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failure("typed_model_record_failed", error.to_string())),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        failure(
            "typed_model_record_failed",
            format!("{}: {error}", path.display()),
        )
    })
}
fn read_receipt(directory: &Path) -> Result<Option<TypedModelReceipt>, RuntimeHostError> {
    let receipt = read_optional_record(&directory.join("receipt.json"))?;
    #[cfg(not(windows))]
    if receipt.is_some() {
        // A reader can open the atomically published name before the writer has synced it.
        sync_operation_directory(directory)?;
    }
    Ok(receipt)
}

pub(crate) async fn evaluate(
    request: EvaluateTypedRequest,
) -> Result<TypedModelReceipt, RuntimeHostError> {
    let config = TypedModelConfig::from_env()?;
    let root = crate::user_data_layout::profile_root();
    let transport = ReqwestJsonHttpTransport::for_session(
        &request.session_id,
        "typed-model",
        build_model_http_client,
    )
    .map_err(|e| RuntimeHostError::new("typed_model_transport_failed", e))?;
    evaluate_with(request, &config, &root, &transport).await
}

fn failure(code: &str, message: impl Into<String>) -> RuntimeHostError {
    RuntimeHostError::new(code, message.into())
}

async fn evaluate_with(
    input: EvaluateTypedRequest,
    config: &TypedModelConfig,
    profile_root: &Path,
    transport: &dyn JsonHttpTransport,
) -> Result<TypedModelReceipt, RuntimeHostError> {
    if input.operation_id.trim().is_empty() || input.session_id.trim().is_empty() {
        return Err(RuntimeHostError::invalid_request(
            "operationId and sessionId are required",
        ));
    }
    if input
        .model
        .as_ref()
        .is_some_and(|model| model != &config.model)
    {
        return Err(RuntimeHostError::invalid_request(
            "model must match the dedicated typed-model configuration",
        ));
    }
    let request = TypedModelRequest {
        model: config.model.clone(),
        state: input.state,
        questions: input.questions,
    };
    request
        .validate()
        .map_err(|e| RuntimeHostError::invalid_request(e.to_string()))?;
    let directory = operation_directory(profile_root, &input.session_id, &input.operation_id)?;
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random)
        .map_err(|e| failure("typed_model_record_failed", e.to_string()))?;
    let attempt_id = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let attempt = TypedAttemptRequest {
        schema_version: 1,
        operation_id: input.operation_id,
        session_id: input.session_id,
        request_id: format!("cet-typed-{attempt_id}"),
        attempt_id,
        endpoint: config.url.clone(),
        request,
    };
    let http = attempt
        .request
        .http_request(&config.url, &config.key, &attempt.request_id)
        .map_err(|e| RuntimeHostError::invalid_request(e.to_string()))?;
    fs::create_dir_all(&directory)
        .map_err(|e| failure("typed_model_record_failed", e.to_string()))?;
    if !create_record(&directory.join("request.json"), &attempt)? {
        let previous: TypedAttemptRequest = read_record(&directory.join("request.json"))?;
        if previous.schema_version != 1
            || previous.operation_id != attempt.operation_id
            || previous.session_id != attempt.session_id
            || previous.request != attempt.request
            || previous.endpoint != attempt.endpoint
        {
            return Err(failure(
                "typed_model_operation_conflict",
                "operationId was already used with different typed-model input",
            ));
        }
        if let Some(receipt) = read_receipt(&directory)? {
            return Ok(receipt);
        }
        return Err(failure("typed_model_operation_indeterminate", format!("typed-model dispatch has no completed receipt; inspect {} before issuing any new operation", directory.display())));
    }
    #[cfg(not(windows))]
    for parent in directory.ancestors().skip(1) {
        sync_operation_directory(parent)?;
        if parent == profile_root {
            break;
        }
    }
    let response = match transport.execute_json(&http).await {
        Ok(response) => response,
        Err(error) => {
            create_record(
                &directory.join("failure.json"),
                &json!({"stage":"transport","message":error.to_string()}),
            )?;
            return Err(failure(
                "typed_model_transport_failed",
                format!("{error}; record: {}", directory.display()),
            ));
        }
    };
    let upstream_request_id =
        header(&response, "x-request-id").or_else(|| header(&response, "request-id"));
    create_record(
        &directory.join("raw-response.json"),
        &json!({
            "statusCode":response.status_code,
            "headers":response.headers,
            "body":response.body_json,
        }),
    )?;
    if !(200..300).contains(&response.status_code) {
        create_record(
            &directory.join("failure.json"),
            &json!({"stage":"http","statusCode":response.status_code}),
        )?;
        return Err(failure(
            "typed_model_http_failed",
            format!(
                "typed-model HTTP {}; record: {}",
                response.status_code,
                directory.display()
            ),
        ));
    }
    let parsed = match attempt.request.decode_response(&response.body_json) {
        Ok(parsed) => parsed,
        Err(error) => {
            create_record(
                &directory.join("failure.json"),
                &json!({"stage":"validation","message":error.to_string()}),
            )?;
            return Err(failure(
                "typed_model_invalid_response",
                format!("{error}; record: {}", directory.display()),
            ));
        }
    };
    let receipt = TypedModelReceipt {
        operation_id: attempt.operation_id,
        session_id: attempt.session_id,
        attempt_id: attempt.attempt_id,
        request_id: attempt.request_id,
        upstream_request_id,
        response_id: parsed.id.clone(),
        record_path: directory.to_string_lossy().into_owned(),
        request: attempt.request,
        usage: parsed.usage.clone(),
        response: parsed,
    };
    create_record(&directory.join("receipt.json"), &receipt)?;
    read_record(&directory.join("receipt.json"))
}

fn header(response: &JsonHttpResponse, name: &str) -> Option<String> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn create_record(path: &Path, value: &impl Serialize) -> Result<bool, RuntimeHostError> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("typed_model_record_failed", "record path has no parent"))?;
    let record_error = |error: io::Error| {
        failure(
            "typed_model_record_failed",
            format!("{}: {error}", path.display()),
        )
    };
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| failure("typed_model_record_failed", error.to_string()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(record_error)?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(record_error)?;
    match persist_new_record(temporary.into_temp_path(), path) {
        Ok(()) => {
            #[cfg(not(windows))]
            sync_operation_directory(parent)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(record_error(error)),
    }
}

#[cfg(not(windows))]
fn persist_new_record(temporary: TempPath, path: &Path) -> io::Result<()> {
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
}

#[cfg(windows)]
fn persist_new_record(mut temporary: TempPath, path: &Path) -> io::Result<()> {
    crate::atomic_file::move_new_file_write_through(&temporary, path)?;
    temporary.disable_cleanup(true);
    Ok(())
}

#[cfg(not(windows))]
fn sync_operation_directory(path: &Path) -> Result<(), RuntimeHostError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            failure(
                "typed_model_record_failed",
                format!("sync {}: {error}", path.display()),
            )
        })
}

fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, RuntimeHostError> {
    let bytes = fs::read(path).map_err(|e| failure("typed_model_record_failed", e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| {
        failure(
            "typed_model_record_failed",
            format!("{}: {e}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests;
