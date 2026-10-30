"""Reclaim unreachable Session bytes and permanently purged-owner payloads.

The upload and download paths take the same Agent and Session locks while creating a local
temporary file, publishing its immutable name, or opening an authorized reader.
Snapshot payload copying stays outside those transactions. Permanent deletion
therefore prevents new names from appearing after a GC pass has enumerated them;
an interrupted upload can leave only a temporary file for a later pass to reclaim.
Retained owners use Runtime-selected recovery roots and a shared relational witness;
purged owners retain their existing cutoff and exact-prefix cleanup policy.
"""

from dataclasses import dataclass
from functools import partial
from pathlib import Path
import os
import re
import stat

from django.core.files.storage import default_storage
from django.db import transaction
from django.db.models import Q

from .assets import delete_stored_object_for_gc
from .models import Agent, AgentRun, Session
from .storage_gc_retry import claim_storage_key, clean_storage_key
from . import workspace_observations as protection_observations


_GENERATION = re.compile(r"[1-9][0-9]*\Z")
_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
_SNAPSHOT_FILE = re.compile(r"[0-9a-f]{64}\.snapshot\Z")
_FILE_OBJECT = re.compile(r"[0-9a-f]{64}\.file\Z")
_FILE_PACK = re.compile(r"[0-9a-f]{32}\.pack\Z")
# tempfile.mkstemp currently uses eight lower-case alphanumeric/underscore
# characters. Keep unrelated temporary names outside this collector's scope.
_TEMPORARY_FILE = re.compile(r"\.centaeris-immutable-[a-z0-9_]{8}\.tmp\Z")


class WorkspaceSnapshotGcError(RuntimeError):
    pass


@dataclass(frozen=True)
class WorkspaceSnapshotGcReport:
    planned: list[str]
    cleaned: list[str]
    blocked: list[str]
    failures: list[str]


def collect_workspace_snapshot_gc(cutoff, dry_run: bool) -> WorkspaceSnapshotGcReport:
    report = _collect_purged_workspace_snapshot_gc(cutoff, dry_run)
    root = _local_storage_root()
    if root is not None:
        from .workspace_snapshot_reachability_gc import collect_reachable_workspace_gc
        collect_reachable_workspace_gc(root, cutoff, dry_run, report)
    return report


def _collect_purged_workspace_snapshot_gc(cutoff, dry_run: bool) -> WorkspaceSnapshotGcReport:
    """Collect canonical workspace snapshot and file objects of permanently deleted Sessions.

    Both the Session or parent Agent tombstone and object modification time must
    precede cutoff. A parent purge also covers children left active or in trash
    by earlier releases, without changing their state or recovery references.
    Unknown Run directories are reported as blocked, not interpreted as owned
    recovery data. This collector never accesses Runtime checkpoint tables.
    """
    root = _local_storage_root()
    report = WorkspaceSnapshotGcReport([], [], [], [])
    if root is None:
        return report
    sessions = Session.objects.filter(
        Q(status="deleted", purgedAt__lte=cutoff)
        | Q(agent__status="deleted", agent__purgedAt__lte=cutoff),
    ).only("id", "workspace_id", "agent_id")
    for session in sessions.order_by("id").iterator():
        try:
            prefix = _session_prefix(session)
        except WorkspaceSnapshotGcError as error:
            report.failures.append(f"session:{session.id}: {error}")
            continue
        for key in _snapshot_keys(root, prefix, session.id, report):
            try:
                # Persist the attempt before the deletion transaction. A crash
                # while unlinking must leave a reclaimable lease, not reset it.
                claim = None
                if not dry_run:
                    path = _checked_path(root, key, directory=False)
                    if path.stat().st_mtime > cutoff.timestamp():
                        report.blocked.append(key)
                        continue
                    # A parent's purge cannot reclaim a child that is still
                    # running. Do not spend attempts on this legitimate wait.
                    with transaction.atomic():
                        agent = Agent.objects.select_for_update().get(pk=session.agent_id)
                        owner = Session.objects.select_for_update().get(pk=session.id)
                        if not _owner_reclaimable(agent, owner, session, cutoff):
                            report.blocked.append(key)
                            continue
                        from .workspace_snapshot_publication import collect_key
                        object=collect_key(owner, key, kind=_object_kind(path.name))
                        token=object.publicationToken
                    claim = claim_storage_key(key, "workspaceSnapshot",publication_token=token)
                    if claim is None:
                        report.blocked.append(key)
                        continue
                # Match public deletion and snapshot I/O: parent before child.
                # Do not enumerate or copy payloads while holding these locks.
                with transaction.atomic():
                    agent = Agent.objects.select_for_update().only(
                        "status", "purgedAt", "workspace_id",
                    ).get(pk=session.agent_id)
                    owner = Session.objects.select_for_update().only(
                        "status", "purgedAt", "workspace_id", "agent_id",
                    ).get(pk=session.id)
                    if not _owner_reclaimable(agent, owner, session, cutoff):
                        report.blocked.append(key)
                        continue
                    try:
                        path = _checked_path(root, key, directory=False)
                    except FileNotFoundError:
                        # A concurrent collector or interrupted retry may have
                        # already removed precisely this object.
                        continue
                    if path.stat().st_mtime > cutoff.timestamp():
                        report.blocked.append(key)
                        continue
                    if dry_run:
                        report.planned.append(key)
                    else:
                        from .workspace_snapshot_publication import require_collected_incarnation
                        if not require_collected_incarnation(owner,key,token):
                            report.blocked.append(key)
                            continue
                        delete = partial(_delete_observed_snapshot, session_id=str(session.id))
                        result = clean_storage_key(key, "workspaceSnapshot", delete, claim=claim,publication_token=token)
                        if result.state == "cleaned":
                            report.cleaned.append(key)
                        elif result.state == "blocked":
                            report.blocked.append(key)
                        else:
                            report.failures.append(f"{key}: {result.failure}")
            except FileNotFoundError:
                # Uploader cleanup does not need the owner lock: after the
                # checked lstat it may independently unlink its temporary name.
                continue
            except Exception as error:
                report.failures.append(f"{key}: {error}")
    return report


