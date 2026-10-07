import hashlib
import json
import mimetypes
import unicodedata

from django.core.files import File
from django.core.files.storage import default_storage
from django.db import transaction
from django.utils import timezone

from .assets import delete_stored_object, safe_filename
from .models import (
    Artifact,
    ArtifactPublication,
    AgentRunAuthorization,
    AgentRun,
    UserLibraryLink,
    UserLibraryObject,
    new_artifact_id,
    new_library_object_id,
)
from .runtime_contract import authorization_digest
from .workspace_access import agent_run_membership_is_current


ARTIFACT_PUBLICATION_SCHEMA = "artifact.publication.v1"
ARTIFACT_PUBLICATION_STATUS_SCHEMA = "artifact.publication.status.v1"
ARTIFACT_PUBLICATION_FIELDS = {
    "schema",
    "publicationId",
    "agentRunId",
    "authorizationDigest",
    "toolCallId",
    "filename",
    "sizeBytes",
    "sha256",
}
ARTIFACT_PUBLICATION_STATUS_FIELDS = {
    "schema",
    "publicationId",
    "agentRunId",
    "authorizationDigest",
    "toolCallId",
}
MAX_ARTIFACT_BYTES = 64 * 1024 * 1024
MAX_METADATA_BYTES = 64 * 1024


class ArtifactPublishError(Exception):
    def __init__(self, code: str, status: int = 409):
        super().__init__(code)
        self.code = code
        self.status = status


def _ingest_artifact_library_copy(artifact: Artifact, user) -> None:
    """Copy published artifact bytes into the user library as a durable savedArtifact.

    Idempotent on (sourceKind=artifact, sourceRefId=artifact.id): if a provenance
    link already exists the copy is left untouched, so replays never duplicate
    bytes nor resurrect a previously deleted library object. Caller must hold the
    artifact row lock in the same transaction; any failure raises so the caller
    transaction aborts and the publication stays staging.
    """
    existing_link = (
        UserLibraryLink.objects.select_related("libraryObject")
        .filter(
            sourceKind="artifact",
            sourceRefId=artifact.id,
            libraryObject__owner=user,
        )
        .first()
    )
    if existing_link is not None:
        return
    if not artifact.storageKey or not default_storage.exists(artifact.storageKey):
        raise ArtifactPublishError("stored_object_not_available", 409)
    library_id = new_library_object_id()
    storage_key = f"users/{user.id}/library/{library_id}/{artifact.safeFilename}"
    with default_storage.open(artifact.storageKey, "rb") as source:
        stored_key = default_storage.save(
            storage_key,
            File(source, name=artifact.safeFilename),
        )
    if stored_key != storage_key:
        delete_stored_object(stored_key)
        raise ArtifactPublishError("library_storage_key_conflict", 409)
    # The source was verified on upload (or staging recovery). A successful
    # storage save is the copy boundary; do not reopen both objects to hash them.
    try:
        item = UserLibraryObject.objects.create(
            id=library_id,
            owner=user,
            displayName=artifact.displayName,
            objectKind="savedArtifact",
            contentType=artifact.contentType,
            sizeBytes=artifact.sizeBytes,
            sha256=artifact.sha256,
            storageKey=storage_key,
            status="ready",
            contentGeneration=1,
        )
        UserLibraryLink.objects.create(
            libraryObject=item,
            sourceKind="artifact",
            sourceRefId=artifact.id,
        )
    except Exception:
        delete_stored_object(storage_key)
        raise


