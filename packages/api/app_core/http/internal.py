import hashlib
import json
import logging
import os
import tempfile
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass

from asgiref.sync import sync_to_async
from django.conf import settings
from django.core.files.storage import default_storage
from django.db import connection, transaction
from django.db.models import Q
from django.http import JsonResponse
from django.utils import timezone
from django.utils.dateparse import parse_datetime
from ninja import Router

from app_core.artifact_publish import (
    ArtifactPublishError,
    publication_response,
    publish_artifact as publish_artifact_operation,
    published_artifact,
)
from app_core.assets import DeferredInputResolutionError, _sync_directory
from app_core.deferred_input import (
    DeferredInputBindingError,
    resolve_deferred_input as resolve_deferred_input_operation,
    resolved_input_storage,
    input_storage_batch,
)
from app_core.models import Agent, Session, AgentRun
from app_core.material_contract import KnowledgeError
from app_core.runtime_contract import (
    agent_run_binding_matches,
    authorization_digest,
    require_opaque_ref,
    require_sha256,
    require_string,
    session_workspace_for_session,
    validate_session_workspace,
    validate_virtual_path,
    _verify_authorization_digest_signature,
)
from app_core.runtime_client import (
    build_agent_run_start,
    agent_run_lifecycle_job_id,
    schedule_agent_run_lifecycle,
    request_agent_run_cancellation,
)
from app_core.runtime_job_client import get_runtime_job
from app_core.session_event import (
    committed_session_terminal_state,
    project_committed_agent_run,
)
from app_core.workspace_access import agent_run_membership_is_current

from .json_body import decode_json_object
from .security import internal_token_auth
from .storage_stream import StoredObjectUnavailable, stored_file_response


logger = logging.getLogger(__name__)
router = Router(tags=["internal"], by_alias=True)
AGENT_RUN_LIFECYCLE_RECONCILE_LIMIT = 100
AGENT_RUN_LIFECYCLE_DEAD_LETTER_REASON = "agent_run_lifecycle_dead_lettered"
SESSION_WORKSPACE_RESOLVE_SCHEMA = "runtime.session_workspace.resolve.v1"
SESSION_WORKSPACE_RESOLVED_SCHEMA = "runtime.session_workspace.resolved.v1"
SESSION_WORKSPACE_DOWNLOAD_SCHEMA = "runtime.session_workspace.download.v1"
SESSION_WORKSPACE_COMMIT_SCHEMA = "runtime.session_workspace.commit.v1"
SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA = "runtime.session_workspace.commit.result.v1"
EXECUTION_WORKSPACE_STAGE_SCHEMA = "runtime.execution_workspace.stage.v1"
EXECUTION_WORKSPACE_STAGE_RESULT_SCHEMA = "runtime.execution_workspace.stage.result.v1"
EXECUTION_WORKSPACE_DOWNLOAD_SCHEMA = "runtime.execution_workspace.download.v1"
SESSION_WORKSPACE_LEASE_FIELDS = {
    "schema",
    "jobId",
    "leaseOwner",
    "agentRunId",
    "authorizationDigest",
}
SESSION_WORKSPACE_COMMIT_FIELDS = SESSION_WORKSPACE_LEASE_FIELDS | {
    "snapshotSha256",
    "snapshotSizeBytes",
    "expandedSizeBytes",
    "fileCount",
}
EXECUTION_WORKSPACE_STAGE_FIELDS = SESSION_WORKSPACE_COMMIT_FIELDS | {
    "checkpointId",
}
EXECUTION_WORKSPACE_DOWNLOAD_FIELDS = SESSION_WORKSPACE_LEASE_FIELDS | {
    "checkpointId",
}
MAX_SESSION_WORKSPACE_METADATA_BYTES = 64 * 1024
MAX_SESSION_WORKSPACE_FILE_COUNT = 2_147_483_647
MAX_SESSION_WORKSPACE_MANIFEST_BYTES = 1024 * 1024
MAX_SESSION_WORKSPACE_PATH_BYTES = 4 * 1024
MAX_SESSION_WORKSPACE_PATH_DEPTH = 64
SESSION_WORKSPACE_SNAPSHOT_SCHEMA = "workspace.snapshot.v1"
SESSION_WORKSPACE_RESTORE_OVERHEAD_BYTES = 64 * 1024


def _internal_post(path: str):
    return router.post(
        path,
        auth=internal_token_auth,
        response=None,
        include_in_schema=False,
    )


class SessionWorkspaceError(Exception):
    def __init__(self, code: str, status: int = 409):
        super().__init__(code)
        self.code = code
        self.status = status


def _workspace_lease_request(body: dict, schema: str, code: str) -> dict:
    if not isinstance(body, dict) or set(body) != SESSION_WORKSPACE_LEASE_FIELDS:
        raise SessionWorkspaceError(code, 400)
    if body["schema"] != schema:
        raise SessionWorkspaceError(code, 400)
    try:
        require_opaque_ref("agentRunId", body["agentRunId"])
        require_sha256("authorizationDigest", body["authorizationDigest"])
        require_string("jobId", body["jobId"])
        if (
            not isinstance(body["leaseOwner"], str)
            or not 16 <= len(body["leaseOwner"].encode("utf-8")) <= 160
            or any(ord(character) < 32 or 127 <= ord(character) < 160 for character in body["leaseOwner"])
            or body["jobId"] != agent_run_lifecycle_job_id(body["agentRunId"])
        ):
            raise ValueError
    except (TypeError, ValueError):
        raise SessionWorkspaceError(code, 400) from None
    return body


def _workspace_snapshot_upload_request(
    request, schema: str, fields: set[str], code: str
) -> dict:
    try:
        content_length = int(request.META.get("CONTENT_LENGTH"))
    except (TypeError, ValueError):
        raise SessionWorkspaceError(code, 400) from None
    if content_length < 4:
        raise SessionWorkspaceError(code, 400)
    prefix = _read_workspace_bytes(request, 4)
    metadata_length = int.from_bytes(prefix, "big")
    if not 0 < metadata_length <= MAX_SESSION_WORKSPACE_METADATA_BYTES:
        raise SessionWorkspaceError(code, 400)
    try:
        body = json.loads(_read_workspace_bytes(request, metadata_length).decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise SessionWorkspaceError(code, 400) from None
    if not isinstance(body, dict) or set(body) != fields:
        raise SessionWorkspaceError(code, 400)
    lease = _workspace_lease_request(
        {name: body[name] for name in SESSION_WORKSPACE_LEASE_FIELDS},
        schema,
        code,
    )
    candidate = {
        "generation": 1,
        "snapshotSha256": body["snapshotSha256"],
        "snapshotSizeBytes": body["snapshotSizeBytes"],
        "expandedSizeBytes": body["expandedSizeBytes"],
        "fileCount": body["fileCount"],
    }
    try:
        validate_session_workspace(candidate)
        if candidate["fileCount"] > MAX_SESSION_WORKSPACE_FILE_COUNT:
            raise ValueError
        if content_length != 4 + metadata_length + candidate["snapshotSizeBytes"]:
            raise ValueError
    except (TypeError, ValueError):
        raise SessionWorkspaceError(code, 400) from None
    return lease | {name: body[name] for name in fields - SESSION_WORKSPACE_LEASE_FIELDS}


def _workspace_commit_request(request) -> dict:
    return _workspace_snapshot_upload_request(
        request,
        SESSION_WORKSPACE_COMMIT_SCHEMA,
        SESSION_WORKSPACE_COMMIT_FIELDS,
        "session_workspace_commit_invalid",
    )


def _read_workspace_bytes(request, size: int) -> bytes:
    chunks = []
    remaining = size
    while remaining:
        chunk = request.read(remaining)
        if not chunk:
            raise SessionWorkspaceError("session_workspace_commit_invalid", 400)
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


class _WorkspaceSnapshotReader:
    def __init__(self, source, candidate: dict):
        self.source = source
        self.candidate = candidate
        self.remaining = candidate["snapshotSizeBytes"]
        self.digest = hashlib.sha256()
        self.header = bytearray()
        self.manifest = bytearray()
        self.manifestLength = None
        self.files = None
        self.fileIndex = 0
        self.fileRemaining = 0
        self.fileDigest = None

    def read(self, size: int = -1) -> bytes:
        if self.remaining == 0:
            return b""
        requested = self.remaining if size is None or size < 0 else min(size, self.remaining)
        chunk = self.source.read(requested)
        if not chunk:
            raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
        self.remaining -= len(chunk)
        self.digest.update(chunk)
        self._consume(chunk)
        return chunk

    def _consume(self, chunk: bytes) -> None:
        offset = 0
        while offset < len(chunk):
            if len(self.header) < 4:
                count = min(4 - len(self.header), len(chunk) - offset)
                self.header.extend(chunk[offset : offset + count])
                offset += count
                if len(self.header) != 4:
                    continue
                self.manifestLength = int.from_bytes(self.header, "big")
                if not 0 < self.manifestLength <= MAX_SESSION_WORKSPACE_MANIFEST_BYTES:
                    raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
                if 4 + self.manifestLength > self.candidate["snapshotSizeBytes"]:
                    raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
            if len(self.manifest) < self.manifestLength:
                count = min(self.manifestLength - len(self.manifest), len(chunk) - offset)
                self.manifest.extend(chunk[offset : offset + count])
                offset += count
                if len(self.manifest) != self.manifestLength:
                    continue
                self.files = _workspace_snapshot_manifest(
                    bytes(self.manifest), self.candidate
                )
                self._advance_empty_files()
                continue
            self._advance_empty_files()
            if self.fileIndex == len(self.files):
                raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
            count = min(self.fileRemaining, len(chunk) - offset)
            self.fileDigest.update(chunk[offset : offset + count])
            self.fileRemaining -= count
            offset += count
            self._advance_empty_files()

    def _advance_empty_files(self) -> None:
        while self.files is not None and self.fileIndex < len(self.files):
            if self.fileDigest is None:
                self.fileRemaining = self.files[self.fileIndex]["sizeBytes"]
                self.fileDigest = hashlib.sha256()
            if self.fileRemaining != 0:
                return
            if (
                f"sha256:{self.fileDigest.hexdigest()}"
                != self.files[self.fileIndex]["sha256"]
            ):
                raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
            self.fileIndex += 1
            self.fileDigest = None

    def require_complete(self) -> None:
        if (
            self.remaining != 0
            or self.files is None
            or self.fileIndex != len(self.files)
            or f"sha256:{self.digest.hexdigest()}" != self.candidate["snapshotSha256"]
            or self.source.read(1)
        ):
            raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)


