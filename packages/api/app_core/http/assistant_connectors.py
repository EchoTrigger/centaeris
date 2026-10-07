import json

from django.db import transaction
from django.http import JsonResponse
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field

from app_core.assistant_connectors import ConnectorRejected, authorize_connector, connector_resource
from app_core.credentials import CredentialDecryptionError, validate_lower_kebab
from app_core.models import (AgentConnectorBinding, AgentConnectorCredentialApproval, AgentDefinition,
                             AgentRunAuthorization, McpBearerCredential, Workspace)
from app_core.plugin_catalog import require_current_plugin_activation
from app_core.runtime_contract import require_opaque_ref, require_sha256

from .agent_definitions import _admin_definition
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import internal_token_auth, session_auth, superuser_auth


router = Router(tags=["assistant connectors"], by_alias=True)
internal_router = Router(tags=["internal"], by_alias=True)


class ConnectorApprovalRequest(StrictSchema):
    workspace_id: str = Field(alias="workspaceId")
    definition_id: str = Field(alias="definitionId")
    server_id: str = Field(alias="serverId")


class ConnectorBindingRequest(StrictSchema):
    approval_id: str = Field(alias="approvalId")


class ConnectorApprovalResponse(StrictSchema):
    id: str
    plugin_name: str = Field(alias="pluginName")
    server_id: str = Field(alias="serverId")
    resource_path: str = Field(alias="resourcePath")
    resource_digest: str = Field(alias="resourceDigest")
    display_name: str = Field(alias="displayName")
    approved_at: str = Field(alias="approvedAt")


class ConnectorApprovalEnvelope(StrictSchema):
    approval: ConnectorApprovalResponse


class ConnectorApprovalsEnvelope(StrictSchema):
    approvals: list[ConnectorApprovalResponse]


class ConnectorBindingResponse(StrictSchema):
    id: str
    plugin_name: str = Field(alias="pluginName")
    server_id: str = Field(alias="serverId")
    approval_id: str = Field(alias="approvalId")


class ConnectorBindingEnvelope(StrictSchema):
    binding: ConnectorBindingResponse


class ConnectorBindingsEnvelope(StrictSchema):
    bindings: list[ConnectorBindingResponse]


def _approval_response(approval):
    return {"id": approval.id, "pluginName": approval.plugin_name, "serverId": approval.server_id,
            "resourcePath": approval.resource_path, "resourceDigest": approval.resource_digest,
            "displayName": approval.credential.display_name, "approvedAt": approval.approved_at.isoformat()}


def _binding_response(binding):
    return {"id": binding.id, "pluginName": binding.plugin_name, "serverId": binding.server_id, "approvalId": binding.approval_id}


@router.post("/admin/mcp-bearer-credentials/{credential_id}/assistant-approvals", auth=superuser_auth,
             response={201: ConnectorApprovalEnvelope} | COMMON_ERROR_RESPONSES)
def approve_assistant_credential(request, credential_id: str, payload: ConnectorApprovalRequest):
    with transaction.atomic():
        workspace = Workspace.objects.select_for_update().filter(id=payload.workspace_id, status="active").first()
        if workspace is None:
            return Status(404, {"error": "workspace_not_found"})
        definition = AgentDefinition.objects.filter(id=payload.definition_id, workspace=workspace).select_related("published_version").first()
        source = McpBearerCredential.objects.select_for_update().filter(id=credential_id, created_by=request.user).first()
        if definition is None or source is None:
            return Status(404, {"error": "mcp_credential_approval_scope_not_found"})
        if definition.published_version is None:
            return Status(409, {"error": "agent_definition_not_published"})
        try:
            require_current_plugin_activation(workspace, definition.published_version.plugin_activation)
            resource = connector_resource(definition.published_version.plugin_activation, source.plugin_name, payload.server_id)
        except (ConnectorRejected, ValueError, KeyError, OSError):
            return Status(400, {"error": "mcp_credential_approval_scope_invalid"})
        if resource["credentialRef"] is None:
            return Status(400, {"error": "mcp_credential_approval_scope_invalid"})
        approval = AgentConnectorCredentialApproval.objects.create(workspace=workspace, definition=definition,
            plugin_name=source.plugin_name, server_id=payload.server_id, resource_path=resource["resourcePath"],
            resource_digest=resource["resourceDigest"], credential=source, credential_version=source.version, approved_by=request.user)
    return Status(201, {"approval": _approval_response(approval)})


@router.delete("/admin/mcp-assistant-credential-approvals/{approval_id}", auth=superuser_auth,
               response={204: None} | COMMON_ERROR_RESPONSES)
