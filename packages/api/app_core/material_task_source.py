"""Platform-owned processing input, separate from a user's right to read it.

Internal services only: never accept task/source identities from an unauthenticated
request. Admission requires current run input authorization; subsequent execution
uses the persisted material identity, not a run credential or a selected waiter.
This resolver is a fresh lifecycle check, not a publication lock. Result commit
must also fence the lease and serialize with material lifecycle changes.
"""

from dataclasses import dataclass

from django.core.files.storage import default_storage
from django.db import transaction

from .material_access import bind_inputs
from .material_contract import KnowledgeError
from .material_identity import processing_spec_digest, representation_id
from .models import Artifact, MaterialProcessingTask, Source, SourceObject, UserLibraryObject


@dataclass(frozen=True)
class ProcessingSource:
    owner: object
    storage_key: str
    input_identity: dict
    size_bytes: int


def admit_task(access, request):
    """Admit a platform task using fresh authorization, without retaining it."""
    bound = bind_inputs(access.agent_run, access.authorization_digest, [request], access.spec_digest)[0]
    payload = {"schema": "workspace.material.processing.v1",
               "inputIdentity": bound.input_identity, "specDigest": access.spec_digest,
               "sizeBytes": bound.resolved["sizeBytes"]}
    with transaction.atomic():
        task, _ = MaterialProcessingTask.objects.get_or_create(
            representationId=bound.representation_id,
            defaults={"executionBackend": "platform", "processingSpecification": access.processing_specification,
                      "payload": payload},
        )
        if task.executionBackend != "platform":
            raise KnowledgeError("material_task_backend_conflict", 409)
        if task.payload != payload or task.processingSpecification != access.processing_specification:
            raise KnowledgeError("material_task_identity_mismatch", 409)
        resolve_task_source(task)
        return task


def resolve_task_source(task, *, lock=False):
    payload = task.payload
    if (task.executionBackend != "platform" or not isinstance(payload, dict)
            or set(payload) != {"schema", "inputIdentity", "specDigest", "sizeBytes"}
            or payload["schema"] != "workspace.material.processing.v1"
            or type(payload["sizeBytes"]) is not int or payload["sizeBytes"] < 0):
        raise KnowledgeError("material_task_payload_invalid")
    digest = processing_spec_digest(task.processingSpecification)
    identity = payload["inputIdentity"]
    if digest != payload["specDigest"] or representation_id(identity, digest) != task.pk:
        raise KnowledgeError("material_task_identity_mismatch")
    if payload["sizeBytes"] > task.processingSpecification["options"]["maxInputBytes"]:
        raise KnowledgeError("material_task_payload_invalid")
    model = {"userLibraryObject": UserLibraryObject, "sourceObject": SourceObject, "artifact": Artifact}[identity["ownerKind"]]
    try:
        owner = model.objects.get(pk=identity["ownerId"])
        if lock:
            # Parent-before-child agrees with source deletion/GC ordering.
            source = Source.objects.select_for_update().get(pk=owner.source_id) if isinstance(owner, SourceObject) else None
            owner = model.objects.select_for_update().get(pk=identity["ownerId"])
            if source is not None:
                if owner.source_id != source.pk:
                    raise KnowledgeError("material_source_identity_changed", 409)
                owner.source = source
    except (model.DoesNotExist, Source.DoesNotExist):
        raise KnowledgeError("material_source_unavailable", 404) from None
    expected_status = "published" if isinstance(owner, Artifact) else "ready"
    if owner.status != expected_status or owner.deletedAt is not None:
        raise KnowledgeError("material_source_unavailable", 404)
    if isinstance(owner, SourceObject) and (owner.source.status != "ready" or owner.objectType != "file"):
        raise KnowledgeError("material_source_unavailable", 404)
    if isinstance(owner, UserLibraryObject) and owner.objectKind == "folder":
        raise KnowledgeError("material_source_unavailable", 404)
    if (owner.contentGeneration != identity["generation"] or owner.sha256 != identity["sha256"]
            or owner.sizeBytes != payload["sizeBytes"]):
        raise KnowledgeError("material_source_identity_changed", 409)
    if not owner.storageKey or not default_storage.exists(owner.storageKey):
        raise KnowledgeError("material_source_unavailable", 404)
    return ProcessingSource(owner, owner.storageKey, identity, payload["sizeBytes"])
