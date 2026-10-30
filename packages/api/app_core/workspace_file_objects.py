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
from django.db.models import F, Q, Case, When, Value
from django.utils import timezone

from .assets import _sync_directory
from .models import (Agent, Session, WorkspaceFileObject, WorkspaceFileReservation, WorkspaceSnapshotFileReference,
                     WorkspaceSnapshotObject, WorkspaceSnapshotUpload, StorageCleanupRetry)
from .resource_commit import require_durable_commit
from .runtime_contract import require_sha256
from .workspace_snapshot_publication import _object, key_digest
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


def ordered_descriptors(files, *, unique=False):
    validated = _descriptors(files)
    if unique and len(validated) != len(files):
        raise _error(status=400)
    return list(files)


def _reserved_files(session, run, body, descriptors):
    keys = {key_digest(file_key(session, item["sha256"])): item for item in descriptors}
    found = list(WorkspaceFileObject.objects.select_related("object").filter(
        pk__in=keys, object__session=session, object__kind="file", object__state="live",
        reservations__agent_run=run, reservations__jobId=body["jobId"],
        reservations__leaseOwner=body["leaseOwner"],
        reservations__authorizationDigest=body["authorizationDigest"],
        reservations__publicationToken=F("object__publicationToken")))
    if len(found) != len(keys) or any(
            file.sha256 != keys[file.pk]["sha256"] or file.sizeBytes != keys[file.pk]["sizeBytes"] for file in found):
        raise _error("workspace_file_reservation_lost")
    return {file.sha256: file for file in found}


def _publication_lost():
    from .http.internal import SessionWorkspaceError
    return SessionWorkspaceError("session_workspace_publication_collected")


def _finish_file_uploads(entries, owner):
    absent = []
    cleanup_error = None
    for entry in entries:
        if entry["fd"] is not None:
            os.close(entry["fd"])
            entry["fd"] = None
        try:
            os.unlink(entry["path"])
        except FileNotFoundError:
            absent.append(entry)
        except OSError as error:
            cleanup_error = cleanup_error or error
        else:
            absent.append(entry)
    uploads = [entry["upload"] for entry in entries if entry.get("upload") is not None]
    if uploads:
        with transaction.atomic():
            Agent.objects.select_for_update().get(pk=owner.agent_id)
            Session.objects.select_for_update().get(pk=owner.pk)
            now = timezone.now()
            WorkspaceSnapshotUpload.objects.filter(pk__in=[upload.pk for upload in uploads]).update(
                state=Case(When(state="streaming", then=Value("abandoned")), default=F("state")),
                finishedAt=now, updatedAt=now)
            matching = Q(pk__in=[])
            for entry in absent:
                temporary = entry.get("temporary")
                if temporary is not None:
                    matching |= Q(pk=temporary.pk, publicationToken=temporary.publicationToken)
            WorkspaceSnapshotObject.objects.filter(matching, state="live").update(
                state="collected", collectedAt=now, updatedAt=now)
    observations.gauge("temporaryBytes", 0)
    if cleanup_error is not None:
        raise cleanup_error


def upload_files(source, body, descriptors, authorize):
    """Receive ordered bodies under one request fence and one publication fence."""
    descriptors = ordered_descriptors(descriptors, unique=True)
    entries = []
    owner = None
    try:
        with transaction.atomic():
            run, owner, *_ = authorize(body)
            require_durable_commit(connection, _error("session_workspace_durability_required"))
            files = _reserved_files(owner, run, body, descriptors)
            temporaries, uploads = [], []
            for descriptor in descriptors:
                file = files[descriptor["sha256"]]
                final_path = Path(default_storage.path(file.object.storageKey))
                final_path.parent.mkdir(parents=True, exist_ok=True)
                fd, path = tempfile.mkstemp(prefix=".centaeris-immutable-", suffix=".tmp", dir=final_path.parent)
                temporary_key = Path(path).relative_to(Path(default_storage.path(""))).as_posix()
                temporary = WorkspaceSnapshotObject(keyDigest=key_digest(temporary_key), storageKey=temporary_key,
                    session=owner, kind="temporary", registered=True)
                upload = WorkspaceSnapshotUpload(id=uuid.uuid4().hex, finalObject=file.object,
                    temporaryObject=temporary, agent_run=run, jobId=body["jobId"], leaseOwner=body["leaseOwner"],
                    authorizationDigest=body["authorizationDigest"], finalPublicationToken=file.object.publicationToken,
                    temporaryPublicationToken=temporary.publicationToken)
                entries.append({"descriptor": descriptor, "fd": fd, "path": path,
                                "temporary": temporary, "upload": upload})
                temporaries.append(temporary)
                uploads.append(upload)
            WorkspaceSnapshotObject.objects.bulk_create(temporaries)
            WorkspaceSnapshotUpload.objects.bulk_create(uploads)
        temporary_bytes = 0
        for entry in entries:
            descriptor = entry["descriptor"]
            digest = hashlib.sha256()
            remaining = descriptor["sizeBytes"]
            fd, entry["fd"] = entry["fd"], None
            with os.fdopen(fd, "wb") as output:
                while remaining:
                    chunk = source.read(min(remaining, IO_BYTES))
                    observations.increment("apiReceivedBytes", len(chunk))
                    if not chunk or len(chunk) > remaining:
                        raise _error(status=400)
                    output.write(chunk)
                    observations.increment("apiWrittenBytes", len(chunk))
                    digest.update(chunk)
                    remaining -= len(chunk)
                if "sha256:" + digest.hexdigest() != descriptor["sha256"]:
                    raise _error(status=400)
                output.flush()
                os.fsync(output.fileno())
            temporary_bytes += descriptor["sizeBytes"]
            observations.gauge("temporaryBytes", temporary_bytes)
        extra = source.read(1)
        observations.increment("apiReceivedBytes", len(extra))
        if extra:
            raise _error(status=400)
        with transaction.atomic():
            run, session, *_ = authorize(body)
            files = _reserved_files(session, run, body, descriptors)
            uploads = {upload.pk: upload for upload in WorkspaceSnapshotUpload.objects.select_related(
                "finalObject", "temporaryObject").filter(pk__in=[entry["upload"].pk for entry in entries])}
            cleaned = set(StorageCleanupRetry.objects.filter(pk__in=[file.pk for file in files.values()],
                collector="workspaceSnapshot", state="cleaned").values_list("pk", "publicationToken"))
            # Validate the entire batch before the first immutable rename.
            for entry in entries:
                file = files[entry["descriptor"]["sha256"]]
                upload = uploads.get(entry["upload"].pk)
                if (upload is None or upload.state == "abandoned"
                        or upload.finalObject_id != file.pk
                        or upload.finalObject.state != "live" or upload.temporaryObject.state != "live"
                        or upload.finalPublicationToken != file.object.publicationToken
                        or upload.temporaryPublicationToken != upload.temporaryObject.publicationToken
                        or (file.pk, file.object.publicationToken) in cleaned):
                    raise _publication_lost()
            changed = []
            directories = set()
            for entry in entries:
                file = files[entry["descriptor"]["sha256"]]
                if file.verifiedPublicationToken != file.object.publicationToken:
                    os.replace(entry["path"], default_storage.path(file.object.storageKey))
                    directories.add(str(Path(entry["path"]).parent))
                    file.verifiedPublicationToken = file.object.publicationToken
                    changed.append(file)
            for directory in directories:
                _sync_directory(directory)
            WorkspaceFileObject.objects.bulk_update(changed, ["verifiedPublicationToken"])
            WorkspaceSnapshotUpload.objects.filter(pk__in=uploads).update(state="committed", updatedAt=timezone.now())
        return descriptors
    finally:
        _finish_file_uploads(entries, owner)


