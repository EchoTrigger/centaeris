"""Read-only observations of existing Session snapshot references.

This inventory is not a GC fence or an authorization decision. It deliberately
retains terminal Run grants: their status alone does not settle recovery or a
lost commit reply. It neither enumerates payload bytes nor declares other keys
unreachable. Missing write/pin/completion indexes remain explicit gaps.
"""
from dataclasses import dataclass, replace
import hashlib
import json
import unicodedata

from django.db import DatabaseError, connection, transaction

from .models import AgentRunAuthorization, Session, WorkspaceSnapshotPublication
from .runtime_contract import require_opaque_ref, require_sha256, session_workspace_for_session, validate_session_workspace
from .workspace_snapshot_gc import _safe_component, _session_prefix


@dataclass(frozen=True)
class SnapshotProtectionRoot:
    storage_key: str
    source_kind: str
    source_id: str
    agent_run_id: str | None


@dataclass(frozen=True)
class SnapshotProtectionInventory:
    session_id: str
    roots: tuple[SnapshotProtectionRoot, ...]
    unresolved: tuple[str, ...]


_OBSERVATION_GAPS = frozenset({
    "concurrentReferencesNotFenced", "inflightWorkspaceUploadsNotIndexed",
    "historicalPinsNotIndexed", "preparedCompletionsNotIndexed",
})


def _snapshot_key(prefix, value):
    validate_session_workspace(value)
    if value["snapshotSizeBytes"] == 0:
        return None
    return (f"{prefix}/snapshots/{value['generation']}/"
            f"{value['snapshotSha256'][7:]}.snapshot")


_CHECKPOINT_FIELDS = frozenset({"schema", "checkpointId", "sessionId", "agentRunId", "executionId",
    "authorizationDigest", "sessionSequence", "modelRequestId", "workspaceSnapshot", "workspaceGeneration", "createdAtMs"})
_SNAPSHOT_FIELDS = frozenset({"objectRef", "snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount"})


def _exact_fields(value, fields):
    if not isinstance(value, dict) or value.keys() != fields:
        raise ValueError("checkpoint fields differ from the Core v1 contract")


def _rust_whitespace(char):
    # Python additionally classifies these four information separators as space;
    # Rust trim uses Unicode White_Space and must retain them.
    return char.isspace() and char not in "\x1c\x1d\x1e\x1f"

def _identity(value):
    # Core validate_required_identity permits internal punctuation; this is not
    # an API opaque-ref or filesystem path. UTF-8 also rejects lone surrogates.
    if (not isinstance(value, str) or not value
            or _rust_whitespace(value[0]) or _rust_whitespace(value[-1])):
        raise ValueError("checkpoint identity invalid")
    value.encode("utf-8")


def _unsigned(value, bits, *, minimum=0):
    if type(value) is not int or not minimum <= value < 2**bits:
        raise ValueError("checkpoint unsigned integer invalid")


def _validate_workspace_generation(value):
    # Core execution::ExecutionWorkspaceGeneration is a closed tagged enum.
    if not isinstance(value, dict):
        raise ValueError("workspace generation invalid")
    if value.get("status") == "known":
        _exact_fields(value, {"status", "token"})
        token = value["token"]
        _exact_fields(token, {"instanceEpoch", "generation"})
        epoch = token["instanceEpoch"]
        if (not isinstance(epoch, str) or not epoch or len(epoch.encode("utf-8")) > 160
                or any(unicodedata.category(char) == "Cc" for char in epoch)):
            raise ValueError("workspace generation epoch invalid")
        _unsigned(token["generation"], 64)
    elif value.get("status") == "unknown":
        _exact_fields(value, {"status", "reason"})
        reason = value["reason"]
        if not isinstance(reason, str) or not any(not _rust_whitespace(char) for char in reason):
            raise ValueError("workspace generation reason invalid")
        reason.encode("utf-8")
    else:
        raise ValueError("workspace generation status invalid")


def _validate_recovery_snapshot(value):
    # Serde's Option<String> permits omission on input. Normalize its null
    # value exactly as Core serialization does; a nonempty snapshot still fails.
    if isinstance(value, dict) and value.keys() == _SNAPSHOT_FIELDS - {"objectRef"}:
        value["objectRef"] = None
    _exact_fields(value, _SNAPSHOT_FIELDS)
    for field, bits in (("snapshotSizeBytes", 64), ("expandedSizeBytes", 64), ("fileCount", 32)):
        _unsigned(value[field], bits)
    if (value["objectRef"] is None and value["snapshotSha256"] == ""
            and value["snapshotSizeBytes"] == 0 and value["expandedSizeBytes"] == 0 and value["fileCount"] == 0):
        return
    _identity(value["objectRef"])
    require_sha256("snapshotSha256", value["snapshotSha256"])
    if value["snapshotSizeBytes"] == 0 or value["fileCount"] == 0:
        raise ValueError("nonempty recovery snapshot shape invalid")


