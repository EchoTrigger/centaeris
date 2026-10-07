"""Durable run-owned wait handles over platform-owned material tasks."""

import json

from django.db import transaction
from django.utils import timezone

from .material_access import bind_inputs
from .material_contract import KnowledgeError, sha256_bytes
from .models import MaterialOperation


ACTIVE = {"pending", "queued", "running"}


def create_operations(access, missing):
    from .material_task_source import admit_task
    bound = bind_inputs(access.agent_run, access.authorization_digest, missing, access.spec_digest)
    results = []
    with transaction.atomic():
        for item in bound:
            ref = item.resolved["inputRef"]
            task = admit_task(access, {"inputRef": ref, "representationId": item.representation_id})
            identity = json.dumps([access.agent_run.id, task.pk], separators=(",", ":")).encode()
            operation, _ = MaterialOperation.objects.get_or_create(
                agent_run=access.agent_run, task=task,
                defaults={"id": "material.operation:" + sha256_bytes(identity)[7:], "inputRef": ref},
            )
            results.append(_result(operation))
    return results


def _result(operation):
    task = operation.task
    status = "cancelled" if operation.cancelledAt is not None else task.status
    return {"operationId": operation.pk, "inputRef": operation.inputRef, "status": status,
            "errorCode": task.errorCode if status == "failed" else None}


def operation_result(access, operation_id, *, cancel=False):
    with transaction.atomic():
        try:
            operation = MaterialOperation.objects.select_for_update().select_related("task").get(
                pk=operation_id, agent_run=access.agent_run,
                task__payload__specDigest=access.spec_digest,
            )
        except MaterialOperation.DoesNotExist:
            raise KnowledgeError("material_operation_not_found", 404) from None
        if cancel:
            if operation.cancelledAt is None and operation.task.status in ACTIVE:
                operation.cancelledAt = timezone.now()
                operation.save(update_fields=["cancelledAt"])
        else:
            bind_inputs(access.agent_run, access.authorization_digest,
                        [{"inputRef": operation.inputRef, "representationId": operation.task_id}], access.spec_digest)
        return _result(operation)