def publish_artifact(request) -> tuple[ArtifactPublication, Artifact]:
    metadata = _decode_publication_body(request)
    agent_run = _bound_agent_run(metadata)
    publication, artifact = _prepare_publication(agent_run, metadata)
    if publication.status == "published":
        verify_stored_object(artifact.storageKey, artifact.sizeBytes, artifact.sha256)
        return publication, artifact

    reader = _HashingReader(request, metadata["sizeBytes"])
    storage_key = artifact.storageKey
    try:
        if default_storage.exists(storage_key):
            verify_stored_object(storage_key, metadata["sizeBytes"], metadata["sha256"])
        else:
            stored_key = default_storage.save(
                storage_key,
                File(reader, name=artifact.safeFilename),
            )
            if stored_key != storage_key:
                delete_stored_object(stored_key)
                raise ArtifactPublishError("artifact_storage_key_conflict")
            reader.require_complete(metadata["sha256"])
    except Exception:
        if default_storage.exists(storage_key):
            try:
                verify_stored_object(storage_key, metadata["sizeBytes"], metadata["sha256"])
            except ArtifactPublishError:
                delete_stored_object(storage_key)
        raise

    with transaction.atomic():
        publication = ArtifactPublication.objects.select_for_update(of=("self",)).select_related("artifact").get(
            publicationId=metadata["publicationId"]
        )
        artifact = Artifact.objects.select_for_update().get(id=publication.artifact_id)
        _require_idempotent_publication(publication, agent_run, metadata)
        _require_artifact_bytes(artifact, metadata)
        if publication.status == "published":
            verify_stored_object(artifact.storageKey, artifact.sizeBytes, artifact.sha256)
            return publication, artifact
        if publication.status != "staging":
            raise ArtifactPublishError("artifact_publication_state_conflict")
        if artifact.status == "staging":
            _ingest_artifact_library_copy(artifact, agent_run.user)
            artifact.status = "published"
            artifact.publishedAt = timezone.now()
            artifact.failureReason = ""
            artifact.save(update_fields=["status", "publishedAt", "failureReason"])
        elif artifact.status != "published":
            raise ArtifactPublishError("artifact_publish_state_conflict")
        publication.status = "published"
        publication.publishedAt = timezone.now()
        publication.save(update_fields=["status", "publishedAt"])
    return publication, artifact


def published_artifact(body: dict) -> tuple[ArtifactPublication, Artifact] | None:
    metadata = _validate_status_request(body)
    agent_run = _bound_agent_run(metadata)
    publication = (
        ArtifactPublication.objects.select_related("artifact")
        .filter(publicationId=metadata["publicationId"])
        .first()
    )
    if publication is None:
        return None
    _require_idempotent_publication(publication, agent_run, metadata)
    if publication.status != "published" or publication.artifact is None:
        return None
    artifact = publication.artifact
    verify_stored_object(artifact.storageKey, artifact.sizeBytes, artifact.sha256)
    return publication, artifact


def publication_response(publication: ArtifactPublication, artifact: Artifact) -> dict:
    return {
        "schema": "artifact.publication.result.v1",
        "publicationId": publication.publicationId,
        "artifactRef": f"artifact:{artifact.id}",
        "filename": publication.filename,
        "contentType": artifact.contentType,
        "sizeBytes": publication.sizeBytes,
        "sha256": publication.sha256,
    }


