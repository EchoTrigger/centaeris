"""Validate the Runtime's presentation receipts without interpreting Agent state."""
import json
import urllib.error
from typing import Literal

from django.conf import settings
from pydantic import BaseModel, ConfigDict, Field, ValidationError

from .runtime_http import request_json


class _Receipt(BaseModel):
    model_config = ConfigDict(extra="forbid", strict=True)


class _Agent(_Receipt):
    runtimeJobId: str
    callId: str
    childSessionId: str
    parentAgentRunId: str
    parentTurnId: str
    description: str
    prompt: str
    status: Literal[
        "queued", "leased", "running", "succeeded", "failed", "dead_lettered", "cancelled"
    ]
    createdAtMs: int
    updatedAtMs: int


class _ToolCall(_Receipt):
    id: str
    name: str
    argsJson: str


class _Message(_Receipt):
    messageId: str
    role: Literal["system", "user", "assistant", "tool"]
    content: str
    createdAtMs: int
    reasoningContent: str | None
    toolCalls: list[_ToolCall]
    toolCallId: str | None
    toolName: str | None
    resultState: str | None


class _Subagents(_Receipt):
    schema_version: Literal["workspace.session.subagents.v1"] = Field(alias="schema")
    sessionId: str
    agents: list[_Agent]


class _Subagent(_Receipt):
    schema_version: Literal["workspace.session.subagent.v1"] = Field(alias="schema")
    sessionId: str
    agent: _Agent
    messages: list[_Message]


def read_session_subagents(session, runtime_job_id: str | None = None) -> dict:
    detail = runtime_job_id is not None
    body = {
        "schema": "runtime.session.subagent.read.v1" if detail else "runtime.session.subagents.read.v1",
        "sessionId": session.id,
        "workspaceId": session.workspace_id,
        "ownerId": session.owner_id,
    }
    if detail:
        body["runtimeJobId"] = runtime_job_id
    try:
        result = request_json(
            "/internal/sessions/subagent" if detail else "/internal/sessions/subagents",
            body,
            timeout=settings.RUNTIME_CONTROL_TIMEOUT_SECONDS,
        )
    except urllib.error.HTTPError as error:
        code = error.code
        error.close()
        if code == 404:
            raise LookupError("session_subagent_not_found") from error
        raise RuntimeError("session_subagent_unavailable") from error
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError("session_subagent_unavailable") from error
    try:
        receipt = (_Subagent if detail else _Subagents).model_validate(result)
    except ValidationError as error:
        raise RuntimeError("session_subagent_response_invalid") from error
    if receipt.sessionId != session.id or (detail and receipt.agent.runtimeJobId != runtime_job_id):
        raise RuntimeError("session_subagent_response_scope_invalid")
    return result
