"""Literal search over authorized, committed user-visible Session text."""
from django.core import signing
from django.db.models import F, Func, Q, TextField, Value
from django.db.models.functions import Greatest, Length, Lower, StrIndex, Substr
from django.utils.dateparse import parse_datetime

from .models import SessionEvent


PAGE_SIZE = 20
CURSOR_SALT = "workspace.session-search.v1"
EVENT_TYPES = ["user_message", "phase_event", "assistant_message"]


def search_sessions(sessions, *, user_id, workspace_id, query, cursor=None):
    selection = {"userId": str(user_id), "workspaceId": workspace_id, "query": query}
    after = None
    if cursor is not None:
        try:
            value = signing.loads(cursor, salt=CURSOR_SALT)
            if not isinstance(value, dict) or set(value) != {"selection", "updatedAt", "sessionId"} or value["selection"] != selection:
                raise ValueError
            after = parse_datetime(value["updatedAt"])
            if after is None or after.utcoffset() is None or not isinstance(value["sessionId"], str) or not value["sessionId"]:
                raise ValueError
        except (signing.BadSignature, ValueError, TypeError):
            raise ValueError("session_search_cursor_invalid") from None

    folded = query.lower()
    snippet_length = max(220, len(query) + 140)
    # Both the partial predicate and expression match the database index.
    events = SessionEvent.objects.filter(session__in=sessions, payload__type__in=EVENT_TYPES).annotate(
        _body=Func(F("payload"), function="workspace_session_search_text", output_field=TextField()),
    ).annotate(_folded=Lower("_body")).filter(_folded__contains=folded)
    matches = sessions.annotate(_title=Lower("title")).filter(
        Q(_title__contains=folded) | Q(pk__in=events.values("session_id")),
    )
    if after is not None:
        matches = matches.filter(Q(updatedAt__lt=after) | Q(updatedAt=after, id__gt=value["sessionId"]))
    rows = list(matches.order_by("-updatedAt", "id").values("id", "title", "updatedAt")[:PAGE_SIZE + 1])
    has_more = len(rows) > PAGE_SIZE
    rows = rows[:PAGE_SIZE]
    found = events.filter(session_id__in=[row["id"] for row in rows]).annotate(
        _start=Greatest(StrIndex("_folded", Value(folded)) - Value(70), Value(1)),
        _length=Length("_body"),
    ).annotate(_snippet=Substr("_body", F("_start"), snippet_length)).order_by("session_id", "-sequence").distinct("session_id").values(
        "session_id", "eventId", "sequence", "payload__type", "payload__payload__messageId", "_snippet", "_start", "_length",
    )
    by_session = {event["session_id"]: event for event in found}
    results = []
    for row in rows:
        event = by_session.get(row["id"])
        hit = {"kind": "title", "snippet": "", "eventId": None, "messageId": None, "sourceSequence": None}
        if event is not None:
            snippet = event["_snippet"]
            if event["_start"] > 1:
                snippet = "…" + snippet
            if event["_length"] >= event["_start"] + snippet_length:
                snippet += "…"
            hit = {
                "kind": {"user_message": "user", "phase_event": "stage", "assistant_message": "final"}[event["payload__type"]],
                "snippet": snippet, "eventId": event["eventId"],
                "messageId": event["payload__payload__messageId"], "sourceSequence": str(event["sequence"]),
            }
        results.append({"sessionId": row["id"], "title": row["title"], "updatedAt": row["updatedAt"].isoformat(), "match": hit})
    next_cursor = None
    if has_more:
        last = rows[-1]
        next_cursor = signing.dumps({"selection": selection, "updatedAt": last["updatedAt"].isoformat(),
            "sessionId": last["id"]}, salt=CURSOR_SALT, compress=True)
    return {"matches": results, "nextCursor": next_cursor}
