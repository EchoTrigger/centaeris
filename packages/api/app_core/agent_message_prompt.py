"""Provider-local delivery guidance for a verified Agent conversation request."""

from .agent_messages import _CONTRACT
from .models import AgentCoordinationSession, AgentWorkSession


_DELIVERY_GUIDANCE = (
    "You are the user-facing coordinator Agent. Answer simple questions directly, "
    "including greetings and image descriptions. For substantial investigation or "
    "execution, use dispatch_work; check get_work_request and read its output before "
    "reporting results. Native agent/task_output is internal work. "
    f"Reply through {_CONTRACT['name']} with the complete body; use empty session_refs/"
    "file_refs when unnecessary. Only committed send_message is visible; reasoning "
    "and Final are hidden. After sending, continue needed work or finish with Final "
    "without repeating it. Continue authorized work without asking to check later. "
    "Omit internal protocol details."
)

_IMAGE_GUIDANCE = (
    "A native image is included in this request. Inspect it directly first. Use OCR "
    "only for transcription or precise extraction; image questions do not by themselves "
    "require file investigation. If unclear, state what you cannot see. Do not guess "
    "local paths; native images do not imply a sandbox file."
)


def _with_guidance(prepared_prompt, guidance):
    if prepared_prompt.get("inputImages"):
        guidance += " " + _IMAGE_GUIDANCE
    system = prepared_prompt.get("systemPrompt")
    return {**prepared_prompt, "systemPrompt": f"{system}\n\n{guidance}" if system else guidance}

_WORK_GUIDANCE = (
    "This is an ordinary work Session, not the coordinator conversation. Complete the "
    "delegated objective and return the full user-facing result directly in Final. "
    "The coordinator receives this retained work output; do not send a coordinator message "
    "or ask whether it should check your result later. Omit internal protocol details."
)


def conversation_provider_prompt(run, prepared_prompt):
    """Call only after model-run authorization and prepared-prompt validation."""
    work = AgentWorkSession.objects.select_related("operation").filter(session_id=run.session_id).first()
    if work is not None and prepared_prompt["toolChoice"] != {"type": "none"}:
        from .agent_work_presentation import verified_work
        child = verified_work(work)
        if (child is not None and child.session_id == run.session_id
                and child.workspace_id == run.workspace_id and child.user_id == run.user_id
                and child.session.agent_id == run.session.agent_id):
            return _with_guidance(prepared_prompt, _WORK_GUIDANCE)
    if prepared_prompt["toolChoice"] not in (
        {"type": "auto"},
        {"type": "required"},
        {"type": "specific", "name": _CONTRACT["name"]},
    ):
        return prepared_prompt
    definition = {"name": _CONTRACT["name"], "description": _CONTRACT["summary"],
                  "inputSchema": _CONTRACT["inputSchema"]}
    if definition not in prepared_prompt["toolDefinitions"]:
        return prepared_prompt
    if not AgentCoordinationSession.objects.filter(
        agent_id=run.session.agent_id, session_id=run.session_id,
    ).exists():
        return prepared_prompt
    return _with_guidance(prepared_prompt, _DELIVERY_GUIDANCE)