@dataclass
class _WorkspaceSnapshotUpload:
    descriptor: int
    temporary_path: str
    validated: bool = False
    existing_verified: bool = False


@contextmanager
def _workspace_snapshot_upload(storage_key: str):
    """Create directory entries while the owning active Session is locked."""
    if not storage_key:
        yield None
        return
    parent = os.path.dirname(default_storage.path(storage_key))
    os.makedirs(parent, exist_ok=True)
    descriptor, temporary_path = tempfile.mkstemp(
        dir=parent, prefix=".centaeris-immutable-", suffix=".tmp"
    )
    upload = _WorkspaceSnapshotUpload(descriptor, temporary_path)
    try:
        yield upload
    finally:
        if upload.descriptor >= 0:
            os.close(upload.descriptor)
        try:
            os.unlink(temporary_path)
        except FileNotFoundError:
            # GC may unlink a purged owner's open temporary on POSIX.
            pass


def _store_workspace_snapshot(
    request, candidate: dict, storage_key: str, upload: _WorkspaceSnapshotUpload | None,
    body: dict,
) -> None:
    if not candidate["snapshotSizeBytes"]:
        _require_workspace_eof(request)
        return
    reader = _WorkspaceSnapshotReader(request, candidate)
    if default_storage.exists(storage_key):
        _verify_owned_workspace_snapshot(body, candidate, storage_key)
        while reader.read(64 * 1024):
            pass
        reader.require_complete()
        upload.validated = True
        upload.existing_verified = True
        return

    descriptor = upload.descriptor
    upload.descriptor = -1
    with os.fdopen(descriptor, "wb") as temporary:
        while chunk := reader.read(64 * 1024):
            temporary.write(chunk)
        reader.require_complete()
        temporary.flush()
        os.fsync(temporary.fileno())
    upload.validated = True


def _link_workspace_snapshot(storage_key: str, upload: _WorkspaceSnapshotUpload | None) -> bool:
    """Publish only under the final active-Session fence; collisions verify outside it."""
    if not storage_key:
        return True
    if upload is None or not upload.validated:
        raise RuntimeError("workspace snapshot upload was not validated")
    if upload.existing_verified:
        return True
    final_path = default_storage.path(storage_key)
    try:
        os.link(upload.temporary_path, final_path)
    except FileExistsError:
        return False
    _sync_directory(os.path.dirname(final_path))
    return True


def _publish_workspace_snapshot(
    body: dict, candidate: dict, storage_key: str, upload: _WorkspaceSnapshotUpload | None,
    *, advance_session: bool,
) -> str:
    def publish():
        with transaction.atomic():
            agent_run, session, frozen = _locked_session_workspace_agent_run(body)
            replay = advance_session and _is_workspace_commit_replay(
                session, candidate, agent_run, storage_key,
            )
            if advance_session and not replay:
                _require_workspace_baseline(session, frozen)
            if not _link_workspace_snapshot(storage_key, upload):
                return None
            if not advance_session:
                return "staged"
            if replay:
                return "idempotent"
            session.workspaceGeneration = candidate["generation"]
            session.workspaceStorageKey = storage_key
            session.workspaceSnapshotSha256 = candidate["snapshotSha256"]
            session.workspaceSnapshotSizeBytes = candidate["snapshotSizeBytes"]
            session.workspaceExpandedSizeBytes = candidate["expandedSizeBytes"]
            session.workspaceFileCount = candidate["fileCount"]
            session.workspaceLastAdvancedAgentRun = agent_run
            session.save(update_fields=[
                "workspaceGeneration", "workspaceStorageKey", "workspaceSnapshotSha256",
                "workspaceSnapshotSizeBytes", "workspaceExpandedSizeBytes", "workspaceFileCount",
                "workspaceLastAdvancedAgentRun", "updatedAt",
            ])
            return "committed"

    disposition = publish()
    if disposition is None:
        # A concurrent immutable writer won the same key. Full verification
        # happens without row locks; publication then rechecks all owner facts.
        _verify_owned_workspace_snapshot(body, candidate, storage_key)
        upload.existing_verified = True
        disposition = publish()
    return disposition


def _verify_owned_workspace_snapshot(body: dict, candidate: dict, storage_key: str) -> None:
    try:
        _verify_workspace_snapshot(candidate, storage_key)
    except FileNotFoundError:
        # Permanent deletion may reclaim an existing key during the unlocked
        # verification phase. Preserve the owner/lease conflict instead of 500.
        with transaction.atomic():
            _locked_session_workspace_agent_run(body)
        raise SessionWorkspaceError("session_workspace_snapshot_invalid") from None


def _verify_workspace_snapshot(candidate: dict, storage_key: str) -> None:
    digest = hashlib.sha256()
    size = 0
    with default_storage.open(storage_key, "rb") as source:
        while chunk := source.read(64 * 1024):
            size += len(chunk)
            if size > candidate["snapshotSizeBytes"]:
                raise SessionWorkspaceError("session_workspace_snapshot_invalid")
            digest.update(chunk)
    if (
        size != candidate["snapshotSizeBytes"]
        or f"sha256:{digest.hexdigest()}" != candidate["snapshotSha256"]
    ):
        raise SessionWorkspaceError("session_workspace_snapshot_invalid")


def _workspace_snapshot_manifest(manifest_bytes: bytes, candidate: dict) -> list[dict]:
    try:
        manifest = json.loads(manifest_bytes.decode("utf-8"))
        canonical = json.dumps(
            manifest, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400) from None
    if canonical != manifest_bytes or not isinstance(manifest, dict) or set(manifest) != {
        "schema",
        "files",
    }:
        raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
    files = manifest["files"]
    if manifest["schema"] != SESSION_WORKSPACE_SNAPSHOT_SCHEMA or not isinstance(files, list):
        raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
    if len(files) != candidate["fileCount"] or not files:
        raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
    previous = None
    expanded_size_bytes = 0
    for file in files:
        if not isinstance(file, dict) or set(file) != {
            "path",
            "sizeBytes",
            "sha256",
            "executable",
        }:
            raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
        try:
            validate_virtual_path(file["path"])
            require_sha256("workspace snapshot file sha256", file["sha256"])
        except ValueError:
            raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400) from None
        if (
            len(file["path"].encode("utf-8")) > MAX_SESSION_WORKSPACE_PATH_BYTES
            or len(file["path"].split("/")) > MAX_SESSION_WORKSPACE_PATH_DEPTH
            or not isinstance(file["sizeBytes"], int)
            or isinstance(file["sizeBytes"], bool)
            or file["sizeBytes"] < 0
            or not isinstance(file["executable"], bool)
            or previous is not None
            and (
                previous >= file["path"]
                or file["path"].startswith(f"{previous}/")
            )
        ):
            raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
        expanded_size_bytes += file["sizeBytes"]
        if expanded_size_bytes > candidate["expandedSizeBytes"]:
            raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
        previous = file["path"]
    if expanded_size_bytes != candidate["expandedSizeBytes"]:
        raise SessionWorkspaceError("session_workspace_snapshot_invalid", 400)
    return files


