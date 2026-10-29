"""Durable exact-key fences shared by snapshot publishers and collectors."""
import hashlib
from pathlib import Path
import uuid

from django.core.files.storage import default_storage
from django.db import connection, transaction
from django.utils import timezone

from .models import Agent, Session, StorageCleanupRetry, WorkspaceSnapshotObject, WorkspaceSnapshotUpload, new_workspace_publication_token
from .resource_commit import require_durable_commit
from .storage_gc_retry import rearm_storage_key


def key_digest(key):
    return hashlib.sha256(key.encode("utf-8")).hexdigest()


def _object(session, run, key, kind):
    from .http.internal import SessionWorkspaceError
    digest = key_digest(key)
    previous = StorageCleanupRetry.objects.filter(pk=digest, collector="workspaceSnapshot", state="cleaned").first()
    object, _ = WorkspaceSnapshotObject.objects.get_or_create(keyDigest=digest,
        defaults={"storageKey": key, "session": session, "kind": kind})
    if object.storageKey != key or object.session_id != session.pk or object.kind != kind:
        raise SessionWorkspaceError("session_workspace_publication_identity_invalid")
    if object.state != "live" or previous is not None:
        from .http.internal import _require_workspace_baseline
        _require_workspace_baseline(session,run.authorization.payload["sessionWorkspace"])
        object.publicationToken = new_workspace_publication_token()
        object.state, object.collectedAt = "live", None
        object.save(update_fields=["publicationToken","state","collectedAt","updatedAt"])
    if not object.registered:
        object.registered=True
        object.save(update_fields=["registered","updatedAt"])
    rearm_storage_key(key,"workspaceSnapshot",object.publicationToken)
    return object


def register_upload(session, run, body, final_key, temporary_path):
    """The caller owns Agent→Session locks before either namespace can be used."""
    from .http.internal import SessionWorkspaceError
    require_durable_commit(connection, SessionWorkspaceError("session_workspace_durability_required"))
    temporary_key = Path(temporary_path).relative_to(Path(default_storage.path(""))).as_posix()
    final = _object(session, run, final_key, "snapshot")
    temporary = _object(session, run, temporary_key, "temporary")
    return WorkspaceSnapshotUpload.objects.create(id=uuid.uuid4().hex, finalObject=final,
        temporaryObject=temporary, agent_run=run, jobId=body["jobId"], leaseOwner=body["leaseOwner"],
        authorizationDigest=body["authorizationDigest"],finalPublicationToken=final.publicationToken,
        temporaryPublicationToken=temporary.publicationToken).pk


def require_upload_live(upload_id, final_key):
    from .http.internal import SessionWorkspaceError
    upload = WorkspaceSnapshotUpload.objects.select_related("finalObject", "temporaryObject").filter(pk=upload_id).first()
    if upload is None:
        raise SessionWorkspaceError("session_workspace_publication_expired")
    if upload.finalObject.storageKey != final_key or upload.state == "abandoned":
        raise SessionWorkspaceError("session_workspace_publication_identity_invalid")
    if (upload.finalObject.state != "live" or upload.temporaryObject.state != "live"
            or upload.finalPublicationToken != upload.finalObject.publicationToken
            or upload.temporaryPublicationToken != upload.temporaryObject.publicationToken):
        raise SessionWorkspaceError("session_workspace_publication_collected")
    if StorageCleanupRetry.objects.filter(pk=upload.finalObject_id, collector="workspaceSnapshot", state="cleaned",
            publicationToken=upload.finalPublicationToken).exists():
        raise SessionWorkspaceError("session_workspace_publication_collected")


def mark_published(upload_id, state):
    WorkspaceSnapshotUpload.objects.filter(pk=upload_id).update(state=state, updatedAt=timezone.now())


def finish_upload(upload_id):
    now = timezone.now()
    upload=WorkspaceSnapshotUpload.objects.select_related("temporaryObject").filter(pk=upload_id).first()
    if upload is None:
        return
    with transaction.atomic():
        owner=Session.objects.only("agent_id").get(pk=upload.temporaryObject.session_id)
        Agent.objects.select_for_update().get(pk=owner.agent_id)
        Session.objects.select_for_update().get(pk=owner.pk)
        WorkspaceSnapshotUpload.objects.filter(pk=upload_id,state="streaming").update(state="abandoned",updatedAt=now)
        WorkspaceSnapshotUpload.objects.filter(pk=upload_id).update(finishedAt=now)
        if not default_storage.exists(upload.temporaryObject.storageKey):
            WorkspaceSnapshotObject.objects.filter(pk=upload.temporaryObject_id,publicationToken=upload.temporaryPublicationToken,
                state="live").update(state="collected",collectedAt=now,updatedAt=now)


