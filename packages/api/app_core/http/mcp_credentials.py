import json
import logging

from django.conf import settings
from django.db import IntegrityError, transaction
from django.http import JsonResponse
from ninja import Router, Status
from ninja.responses import codes_4xx

from app_core.credentials import (
    CredentialDecryptionError,
    decrypt_credential_secret,
    encrypt_credential_secret,
    normalize_bearer_token_input,
    validate_bearer_token,
    validate_display_name,
    validate_lower_kebab,
)
from app_core.models import (
    AgentRunAuthorization,
    McpBearerCredential,
    McpCredentialAuditEvent,
)
from app_core.plugin_catalog import load_plugin_catalog, plugin_lifecycle_lock
from app_core.runtime_contract import (
    authorization_digest,
    require_opaque_ref,
    require_sha256,
    validate_agent_run_authorization_payload,
    verify_agent_run_authorization_signature,
)

from .response_schema import COMMON_ERROR_RESPONSES
from .schema import ErrorResponse, StrictSchema
from .security import internal_token_auth, superuser_auth


logger = logging.getLogger(__name__)
router = Router(tags=["mcp-credentials"], by_alias=True)
internal_router = Router(tags=["internal"], by_alias=True)

MCP_CREDENTIAL_RESOLVE_SCHEMA = "runtime.mcp_bearer_credential.resolve.v1"
MCP_CREDENTIAL_RESOLVED_SCHEMA = "runtime.mcp_bearer_credential.resolved.v1"


class McpBearerCredentialRequest(StrictSchema):
    plugin_name: str
    credential_ref: str
    display_name: str
    secret: str


class RotateMcpBearerCredentialRequest(StrictSchema):
    secret: str


class McpBearerCredentialResponse(StrictSchema):
    id: str
    plugin_name: str
    credential_ref: str
    display_name: str
    version: int
    updated_at: str


class McpBearerCredentialEnvelope(StrictSchema):
    credential: McpBearerCredentialResponse


class McpBearerCredentialsEnvelope(StrictSchema):
    credentials: list[McpBearerCredentialResponse]


@router.get(
    "/admin/mcp-bearer-credentials",
    auth=superuser_auth,
    response={200: McpBearerCredentialsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_mcp_bearer_credentials(request):
    credentials = McpBearerCredential.objects.filter(workspace__isnull=True).order_by(
        "plugin_name", "credential_ref"
    )
    return {"credentials": [_serialize_credential(item) for item in credentials]}


@router.post(
    "/admin/mcp-bearer-credentials",
    auth=superuser_auth,
    response={201: McpBearerCredentialEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_mcp_bearer_credential(
    request, payload: McpBearerCredentialRequest
):
    try:
        with plugin_lifecycle_lock():
            try:
                plugin_exists = _plugin_exists(payload.plugin_name)
            except ValueError:
                return Status(503, {"error": "plugin_catalog_invalid"})
            plugin_name = validate_lower_kebab("MCP plugin name", payload.plugin_name)
            credential_ref = validate_lower_kebab(
                "MCP bearer credential ref", payload.credential_ref
            )
            display_name = validate_display_name(payload.display_name)
            secret = normalize_bearer_token_input(payload.secret)
            if not plugin_exists:
                return Status(404, {"error": "plugin_not_found"})
            with transaction.atomic():
                credential = McpBearerCredential.objects.create(
                    plugin_name=plugin_name,
                    credential_ref=credential_ref,
                    display_name=display_name,
                    encrypted_secret=encrypt_credential_secret(secret),
                    created_by=request.user,
                    updated_by=request.user,
                )
                _record_audit(credential, "created", request.user)
    except IntegrityError:
        return Status(409, {"error": "mcp_bearer_credential_conflict"})
    except ValueError:
        return Status(400, {"error": "mcp_bearer_credential_invalid"})
    return Status(201, {"credential": _serialize_credential(credential)})


@router.post(
    "/admin/mcp-bearer-credentials/{credential_id}/rotate",
    auth=superuser_auth,
    response={200: McpBearerCredentialEnvelope} | COMMON_ERROR_RESPONSES,
)
def rotate_mcp_bearer_credential(
    request, credential_id: str, payload: RotateMcpBearerCredentialRequest
):
    try:
        secret = normalize_bearer_token_input(payload.secret)
        with transaction.atomic():
            credential = McpBearerCredential.objects.select_for_update().get(
                id=credential_id, workspace__isnull=True
            )
            credential.encrypted_secret = encrypt_credential_secret(secret)
            credential.version += 1
            credential.updated_by = request.user
            credential.save(
                update_fields=[
                    "encrypted_secret",
                    "version",
                    "updated_by",
                    "updated_at",
                ]
            )
            _record_audit(credential, "rotated", request.user)
    except McpBearerCredential.DoesNotExist:
        return Status(404, {"error": "mcp_bearer_credential_not_found"})
    except ValueError:
        return Status(400, {"error": "mcp_bearer_credential_invalid"})
    return {"credential": _serialize_credential(credential)}


@router.delete(
    "/admin/mcp-bearer-credentials/{credential_id}",
    auth=superuser_auth,
    response={204: None, codes_4xx: ErrorResponse},
)
def delete_mcp_bearer_credential(request, credential_id: str):
    with transaction.atomic():
        try:
            credential = McpBearerCredential.objects.select_for_update().get(
                id=credential_id, workspace__isnull=True
            )
        except McpBearerCredential.DoesNotExist:
            return Status(404, {"error": "mcp_bearer_credential_not_found"})
        _record_audit(credential, "deleted", request.user)
        credential.delete()
    return Status(204, None)




def _plugin_exists(plugin_name: str) -> bool:
    try:
        return any(
            package["name"] == plugin_name
            for package in load_plugin_catalog(require_packages=False)["packages"]
        )
    except ValueError:
        logger.exception("Release Plugin catalog is invalid")
        raise




def _serialize_credential(credential: McpBearerCredential) -> dict:
    return {
        "id": credential.id,
        "pluginName": credential.plugin_name,
        "credentialRef": credential.credential_ref,
        "displayName": credential.display_name,
        "version": credential.version,
        "updatedAt": credential.updated_at.isoformat(),
    }


def _record_audit(credential: McpBearerCredential, action: str, actor) -> None:
    McpCredentialAuditEvent.objects.create(
        credential_id=credential.id,
        plugin_name=credential.plugin_name,
        credential_ref=credential.credential_ref,
        display_name=credential.display_name,
        action=action,
        actor=actor,
    )
