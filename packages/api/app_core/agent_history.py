"""One ordered projection of retained inputs and verified committed outputs."""
import base64
import json
from types import SimpleNamespace

from django.db import connection

from .agent_inputs import AgentInputError, owned_input_binding, serialize_input
from .agent_messages import _trusted_message


def encode_history_cursor(session_id, position):
    raw = json.dumps({"schema": "agent.history.cursor.v1", "sessionId": session_id,
        "position": list(position)}, separators=(",", ":")).encode("utf-8")
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate cursor field")
        result[key] = value
    return result


def decode_history_cursor(cursor, session_id, *, include_reads=False):
    if cursor is None:
        return (-1, 0, 0)
    try:
        if not isinstance(cursor, str) or not 1 <= len(cursor) <= 512:
            raise ValueError("invalid cursor")
        raw = base64.b64decode(cursor + "=" * (-len(cursor) % 4), altchars=b"-_", validate=True)
        value = json.loads(raw, object_pairs_hook=_unique_object)
        if (set(value) != {"schema", "sessionId", "position"}
                or value["schema"] != "agent.history.cursor.v1" or value["sessionId"] != session_id):
            raise ValueError("invalid cursor scope")
        position = value["position"]
        if (not isinstance(position, list) or len(position) != 3
                or any(type(part) is not int or part < 0 for part in position)
                or position[0] > 2_147_483_647 or position[2] > 9_223_372_036_854_775_807
                or position[1] not in ((0, 1, 2) if include_reads else (0, 1))
                or (position[1] == 0 and (position[0] == 0 or position[2] != 0))
                or (position[1] == 1 and position[2] == 0)
                or encode_history_cursor(session_id, position) != cursor):
            raise ValueError("invalid cursor position")
        return tuple(position)
    except (ValueError, TypeError, KeyError, UnicodeError) as error:
        raise AgentInputError(400, "agent_history_cursor_invalid") from error


# One SQL statement gives both carriers and each output's matching call one
# MVCC snapshot. Separate input/output queries can skip a concurrent input.
_HISTORY_CANDIDATES = """
SELECT anchor, kind_rank, tie, document FROM (
    SELECT i.accepted_source_sequence AS anchor, 1 AS kind_rank, i.sequence AS tie,
        jsonb_build_object('kind', 'input', 'input_id', i.input_id, 'sequence', i.sequence,
            'created_at_ms', i.created_at_ms, 'body', i.body, 'attachments', i.attachments,
            'read', input_read.value) AS document
    FROM app_core_agentinput i {input_read_lateral} WHERE i.session_id=%s AND i.agent_id=%s
    UNION ALL
    SELECT e.sequence AS anchor, 0 AS kind_rank, 0 AS tie,
        jsonb_build_object('kind', 'message', 'eventId', e."eventId", 'sequence', e.sequence,
            'createdAtMs', e."createdAtMs", 'wire', e.payload,
            'run', jsonb_build_object('id', r.id, 'session_id', r.session_id,
                'workspace_id', r.workspace_id, 'user_id', r.user_id),
            'workSnapshot', COALESCE(w.value, '[]'::jsonb),
            'call', CASE WHEN c."eventId" IS NULL THEN NULL ELSE
                jsonb_build_object('eventId', c."eventId", 'sequence', c.sequence, 'payload', c.payload) END)
    FROM app_core_sessionevent e JOIN app_core_agentrun r ON r.id=e.agent_run_id
    LEFT JOIN LATERAL (
        SELECT call."eventId", call.sequence, call.payload FROM app_core_sessionevent call
        WHERE call.session_id=e.session_id AND call.agent_run_id=e.agent_run_id
            AND call.projects_to_agent_run_stream AND call.sequence<e.sequence
            AND call.payload->>'type'='tool_call'
            AND call.payload->>'turnId'=e.payload->>'turnId'
            AND call.payload#>>'{payload,callId}'=e.payload#>>'{payload,callId}'
        ORDER BY call.sequence DESC LIMIT 1
    ) c ON TRUE
    {work_snapshot_lateral}
    WHERE e.session_id=%s AND e.workspace_id=%s AND e.projects_to_agent_run_stream
        AND e.payload->>'type'='tool_result'
        AND e.payload#>>'{payload,toolName}'='send_message'
        AND e.payload#>>'{payload,resultState}'='successWithOutput'
) candidates WHERE (anchor, kind_rank, tie)>(%s, %s, %s)
ORDER BY anchor, kind_rank, tie LIMIT %s
"""


def _event_document(alias):
    return f"""jsonb_build_object('eventId', {alias}."eventId", 'sequence', {alias}.sequence,
        'createdAtMs', {alias}."createdAtMs", 'payload', {alias}.payload,
        'workspace_id', {alias}.workspace_id, 'session_id', {alias}.session_id,
        'agent_run_id', {alias}.agent_run_id, 'session_level', {alias}.session_level,
        'projects_to_agent_run_stream', {alias}.projects_to_agent_run_stream)"""


