"""One fenced execution binding for every environment lifecycle policy."""
import hashlib
import hmac
import json
import re

from django.conf import settings

SCHEMA = "workspace.environment_authorization.v1"
DOMAIN = b"workspace:environment-authorization:v1\0"
FIELDS = {"schema", "environmentId", "sessionId", "agentRunId", "agentRunAuthorizationDigest", "executionMode",
    "controllerId", "controllerEpoch", "controllerLeaseDigest", "controllerPurpose"}


def sign_environment_authorization(payload, signing_key):
    if not signing_key or not isinstance(payload, dict) or set(payload) != FIELDS:
        raise ValueError("environment_controller_authorization_fields_invalid")
    if payload["schema"] != SCHEMA or payload["executionMode"] not in ("perRun", "retained"):
        raise ValueError("environment_controller_authorization_schema_or_mode_invalid")
    for name in ("environmentId", "sessionId", "agentRunId", "controllerId"):
        if not isinstance(payload[name], str) or not re.fullmatch(r"[A-Za-z0-9_.:-]{1,160}", payload[name]):
            raise ValueError("environment_controller_authorization_identity_invalid")
    if payload["controllerId"] != payload["agentRunId"]:
        raise ValueError("environment_controller_authorization_controller_invalid")
    if payload["controllerPurpose"] not in ("execute", "cleanup"):
        raise ValueError("environment_controller_authorization_purpose_invalid")
    epoch = payload["controllerEpoch"]
    if type(epoch) is not int or not 1 <= epoch <= 2**63 - 1:
        raise ValueError("environment_controller_authorization_epoch_invalid")
    for name in ("agentRunAuthorizationDigest", "controllerLeaseDigest"):
        if not isinstance(payload[name], str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", payload[name]):
            raise ValueError("environment_controller_authorization_digest_invalid")
    canonical = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    digest = "sha256:" + hashlib.sha256(canonical).hexdigest()
    return "hmac-sha256:" + hmac.new(signing_key.encode("utf-8"),
        DOMAIN + digest.encode("ascii"), hashlib.sha256).hexdigest()


def build_environment_authorization(agent_run, authorization, authority, *, execution_mode="perRun"):
    from .session_environments import agent_run_environment_matches
    if (not agent_run_environment_matches(agent_run)
            or set(authority) != {"environmentId", "controllerId", "controllerEpoch", "controllerLeaseDigest", "controllerPurpose"}
            or authority["environmentId"] != agent_run.environment_id
            or authority["controllerId"] != agent_run.pk):
        raise ValueError("environment_controller_authorization_binding_invalid")
    payload = {"schema": SCHEMA, "environmentId": agent_run.environment_id,
        "sessionId": agent_run.session_id, "agentRunId": agent_run.pk,
        "agentRunAuthorizationDigest": authorization.digest, "executionMode": execution_mode,
        "controllerId": authority["controllerId"], "controllerEpoch": authority["controllerEpoch"],
        "controllerLeaseDigest": authority["controllerLeaseDigest"], "controllerPurpose": authority["controllerPurpose"]}
    return {"payload": payload, "signature": sign_environment_authorization(
        payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY)}
