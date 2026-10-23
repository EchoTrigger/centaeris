"""Host display DTOs for committed tool facts, separate from Core event schemas."""
import json

from django.db.models import JSONField
from django.db.models.expressions import RawSQL

from .session_payload import decode_session_payload


OPERATION_PREVIEW_BYTES = 512 * 1024
OPERATION_PREVIEW_STRING_BYTES = 65536


def operation_records(query):
    # Query only display fields. In particular a multi-megabyte modelContent or
    # private output path must not be fetched to draw an ordinary tool node.
    # PostgreSQL cannot traverse a JSON object containing escaped U+0000, even
    # with the json rather than jsonb type. Only those exceptional envelope
    # records need the existing application-side lossless decoder; ordinary
    # records select display fields without transferring modelContent.
    fields = ("callId", "toolName", "normalizedInput", "resultState", "operations")
    expression = "jsonb_build_object('type', payload->'type', " + ", ".join(
        "'%s', payload #> '{payload,%s}'" % (field, field) for field in fields
    ) + ")"
    return query.annotate(display=RawSQL(expression, (), output_field=JSONField()),
        canonicalRecord=RawSQL("CASE WHEN payload ? '__centaerisSessionStorage' THEN payload ELSE NULL END", (), output_field=JSONField()),
    ).values("eventId", "display", "canonicalRecord")


def operation_detail(record, *, preview=True):
    payload = record["display"]
    if record.get("canonicalRecord") is not None:
        wire = decode_session_payload(record["canonicalRecord"])
        payload = {"type": wire.get("type"), **wire.get("payload", {})}
    event_type = payload.get("type")
    call_id, tool_name = payload.get("callId"), payload.get("toolName")
    if event_type not in {"tool_call", "tool_result"} or not isinstance(call_id, str) or not call_id or not isinstance(tool_name, str) or not tool_name:
        raise ValueError("presentation_operation_identity_invalid")
    normalized = payload.get("normalizedInput")
    operations = payload.get("operations")
    result = payload.get("resultState")
    if ((normalized is not None and not isinstance(normalized, dict))
            or (operations is not None and not isinstance(operations, list))
            or (result is not None and not isinstance(result, str))):
        raise ValueError("presentation_operation_invalid")
    detail = {"kind": "call" if event_type == "tool_call" else "result", "callId": call_id,
        "toolName": tool_name, "normalizedInput": normalized, "resultState": result,
        "operations": operations or [], "previewTruncated": False, "sourceEventId": record["eventId"]}
    if not preview:
        return detail
    string_limit = OPERATION_PREVIEW_STRING_BYTES
    while True:
        truncated = False
        nodes = 0

        def bounded(value, depth=0, key=None):
            nonlocal truncated, nodes
            nodes += 1
            if isinstance(value, str):
                encoded = value.encode()
                if len(encoded) > string_limit and key not in {"callId", "operationId", "toolName", "kind", "resultState", "sourceEventId"}:
                    truncated = True
                    return encoded[:string_limit].decode("utf-8", errors="ignore")
                return value
            if depth > 12 or nodes > 4096:
                truncated = True
                return None
            if isinstance(value, list):
                if len(value) > 128:
                    truncated = True
                items = []
                for item in value[:128]:
                    if nodes > 4096:
                        truncated = True
                        break
                    items.append(bounded(item, depth + 1))
                return items
            if isinstance(value, dict):
                if len(value) > 128:
                    truncated = True
                return {name: bounded(item, depth + 1, name) for name, item in list(value.items())[:128]}
            return value

        candidate = bounded(detail)
        if not isinstance(candidate["operations"], list):
            candidate["operations"] = []
            truncated = True
        candidate["previewTruncated"] = truncated
        if len(json.dumps(candidate, ensure_ascii=False, separators=(",", ":")).encode()) <= OPERATION_PREVIEW_BYTES:
            return candidate
        string_limit //= 2
        if string_limit < 128:
            # Core identities themselves have bounded lengths. A record whose
            # structural overhead exceeds the display budget is not a valid
            # preview; the explicit download remains available from its id.
            return {**detail, "normalizedInput": None, "operations": [], "previewTruncated": True}
