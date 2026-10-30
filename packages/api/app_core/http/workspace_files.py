"""Authenticated file content below immutable Session workspace manifests."""
import json

from asgiref.sync import sync_to_async
from django.db import DatabaseError, transaction
from django.http import JsonResponse

from app_core import workspace_file_objects as objects
from app_core.models import AgentRun, WorkspaceSnapshotFileReference
from app_core.runtime_contract import validate_session_workspace, require_opaque_ref
from app_core.workspace_snapshot_roots import (
    WorkspaceRootsUnavailable, request_workspace_snapshot_roots, require_current_roots,
)
from .json_body import decode_json_object
from .storage_stream import stored_file_response


MISSING = "runtime.workspace_files.missing.v1"
UPLOAD = "runtime.workspace_files.upload.v1"
DOWNLOAD = "runtime.workspace_files.download.v1"


def _identity(body, schema, fields):
    from . import internal
    expected = internal.SESSION_WORKSPACE_LEASE_FIELDS | fields
    if "preservation" in body:
        expected = expected | {"preservation"}
    if not isinstance(body, dict) or set(body) != expected:
        raise internal.SessionWorkspaceError("workspace_file_request_invalid", 400)
    internal._workspace_lease_request(
        {key: body[key] for key in internal.SESSION_WORKSPACE_LEASE_FIELDS}, schema,
        "workspace_file_request_invalid")
    return body


def _authorize(body):
    from . import internal
    proof = body.get("preservation")
    if proof is None:
        result = internal._locked_session_workspace_agent_run(body)
        internal.protection_observations.bind(sessionId=str(result[1].pk),
            runId=str(result[0].pk), jobId=body["jobId"])
        return result
    from app_core.session_workspace_preservation import _locked_preservation, SCHEMA, TARGET_FIELDS
    if (not isinstance(proof, dict)
            or set(proof) != internal.SESSION_WORKSPACE_COMMIT_FIELDS | TARGET_FIELDS
            or proof.get("schema") != SCHEMA
            or any(proof.get(field) != body[field] for field in
                   ("jobId", "leaseOwner", "agentRunId", "authorizationDigest"))):
        raise internal.SessionWorkspaceError("workspace_file_preservation_invalid", 400)
    result = _locked_preservation(proof)
    internal.protection_observations.bind(sessionId=str(result[1].pk),
        runId=str(result[0].pk), jobId=body["jobId"])
    return result


def _failure(error):
    from . import internal
    if isinstance(error, (internal.SessionWorkspaceError, objects.WorkspaceFileError)):
        return JsonResponse({"error": error.code}, status=error.status)
    if isinstance(error, WorkspaceRootsUnavailable):
        return JsonResponse({"error": "workspace_file_roots_unavailable"}, status=503)
    if isinstance(error, DatabaseError):
        busy = getattr(error.__cause__, "sqlstate", None) == "55P03"
        return JsonResponse({"error": "environment_controller_busy" if busy else "workspace_file_unavailable"},
                            status=409 if busy else 503)
    if isinstance(error, (ValueError, TypeError, KeyError, UnicodeError)):
        return JsonResponse({"error": "workspace_file_request_invalid"}, status=400)
    internal.logger.exception("Workspace file operation failed")
    return JsonResponse({"error": "workspace_file_operation_failed"}, status=500)


def missing_files(request):
    try:
        body = _identity(decode_json_object(request), MISSING, {"files"})
        with transaction.atomic():
            run, session, *_ = _authorize(body)
            from . import internal
            descriptors = objects._descriptors(body["files"])
            if not internal._workspace_budget_fits(run, sum(file["sizeBytes"] for file in descriptors)):
                raise internal.SessionWorkspaceError("workspace_file_budget_exceeded", 400)
            missing = objects.reserve_files(session, run, body, descriptors)
        return JsonResponse({"schema": "runtime.workspace_files.missing.result.v1",
                             "missing": [file["sha256"] for file in missing]})
    except Exception as error:
        return _failure(error)


