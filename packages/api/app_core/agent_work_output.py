"""Read a completed work result from its authoritative, retained Final event."""
from .agent_work_returns import _terminal_fact, WorkReturnError
from .agent_work_confirmation import _owned_record
from .models import SessionEvent


def completed_work_output(child):
    fact = _terminal_fact(child)
    if fact is None or fact["kind"] != "sessionTerminal" or fact["state"] != "completed":
        return None
    terminal = SessionEvent.objects.get(pk=fact["factRef"])
    # A run-level terminal uses the AgentRun ID as turnId. The most recent retained
    # Final belongs to the actual model turn and precedes that terminal boundary.
    event = SessionEvent.objects.filter(agent_run=child, session_level=False,
        projects_to_agent_run_stream=True, payload__type="assistant_message",
        sequence__lt=terminal.sequence).order_by("-sequence").first()
    if event is None:
        return None
    payload = event.payload.get("payload", {})
    body = payload.get("modelMarkdown")
    if (not _owned_record(event, child, "assistant_message")
            or event.payload.get("createdAtMs") != event.createdAtMs
            or payload.get("status") != "done" or not isinstance(body, str) or not body.strip()):
        raise WorkReturnError(409, "agent_work_output_untrusted")
    return {"schema": "workspace.agent_work.output.v1", "source": "untrustedWorkOutput",
        "sessionId": child.session_id, "agentRunId": child.id, "eventId": event.eventId,
        "throughSequence": terminal.sequence, "body": body}