def _work_snapshot_lateral():
    source, prior, call = (_event_document(alias) for alias in ("origin", "prior", "pc"))
    return """
    LEFT JOIN LATERAL (
        SELECT jsonb_agg(view.document ORDER BY view.created_at, view.session_id) AS value FROM (
            SELECT work.created_at, work.session_id, jsonb_build_object('sessionId', work.session_id,
                'source', SOURCE_DOCUMENT, 'earlier', COALESCE(previous.value, '[]'::jsonb)) AS document
            FROM app_core_agentworksession work
            JOIN app_core_session child ON child.id=work.session_id
            JOIN app_core_sessionevent origin ON origin."eventId"=work.source_event_id
            LEFT JOIN LATERAL (
                SELECT jsonb_agg(earlier.document ORDER BY earlier.sequence) AS value FROM (
                    SELECT prior.sequence, jsonb_build_object('result', PRIOR_DOCUMENT,
                        'call', CASE WHEN pc."eventId" IS NULL THEN NULL ELSE CALL_DOCUMENT END) AS document
                    FROM app_core_sessionevent prior
                    LEFT JOIN LATERAL (
                        SELECT matched.* FROM app_core_sessionevent matched
                        WHERE matched.session_id=prior.session_id AND matched.agent_run_id=prior.agent_run_id
                            AND matched.projects_to_agent_run_stream AND matched.sequence<prior.sequence
                            AND matched.payload->>'type'='tool_call'
                            AND matched.payload->>'turnId'=prior.payload->>'turnId'
                            AND matched.payload#>>'{payload,callId}'=prior.payload#>>'{payload,callId}'
                        ORDER BY matched.sequence DESC LIMIT 1
                    ) pc ON TRUE
                    WHERE prior.session_id=e.session_id AND prior.workspace_id=e.workspace_id
                        AND prior.agent_run_id=e.agent_run_id AND prior.projects_to_agent_run_stream
                        AND prior.sequence>origin.sequence AND prior.sequence<e.sequence
                        AND prior.payload->>'type'='tool_result'
                        AND prior.payload#>>'{payload,toolName}'='send_message'
                        AND prior.payload#>>'{payload,resultState}'='successWithOutput'
                    ORDER BY prior.sequence LIMIT 101
                ) earlier
            ) previous ON TRUE
            WHERE work.coordination_session_id=e.session_id AND work.source_run_id=e.agent_run_id
                AND origin.sequence<e.sequence AND child.status='active' AND child."purgedAt" IS NULL
                AND child.owner_id=r.user_id AND child.workspace_id=r.workspace_id
            ORDER BY work.created_at, work.session_id LIMIT 8
        ) view
    ) w ON TRUE
    """.replace("SOURCE_DOCUMENT", source).replace("PRIOR_DOCUMENT", prior).replace("CALL_DOCUMENT", call)


def agent_history(user, agent_id, after_cursor=None, limit=50, before_cursor=None, *, from_start=False, include_reads=False):
    from .agent_input_reads import input_read_lateral
    binding = owned_input_binding(user, agent_id)
    newer = after_cursor is not None or from_start
    supplied_cursor = after_cursor if newer else before_cursor
    position = decode_history_cursor(supplied_cursor, binding.session_id, include_reads=include_reads)
    query = _HISTORY_CANDIDATES.replace("{input_read_lateral}", input_read_lateral()).replace(
        "{work_snapshot_lateral}", _work_snapshot_lateral())
    parameters = [binding.session_id, binding.agent_id, binding.session_id, binding.agent.workspace_id]
    if include_reads:
        # Recover updates from the same snapshot and verified uptake projection
        # as creation. A disconnected reader cannot miss an older input's Read.
        updates = """
        UNION ALL
        SELECT read_event.sequence AS anchor, 2 AS kind_rank, i.sequence AS tie,
            jsonb_build_object('kind', 'read', 'input_id', i.input_id, 'sequence', i.sequence,
                'created_at_ms', i.created_at_ms, 'body', i.body, 'attachments', i.attachments,
                'read', input_read.value) AS document
        FROM app_core_agentinput i READ_LATERAL
        JOIN app_core_sessionevent read_event ON read_event."eventId"=input_read.value->>'eventId'
        WHERE i.session_id=%s AND i.agent_id=%s
        """.replace("READ_LATERAL", input_read_lateral())
        query = query.replace(") candidates WHERE", updates + ") candidates WHERE")
        parameters.extend([binding.session_id, binding.agent_id])
    if not newer:
        query = query.replace(">(%s, %s, %s)", "<(%s, %s, %s)").replace("ORDER BY anchor, kind_rank, tie", "ORDER BY anchor DESC, kind_rank DESC, tie DESC")
        if before_cursor is None:
            position = (2_147_483_648, 0, 0)
    with connection.cursor() as cursor:
        cursor.execute(query, [*parameters, *position, limit + 1])
        candidates = cursor.fetchall()
    items, seen, next_cursor = [], set(), supplied_cursor
    newest_cursor = after_cursor
    if candidates:
        newest_cursor = encode_history_cursor(binding.session_id, candidates[min(limit, len(candidates)) - 1 if newer else 0][:3])
    for anchor, rank, tie, document in candidates[:limit]:
        next_cursor = encode_history_cursor(binding.session_id, (anchor, rank, tie))
        if isinstance(document, str):
            document = json.loads(document)
        if document["kind"] in {"input", "read"}:
            fact = SimpleNamespace(**{key: document[key] for key in ("input_id", "sequence", "created_at_ms", "body", "attachments", "read")})
            items.append({"cursor": next_cursor, "kind": document["kind"], "input": serialize_input(fact)})
            continue
        call = SimpleNamespace(**document["call"]) if document["call"] is not None else None
        result = SimpleNamespace(eventId=document["eventId"], sequence=document["sequence"],
            createdAtMs=document["createdAtMs"], payload=document["wire"],
            agent_run=SimpleNamespace(**document["run"]))
        message = _trusted_message(binding, result, call=call, work_snapshot=document["workSnapshot"])
        if message is not None:
            identity = (message["agentRunId"], message["turnId"], message["toolCallId"])
            if identity not in seen:
                seen.add(identity)
                items.append({"cursor": next_cursor, "kind": "message", "message": message})
    if not newer:
        items.reverse()
    return {"schema": "agent.history.v1", "agentId": binding.agent_id, "sessionId": binding.session_id,
        "items": items, "nextCursor": next_cursor, "newestCursor": newest_cursor, "hasMore": len(candidates) > limit}