def revoke_assistant_credential_approval(request, approval_id: str):
    approved = AgentConnectorCredentialApproval.objects.filter(id=approval_id, approved_by=request.user).first()
    if approved is None:
        return Status(404, {"error": "mcp_credential_approval_not_found"})
    with transaction.atomic():
        Workspace.objects.select_for_update().get(pk=approved.workspace_id)
        approved = AgentConnectorCredentialApproval.objects.select_for_update().get(pk=approved.pk)
        if approved.revoked_at is None:
            approved.revoked_at = timezone.now()
            approved.save(update_fields=["revoked_at"])
    return Status(204, None)


@router.get("/workspaces/{workspace_id}/agent-definitions/{definition_id}/credential-approvals", auth=session_auth,
            response={200: ConnectorApprovalsEnvelope} | COMMON_ERROR_RESPONSES)
def list_assistant_credential_approvals(request, workspace_id: str, definition_id: str):
    if _admin_definition(request.user, workspace_id, definition_id) is None:
        return Status(404, {"error": "agent_definition_not_found"})
    if request.GET:
        return Status(400, {"error": "mcp_connector_request_invalid"})
    approvals = AgentConnectorCredentialApproval.objects.filter(workspace_id=workspace_id, definition_id=definition_id,
        revoked_at__isnull=True).select_related("credential").order_by("approved_at", "id")
    return {"approvals": [_approval_response(item) for item in approvals if item.credential.version == item.credential_version]}


@router.get("/workspaces/{workspace_id}/agent-definitions/{definition_id}/connector-bindings", auth=session_auth,
            response={200: ConnectorBindingsEnvelope} | COMMON_ERROR_RESPONSES)
def list_assistant_connector_bindings(request, workspace_id: str, definition_id: str):
    if _admin_definition(request.user, workspace_id, definition_id) is None:
        return Status(404, {"error": "agent_definition_not_found"})
    if request.GET:
        return Status(400, {"error": "mcp_connector_request_invalid"})
    return {"bindings": [_binding_response(item) for item in AgentConnectorBinding.objects.filter(
        workspace_id=workspace_id, definition_id=definition_id).order_by("plugin_name", "server_id")]}


@router.put("/workspaces/{workspace_id}/agent-definitions/{definition_id}/connector-bindings/{plugin_name}/{server_id}", auth=session_auth,
            response={200: ConnectorBindingEnvelope} | COMMON_ERROR_RESPONSES)
def bind_assistant_connector(request, workspace_id: str, definition_id: str, plugin_name: str, server_id: str, payload: ConnectorBindingRequest):
    with transaction.atomic():
        definition = _admin_definition(request.user, workspace_id, definition_id, lock=True)
        if definition is None:
            return Status(404, {"error": "agent_definition_not_found"})
        approval = AgentConnectorCredentialApproval.objects.select_for_update().filter(id=payload.approval_id,
            workspace_id=workspace_id, definition=definition, plugin_name=plugin_name, server_id=server_id,
            revoked_at__isnull=True).select_related("credential").first()
        if approval is None or approval.credential.version != approval.credential_version:
            return Status(400, {"error": "mcp_credential_approval_scope_invalid"})
        try:
            if definition.published_version is None:
                raise ConnectorRejected("agent_definition_not_published")
            require_current_plugin_activation(definition.workspace, definition.published_version.plugin_activation)
            resource = connector_resource(definition.published_version.plugin_activation, plugin_name, server_id)
            if (approval.resource_path, approval.resource_digest) != (resource["resourcePath"], resource["resourceDigest"]):
                raise ConnectorRejected("mcp_connector_resource_invalid")
        except (ConnectorRejected, ValueError, KeyError, OSError):
            return Status(400, {"error": "mcp_credential_approval_scope_invalid"})
        binding, _ = AgentConnectorBinding.objects.update_or_create(workspace_id=workspace_id, definition=definition,
            plugin_name=plugin_name, server_id=server_id, defaults={"approval": approval, "updated_by": request.user})
    return {"binding": _binding_response(binding)}


@router.delete("/workspaces/{workspace_id}/agent-definitions/{definition_id}/connector-bindings/{plugin_name}/{server_id}", auth=session_auth,
               response={204: None} | COMMON_ERROR_RESPONSES)
def remove_assistant_connector_binding(request, workspace_id: str, definition_id: str, plugin_name: str, server_id: str):
    with transaction.atomic():
        if _admin_definition(request.user, workspace_id, definition_id, lock=True) is None:
            return Status(404, {"error": "agent_definition_not_found"})
        AgentConnectorBinding.objects.filter(workspace_id=workspace_id, definition_id=definition_id,
            plugin_name=plugin_name, server_id=server_id).delete()
    return Status(204, None)


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
    authorization = AgentRunAuthorization.objects.select_related("agent_run__session__agent", "agent_run__user",
        "agent_run__workspace", "agent_run__definition_version").filter(id=body["authorizationRef"],
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
