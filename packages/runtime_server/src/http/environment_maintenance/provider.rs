//! Same maintenance HTTP paths, with explicit native SDK provider contracts.
use super::*;
use crate::e2b_run_registry::E2bEnvironmentBinding;
use crate::e2b_runtime::{maintenance::DirectRequest, E2bRuntimeError};
use crate::environment_authorization::provider_maintenance::{
    ProviderMaintenanceAuthorization, SCHEMA,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Dispatch {
    authorization: ProviderMaintenanceAuthorization,
    request: DirectRequest,
}

pub(super) fn owns_task_request(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("authorization")?
                .get("payload")?
                .get("schema")?
                .as_str()
                .map(str::to_owned)
        })
        .as_deref()
        == Some(SCHEMA)
}
pub(super) fn owns_file_request(bytes: &[u8]) -> bool {
    let Some(prefix) = bytes.get(..4) else {
        return false;
    };
    let Ok(prefix) = <&[u8; 4]>::try_from(prefix) else {
        return false;
    };
    let size = u32::from_be_bytes(*prefix) as usize;
    size > 0 && size <= 64 * 1024 && bytes.get(4..4 + size).is_some_and(owns_task_request)
}
fn decode_task(bytes: &[u8]) -> Result<Dispatch, &'static str> {
    let dispatch: Dispatch =
        serde_json::from_slice(bytes).map_err(|_| "provider_maintenance_request_invalid")?;
    if !matches!(&dispatch.request, DirectRequest::Command(_)) {
        return Err("provider_maintenance_request_invalid");
    }
    dispatch.request.validate(&dispatch.authorization, &[])?;
    Ok(dispatch)
}
fn decode_file(bytes: &[u8]) -> Result<(Dispatch, &[u8]), &'static str> {
    if bytes.len() < 4 {
        return Err("provider_maintenance_frame_invalid");
    }
    let size = u32::from_be_bytes(
        bytes[..4]
            .try_into()
            .map_err(|_| "provider_maintenance_frame_invalid")?,
    ) as usize;
    if size == 0 || size > 64 * 1024 || bytes.len() < 4 + size {
        return Err("provider_maintenance_frame_invalid");
    }
    let dispatch: Dispatch = serde_json::from_slice(&bytes[4..4 + size])
        .map_err(|_| "provider_maintenance_frame_invalid")?;
    if !matches!(&dispatch.request, DirectRequest::Filesystem(_)) {
        return Err("provider_maintenance_frame_invalid");
    }
    let raw = &bytes[4 + size..];
    dispatch.request.validate(&dispatch.authorization, raw)?;
    Ok((dispatch, raw))
}
pub(super) fn task(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let dispatch = match decode_task(request.body()) {
        Ok(value) => value,
        Err(code) => return json_error_response(400, code),
    };
    run(state, dispatch, &[])
}
pub(super) fn filesystem(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let (dispatch, raw) = match decode_file(request.body()) {
        Ok(value) => value,
        Err(code) => return json_error_response(400, code),
    };
    run(state, dispatch, raw)
}
fn run(state: HttpServerState, dispatch: Dispatch, raw: &[u8]) -> Result<Response<Body>, String> {
    let key = match env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY") {
        Ok(value) if !value.is_empty() => value,
        _ => return json_error_response(503, "maintenance_signing_key_unavailable"),
    };
    let Some(runtime) = state.e2b_runtime.as_ref() else {
        return json_error_response(503, "provider_maintenance_not_configured");
    };
    let binding = E2bEnvironmentBinding {
        environment_id: dispatch.authorization.payload.environment_id.clone(),
        profile_digest: dispatch.authorization.payload.profile_digest.clone(),
        provider_config_digest: dispatch
            .authorization
            .payload
            .provider_target
            .provider_config_digest
            .clone(),
    };
    let slot = match state.e2b_runs.existing_environment(&binding) {
        Ok(slot) => slot,
        Err(_) => return json_error_response(409, "provider_maintenance_binding_rejected"),
    };
    let _guard = match slot.as_ref().map(|slot| slot.try_enter()).transpose() {
        Ok(Some(Some(guard))) => Some(guard),
        Ok(None) => None,
        _ => return json_error_response(409, "provider_maintenance_environment_busy"),
    };
    match runtime.maintenance_direct(
        &dispatch.authorization,
        key.as_bytes(),
        &dispatch.request,
        raw,
    ) {
        Ok(completion) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&completion).map_err(|_| "provider_maintenance_response_invalid")?,
        )),
        Err(E2bRuntimeError::Rejected(code)) => json_error_response(409, code),
        Err(E2bRuntimeError::Unavailable(code) | E2bRuntimeError::Unknown(code)) => {
            json_error_response(503, code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2b_runtime::maintenance::{DirectCommand, NativeFileFrame, NativeFileOperation};
    fn auth(request: &DirectRequest) -> ProviderMaintenanceAuthorization {
        let mut auth:ProviderMaintenanceAuthorization=serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json"))).unwrap();
        auth.payload.request_digest = format!(
            "sha256:{:x}",
            sha2::Sha256::digest(
                crate::agent_run_authorization::canonical_json(
                    serde_json::to_value(request).unwrap()
                )
                .unwrap()
                .as_bytes()
            )
        );
        auth
    }
    #[test]
    fn provider_direct_command_route_uses_strict_schema_without_resident_task_envelopes() {
        let request = DirectRequest::Command(DirectCommand {
            program: "/bin/bash".into(),
            args: vec!["-lc".into(), "verifier".into()],
            cwd: "/app".into(),
            timeout_ms: 900000,
        });
        let mut value = json!({"authorization":auth(&request),"request":request});
        let wire = serde_json::to_vec(&value).unwrap();
        assert!(owns_task_request(&wire));
        assert!(decode_task(&wire).is_ok());
        value["task"] = json!({"schema":"environment.task.v1"});
        assert!(decode_task(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn provider_native_file_route_binds_exact_raw_bytes_without_core_atomic_fields() {
        let raw = b"\0\xfftask file";
        let request = DirectRequest::Filesystem(NativeFileFrame {
            request: NativeFileOperation::WriteFile {
                path: "/tests/file".into(),
            },
            content_length: raw.len() as u64,
            content_digest: format!("sha256:{:x}", sha2::Sha256::digest(raw)),
        });
        let header =
            serde_json::to_vec(&json!({"authorization":auth(&request),"request":request})).unwrap();
        let mut wire = (header.len() as u32).to_be_bytes().to_vec();
        wire.extend(header);
        wire.extend(raw);
        assert!(owns_file_request(&wire));
        assert_eq!(decode_file(&wire).unwrap().1, raw);
        *wire.last_mut().unwrap() = 0;
        assert!(decode_file(&wire).is_err());
    }
}
