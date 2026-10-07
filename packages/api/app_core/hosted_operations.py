"""Hosted command acceptance, independent of Core execution and current projections."""

import hashlib
import json
import unicodedata

from app_core.assets import MAX_DIRECT_INPUT_BYTES, safe_filename
from app_core.app_delegations import require_delegated_session
from app_core.models import Agent, AgentRun, HostedOperationReceipt, Session


OPERATION_ID_PATTERN = r"^[A-Za-z0-9_.:-]{1,128}$"


class HostedOperationError(Exception):
    def __init__(self, status, code):
        super().__init__(code)
        self.status = status
        self.code = code


def request_digest(payload, *, session_id=None, uploads=()):
    """Hash normalized caller input, never a model default or allocated resource ID."""
    value = payload.model_dump(by_alias=True, exclude={"operation_id"})
    if session_id is not None:
        value["sessionId"] = session_id
        value["text"] = value["text"].strip()
        value["uploads"] = []
        for upload in uploads:
            display_name = unicodedata.normalize("NFC", upload.name)
            safe_filename(display_name)
            digest = hashlib.sha256()
            size = 0
            position = upload.tell()
            try:
                upload.seek(0)
                for chunk in upload.chunks():
                    size += len(chunk)
                    if size > MAX_DIRECT_INPUT_BYTES:
                        raise ValueError("attachment_too_large")
                    digest.update(chunk)
            finally:
                upload.seek(position)
            value["uploads"].append({
                "displayName": display_name,
                "contentType": upload.content_type or "application/octet-stream",
                "sizeBytes": size,
                "sha256": digest.hexdigest(),
            })
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def replay_operation(user, workspace_id, command, operation_id, digest=None, *, app_delegation=None):
    """Caller holds the Workspace/membership lock; authorize before digest comparison."""
    receipt = HostedOperationReceipt.objects.filter(
        user=user, workspace_id=workspace_id, command=command, operationId=operation_id,
    ).first()
    if receipt is None:
        return None
    # Match the existing deletion lock order: Workspace, Agent, then Session.
    binding = Session.objects.filter(id=receipt.sessionId).values("agent_id").first()
    if binding is None:
        raise HostedOperationError(410, "operation_resource_unavailable")
    agent = Agent.objects.select_for_update().filter(id=binding["agent_id"]).first()
    session = Session.objects.select_for_update().filter(id=receipt.sessionId).first()
    if session is None:
        raise HostedOperationError(410, "operation_resource_unavailable")
    if (session.workspace_id != workspace_id or session.owner_id != user.id
            or agent is None or agent.owner_id != user.id or agent.workspace_id != workspace_id):
        raise HostedOperationError(404, "operation_not_found")
    if app_delegation is not None:
        require_delegated_session(app_delegation, session.id, require_active=False)
    if session.status != "active" or agent.status != "active":
        raise HostedOperationError(410, "operation_resource_unavailable")
    if receipt.agentRunId is not None and not AgentRun.objects.filter(
        id=receipt.agentRunId, turn_id=receipt.turnId, session=session,
        user=user, workspace_id=workspace_id,
    ).exists():
        raise HostedOperationError(410, "operation_resource_unavailable")
    if digest is not None and receipt.requestDigest != digest:
        raise HostedOperationError(409, "operation_conflict")
    return receipt


def accept_operation(user, workspace, command, operation_id, digest, session, agent_run=None, *, app_delegation=None):
    # Business records and receipt must be committed in the same caller transaction.
    return HostedOperationReceipt.objects.create(
        user=user, workspace=workspace, command=command, operationId=operation_id,
        requestDigest=digest, sessionId=session.id,
        agentRunId=agent_run.id if agent_run else None,
        turnId=agent_run.turn_id if agent_run else None,
        acting_app=app_delegation.app if app_delegation else None,
        app_delegation=app_delegation,
    )


def serialize_operation(receipt):
    return {"operationId": receipt.operationId, "command": receipt.command,
            "status": "accepted", "sessionId": receipt.sessionId,
            "agentRunId": receipt.agentRunId, "turnId": receipt.turnId}