def _require_workspace_eof(request) -> None:
    if request.read(1):
        raise SessionWorkspaceError("session_workspace_commit_invalid", 400)


def _locked_session_workspace_agent_run(body: dict) -> tuple[AgentRun, Session, dict]:
    lease_query = (
        "SELECT session_id FROM runtime.runtime_jobs "
        "WHERE job_id=%s AND job_kind='agent_run.lifecycle' AND status='running' "
        "AND lease_owner=%s AND payload_ref=%s AND idempotency_key=%s "
        "AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint"
    )
    lease_parameters = [
        body["jobId"], body["leaseOwner"], f"record:agent_run:{body['agentRunId']}",
        f"agent_run.lifecycle:{body['agentRunId']}:{body['authorizationDigest']}",
    ]
    with connection.cursor() as cursor:
        cursor.execute(lease_query, lease_parameters)
        job = cursor.fetchone()
    if job is None:
        raise SessionWorkspaceError("session_workspace_lease_lost")
    # Public deletion and GC lock Agent before Session. A parent purge leaves
    # child state unchanged, so lock and recheck both owners before snapshot I/O.
    # Then retain the existing Run/lease order and recheck the lease after waiting.
    agent_id = Session.objects.filter(id=job[0]).values_list("agent_id", flat=True).first()
    agent = Agent.objects.select_for_update().filter(id=agent_id).first()
    session = Session.objects.select_for_update().filter(id=job[0]).first()
    try:
        agent_run = (
            AgentRun.objects.select_for_update(of=("self",))
            .select_related("authorization")
            .get(id=body["agentRunId"])
        )
    except AgentRun.DoesNotExist:
        raise SessionWorkspaceError("session_workspace_agent_run_not_found") from None
    with connection.cursor() as cursor:
        cursor.execute(lease_query + " FOR UPDATE", lease_parameters)
        locked_job = cursor.fetchone()
    if locked_job is None or locked_job != job:
        raise SessionWorkspaceError("session_workspace_lease_lost")
    if not agent_run_membership_is_current(agent_run):
        raise SessionWorkspaceError("session_workspace_agent_run_not_found")
    if job[0] != agent_run.session_id:
        raise SessionWorkspaceError("session_workspace_lease_lost")
    if session is None:
        raise SessionWorkspaceError("session_workspace_session_unavailable")
    try:
        authorization = agent_run.authorization
        digest = authorization_digest(authorization.payload)
        _verify_authorization_digest_signature(
            digest,
            settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
            authorization.signature,
        )
        if (
            digest != authorization.digest
            or body["authorizationDigest"] != authorization.digest
            or not agent_run_binding_matches(authorization.payload, agent_run, session=session)
        ):
            raise ValueError
    except (AttributeError, ValueError):
        raise SessionWorkspaceError("session_workspace_authorization_invalid") from None
    if (
        agent is None
        or agent.id != session.agent_id
        or agent.workspace_id != session.workspace_id
        or agent.status != "active"
        or agent.purgedAt is not None
        or agent_run.status not in {"queued", "running"}
        or session.status != "active"
        or session.purgedAt is not None
    ):
        raise SessionWorkspaceError("session_workspace_session_unavailable")
    return agent_run, session, authorization.payload["sessionWorkspace"]


def _require_workspace_baseline(session: Session, frozen: dict) -> None:
    if session_workspace_for_session(session) != frozen:
        raise SessionWorkspaceError("session_workspace_baseline_conflict")


def _recovery_checkpoint_workspace(body: dict, agent_run: AgentRun) -> dict:
    with connection.cursor() as cursor:
        cursor.execute(
            "SELECT payload_json FROM runtime.checkpoints "
            "WHERE checkpoint_id=%s AND kind='recovery' AND status='committed'",
            [body["checkpointId"]],
        )
        row = cursor.fetchone()
    if row is None:
        raise SessionWorkspaceError("execution_workspace_checkpoint_not_found", 404)
    try:
        payload = json.loads(row[0])
        if (
            payload["schema"] != "runtime.recovery_checkpoint.v1"
            or payload["checkpointId"] != body["checkpointId"]
            or payload["agentRunId"] != body["agentRunId"]
            or payload["sessionId"] != str(agent_run.session_id)
            or payload["authorizationDigest"] != body["authorizationDigest"]
        ):
            raise ValueError
        snapshot = payload["workspaceSnapshot"]
        candidate = {
            "generation": 1,
            "snapshotSha256": snapshot["snapshotSha256"],
            "snapshotSizeBytes": snapshot["snapshotSizeBytes"],
            "expandedSizeBytes": snapshot["expandedSizeBytes"],
            "fileCount": snapshot["fileCount"],
        }
        validate_session_workspace(candidate)
        require_string("objectRef", snapshot["objectRef"])
    except (KeyError, TypeError, ValueError, json.JSONDecodeError):
        raise SessionWorkspaceError("execution_workspace_checkpoint_invalid") from None
    return snapshot


def _is_workspace_commit_replay(
    session: Session,
    candidate: dict,
    agent_run: AgentRun,
    storage_key: str,
) -> bool:
    return (
        session_workspace_for_session(session) == candidate
        and session.workspaceStorageKey == storage_key
        and session.workspaceLastAdvancedAgentRun_id == agent_run.id
    )


def _workspace_advanced_by_agent_run(session: Session, frozen: dict, agent_run: AgentRun) -> bool:
    current = session_workspace_for_session(session)
    return (
        current["generation"] == frozen["generation"] + 1
        and session.workspaceLastAdvancedAgentRun_id == agent_run.id
    )


def _workspace_tmpfs_fits(agent_run: AgentRun, expanded_size_bytes: int) -> bool:
    inputs_size_bytes = sum(
        item["sizeBytes"] for item in agent_run.authorization.payload["assetRefs"]
    )
    return (
        expanded_size_bytes
        + inputs_size_bytes
        + SESSION_WORKSPACE_RESTORE_OVERHEAD_BYTES
        <= agent_run.authorization.payload["resources"]["dataTmpfsBytes"]
    )


@_internal_post("/agent-runs/session-workspace/resolve")
def resolve_session_workspace(request):
    try:
        body = _workspace_lease_request(
            decode_json_object(request),
            SESSION_WORKSPACE_RESOLVE_SCHEMA,
            "session_workspace_resolve_invalid",
        )
        with transaction.atomic():
            agent_run, session, frozen = _locked_session_workspace_agent_run(body)
            if _workspace_advanced_by_agent_run(session, frozen, agent_run):
                disposition = "advanced"
                resolved = session_workspace_for_session(session)
            else:
                _require_workspace_baseline(session, frozen)
                disposition = "empty" if frozen["snapshotSizeBytes"] == 0 else "download"
                resolved = frozen
    except SessionWorkspaceError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except Exception:
        logger.exception("Session workspace resolve failed")
        return JsonResponse({"error": "session_workspace_resolve_failed"}, status=500)
    return JsonResponse(
        {
            "schema": SESSION_WORKSPACE_RESOLVED_SCHEMA,
            "disposition": disposition,
            "sessionWorkspace": resolved,
        }
    )


@_internal_post("/agent-runs/session-workspace/download")
async def download_session_workspace(request):
    prepared = await _prepare_session_workspace_download(request)
    if isinstance(prepared, JsonResponse):
        return prepared
    frozen, storage_key = prepared
    async def authorized_open():
        _frozen, handle = await _prepare_session_workspace_download(request, open_file=True)
        return handle

    try:
        response = await stored_file_response(
            storage_key,
            "application/vnd.centaeris.workspace-snapshot",
            f"workspace-{frozen['generation']}.snapshot",
            as_attachment=False,
            content_length=frozen["snapshotSizeBytes"],
            authorized_open=authorized_open,
        )
    except SessionWorkspaceError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except Exception:
        logger.exception("Session workspace download preparation failed")
        return JsonResponse({"error": "session_workspace_download_failed"}, status=500)
    if response.status_code == 200:
        response["X-Content-Sha256"] = frozen["snapshotSha256"]
    return response


