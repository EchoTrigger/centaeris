"""Read activity from committed Run facts and Core's current wait authority."""
import json

from django.db import connection

from .models import SessionEvent


ACTIVITY_EVENTS = (
    "model_request_started", "tool_call", "tool_result", "tool_call_closure",
    "agent_run_execution_started", "agent_run_execution_ended", "phase_event",
)
QUESTION_FIELDS = {
    "schema", "continuationId", "agentRunId", "authorizationDigest", "turnId", "questionId",
}


def presentation_activity(run, source_high_water):
    if run is None or run.status not in {"queued", "running"}:
        return None
    if run.status == "queued":
        return "working"
    if run.status == "running":
        # Checkpoint consumption deletes this authority. Historical paused facts
        # alone cannot establish that the Run is still waiting for input.
        with connection.cursor() as cursor:
            cursor.execute(
                "SELECT turn_id,payload_json FROM runtime.checkpoints "
                "WHERE session_id=%s AND kind='wait' AND status='paused_question' "
                "AND done_reason='question' AND payload_json::jsonb->>'agentRunId'=%s "
                "AND payload_json::jsonb->>'authorizationDigest'=%s",
                [run.session_id, run.id, run.authorization.digest],
            )
            for turn_id, raw in cursor.fetchall():
                payload = json.loads(raw)
                if set(payload) != QUESTION_FIELDS or payload["schema"] != "runtime.await_question.v1":
                    raise RuntimeError("presentation_question_checkpoint_invalid")
                if payload["turnId"] == turn_id and all(
                    isinstance(payload[field], str) and payload[field].strip()
                    for field in QUESTION_FIELDS
                ):
                    return "waitingInput"
    latest = SessionEvent.objects.filter(
        session_id=run.session_id, agent_run_id=run.id, sequence__lte=source_high_water,
        payload__type__in=ACTIVITY_EVENTS,
    ).order_by("-sequence").values_list("payload__type", flat=True).first()
    return "thinking" if latest == "model_request_started" else "working"
