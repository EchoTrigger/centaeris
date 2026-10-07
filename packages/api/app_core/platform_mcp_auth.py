"""Short-lived, first-party service credentials, not public OAuth access tokens.

Only the authenticated host may issue a credential. The model never supplies
these claims. Recheck the current signed run authorization on every request.
"""

import time

from django.conf import settings
from django.core import signing
from django.core.exceptions import ObjectDoesNotExist

from .material_access import MaterialAccessContext, authorize_material_access
from .material_contract import KnowledgeError
from .runtime_contract import _verify_authorization_digest_signature, agent_run_binding_matches


SALT = "centaeris.workspace.platform-mcp.credential.v1"
TTL_SECONDS = 300
MAX_TOKEN_BYTES = 16 * 1024
FIELDS = {"schema", "agentRunId", "authorizationDigest", "processingSpecification", "specDigest", "issuedAt", "expiresAt"}


class CredentialRejected(ValueError):
    def __init__(self):
        super().__init__("platform_mcp_credential_rejected")


def signing_key():
    # Domain-separated from AgentRun authorization signatures and Django sessions.
    return settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY


def authorize_context(context: MaterialAccessContext):
    try:
        access = authorize_material_access(context)
        run = access.agent_run
        payload = run.authorization.payload
        _verify_authorization_digest_signature(access.authorization_digest, signing_key(), run.authorization.signature)
        if not agent_run_binding_matches(payload, run):
            raise CredentialRejected()
        return access
    except (KnowledgeError, ObjectDoesNotExist, ValueError, TypeError, KeyError) as error:
        raise CredentialRejected() from error


def issue_credential(context: MaterialAccessContext) -> dict:
    authorize_context(context)
    now = int(time.time())
    claims = {
        "schema": "workspace.mcp.credential.v1", "agentRunId": context.agent_run_id,
        "authorizationDigest": context.authorization_digest,
        "processingSpecification": context.processing_specification,
        "specDigest": context.spec_digest, "issuedAt": now, "expiresAt": now + TTL_SECONDS,
    }
    token = signing.dumps(claims, key=signing_key(), salt=SALT)
    if len(token) > MAX_TOKEN_BYTES:
        raise CredentialRejected()
    return {"schema": "workspace.mcp.credential.result.v1", "accessToken": token, "expiresAt": now + TTL_SECONDS}


def verify_credential(token: str) -> MaterialAccessContext:
    try:
        if not isinstance(token, str) or not 0 < len(token) <= MAX_TOKEN_BYTES:
            raise CredentialRejected()
        claims = signing.loads(token, key=signing_key(), salt=SALT, fallback_keys=[])
        if not isinstance(claims, dict) or set(claims) != FIELDS or claims["schema"] != "workspace.mcp.credential.v1":
            raise CredentialRejected()
        issued, expires = claims["issuedAt"], claims["expiresAt"]
        if type(issued) is not int or type(expires) is not int or expires - issued != TTL_SECONDS or not issued <= time.time() < expires:
            raise CredentialRejected()
        context = MaterialAccessContext(claims["agentRunId"], claims["authorizationDigest"], claims["processingSpecification"], claims["specDigest"])
        authorize_context(context)
        return context
    except (signing.BadSignature, ValueError, TypeError, KeyError) as error:
        raise CredentialRejected() from error
