"""Short database admissions and Host-confirmed original-file delivery facts."""
from django.db import transaction
from django.db.models import Q

from .assets import DeferredInputResolutionError, MAX_DIRECT_INPUT_BYTES
from .deferred_input import resolved_input_storage, verify_input_run
from .library_materials import input_identity, identity_key, personal_library_allowed, remember_input, resolve_library_input
from .material_contract import KnowledgeError
from .material_receipts import authorize_call
from .models import AgentRun, RunMaterialAdmission, Session, SessionMaterialInput, UserLibraryObject
from .workspace_access import shared_submission_membership_for
from .runtime_contract import MAX_PROJECTED_INPUTS


def lock_material_run(run_id, digest):
    """Read routing facts first, then authority → Session → Run. Caller owns transaction."""
    try:
        routing = AgentRun.objects.only("workspace_id", "session_id", "user_id").get(pk=run_id)
        membership = shared_submission_membership_for(routing.user, routing.workspace_id)
        if membership is None:
            raise KnowledgeError("knowledge_authorization_mismatch", 403)
        Session.objects.select_for_update().get(pk=routing.session_id, status="active", owner_id=routing.user_id)
        run = AgentRun.objects.select_for_update(of=("self",)).select_related("authorization", "environment", "session").get(pk=run_id, status__in={"queued", "running"})
    except (AgentRun.DoesNotExist, Session.DoesNotExist):
        raise KnowledgeError("knowledge_agent_run_not_found", 404) from None
    verify_input_run(run, digest)
    return run


def _original_budget(run):
    """All unsettled reservations survive failures until authoritative Host reconciliation."""
    records = SessionMaterialInput.objects.filter(session_id=run.session_id).filter(
        Q(confirmed=True) | Q(admissions__confirmed=False, admissions__settled_absent=False)).distinct()
    declared_identities = {identity_key(item["inputIdentity"]) for item in run.authorization.payload["assetRefs"]}
    retained_identities = set(declared_identities)
    additional_bytes = 0
    for item in records:
        retained_identities.add(item.identity_key)
        if item.identity_key not in declared_identities:
            additional_bytes += item.resolved["sizeBytes"]
    return additional_bytes, len(retained_identities)


def reserved_original_bytes(run):
    return _original_budget(run)[0]


def check_original_budget(run):
    attached = sum(item["sizeBytes"] for item in run.authorization.payload["assetRefs"])
    originals, retained_count = _original_budget(run)
    if retained_count > MAX_PROJECTED_INPUTS:
        raise KnowledgeError("material_execution_input_count_exceeded", 409)
    capacity = run.authorization.payload["environmentProfile"]["resources"]["workspaceBytes"]
    if attached + originals > capacity // 2 or attached + originals + run.session.workspaceExpandedSizeBytes + 64 * 1024 > capacity:
        raise KnowledgeError("material_execution_input_budget_exceeded", 409)


def _current_record(run, record):
    if record.resolved["ownerKind"] == "userLibraryObject":
        library = resolve_library_input(run, record.input_ref, include_content=False)
        if library is None:
            raise DeferredInputResolutionError("asset_unavailable")
        return library[0]
    resolved, _ = resolved_input_storage(run, record.input_ref, run.authorization.digest)
    return resolved


def material_context(run_id, digest):
    with transaction.atomic():
        run = lock_material_run(run_id, digest)
        inputs, pending = [], []
        records = SessionMaterialInput.objects.filter(session_id=run.session_id).filter(
            Q(confirmed=True) | Q(admissions__confirmed=False, admissions__settled_absent=False)).distinct().order_by("id")
        for record in records:
            try:
                current = _current_record(run, record)
                state = "active" if current == record.resolved else "stale_generation"
            except DeferredInputResolutionError as error:
                state = error.errorCode
            reservations = list(record.admissions.filter(confirmed=False, settled_absent=False).select_related("origin_call"))
            if record.confirmed and state == "active":
                inputs.append(record.resolved)
            elif reservations or record.confirmed:
                pending.append({"resolvedInput": record.resolved, "state": state, "reservations": [
                    {"admissionId": item.pk, "originRunId": item.agent_run_id,
                     "callId": item.origin_call.payload["payload"]["callId"] if item.origin_call_id else None}
                    for item in reservations]})
        return {"schema": "workspace.material.context.result.v1", "personalLibrary": personal_library_allowed(run),
                "inputs": inputs, "pendingInputs": pending}


def _file_call(run, call_id, ref):
    # Match the committed original invocation; no model or external MCP may invent provenance.
    from types import SimpleNamespace
    return authorize_call(SimpleNamespace(agent_run=run), call_id, "read_material",
                          {"input_ref": ref, "delivery": "execution_file"})


def prepare_original(run_id, digest, ref, call_id):
    with transaction.atomic():
        run = lock_material_run(run_id, digest)
        call = _file_call(run, call_id, ref)
        # Serialize with Library version/deletion writers, never acquire a processing Task.
        record = SessionMaterialInput.objects.filter(session_id=run.session_id, input_ref=ref).first()
        owner_id = record.resolved["objectRef"] if record and record.resolved["ownerKind"] == "userLibraryObject" else (
            ref.split(":")[1] if ref.startswith("library:") and len(ref.split(":")) == 4 else None)
        if owner_id is None:
            declared = next((item for item in run.authorization.payload["assetRefs"] if item["inputRef"] == ref), None)
            if declared is not None and declared["inputIdentity"]["ownerKind"] == "userLibraryObject":
                owner_id = declared["inputIdentity"]["ownerId"]
        if owner_id:
            UserLibraryObject.objects.select_for_update(of=("self",)).filter(pk=owner_id).first()
        resolved, _source = resolved_input_storage(run, ref, digest)
        if resolved["sizeBytes"] > MAX_DIRECT_INPUT_BYTES:
            raise KnowledgeError("attachment_too_large", 400)
        record = remember_input(run, resolved)
        admission, _ = RunMaterialAdmission.objects.get_or_create(agent_run=run, material_input=record, origin_call=call)
        if admission.settled_absent:
            raise KnowledgeError("material_execution_call_settled", 409)
        check_original_budget(run)
        return {"schema": "workspace.material.prepare.result.v1", "resolvedInput": record.resolved}


def confirm_original(run_id, digest, ref, version, sha256, call_id):
    with transaction.atomic():
        run = lock_material_run(run_id, digest)
        call = _file_call(run, call_id, ref)
        try:
            admission = RunMaterialAdmission.objects.select_related("material_input").get(
                agent_run=run, material_input__input_ref=ref, origin_call=call, settled_absent=False)
        except RunMaterialAdmission.DoesNotExist:
            raise KnowledgeError("material_execution_admission_missing", 409) from None
        record = admission.material_input
        if record.resolved["ownerKind"] == "userLibraryObject":
            UserLibraryObject.objects.select_for_update(of=("self",)).filter(pk=record.resolved["objectRef"]).first()
        current = _current_record(run, record)
        if current != record.resolved or current["sourceVersion"] != version or current["sha256"] != sha256:
            raise KnowledgeError("stale_generation", 409)
        admission.confirmed = True
        admission.save(update_fields=["confirmed"])
        record.confirmed = True
        record.save(update_fields=["confirmed"])
        return {"schema": "workspace.material.confirm.result.v1", "resolvedInput": current}
