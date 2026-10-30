"""Session-scoped immutable file content and indexed manifest membership.

Callers own authorization and Agent->Session locking. Only upload_file crosses
that boundary itself, invoking the supplied authority callback before accepting
bytes and again before publishing the verified private file.
"""
import hashlib
import os
from pathlib import Path
import tempfile
import uuid

from django.core.files.storage import default_storage
from django.db import connection, transaction
from django.db.models import F

from .assets import _sync_directory
from .models import (WorkspaceFileObject, WorkspaceFileReservation, WorkspaceSnapshotFileReference,
                     WorkspaceSnapshotObject, WorkspaceSnapshotUpload, StorageCleanupRetry)
from .resource_commit import require_durable_commit
from .runtime_contract import require_sha256
from .workspace_snapshot_publication import (_object, finish_upload, key_digest, mark_published,
                                             require_upload_live)
from . import workspace_observations as observations


FILE_MANIFEST_ENCODING = "fileManifestV1"
IO_BYTES = 64 * 1024


class WorkspaceFileError(ValueError):
    def __init__(self, code, status=409):
        super().__init__(code)
        self.code = code
        self.status = status


def _error(code="workspace_file_invalid", status=409):
    return WorkspaceFileError(code, status)


def file_key(session, sha256):
    require_sha256("workspace file sha256", sha256)
    digest = sha256[7:]
    return f"workspaces/{session.workspace_id}/sessions/{session.pk}/files/{digest[:2]}/{digest}.file"


def _descriptors(files):
    by_hash = {}
    if not isinstance(files, list):
        raise _error(status=400)
    for file in files:
        if not isinstance(file, dict) or set(file) != {"sha256", "sizeBytes"}:
            raise _error(status=400)
        try:
            require_sha256("workspace file sha256", file["sha256"])
        except ValueError:
            raise _error(status=400) from None
        if type(file["sizeBytes"]) is not int or file["sizeBytes"] < 0:
            raise _error(status=400)
        previous = by_hash.setdefault(file["sha256"], file)
        if previous["sizeBytes"] != file["sizeBytes"]:
            raise _error(status=400)
    return list(by_hash.values())


def _manifest_files(manifest):
    return _descriptors([{"sha256": file["sha256"], "sizeBytes": file["sizeBytes"]}
                         for file in manifest["files"]])


def reserve_files(session, run, body, files):
    """Reserve all reused/new content under the caller's current authority fence."""
    require_durable_commit(connection, _error("session_workspace_durability_required"))
    descriptors = _descriptors(files)
    keys = {key_digest(file_key(session, file["sha256"])): (file_key(session, file["sha256"]), file)
            for file in descriptors}
    registered = {obj.pk: obj for obj in WorkspaceSnapshotObject.objects.filter(pk__in=keys)}
    new = [WorkspaceSnapshotObject(keyDigest=digest, storageKey=key, session=session,
                                  kind="file", registered=True)
           for digest, (key, _file) in keys.items() if digest not in registered]
    WorkspaceSnapshotObject.objects.bulk_create(new)
    registered.update((obj.pk, obj) for obj in new)
    cleaned = set(StorageCleanupRetry.objects.filter(pk__in=keys, collector="workspaceSnapshot", state="cleaned")
                  .values_list("pk", flat=True))
    for digest, obj in registered.items():
        key = keys[digest][0]
        if obj.storageKey != key or obj.session_id != session.pk or obj.kind != "file":
            raise _error("session_workspace_publication_identity_invalid")
        if obj.state != "live" or digest in cleaned:
            registered[digest] = _object(session, run, key, "file")
        elif not obj.registered:
            obj.registered = True
            obj.save(update_fields=["registered", "updatedAt"])
    contents = {file.pk: file for file in WorkspaceFileObject.objects.filter(pk__in=keys)}
    missing_metadata = [WorkspaceFileObject(object=registered[digest], sha256=descriptor["sha256"],
                                           sizeBytes=descriptor["sizeBytes"])
                        for digest, (_key, descriptor) in keys.items() if digest not in contents]
    WorkspaceFileObject.objects.bulk_create(missing_metadata)
    contents.update((file.pk, file) for file in missing_metadata)
    reservations = []
    missing = []
    for digest, (_key, descriptor) in keys.items():
        file = contents[digest]
        obj = registered[digest]
        if file.sha256 != descriptor["sha256"] or file.sizeBytes != descriptor["sizeBytes"]:
            raise _error("workspace_file_identity_invalid")
        reservations.append(WorkspaceFileReservation(file=file, agent_run=run, jobId=body["jobId"],
            leaseOwner=body["leaseOwner"], authorizationDigest=body["authorizationDigest"],
            publicationToken=obj.publicationToken))
        if file.verifiedPublicationToken != obj.publicationToken:
            missing.append(descriptor)
    WorkspaceFileReservation.objects.bulk_create(reservations, update_conflicts=True,
        unique_fields=["file", "jobId", "leaseOwner"],
        update_fields=["agent_run", "authorizationDigest", "publicationToken"])
    return missing


