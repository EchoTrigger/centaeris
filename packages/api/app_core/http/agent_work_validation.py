from typing import Literal

from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.agent_work_validation import AgentWorkRejected, validate_agent_work, validate_work_args
from app_core.agent_work_query import WorkQueryError, query_work_request
from app_core.runtime_contract import require_opaque_ref, require_sha256

from .response_schema import COMMON_ERROR_RESPONSES, HostedOperationResponse
from .schema import StrictSchema
from .security import internal_token_auth


router = Router(tags=["internal-agent-work"], by_alias=True)


class ValidateAgentWorkRequest(StrictSchema):
    schema_id: Literal["workspace.agent_work.validate.v1"] = Field(alias="schema")
    agent_run_id: str = Field(alias="agentRunId")
    authorization_digest: str = Field(alias="authorizationDigest")
    coordination_session_id: str = Field(alias="coordinationSessionId")
    tool_call_id: str = Field(alias="toolCallId")
    objective: str
    session_refs: list[str] = Field(alias="sessionRefs", max_length=8)
    file_refs: list[str] = Field(alias="fileRefs", max_length=8)

    @field_validator("agent_run_id", "coordination_session_id", "tool_call_id")
    @classmethod
    def identity(cls, value):
        require_opaque_ref("agent_work_identity", value)
        return value

    @field_validator("authorization_digest")
    @classmethod
    def digest(cls, value):
        require_sha256("authorizationDigest", value)
        return value

    @field_validator("objective")
    @classmethod
    def text(cls, value):
        validate_work_args({"objective": value, "session_refs": [], "file_refs": []})
        return value

    @field_validator("session_refs", "file_refs")
    @classmethod
    def refs(cls, value):
        for ref in value:
            require_opaque_ref("resource_ref", ref)
        return value


