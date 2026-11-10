"""Durable storage intents recover partial writes and ambiguous DB commits."""

from django.db import transaction
from django.db.models import Q

from . import material_storage
from .material_contract import KnowledgeError
from .material_leases import fenced_task
from .models import DerivedRepresentation, DerivedResource, MaterialStagedObject


def prepare_staging(lease, output):
    prefix = "materials/" + lease.representation_id.split(":")[-1] + "/"
    entries = [(prefix + output.canonical_sha256[7:] + "/canonical.md", output.canonical_size_bytes, output.canonical_sha256)]
    if output.preview_size_bytes:
        entries.append((prefix + output.preview_sha256[7:] + "/preview.pdf", output.preview_size_bytes, output.preview_sha256))
    if output.workbook_size_bytes:
        entries.append((prefix + output.workbook_sha256[7:] + "/workbook.json", output.workbook_size_bytes, output.workbook_sha256))
    with transaction.atomic(durable=True):
        with fenced_task(lease, completed_replay=True) as task:
            for key, size, digest in entries:
                stage, _ = MaterialStagedObject.objects.get_or_create(storageKey=key,
                    defaults={"task": task, "sizeBytes": size, "sha256": digest})
                if stage.task_id != task.pk or stage.sizeBytes != size or stage.sha256 != digest:
                    raise KnowledgeError("material_staging_identity_conflict")
    by_name = {key.rsplit("/", 1)[-1]: key for key, _, _ in entries}
    return entries[0][0], by_name.get("preview.pdf", ""), by_name.get("workbook.json", "")


def recover_staging(task, *, discard=False):
    """Caller holds the task row lock. Published objects are never removed."""
    prefix = "materials/" + task.pk.split(":")[-1] + "/"
    for stage in MaterialStagedObject.objects.filter(task=task):
        expected = {prefix + stage.sha256[7:] + "/canonical.md", prefix + stage.sha256[7:] + "/preview.pdf", prefix + stage.sha256[7:] + "/workbook.json"}
        if stage.storageKey not in expected:
            raise KnowledgeError("material_staging_identity_conflict")
        published = DerivedRepresentation.objects.filter(Q(canonicalTextKey=stage.pk) | Q(previewPdfKey=stage.pk) | Q(workbookPreviewKey=stage.pk)).exists()
        registered = DerivedResource.objects.filter(resourceKey=stage.pk).exists()
        if published or registered:
            stage.delete()
            continue
        if material_storage.default_storage.exists(stage.pk):
            try:
                material_storage._validate_stored(stage.pk, stage.sizeBytes, stage.sha256)
            except KnowledgeError:
                material_storage.delete_stored_object(stage.pk)
            else:
                if discard:
                    material_storage.delete_stored_object(stage.pk)
        if discard:
            stage.delete()
