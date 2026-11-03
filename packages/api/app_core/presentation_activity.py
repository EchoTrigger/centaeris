"""Read activity from committed Run facts and Core's current wait authority."""
import json

from django.db import connection

from .models import AgentRunEnvironmentRelease, SessionEvent


ACTIVITY_EVENTS = (
    "model_request_started", "tool_call", "tool_result", "tool_call_closure",
    "agent_run_execution_started", "agent_run_execution_ended", "phase_event",
)
QUESTION_FIELDS = {
    "schema", "continuationId", "agentRunId", "authorizationDigest", "turnId", "questionId", "question", "toolCall",
}


def presentation_question(run):
    if run is None or run.status != "running":
        return None, False
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
            if set(payload) != QUESTION_FIELDS or payload["schema"] != "runtime.await_question.v2":
                raise RuntimeError("presentation_question_checkpoint_invalid")
            question, tool = payload["question"], payload["toolCall"]
            if (not isinstance(question, dict) or set(question) != {"question", "options"}
                    or not isinstance(question["question"], str) or not question["question"].strip()
                    or not isinstance(question["options"], list)
                    or any(not isinstance(option, str) or not option.strip() for option in question["options"])
                    or not isinstance(tool, dict) or set(tool) != {"id", "name", "argsJson"}
                    or tool["name"] != "request_user_input"
                    or any(not isinstance(tool[key], str) or not tool[key].strip() for key in ("id", "argsJson"))):
                raise RuntimeError("presentation_question_checkpoint_invalid")
            if payload["turnId"] == turn_id and all(
                isinstance(payload[field], str) and payload[field].strip()
                for field in QUESTION_FIELDS - {"question", "toolCall"}
            ):
                # The response receipt is authoritative before Core resumes the
                # original Job and consumes its checkpoint. Restore that fact in
                # every tab rather than reviving an already answered question.
                cursor.execute(
                    "SELECT EXISTS(SELECT 1 FROM runtime.runtime_events WHERE event_id=%s "
                    "AND session_id=%s AND task_id=%s AND event_type='runtime.question.response.v1' "
                    "AND payload_json::jsonb->>'schema'='runtime.question.response.v1' "
                    "AND payload_json::jsonb->>'agentRunId'=%s AND payload_json::jsonb->>'authorizationDigest'=%s "
                    "AND payload_json::jsonb->>'continuationId'=%s AND payload_json::jsonb->>'questionId'=%s)",
                    ["runtime_question_response:" + payload["continuationId"], run.session_id, turn_id,
                        run.id, run.authorization.digest, payload["continuationId"], payload["questionId"]],
                )
                if cursor.fetchone()[0]:
                    return None, True
                return {key: payload[key] for key in ("questionId", "continuationId", "turnId")} | question, False
    return None, False


def presentation_activity(run, source_high_water, *, question, response_accepted):
    if run is None:
        return None
    if run.status in {"completed", "failed", "cancelled"}:
        return "preserving" if (run.environment_id == run.session.current_environment_id
            and not AgentRunEnvironmentRelease.objects.filter(agent_run=run).exists()) else None
    if question is not None:
        return "waitingInput"
    if response_accepted:
        return "working"
    if run.status == "queued":
        return "working"
    latest = SessionEvent.objects.filter(
        session_id=run.session_id, agent_run_id=run.id, sequence__lte=source_high_water,
        payload__type__in=ACTIVITY_EVENTS,
    ).order_by("-sequence").values_list("payload", flat=True).first()
    if latest is not None and latest["type"] == "phase_event":
        phase = latest["payload"]
        if phase["stage"] == "model_process_state":
            return {"retrying": "retrying", "thinking": "thinking"}.get(phase["processState"], "working")
    return "thinking" if latest is not None and latest["type"] == "model_request_started" else "working"