def _delete_observed_snapshot(key, *, session_id):
    with protection_observations.operation("snapshotGc"):
        protection_observations.bind(sessionId=session_id)
        measured = False
        metadata = None
        if protection_observations.active():
            if _TEMPORARY_FILE.fullmatch(key.rsplit("/", 1)[-1]):
                # The uploader can still write its open FD or unlink its name
                # outside the owner lock. A stat cannot attribute reclaimed
                # bytes to this attempt; preserve that missing measurement.
                protection_observations.unavailable("gcReclaimedBytes")
            else:
                try:
                    metadata = os.stat(default_storage.path(key))
                    measured = True
                except FileNotFoundError:
                    measured = True
                except Exception:
                    protection_observations.unavailable("gcReclaimedBytes")
        with protection_observations.elapsed("gcElapsedMs"):
            try:
                delete_stored_object_for_gc(key)
            finally:
                if measured:
                    try:
                        remains = default_storage.exists(key)
                    except Exception:
                        protection_observations.unavailable("gcReclaimedBytes")
                    else:
                        # Count logical last-link bytes only. An open reader,
                        # sparse allocation or backend durability is not measured.
                        reclaimed = (metadata.st_size if metadata is not None
                                     and metadata.st_nlink == 1 and not remains else 0)
                        protection_observations.increment("gcReclaimedBytes", reclaimed)


def _owner_reclaimable(agent, owner, session, cutoff):
    session_purged = owner.status == "deleted" and owner.purgedAt is not None and owner.purgedAt <= cutoff
    agent_purged = agent.status == "deleted" and agent.purgedAt is not None and agent.purgedAt <= cutoff
    return (
        owner.workspace_id == session.workspace_id
        and owner.agent_id == agent.id
        and agent.workspace_id == owner.workspace_id
        and (session_purged or (agent_purged and not owner.agent_runs.filter(status__in={"queued", "running"}).exists()))
    )


def _session_prefix(session: Session) -> str:
    for value in (session.workspace_id, session.id):
        if not _safe_component(value):
            raise WorkspaceSnapshotGcError("workspace_snapshot_owner_path_invalid")
    return f"workspaces/{session.workspace_id}/sessions/{session.id}"


def _safe_component(value: str) -> bool:
    return (
        bool(value)
        and value not in {".", ".."}
        and "/" not in value
        and "\\" not in value
        and ":" not in value
        and "\x00" not in value
    )