def _reserved_file(session, run, body, descriptor):
    descriptor = _descriptors([descriptor])[0]
    file = WorkspaceFileObject.objects.select_related("object").filter(
        pk=key_digest(file_key(session, descriptor["sha256"])), object__session=session,
        object__kind="file", object__state="live", sha256=descriptor["sha256"],
        sizeBytes=descriptor["sizeBytes"], reservations__agent_run=run,
        reservations__jobId=body["jobId"], reservations__leaseOwner=body["leaseOwner"],
        reservations__authorizationDigest=body["authorizationDigest"],
        reservations__publicationToken=F("object__publicationToken")).first()
    if file is None:
        raise _error("workspace_file_reservation_lost")
    return file


def upload_file(source, body, descriptor, authorize):
    """Accept one body, hash while writing once, then publish under fresh authority.

    authorize(body) runs inside transaction.atomic and returns (run, session, ...).
    It owns the actual membership/controller/job semantics, including cleanup lanes.
    """
    descriptor = _descriptors([descriptor])[0]
    temporary_path = None
    upload_id = None
    fd = None
    try:
        with transaction.atomic():
            run, session, *_ = authorize(body)
            require_durable_commit(connection, _error("session_workspace_durability_required"))
            file = _reserved_file(session, run, body, descriptor)
            final = file.object
            final_path = Path(default_storage.path(final.storageKey))
            final_path.parent.mkdir(parents=True, exist_ok=True)
            fd, temporary_path = tempfile.mkstemp(prefix=".centaeris-immutable-", suffix=".tmp", dir=final_path.parent)
            temporary_key = Path(temporary_path).relative_to(Path(default_storage.path(""))).as_posix()
            temporary = _object(session, run, temporary_key, "temporary")
            upload_id = WorkspaceSnapshotUpload.objects.create(id=uuid.uuid4().hex,
                finalObject=final, temporaryObject=temporary, agent_run=run,
                jobId=body["jobId"], leaseOwner=body["leaseOwner"],
                authorizationDigest=body["authorizationDigest"], finalPublicationToken=final.publicationToken,
                temporaryPublicationToken=temporary.publicationToken).pk
        digest = hashlib.sha256()
        remaining = descriptor["sizeBytes"]
        opened_fd, fd = fd, None
        with os.fdopen(opened_fd, "wb") as output:
            while remaining:
                chunk = source.read(min(remaining, IO_BYTES))
                observations.increment("apiReceivedBytes", len(chunk))
                if not chunk or len(chunk) > remaining:
                    raise _error(status=400)
                output.write(chunk)
                observations.increment("apiWrittenBytes", len(chunk))
                digest.update(chunk)
                remaining -= len(chunk)
            extra = source.read(1)
            observations.increment("apiReceivedBytes", len(extra))
            if extra or "sha256:" + digest.hexdigest() != descriptor["sha256"]:
                raise _error(status=400)
            output.flush()
            os.fsync(output.fileno())
        with transaction.atomic():
            run, session, *_ = authorize(body)
            file = _reserved_file(session, run, body, descriptor)
            require_upload_live(upload_id, file.object.storageKey)
            if file.verifiedPublicationToken != file.object.publicationToken:
                os.replace(temporary_path, default_storage.path(file.object.storageKey))
                _sync_directory(str(Path(temporary_path).parent))
                file.verifiedPublicationToken = file.object.publicationToken
                file.save(update_fields=["verifiedPublicationToken"])
            mark_published(upload_id, "committed")
        return descriptor
    finally:
        if fd is not None:
            os.close(fd)
        if temporary_path is not None:
            try:
                os.unlink(temporary_path)
            except FileNotFoundError:
                pass
        if upload_id is not None:
            finish_upload(upload_id)