class ValidatedAgentWorkResponse(StrictSchema):
    schema_id: Literal["workspace.agent_work.validated.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    agent_run_id: str = Field(alias="agentRunId")
    tool_call_id: str = Field(alias="toolCallId")
    authorization_digest: str = Field(alias="authorizationDigest")
    input_digest: str = Field(alias="inputDigest")


@router.post("/agent-work/validate", auth=internal_token_auth,
             response={200: ValidatedAgentWorkResponse} | COMMON_ERROR_RESPONSES, include_in_schema=False)
def validate_work(request, payload: ValidateAgentWorkRequest):
    try:
        return validate_agent_work(payload.model_dump(by_alias=True))
    except AgentWorkRejected:
        return Status(403, {"error": "agent_work_authority_rejected"})


class QueryAgentWorkRequest(StrictSchema):
    schema_id: Literal["workspace.agent_work.query.v1"] = Field(alias="schema")
    agent_run_id: str = Field(alias="agentRunId", max_length=160)
    authorization_digest: str = Field(alias="authorizationDigest")
    coordination_session_id: str = Field(alias="coordinationSessionId", max_length=160)
    tool_call_id: str = Field(alias="toolCallId", max_length=160)
    source_agent_run_id: str = Field(alias="sourceAgentRunId", max_length=160)
    source_tool_call_id: str = Field(alias="sourceToolCallId", max_length=160)

    @field_validator("agent_run_id", "coordination_session_id", "tool_call_id", "source_agent_run_id", "source_tool_call_id")
    @classmethod
    def identity(cls, value):
        require_opaque_ref("agent_work_identity", value)
        return value

    @field_validator("authorization_digest")
    @classmethod
    def digest(cls, value):
        require_sha256("authorizationDigest", value)
        return value


class WorkRequestSourceResponse(StrictSchema):
    source_agent_run_id: str = Field(alias="sourceAgentRunId")
    source_turn_id: str = Field(alias="sourceTurnId")
    source_tool_call_id: str = Field(alias="sourceToolCallId")
    source_event_id: str | None = Field(alias="sourceEventId")
    projects_to_agent_run_stream: bool | None = Field(alias="projectsToAgentRunStream")


class WorkReturnAttemptResponse(StrictSchema):
    attempt_id: str = Field(alias="attemptId")
    agent_run_id: str | None = Field(alias="agentRunId")


class WorkReturnConfirmationSourceResponse(StrictSchema):
    event_id: str = Field(alias="eventId")
    call_event_id: str = Field(alias="callEventId")
    sequence: int
    agent_run_id: str = Field(alias="agentRunId")
    turn_id: str = Field(alias="turnId")
    tool_call_id: str = Field(alias="toolCallId")


class WorkReturnViewResponse(StrictSchema):
    notice: dict
    attempt: WorkReturnAttemptResponse | None
    handled: bool
    confirmation: WorkReturnConfirmationSourceResponse | None


class WorkRequestResourceResponse(StrictSchema):
    available: bool
    agent_run_status: str | None = Field(alias="agentRunStatus")
    returns: list[WorkReturnViewResponse]
    output: "WorkOutputResponse | None"


class WorkOutputResponse(StrictSchema):
    schema_id: Literal["workspace.agent_work.output.v1"] = Field(alias="schema")
    source: Literal["untrustedWorkOutput"]
    session_id: str = Field(alias="sessionId")
    agent_run_id: str = Field(alias="agentRunId")
    event_id: str = Field(alias="eventId")
    through_sequence: int = Field(alias="throughSequence")
    body: str


class WorkRequestQueryResponse(StrictSchema):
    schema_id: Literal["workspace.agent_work.query_result.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    agent_run_id: str = Field(alias="agentRunId")
    tool_call_id: str = Field(alias="toolCallId")
    authorization_digest: str = Field(alias="authorizationDigest")
    source_agent_run_id: str = Field(alias="sourceAgentRunId")
    source_tool_call_id: str = Field(alias="sourceToolCallId")
    status: Literal["notRecorded", "pending", "admitted"]
    source: WorkRequestSourceResponse | None
    operation: HostedOperationResponse | None
    work: WorkRequestResourceResponse | None


@router.post("/agent-work/query", auth=internal_token_auth,
             response={200: WorkRequestQueryResponse} | COMMON_ERROR_RESPONSES, include_in_schema=False)
def query_work(request, payload: QueryAgentWorkRequest):
    try:
        return query_work_request(payload.model_dump(by_alias=True))
    except WorkQueryError as error:
        return Status(error.status, {"error": error.code})


class ValidateWorkReturnConfirmationRequest(StrictSchema):
    schema_id: Literal["workspace.agent_work.return_confirm.validate.v1"] = Field(alias="schema")
    agent_run_id: str = Field(alias="agentRunId", max_length=160)
    authorization_digest: str = Field(alias="authorizationDigest")
    coordination_session_id: str = Field(alias="coordinationSessionId", max_length=160)
    tool_call_id: str = Field(alias="toolCallId", max_length=160)
    notice_id: str = Field(alias="noticeId", max_length=160)
    attempt_id: str = Field(alias="attemptId", max_length=160)

    @field_validator("agent_run_id", "coordination_session_id", "tool_call_id", "notice_id", "attempt_id")
    @classmethod
    def identity(cls, value):
        require_opaque_ref("agent_work_confirmation_identity", value)
        return value

    @field_validator("authorization_digest")
    @classmethod
    def digest(cls, value):
        require_sha256("authorizationDigest", value)
        return value


class ValidatedWorkReturnConfirmationResponse(StrictSchema):
    schema_id: Literal["workspace.agent_work.return_confirm.validated.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    agent_run_id: str = Field(alias="agentRunId")
    tool_call_id: str = Field(alias="toolCallId")
    authorization_digest: str = Field(alias="authorizationDigest")
    input_digest: str = Field(alias="inputDigest")
    notice_id: str = Field(alias="noticeId")
    attempt_id: str = Field(alias="attemptId")
    notice_identity: dict = Field(alias="noticeIdentity")


@router.post("/agent-work/returns/confirm/validate", auth=internal_token_auth,
             response={200: ValidatedWorkReturnConfirmationResponse} | COMMON_ERROR_RESPONSES, include_in_schema=False)
def validate_return_confirmation(request, payload: ValidateWorkReturnConfirmationRequest):
    from app_core.agent_work_confirmation import validate_work_return_confirmation
    from app_core.agent_work_returns import WorkReturnError
    try:
        return validate_work_return_confirmation(payload.model_dump(by_alias=True))
    except WorkReturnError as error:
        return Status(error.status, {"error": error.code})
