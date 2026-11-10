from app_core.models import McpBearerCredential, McpCredentialAuditEvent

from .schema import StrictSchema


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