def require_manifest_files(session, run, body, manifest):
    """Check immutable file identities and reservations without rereading bodies."""
    descriptors = _manifest_files(manifest)
    keys = {key_digest(file_key(session, descriptor["sha256"])): descriptor for descriptor in descriptors}
    files = list(WorkspaceFileObject.objects.select_related("object").filter(
        pk__in=keys, object__session=session, object__kind="file", object__state="live",
        verifiedPublicationToken=F("object__publicationToken"), reservations__agent_run=run,
        reservations__jobId=body["jobId"], reservations__leaseOwner=body["leaseOwner"],
        reservations__authorizationDigest=body["authorizationDigest"],
        reservations__publicationToken=F("object__publicationToken")))
    if len(files) != len(keys) or any(
            file.sha256 != keys[file.pk]["sha256"] or file.sizeBytes != keys[file.pk]["sizeBytes"] for file in files):
        raise _error("workspace_manifest_file_unavailable")
    return files


def register_snapshot_files(snapshot_object, files):
    """Publish edges in the same transaction as the immutable snapshot reference."""
    if snapshot_object.kind != "snapshot" or snapshot_object.state != "live":
        raise _error("session_workspace_publication_collected")
    if any(file.object.session_id != snapshot_object.session_id for file in files):
        raise _error("workspace_file_identity_invalid")
    WorkspaceSnapshotFileReference.objects.bulk_create([
        WorkspaceSnapshotFileReference(snapshot=snapshot_object, file=file) for file in files],
        ignore_conflicts=True)


def open_file(session, sha256):
    """Open after the caller proved a selected snapshot/reference or reservation."""
    file = WorkspaceFileObject.objects.select_related("object").filter(
        pk=key_digest(file_key(session, sha256)), object__session=session, object__state="live",
        verifiedPublicationToken=F("object__publicationToken")).first()
    if file is None:
        raise _error("workspace_file_unavailable", 404)
    handle = default_storage.open(file.object.storageKey, "rb")
    if os.fstat(handle.fileno()).st_size != file.sizeBytes:
        handle.close()
        raise _error("workspace_file_identity_invalid")
    return handle


def referenced_file_keys(snapshot_keys):
    return set(WorkspaceSnapshotFileReference.objects.filter(snapshot__storageKey__in=snapshot_keys)
               .values_list("file__object__storageKey", flat=True))


def live_reservation_keys(session):
    """Only the current canonical lifecycle lease roots a file reservation."""
    with connection.cursor() as cursor:
        cursor.execute('SELECT o."storageKey" FROM public.app_core_workspacefilereservation r '
            'JOIN public.app_core_workspacefileobject f ON f.object_id=r.file_id '
            'JOIN public.app_core_workspacesnapshotobject o ON o."keyDigest"=f.object_id '
            'JOIN runtime.runtime_jobs j ON j.job_id=r."jobId" '
            'WHERE o.session_id=%s AND o.state=\'live\' AND r."publicationToken"=o."publicationToken" '
            'AND j.job_kind=\'agent_run.lifecycle\' AND j.status IN (\'leased\',\'running\') '
            'AND j.lease_purpose IN (\'execute\',\'control\') '
            'AND j.lease_owner=r."leaseOwner" '
            'AND j.lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint '
            'AND j.session_id=%s AND j.payload_ref=\'record:agent_run:\'||r.agent_run_id '
            'AND j.idempotency_key=j.job_id||\':\'||r."authorizationDigest"', [session.pk, session.pk])
        return {row[0] for row in cursor.fetchall()}
