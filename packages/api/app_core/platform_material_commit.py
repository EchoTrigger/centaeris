"""Lease-fenced platform publication without a run credential or legacy job ID.

Internal service only; a Worker transport must authenticate and bound its body
before calling this. This owns the outermost transaction: do not nest it in a
request transaction that can subsequently roll back successful publication.
"""

from django.db import transaction

from .material_commit import ProcessingOutput, commit_bound_material, validate_commit
from .material_contract import BoundInput, KnowledgeError
from .material_leases import fenced_task
from .material_task_source import resolve_task_source
from .models import DerivedRepresentation
from .models import MaterialStagedObject
from .material_staging import prepare_staging, recover_staging


def commit_processing_output(lease, output: ProcessingOutput, stream):
    validate_commit(output)
    if output.representation_id != lease.representation_id:
        raise KnowledgeError("material_task_identity_mismatch")
    storage_keys = prepare_staging(lease, output)
    with transaction.atomic(durable=True):
        with fenced_task(lease, completed_replay=True) as task:
            source = resolve_task_source(task, lock=True)
            if task.status == "completed" and not DerivedRepresentation.objects.filter(pk=task.pk).exists():
                raise KnowledgeError("material_processing_result_missing")
            recover_staging(task)
            bound = BoundInput({}, source.storage_key, source.owner, source.input_identity, task.pk)

            def complete():
                # Recheck DB time after receiving/verifying the potentially slow
                # stream. A lease valid at entry is not enough for publication.
                with fenced_task(lease, completed_replay=True) as current:
                    if current.status != "completed":
                        current.status, current.errorCode = "completed", ""
                        current.save(update_fields=["status", "errorCode", "updatedAt"])
                    MaterialStagedObject.objects.filter(task=current, storageKey__in=storage_keys).delete()

            return commit_bound_material(
                bound, task.processingSpecification, task.payload["specDigest"], output, stream,
                published=complete, storage_keys=storage_keys,
            )