@sync_to_async(thread_sensitive=True)
def _prepare_session_workspace_download(request, *, open_file=False):
    handle = None
    transferred = False
    try:
        body = _workspace_lease_request(
            decode_json_object(request),
            SESSION_WORKSPACE_DOWNLOAD_SCHEMA,
            "session_workspace_download_invalid",
        )
        with transaction.atomic():
            _run, session, frozen = _locked_session_workspace_agent_run(body)
            _require_workspace_baseline(session, frozen)
            if frozen["snapshotSizeBytes"] == 0 or not session.workspaceStorageKey:
                raise SessionWorkspaceError("session_workspace_snapshot_empty")
            if open_file:
                handle = _open_workspace_snapshot_handle(session.workspaceStorageKey)
        transferred = True
        return frozen, handle if open_file else session.workspaceStorageKey
    except SessionWorkspaceError as error:
        if open_file:
            raise
        return JsonResponse({"error": error.code}, status=error.status)
    except Exception:
        if open_file:
            raise
        logger.exception("Session workspace download preparation failed")
        return JsonResponse({"error": "session_workspace_download_failed"}, status=500)
    finally:
        if handle is not None and not transferred:
            handle.close()


def _open_workspace_snapshot_handle(storage_key):
    try:
        return default_storage.open(storage_key, "rb")
    except FileNotFoundError as error:
        raise StoredObjectUnavailable("stored_object_not_available") from error


@_internal_post("/agent-runs/session-workspace/commit")
def commit_session_workspace(request):
    try:
        body = _workspace_commit_request(request)
        with ExitStack() as uploads:
            with transaction.atomic():
                agent_run, session, frozen = _locked_session_workspace_agent_run(body)
                max_bytes = agent_run.authorization.payload["resources"]["dataTmpfsBytes"]
                if (
                    body["snapshotSizeBytes"] > max_bytes
                    or body["expandedSizeBytes"] > max_bytes
                    or not _workspace_tmpfs_fits(agent_run, body["expandedSizeBytes"])
                ):
                    raise SessionWorkspaceError("session_workspace_commit_invalid", 400)
                candidate = {
                    "generation": frozen["generation"] + 1,
                    "snapshotSha256": body["snapshotSha256"],
                    "snapshotSizeBytes": body["snapshotSizeBytes"],
                    "expandedSizeBytes": body["expandedSizeBytes"],
                    "fileCount": body["fileCount"],
                }
                storage_key = (
                    ""
                    if candidate["snapshotSizeBytes"] == 0
                    else (
                        f"workspaces/{agent_run.workspace_id}/sessions/{agent_run.session_id}/"
                        f"snapshots/{candidate['generation']}/"
                        f"{candidate['snapshotSha256'].removeprefix('sha256:')}.snapshot"
                    )
                )
                if (
                    session_workspace_for_session(session) != frozen
                    and not _is_workspace_commit_replay(session, candidate, agent_run, storage_key)
                ):
                    raise SessionWorkspaceError("session_workspace_baseline_conflict")
                upload = uploads.enter_context(_workspace_snapshot_upload(storage_key))

            _store_workspace_snapshot(request, candidate, storage_key, upload, body)
            disposition = _publish_workspace_snapshot(
                body, candidate, storage_key, upload, advance_session=True,
            )
    except SessionWorkspaceError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except Exception:
        logger.exception("Session workspace commit failed")
        return JsonResponse({"error": "session_workspace_commit_failed"}, status=500)
    return JsonResponse(
        {
            "schema": SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA,
            "disposition": disposition,
            "sessionWorkspace": candidate,
        },
        status=201 if disposition == "committed" else 200,
    )


@_internal_post("/agent-runs/execution-workspace/stage")
def stage_execution_workspace(request):
    try:
        body = _workspace_snapshot_upload_request(
            request,
            EXECUTION_WORKSPACE_STAGE_SCHEMA,
            EXECUTION_WORKSPACE_STAGE_FIELDS,
            "execution_workspace_stage_invalid",
        )
        require_opaque_ref("checkpointId", body["checkpointId"])
        candidate = {
            "generation": 1,
            "snapshotSha256": body["snapshotSha256"],
            "snapshotSizeBytes": body["snapshotSizeBytes"],
            "expandedSizeBytes": body["expandedSizeBytes"],
            "fileCount": body["fileCount"],
        }
        with ExitStack() as uploads:
            with transaction.atomic():
                agent_run, _session, _frozen = _locked_session_workspace_agent_run(body)
                if not _workspace_tmpfs_fits(agent_run, body["expandedSizeBytes"]):
                    raise SessionWorkspaceError("execution_workspace_stage_invalid", 400)
                storage_key = ""
                if candidate["snapshotSizeBytes"]:
                    checkpoint_key = hashlib.sha256(body["checkpointId"].encode("utf-8")).hexdigest()
                    storage_key = (
                        f"workspaces/{agent_run.workspace_id}/sessions/{agent_run.session_id}/"
                        f"agent-runs/{agent_run.id}/execution-checkpoints/{checkpoint_key}/"
                        f"{candidate['snapshotSha256'].removeprefix('sha256:')}.snapshot"
                    )
                upload = uploads.enter_context(_workspace_snapshot_upload(storage_key))
            _store_workspace_snapshot(request, candidate, storage_key, upload, body)
            _publish_workspace_snapshot(
                body, candidate, storage_key, upload, advance_session=False,
            )
    except SessionWorkspaceError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (TypeError, ValueError):
        return JsonResponse({"error": "execution_workspace_stage_invalid"}, status=400)
    except Exception:
        logger.exception("Execution workspace stage failed")
        return JsonResponse({"error": "execution_workspace_stage_failed"}, status=500)
    return JsonResponse(
        {
            "schema": EXECUTION_WORKSPACE_STAGE_RESULT_SCHEMA,
            "objectRef": storage_key or None,
            "snapshotSha256": candidate["snapshotSha256"],
            "snapshotSizeBytes": candidate["snapshotSizeBytes"],
            "expandedSizeBytes": candidate["expandedSizeBytes"],
            "fileCount": candidate["fileCount"],
        },
        status=201,
    )


@_internal_post("/agent-runs/execution-workspace/download")
async def download_execution_workspace(request):
    prepared = await _prepare_execution_workspace_download(request)
    if isinstance(prepared, JsonResponse):
        return prepared
    body = prepared
    async def authorized_open():
        _body, handle = await _prepare_execution_workspace_download(request, open_file=True)
        return handle

    try:
        response = await stored_file_response(
            body["objectRef"],
            "application/vnd.centaeris.workspace-snapshot",
            "execution-workspace.snapshot",
            as_attachment=False,
            content_length=body["snapshotSizeBytes"],
            authorized_open=authorized_open,
        )
    except SessionWorkspaceError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (TypeError, ValueError):
        return JsonResponse({"error": "execution_workspace_download_invalid"}, status=400)
    except Exception:
        logger.exception("Execution workspace download preparation failed")
        return JsonResponse({"error": "execution_workspace_download_failed"}, status=500)
    if response.status_code == 200:
        response["X-Content-Sha256"] = body["snapshotSha256"]
    return response


