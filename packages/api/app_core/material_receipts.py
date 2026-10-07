"""Server evidence is not a citation until a matching durable tool success exists.

No model text or external MCP metadata can create receipts. Projection reads
committed Session rows; a crash between response and commit leaves inert evidence.
"""

import hashlib
import json

from django.db import transaction

from .models import MaterialEvidenceReceipt, SessionCitationProjection, SessionEvent


PROVIDER_ID = "workspace.materials"
CITABLE_TOOLS = {"read_material", "search_materials", "read_material_result"}
EVIDENCE_FIELDS = ("inputRef", "ownerRef", "ownerKind", "displayName", "evidenceKind",
                   "ownerSha256", "ownerGeneration", "representationId", "specDigest",
                   "evidenceSha256", "locator")


def _identity(prefix, value):
    return prefix + hashlib.sha256(value.encode("utf-8")).hexdigest()


def authorize_call(access, call_id, name, arguments):
    if not isinstance(call_id, str) or not 0 < len(call_id) <= 160:
        raise ValueError("material_call_binding_invalid")
    calls = list(SessionEvent.objects.filter(agent_run=access.agent_run,
        payload__type="tool_call", payload__payload__callId=call_id)[:2])
    if len(calls) != 1:
        raise ValueError("material_call_binding_invalid")
    call = calls[0]
    payload = call.payload["payload"]
    if (call.session_id != access.agent_run.session_id or call.workspace_id != access.agent_run.workspace_id
        or payload.get("providerId") != PROVIDER_ID or payload.get("toolName") != name
        or payload.get("normalizedInput") != arguments):
        raise ValueError("material_call_binding_invalid")
    return call


def persist_receipt(access, call, name, result):
    if name not in CITABLE_TOOLS or result.get("disposition") != "ready":
        return result
    evidence = [{field: item[field] for field in EVIDENCE_FIELDS}
                for item in result["items" if "items" in result else "hits"]
                if item.get("citationAllowed") is True and item.get("content") and item.get("locator")]
    if not evidence:
        return result
    identity = _identity("material.receipt:", call.pk)
    output = {**result, "receiptId": identity,
              "citationIds": [_identity("citation:", f"{identity}:{index}") for index in range(len(evidence))]}
    text = json.dumps(output, ensure_ascii=False)
    with transaction.atomic():
        receipt, _ = MaterialEvidenceReceipt.objects.get_or_create(call=call, defaults={
            "id": identity, "authorizationDigest": access.authorization_digest,
            "responseText": text, "modelProjection": output, "evidence": evidence})
        if (receipt.authorizationDigest != access.authorization_digest
            or receipt.evidence != evidence or json.loads(receipt.responseText) != output):
            raise ValueError("material_receipt_replay_conflict")
    return json.loads(receipt.responseText)


def citation_projections(agent_run, through_sequence=None):
    """Rebuild from durable evidence + success, never from an MCP receipt ID alone.

The caller owns the run-level projection transaction. Repeated/restarted calls
derive identical rows, requiring no ephemeral callback or extra success RPC.
"""
    projections = []
    for receipt in MaterialEvidenceReceipt.objects.filter(call__agent_run=agent_run).select_related("call"):
        call = receipt.call
        payload = call.payload["payload"]
        result_query = SessionEvent.objects.filter(agent_run=agent_run, payload__type="tool_result",
            payload__payload__callId=payload["callId"])
        if through_sequence is not None:
            result_query = result_query.filter(sequence__lte=through_sequence)
        results = list(result_query[:2])
        if len(results) != 1:
            continue
        result = results[0]
        body = result.payload["payload"]
        if (payload.get("providerId") != PROVIDER_ID or payload.get("toolName") not in CITABLE_TOOLS
            or result.session_id != call.session_id or result.workspace_id != call.workspace_id
            or result.agent_run_sequence <= call.agent_run_sequence
            or result.payload.get("turnId") != call.payload.get("turnId")
            or body.get("toolName") != payload["toolName"]
            or body.get("resultState") != "successWithOutput" or body.get("outputComplete") is not True):
            continue
        try:
            output = json.loads(receipt.responseText)
            actual = json.loads(body["modelContent"])
        except (KeyError, TypeError, ValueError):
            continue
        # The MCP adapter projects structured-only results without duplicated text.
        if actual != receipt.modelProjection:
            continue
        for index, evidence in enumerate(receipt.evidence):
            projections.append(SessionCitationProjection(
                citationId=output["citationIds"][index], workspace=agent_run.workspace,
                session=agent_run.session, agent_run=agent_run, sequence=result.sequence,
                sourceToolName=payload["toolName"], sourceToolCallId=payload["callId"], **evidence))
    return projections
