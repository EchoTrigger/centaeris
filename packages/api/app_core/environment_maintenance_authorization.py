"""Signed maintenance authority, independent of AgentRun grants.

Issuers must hold the environment controller lock and record the grant before
returning it. Signature verification does not replace checking the current
controller epoch, lease deadline, physical identity and operation receipt.
"""
import hashlib
import hmac
import json
import re


SCHEMA = "workspace.environment_maintenance_authorization.v1"
DOMAIN = b"workspace:environment-maintenance-authorization:v1\0"
FIELDS = {
    "schema", "workspaceId", "sessionId", "environmentId", "instanceId", "containerId",
    "profileDigest", "controllerOwner", "controllerEpoch", "controllerLeaseDigest",
    "controllerPurpose", "requestDigest", "expiresAtMs",
}


def _matches(value, pattern):
    return isinstance(value, str) and re.fullmatch(pattern, value) is not None


def sign_maintenance_authorization(payload, signing_key):
    if not isinstance(signing_key, str) or not signing_key:
        raise ValueError("maintenance_authorization_key_invalid")
    if not isinstance(payload, dict) or set(payload) != FIELDS or payload["schema"] != SCHEMA:
        raise ValueError("maintenance_authorization_fields_invalid")
    for field in ("workspaceId", "sessionId", "environmentId"):
        if not _matches(payload[field], r"[A-Za-z0-9_.:-]{1,160}"):
            raise ValueError("maintenance_authorization_identity_invalid")
    owner = payload["controllerOwner"]
    if (not isinstance(owner, dict) or set(owner) != {"kind", "operationId"}
            or owner["kind"] != "maintenanceOperation"
            or not _matches(owner["operationId"], r"[A-Za-z0-9_.:-]{1,160}")):
        raise ValueError("maintenance_authorization_owner_invalid")
    if (not _matches(payload["instanceId"], r"instance_[0-9a-f]{64}")
            or not _matches(payload["containerId"], r"[0-9a-f]{64}")):
        raise ValueError("maintenance_authorization_target_invalid")
    for field in ("profileDigest", "controllerLeaseDigest", "requestDigest"):
        if not _matches(payload[field], r"sha256:[0-9a-f]{64}"):
            raise ValueError("maintenance_authorization_digest_invalid")
    for field in ("controllerEpoch", "expiresAtMs"):
        if type(payload[field]) is not int or not 1 <= payload[field] <= 2**63 - 1:
            raise ValueError("maintenance_authorization_fence_invalid")
    if payload["controllerPurpose"] not in ("execute", "cleanup"):
        raise ValueError("maintenance_authorization_purpose_invalid")
    canonical = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    digest = "sha256:" + hashlib.sha256(canonical).hexdigest()
    return "hmac-sha256:" + hmac.new(signing_key.encode("utf-8"),
        DOMAIN + digest.encode("ascii"), hashlib.sha256).hexdigest()