@sync_to_async(thread_sensitive=True)
def _prepare_execution_workspace_download(request, *, open_file=False):
    handle = None
    transferred = False
    try:
        body = decode_json_object(request)
        if not isinstance(body, dict) or set(body) != EXECUTION_WORKSPACE_DOWNLOAD_FIELDS:
            raise SessionWorkspaceError("execution_workspace_download_invalid", 400)
        _workspace_lease_request(
            {name: body[name] for name in SESSION_WORKSPACE_LEASE_FIELDS},
            EXECUTION_WORKSPACE_DOWNLOAD_SCHEMA,
            "execution_workspace_download_invalid",
        )
        require_opaque_ref("checkpointId", body["checkpointId"])
        with transaction.atomic():
            agent_run, _session, _frozen = _locked_session_workspace_agent_run(body)
            snapshot = _recovery_checkpoint_workspace(body, agent_run)
            body.update(snapshot)
            object_ref_prefix = (
                f"workspaces/{agent_run.workspace_id}/sessions/{agent_run.session_id}/"
                f"agent-runs/{agent_run.id}/execution-checkpoints/"
            )
            object_ref_suffix = body["objectRef"].removeprefix(object_ref_prefix)
            object_ref_parts = object_ref_suffix.split("/")
            checkpoint_key = object_ref_parts[0] if len(object_ref_parts) == 2 else ""
            expected_file = f"{body['snapshotSha256'].removeprefix('sha256:')}.snapshot"
            if (
                body["snapshotSizeBytes"] == 0
                or not body["objectRef"].startswith(object_ref_prefix)
                or len(checkpoint_key) != 64
                or any(character not in "0123456789abcdef" for character in checkpoint_key)
                or len(object_ref_parts) != 2
                or object_ref_parts[1] != expected_file
                or not default_storage.exists(body["objectRef"])
            ):
                raise SessionWorkspaceError("execution_workspace_download_invalid", 400)
            if open_file:
                handle = _open_workspace_snapshot_handle(body["objectRef"])
        transferred = True
        return (body, handle) if open_file else body
    except SessionWorkspaceError as error:
        if open_file:
            raise
        return JsonResponse({"error": error.code}, status=error.status)
    except (TypeError, ValueError):
        if open_file:
            raise
        return JsonResponse({"error": "execution_workspace_download_invalid"}, status=400)
    except Exception:
        if open_file:
            raise
        logger.exception("Execution workspace download preparation failed")
        return JsonResponse({"error": "execution_workspace_download_failed"}, status=500)
    finally:
        if handle is not None and not transferred:
            handle.close()


@_internal_post("/artifacts/publish")
def publish_artifact(request):
    try:
        publication, artifact = publish_artifact_operation(request)
    except ArtifactPublishError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except Exception:
        logger.exception("Artifact publication failed")
        return JsonResponse({"error": "artifact_publication_failed"}, status=500)
    return JsonResponse(publication_response(publication, artifact), status=201)


@_internal_post("/artifacts/status")
def artifact_status(request):
    try:
        result = published_artifact(decode_json_object(request))
    except ValueError:
        return JsonResponse({"error": "invalid_json"}, status=400)
    except ArtifactPublishError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except Exception:
        logger.exception("Artifact publication status failed")
        return JsonResponse({"error": "artifact_publication_status_failed"}, status=500)
    if result is None:
        return JsonResponse({"error": "artifact_publication_not_found"}, status=404)
    publication, artifact = result
    return JsonResponse(publication_response(publication, artifact))




@_internal_post("/mcp/credential")
def issue_platform_mcp_credential(request):
    from app_core.material_access import MaterialAccessContext
    from app_core.platform_mcp_auth import CredentialRejected, issue_credential

    response_headers = {"Cache-Control": "no-store"}
    if "HTTP_ORIGIN" in request.META:
        return JsonResponse({"error": "unauthorized"}, status=401, headers=response_headers)
    try:
        raw = request.read(16 * 1024 + 1)
        if len(raw) > 16 * 1024:
            raise ValueError
        body = json.loads(raw)
        if not isinstance(body, dict) or set(body) != {"schema", "agentRunId", "authorizationDigest", "processingSpecification", "specDigest"} or body["schema"] != "workspace.mcp.credential.issue.v1":
            raise ValueError
        result = issue_credential(MaterialAccessContext(body["agentRunId"], body["authorizationDigest"], body["processingSpecification"], body["specDigest"]))
        return JsonResponse(result, headers=response_headers)
    except CredentialRejected:
        return JsonResponse({"error": "unauthorized"}, status=401, headers=response_headers)
    except (ValueError, TypeError, UnicodeDecodeError):
        return JsonResponse({"error": "platform_mcp_credential_request_invalid"}, status=400, headers=response_headers)


@_internal_post("/materials/processor")
def material_processor_specification(request):
    from app_core.models import MaterialProcessor
    try:
        if decode_json_object(request) != {"schema": "workspace.material.processor.v1"}:
            raise ValueError
        processor = MaterialProcessor.objects.select_related("specification").get(name="document")
        return JsonResponse({"schema": "workspace.material.processor.result.v1",
            "processingSpecification": processor.specification.payload, "specDigest": processor.specification_id},
            headers={"Cache-Control": "no-store"})
    except ValueError:
        return JsonResponse({"error": "material_processor_request_invalid"}, status=400)
    except MaterialProcessor.DoesNotExist:
        return JsonResponse({"error": "material_processor_unavailable"}, status=503)




@_internal_post("/agent-runs/resolve-input")
def resolve_deferred_input(request):
    try:
        body = decode_json_object(request)
        if (
            set(body) != {"schema", "agentRunId", "authorizationDigest", "inputRef"}
            or body["schema"] != "runtime.deferred_input.resolve.v1"
        ):
            raise ValueError("deferred_input_request_invalid")
        require_string("agentRunId", body["agentRunId"])
        require_string("authorizationDigest", body["authorizationDigest"])
        require_string("inputRef", body["inputRef"])
        agent_run_id = body["agentRunId"]
        digest = body["authorizationDigest"]
        input_ref = body["inputRef"]
        if not agent_run_id or not digest or not input_ref:
            raise ValueError("deferred_input_request_invalid")
        with transaction.atomic():
            agent_run = (
                AgentRun.objects.select_for_update(of=("self",))
                .select_related("authorization")
                .get(id=agent_run_id)
            )
            resolved_input = resolve_deferred_input_operation(agent_run, input_ref, digest)
    except AgentRun.DoesNotExist:
        return JsonResponse({"error": "asset_unavailable"}, status=409)
    except DeferredInputResolutionError as error:
        return JsonResponse({"error": error.errorCode}, status=409)
    except (ValueError, TypeError) as error:
        return JsonResponse({"error": str(error)}, status=400)
    except DeferredInputBindingError as error:
        logger.error("Deferred input resolution failed: %s", error)
        return JsonResponse(
            {"error": "deferred_input_binding_invalid"},
            status=500,
        )
    return JsonResponse(
        {
            "schema": "runtime.deferred_input.resolve.v1",
            "resolvedInput": resolved_input,
        }
    )


@_internal_post("/agent-runs/read-input")
async def read_deferred_input(request):
    prepared = await _prepare_deferred_input_read(request)
    if isinstance(prepared, JsonResponse):
        return prepared
    resolved_input, storage_key = prepared
    response = await stored_file_response(
        storage_key,
        resolved_input["contentType"],
        resolved_input["displayName"],
        as_attachment=False,
        content_length=resolved_input["sizeBytes"],
    )
    if response.status_code == 200:
        response["X-Content-Sha256"] = resolved_input["sha256"]
        response["X-Source-Version"] = resolved_input["sourceVersion"]
    return response


@sync_to_async(thread_sensitive=True)
def _prepare_deferred_input_read(request):
    try:
        body = decode_json_object(request)
        if (
            set(body)
            != {
                "schema",
                "agentRunId",
                "authorizationDigest",
                "inputRef",
                "sourceVersion",
                "sha256",
            }
            or body["schema"] != "runtime.deferred_input.read.v1"
        ):
            raise ValueError("deferred_input_read_invalid")
        with transaction.atomic():
            agent_run = (
                AgentRun.objects.select_for_update(of=("self",))
                .select_related("authorization")
                .get(id=str(body["agentRunId"]))
            )
            resolved_input, storage_key = resolved_input_storage(
                agent_run,
                str(body["inputRef"]),
                str(body["authorizationDigest"]),
            )
            if (
                resolved_input["sourceVersion"] != body["sourceVersion"]
                or resolved_input["sha256"] != body["sha256"]
            ):
                return JsonResponse({"error": "stale_generation"}, status=409)
    except AgentRun.DoesNotExist:
        return JsonResponse({"error": "asset_unavailable"}, status=409)
    except DeferredInputResolutionError as error:
        return JsonResponse({"error": error.errorCode}, status=409)
    except (DeferredInputBindingError, ValueError, TypeError):
        return JsonResponse({"error": "deferred_input_read_invalid"}, status=400)
    return resolved_input, storage_key


