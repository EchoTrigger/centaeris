"""Strict hosted input contract. Execution intake is not certified by acceptance."""
from typing import Literal

from ninja import Router, Status
from pydantic import Field, field_validator, model_validator

from app_core.agent_input_contract import validate_input_body, validate_input_id
from app_core.agent_inputs import AgentInputError, accept_agent_input, input_history, require_native_agent_delegation, serialize_input

from .response_schema import COMMON_ERROR_RESPONSES
from .response_schema import AgentMessageResponse
from .schema import StrictSchema
from .security import usage_auth


router = Router(tags=["agent-inputs"], by_alias=True)


class SubmitAgentInputRequest(StrictSchema):
    schema_id: Literal["agent.input.submit.v1"] = Field(alias="schema")
    input_id: str = Field(alias="inputId")
    body: str
    attachment_refs: list[str] = Field(alias="attachmentRefs", max_length=50)

    @field_validator("input_id")
    @classmethod
    def identity(cls, value):
        return validate_input_id(value)

    @field_validator("body")
    @classmethod
    def text(cls, value):
        return validate_input_body(value, allow_empty=True)

    @model_validator(mode="after")
    def content(self):
        from app_core.agent_input_attachments import validate_attachment_refs
        validate_attachment_refs(self.attachment_refs)
        validate_input_body(self.body, allow_empty=bool(self.attachment_refs))
        return self


class AgentInputRead(StrictSchema):
    agent_run_id: str = Field(alias="agentRunId")
    event_id: str = Field(alias="eventId")
    request_id: str = Field(alias="requestId")
    created_at_ms: int = Field(alias="createdAtMs")


class AgentInputAttachment(StrictSchema):
    input_ref: str = Field(alias="inputRef")
    display_name: str = Field(alias="displayName")
    content_type: str = Field(alias="contentType")


class AgentInputFact(StrictSchema):
    input_id: str = Field(alias="inputId")
    sequence: int = Field(ge=1)
    created_at_ms: int = Field(alias="createdAtMs")
    body: str
    attachments: list[AgentInputAttachment]
    read: AgentInputRead | None


class AgentInputAccepted(StrictSchema):
    schema_id: Literal["agent.input.accepted.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    input: AgentInputFact


class AgentInputs(StrictSchema):
    schema_id: Literal["agent.inputs.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    inputs: list[AgentInputFact]
    next_after_sequence: int | None = Field(alias="nextAfterSequence")


class HistoryInput(StrictSchema):
    cursor: str
    kind: Literal["input"]
    input: AgentInputFact


class HistoryMessage(StrictSchema):
    cursor: str
    kind: Literal["message"]
    message: AgentMessageResponse


class AgentHistory(StrictSchema):
    schema_id: Literal["agent.history.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    items: list[HistoryInput | HistoryMessage]
    next_cursor: str | None = Field(alias="nextCursor")
    newest_cursor: str | None = Field(alias="newestCursor")
    has_more: bool = Field(alias="hasMore")


@router.post("/agents/{agent_id}/inputs", auth=usage_auth("messages:submit"),
    response={200: AgentInputAccepted, 201: AgentInputAccepted} | COMMON_ERROR_RESPONSES)
def submit_input(request, agent_id: str, payload: SubmitAgentInputRequest):
    try:
        fact, created = accept_agent_input(request.user, agent_id, payload.input_id, payload.body,
            payload.attachment_refs, request=request)
        return Status(201 if created else 200, {"schema": "agent.input.accepted.v1",
            "agentId": agent_id, "sessionId": fact.session_id, "input": serialize_input(fact)})
    except AgentInputError as error:
        return Status(error.status, {"error": error.code})


@router.get("/agents/{agent_id}/history", auth=usage_auth("sessions:read"),
    response={200: AgentHistory} | COMMON_ERROR_RESPONSES)
def get_history(request, agent_id: str, afterCursor: str | None = None, beforeCursor: str | None = None, limit: int = 50):
    if (set(request.GET) - {"afterCursor", "beforeCursor", "limit"}
            or afterCursor is not None and beforeCursor is not None
            or any(len(request.GET.getlist(field)) != 1 for field in request.GET)
            or not 1 <= limit <= 100):
        return Status(400, {"error": "agent_history_cursor_invalid"})
    from app_core.agent_history import agent_history
    try:
        require_native_agent_delegation(request, "sessions:read", agent_id=agent_id)
        return agent_history(request.user, agent_id, afterCursor, limit, beforeCursor)
    except AgentInputError as error:
        return Status(error.status, {"error": error.code})


@router.get("/agents/{agent_id}/inputs", auth=usage_auth("sessions:read"),
    response={200: AgentInputs} | COMMON_ERROR_RESPONSES)
def get_inputs(request, agent_id: str, afterSequence: int = 0, limit: int = 50):
    if (set(request.GET) - {"afterSequence", "limit"}
            or any(len(request.GET.getlist(field)) != 1 for field in request.GET)
            or afterSequence < 0 or not 1 <= limit <= 100):
        return Status(400, {"error": "agent_inputs_cursor_invalid"})
    try:
        require_native_agent_delegation(request, "sessions:read", agent_id=agent_id)
        return input_history(request.user, agent_id, afterSequence, limit)
    except AgentInputError as error:
        return Status(error.status, {"error": error.code})
