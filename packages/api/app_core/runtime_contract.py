import hashlib
import hmac
import json
import unicodedata

from django.conf import settings

from .agent_identity import validate_agent_id
from .models import validate_thinking_mode
from .plugin_catalog import plugin_activation_for_workspace, validate_plugin_activation


AGENT_RUN_AUTHORIZATION_SCHEMA = "workspace.agent_run_authorization.v1"
RESOLVED_INPUT_SCHEMA = "runtime.resolved_input.v1"
DECLARED_INPUT_SCHEMA = "runtime.declared_input.v1"
AGENT_RUN_START_SCHEMA = "workspace.agent_run.start.v2"
MODEL_RUN_SCHEMA = "api.model.run.v1"
AGENT_RUN_AUTHORIZATION_FIELDS = {
    "schema",
    "id",
    "organizationId",
    "workspaceId",
    "userId",
    "agentId",
    "sessionId",
    "agentRunId",
    "sessionWorkspace",
    "modelConfigRef",
    "thinkingMode",
    "artifactScopeRef",
    "assetRefs",
    "messageAssetRefs",
    "imageCapability",
    "imageDigest",
    "pluginActivation",
    "resources",
}
SESSION_WORKSPACE_FIELDS = {
    "generation",
    "snapshotSha256",
    "snapshotSizeBytes",
    "expandedSizeBytes",
    "fileCount",
}
DECLARED_INPUT_FIELDS = {
    "schema",
    "inputRef",
    "displayName",
    "contentType",
    "inputIdentity",
    "sizeBytes",
}
INPUT_IDENTITY_FIELDS = {"ownerKind", "ownerId", "generation", "sha256"}
RESOLVED_INPUT_FIELDS = {
    "schema",
    "inputRef",
    "objectRef",
    "ownerKind",
    "virtualPath",
    "displayName",
    "contentType",
    "sizeBytes",
    "sha256",
    "sourceVersion",
    "evidenceKind",
    "citationAllowed",
}
OWNER_KINDS = {"sourceObject", "userLibraryObject", "artifact"}
EVIDENCE_KINDS = {"workspaceSource", "userProvided", "generatedArtifact"}
MAX_DECLARED_INPUTS = 64


def build_agent_run_authorization_payload(
    agent_run,
    authorization_id: str,
    message_asset_refs: list[str] | None = None,
    *,
    image_digest: str,
    declared_inputs: list[dict] | None = None,
) -> dict:
    from .assets import deferred_input_refs

    asset_refs = deferred_input_refs(agent_run) if declared_inputs is None else declared_inputs
    available_input_refs = {item["inputRef"] for item in asset_refs}
    normalized_message_asset_refs = sorted(set(message_asset_refs or []))
    if any(
        input_ref not in available_input_refs for input_ref in normalized_message_asset_refs
    ):
        raise ValueError("messageAssetRefs must reference authorized session assets")
    payload = {
        "schema": AGENT_RUN_AUTHORIZATION_SCHEMA,
        "id": authorization_id,
        "organizationId": "default_org",
        "workspaceId": agent_run.workspace_id,
        "userId": str(agent_run.user_id),
        "agentId": agent_run.session.agent_id,
        "sessionId": agent_run.session_id,
        "agentRunId": agent_run.id,
        "sessionWorkspace": session_workspace_for_session(agent_run.session),
        "modelConfigRef": agent_run.modelConfig_id,
        "thinkingMode": agent_run.thinkingMode or None,
        "artifactScopeRef": f"artifact_scope_{agent_run.id}",
        "assetRefs": asset_refs,
        "messageAssetRefs": normalized_message_asset_refs,
        "imageCapability": "workspace_general_v1",
        "imageDigest": image_digest,
        "pluginActivation": (
            agent_run.definition_version.plugin_activation
            if agent_run.definition_version_id else plugin_activation_for_workspace(agent_run.workspace)
        ),
        "resources": {
            "memoryBytes": settings.SANDBOX_MEMORY_BYTES,
            "cpuMilli": settings.SANDBOX_CPU_MILLI,
            "pidsLimit": settings.SANDBOX_PIDS_LIMIT,
            "dataTmpfsBytes": settings.SANDBOX_DATA_TMPFS_BYTES,
        },
    }
    validate_agent_run_authorization_payload(payload)
    return payload


def session_workspace_for_session(session) -> dict:
    return {
        "generation": session.workspaceGeneration,
        "snapshotSha256": session.workspaceSnapshotSha256,
        "snapshotSizeBytes": session.workspaceSnapshotSizeBytes,
        "expandedSizeBytes": session.workspaceExpandedSizeBytes,
        "fileCount": session.workspaceFileCount,
    }


def authorization_digest(payload: dict) -> str:
    validate_agent_run_authorization_payload(payload)
    return _validated_authorization_digest(payload)


def _validated_authorization_digest(payload: dict) -> str:
    canonical = json.dumps(
        payload, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    )
    return f"sha256:{hashlib.sha256(canonical.encode('utf-8')).hexdigest()}"