@_internal_post("/agent-runs/validate-inputs")
def validate_projected_inputs(request):
    try:
        body = decode_json_object(request)
        if (
            set(body) != {"schema", "agentRunId", "authorizationDigest", "inputs"}
            or body["schema"] != "runtime.projected_input.validate.v1"
            or not isinstance(body["inputs"], list)
            or len(body["inputs"]) > 128
        ):
            raise ValueError("projected_input_validation_invalid")
        expected_fields = {
            "inputRef",
            "virtualPath",
            "sizeBytes",
            "sha256",
            "sourceVersion",
        }
        if any(
            not isinstance(item, dict) or set(item) != expected_fields
            for item in body["inputs"]
        ):
            raise ValueError("projected_input_validation_invalid")
        if any(
            not isinstance(item["sizeBytes"], int)
            or isinstance(item["sizeBytes"], bool)
            or not 0 <= item["sizeBytes"] <= 64 * 1024 * 1024
            for item in body["inputs"]
        ):
            raise ValueError("projected_input_validation_invalid")
        require_opaque_ref("agentRunId", body["agentRunId"])
        require_sha256("authorizationDigest", body["authorizationDigest"])
        for item in body["inputs"]:
            require_opaque_ref("inputRef", item["inputRef"])
            validate_virtual_path(item["virtualPath"])
            require_sha256("sha256", item["sha256"])
            require_string("sourceVersion", item["sourceVersion"])
        input_refs = [item["inputRef"] for item in body["inputs"]]
        if input_refs != sorted(set(input_refs)):
            raise ValueError("projected_input_validation_invalid")
        with transaction.atomic():
            agent_run = (
                AgentRun.objects.select_for_update(of=("self",))
                .select_related("authorization")
                .get(id=str(body["agentRunId"]))
            )
            states = []
            resolve_storage = input_storage_batch(agent_run, str(body["authorizationDigest"]))
            for expected in body["inputs"]:
                try:
                    current, _storage_key = resolve_storage(expected["inputRef"])
                    state = (
                        "active"
                        if all(
                            current[name] == expected[name] for name in expected_fields
                        )
                        else "stale_generation"
                    )
                except DeferredInputResolutionError as error:
                    if error.errorCode in {
                        "asset_removed",
                        "access_revoked",
                        "source_deleted",
                        "stale_generation",
                    }:
                        state = error.errorCode
                    elif error.errorCode == "asset_unavailable":
                        state = "access_revoked"
                    else:
                        raise
                states.append({"inputRef": expected["inputRef"], "state": state})
    except AgentRun.DoesNotExist:
        return JsonResponse({"error": "projected_input_validation_invalid"}, status=409)
    except (DeferredInputBindingError, ValueError, TypeError):
        return JsonResponse({"error": "projected_input_validation_invalid"}, status=400)
    return JsonResponse(
        {
            "schema": "runtime.projected_input.validate.v1",
            "inputs": states,
        }
    )


@_internal_post("/agent-work/returns/materialize")
def materialize_agent_work_return(request):
    from app_core.agent_work_returns import materialize_work_return, WorkReturnError
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "workAgentRunId"} or body["schema"] != "workspace.agent_work.return_materialize.v1":
            raise ValueError
        require_opaque_ref("workAgentRunId", body["workAgentRunId"])
        disposition, notice = materialize_work_return(body["workAgentRunId"])
        return JsonResponse({"schema": "workspace.agent_work.return_materialized.v1",
            "disposition": disposition, "notice": notice},
            status=201 if disposition == "delivered" else 202 if disposition == "pending" else 200)
    except WorkReturnError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_return_invalid"}, status=400)
    except RuntimeError:
        return JsonResponse({"error": "agent_work_return_dependency_unavailable"}, status=503)


@_internal_post("/agent-work/returns/query")
def query_agent_work_return(request):
    from app_core.agent_work_returns import query_work_return, WorkReturnError
    try:
        body = decode_json_object(request)
        fields = {"schema", "agentRunId", "authorizationDigest", "coordinationSessionId", "toolCallId", "noticeId"}
        if set(body) != fields or body["schema"] != "workspace.agent_work.return_query.v1":
            raise ValueError
        require_sha256("authorizationDigest", body["authorizationDigest"])
        for name in fields - {"schema", "authorizationDigest"}:
            require_opaque_ref(name, body[name])
        return JsonResponse(query_work_return(body))
    except WorkReturnError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_return_invalid"}, status=400)


@_internal_post("/agent-work/returns/consume")
def consume_agent_work_return(request):
    from django.db import IntegrityError
    from app_core.agent_work_consumption import consume_work_return
    from app_core.agent_work_returns import WorkReturnError
    from app_core.hosted_operations import HostedOperationError, OPERATION_ID_PATTERN
    import re
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "noticeId", "operationId"} or body["schema"] != "workspace.agent_work.consume.v1":
            raise ValueError
        require_opaque_ref("noticeId", body["noticeId"])
        if not isinstance(body["operationId"], str) or re.fullmatch(OPERATION_ID_PATTERN, body["operationId"]) is None:
            raise ValueError
        response, status = consume_work_return(body["noticeId"], body["operationId"])
        return JsonResponse(response, status=status)
    except (WorkReturnError, HostedOperationError) as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (ValueError, TypeError, KeyError):
        return JsonResponse({"error": "agent_work_consume_invalid"}, status=400)
    except IntegrityError:
        return JsonResponse({"error": "agent_work_consume_conflict"}, status=409)
    except RuntimeError:
        return JsonResponse({"error": "agent_work_consume_dependency_unavailable"}, status=503)


@_internal_post("/agent-work/returns/consume/discover")
def discover_agent_work_consumption(request):
    from app_core.agent_work_consumption import discover_pending_work_returns
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "limit", "after", "through"} or body["schema"] != "workspace.agent_work.consume_discover.v1":
            raise ValueError
        limit = body["limit"]
        if type(limit) is not int or not 1 <= limit <= AGENT_RUN_LIFECYCLE_RECONCILE_LIMIT:
            raise ValueError
        for name in ("after", "through"):
            if body[name] is not None:
                require_opaque_ref(name, body[name])
                if len(body[name]) > 96:
                    raise ValueError
        if body["after"] is not None and (body["through"] is None or body["after"] > body["through"]):
            raise ValueError
        return JsonResponse(discover_pending_work_returns(body["after"], body["through"], limit))
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_consume_discover_invalid"}, status=400)


@_internal_post("/agent-work/returns/schedule")
def schedule_agent_work_return(request):
    from app_core.agent_work_return_jobs import schedule_work_return_job
    from app_core.agent_work_returns import WorkReturnError
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "workAgentRunId"} or body["schema"] != "workspace.agent_work.return_schedule.v1":
            raise ValueError
        require_opaque_ref("workAgentRunId", body["workAgentRunId"])
        disposition = schedule_work_return_job(AgentRun.objects.get(pk=body["workAgentRunId"]))
        return JsonResponse({"schema": "workspace.agent_work.return_scheduled.v1", "disposition": disposition})
    except WorkReturnError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except AgentRun.DoesNotExist:
        return JsonResponse({"error": "agent_work_return_source_not_found"}, status=404)
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_return_schedule_invalid"}, status=400)
    except RuntimeError:
        return JsonResponse({"error": "agent_work_return_schedule_unavailable"}, status=503)


@_internal_post("/agent-work/returns/audit/claim")
def claim_agent_work_return_audit(request):
    from app_core.agent_work_return_jobs import claim_work_return_audit
    try:
        body = decode_json_object(request)
        if (set(body) != {"schema", "limit"} or body["schema"] != "workspace.agent_work.return_audit.claim.v1"
                or type(body["limit"]) is not int or not 1 <= body["limit"] <= 100):
            raise ValueError
        return JsonResponse(claim_work_return_audit(body["limit"]))
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_return_audit_invalid"}, status=400)


@_internal_post("/agent-work/returns/audit/finish")
def finish_agent_work_return_audit(request):
    from app_core.agent_work_return_jobs import finish_work_return_audit
    from app_core.agent_work_returns import WorkReturnError
    try:
        body = decode_json_object(request)
        if (set(body) != {"schema", "leaseOwner", "after", "complete"}
                or body["schema"] != "workspace.agent_work.return_audit.finish.v1"
                or type(body["complete"]) is not bool):
            raise ValueError
        require_opaque_ref("leaseOwner", body["leaseOwner"])
        if body["after"] is not None:
            require_opaque_ref("after", body["after"])
        finish_work_return_audit(body["leaseOwner"], body["after"], body["complete"])
        return JsonResponse({"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"})
    except WorkReturnError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_return_audit_invalid"}, status=400)


@_internal_post("/agent-work/returns/query")
def query_agent_work_return(request):
    from app_core.agent_work_returns import query_work_return, WorkReturnError
    try:
        body = decode_json_object(request)
        fields = {"schema", "agentRunId", "authorizationDigest", "coordinationSessionId", "toolCallId", "noticeId"}
        if set(body) != fields or body["schema"] != "workspace.agent_work.return_query.v1":
            raise ValueError
        require_sha256("authorizationDigest", body["authorizationDigest"])
        for name in fields - {"schema", "authorizationDigest"}:
            require_opaque_ref(name, body[name])
        return JsonResponse(query_work_return(body))
    except WorkReturnError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_return_invalid"}, status=400)