def _unique_json_fields(pairs):
    result = {}
    for name, value in pairs:
        if name in result:
            raise ValueError("duplicate checkpoint JSON field")
        result[name] = value
    return result


def _invalid_json_constant(_value):
    raise ValueError("nonfinite checkpoint JSON number")


def _read_checkpoint(stored_json):
    # Shape and scalar parity with Core RuntimeRecoveryCheckpointV1, not proof
    # of a committed frontier, unused recovery attempt, or executable recovery.
    value = json.loads(stored_json, object_pairs_hook=_unique_json_fields, parse_constant=_invalid_json_constant)
    _exact_fields(value, _CHECKPOINT_FIELDS)
    if value["schema"] != "runtime.recovery_checkpoint.v1":
        raise ValueError("checkpoint schema invalid")
    for field in ("checkpointId", "sessionId", "agentRunId", "executionId", "modelRequestId"):
        _identity(value[field])
    require_sha256("authorizationDigest", value["authorizationDigest"])
    _unsigned(value["sessionSequence"], 64, minimum=1)
    if type(value["createdAtMs"]) is not int or not 0 <= value["createdAtMs"] < 2**63:
        raise ValueError("checkpoint timestamp invalid")
    _validate_recovery_snapshot(value["workspaceSnapshot"])
    _validate_workspace_generation(value["workspaceGeneration"])
    return value

def snapshot_protection_for_session(session, *, authorizations, recovery_checkpoints):
    """Observe persisted records without mutating them or reading file bodies.

    ``recovery_checkpoints`` contains (checkpoint ID, stored JSON) pairs, or None
    when that source could not be read. All recorded Run grants are retained;
    this reports their file references without replacing signature validation.
    Repeated keys keep each independent retaining source.
    """
    prefix = _session_prefix(session)
    roots = []
    unresolved = set(_OBSERVATION_GAPS)
    try:
        key = _snapshot_key(prefix, session_workspace_for_session(session))
        if (key or "") != session.workspaceStorageKey:
            raise ValueError("snapshot key differs from its descriptor")
        if key:
            roots.append(SnapshotProtectionRoot(key, "sessionHead", str(session.id), None))
    except (AttributeError, TypeError, ValueError):
        unresolved.add("sessionHeadInvalid")

    grants_by_run = {}
    for grant in authorizations:
        try:
            run = grant.agent_run
            if (run.session_id != session.id or run.workspace_id != session.workspace_id
                    or not _safe_component(run.id)
                    or grant.payload["workspaceId"] != session.workspace_id
                    or grant.payload["sessionId"] != session.id
                    or grant.payload["agentRunId"] != run.id):
                raise ValueError("snapshot grant scope differs")
            require_sha256("authorizationDigest", grant.digest)
            key = _snapshot_key(prefix, grant.payload["sessionWorkspace"])
            grants_by_run[run.id] = grant
            if key:
                roots.append(SnapshotProtectionRoot(key, "runAuthorization", str(grant.id), str(run.id)))
        except (AttributeError, KeyError, TypeError, ValueError):
            unresolved.add("runAuthorizationInvalid:" + str(grant.id))

    if recovery_checkpoints is None:
        unresolved.add("runtimeCheckpointsUnavailable")
    else:
        for checkpoint_id, stored_json in recovery_checkpoints:
            try:
                _identity(checkpoint_id)
                payload = _read_checkpoint(stored_json)
                grant = grants_by_run[payload["agentRunId"]]
                if (payload["schema"] != "runtime.recovery_checkpoint.v1"
                        or payload["checkpointId"] != checkpoint_id
                        or payload["sessionId"] != session.id
                        or payload["authorizationDigest"] != grant.digest):
                    raise ValueError("snapshot checkpoint scope differs")
                snapshot = payload["workspaceSnapshot"]
                value = {"generation": 1, **{name: snapshot[name] for name in (
                    "snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount")}}
                object_ref = snapshot["objectRef"]
                if isinstance(object_ref, str) and object_ref.startswith("session-workspace:generation:"):
                    frozen = grant.payload["sessionWorkspace"]
                    expected_snapshot = {name: frozen[name] for name in (
                        "snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount")}
                    expected_snapshot["objectRef"] = (f"session-workspace:generation:{frozen['generation']}"
                        if frozen["snapshotSizeBytes"] else None)
                    if snapshot != expected_snapshot:
                        raise ValueError("checkpoint differs from its frozen Session workspace")
                    frozen_key = _snapshot_key(prefix, frozen)
                    if frozen_key:
                        roots.append(SnapshotProtectionRoot(frozen_key, "recoveryCheckpoint", checkpoint_id,
                                                            str(grant.agent_run.id)))
                    continue
                key = _snapshot_key(prefix, value)
                if key is None:
                    if snapshot["objectRef"] is not None:
                        raise ValueError("empty snapshot has an object")
                    continue
                checkpoint_digest = hashlib.sha256(checkpoint_id.encode("utf-8")).hexdigest()
                expected = (f"{prefix}/agent-runs/{grant.agent_run.id}/execution-checkpoints/"
                            f"{checkpoint_digest}/{value['snapshotSha256'][7:]}.snapshot")
                if snapshot["objectRef"] != expected:
                    raise ValueError("snapshot checkpoint object differs")
                roots.append(SnapshotProtectionRoot(expected, "recoveryCheckpoint", checkpoint_id,
                                                    str(grant.agent_run.id)))
            except (AttributeError, KeyError, TypeError, ValueError, RecursionError):
                unresolved.add("recoveryCheckpointInvalid:" + str(checkpoint_id))

    return SnapshotProtectionInventory(str(session.id),
        tuple(sorted(roots, key=lambda root: (root.source_kind, root.source_id, root.storage_key))),
        tuple(sorted(unresolved)))


