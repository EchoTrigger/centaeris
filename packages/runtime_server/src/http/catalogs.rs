use super::*;

pub(super) fn host_event_input(
    _state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let input = match serde_json::from_slice::<HostEventInputRequest>(&body)
        .map_err(|error| error.to_string())
        .and_then(HostEventInputRequest::create)
    {
        Ok(input) => input,
        Err(_) => return json_error_response(400, "host_event_input_invalid"),
    };
    Ok(http_response(
        200,
        "application/json",
        serde_json::to_vec(&json!({
            "schema":"runtime.host_event_input.created.v1", "input":input,
        }))
        .map_err(|error| error.to_string())?,
    ))
}

pub(super) fn plugin_inspect(
    _state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let request = match serde_json::from_slice::<WorkspacePluginInspectRequest>(body.as_ref()) {
        Ok(value) if value.schema == WORKSPACE_PLUGIN_INSPECT_SCHEMA => value,
        Ok(_) | Err(_) => {
            return json_error_response(400, "plugin_inspection_request_invalid");
        }
    };
    let package = match inspect_plugin_package_at(
        Path::new(PLUGIN_CATALOG_ROOT),
        request.package_path.as_str(),
    ) {
        Ok(package) => package,
        Err(error) => {
            eprintln!("workspace Plugin package inspection failed: {error}");
            return json_error_response(400, "plugin_package_invalid");
        }
    };
    let response = WorkspacePluginInspectResponse {
        schema: WORKSPACE_PLUGIN_INSPECT_RESULT_SCHEMA,
        package,
    };
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("encode workspace Plugin inspection failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

pub(super) fn skills_catalog(
    _state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let request = match serde_json::from_slice::<WorkspaceSkillCatalogRequest>(body.as_ref())
        .map_err(|error| format!("invalid json: {error}"))
    {
        Ok(value) if value.schema == WORKSPACE_SKILL_CATALOG_SCHEMA => value,
        Ok(_) => {
            return json_error_response(400, "workspace_skill_catalog_schema_mismatch");
        }
        Err(error) => {
            return json_error_response(400, error.as_str());
        }
    };
    let response = match workspace_skill_catalog(&request.plugin_activation) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("workspace skill catalog failed: {error}");
            return json_error_response(500, "workspace_skill_catalog_unavailable");
        }
    };
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("encode workspace skill catalog failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

pub(super) fn skill_detail(
    _state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let request = match serde_json::from_slice::<WorkspaceSkillDetailRequest>(body.as_ref())
        .map_err(|error| format!("invalid json: {error}"))
    {
        Ok(value) if value.schema == WORKSPACE_SKILL_DETAIL_SCHEMA => value,
        Ok(_) => {
            return json_error_response(400, "workspace_skill_detail_schema_mismatch");
        }
        Err(error) => {
            return json_error_response(400, error.as_str());
        }
    };
    let response =
        match workspace_skill_detail(&request.plugin_activation, request.skill_id.as_str()) {
            Ok(Some(value)) => value,
            Ok(None) => {
                return json_error_response(404, "skill_not_found");
            }
            Err(error) => {
                eprintln!("workspace skill detail failed: {error}");
                return json_error_response(500, "workspace_skill_detail_unavailable");
            }
        };
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("encode workspace skill detail failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

pub(super) fn mcp_catalog(
    _state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let request = match serde_json::from_slice::<WorkspaceMcpCatalogRequest>(body.as_ref())
        .map_err(|error| format!("invalid json: {error}"))
    {
        Ok(value) if value.schema == WORKSPACE_MCP_CATALOG_SCHEMA => value,
        Ok(_) => return json_error_response(400, "workspace_mcp_catalog_schema_mismatch"),
        Err(error) => return json_error_response(400, error.as_str()),
    };
    let response = match workspace_mcp_catalog(&request.plugin_activation) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("workspace MCP catalog failed: {error}");
            return json_error_response(500, "workspace_mcp_catalog_unavailable");
        }
    };
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("encode workspace MCP catalog failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

pub(super) fn hooks_catalog(
    _state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let body = request.into_body();
    let request = match serde_json::from_slice::<WorkspaceHookCatalogRequest>(body.as_ref())
        .map_err(|error| format!("invalid json: {error}"))
    {
        Ok(value) if value.schema == WORKSPACE_HOOK_CATALOG_SCHEMA => value,
        Ok(_) => return json_error_response(400, "workspace_hook_catalog_schema_mismatch"),
        Err(error) => return json_error_response(400, error.as_str()),
    };
    let response = match workspace_hook_catalog(&request.plugin_activation) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("workspace Hook catalog failed: {error}");
            return json_error_response(500, "workspace_hook_catalog_unavailable");
        }
    };
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("encode workspace Hook catalog failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

pub(super) fn model_catalog(
    _state: HttpServerState,
    _request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let response = serde_json::to_vec(&workspace_model_catalog_response())
        .map_err(|error| format!("encode workspace model catalog failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}

pub(super) fn execution_profile(
    state: HttpServerState,
    _request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let response = serde_json::to_vec(state.execution_profile.as_ref())
        .map_err(|error| format!("encode Runtime execution profile failed: {error}"))?;
    Ok(http_response(200, "application/json", response))
}