def live_upload_keys(session):
    """A canonical currently leased Job protects all its staged request keys."""
    with connection.cursor() as cursor:
        cursor.execute('SELECT u.id,u."finalObject_id",u."temporaryObject_id" '
            'FROM public.app_core_workspacesnapshotupload u '
            'JOIN public.app_core_workspacesnapshotobject o ON o."keyDigest"=u."finalObject_id" '
            'JOIN runtime.runtime_jobs j ON j.job_id=u."jobId" '
            'WHERE o.session_id=%s AND u.state IN (\'streaming\',\'staged\') AND j.job_kind=\'agent_run.lifecycle\' '
            "AND j.status IN ('leased','running') AND j.lease_owner IS NOT NULL "
            "AND j.lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint "
            "AND j.session_id=%s AND j.payload_ref='record:agent_run:'||u.agent_run_id "
            'AND j.idempotency_key=j.job_id||\':\'||u."authorizationDigest"', [session.pk, session.pk])
        digests = {digest for row in cursor.fetchall() for digest in row[1:]}
    return set(WorkspaceSnapshotObject.objects.filter(pk__in=digests).values_list("storageKey", flat=True))


def pin_job_ownership(session):
    # Lease renewal/handoff cannot race the final current-ownership observation
    # and exact unlink. New publishers additionally require the held Session lock.
    with connection.cursor() as cursor:
        cursor.execute("SELECT job_id FROM runtime.runtime_jobs WHERE session_id=%s "
            "AND job_kind='agent_run.lifecycle' FOR SHARE NOWAIT", [session.pk])
        cursor.fetchall()


def unregistered_key_has_live_owner(session, key):
    digest = key_digest(key)
    if WorkspaceSnapshotObject.objects.filter(pk=digest,registered=True).exists():
        return False
    if WorkspaceSnapshotUpload.objects.filter(finalObject_id=digest).exists() or WorkspaceSnapshotUpload.objects.filter(temporaryObject_id=digest).exists():
        return False
    with connection.cursor() as cursor:
        cursor.execute("SELECT r.id FROM public.app_core_agentrun r "
            "JOIN public.app_core_agentrunauthorization a ON a.agent_run_id=r.id "
            "JOIN runtime.runtime_jobs j ON j.job_id='agent_run.lifecycle:'||r.id "
            "WHERE r.session_id=%s AND j.session_id=r.session_id AND j.job_kind='agent_run.lifecycle' "
            "AND j.status IN ('leased','running') AND j.lease_owner IS NOT NULL "
            "AND j.lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint "
            "AND j.payload_ref='record:agent_run:'||r.id AND j.idempotency_key=j.job_id||':'||a.digest", [session.pk])
        runs = [row[0] for row in cursor.fetchall()]
    prefix = f"workspaces/{session.workspace_id}/sessions/{session.pk}/"
    return bool(runs) and (key.startswith(prefix + "snapshots/") or any(
        key.startswith(prefix + f"agent-runs/{run}/execution-checkpoints/") for run in runs))


def collect_key(session, key, *, kind):
    """Invalidate the current incarnation under the caller's owner lock."""
    from .http.internal import SessionWorkspaceError
    require_durable_commit(connection, SessionWorkspaceError("session_workspace_durability_required"))
    object, _ = WorkspaceSnapshotObject.objects.get_or_create(keyDigest=key_digest(key),
        defaults={"storageKey": key, "session": session, "kind": kind})
    if object.storageKey != key or object.session_id != session.pk or object.kind != kind:
        raise SessionWorkspaceError("session_workspace_publication_identity_invalid")
    if object.state != "collected":
        object.state, object.collectedAt = "collected", timezone.now()
        object.save(update_fields=["state", "collectedAt", "updatedAt"])
    return object


def require_collected_incarnation(session, key, token):
    return WorkspaceSnapshotObject.objects.filter(pk=key_digest(key),session=session,state="collected",publicationToken=token).exists()


def retire_upload_metadata(session, cutoff):
    """Retire expired request tokens; selected snapshot refs do not need upload rows."""
    uploads=WorkspaceSnapshotUpload.objects.filter(finalObject__session=session,updatedAt__lte=cutoff)
    live=live_upload_keys(session)
    for upload in uploads.select_related("finalObject","temporaryObject"):
        if upload.finalObject.storageKey in live or upload.temporaryObject.storageKey in live:
            continue
        upload.delete()
    temporary=WorkspaceSnapshotObject.objects.filter(session=session,kind="temporary",state="collected",
        collectedAt__lte=cutoff,temporaryUpload__isnull=True,finalUploads__isnull=True)
    for object in temporary:
        if not default_storage.exists(object.storageKey):
            StorageCleanupRetry.objects.filter(pk=object.pk,collector="workspaceSnapshot",state="cleaned",
                publicationToken=object.publicationToken).delete()
            object.delete()