def collect_session_workspace_protection(session_id, *, max_records=1000):
    """Read bounded known roots for one Session, never authorize deletion.

    Each history source has its own record budget. Truncation is explicit, not
    evidence of absent roots. No owner locks or file storage operations occur.
    Concurrent changes can make observations disagree.
    """
    require_opaque_ref("sessionId", session_id)
    if type(max_records) is not int or not 1 <= max_records <= 10000:
        raise ValueError("snapshot protection record budget must be 1..10000")
    session = Session.objects.get(pk=session_id)
    grants = list(AgentRunAuthorization.objects.filter(agent_run__session_id=session_id)
        .select_related("agent_run").only("id", "digest", "payload", "agent_run__id",
            "agent_run__session_id", "agent_run__workspace_id").order_by("id")[:max_records + 1]
        .iterator(chunk_size=min(200, max_records + 1)))
    gaps = set()
    if len(grants) > max_records:
        gaps.add("runAuthorizationsTruncated")
        grants.pop()
    try:
        # Isolate a missing/incompatible Runtime table from the caller's outer
        # transaction. This savepoint writes no application records.
        with transaction.atomic():
            with connection.cursor() as cursor:
                cursor.execute("SELECT checkpoint_id, payload_json FROM runtime.checkpoints "
                    "WHERE session_id=%s AND kind='recovery' ORDER BY checkpoint_id LIMIT %s",
                    [session_id, max_records + 1])
                checkpoints = cursor.fetchmany(max_records + 1)
        if len(checkpoints) > max_records:
            gaps.add("runtimeCheckpointsTruncated")
            checkpoints.pop()
    except DatabaseError:
        checkpoints = None
    inventory = snapshot_protection_for_session(session, authorizations=grants, recovery_checkpoints=checkpoints)
    roots = list(inventory.roots)
    from .workspace_snapshot_publication import SnapshotPublicationError, validate_record
    try:
        with transaction.atomic():
            publications = list(WorkspaceSnapshotPublication.objects.filter(session_id=session_id)
                .select_related("agent_run", "authorization")
                .only("id", "session_id", "agent_run_id", "authorization_id", "purpose", "purpose_key",
                    "checkpoint_id", "storage_key", "request_digest", "generation", "encoding",
                    "snapshot_sha256", "snapshot_size_bytes", "expanded_size_bytes", "file_count",
                    "state", "created_at", "confirmed_at", "agent_run__id", "agent_run__session_id",
                    "agent_run__workspace_id", "authorization__id", "authorization__digest", "authorization__agent_run_id")
                .order_by("id")[:max_records + 1])
        if len(publications) > max_records:
            gaps.add("snapshotPublicationsTruncated")
            publications.pop()
        for record in publications:
            try:
                validate_record(record, session)
                if record.storage_key and record.state != "committed":
                    roots.append(SnapshotProtectionRoot(record.storage_key, "snapshotPublication",
                                                       record.pk, record.agent_run_id))
                if record.state != "committed":
                    gaps.add("snapshotPublicationUnconfirmed:" + record.pk)
            except (SnapshotPublicationError, AttributeError, KeyError, TypeError, ValueError):
                gaps.add("snapshotPublicationInvalid:" + record.pk)
    except DatabaseError:
        gaps.add("snapshotPublicationsUnavailable")
    # Old in-flight writes cannot be reconstructed from object names or mtimes.
    # Keep the existing coverage gap until a separately verified cutover exists.
    return replace(inventory,
        roots=tuple(sorted(roots, key=lambda root: (root.source_kind, root.source_id, root.storage_key))),
        unresolved=tuple(sorted(set(inventory.unresolved) | gaps)))