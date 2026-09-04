use crate::errors::RuntimeHostError;
use crate::host_protocol;
use crate::processes;
use crate::runtime_rpc_transport::EventWriter;
use crate::runtime_server::RuntimeClientKind;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::time::Duration;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InitializeResponse {
    pub(crate) status: &'static str,
    pub(crate) runtime: &'static str,
    pub(crate) protocol: &'static str,
    pub(crate) protocol_version: u32,
    pub(crate) capabilities: &'static [&'static str],
    pub(crate) events: &'static [&'static str],
    pub(crate) projections: &'static [&'static str],
    pub(crate) build_id: String,
    pub(crate) core_protocol_version: &'static str,
    pub(crate) profile_id: String,
    pub(crate) store_id: String,
    pub(crate) store_schema_version: i64,
    pub(crate) layout_schema_version: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InitializeRequest {
    client_kind: RuntimeClientKind,
    viewer_id: String,
}

pub(crate) fn initialize(
    event_writer: &EventWriter,
    request: InitializeRequest,
) -> Result<InitializeResponse, RuntimeHostError> {
    let response = initialize_response()?;
    event_writer.register_client(request.client_kind, request.viewer_id.as_str())?;
    Ok(response)
}

fn initialize_response() -> Result<InitializeResponse, RuntimeHostError> {
    let build_id = current_executable_build_id()?;
    let profile_id = crate::user_data_layout::profile_identity()
        .map_err(|error| RuntimeHostError::new("runtime_profile_identity_failed", error))?;
    let store_id = crate::user_data_layout::runtime_store_identity()
        .map_err(|error| RuntimeHostError::new("runtime_store_identity_failed", error))?;
    Ok(initialize_response_with_identities(
        build_id, profile_id, store_id,
    ))
}

fn initialize_response_with_identities(
    build_id: String,
    profile_id: String,
    store_id: String,
) -> InitializeResponse {
    InitializeResponse {
        status: "ok",
        runtime: "centaeris-runtime",
        protocol: host_protocol::CENTAERIS_RUNTIME_PROTOCOL_NAME,
        protocol_version: host_protocol::CENTAERIS_RUNTIME_PROTOCOL_VERSION,
        capabilities: host_protocol::CENTAERIS_RUNTIME_PROTOCOL_CAPABILITIES,
        events: host_protocol::CENTAERIS_RUNTIME_PROTOCOL_EVENTS,
        projections: host_protocol::CENTAERIS_RUNTIME_PROTOCOL_PROJECTIONS,
        build_id,
        core_protocol_version: centaeris_core::runtime::CORE_PROTOCOL_VERSION,
        profile_id,
        store_id,
        store_schema_version: crate::sqlite_store::STORE_SCHEMA_VERSION,
        layout_schema_version: crate::user_data_layout::LAYOUT_SCHEMA_VERSION,
    }
}

fn current_executable_build_id() -> Result<String, RuntimeHostError> {
    let path = std::env::current_exe().map_err(|error| {
        RuntimeHostError::new(
            "runtime_build_identity_failed",
            format!("resolve current executable failed: {error}"),
        )
    })?;
    let file = File::open(path.as_path()).map_err(|error| {
        RuntimeHostError::new(
            "runtime_build_identity_failed",
            format!("open current executable failed {}: {error}", path.display()),
        )
    })?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            RuntimeHostError::new(
                "runtime_build_identity_failed",
                format!("read current executable failed {}: {error}", path.display()),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProcessCaptureRequest {
    pub(crate) program: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    pub(crate) timeout_ms: u64,
    #[serde(default = "default_process_capture_output_chars")]
    pub(crate) max_output_chars: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProcessCaptureResponse {
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
}

fn default_process_capture_output_chars() -> usize {
    1600
}

pub(crate) fn process_capture(
    request: ProcessCaptureRequest,
) -> Result<ProcessCaptureResponse, RuntimeHostError> {
    if request.program.trim().is_empty() {
        return Err(RuntimeHostError::invalid_request("program is required"));
    }
    if request.timeout_ms == 0 {
        return Err(RuntimeHostError::invalid_request(
            "timeoutMs must be positive",
        ));
    }
    if request.max_output_chars == 0 {
        return Err(RuntimeHostError::invalid_request(
            "maxOutputChars must be positive",
        ));
    }

    let args = request.args.iter().map(String::as_str).collect::<Vec<_>>();
    let capture = processes::run_command_capture(
        request.program.as_str(),
        args.as_slice(),
        Duration::from_millis(request.timeout_ms),
    )
    .map_err(|error| RuntimeHostError::new("process_capture_failed", error))?;

    Ok(ProcessCaptureResponse {
        exit_code: capture.exit_code,
        stdout: processes::command_capture_tail(capture.stdout.as_str(), request.max_output_chars),
        stderr: processes::command_capture_tail(capture.stderr.as_str(), request.max_output_chars),
        timed_out: capture.timed_out,
    })
}

pub(crate) fn deserialize_request<TRequest>(
    payload: serde_json::Value,
) -> Result<TRequest, RuntimeHostError>
where
    TRequest: DeserializeOwned,
{
    let request = payload
        .get("request")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    serde_json::from_value(request).map_err(|error| {
        RuntimeHostError::invalid_request(format!("invalid command request: {error}"))
    })
}

pub(crate) fn deserialize_optional_request<TRequest>(
    payload: serde_json::Value,
) -> Result<Option<TRequest>, RuntimeHostError>
where
    TRequest: DeserializeOwned,
{
    if payload.get("request").is_none() {
        return Ok(None);
    }
    deserialize_request(payload).map(Some)
}

#[cfg(test)]
mod tests {
    use super::{deserialize_request, initialize_response_with_identities, InitializeRequest};
    use serde_json::json;

    #[test]
    fn initialize_exposes_centaeris_runtime_protocol_v1_descriptor() {
        let response = initialize_response_with_identities(
            format!("sha256:{}", "a".repeat(64)),
            "profile-identity".to_string(),
            "store-identity".to_string(),
        );
        assert_eq!(response.status, "ok");
        assert_eq!(response.runtime, "centaeris-runtime");
        assert_eq!(response.protocol, "centaeris.runtime");
        assert_eq!(response.protocol_version, 1);
        assert!(response.projections.contains(&"session_event"));
        assert_eq!(response.build_id, format!("sha256:{}", "a".repeat(64)));
        assert_eq!(
            response.core_protocol_version,
            centaeris_core::runtime::CORE_PROTOCOL_VERSION
        );
        assert_eq!(response.profile_id, "profile-identity");
        assert_eq!(response.store_id, "store-identity");
        assert_eq!(response.store_schema_version, 1);
        assert_eq!(response.layout_schema_version, 1);
    }

    #[test]
    fn initialize_request_rejects_unknown_client_identity() {
        let request = deserialize_request::<InitializeRequest>(json!({
            "request": {"clientKind": "tui", "viewerId": "tui-test"}
        }))
        .expect("valid initialize request");
        assert_eq!(request.viewer_id, "tui-test");
        assert!(deserialize_request::<InitializeRequest>(json!({
            "request": {"clientKind": "banana", "viewerId": "tui-test"}
        }))
        .is_err());
        assert!(deserialize_request::<InitializeRequest>(json!({
            "request": {"clientKind": "tui", "viewerId": "tui-test", "extra": true}
        }))
        .is_err());
    }
}