def upload_file(source, body, descriptor, authorize):
    """Private single-file convenience; the HTTP contract always uses batches."""
    return upload_files(source, body, [descriptor], authorize)[0]


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


class FileSequence:
    """Own ordered accepted file handles through EOF, failure or cancellation."""
    def __init__(self, handles):
        self.handles = handles
        self.index = 0
        self.remaining = handles[0][1] if handles else 0
        self.closed = False

    def read(self, size=-1):
        if self.closed or size == 0:
            return b""
        parts = []
        wanted = None if size is None or size < 0 else size
        try:
            while self.index < len(self.handles) and (wanted is None or wanted > 0):
                handle, _length = self.handles[self.index]
                if not self.remaining:
                    handle.close()
                    self.index += 1
                    if self.index < len(self.handles):
                        self.remaining = self.handles[self.index][1]
                    continue
                chunk = handle.read(self.remaining if wanted is None else min(wanted, self.remaining))
                if not chunk:
                    raise OSError("workspace file stream truncated")
                parts.append(chunk)
                self.remaining -= len(chunk)
                if wanted is not None:
                    wanted -= len(chunk)
            if self.index == len(self.handles):
                self.close()
            return b"".join(parts)
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.closed:
            return
        self.closed = True
        failure = None
        for handle, _length in self.handles:
            try:
                handle.close()
            except OSError as error:
                failure = failure or error
        if failure is not None:
            raise failure


def open_manifest_files(session, snapshot_key, descriptors):
    """Bulk-check selected manifest membership, then open in request order."""
    descriptors = ordered_descriptors(descriptors)
    keys = {key_digest(file_key(session, item["sha256"])): item for item in descriptors}
    found = list(WorkspaceFileObject.objects.select_related("object").filter(
        pk__in=keys, object__session=session, object__state="live", object__kind="file",
        verifiedPublicationToken=F("object__publicationToken"),
        snapshotReferences__snapshot__storageKey=snapshot_key,
        snapshotReferences__snapshot__session=session, snapshotReferences__snapshot__state="live",
        snapshotReferences__snapshot__snapshotEncoding=FILE_MANIFEST_ENCODING))
    if len(found) != len(keys) or any(
            file.sha256 != keys[file.pk]["sha256"] or file.sizeBytes != keys[file.pk]["sizeBytes"] for file in found):
        raise _error("workspace_file_reference_not_found", 404)
    files = {file.sha256: file for file in found}
    handles = []
    try:
        for descriptor in descriptors:
            file = files[descriptor["sha256"]]
            handle = default_storage.open(file.object.storageKey, "rb")
            handles.append((handle, file.sizeBytes))
            if os.fstat(handle.fileno()).st_size != file.sizeBytes:
                raise _error("workspace_file_identity_invalid")
        return FileSequence(handles)
    except BaseException:
        FileSequence(handles).close()
        raise


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


def referenced_file_keys(snapshot_keys, candidate_keys=None):
    references = WorkspaceSnapshotFileReference.objects.filter(snapshot__storageKey__in=snapshot_keys)
    if candidate_keys is not None:
        references = references.filter(file__object__storageKey__in=candidate_keys)
    return set(references.values_list("file__object__storageKey", flat=True).distinct())


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
