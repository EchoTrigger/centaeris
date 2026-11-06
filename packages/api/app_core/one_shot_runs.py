"""Atomic acceptance for external, single-response executions."""
from datetime import datetime, timedelta, timezone as utc
import logging
import re
import uuid

from django.db import transaction
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .assets import captured_input_fields
from .hosted_operations import HostedOperationError, lock_operation_identity, request_digest
from .models import AgentRun, OneShotCredential, OneShotOperation, Session, SessionAssetLink, SourceObject
from .one_shot_credentials import authority_is_current, source_object_allowed
from .runtime_client import request_execution_profile, schedule_agent_run_lifecycle
from .session_model_settings import selected_session_model
from .workspace_access import shared_submission_membership_for


logger = logging.getLogger(__name__)
RUN_LIFETIME = timedelta(minutes=10)
DELIVERY_LIFETIME = timedelta(seconds=60)
RECEIPT_LIFETIME = timedelta(hours=24)


def operation_expiry(operation_id):
    match = re.fullmatch(r"([1-9][0-9]{8,10})\.([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12})", operation_id)
    if match is None:
        raise HostedOperationError(400, "operation_id_invalid")
    uuid.UUID(match[2])
    issued_at = datetime.fromtimestamp(int(match[1]), tz=utc.utc)
    now = timezone.now()
    if issued_at > now + timedelta(minutes=5):
        raise HostedOperationError(400, "operation_id_invalid")
    expires_at = issued_at + RECEIPT_LIFETIME
    if expires_at <= now:
        raise HostedOperationError(410, "operation_expired")
    return expires_at


def _locked_credential(credential):
    membership = shared_submission_membership_for(credential.user, credential.workspace_id)
    current = OneShotCredential.objects.select_for_update(of=("self",)).select_related(
        "workspace", "user", "modelConfig",
    ).filter(pk=credential.pk).first()
    if membership is None or current is None or not authority_is_current(current):
        raise HostedOperationError(403, "credential_revoked")
    return current


def _replay(credential, operation_id, digest):
    lock_operation_identity(credential.user, credential.workspace_id, "oneShotRun", f"{credential.pk}:{operation_id}")
    operation = OneShotOperation.objects.filter(credential=credential, operationId=operation_id).first()
    if operation is not None and operation.requestDigest != digest:
        raise HostedOperationError(409, "operation_conflict")
    return operation


def accept_run(credential, operation_id, payload):
    expires_at = operation_expiry(operation_id)
    payload.prompt = payload.prompt.strip()
    if not payload.prompt:
        raise HostedOperationError(400, "empty_prompt")
    refs = payload.source_object_refs
    if refs != sorted(set(refs)):
        raise HostedOperationError(400, "source_object_refs_invalid")
    digest = request_digest(payload)
    with transaction.atomic():
        current = _locked_credential(credential)
        replay = _replay(current, operation_id, digest)
        if replay is not None:
            return replay, None
    profile = request_execution_profile()
    with transaction.atomic():
        current = _locked_credential(credential)
        replay = _replay(current, operation_id, digest)
        if replay is not None:
            return replay, None
        model, thinking_mode = selected_session_model(current.workspace_id, current.modelConfig_id, None)
        sources = list(SourceObject.objects.select_related("source").filter(id__in=refs))
        if len(sources) != len(refs) or any(not source_object_allowed(current, item) for item in sources):
            raise HostedOperationError(404, "source_object_not_found")
        session = Session.objects.create(workspace=current.workspace, owner=current.user, purpose="oneShot")
        asset_refs = []
        for item in sources:
            link = SessionAssetLink.objects.create(
                workspace=current.workspace, session=session, sourceObject=item,
                attachedBy=current.user,
                capturedDisplayName=item.displayName,
                capturedContentType=item.contentType,
                **captured_input_fields(item),
            )
            asset_refs.append(link.pk)
        run = AgentRun.objects.create(
            workspace=current.workspace, session=session, user=current.user,
            modelConfig=model, thinkingMode=thinking_mode, prompt=payload.prompt,
        )
        deadline = timezone.now() + RUN_LIFETIME
        operation = OneShotOperation.objects.create(
            credential=current, operationId=operation_id, requestDigest=digest,
            agentRunId=run.pk, sessionId=session.pk,
            deadlineAt=deadline, expiresAt=expires_at,
            deliveryLeaseUntil=deadline + DELIVERY_LIFETIME,
        )
        create_agent_run_authorization(
            run, message_asset_refs=sorted(asset_refs), image_digest=profile["imageDigest"],
            personal_library=False,
            execution_policy={"purpose": "oneShot", "deadlineAtMs": int(deadline.timestamp() * 1000)},
        )
        from .material_files import check_original_budget
        check_original_budget(run)
        # Profile resolution and transaction locks may outlive the key window.
        if expires_at <= timezone.now():
            raise HostedOperationError(410, "operation_expired")
    try:
        schedule_agent_run_lifecycle(run)
    except Exception:
        logger.exception("One-shot lifecycle scheduling awaits reconciliation", extra={"agentRunId": run.pk})
        AgentRun.objects.filter(pk=run.pk, status="queued").update(transitionReason="agent_run_lifecycle_schedule_pending")
    return operation, run


def refresh_operation(operation):
    run = AgentRun.objects.filter(pk=operation.agentRunId).first()
    if run is not None:
        operation.runStatus = run.status
        operation.reasonCode = run.transitionReason if run.status in {"failed", "cancelled"} else ""
        operation.save(update_fields=["runStatus", "reasonCode"])
    return operation