def authorization_signature(payload: dict, signing_key: str) -> str:
    if not signing_key:
        raise ValueError("AgentRun authorization signing key is required")
    digest = authorization_digest(payload)
    return _authorization_signature_for_digest(digest, signing_key)


def _authorization_signature_for_digest(digest: str, signing_key: str) -> str:
    if not signing_key:
        raise ValueError("AgentRun authorization signing key is required")
    signature = hmac.new(
        signing_key.encode("utf-8"),
        f"workspace:agent-run-authorization:v1\0{digest}".encode("utf-8"),
        hashlib.sha256,
    ).hexdigest()
    return f"hmac-sha256:{signature}"


def verify_agent_run_authorization_signature(
    payload: dict, signing_key: str, signature: str
) -> None:
    if not signing_key:
        raise ValueError("AgentRun authorization signing key is required")
    _verify_authorization_digest_signature(authorization_digest(payload), signing_key, signature)


def _verify_authorization_digest_signature(digest: str, signing_key: str, signature: str) -> None:
    # Internal consumers pass the digest they just computed from a validated payload,
    # never an unverified digest from a request.
    if not hmac.compare_digest(_authorization_signature_for_digest(digest, signing_key), signature):
        raise ValueError("AgentRunAuthorization signature mismatch")


def agent_run_binding_matches(payload: dict, agent_run, *, session=None) -> bool:
    return (
        payload["agentRunId"] == agent_run.id
        and payload["workspaceId"] == agent_run.workspace_id
        and payload["sessionId"] == agent_run.session_id
        and payload["userId"] == str(agent_run.user_id)
        and payload["agentId"] == (agent_run.session if session is None else session).agent_id
        and payload["modelConfigRef"] == agent_run.modelConfig_id
    )


def validate_agent_run_authorization_payload(payload: dict) -> None:
    require_exact_fields(payload, AGENT_RUN_AUTHORIZATION_FIELDS, "AgentRun authorization")
    if payload["schema"] != AGENT_RUN_AUTHORIZATION_SCHEMA:
        raise ValueError("agent_run_authorization_schema_mismatch")
    for name in [
        "id",
        "organizationId",
        "workspaceId",
        "userId",
        "agentId",
        "sessionId",
        "agentRunId",
        "modelConfigRef",
        "artifactScopeRef",
    ]:
        require_opaque_ref(name, payload[name])
    validate_agent_id(payload["agentId"])
    if payload["thinkingMode"] is not None:
        validate_thinking_mode(payload["thinkingMode"])
    validate_session_workspace(payload["sessionWorkspace"])
    asset_refs = payload["assetRefs"]
    if not isinstance(asset_refs, list):
        raise ValueError("assetRefs must be a list")
    if len(asset_refs) > MAX_DECLARED_INPUTS:
        raise ValueError("assetRefs must contain at most 64 direct inputs")
    for item in asset_refs:
        validate_declared_input(item)
    input_refs = [item["inputRef"] for item in asset_refs]
    if input_refs != sorted(set(input_refs)):
        raise ValueError("declared inputs must be sorted by unique inputRef")
    message_asset_refs = payload["messageAssetRefs"]
    if (
        not isinstance(message_asset_refs, list)
        or any(not isinstance(input_ref, str) for input_ref in message_asset_refs)
        or message_asset_refs != sorted(set(message_asset_refs))
        or any(input_ref not in set(input_refs) for input_ref in message_asset_refs)
    ):
        raise ValueError("messageAssetRefs must be sorted unique authorized inputRefs")
    if payload["imageCapability"] != "workspace_general_v1":
        raise ValueError("imageCapability must be workspace_general_v1")
    require_sha256("imageDigest", payload["imageDigest"])
    validate_plugin_activation(payload["pluginActivation"])
    resources = payload["resources"]
    require_exact_fields(
        resources,
        {"memoryBytes", "cpuMilli", "pidsLimit", "dataTmpfsBytes"},
        "sandbox resources",
    )
    if any(
        not isinstance(resources[name], int)
        or isinstance(resources[name], bool)
        or resources[name] <= 0
        for name in resources
    ):
        raise ValueError("sandbox resources must be positive integers")
    for name, maximum in (
        ("memoryBytes", 2**64 - 1),
        ("cpuMilli", 2**32 - 1),
        ("pidsLimit", 2**32 - 1),
        ("dataTmpfsBytes", 2**64 - 1),
    ):
        if resources[name] > maximum:
            raise ValueError(f"sandbox resources.{name} exceeds transport range")
    if sum(item["sizeBytes"] for item in asset_refs) > resources["dataTmpfsBytes"] // 2:
        raise ValueError("declared inputs must fit within half of dataTmpfsBytes")


def validate_session_workspace(payload: dict) -> None:
    require_exact_fields(payload, SESSION_WORKSPACE_FIELDS, "session workspace")
    for name in [
        "generation",
        "snapshotSizeBytes",
        "expandedSizeBytes",
        "fileCount",
    ]:
        value = payload[name]
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise ValueError(f"sessionWorkspace.{name} must be a non-negative integer")
    is_empty_snapshot = (
        payload["snapshotSha256"] == ""
        and payload["snapshotSizeBytes"] == 0
        and payload["expandedSizeBytes"] == 0
        and payload["fileCount"] == 0
    )
    if payload["generation"] == 0 and not is_empty_snapshot:
        raise ValueError("sessionWorkspace generation zero requires an empty snapshot")
    if is_empty_snapshot:
        return
    require_sha256("sessionWorkspace.snapshotSha256", payload["snapshotSha256"])
    if payload["snapshotSizeBytes"] == 0 or payload["fileCount"] == 0:
        raise ValueError(
            "sessionWorkspace non-empty snapshot requires size and fileCount"
        )