def upload_file(request):
    from . import internal
    try:
        prefix = internal._read_workspace_bytes(request, 4)
        size = int.from_bytes(prefix, "big")
        if not 0 < size <= internal.MAX_SESSION_WORKSPACE_METADATA_BYTES:
            raise ValueError("metadata length")
        body = _identity(json.loads(internal._read_workspace_bytes(request, size).decode("utf-8")),
                         UPLOAD, {"sha256", "sizeBytes"})
        if type(body["sizeBytes"]) is not int or body["sizeBytes"] < 0:
            raise ValueError("file length")
        if int(request.META.get("CONTENT_LENGTH", -1)) != 4 + size + body["sizeBytes"]:
            raise ValueError("content length")
        result = objects.upload_file(request, body,
            {"sha256": body["sha256"], "sizeBytes": body["sizeBytes"]}, _authorize)
        return JsonResponse({"schema": "runtime.workspace_files.upload.result.v1", **result}, status=201)
    except Exception as error:
        return _failure(error)


def _selected_source(body):
    from . import internal
    roots = None
    if body["checkpointId"] is not None:
        with transaction.atomic():
            _run, session, _frozen = _authorize(body)
        roots = request_workspace_snapshot_roots(session)
    return roots


def _open_authorized(body):
    from . import internal
    roots = _selected_source(body)
    with transaction.atomic():
        run, session, frozen = _authorize(body)
        if body["checkpointId"] is None:
            internal._require_workspace_baseline(session, frozen)
            snapshot = {key: value for key, value in frozen.items() if key != "generation"}
            key = session.workspaceStorageKey
        else:
            require_current_roots(session, roots)
            snapshot = internal._recovery_checkpoint_workspace(body, run)
            selected = {"agentRunId": run.pk, "authorizationDigest": body["authorizationDigest"],
                        "checkpointId": body["checkpointId"], **snapshot}
            if selected not in roots["roots"]:
                raise internal.SessionWorkspaceError("execution_workspace_checkpoint_unreachable")
            key = snapshot.pop("objectRef")
        if snapshot != body["snapshot"]:
            raise internal.SessionWorkspaceError("workspace_file_snapshot_rejected")
        linked = WorkspaceSnapshotFileReference.objects.filter(
            snapshot__session=session, snapshot__storageKey=key, snapshot__state="live",
            snapshot__snapshotEncoding=objects.FILE_MANIFEST_ENCODING,
            file__sha256=body["sha256"], file__sizeBytes=body["sizeBytes"]).exists()
        if not linked:
            raise internal.SessionWorkspaceError("workspace_file_reference_not_found", 404)
        return objects.open_file(session, body["sha256"])


async def download_file(request):
    try:
        body = _identity(decode_json_object(request), DOWNLOAD,
                         {"checkpointId", "snapshot", "sha256", "sizeBytes"})
        validate_session_workspace({"generation": 1, **body["snapshot"]})
        objects._descriptors([{"sha256": body["sha256"], "sizeBytes": body["sizeBytes"]}])
        if body["checkpointId"] is not None:
            require_opaque_ref("checkpointId", body["checkpointId"])
        # Metadata lookup supplies the response key only; authorized_open owns
        # the current lease/source proof and opens the actual content handle.
        session = await sync_to_async(lambda: AgentRun.objects.select_related("session").get(
            pk=body["agentRunId"]).session, thread_sensitive=True)()
        key = objects.file_key(session, body["sha256"])
        async def authorized_open():
            return await sync_to_async(_open_authorized, thread_sensitive=True)(body)
        response = await stored_file_response(key, "application/octet-stream", "workspace.file",
            as_attachment=False, content_length=body["sizeBytes"], authorized_open=authorized_open)
        if response.status_code == 200:
            response["X-Content-Sha256"] = body["sha256"]
        return response
    except Exception as error:
        return _failure(error)
