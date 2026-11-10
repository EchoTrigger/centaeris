import json
from django.http import JsonResponse
from ninja import Router
from app_core.assistant_connectors import ConnectorRejected, authorize_connector
from app_core.credentials import CredentialDecryptionError, validate_lower_kebab
from app_core.models import AgentRunAuthorization
from app_core.runtime_contract import require_opaque_ref, require_sha256
from .security import internal_token_auth
internal_router = Router(tags=["internal"], by_alias=True)


@internal_router.post("/mcp-connectors/authorize", auth=internal_token_auth, response=None, include_in_schema=False)
def authorize_mcp_connector(request):
    expected = {"schema", "agentRunId", "authorizationRef", "authorizationDigest", "pluginName", "serverId",
                "resourcePath", "resourceDigest", "operation", "bindingDigest"}
    try:
        body = json.loads(request.body.decode("utf-8"))
        if not isinstance(body, dict) or set(body) != expected:
            return JsonResponse({"error": "mcp_connector_fields_invalid"}, status=400)
        if body["schema"] != "runtime.mcp_connector.authorization.v1" or body["operation"] not in {"connect", "dispatch"}:
            raise ValueError
        for field in ("agentRunId", "authorizationRef"):
            require_opaque_ref(field, body[field])
        for field in ("authorizationDigest", "resourceDigest"):
            require_sha256(field, body[field])
        if body["bindingDigest"] is not None:
            require_sha256("bindingDigest", body["bindingDigest"])
        for field in ("pluginName", "serverId"):
            validate_lower_kebab(field, body[field])
        from app_core.plugin_catalog import _require_resource_path
        _require_resource_path(body["resourcePath"])
    except (ValueError, TypeError, UnicodeDecodeError):
        return JsonResponse({"error": "mcp_connector_request_invalid"}, status=400)
    authorization = AgentRunAuthorization.objects.select_related("agent_run__session", "agent_run__user",
        "agent_run__workspace").filter(id=body["authorizationRef"],
        agent_run_id=body["agentRunId"], digest=body["authorizationDigest"]).first()
    if authorization is None:
        return JsonResponse({"error": "agent_run_authorization_not_found"}, status=404)
    try:
        result = authorize_connector(authorization, body)
    except ConnectorRejected as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (ValueError, KeyError, OSError, CredentialDecryptionError):
        return JsonResponse({"error": "mcp_connector_authorization_invalid"}, status=409)
    response = JsonResponse(result)
    response["Cache-Control"] = "no-store"
    return response
