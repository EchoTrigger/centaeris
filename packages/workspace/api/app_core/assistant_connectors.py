"""Hosted connector grants; immutable configuration never freezes security authority."""
import hashlib
import json

from django.conf import settings
from django.db import transaction

from .app_delegations import DelegationRejected, require_run_delegation
from .credentials import decrypt_credential_secret, validate_bearer_token
from .models import McpBearerCredential, McpCredentialAuditEvent
from .plugin_catalog import load_plugin_connector_resources
from .runtime_contract import (
    authorization_digest, validate_agent_run_authorization_payload, verify_agent_run_authorization_signature,
)
from .workspace_access import locked_workspace_membership_for


class ConnectorRejected(Exception):
    def __init__(self, code, status=409):
        self.code, self.status = code, status
        super().__init__(code)


def connector_resource(activation, plugin_name, server_id):
    package = next((item for item in activation["packages"] if item["name"] == plugin_name), None)
    if package is None:
        raise ConnectorRejected("mcp_connector_capability_invalid")
    server = next((item for item in load_plugin_connector_resources(package) if item["serverId"] == server_id), None)
    if server is None:
        raise ConnectorRejected("mcp_connector_resource_invalid")
    return server


def _check_authorization(authorization):
    payload = authorization.payload
    validate_agent_run_authorization_payload(payload)
    verify_agent_run_authorization_signature(payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, authorization.signature)
    run = authorization.agent_run
    session = run.session
    bindings = {"id": authorization.id, "agentRunId": run.id, "workspaceId": run.workspace_id,
                "userId": str(run.user_id), "sessionId": run.session_id, "modelConfigRef": run.modelConfig_id}
    if authorization_digest(payload) != authorization.digest or any(payload[key] != value for key, value in bindings.items()):
        raise ConnectorRejected("mcp_connector_authorization_invalid")
    if (run.status not in {"queued", "running"} or session.status != "active"
            or session.owner_id != run.user_id or session.workspace_id != run.workspace_id):
        raise ConnectorRejected("mcp_connector_authorization_invalid")
    return "workspace"


def authorize_connector(authorization, body):
    scope = _check_authorization(authorization)
    run = authorization.agent_run
    with transaction.atomic():
        membership = locked_workspace_membership_for(run.user, run.workspace_id)
        if membership is None or membership.id != run.membership_ref:
            raise ConnectorRejected("mcp_connector_membership_revoked", 403)
        if run.session.purpose == "agentRunApi":
            from .agent_run_api_credentials import authority_is_current, credential_for_run
            credential = credential_for_run(run)
            if credential is None or not authority_is_current(credential):
                raise ConnectorRejected("api_credential_revoked", 403)
            if body["pluginName"] not in credential.pluginNames:
                raise ConnectorRejected("mcp_connector_capability_revoked", 403)
        run.refresh_from_db()
        scope = _check_authorization(authorization)
        try:
            require_run_delegation(run, lock=True)
        except DelegationRejected as error:
            raise ConnectorRejected(error.code, error.status) from error
        if not run.workspace.pluginEnablements.filter(pluginName=body["pluginName"]).exists():
            raise ConnectorRejected("mcp_connector_capability_revoked", 403)
        resource = connector_resource(authorization.payload["pluginActivation"], body["pluginName"], body["serverId"])
        if (resource["resourcePath"], resource["resourceDigest"]) != (body["resourcePath"], body["resourceDigest"]):
            raise ConnectorRejected("mcp_connector_resource_invalid")
        identity = {"scope": scope, "workspace": run.workspace_id,
                    "plugin": body["pluginName"], "server": body["serverId"],
                    "resourcePath": resource["resourcePath"], "resourceDigest": resource["resourceDigest"]}
        credential = None
        if resource["credentialRef"] is not None:
            from .workspace_resources import credential_for_workspace
            try:
                credential = credential_for_workspace(run.workspace_id, body["pluginName"], resource["credentialRef"], lock=True)
            except McpBearerCredential.DoesNotExist:
                raise ConnectorRejected("mcp_bearer_credential_not_found", 404) from None
            identity.update(credential=credential.id, credentialVersion=credential.version)
        digest = "sha256:" + hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        if body["bindingDigest"] is not None and body["bindingDigest"] != digest:
            raise ConnectorRejected("mcp_connector_binding_changed")
        token = None
        if body["operation"] == "connect" and credential is not None:
            token = validate_bearer_token(decrypt_credential_secret(credential.encrypted_secret))
            McpCredentialAuditEvent.objects.create(credential_id=credential.id, plugin_name=credential.plugin_name,
                credential_ref=credential.credential_ref, display_name=credential.display_name, action="resolved", actor=run.user)
        return {"schema": "runtime.mcp_connector.authorized.v1", "scope": scope, "bindingDigest": digest, "token": token}