@_internal_post("/agent-work/returns/consume")
def consume_agent_work_return(request):
    from django.db import IntegrityError
    from app_core.agent_work_consumption import consume_work_return
    from app_core.agent_work_returns import WorkReturnError
    from app_core.hosted_operations import HostedOperationError, OPERATION_ID_PATTERN
    import re
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "noticeId", "operationId"} or body["schema"] != "workspace.agent_work.consume.v1":
            raise ValueError
        require_opaque_ref("noticeId", body["noticeId"])
        if not isinstance(body["operationId"], str) or re.fullmatch(OPERATION_ID_PATTERN, body["operationId"]) is None:
            raise ValueError
        response, status = consume_work_return(body["noticeId"], body["operationId"])
        return JsonResponse(response, status=status)
    except (WorkReturnError, HostedOperationError) as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except (ValueError, TypeError, KeyError):
        return JsonResponse({"error": "agent_work_consume_invalid"}, status=400)
    except IntegrityError:
        return JsonResponse({"error": "agent_work_consume_conflict"}, status=409)
    except RuntimeError:
        return JsonResponse({"error": "agent_work_consume_dependency_unavailable"}, status=503)


@_internal_post("/agent-work/returns/consume/discover")
def discover_agent_work_consumption(request):
    from app_core.agent_work_consumption import discover_pending_work_returns
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "limit", "after", "through"} or body["schema"] != "workspace.agent_work.consume_discover.v1":
            raise ValueError
        limit = body["limit"]
        if type(limit) is not int or not 1 <= limit <= AGENT_RUN_LIFECYCLE_RECONCILE_LIMIT:
            raise ValueError
        for name in ("after", "through"):
            if body[name] is not None:
                require_opaque_ref(name, body[name])
                if len(body[name]) > 96:
                    raise ValueError
        if body["after"] is not None and (body["through"] is None or body["after"] > body["through"]):
            raise ValueError
        return JsonResponse(discover_pending_work_returns(body["after"], body["through"], limit))
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_consume_discover_invalid"}, status=400)


@_internal_post("/agent-work/returns/discover")
def discover_agent_work_returns(request):
    from app_core.agent_work_returns import discover_work_returns
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "limit", "after", "through"} or body["schema"] != "workspace.agent_work.returns.discover.v1":
            raise ValueError
        limit = body["limit"]
        if type(limit) is not int or not 1 <= limit <= AGENT_RUN_LIFECYCLE_RECONCILE_LIMIT:
            raise ValueError
        for name in ("after", "through"):
            if body[name] is not None:
                require_opaque_ref(name, body[name])
                if len(body[name]) > 64:
                    raise ValueError
        if body["after"] is not None and (body["through"] is None or body["after"] > body["through"]):
            raise ValueError
        return JsonResponse(discover_work_returns(body["after"], body["through"], limit))
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_work_return_invalid"}, status=400)


@_internal_post("/agent-work/discover")
def discover_agent_work(request):
    from app_core.agent_work_recovery import discover_work_requests, scan_cursor
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "limit", "after", "through"} or body["schema"] != "workspace.agent_work.discover.v1":
            raise ValueError
        limit = body["limit"]
        if type(limit) is not int or not 1 <= limit <= AGENT_RUN_LIFECYCLE_RECONCILE_LIMIT:
            raise ValueError
        after, through = scan_cursor(body["after"]), scan_cursor(body["through"])
        if after is not None and (through is None or after > through):
            raise ValueError
        return JsonResponse(discover_work_requests(after, through, limit))
    except (ValueError, TypeError):
        return JsonResponse({"error":"agent_work_discover_invalid"}, status=400)


@_internal_post("/agent-work/materialize")
def materialize_agent_work(request):
    from app_core.agent_work import materialize_work_request, WorkMaterializationError
    from app_core.hosted_operations import HostedOperationError

    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "sourceEventId"} or body["schema"] != "workspace.agent_work.materialize.v1":
            raise ValueError("agent_work_materialize_invalid")
        require_opaque_ref("sourceEventId", body["sourceEventId"])
        response, created = materialize_work_request(body["sourceEventId"])
        return JsonResponse(response, status=201 if created else 200)
    except (WorkMaterializationError, HostedOperationError) as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except ValueError:
        return JsonResponse({"error": "agent_work_materialize_invalid"}, status=400)


@_internal_post("/agent-run-lifecycle/resolve")
def resolve_agent_run_lifecycle(request):
    try:
        body = decode_json_object(request)
        if (
            set(body) != {"schema", "jobId", "agentRunId", "authorizationDigest"}
            or body["schema"] != "runtime.agent_run_lifecycle.resolve.v1"
        ):
            raise ValueError("agent_run_lifecycle_resolve_invalid")
        require_string("agentRunId", body["agentRunId"])
        require_string("jobId", body["jobId"])
        require_string("authorizationDigest", body["authorizationDigest"])
        agent_run_id = body["agentRunId"]
        job_id = body["jobId"]
        digest = body["authorizationDigest"]
        if not agent_run_id or job_id != agent_run_lifecycle_job_id(agent_run_id) or not digest:
            raise ValueError("agent_run_lifecycle_resolve_invalid")
        with transaction.atomic():
            agent_run = (
                AgentRun.objects.select_for_update(of=("self",))
                .select_related("authorization", "modelConfig", "session")
                .get(id=agent_run_id)
            )
            if agent_run.authorization.digest != digest:
                return JsonResponse(
                    {"error": "agent_run_lifecycle_binding_mismatch"}, status=409
                )
            terminal_state = committed_session_terminal_state(agent_run)
            if terminal_state is None:
                if agent_run.status not in {"queued", "running"}:
                    return JsonResponse(
                        {"error": "agent_run_lifecycle_binding_mismatch"}, status=409
                    )
            agent_run_start = build_agent_run_start(agent_run)
    except AgentRun.DoesNotExist:
        return JsonResponse({"error": "agent_run_not_found"}, status=404)
    except (RuntimeError, ValueError, TypeError):
        return JsonResponse({"error": "agent_run_start_invalid"}, status=409)
    if terminal_state is not None:
        return JsonResponse(
            {
                "schema": "runtime.agent_run_lifecycle.resolved.v1",
                "disposition": "terminal",
                "terminalState": terminal_state,
                "agentRunStart": agent_run_start,
            }
        )
    return JsonResponse(
        {
            "schema": "runtime.agent_run_lifecycle.resolved.v1",
            "disposition": "ready",
            "agentRunStart": agent_run_start,
        }
    )


def _lifecycle_scan_cursor(value):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {"createdAt", "id"}:
        raise ValueError
    if not isinstance(value["id"], str) or not 1 <= len(value["id"]) <= 256:
        raise ValueError
    if not isinstance(value["createdAt"], str):
        raise ValueError
    created_at = parse_datetime(value["createdAt"])
    if created_at is None or timezone.is_naive(created_at):
        raise ValueError
    return created_at, value["id"]


def _expire_queued_run_locked(agent_run):
    """The row lock serializes expiry against the first running transition.

    Logical waits remain running and never enter this policy. Cancellation is
    committed by Runtime; the API records the reason, not a fabricated terminal.
    """
    if (agent_run.status != "queued" or agent_run.startedAt is not None
            or (timezone.now() - agent_run.createdAt).total_seconds()
            < settings.EXECUTION_QUEUE_WAIT_SECONDS):
        return False
    request_agent_run_cancellation(agent_run)
    agent_run.transitionReason = "execution_queue_expired"
    agent_run.save(update_fields=["transitionReason", "updatedAt"])
    return True


def _lifecycle_scan_page(queryset, after, limit):
    if after is not None:
        created_at, identity = after
        queryset = queryset.filter(
            Q(createdAt__gt=created_at) | Q(createdAt=created_at, id__gt=identity)
        )
    rows = list(queryset.order_by("createdAt", "id")[:limit + 1])
    page = rows[:limit]
    next_cursor = None
    if len(rows) > limit:
        next_cursor = {"createdAt": page[-1].createdAt.isoformat(), "id": page[-1].id}
    return page, next_cursor


