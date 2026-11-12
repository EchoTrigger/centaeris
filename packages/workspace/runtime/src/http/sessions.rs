//! Django authorizes permanent deletion; Runtime acknowledges the settled tree.
use super::*;

pub(super) fn purge(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let query = match serde_json::from_slice::<
        crate::postgres_store::session_deletion::SessionPurgeRequest,
    >(request.body())
    {
        Ok(query) if query.validate().is_ok() => query,
        _ => return json_error_response(400, "session_purge_request_invalid"),
    };
    let retirements = match state.job_store.preflight_interactive_session_purge(&query) {
        Ok(retirements) => retirements,
        Err(error) => {
            eprintln!("Session purge preparation unconfirmed: {error}");
            return json_error_response(409, "session_purge_unconfirmed");
        }
    };
    for retirement in retirements {
        // The command can retire only an incarnation derived from its exact
        // owned Session. The existing retirement transaction pins its epoch.
        let host = crate::docker_execution_host::resident_host_context().map(|(id, _)| id);
        if host.as_ref() != Ok(&retirement.host_id) {
            return json_error_response(503, "session_purge_host_unavailable");
        }
        if let Err(error) = state
            .job_store
            .retire_retained_instance(&retirement, super::environment_retirement::retire_physical)
        {
            eprintln!("Session environment retirement unconfirmed: {error}");
            return json_error_response(409, "session_purge_unconfirmed");
        }
    }
    let sessions = match state.job_store.purge_interactive_session(&query) {
        Ok(sessions) => sessions,
        Err(error) => {
            eprintln!("Session purge unconfirmed: {error}");
            return json_error_response(409, "session_purge_unconfirmed");
        }
    };
    let mut contexts = state
        .tool_layers
        .lock()
        .map_err(|_| "Session purge runtime context lock poisoned".to_string())?;
    for session in sessions {
        contexts.remove(&session);
    }
    Ok(http_response(
        200,
        "application/json",
        serde_json::to_vec(&json!({
            "schema": "runtime.session.purged.v1",
            "sessionId": query.session_id,
            "workspaceId": query.workspace_id,
            "purged": true,
        }))
        .map_err(|error| error.to_string())?,
    ))
}
