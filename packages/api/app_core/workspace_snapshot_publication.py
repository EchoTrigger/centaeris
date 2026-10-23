"""Exact, owner-fenced snapshot publication responsibilities.

Byte copying is outside the owner transaction. Pending and staged records remain
retaining roots until an authoritative handoff; no elapsed-time release exists.
Session commits hand off atomically to their durable head and completion record.
"""
import hashlib
import json

from django.db import connection
from django.utils import timezone

from .models import WorkspaceSnapshotPublication
from .resource_commit import require_durable_commit
from .runtime_contract import validate_session_workspace


class SnapshotPublicationError(Exception):
    code = "workspace_snapshot_publication_conflict"


def _digest(value):
    wire = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(wire).hexdigest()


def _identity(agent_run, session, body, candidate, storage_key, *, advance_session, authorization=None):
    validate_session_workspace(candidate)
    authorization = authorization if authorization is not None else agent_run.authorization
    if (agent_run.session_id != session.pk or agent_run.workspace_id != session.workspace_id
            or body["agentRunId"] != agent_run.pk or body["authorizationDigest"] != authorization.digest):
        raise SnapshotPublicationError
    purpose = "sessionCommit" if advance_session else "executionCheckpoint"
    checkpoint = "" if advance_session else body["checkpointId"]
    # Session commits can retry a newly captured candidate after a rejected frame.
    # Each immutable candidate keeps its own responsibility; a checkpoint ID names
    # one exact frame and must never be rebound to a different descriptor.
    purpose_key = _digest([candidate["generation"], candidate["snapshotSha256"]] if advance_session else checkpoint)
    fields = {"session_id": session.pk, "agent_run_id": agent_run.pk,
        "authorization_id": authorization.pk, "purpose": purpose, "purpose_key": purpose_key,
        "checkpoint_id": checkpoint, "storage_key": storage_key, "generation": candidate["generation"],
        "encoding": "frameV1", "snapshot_sha256": candidate["snapshotSha256"],
        "snapshot_size_bytes": candidate["snapshotSizeBytes"],
        "expanded_size_bytes": candidate["expandedSizeBytes"], "file_count": candidate["fileCount"]}
    fields["request_digest"] = "sha256:" + _digest(fields | {"authorizationDigest": authorization.digest})
    identity = "snapshot_publication_" + _digest([agent_run.pk, purpose, purpose_key])
    return identity, fields


def locked_publication(agent_run, session, body, candidate, storage_key, *, advance_session, create=False):
    """Caller holds Agent -> Session -> Run -> current Job, then locks this row."""
    if not connection.in_atomic_block:
        raise RuntimeError("snapshot publication requires owner transaction")
    require_durable_commit(connection, SnapshotPublicationError())
    identity, fields = _identity(agent_run, session, body, candidate, storage_key, advance_session=advance_session)
    record = WorkspaceSnapshotPublication.objects.select_for_update().filter(pk=identity).first()
    if record is None:
        if not create:
            raise SnapshotPublicationError
        return WorkspaceSnapshotPublication.objects.create(pk=identity, **fields)
    if any(getattr(record, name) != value for name, value in fields.items()):
        raise SnapshotPublicationError
    if record.state not in {"pending", "staged", "committed"}:
        raise SnapshotPublicationError
    return record


def confirm_publication(record, *, advance_session):
    expected = "committed" if advance_session else "staged"
    if record.state == expected:
        return
    if record.state != "pending":
        raise SnapshotPublicationError
    record.state = expected
    record.confirmed_at = timezone.now()
    record.save(update_fields=["state", "confirmed_at"])


def validate_record(record, session):
    """Validate metadata only; do not infer recovery or payload integrity."""
    if (record.session_id != session.pk or record.agent_run.session_id != session.pk
            or record.agent_run.workspace_id != session.workspace_id
            or record.authorization.agent_run_id != record.agent_run_id):
        raise SnapshotPublicationError
    candidate = {"generation": record.generation, "snapshotSha256": record.snapshot_sha256,
        "snapshotSizeBytes": record.snapshot_size_bytes, "expandedSizeBytes": record.expanded_size_bytes,
        "fileCount": record.file_count}
    body = {"agentRunId": record.agent_run_id, "authorizationDigest": record.authorization.digest,
        "checkpointId": record.checkpoint_id}
    identity, fields = _identity(record.agent_run, session, body, candidate, record.storage_key,
        advance_session=record.purpose == "sessionCommit", authorization=record.authorization)
    if record.pk != identity or any(getattr(record, name) != value for name, value in fields.items()):
        raise SnapshotPublicationError
    if record.state not in {"pending", "staged", "committed"}:
        raise SnapshotPublicationError
    if ((record.state == "pending") != (record.confirmed_at is None)
            or record.state == "committed" and record.purpose != "sessionCommit"
            or record.state == "staged" and record.purpose != "executionCheckpoint"):
        raise SnapshotPublicationError
    prefix = f"workspaces/{session.workspace_id}/sessions/{session.pk}"
    if not record.snapshot_size_bytes:
        expected = ""
    elif record.purpose == "sessionCommit":
        expected = f"{prefix}/snapshots/{record.generation}/{record.snapshot_sha256[7:]}.snapshot"
    else:
        checkpoint = hashlib.sha256(record.checkpoint_id.encode("utf-8")).hexdigest()
        expected = f"{prefix}/agent-runs/{record.agent_run_id}/execution-checkpoints/{checkpoint}/{record.snapshot_sha256[7:]}.snapshot"
    if record.storage_key != expected:
        raise SnapshotPublicationError


def blocks_deleted_owner_gc(session_id, storage_key, *, temporary=False):
    """Only unresolved publication duties block the existing explicit purge GC.

    Caller holds Agent then Session until deletion. Publication creators take
    the same owner fence, so no new responsibility can race this checked unlink.
    A committed Session publication is a durable handoff, not a permanent pin.
    """
    query = WorkspaceSnapshotPublication.objects.select_for_update().filter(
        session_id=session_id, state__in=["pending", "staged"])
    if temporary:
        query = query.filter(storage_key__startswith=storage_key.rsplit("/", 1)[0] + "/")
    else:
        query = query.filter(storage_key=storage_key)
    return query.exists()
