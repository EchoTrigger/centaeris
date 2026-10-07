"""Reclaim local workspace bytes after their Session or Agent is permanently deleted.

The upload and download paths take the same Agent and Session locks while creating a local
temporary file, publishing its immutable name, or opening an authorized reader.
Snapshot payload copying stays outside those transactions. Permanent deletion
therefore prevents new names from appearing after a GC pass has enumerated them;
an interrupted upload can leave only a temporary file for a later pass to reclaim.
Active Sessions and restorable trash under retained Agents never enter this collector.
"""

from dataclasses import dataclass
from pathlib import Path
import os
import re
import stat

from django.core.files.storage import default_storage
from django.db import transaction
from django.db.models import Q

from .assets import delete_stored_object_for_gc
from .models import Agent, AgentRun, Session


_GENERATION = re.compile(r"[1-9][0-9]*\Z")
_DIGEST = re.compile(r"[0-9a-f]{64}\Z")
_SNAPSHOT_FILE = re.compile(r"[0-9a-f]{64}\.snapshot\Z")
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
    """Collect the two precise key families owned by permanently deleted Sessions.

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
                # Match public deletion and snapshot I/O: parent before child.
                # Do not enumerate or copy payloads while holding these locks.
                with transaction.atomic():
                    agent = Agent.objects.select_for_update().only(
                        "status", "purgedAt", "workspace_id",
                    ).get(pk=session.agent_id)
                    owner = Session.objects.select_for_update().only(
                        "status", "purgedAt", "workspace_id", "agent_id",
                    ).get(pk=session.id)
                    session_purged = (
                        owner.status == "deleted"
                        and owner.purgedAt is not None
                        and owner.purgedAt <= cutoff
                    )
                    agent_purged = (
                        agent.status == "deleted"
                        and agent.purgedAt is not None
                        and agent.purgedAt <= cutoff
                    )
                    if (
                        owner.workspace_id != session.workspace_id
                        or owner.agent_id != agent.id
                        or agent.workspace_id != owner.workspace_id
                        or not (
                            session_purged
                            or (
                                agent_purged
                                and not owner.agent_runs.filter(status__in={"queued", "running"}).exists()
                            )
                        )
                    ):
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
                        delete_stored_object_for_gc(key)
                        report.cleaned.append(key)
            except FileNotFoundError:
                # Uploader cleanup does not need the owner lock: after the
                # checked lstat it may independently unlink its temporary name.
                continue
            except Exception as error:
                report.failures.append(f"{key}: {error}")
    return report


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


def _directory_files(root: Path, prefix: str, report: WorkspaceSnapshotGcReport):
    for name in _children(root, prefix, report):
        if _SNAPSHOT_FILE.fullmatch(name) or _TEMPORARY_FILE.fullmatch(name):
            yield f"{prefix}/{name}"


def _snapshot_keys(root: Path, prefix: str, session_id: str, report: WorkspaceSnapshotGcReport):
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