def _local_storage_root() -> Path | None:
    try:
        root = Path(os.path.abspath(default_storage.path("")))
    except Exception as error:
        raise WorkspaceSnapshotGcError("workspace_snapshot_gc_requires_local_storage") from error
    # Resolve no aliases, including an alias in the configured root's ancestors.
    # A future object backend must provide a separate exact-version collector.
    current = Path(root.anchor)
    try:
        for part in root.parts[1:]:
            current /= part
            metadata = current.lstat()
            if _is_link(metadata) or not stat.S_ISDIR(metadata.st_mode):
                raise WorkspaceSnapshotGcError("workspace_snapshot_storage_root_invalid")
    except FileNotFoundError:
        # An installation without any file uploads need not have created its
        # local storage directory. Do not create one merely to collect no keys.
        return None
    except OSError as error:
        raise WorkspaceSnapshotGcError("workspace_snapshot_storage_root_unavailable") from error
    return root


def _is_link(metadata) -> bool:
    return stat.S_ISLNK(metadata.st_mode) or bool(
        getattr(metadata, "st_file_attributes", 0)
        & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
    )


def _checked_path(root: Path, key: str, *, directory: bool) -> Path:
    parts = key.split("/")
    if any(not _safe_component(part) for part in parts):
        raise WorkspaceSnapshotGcError("workspace_snapshot_gc_key_invalid")
    current = root
    for index, part in enumerate(parts):
        current /= part
        metadata = current.lstat()
        if _is_link(metadata):
            raise WorkspaceSnapshotGcError("workspace_snapshot_path_symlink")
        must_be_directory = index < len(parts) - 1 or directory
        valid = stat.S_ISDIR(metadata.st_mode) if must_be_directory else stat.S_ISREG(metadata.st_mode)
        if not valid:
            raise WorkspaceSnapshotGcError("workspace_snapshot_path_kind_invalid")
    return current


def _children(root: Path, prefix: str, report: WorkspaceSnapshotGcReport) -> list[str]:
    try:
        path = _checked_path(root, prefix, directory=True)
        with os.scandir(path) as entries:
            return sorted(entry.name for entry in entries)
    except FileNotFoundError:
        return []
    except Exception as error:
        report.failures.append(f"{prefix}: {error}")
        return []


def _object_kind(name):
    if _TEMPORARY_FILE.fullmatch(name):
        return "temporary"
    if _FILE_PACK.fullmatch(name):
        return "filePack"
    return "file" if _FILE_OBJECT.fullmatch(name) else "snapshot"


def _directory_files(root: Path, prefix: str, report: WorkspaceSnapshotGcReport):
    for name in _children(root, prefix, report):
        if _SNAPSHOT_FILE.fullmatch(name) or _TEMPORARY_FILE.fullmatch(name):
            yield f"{prefix}/{name}"


def _snapshot_keys(root: Path, prefix: str, session_id: str, report: WorkspaceSnapshotGcReport):
    files = f"{prefix}/files"
    for shard in _children(root, files, report):
        if len(shard) == 2 and all(char in "0123456789abcdef" for char in shard):
            for name in _children(root, f"{files}/{shard}", report):
                if ((_FILE_OBJECT.fullmatch(name) and name.startswith(shard))
                        or _TEMPORARY_FILE.fullmatch(name)):
                    yield f"{files}/{shard}/{name}"

    packs = f"{prefix}/file-packs"
    for name in _children(root, packs, report):
        if _FILE_PACK.fullmatch(name) or _TEMPORARY_FILE.fullmatch(name):
            yield f"{packs}/{name}"

    snapshots = f"{prefix}/snapshots"
    for generation in _children(root, snapshots, report):
        if _GENERATION.fullmatch(generation):
            yield from _directory_files(root, f"{snapshots}/{generation}", report)

    known_runs = set(AgentRun.objects.filter(session_id=session_id).values_list("id", flat=True))
    run_prefix = f"{prefix}/agent-runs"
    for run_id in _children(root, run_prefix, report):
        if run_id not in known_runs:
            path = f"{run_prefix}/{run_id}"
            try:
                _checked_path(root, path, directory=True)
            except FileNotFoundError:
                continue
            except Exception as error:
                report.failures.append(f"{path}: {error}")
            else:
                report.blocked.append(path)
            continue
        checkpoints = f"{run_prefix}/{run_id}/execution-checkpoints"
        for digest in _children(root, checkpoints, report):
            if _DIGEST.fullmatch(digest):
                yield from _directory_files(root, f"{checkpoints}/{digest}", report)
