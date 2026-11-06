"""Reclaim one-shot payloads after execution, physical cleanup and delivery settle."""

from dataclasses import dataclass
import logging
import urllib.error

import redis
from django.apps import apps
from django.conf import settings
from django.db import connection, transaction
from django.db.models import Q
from django.utils import timezone

from . import models
from .assets import delete_stored_object_for_gc
from .runtime_client import build_agent_run_control
from .runtime_http import request_json
from .workspace_snapshot_gc import (
    WorkspaceSnapshotGcReport, _checked_path, _local_storage_root, _session_prefix, _snapshot_keys,
)


logger = logging.getLogger(__name__)
TERMINAL_STATUSES = {"completed", "failed", "cancelled"}


@dataclass
class OneShotCleanupReport:
    planned: list[str]
    cleaned: list[str]
    blocked: list[str]
    failures: list[str]


def _delivery_closed(operation):
    return operation.deliveryClosedAt is not None or (
        operation.deliveryLeaseUntil is not None and operation.deliveryLeaseUntil <= timezone.now()
    )


def _purge_runtime(run):
    result = request_json("/internal/agent-runs/one-shot-purge", {
        "schema": "runtime.agent_run.one_shot_purge.v1",
        "agentRunControl": build_agent_run_control(run, require_current_authority=False),
    }, timeout=settings.RUNTIME_CONTROL_TIMEOUT_SECONDS)
    if result != {
        "schema": "runtime.agent_run.one_shot_purged.v1",
        "agentRunId": run.pk, "sessionId": run.session_id, "purged": True,
    }:
        raise ValueError("one_shot_runtime_purge_receipt_invalid")


def _owned_rows(session):
    """Capture the relational deletion set and reject cross-session references."""
    doomed = {models.Session: {session.pk}}
    candidates = list(apps.get_app_config("app_core").get_models())
    changed = True
    while changed:
        changed = False
        for model in candidates:
            identities = set(doomed.get(model, ()))
            for field in model._meta.fields:
                target = getattr(field.remote_field, "model", None)
                if target not in doomed or not doomed[target]:
                    continue
                rows = model.objects.filter(**{field.attname + "__in": doomed[target]})
                if any(item.name == "session" for item in model._meta.fields):
                    if rows.exclude(session_id=session.pk).exists():
                        raise ValueError("one_shot_cross_session_reference")
                if model is models.Session and rows.exclude(pk=session.pk).exists():
                    raise ValueError("one_shot_cross_session_reference")
                if model is models.WorkspaceSnapshotFileReference and rows.exclude(snapshot__session=session).exists():
                    raise ValueError("one_shot_cross_session_file_reference")
                identities.update(rows.values_list("pk", flat=True))
            if identities != doomed.get(model, set()):
                doomed[model] = identities
                changed = True
    return doomed


def _storage_keys(session, artifact_ids):
    keys = {session.workspaceStorageKey}
    keys.update(models.Artifact.objects.filter(pk__in=artifact_ids).values_list("storageKey", flat=True))
    keys.update(models.WorkspaceSnapshotObject.objects.filter(session=session).values_list("storageKey", flat=True))
    keys.update(models.SessionWorkspacePreservation.objects.filter(session=session).values_list("storage_key", flat=True))
    keys.update(models.DerivedResource.objects.filter(
        ownerKind="artifact", ownerId__in=artifact_ids,
    ).values_list("resourceKey", flat=True))
    for row in models.DerivedRepresentation.objects.filter(ownerKind="artifact", ownerId__in=artifact_ids):
        keys.update((row.canonicalTextKey, row.previewPdfKey, row.workbookPreviewKey))
    root = _local_storage_root()
    if root is None:
        raise ValueError("one_shot_workspace_storage_enumeration_unavailable")
    report = WorkspaceSnapshotGcReport([], [], [], [])
    for key in _snapshot_keys(root, _session_prefix(session), session.pk, report):
        _checked_path(root, key, directory=False)
        keys.add(key)
    if report.failures or report.blocked:
        raise ValueError("one_shot_workspace_storage_unsettled")
    return keys - {""}


def _shared_storage_key(key, session, artifact_ids):
    if models.SourceObject.objects.filter(storageKey=key).exists():
        return True
    if models.UserLibraryObject.objects.filter(storageKey=key).exists():
        return True
    if models.Artifact.objects.filter(storageKey=key).exclude(pk__in=artifact_ids).exists():
        return True
    if models.Session.objects.filter(workspaceStorageKey=key).exclude(pk=session.pk).exists():
        return True
    if models.WorkspaceSnapshotObject.objects.filter(storageKey=key).exclude(session=session).exists():
        return True
    if models.SessionWorkspacePreservation.objects.filter(storage_key=key).exclude(session=session).exists():
        return True
    representations = models.DerivedRepresentation.objects.filter(
        Q(canonicalTextKey=key) | Q(previewPdfKey=key) | Q(workbookPreviewKey=key),
    )
    if representations.exclude(ownerKind="artifact", ownerId__in=artifact_ids).exists():
        return True
    return models.DerivedResource.objects.filter(resourceKey=key).exclude(
        ownerKind="artifact", ownerId__in=artifact_ids,
    ).exclude(state="cleaned").exists()


