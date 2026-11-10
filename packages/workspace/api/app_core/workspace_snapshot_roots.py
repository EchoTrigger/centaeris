"""Consume Runtime-owned recovery roots with a shared relational witness."""
import hashlib
import json
from pathlib import Path
import re
import urllib.error

from django.conf import settings
from django.db import connection

from .runtime_contract import require_opaque_ref, require_sha256, validate_session_workspace
from .runtime_http import request_json


ROOT_FIELDS = {"agentRunId", "authorizationDigest", "checkpointId", "objectRef", "snapshotSha256",
               "snapshotSizeBytes", "expandedSizeBytes", "fileCount"}
MANIFEST_FIELDS = {"schema", "sessionId", "sessionSequence", "authorityFingerprint", "roots", "protectedAgentRunIds"}
WITNESS_SQL = Path(__file__).with_name("workspace_snapshot_gc_witness.sql").read_text(encoding="utf-8").replace("$1", "%s")


class WorkspaceRootsUnavailable(RuntimeError):
    pass


def current_witness(session_id):
    with connection.cursor() as cursor:
        cursor.execute(WITNESS_SQL, [session_id])
        row = cursor.fetchone()
    if row is None:
        raise WorkspaceRootsUnavailable("workspace_snapshot_owner_missing")
    return (row[0], "sha256:" + hashlib.sha256(row[1].encode("utf-8")).hexdigest())


def validate_roots(session, manifest):
    try:
        if (not isinstance(manifest, dict) or set(manifest) != MANIFEST_FIELDS
                or manifest["schema"] != "runtime.workspace_snapshot.roots.v1"
                or manifest["sessionId"] != session.pk or type(manifest["sessionSequence"]) is not int
                or manifest["sessionSequence"] < 0 or not isinstance(manifest["roots"], list)
                or not isinstance(manifest["protectedAgentRunIds"], list)
                or len(manifest["roots"]) > 256 or len(manifest["protectedAgentRunIds"]) > 256):
            raise ValueError
        require_sha256("authorityFingerprint", manifest["authorityFingerprint"])
        ids = manifest["protectedAgentRunIds"]
        if len(ids) != len(set(ids)):
            raise ValueError
        for identity in ids:
            require_opaque_ref("agentRunId", identity)
        prefix = f"workspaces/{session.workspace_id}/sessions/{session.pk}/"
        for root in manifest["roots"]:
            if not isinstance(root, dict) or set(root) != ROOT_FIELDS:
                raise ValueError
            for field in ("agentRunId", "checkpointId"):
                require_opaque_ref(field, root[field])
            require_sha256("authorizationDigest", root["authorizationDigest"])
            validate_session_workspace({"generation": 1,
                **{field: root[field] for field in ("snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount")}})
            if root["snapshotSizeBytes"] == 0 or not isinstance(root["objectRef"], str):
                raise ValueError
            digest = root["snapshotSha256"][7:]
            checkpoint_prefix = prefix + f"agent-runs/{root['agentRunId']}/execution-checkpoints/"
            if not re.fullmatch(re.escape(checkpoint_prefix) + r"[0-9a-f]{64}/" + digest + r"\.snapshot", root["objectRef"]) and not re.fullmatch(
                    re.escape(prefix) + r"snapshots/[1-9][0-9]*/" + digest + r"\.snapshot", root["objectRef"]):
                raise ValueError
    except (KeyError, TypeError, ValueError):
        raise WorkspaceRootsUnavailable("workspace_snapshot_roots_invalid") from None
    return manifest


def request_workspace_snapshot_roots(session):
    try:
        manifest = request_json("/internal/workspace-snapshots/roots",
            {"schema": "runtime.workspace_snapshot.roots.get.v1", "sessionId": session.pk},
            timeout=settings.RUNTIME_CONTROL_TIMEOUT_SECONDS)
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        if isinstance(error, urllib.error.HTTPError):
            error.close()
        raise WorkspaceRootsUnavailable("workspace_snapshot_roots_unavailable") from error
    return validate_roots(session, manifest)


def require_current_roots(session, manifest):
    if (manifest["sessionId"] != session.pk or current_witness(session.pk)
            != (manifest["sessionSequence"], manifest["authorityFingerprint"])):
        raise WorkspaceRootsUnavailable("workspace_snapshot_roots_changed")