def _decode_publication_body(request) -> dict:
    content_length = request.META.get("CONTENT_LENGTH")
    try:
        content_length = int(content_length)
    except (TypeError, ValueError) as error:
        raise ArtifactPublishError("artifact_publication_content_length_invalid", 400) from error
    prefix = _read_exact(request, 4)
    metadata_length = int.from_bytes(prefix, "big")
    if metadata_length <= 0 or metadata_length > MAX_METADATA_BYTES:
        raise ArtifactPublishError("artifact_publication_metadata_length_invalid", 400)
    try:
        metadata = json.loads(_read_exact(request, metadata_length).decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ArtifactPublishError("artifact_publication_metadata_invalid", 400) from error
    metadata = _validate_publication_metadata(metadata)
    if content_length != 4 + metadata_length + metadata["sizeBytes"]:
        raise ArtifactPublishError("artifact_publication_content_length_mismatch", 400)
    return metadata


def _read_exact(request, size: int) -> bytes:
    chunks = []
    remaining = size
    while remaining:
        chunk = request.read(remaining)
        if not chunk:
            raise ArtifactPublishError("artifact_publication_body_truncated", 400)
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _validate_publication_metadata(body: dict) -> dict:
    if not isinstance(body, dict) or set(body) != ARTIFACT_PUBLICATION_FIELDS:
        raise ArtifactPublishError("artifact_publication_fields_mismatch", 400)
    if body.get("schema") != ARTIFACT_PUBLICATION_SCHEMA:
        raise ArtifactPublishError("artifact_publication_schema_mismatch", 400)
    _validate_identity_fields(body)
    filename = body.get("filename")
    if (
        not isinstance(filename, str)
        or not filename
        or len(filename) > 255
        or filename.strip() != filename
        or unicodedata.normalize("NFC", filename) != filename
        or "/" in filename
        or "\\" in filename
        or filename in {".", ".."}
        or any(ord(char) < 32 or ord(char) == 127 for char in filename)
    ):
        raise ArtifactPublishError("artifact_filename_invalid", 400)
    size_bytes = body.get("sizeBytes")
    if (
        isinstance(size_bytes, bool)
        or not isinstance(size_bytes, int)
        or not 0 <= size_bytes <= MAX_ARTIFACT_BYTES
    ):
        raise ArtifactPublishError("artifact_size_invalid", 400)
    _require_hash(body.get("sha256"), "sha256:", "artifact_sha256_invalid")
    return body


def _validate_status_request(body: dict) -> dict:
    if not isinstance(body, dict) or set(body) != ARTIFACT_PUBLICATION_STATUS_FIELDS:
        raise ArtifactPublishError("artifact_publication_status_fields_mismatch", 400)
    if body.get("schema") != ARTIFACT_PUBLICATION_STATUS_SCHEMA:
        raise ArtifactPublishError("artifact_publication_status_schema_mismatch", 400)
    _validate_identity_fields(body)
    return body


def _validate_identity_fields(body: dict) -> None:
    _require_hash(body.get("publicationId"), "pub_", "artifact_publication_id_invalid")
    _require_hash(
        body.get("authorizationDigest"),
        "sha256:",
        "artifact_authorization_digest_invalid",
    )
    for name in ["agentRunId", "toolCallId"]:
        value = body.get(name)
        if (
            not isinstance(value, str)
            or not value
            or len(value) > 160
            or value.strip() != value
            or any(not (char.isascii() and (char.isalnum() or char in "_-:.")) for char in value)
        ):
            raise ArtifactPublishError(f"artifact_{name}_invalid", 400)
    expected_publication_id = "pub_" + hashlib.sha256(
        json.dumps(
            [body["agentRunId"], body["toolCallId"]],
            separators=(",", ":"),
        ).encode("utf-8")
    ).hexdigest()
    if body["publicationId"] != expected_publication_id:
        raise ArtifactPublishError("artifact_publication_identity_mismatch", 409)


def _require_hash(value, prefix: str, code: str) -> None:
    digest = value.removeprefix(prefix) if isinstance(value, str) and value.startswith(prefix) else ""
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise ArtifactPublishError(code, 400)


def _bound_agent_run(body: dict) -> AgentRun:
    try:
        frozen = AgentRunAuthorization.objects.select_related(
            "agent_run__workspace", "agent_run__session", "agent_run__user"
        ).get(agent_run_id=body["agentRunId"])
    except AgentRunAuthorization.DoesNotExist as error:
        raise ArtifactPublishError("artifact_agent_run_not_found", 404) from error
    if not agent_run_membership_is_current(frozen.agent_run):
        raise ArtifactPublishError("artifact_scope_mismatch", 403)
    if authorization_digest(frozen.payload) != frozen.digest:
        raise ArtifactPublishError("artifact_agent_run_authorization_invalid")
    if body["authorizationDigest"] != frozen.digest:
        raise ArtifactPublishError("artifact_scope_mismatch", 403)
    return frozen.agent_run


def _prepare_publication(
    agent_run: AgentRun, metadata: dict
) -> tuple[ArtifactPublication, Artifact]:
    with transaction.atomic():
        locked_agent_run = AgentRun.objects.select_for_update().get(id=agent_run.id)
        publication = (
            ArtifactPublication.objects.select_for_update(of=("self",))
            .select_related("artifact")
            .filter(publicationId=metadata["publicationId"])
            .first()
        )
        if publication is not None:
            _require_idempotent_publication(publication, locked_agent_run, metadata)
            if publication.artifact is None:
                raise ArtifactPublishError("artifact_publication_state_conflict")
            return publication, publication.artifact

        filename_key = unicodedata.normalize("NFC", metadata["filename"]).casefold()
        artifact = next(
            (
                item
                for item in Artifact.objects.select_for_update().filter(
                    agent_run=locked_agent_run,
                    status__in={"staging", "published"},
                )
                if unicodedata.normalize("NFC", item.displayName).casefold() == filename_key
            ),
            None,
        )
        if artifact is not None:
            _require_artifact_bytes(artifact, metadata, "artifact_filename_conflict")
        else:
            artifact_id = new_artifact_id()
            try:
                stored_filename = safe_filename(metadata["filename"])
            except ValueError as error:
                raise ArtifactPublishError("artifact_filename_invalid", 400) from error
            content_type = mimetypes.guess_type(metadata["filename"], strict=False)[0]
            artifact = Artifact.objects.create(
                id=artifact_id,
                workspace=locked_agent_run.workspace,
                agent_run=locked_agent_run,
                session=locked_agent_run.session,
                createdBy=locked_agent_run.user,
                displayName=metadata["filename"],
                safeFilename=stored_filename,
                contentType=content_type or "application/octet-stream",
                sizeBytes=metadata["sizeBytes"],
                sha256=metadata["sha256"],
                storageKey=(
                    f"workspaces/{locked_agent_run.workspace_id}/artifacts/"
                    f"{artifact_id}/{stored_filename}"
                ),
                status="staging",
            )
        publication = ArtifactPublication.objects.create(
            publicationId=metadata["publicationId"],
            agent_run=locked_agent_run,
            authorizationDigest=metadata["authorizationDigest"],
            toolCallId=metadata["toolCallId"],
            filename=metadata["filename"],
            sizeBytes=metadata["sizeBytes"],
            sha256=metadata["sha256"],
            status="published" if artifact.status == "published" else "staging",
            artifact=artifact,
            publishedAt=timezone.now() if artifact.status == "published" else None,
        )
        return publication, artifact


def _require_idempotent_publication(
    publication: ArtifactPublication,
    agent_run: AgentRun,
    metadata: dict,
) -> None:
    expected = {
        "agent_run_id": agent_run.id,
        "authorizationDigest": metadata["authorizationDigest"],
        "toolCallId": metadata["toolCallId"],
    }
    if "filename" in metadata:
        expected.update(
            filename=metadata["filename"],
            sizeBytes=metadata["sizeBytes"],
            sha256=metadata["sha256"],
        )
    if any(getattr(publication, name) != value for name, value in expected.items()):
        raise ArtifactPublishError("artifact_publication_idempotency_conflict")


def _require_artifact_bytes(
    artifact: Artifact,
    metadata: dict,
    code: str = "artifact_integrity_mismatch",
) -> None:
    if artifact.sizeBytes != metadata["sizeBytes"] or artifact.sha256 != metadata["sha256"]:
        raise ArtifactPublishError(code)


class _HashingReader:
    def __init__(self, source, size_bytes: int):
        self.source = source
        self.size = size_bytes
        self.remaining = size_bytes
        self.digest = hashlib.sha256()

    def read(self, size: int = -1) -> bytes:
        if self.remaining == 0:
            return b""
        requested = self.remaining if size is None or size < 0 else min(size, self.remaining)
        chunk = self.source.read(requested)
        if not chunk:
            raise ArtifactPublishError("artifact_publication_body_truncated", 400)
        self.remaining -= len(chunk)
        self.digest.update(chunk)
        return chunk

    def require_complete(self, expected_hash: str) -> None:
        if self.remaining != 0 or f"sha256:{self.digest.hexdigest()}" != expected_hash:
            raise ArtifactPublishError("artifact_integrity_mismatch")


def verify_stored_object(storage_key: str, expected_size: int, expected_hash: str) -> None:
    if not storage_key or not default_storage.exists(storage_key):
        raise ArtifactPublishError("artifact_storage_missing")
    digest = hashlib.sha256()
    size = 0
    with default_storage.open(storage_key, "rb") as source:
        while chunk := source.read(64 * 1024):
            size += len(chunk)
            if size > MAX_ARTIFACT_BYTES:
                raise ArtifactPublishError("artifact_storage_integrity_mismatch")
            digest.update(chunk)
    if size != expected_size or f"sha256:{digest.hexdigest()}" != expected_hash:
        raise ArtifactPublishError("artifact_storage_integrity_mismatch")