def _purge_payloads(operation, session):
    doomed = _owned_rows(session)
    artifact_ids = doomed.get(models.Artifact, set())
    keys = _storage_keys(session, artifact_ids)
    for key in sorted(keys):
        if not _shared_storage_key(key, session, artifact_ids):
            # Keys remain recoverable from their owners if this transaction aborts.
            delete_stored_object_for_gc(key)
    transient = redis.Redis.from_url(settings.REDIS_URL, socket_timeout=5, socket_connect_timeout=5)
    try:
        prefix = f"workspace-agent:agent-run:{{{operation.agentRunId}}}"
        transient.delete(f"{prefix}:signals", f"{prefix}:live:text", f"{prefix}:live:meta")
    finally:
        transient.close()
    representations = models.DerivedRepresentation.objects.filter(ownerKind="artifact", ownerId__in=artifact_ids)
    models.KnowledgeSegment.objects.filter(representation__in=representations).delete()
    representations.delete()
    models.DerivedResource.objects.filter(ownerKind="artifact", ownerId__in=artifact_ids).delete()
    models.ModelRunLog.objects.filter(agentRunId=operation.agentRunId).delete()
    # These two guards need the signed Run and Session while evaluating deletes.
    first = (models.SessionEnvironment, models.AgentRunEnvironmentRelease)
    last = (models.AgentRunAuthorization, models.AgentRun, models.Session)
    order = [*first, *(model for model in doomed if model not in first + last), *last]
    for model in order:
        identities = doomed.get(model)
        if identities:
            model.objects.filter(pk__in=identities)._raw_delete(connection.alias)
    change_keys = [f"session:{session.pk}:{kind}" for kind in ("state", "context")]
    models.PresentationChangeOutbox.objects.filter(key__in=change_keys).delete()
    models.PresentationChange.objects.filter(key__in=change_keys).delete()
    # Remove only successfully erased exact-key ledger entries from this owner.
    models.StorageCleanupRetry.objects.filter(storageKey__in=keys, state="cleaned").delete()


def _collect_operation(operation_id, dry_run):
    operation = models.OneShotOperation.objects.get(pk=operation_id)
    if operation.cleanupStatus == "completed" or not _delivery_closed(operation):
        return "blocked"
    run = models.AgentRun.objects.select_related("session", "authorization", "workspace", "modelConfig").get(
        pk=operation.agentRunId, session_id=operation.sessionId, session__purpose="oneShot",
    )
    if run.status not in TERMINAL_STATUSES:
        return "blocked"
    if models.SessionEnvironment.objects.filter(session_id=operation.sessionId, controller_released=False).exists():
        return "blocked"
    if not (run.preAdmissionCancelledAt is not None or models.AgentRunEnvironmentRelease.objects.filter(agent_run=run).exists()):
        return "blocked"
    if dry_run:
        return "planned"
    try:
        # Runtime fences the same Session. Release our DB transaction before HTTP.
        _purge_runtime(run)
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
        if models.OneShotOperation.objects.filter(pk=operation_id, cleanupStatus="completed").exists():
            return "cleaned"
        if status == 409:
            return "blocked"
        raise
    with transaction.atomic():
        operation = models.OneShotOperation.objects.select_for_update(skip_locked=True).filter(pk=operation_id).first()
        if operation is None or operation.cleanupStatus == "completed" or not _delivery_closed(operation):
            return "blocked"
        session = models.Session.objects.select_for_update().get(pk=operation.sessionId, purpose="oneShot")
        run = models.AgentRun.objects.select_related("session", "authorization", "workspace", "modelConfig").get(
            pk=operation.agentRunId, session=session,
        )
        operation.runStatus = run.status
        if run.status in {"failed", "cancelled"}:
            operation.reasonCode = run.transitionReason
        if run.status not in TERMINAL_STATUSES:
            operation.save(update_fields=["runStatus"])
            return "blocked"
        if models.SessionEnvironment.objects.filter(session=session, controller_released=False).exists():
            return "blocked"
        if not (run.preAdmissionCancelledAt is not None or models.AgentRunEnvironmentRelease.objects.filter(agent_run=run).exists()):
            return "blocked"
        operation.cleanupStatus = "cleaning"
        operation.save(update_fields=["runStatus", "cleanupStatus", "reasonCode"])
        _purge_payloads(operation, session)
        operation.cleanupStatus = "completed"
        operation.deliveryClosedAt = operation.deliveryClosedAt or timezone.now()
        operation.deliveryLeaseUntil = None
        operation.save(update_fields=["cleanupStatus", "deliveryClosedAt", "deliveryLeaseUntil"])
        return "cleaned"


def collect_one_shot_cleanup(dry_run=False, *, limit=64):
    report = OneShotCleanupReport([], [], [], [])
    due = models.OneShotOperation.objects.exclude(cleanupStatus="completed").filter(
        agentRunId__in=models.AgentRun.objects.filter(status__in=TERMINAL_STATUSES).values("pk"),
    ).filter(Q(deliveryClosedAt__isnull=False) | Q(deliveryLeaseUntil__lte=timezone.now())).order_by("acceptedAt")
    for operation in due[:limit]:
        try:
            state = _collect_operation(operation.pk, dry_run)
            getattr(report, state).append(operation.agentRunId)
        except Exception as error:
            models.OneShotOperation.objects.filter(pk=operation.pk).exclude(cleanupStatus="completed").update(cleanupStatus="failed")
            report.failures.append(operation.agentRunId)
            logger.exception("One-shot cleanup failed", extra={"agentRunId": operation.agentRunId, "exceptionType": type(error).__name__})
    if not dry_run:
        models.OneShotOperation.objects.filter(cleanupStatus="completed", expiresAt__lte=timezone.now()).delete()
    return report
