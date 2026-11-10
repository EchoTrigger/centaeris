"""Trusted Host transport for original-file admissions and installation receipts."""
from typing import Literal

from django.db import transaction
from django.http import JsonResponse
from ninja import Router
from pydantic import Field

from app_core.assets import DeferredInputResolutionError
from app_core.deferred_input import DeferredInputBindingError
from app_core.material_contract import KnowledgeError
from app_core.material_files import material_context, prepare_original, confirm_original, _current_record, lock_material_run
from app_core.models import SessionMaterialInput, UserLibraryObject
from app_core.runtime_contract import MAX_PROJECTED_INPUTS

from .schema import StrictSchema
from .security import internal_token_auth


router = Router(tags=["internal-material-inputs"], by_alias=True)


class MaterialContextRequest(StrictSchema):
    schema_id: Literal["workspace.material.context.read.v1"] = Field(alias="schema")
    agent_run_id: str
    authorization_digest: str


class MaterialPrepareRequest(StrictSchema):
    schema_id: Literal["workspace.material.prepare.v1"] = Field(alias="schema")
    agent_run_id: str
    authorization_digest: str
    input_ref: str
    call_id: str


class MaterialConfirmRequest(StrictSchema):
    schema_id: Literal["workspace.material.confirm.v1"] = Field(alias="schema")
    agent_run_id: str
    authorization_digest: str
    input_ref: str
    source_version: str
    sha256: str
    call_id: str


class ReconciledInput(StrictSchema):
    input_ref: str
    source_version: str
    sha256: str
    state: Literal["present", "absent"]
    admission_ids: list[int] = Field(max_length=128)


class MaterialReconcileRequest(StrictSchema):
    schema_id: Literal["workspace.material.reconcile.v1"] = Field(alias="schema")
    agent_run_id: str
    authorization_digest: str
    job_id: str
    lease_owner: str
    inputs: list[ReconciledInput] = Field(max_length=MAX_PROJECTED_INPUTS)


def _response(function, *args):
    try:
        return JsonResponse(function(*args), headers={"Cache-Control": "no-store"})
    except KnowledgeError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    except DeferredInputResolutionError as error:
        return JsonResponse({"error": error.errorCode}, status=409)
    except (DeferredInputBindingError, ValueError):
        return JsonResponse({"error": "material_execution_request_invalid"}, status=400)


@router.post("/agent-runs/materials/context", auth=internal_token_auth, response=None, include_in_schema=False)
def read_material_context(request, payload: MaterialContextRequest):
    return _response(material_context, payload.agent_run_id, payload.authorization_digest)


@router.post("/agent-runs/materials/prepare", auth=internal_token_auth, response=None, include_in_schema=False)
def prepare_material_file(request, payload: MaterialPrepareRequest):
    return _response(prepare_original, payload.agent_run_id, payload.authorization_digest, payload.input_ref, payload.call_id)


@router.post("/agent-runs/materials/confirm", auth=internal_token_auth, response=None, include_in_schema=False)
def confirm_material_file(request, payload: MaterialConfirmRequest):
    return _response(confirm_original, payload.agent_run_id, payload.authorization_digest, payload.input_ref,
                     payload.source_version, payload.sha256, payload.call_id)


def _reconcile(payload):
    from .internal import _locked_session_workspace_agent_run, _workspace_lease_request, SessionWorkspaceError
    lease = {key: value for key, value in payload.model_dump(by_alias=True).items() if key != "inputs"}
    try:
        _workspace_lease_request(lease, "workspace.material.reconcile.v1", "material_reconcile_request_invalid")
        with transaction.atomic():
            lock_material_run(payload.agent_run_id, payload.authorization_digest)
            run, _session, _frozen = _locked_session_workspace_agent_run(lease)
            settled = []
            existing = {record.input_ref: record for record in SessionMaterialInput.objects.filter(
                session_id=run.session_id, input_ref__in=[item.input_ref for item in payload.inputs])}
            for item in sorted(payload.inputs, key=lambda item: existing[item.input_ref].resolved["objectRef"] if item.input_ref in existing else item.input_ref):
                try:
                    record = SessionMaterialInput.objects.get(session_id=run.session_id, input_ref=item.input_ref)
                except SessionMaterialInput.DoesNotExist:
                    raise KnowledgeError("material_execution_admission_missing", 409) from None
                if record.resolved["sourceVersion"] != item.source_version or record.resolved["sha256"] != item.sha256:
                    raise KnowledgeError("stale_generation", 409)
                admissions = record.admissions.filter(pk__in=item.admission_ids)
                if admissions.count() != len(set(item.admission_ids)):
                    raise KnowledgeError("material_reconcile_admission_invalid", 409)
                if item.state == "present":
                    if record.resolved["ownerKind"] == "userLibraryObject":
                        UserLibraryObject.objects.select_for_update(of=("self",)).filter(pk=record.resolved["objectRef"]).first()
                    if _current_record(run, record) != record.resolved:
                        raise KnowledgeError("stale_generation", 409)
                    admissions.update(confirmed=True)
                    record.confirmed = True
                else:
                    admissions.update(settled_absent=True)
                    record.confirmed = False
                record.save(update_fields=["confirmed"])
                settled.append(item.input_ref)
            return {"schema": "workspace.material.reconcile.result.v1", "settledInputRefs": settled}
    except SessionWorkspaceError as error:
        raise KnowledgeError(error.code, error.status) from error


@router.post("/agent-runs/materials/reconcile", auth=internal_token_auth, response=None, include_in_schema=False)
def reconcile_material_files(request, payload: MaterialReconcileRequest):
    return _response(_reconcile, payload)
