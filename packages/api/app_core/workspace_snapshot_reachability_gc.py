"""Reclaim unreachable Session objects after a Runtime-owned root plan is fenced."""
from functools import partial

from django.db import connection, transaction
from django.db.models import Q

from .models import Agent, AgentRunAuthorization, Session, SessionWorkspacePreservation, WorkspaceSnapshotObject, WorkspaceSnapshotUpload
from .runtime_contract import validate_session_workspace
from .storage_gc_retry import claim_storage_key, clean_storage_key
from .workspace_snapshot_publication import collect_key, live_upload_keys, pin_job_ownership, unregistered_key_has_live_owner, require_collected_incarnation, retire_upload_metadata
from .workspace_snapshot_roots import WorkspaceRootsUnavailable, request_workspace_snapshot_roots, require_current_roots


def _descriptor_key(session, descriptor):
    validate_session_workspace(descriptor)
    if not descriptor["snapshotSizeBytes"]:
        return None
    return (f"workspaces/{session.workspace_id}/sessions/{session.pk}/snapshots/{descriptor['generation']}/"
            f"{descriptor['snapshotSha256'][7:]}.snapshot")


def _api_roots(session, manifest, key):
    roots = {root["objectRef"] for root in manifest["roots"]}
    if session.workspaceStorageKey:
        roots.add(session.workspaceStorageKey)
    ids = manifest["protectedAgentRunIds"]
    grants = AgentRunAuthorization.objects.filter(agent_run__session=session, agent_run_id__in=ids)
    for grant in grants:
        frozen_key = _descriptor_key(session, grant.payload["sessionWorkspace"])
        if frozen_key:
            roots.add(frozen_key)
    receipts = SessionWorkspacePreservation.objects.filter(session=session).exclude(storage_key="")
    for receipt in receipts:
        # A terminal Run is protected by Runtime only while cleanup remains its
        # canonical responsibility. Exact holding physical identity also roots
        # preservation bytes until that original cleanup liability is gone.
        holding = False
        with connection.cursor() as cursor:
            cursor.execute("SELECT EXISTS(SELECT 1 FROM runtime.execution_instances i "
                "JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id "
                "AND s.host_id=i.host_id AND s.environment_id=i.environment_id "
                "WHERE i.execution_id=%s AND i.allocation_execution_id=i.execution_id AND i.instance_id=%s "
                "AND i.environment_id=%s AND i.session_id=%s AND i.profile_digest=%s "
                "AND s.container_id=%s AND s.workspace_id=%s)",
                [receipt.allocation_execution_id,receipt.instance_id,receipt.environment_id,session.pk,
                 receipt.profile_digest,receipt.container_id,session.workspace_id])
            holding = cursor.fetchone()[0]
        if holding or receipt.agent_run_id in ids:
            roots.add(receipt.storage_key)
    roots.update(live_upload_keys(session))
    if unregistered_key_has_live_owner(session, key):
        roots.add(key)
    return roots


def _recent_registry_keys(session, cutoff):
    objects = WorkspaceSnapshotObject.objects.filter(session=session, state="live").filter(
        Q(createdAt__gt=cutoff) | Q(updatedAt__gt=cutoff))
    keys = set(objects.values_list("storageKey", flat=True))
    uploads = WorkspaceSnapshotUpload.objects.filter(finalObject__session=session, updatedAt__gt=cutoff)
    for final, temporary in uploads.values_list("finalObject__storageKey", "temporaryObject__storageKey"):
        keys.update((final, temporary))
    return keys


def _owner_retained(agent, owner, session):
    return (owner.workspace_id == session.workspace_id and owner.agent_id == agent.pk
            and agent.workspace_id == owner.workspace_id and owner.purgedAt is None
            and owner.workspaceBaselineKnown)


def collect_reachable_workspace_gc(root, cutoff, dry_run, report):
    from . import workspace_snapshot_gc as gc
    owners = Session.objects.filter(purgedAt__isnull=True).only(
        "id", "workspace_id", "agent_id", "workspaceBaselineKnown")
    for session in owners.order_by("id").iterator():
        prefix = gc._session_prefix(session)
        if not dry_run and (WorkspaceSnapshotUpload.objects.filter(finalObject__session=session).exists()
                or WorkspaceSnapshotObject.objects.filter(session=session,kind="temporary",state="collected").exists()):
            with transaction.atomic():
                Agent.objects.select_for_update().get(pk=session.agent_id)
                owner=Session.objects.select_for_update().get(pk=session.pk)
                pin_job_ownership(owner)
                retire_upload_metadata(owner,cutoff)
        keys = list(gc._snapshot_keys(root, prefix, session.pk, report))
        if not keys:
            continue
        if set(keys).issubset({session.workspaceStorageKey}):
            continue
        if not session.workspaceBaselineKnown:
            report.blocked.append(prefix + ":baseline_unknown")
            continue
        # Runtime I/O must never run under Agent/Session database locks.
        try:
            manifest = request_workspace_snapshot_roots(session)
        except Exception:
            report.blocked.append(prefix + ":roots_unavailable")
            continue
        for key in keys:
            try:
                with transaction.atomic():
                    agent = Agent.objects.select_for_update().get(pk=session.agent_id)
                    owner = Session.objects.select_for_update().get(pk=session.pk)
                    if not _owner_retained(agent, owner, session):
                        report.blocked.append(key)
                        continue
                    require_current_roots(owner, manifest)
                    pin_job_ownership(owner)
                    roots = _api_roots(owner, manifest, key)
                    if key in roots:
                        continue
                    path = gc._checked_path(root, key, directory=False)
                    if path.stat().st_mtime > cutoff.timestamp() or key in _recent_registry_keys(owner, cutoff):
                        report.blocked.append(key)
                        continue
                    if dry_run:
                        report.planned.append(key)
                        continue
                    object=collect_key(owner, key, kind="temporary" if gc._TEMPORARY_FILE.fullmatch(path.name) else "snapshot")
                    token=object.publicationToken
                # The incarnation fence commits before physical unlink. An I/O
                # crash can leave bytes, but no older publisher may recreate a
                # collected name after any successful or retried deletion.
                claim = claim_storage_key(key, "workspaceSnapshot",publication_token=token)
                if claim is None:
                    report.blocked.append(key)
                    continue
                with transaction.atomic():
                    agent = Agent.objects.select_for_update().get(pk=session.agent_id)
                    owner = Session.objects.select_for_update().get(pk=session.pk)
                    if not _owner_retained(agent, owner, session):
                        report.blocked.append(key)
                        continue
                    require_current_roots(owner, manifest)
                    pin_job_ownership(owner)
                    if not require_collected_incarnation(owner,key,token):
                        report.blocked.append(key)
                        continue
                    if key in _api_roots(owner, manifest, key):
                        report.blocked.append(key)
                        continue
                    gc._checked_path(root, key, directory=False)
                    result = clean_storage_key(key, "workspaceSnapshot",
                        partial(gc._delete_observed_snapshot, session_id=str(owner.pk)), claim=claim,publication_token=token)
                    if result.state == "cleaned":
                        report.cleaned.append(key)
                    elif result.state == "blocked":
                        report.blocked.append(key)
                    else:
                        report.failures.append(f"{key}: {result.failure}")
            except FileNotFoundError:
                continue
            except WorkspaceRootsUnavailable:
                report.blocked.append(prefix + ":roots_changed")
                break
            except Exception as error:
                report.failures.append(f"{key}: {error}")
    return report
