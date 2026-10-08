"""Product projections over existing, authoritative business Agent records."""
from datetime import datetime, timezone

from django.core import signing

from .agent_history import agent_history
from .agent_inputs import AgentInputError, serialize_input
from .app_delegations import require_request_delegation
from .models import AgentInput


def opaque(kind, chat_id, value):
    return signing.Signer(salt="conversation-public-v1").sign_object([kind, chat_id, value], compress=True)


def unpack(value, kind, chat_id):
    try:
        if not isinstance(value, str) or not 1 <= len(value) <= 4096:
            raise ValueError()
        document = signing.Signer(salt="conversation-public-v1").unsign_object(value)
        if not isinstance(document, list) or len(document) != 3 or document[:2] != [kind, chat_id]:
            raise ValueError()
        return document[2]
    except (signing.BadSignature, ValueError, TypeError, UnicodeError) as error:
        raise AgentInputError(400, "cursor_invalid" if kind.endswith("cursor") else "resource_id_invalid") from error


def timestamp(ms):
    return datetime.fromtimestamp(ms / 1000, timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def project_input(cid, fact):
    return {"id": opaque("message", cid, ["input", fact["inputId"]]), "chatId": cid,
        "author": "user", "text": fact["body"],
        "fileIds": [opaque("file", cid, ["upload", None, a["inputRef"]]) for a in fact["attachments"]],
        "createdAt": timestamp(fact["createdAtMs"]),
        "readAt": timestamp(fact["read"]["createdAtMs"]) if fact["read"] else None}


def project_output(cid, fact):
    return {"id": opaque("message", cid, ["output", fact["id"]]), "chatId": cid,
        "author": "agent", "text": fact["body"],
        "fileIds": [opaque("file", cid, ["output", fact["id"], ref]) for ref in fact["fileRefs"]],
        "createdAt": timestamp(fact["createdAtMs"]), "readAt": None}


def project_item(cid, item):
    return project_input(cid, item["input"]) if item["kind"] in {"input", "read"} else project_output(cid, item["message"])


def authorize(request, scope):
    branch = request.business_branch
    require_request_delegation(request, scope, agent_id=branch.agent_id)
    return request.business_branch


def message(request, cid, mid):
    from .http.agent_previews import PreviewRejected, _message
    branch = authorize(request, "sessions:read")
    try:
        kind, source = unpack(mid, "message", cid)
        if kind == "input":
            fact = AgentInput.objects.filter(agent_id=branch.agent_id, session_id=branch.session_id, input_id=source).first()
            if fact is None:
                raise AgentInputError(404, "message_not_found")
            return project_input(cid, serialize_input(fact))
        if kind == "output":
            return project_output(cid, _message(request.user, branch.agent_id, source)[2])
    except (PreviewRejected, ValueError, TypeError):
        pass
    except AgentInputError as error:
        if error.status != 400:
            raise
    raise AgentInputError(404, "message_not_found")


def history(request, cid, cursor, limit):
    branch = authorize(request, "sessions:read")
    before = unpack(cursor, "history-cursor", cid) if cursor else None
    page = agent_history(request.user, branch.agent_id, before_cursor=before, limit=limit)
    return {"data": [project_item(cid, item) for item in page["items"]],
        "nextCursor": opaque("history-cursor", cid, page["nextCursor"]) if page["hasMore"] else None,
        "hasMore": page["hasMore"]}


def event_page(request, cid, cursor, limit=50):
    """Recover creation and Read updates together from retained authority."""
    branch = authorize(request, "sessions:read")
    after = unpack(cursor, "event-cursor", cid) if cursor else None
    page = agent_history(request.user, branch.agent_id, after_cursor=after, limit=limit,
        from_start=after is None, include_reads=True)
    events = [(opaque("event-cursor", cid, item["cursor"]),
        "message.updated" if item["kind"] == "read" else "message.created", project_item(cid, item))
        for item in page["items"]]
    return events, opaque("event-cursor", cid, page["newestCursor"]), page["hasMore"]


def resolve_file(request, cid, fid):
    from .agent_input_attachments import resolve_agent_attachment
    from .http.agent_previews import _resolve_message_file
    branch = authorize(request, "artifacts:read")
    try:
        kind, source, ref = unpack(fid, "file", cid)
    except (AgentInputError, ValueError, TypeError) as error:
        raise AgentInputError(404, "file_not_found") from error
    if kind in {"input", "upload"}:
        return resolve_agent_attachment(request.user, branch.agent_id, ref, source)
    if kind == "output":
        resolved, key, _ = _resolve_message_file(request.user, branch.agent_id, source, ref)
        return resolved, key
    raise AgentInputError(404, "file_not_found")