def validate_declared_input(payload: dict) -> None:
    require_exact_fields(payload, DECLARED_INPUT_FIELDS, "declared input")
    if payload["schema"] != DECLARED_INPUT_SCHEMA:
        raise ValueError("declared_input_schema_mismatch")
    require_opaque_ref("inputRef", payload["inputRef"])
    require_string("displayName", payload["displayName"])
    require_string("contentType", payload["contentType"])
    identity = payload["inputIdentity"]
    require_exact_fields(identity, INPUT_IDENTITY_FIELDS, "input identity")
    if identity["ownerKind"] not in OWNER_KINDS:
        raise ValueError(f"unsupported ownerKind: {identity['ownerKind']}")
    require_opaque_ref("ownerId", identity["ownerId"])
    if (
        not isinstance(identity["generation"], int)
        or isinstance(identity["generation"], bool)
        or identity["generation"] <= 0
    ):
        raise ValueError("input identity generation must be positive")
    require_sha256("inputIdentity.sha256", identity["sha256"])
    if (
        not isinstance(payload["sizeBytes"], int)
        or isinstance(payload["sizeBytes"], bool)
        or not 0 <= payload["sizeBytes"] <= 64 * 1024 * 1024
    ):
        raise ValueError("declared input sizeBytes exceeds direct input policy")


def require_sha256(name: str, value) -> None:
    if not isinstance(value, str) or not value.startswith("sha256:"):
        raise ValueError(f"{name} must use sha256:<hex> format")
    digest = value.removeprefix("sha256:")
    if len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        raise ValueError(f"{name} must contain 64 lowercase hexadecimal characters")


def validate_resolved_input(payload: dict) -> None:
    require_exact_fields(payload, RESOLVED_INPUT_FIELDS, "resolved input")
    if payload["schema"] != RESOLVED_INPUT_SCHEMA:
        raise ValueError("resolved_input_schema_mismatch")
    require_opaque_ref("inputRef", payload["inputRef"])
    require_opaque_ref("objectRef", payload["objectRef"])
    require_string("displayName", payload["displayName"])
    require_string("contentType", payload["contentType"])
    require_string("sourceVersion", payload["sourceVersion"])
    if payload["ownerKind"] not in OWNER_KINDS:
        raise ValueError(f"unsupported ownerKind: {payload['ownerKind']}")
    if payload["evidenceKind"] not in EVIDENCE_KINDS:
        raise ValueError(f"unsupported evidenceKind: {payload['evidenceKind']}")
    if not isinstance(payload["citationAllowed"], bool):
        raise ValueError("citationAllowed must be a boolean")
    if (
        not isinstance(payload["sizeBytes"], int)
        or isinstance(payload["sizeBytes"], bool)
        or payload["sizeBytes"] < 0
    ):
        raise ValueError("sizeBytes must be a non-negative integer")
    validate_virtual_path(payload["virtualPath"])
    sha256 = payload["sha256"]
    if not isinstance(sha256, str) or not sha256.startswith("sha256:"):
        raise ValueError("sha256 must use sha256:<hex> format")
    digest = sha256.removeprefix("sha256:")
    if len(digest) != 64 or any(
        character not in "0123456789abcdef" for character in digest
    ):
        raise ValueError("sha256 must contain 64 lowercase hexadecimal characters")


def validate_virtual_path(value) -> None:
    require_string("virtualPath", value)
    if "\\" in value or ":" in value or value.startswith("/"):
        raise ValueError("virtualPath must be a relative POSIX path")
    if any(part in {"", ".", ".."} for part in value.split("/")):
        raise ValueError("virtualPath must not contain dot or parent components")
    if any(unicodedata.category(character).startswith("C") for character in value):
        raise ValueError("virtualPath must not contain control characters")


def require_exact_fields(payload, expected: set[str], label: str) -> None:
    if not isinstance(payload, dict):
        raise ValueError(f"{label} must be an object")
    fields = set(payload)
    if fields != expected:
        missing = sorted(expected - fields)
        unknown = sorted(fields - expected)
        raise ValueError(
            f"{label} fields mismatch: missing={missing}, unknown={unknown}"
        )


def require_string(name: str, value) -> None:
    if not isinstance(value, str) or not value.strip() or value != value.strip():
        raise ValueError(f"{name} is required without outer whitespace")
    if unicodedata.normalize("NFC", value) != value:
        raise ValueError(f"{name} must use NFC Unicode normalization")


def require_opaque_ref(name: str, value) -> None:
    require_string(name, value)
    if "/" in value or "\\" in value:
        raise ValueError(f"{name} must be an opaque ref, not a path")