@_internal_post("/agent-run-lifecycle/reconcile")
def reconcile_agent_run_lifecycle(request):
    try:
        body = decode_json_object(request)
        if (
            not {"schema", "limit"} <= set(body)
            or set(body) - {"schema", "limit", "activeAfter", "deadLetterAfter"}
            or body["schema"] != "runtime.agent_run_lifecycle.reconcile.v1"
        ):
            raise ValueError
        active_after = _lifecycle_scan_cursor(body.get("activeAfter"))
        dead_letter_after = _lifecycle_scan_cursor(body.get("deadLetterAfter"))
        limit = body["limit"]
        if (
            isinstance(limit, bool)
            or not isinstance(limit, int)
            or not 1 <= limit <= AGENT_RUN_LIFECYCLE_RECONCILE_LIMIT
        ):
            raise ValueError
    except (ValueError, TypeError):
        return JsonResponse({"error": "agent_run_lifecycle_reconcile_invalid"}, status=400)

    scheduled = 0
    terminalized = 0
    pending = 0
    input_handoff_agents = set()
    active_agent_runs, active_next = _lifecycle_scan_page(
        AgentRun.objects.filter(status__in={"queued", "running"})
        .select_related("authorization", "modelConfig", "session"),
        active_after, limit,
    )
    dead_letter_agent_runs, dead_letter_next = _lifecycle_scan_page(
        AgentRun.objects.filter(
            status="failed",
            transitionReason=AGENT_RUN_LIFECYCLE_DEAD_LETTER_REASON,
            events__payload__type__in={
                "agent_run_completed",
                "agent_run_failed",
                "agent_run_interrupted",
            },
        )
        .select_related("authorization", "modelConfig", "session")
        .distinct(),
        dead_letter_after, limit,
    )
    agent_runs = dead_letter_agent_runs + active_agent_runs
    for agent_run in agent_runs:
        try:
            if agent_run.status == "failed":
                if committed_session_terminal_state(agent_run) is not None:
                    projected = project_committed_agent_run(agent_run)
                    projected.transitionReason = "runtime_session_terminal_committed"
                    projected.save(update_fields=["transitionReason", "updatedAt"])
                    terminalized += 1
                    input_handoff_agents.add(projected.session.agent_id)
                continue
            job = get_runtime_job(agent_run_lifecycle_job_id(agent_run.id))
            if job is not None and job.get("status") in {"failed", "dead_lettered"}:
                _fail_agent_run_lifecycle(agent_run.id)
                terminalized += 1
                input_handoff_agents.add(agent_run.session.agent_id)
                continue
            if job is not None and job.get("status") == "succeeded":
                projected = project_committed_agent_run(agent_run)
                if projected.status not in {"completed", "failed", "cancelled"}:
                    raise RuntimeError(
                        "agent_run_lifecycle_completed_without_session_terminal"
                    )
                terminalized += 1
                input_handoff_agents.add(projected.session.agent_id)
                continue
            schedule_agent_run_lifecycle(agent_run)
            if agent_run.status == "queued":
                with transaction.atomic():
                    current = (AgentRun.objects.select_for_update(of=("self",))
                               .select_related("authorization", "modelConfig", "session")
                               .get(id=agent_run.id))
                    _expire_queued_run_locked(current)
            scheduled += 1
        except Exception:
            pending += 1
            logger.exception(
                "Run lifecycle reconciliation remains pending",
                extra={"agentRunId": agent_run.id},
            )
            AgentRun.objects.filter(
                id=agent_run.id, status__in={"queued", "running"}
            ).exclude(transitionReason="execution_queue_expired").update(
                transitionReason="agent_run_lifecycle_reconcile_pending")
    from app_core.agent_input_delivery import try_dispatch_agent_inputs
    from app_core.models import AgentInput
    for agent_id in sorted(input_handoff_agents):
        if AgentInput.objects.filter(agent_id=agent_id, delivery__isnull=True).exists():
            try_dispatch_agent_inputs(agent_id)
    return JsonResponse(
        {"scheduled": scheduled, "terminalized": terminalized, "pending": pending,
         "activeNext": active_next, "deadLetterNext": dead_letter_next}
    )


def _fail_agent_run_lifecycle(agent_run_id: str) -> AgentRun:
    with transaction.atomic():
        agent_run = AgentRun.objects.select_for_update().get(id=agent_run_id)
        if agent_run.status in {"completed", "cancelled"} or (
            agent_run.status == "failed"
            and agent_run.transitionReason != AGENT_RUN_LIFECYCLE_DEAD_LETTER_REASON
        ):
            return agent_run
        if committed_session_terminal_state(agent_run) is not None:
            projected = project_committed_agent_run(agent_run)
            if projected.status in {"completed", "failed", "cancelled"}:
                if projected.preAdmissionCancelledAt is None:
                    projected.transitionReason = "runtime_session_terminal_committed"
                elif projected.transitionReason != "execution_queue_expired":
                    projected.transitionReason = "agent_run_cancelled"
                projected.save(update_fields=["transitionReason", "updatedAt"])
                return projected
        if agent_run.status == "failed":
            return agent_run
        agent_run.status = "failed"
        agent_run.transitionReason = AGENT_RUN_LIFECYCLE_DEAD_LETTER_REASON
        agent_run.completedAt = timezone.now()
        agent_run.save(
            update_fields=["status", "transitionReason", "completedAt", "updatedAt"]
        )
        return agent_run


@_internal_post("/agent-runs/transition")
def transition_agent_run(request):
    try:
        body = decode_json_object(request)
        if set(body) != {"schema", "agentRunId", "state", "transitionReason"}:
            raise ValueError("fields_mismatch")
        if body["schema"] != "runtime.agent_run.transition.v1":
            raise ValueError("schema_mismatch")
        require_string("agentRunId", body["agentRunId"])
        require_string("state", body["state"])
        require_string("transitionReason", body["transitionReason"])
        agent_run_id = body["agentRunId"]
        state = body["state"]
        reason = body["transitionReason"]
        if (
            not agent_run_id
            or not reason
            or state
            not in {
                "running",
                "completed",
                "failed",
                "cancelled",
            }
        ):
            raise ValueError("transition_invalid")
        with transaction.atomic():
            agent_run = AgentRun.objects.select_for_update().get(id=agent_run_id)
            if state == "running" and agent_run.status == "queued":
                try:
                    _expire_queued_run_locked(agent_run)
                except RuntimeError:
                    response = JsonResponse({"error": "execution_queue_expiry_unavailable"}, status=503)
                    response["Retry-After"] = "5"
                    return response
            if agent_run.transitionReason == "execution_queue_expired":
                reason = "execution_queue_expired"
            if state == "running":
                if agent_run.status == "running":
                    agent_run.transitionReason = reason
                    agent_run.save(update_fields=["transitionReason", "updatedAt"])
                    return JsonResponse({"agentRunId": agent_run.id, "state": agent_run.status})
                if agent_run.status != "queued":
                    return JsonResponse(
                        {"error": "agent_run_transition_conflict"},
                        status=409,
                    )
                agent_run.status = "running"
                agent_run.transitionReason = reason
                agent_run.startedAt = timezone.now()
                agent_run.save(
                    update_fields=[
                        "status",
                        "transitionReason",
                        "startedAt",
                        "updatedAt",
                    ]
                )
            else:
                if state == "failed" and reason == AGENT_RUN_LIFECYCLE_DEAD_LETTER_REASON:
                    agent_run = _fail_agent_run_lifecycle(agent_run.id)
                    return JsonResponse({"agentRunId": agent_run.id, "state": agent_run.status})
                expected = state
                projected = project_committed_agent_run(agent_run, expected)
                if projected.status != expected:
                    return JsonResponse(
                        {"error": "semantic_terminal_missing"},
                        status=409,
                    )
                projected.transitionReason = reason
                projected.save(update_fields=["transitionReason", "updatedAt"])
                agent_run = projected
    except AgentRun.DoesNotExist:
        return JsonResponse({"error": "agent_run_not_found"}, status=404)
    except (ValueError, TypeError) as error:
        return JsonResponse({"error": str(error)}, status=400)
    if agent_run.status in {"completed", "failed", "cancelled"}:
        # The terminal is already durable. Delivery failure must not undo it or
        # block lifecycle teardown; the independent ledger pass repairs this gap.
        try:
            from app_core.agent_work_return_jobs import schedule_work_return_job
            schedule_work_return_job(agent_run)
        except Exception as error:
            logger.warning("Work return delivery remains pending: %s", type(error).__name__)
    return JsonResponse({"agentRunId": agent_run.id, "state": agent_run.status})
