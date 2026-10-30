use super::*;
use crate::environment_authorization::provision::{ProvisionAuthorization, ProvisionRequest};
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestBody {
    authorization: ProvisionAuthorization,
    request: ProvisionRequest,
}
pub(super) fn provision(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if let Err((status, code)) = super::environment_maintenance::body_authority(&request) {
        return json_error_response(status, code);
    }
    let body: RequestBody = match serde_json::from_slice(request.body()) {
        Ok(body) => body,
        Err(_) => return json_error_response(400, "provision_request_invalid"),
    };
    if body
        .authorization
        .payload
        .validate_request(&body.request)
        .is_err()
    {
        return json_error_response(400, "provision_request_binding_rejected");
    }
    let key = match env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => return json_error_response(503, "provision_signing_key_unavailable"),
    };
    let (host, limits) = match crate::docker_execution_host::resident_host_context() {
        Ok(value) => value,
        Err(_) => return json_error_response(503, "provision_host_unavailable"),
    };
    match state.job_store.provision_environment(&body.authorization,key.as_bytes(),&host,limits,crate::resident_capacity::FilteredResidentInventory(crate::docker_execution_host::observed_resident_sandboxes_excluding),|allocation|crate::docker_execution_host::provision_environment(&body.authorization.payload,allocation)){
        Ok(allocation)=>Ok(http_response(200,"application/json",serde_json::to_vec(&json!({"schema":"runtime.environment.provision.result.v1","operationId":body.authorization.payload.operation_id,"environmentId":allocation.environment_id,"instanceId":allocation.instance_id,"containerId":allocation.container_id,"profileDigest":allocation.profile_digest,"status":"resident"})).map_err(|_|"provision_response_invalid")?)),
        Err(_)=>json_error_response(409,"provision_outcome_unconfirmed"),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Observe {
    schema: String,
    session_id: String,
    environment_id: String,
}
pub(super) fn observe(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if let Err((status, code)) = super::environment_maintenance::body_authority(&request) {
        return json_error_response(status, code);
    }
    let body: Observe = match serde_json::from_slice(request.body()) {
        Ok(body) => body,
        Err(_) => return json_error_response(400, "environment_observation_invalid"),
    };
    if body.schema != "runtime.environment.observe.v1"
        || [&body.session_id, &body.environment_id]
            .iter()
            .any(|v| v.is_empty() || v.len() > 160 || v.chars().any(char::is_control))
    {
        return json_error_response(400, "environment_observation_invalid");
    }
    let observed=state.job_store.with_environment_observation(&body.session_id,&body.environment_id,|target|{
        let physical_state=crate::docker_execution_host::observe_environment(target);
        let a=target.allocation.as_ref();
        Ok(json!({"schema":"runtime.environment.observe.result.v1","sessionId":target.session_id,"environmentId":target.environment_id,"profile":target.profile,"profileDigest":target.profile_digest,"state":physical_state,"controllerEpoch":target.controller_epoch,"controllerOwner":target.controller_owner,"instanceId":a.map(|a|&a.instance_id),"containerId":a.and_then(|a|a.container_id.as_ref()),"hostId":a.map(|a|&a.host_id),"resourceHolding":u8::from(target.resource_holding)}))
    });
    match observed {
        Ok(value) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&value).map_err(|_| "environment_observation_invalid")?,
        )),
        Err(_) => json_error_response(409, "environment_observation_rejected"),
    }
}
