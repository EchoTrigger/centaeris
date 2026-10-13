"""Versioned execution binding; historical AgentRun grants remain unchanged."""
import hashlib
import hmac
import json
import re

from django.conf import settings

SCHEMA = "workspace.environment_authorization.v1"
DOMAIN = b"workspace:environment-authorization:v1\0"
FIELDS = {"schema", "environmentId", "sessionId", "agentRunId", "agentRunAuthorizationDigest", "executionMode"}


def sign_environment_authorization(payload, signing_key):
    if not signing_key or not isinstance(payload, dict) or set(payload) != FIELDS:
        raise ValueError("environment_authorization_fields_invalid")
    if payload["schema"] != SCHEMA or payload["executionMode"] != "perRun":
        raise ValueError("environment_authorization_schema_or_mode_invalid")
    for key in ("environmentId", "sessionId", "agentRunId"):
        if not isinstance(payload[key], str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,160}", payload[key]):
            raise ValueError("environment_authorization_identity_invalid")
    digest = payload["agentRunAuthorizationDigest"]
    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("environment_authorization_digest_invalid")
    canonical = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    digest = "sha256:" + hashlib.sha256(canonical).hexdigest()
    return "hmac-sha256:" + hmac.new(signing_key.encode("utf-8"), DOMAIN + digest.encode("ascii"), hashlib.sha256).hexdigest()


def build_environment_authorization(agent_run, authorization):
    from .session_environments import agent_run_environment_matches
    if not agent_run_environment_matches(agent_run):
        raise ValueError("environment_authorization_binding_invalid")
    payload = {"schema": SCHEMA, "environmentId": agent_run.environment_id,
        "sessionId": agent_run.session_id, "agentRunId": agent_run.pk,
        "agentRunAuthorizationDigest": authorization.digest, "executionMode": "perRun"}
    return {"payload": payload, "signature": sign_environment_authorization(
        payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY)}
