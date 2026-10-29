"""Preserve one stopped allocation through the ordinary Session snapshot commit.

Runtime proves physical identity, stopped state and readable storage while holding
its host pin. This boundary proves durable target and cleanup authority again at
publication; cleanup authority never becomes permission to execute in a guest.
"""
from contextlib import ExitStack
import hashlib
import json
import secrets
import re

from django.conf import settings
from django.db import DatabaseError, connection, transaction
from django.http import JsonResponse

from .environment_authorization import build_environment_authorization
from .environment_lifecycle import _authority, _locked_binding
from .models import AgentRun, AgentRunAuthorization, SessionWorkspacePreservation
from .resource_commit import require_durable_commit
from .runtime_contract import (
    agent_run_binding_matches, authorization_digest, require_opaque_ref, require_sha256,
    session_workspace_for_session, validate_session_workspace, verify_agent_run_authorization_signature,
)


SCHEMA = "runtime.session_workspace.preserve.v1"
RESULT_SCHEMA = "runtime.session_workspace.preserve.result.v1"
TARGET_FIELDS = {"environmentAuthorization", "allocationExecutionId", "instanceId", "containerId",
                 "profileDigest", "baseline"}


def _locked_preservation(body):
    from .http.internal import SessionWorkspaceError
    require_durable_commit(connection, SessionWorkspaceError("session_workspace_durability_required"))
    run, environment = _locked_binding(body["agentRunId"], body["leaseOwner"], acquiring=False,
        protect_session_owner=True)
    with connection.cursor() as cursor:
        cursor.execute("SELECT lease_purpose FROM runtime.runtime_jobs WHERE job_id=%s", [body["jobId"]])
        if cursor.fetchone()[0] not in {"execute", "control"}:
            raise SessionWorkspaceError("session_workspace_preservation_authority_rejected")
    session = run.session
    if (run.status not in {"completed", "failed", "cancelled"}
            or session.status != "active" or session.purgedAt is not None
            or session.agent.status != "active" or session.agent.purgedAt is not None
            or run.workspace.status != "active" or session.current_environment_id != environment.pk
            or environment.controller_released or environment.controller_purpose != "cleanup"
            or environment.controller_run_id != run.pk
            or environment.controller_lease_owner != body["leaseOwner"]
            or run.authorization.digest != body["authorizationDigest"]):
        raise SessionWorkspaceError("session_workspace_preservation_authority_rejected")
    if body["baseline"] != run.authorization.payload["sessionWorkspace"]:
        raise SessionWorkspaceError("session_workspace_baseline_conflict")
    supplied = body["environmentAuthorization"]
    if (not isinstance(supplied, dict) or set(supplied) != {"payload", "signature"}
            or not isinstance(supplied["payload"], dict)
            or supplied["payload"].get("executionMode") not in {"perRun", "retained"}):
        raise SessionWorkspaceError("session_workspace_preservation_authority_rejected")
    expected = build_environment_authorization(run, run.authorization, _authority(environment),
        execution_mode=supplied["payload"]["executionMode"])
    if (supplied["payload"] != expected["payload"] or not isinstance(supplied["signature"], str)
            or not secrets.compare_digest(supplied["signature"], expected["signature"])):
        raise SessionWorkspaceError("session_workspace_preservation_authority_rejected")
    with connection.cursor() as cursor:
        cursor.execute("SELECT i.agent_run_id,i.authorization_digest,i.image_digest,i.profile_digest "
            "FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s "
            "ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id "
            "AND s.environment_id=i.environment_id AND s.host_id=i.host_id "
            "WHERE i.execution_id=%s AND i.allocation_execution_id=i.execution_id AND i.instance_id=%s "
            "AND i.environment_id=%s AND i.session_id=%s AND i.allocation_mode='retained' "
            "AND s.workspace_id=%s AND s.container_id=%s AND s.allocation_mode='retained' "
            "AND s.state IN ('resident','releasing') FOR SHARE OF i,s NOWAIT",
            [body["allocationExecutionId"], body["instanceId"], environment.pk, session.pk,
             session.workspace_id, body["containerId"]])
        allocation = cursor.fetchone()
    if allocation is None:
        raise SessionWorkspaceError("session_workspace_preservation_target_rejected")
    origin = AgentRun.objects.select_related("session", "environment").get(pk=allocation[0])
    grant = AgentRunAuthorization.objects.get(agent_run=origin)
    profile = environment.environment_profile
    if (origin.session_id != session.pk or origin.environment_id != environment.pk
            or grant.digest != allocation[1] or authorization_digest(grant.payload) != grant.digest
            or not agent_run_binding_matches(grant.payload, origin)
            or profile is None or profile["imageDigest"] != allocation[2]
            or environment.environment_profile_digest != allocation[3]
            or allocation[3] != body["profileDigest"]):
        raise SessionWorkspaceError("session_workspace_preservation_target_rejected")
    verify_agent_run_authorization_signature(grant.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, grant.signature)
    run._preservation_origin = (origin.pk, grant.digest)
    return run, session, body["baseline"]


