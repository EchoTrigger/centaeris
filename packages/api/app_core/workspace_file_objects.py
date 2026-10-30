"""Session-scoped immutable file content and indexed manifest membership.

Callers own authorization and Agent->Session locking. Only upload_file crosses
that boundary itself, invoking the supplied authority callback before accepting
bytes and again before publishing the verified private file.
"""
from contextlib import ExitStack
from concurrent.futures import ThreadPoolExecutor
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
from .models import (Agent, Session, WorkspaceFilePack, WorkspaceFileObject, WorkspaceFileReservation, WorkspaceSnapshotFileReference,
                     WorkspaceSnapshotObject, WorkspaceSnapshotUpload, StorageCleanupRetry)
from .resource_commit import require_durable_commit
from .runtime_contract import require_sha256
from .workspace_snapshot_publication import _object, key_digest
from . import workspace_observations as observations


FILE_MANIFEST_ENCODING = "fileManifestV1"
IO_BYTES = 64 * 1024
FILE_SYNC_WORKERS = 8
SMALL_FILE_BYTES = 16 * 1024
PACK_BYTES = 512 * 1024
PACK_FILES = 32


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


def file_available(file):
    if (file.object.state != "live" or file.object.kind != "file"
            or file.verifiedPublicationToken != file.object.publicationToken):
        return False
    if file.pack_id is None:
        return True
    pack = file.pack
    return (pack.object.session_id == file.object.session_id and pack.object.kind == "filePack"
        and pack.object.state == "live" and pack.verifiedPublicationToken == pack.object.publicationToken
        and file.packOffset + file.sizeBytes <= pack.sizeBytes)


def physical_file_keys(logical_keys, candidate_keys=None):
    files = WorkspaceFileObject.objects.filter(object__storageKey__in=logical_keys)
    if candidate_keys is not None:
        files = files.filter(Q(pack__isnull=True, object__storageKey__in=candidate_keys)
                             | Q(pack__object__storageKey__in=candidate_keys))
    return set(files.annotate(physicalKey=Case(When(pack__isnull=True, then=F("object__storageKey")),
        default=F("pack__object__storageKey"))).values_list("physicalKey", flat=True).distinct())


