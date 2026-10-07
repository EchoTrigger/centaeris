from typing import Literal

from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.agent_messages import AgentMessageRejected, committed_agent_messages, validate_agent_message, validate_message_args
from app_core.agent_inputs import require_native_agent_delegation
from app_core.app_delegations import session_authority_is_current
from app_core.models import AgentCoordinationSession
from app_core.runtime_contract import require_opaque_ref, require_sha256

from .response_schema import AgentMessagesResponse, COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import internal_token_auth, usage_auth


router = Router(tags=["agent-messages"], by_alias=True)
internal_router = Router(tags=["internal-agent-messages"], by_alias=True)


class ValidateAgentMessageRequest(StrictSchema):
    schema_id: Literal["workspace.agent_message.validate.v1"] = Field(alias="schema")
    agent_run_id: str = Field(alias="agentRunId")
    authorization_digest: str = Field(alias="authorizationDigest")
    coordination_session_id: str = Field(alias="coordinationSessionId")
    tool_call_id: str = Field(alias="toolCallId")
    body: str
    session_refs: list[str] = Field(alias="sessionRefs", max_length=8)
    file_refs: list[str] = Field(alias="fileRefs", max_length=8)

    @field_validator("agent_run_id", "coordination_session_id", "tool_call_id")
    @classmethod
    def identity(cls, value):
        require_opaque_ref("agent_message_identity", value)
        return value

    @field_validator("authorization_digest")
    @classmethod
    def digest(cls, value):
        require_sha256("authorizationDigest", value)
        return value

    @field_validator("body")
    @classmethod
    def text(cls, value):
        validate_message_args({"body": value, "session_refs": [], "file_refs": []})
        return value

    @field_validator("session_refs", "file_refs")
    @classmethod
    def refs(cls, value):
        for ref in value:
            require_opaque_ref("resource_ref", ref)
        return value


class ValidatedAgentMessageResponse(StrictSchema):
    schema_id: Literal["workspace.agent_message.validated.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    agent_run_id: str = Field(alias="agentRunId")
    tool_call_id: str = Field(alias="toolCallId")
    authorization_digest: str = Field(alias="authorizationDigest")
    input_digest: str = Field(alias="inputDigest")


@internal_router.post("/agent-messages/validate", auth=internal_token_auth,
    response={200: ValidatedAgentMessageResponse} | COMMON_ERROR_RESPONSES, include_in_schema=False)
def validate_message(request, payload: ValidateAgentMessageRequest):
    try:
        return validate_agent_message(payload.model_dump(by_alias=True))
    except AgentMessageRejected:
        return Status(403, {"error": "agent_message_authority_rejected"})


@router.get("/agents/{agent_id}/messages", auth=usage_auth("sessions:read"),
            response={200: AgentMessagesResponse} | COMMON_ERROR_RESPONSES)
def get_messages(request, agent_id: str, afterSequence: int = 0, limit: int = 50):
    if (set(request.GET) - {"afterSequence", "limit"}
            or any(len(request.GET.getlist(field)) != 1 for field in request.GET)
            or afterSequence < 0 or not 1 <= limit <= 100):
        return Status(400, {"error": "agent_messages_cursor_invalid"})
    require_native_agent_delegation(request, "sessions:read", agent_id=agent_id)
    binding = AgentCoordinationSession.objects.select_related("agent").filter(agent_id=agent_id,
        agent__owner=request.user).first()
    if binding is None or not session_authority_is_current(request.user.id, binding.session_id):
        return Status(404, {"error": "coordination_session_not_found"})
    messages, next_sequence = committed_agent_messages(binding, afterSequence, limit)
    return {"schema": "agent.messages.v1", "agentId": binding.agent_id, "sessionId": binding.session_id,
            "messages": messages, "nextAfterSequence": next_sequence}