def _receipt_identity(body, run, session, candidate, storage_key):
    identity = {"session_id": session.pk, "agent_run_id": run.pk, "environment_id": run.environment_id,
        "authorization_digest": run.authorization.digest,
        "origin_agent_run_id": run._preservation_origin[0],
        "origin_authorization_digest": run._preservation_origin[1],
        "allocation_execution_id": body["allocationExecutionId"], "instance_id": body["instanceId"],
        "container_id": body["containerId"], "profile_digest": body["profileDigest"],
        "baseline": body["baseline"], "workspace": candidate, "storage_key": storage_key,
        "snapshot_encoding": "frameV1"}
    receipt_id = "sha256:" + hashlib.sha256(json.dumps(identity, sort_keys=True,
        separators=(",", ":"), ensure_ascii=False).encode("utf-8")).hexdigest()
    return receipt_id, identity


def _is_preservation_replay(body, run, session, candidate, storage_key):
    if not session.workspaceBaselineKnown or session.workspaceGeneration <= candidate["generation"]:
        return False
    receipt_id, identity = _receipt_identity(body, run, session, candidate, storage_key)
    receipt = SessionWorkspacePreservation.objects.filter(pk=receipt_id).first()
    return receipt is not None and all(getattr(receipt, key) == value for key, value in identity.items())


def _record_preservation(body, run, session, candidate, storage_key):
    from .http.internal import SessionWorkspaceError
    receipt_id, identity = _receipt_identity(body, run, session, candidate, storage_key)
    receipt, created = SessionWorkspacePreservation.objects.get_or_create(pk=receipt_id, defaults={**identity,
        "creator_lease_digest": body["environmentAuthorization"]["payload"]["controllerLeaseDigest"],
        "creator_controller_epoch": body["environmentAuthorization"]["payload"]["controllerEpoch"]})
    if any(getattr(receipt, key) != value for key, value in identity.items()):
        raise SessionWorkspaceError("session_workspace_preservation_receipt_conflict")
    return receipt_id


def preserve_session_workspace(request):
    from .http import internal
    try:
        body = internal._workspace_snapshot_upload_request(request, SCHEMA,
            internal.SESSION_WORKSPACE_COMMIT_FIELDS | TARGET_FIELDS, "session_workspace_preserve_invalid")
        validate_session_workspace(body["baseline"])
        require_opaque_ref("allocationExecutionId", body["allocationExecutionId"])
        require_sha256("profileDigest", body["profileDigest"])
        if (not isinstance(body["instanceId"], str) or not re.fullmatch(r"instance_[0-9a-f]{64}", body["instanceId"])
                or not isinstance(body["containerId"], str) or not re.fullmatch(r"[0-9a-f]{64}", body["containerId"])):
            raise ValueError("session_workspace_preserve_invalid")
        with ExitStack() as uploads:
            with transaction.atomic():
                run, session, baseline = _locked_preservation(body)
                budget = run.authorization.payload["environmentProfile"]["resources"]["dataTmpfsBytes"]
                if (body["snapshotSizeBytes"] > budget or body["expandedSizeBytes"] > budget
                        or not internal._workspace_tmpfs_fits(run, body["expandedSizeBytes"])):
                    raise internal.SessionWorkspaceError("session_workspace_preserve_invalid", 400)
                candidate = {"generation": baseline["generation"] + 1,
                    **{name: body[name] for name in ("snapshotSha256", "snapshotSizeBytes", "expandedSizeBytes", "fileCount")}}
                storage_key = ("" if candidate["snapshotSizeBytes"] == 0 else
                    f"workspaces/{run.workspace_id}/sessions/{session.pk}/snapshots/{candidate['generation']}/"
                    f"{candidate['snapshotSha256'].removeprefix('sha256:')}.snapshot")
                if (session_workspace_for_session(session) != baseline
                        and not internal._is_workspace_commit_replay(session, candidate, run, storage_key)
                        and not _is_preservation_replay(body, run, session, candidate, storage_key)):
                    raise internal.SessionWorkspaceError("session_workspace_baseline_conflict")
                upload = uploads.enter_context(internal._workspace_snapshot_upload(storage_key,
                    body=body, agent_run=run, session=session))
            internal._store_workspace_snapshot(request, candidate, storage_key, upload, body,
                lock_workspace=_locked_preservation)
            receipt_id = None
            def record(run, session, candidate, storage_key):
                nonlocal receipt_id
                receipt_id = _record_preservation(body, run, session, candidate, storage_key)
            disposition = internal._publish_workspace_snapshot(body, candidate, storage_key, upload,
                advance_session=True, lock_workspace=_locked_preservation, record_commit=record,
                is_commit_replay=lambda run, session, candidate, key:
                    _is_preservation_replay(body, run, session, candidate, key))
    except internal.SessionWorkspaceError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except DatabaseError as error:
        code = "environment_controller_busy" if getattr(error.__cause__, "sqlstate", None) == "55P03" else "session_workspace_preserve_unavailable"
        return JsonResponse({"error": code}, status=409 if code == "environment_controller_busy" else 503)
    except (ValueError, TypeError, KeyError, AgentRun.DoesNotExist, AgentRunAuthorization.DoesNotExist):
        return JsonResponse({"error": "session_workspace_preservation_authority_rejected"}, status=409)
    except Exception:
        internal.logger.exception("Session workspace preservation failed")
        return JsonResponse({"error": "session_workspace_preserve_failed"}, status=500)
    return JsonResponse({"schema": RESULT_SCHEMA, "disposition": disposition, "sessionWorkspace": candidate,
        "preservationReceiptId": receipt_id})