def logical_file_keys(physical_candidates):
    return set(WorkspaceFileObject.objects.filter(
        Q(pack__isnull=True, object__storageKey__in=physical_candidates)
        | Q(pack__object__storageKey__in=physical_candidates))
        .values_list("object__storageKey", flat=True).distinct())


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
    contents = {file.pk: file for file in WorkspaceFileObject.objects.select_related("object", "pack__object").filter(pk__in=keys)}
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
        if not file_available(file):
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
    found = list(WorkspaceFileObject.objects.select_related("object", "pack__object").filter(
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


def _physical_groups(session, descriptors, files):
    groups, small = [], []
    small_size = 0
    for descriptor in descriptors:
        file = files[descriptor["sha256"]]
        eligible = (descriptor["sizeBytes"] <= SMALL_FILE_BYTES and not file_available(file)
            and (file.pack_id is not None or not Path(default_storage.path(file.object.storageKey)).exists()))
        if not eligible:
            groups.append([descriptor])
            continue
        if small and (len(small) == PACK_FILES or small_size + descriptor["sizeBytes"] > PACK_BYTES):
            groups.append(small)
            small, small_size = [], 0
        small.append(descriptor)
        small_size += descriptor["sizeBytes"]
    if small:
        groups.append(small)
    return groups


def _publish_file_ranges(files):
    if not files:
        return
    meta = WorkspaceFileObject._meta
    quote = connection.ops.quote_name
    columns = [quote(meta.get_field(field).column)
               for field in ("object", "pack", "packOffset", "verifiedPublicationToken")]
    identity, pack, offset, token = columns
    rows = ", ".join(["(%s, %s, %s::bigint, %s)"] * len(files))
    parameters = [value for file in files
                  for value in (file.pk, file.pack_id, file.packOffset, file.verifiedPublicationToken)]
    with connection.cursor() as cursor:
        cursor.execute(
            f"UPDATE {quote(meta.db_table)} AS target "
            f"SET {pack}=changes.{pack}, {offset}=changes.{offset}, {token}=changes.{token} "
            f"FROM (VALUES {rows}) AS changes ({', '.join(columns)}) "
            f"WHERE target.{identity}=changes.{identity}", parameters)


def upload_files(source, body, descriptors, authorize):
    """Verify logical files once and durably publish bounded physical units."""
    descriptors = ordered_descriptors(descriptors, unique=True)
    entries, ranges = [], {}
    owner = None
    try:
        with transaction.atomic():
            run, owner, *_ = authorize(body)
            require_durable_commit(connection, _error("session_workspace_durability_required"))
            files = _reserved_files(owner, run, body, descriptors)
            member_tokens = {sha: file.object.publicationToken for sha, file in files.items()}
            final_objects, temporaries, packs, uploads = [], [], [], []
            for members in _physical_groups(owner, descriptors, files):
                size = sum(member["sizeBytes"] for member in members)
                pack = None
                if len(members) > 1:
                    key = f"workspaces/{owner.workspace_id}/sessions/{owner.pk}/file-packs/{uuid.uuid4().hex}.pack"
                    final = WorkspaceSnapshotObject(keyDigest=key_digest(key), storageKey=key,
                        session=owner, kind="filePack", registered=True)
                    final_objects.append(final)
                    pack = WorkspaceFilePack(object=final, sizeBytes=size)
                    packs.append(pack)
                else:
                    final = files[members[0]["sha256"]].object
                final_path = Path(default_storage.path(final.storageKey))
                final_path.parent.mkdir(parents=True, exist_ok=True)
                fd, path = tempfile.mkstemp(prefix=".centaeris-immutable-", suffix=".tmp", dir=final_path.parent)
                temporary_key = Path(path).relative_to(Path(default_storage.path(""))).as_posix()
                temporary = WorkspaceSnapshotObject(keyDigest=key_digest(temporary_key), storageKey=temporary_key,
                    session=owner, kind="temporary", registered=True)
                upload = WorkspaceSnapshotUpload(id=uuid.uuid4().hex, finalObject=final,
                    temporaryObject=temporary, agent_run=run, jobId=body["jobId"], leaseOwner=body["leaseOwner"],
                    authorizationDigest=body["authorizationDigest"], finalPublicationToken=final.publicationToken,
                    temporaryPublicationToken=temporary.publicationToken)
                entry = {"members": members, "fd": fd, "path": path, "writer": None,
                         "final": final, "pack": pack, "temporary": temporary, "upload": upload}
                entries.append(entry)
                offset = 0
                for member in members:
                    ranges[member["sha256"]] = (entry, offset)
                    offset += member["sizeBytes"]
                temporaries.append(temporary)
                uploads.append(upload)
            WorkspaceSnapshotObject.objects.bulk_create(final_objects + temporaries)
            WorkspaceFilePack.objects.bulk_create(packs)
            WorkspaceSnapshotUpload.objects.bulk_create(uploads)
        with ExitStack() as writers:
            pending_sync = []
            temporary_bytes = 0
            for descriptor in descriptors:
                entry, _offset = ranges[descriptor["sha256"]]
                if entry["writer"] is None:
                    output = os.fdopen(entry["fd"], "wb")
                    entry["fd"] = None
                    entry["writer"] = writers.enter_context(output)
                    pending_sync.append(entry["writer"])
                output = entry["writer"]
                digest = hashlib.sha256()
                remaining = descriptor["sizeBytes"]
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
                temporary_bytes += descriptor["sizeBytes"]
            for output in pending_sync:
                output.flush()
            observations.gauge("temporaryBytes", temporary_bytes)
            extra = source.read(1)
            observations.increment("apiReceivedBytes", len(extra))
            if extra:
                raise _error(status=400)
            with ThreadPoolExecutor(max_workers=FILE_SYNC_WORKERS) as syncs:
                pending = [syncs.submit(os.fsync, output.fileno()) for output in pending_sync]
                for completion in pending:
                    completion.result()
        with transaction.atomic():
            run, session, *_ = authorize(body)
            files = _reserved_files(session, run, body, descriptors)
            if any(file.object.publicationToken != member_tokens[sha] for sha, file in files.items()):
                raise _publication_lost()
            uploads = {upload.pk: upload for upload in WorkspaceSnapshotUpload.objects.select_related(
                "finalObject", "temporaryObject").filter(pk__in=[entry["upload"].pk for entry in entries])}
            cleaned = set(StorageCleanupRetry.objects.filter(pk__in=[entry["final"].pk for entry in entries],
                collector="workspaceSnapshot", state="cleaned").values_list("pk", "publicationToken"))
            for entry in entries:
                upload = uploads.get(entry["upload"].pk)
                if (upload is None or upload.state == "abandoned" or upload.finalObject_id != entry["final"].pk
                        or upload.finalObject.state != "live" or upload.temporaryObject.state != "live"
                        or upload.finalPublicationToken != upload.finalObject.publicationToken
                        or upload.temporaryPublicationToken != upload.temporaryObject.publicationToken
                        or (upload.finalObject_id, upload.finalPublicationToken) in cleaned):
                    raise _publication_lost()
            changed, verified_packs, directories = [], [], set()
            for entry in entries:
                members = [files[member["sha256"]] for member in entry["members"]
                           if not file_available(files[member["sha256"]])]
                if not members:
                    continue
                os.replace(entry["path"], default_storage.path(entry["final"].storageKey))
                directories.add(str(Path(entry["path"]).parent))
                if entry["pack"] is not None:
                    entry["pack"].verifiedPublicationToken = entry["final"].publicationToken
                    verified_packs.append(entry["pack"])
                for file in members:
                    file.pack = entry["pack"]
                    file.packOffset = ranges[file.sha256][1] if entry["pack"] is not None else 0
                    file.verifiedPublicationToken = file.object.publicationToken
                    changed.append(file)
            for directory in directories:
                _sync_directory(directory)
            WorkspaceFilePack.objects.bulk_update(verified_packs, ["verifiedPublicationToken"])
            _publish_file_ranges(changed)
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
    files = list(WorkspaceFileObject.objects.select_related("object", "pack__object").filter(
        pk__in=keys, object__session=session, object__kind="file", object__state="live",
        verifiedPublicationToken=F("object__publicationToken"), reservations__agent_run=run,
        reservations__jobId=body["jobId"], reservations__leaseOwner=body["leaseOwner"],
        reservations__authorizationDigest=body["authorizationDigest"],
        reservations__publicationToken=F("object__publicationToken")))
    if len(files) != len(keys) or any(
            not file_available(file) or file.sha256 != keys[file.pk]["sha256"]
            or file.sizeBytes != keys[file.pk]["sizeBytes"] for file in files):
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


class FileRange:
    """Expose exactly one verified member without leaking adjacent pack bytes."""
    def __init__(self, handle, offset, length, *, owns=True):
        self.handle, self.offset, self.length = handle, offset, length
        self.position, self.owns, self.closed = 0, owns, False

    def read(self, size=-1):
        if self.closed:
            return b""
        remaining = self.length - self.position
        amount = remaining if size is None or size < 0 else min(size, remaining)
        self.handle.seek(self.offset + self.position)
        content = self.handle.read(amount)
        self.position += len(content)
        return content

    def close(self):
        if not self.closed:
            self.closed = True
            if self.owns:
                self.handle.close()

    def __enter__(self):
        return self

    def __exit__(self, *_error):
        self.close()


class FileSequence:
    """Own ordered accepted file handles through EOF, failure or cancellation."""
    def __init__(self, handles, owners=()):
        self.handles = handles
        self.owners = owners
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
        for handle in [handle for handle, _length in self.handles] + list(self.owners):
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
    found = list(WorkspaceFileObject.objects.select_related("object", "pack__object").filter(
        pk__in=keys, object__session=session, object__state="live", object__kind="file",
        verifiedPublicationToken=F("object__publicationToken"),
        snapshotReferences__snapshot__storageKey=snapshot_key,
        snapshotReferences__snapshot__session=session, snapshotReferences__snapshot__state="live",
        snapshotReferences__snapshot__snapshotEncoding=FILE_MANIFEST_ENCODING))
    if len(found) != len(keys) or any(
            not file_available(file) or file.sha256 != keys[file.pk]["sha256"]
            or file.sizeBytes != keys[file.pk]["sizeBytes"] for file in found):
        raise _error("workspace_file_reference_not_found", 404)
    files = {file.sha256: file for file in found}
    handles, owners = [], {}
    try:
        for descriptor in descriptors:
            file = files[descriptor["sha256"]]
            key = file.pack.object.storageKey if file.pack_id else file.object.storageKey
            if key not in owners:
                handle = default_storage.open(key, "rb")
                owners[key] = handle
                length = file.pack.sizeBytes if file.pack_id else file.sizeBytes
                if os.fstat(handle.fileno()).st_size != length:
                    raise _error("workspace_file_identity_invalid")
            handles.append((FileRange(owners[key], file.packOffset if file.pack_id else 0,
                                      file.sizeBytes, owns=False), file.sizeBytes))
        return FileSequence(handles, tuple(owners.values()))
    except BaseException:
        FileSequence(handles, tuple(owners.values())).close()
        raise


def open_file(session, sha256):
    """Open after the caller proved a selected snapshot/reference or reservation."""
    file = WorkspaceFileObject.objects.select_related("object", "pack__object").filter(
        pk=key_digest(file_key(session, sha256)), object__session=session, object__state="live",
        verifiedPublicationToken=F("object__publicationToken")).first()
    if file is None or not file_available(file):
        raise _error("workspace_file_unavailable", 404)
    key = file.pack.object.storageKey if file.pack_id else file.object.storageKey
    handle = default_storage.open(key, "rb")
    length = file.pack.sizeBytes if file.pack_id else file.sizeBytes
    try:
        if os.fstat(handle.fileno()).st_size != length:
            raise _error("workspace_file_identity_invalid")
        return FileRange(handle, file.packOffset if file.pack_id else 0, file.sizeBytes)
    except BaseException:
        handle.close()
        raise


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
