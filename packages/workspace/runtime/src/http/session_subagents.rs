//! Private read transport; Django authorizes the owning parent Session.
use super::*;
use crate::postgres_store::session_subagents::{
    SessionSubagentReadRequest, SessionSubagentsReadRequest,
};

pub(super) fn list(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let query = match serde_json::from_slice::<SessionSubagentsReadRequest>(request.body()) {
        Ok(query) if query.validate() => query,
        _ => return json_error_response(400, "session_subagents_request_invalid"),
    };
    response(state.job_store.read_session_subagents(&query))
}

pub(super) fn detail(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let query = match serde_json::from_slice::<SessionSubagentReadRequest>(request.body()) {
        Ok(query) if query.validate() => query,
        _ => return json_error_response(400, "session_subagent_request_invalid"),
    };
    response(state.job_store.read_session_subagent(&query))
}

fn response<T: serde::Serialize>(
    value: Result<Option<T>, String>,
) -> Result<Response<Body>, String> {
    match value {
        Ok(Some(value)) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )),
        Ok(None) => json_error_response(404, "session_subagent_not_found"),
        Err(error) => {
            eprintln!("Session subagent read failed: {error}");
            json_error_response(503, "session_subagent_unavailable")
        }
    }
}
