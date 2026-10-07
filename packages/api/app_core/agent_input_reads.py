"""Read is a projection of the owning Run's committed main request, never ACK."""
from django.db import connection


_MANIFEST_UPTAKE = """
SELECT content.content_json::jsonb AS observation FROM (
    WITH RECURSIVE lineage AS (
        SELECT m.manifest_digest, m.parent_digest, m.manifest_json::jsonb AS document, 0 AS depth,
            (m.manifest_json::jsonb->>'observationCount')::bigint-1 AS target
        FROM runtime.model_observation_manifests m
        WHERE m.session_id=e.session_id AND m.manifest_digest=e.payload#>>'{payload,observations,manifestDigest}'
        UNION ALL
        SELECT m.manifest_digest, m.parent_digest, m.manifest_json::jsonb, l.depth+1, l.target
        FROM lineage l JOIN runtime.model_observation_manifests m
            ON m.session_id=e.session_id AND m.manifest_digest=l.parent_digest
        WHERE l.depth<128
    )
    SELECT change->>'kind' AS kind, change->>'contentDigest' AS digest
    FROM lineage l CROSS JOIN LATERAL jsonb_array_elements(l.document->'changes') change
    WHERE (change->>'index')::bigint=l.target ORDER BY l.depth LIMIT 1
) reference JOIN runtime.model_observation_contents content
    ON content.session_id=e.session_id AND content.content_digest=reference.digest
        AND content.kind=reference.kind
WHERE reference.kind='input_uptake'
"""


def input_read_lateral():
    # API-only startup and migrated test databases need no Runtime schema. This
    # capability lookup contains no input/output facts; the projection below
    # still reads both carriers and their uptake in one statement/snapshot.
    with connection.cursor() as cursor:
        cursor.execute("SELECT to_regclass('runtime.model_observation_manifests') IS NOT NULL "
                       "AND to_regclass('runtime.model_observation_contents') IS NOT NULL")
        has_manifests = cursor.fetchone()[0]
    observations = """
        SELECT observation FROM jsonb_array_elements(CASE
            WHEN jsonb_typeof(e.payload#>'{payload,observations}')='array'
            THEN e.payload#>'{payload,observations}' ELSE '[]'::jsonb END) observation
    """
    if has_manifests:
        observations += " UNION ALL " + _MANIFEST_UPTAKE
    return """
    LEFT JOIN LATERAL (
        SELECT jsonb_build_object('agentRunId', e.agent_run_id, 'eventId', e."eventId",
            'requestId', e.payload#>>'{payload,requestId}', 'createdAtMs', e."createdAtMs") AS value
        FROM app_core_agentinputdelivery d JOIN app_core_agentinputqueue q ON q.agent_run_id=d.queue_id
        JOIN app_core_agentrun r ON r.id=q.agent_run_id
        JOIN app_core_agentrunauthorization a ON a.agent_run_id=r.id AND a.digest=q.authorization_digest
        JOIN app_core_sessionevent e ON e.agent_run_id=r.id AND e.session_id=i.session_id
            AND e.workspace_id=r.workspace_id AND e.agent_run_sequence IS NOT NULL AND NOT e.session_level
        CROSS JOIN LATERAL (""" + observations + """) uptake
        WHERE d.input_id=i.id AND r.session_id=i.session_id AND r.membership_ref=i.membership_ref
            AND e.payload->>'type'='model_request_started' AND e.payload#>>'{payload,purpose}'='main'
            AND COALESCE(e.payload#>>'{payload,requestId}', '')<>''
            AND uptake.observation->>'kind'='input_uptake'
            AND jsonb_typeof(uptake.observation->'inputIds')='array'
            AND uptake.observation->'inputIds' ? i.input_id
        ORDER BY e.sequence LIMIT 1
    ) input_read ON TRUE
    """


def input_read(fact):
    lateral = input_read_lateral()
    with connection.cursor() as cursor:
        cursor.execute("SELECT input_read.value FROM app_core_agentinput i " + lateral +
                       " WHERE i.id=%s", [fact.pk])
        row = cursor.fetchone()
    return row[0] if row is not None else None
