"""Wire shapes for the external Agent Run API and its bundled reference."""

import json
from dataclasses import dataclass
from typing import Annotated, Literal, NotRequired, TypedDict

from pydantic import Field, WithJsonSchema

from .http.schema import StrictSchema


CREATE_RUN_PATH = "/v1/agent-runs"
GET_OPERATION_PATH = "/v1/agent-runs/operations/{operation_id}"
CANCEL_RUN_PATH = "/v1/agent-runs/{run_id}/cancel"
IDEMPOTENCY_HEADER = "Idempotency-Key"

TerminalRunStatus = Literal["completed", "failed", "cancelled"]
RunStatus = Literal["queued", "running", TerminalRunStatus]
DateTimeText = Annotated[str, WithJsonSchema({"type": "string", "format": "date-time"})]
Base64Text = Annotated[str, WithJsonSchema({"type": "string", "contentEncoding": "base64"})]


class CreateRunRequest(StrictSchema):
    prompt: str = Field(min_length=1, max_length=131072)
    source_object_refs: list[str] = Field(default_factory=list, max_length=64)


class OperationReceipt(TypedDict):
    operationId: str
    runId: str
    acceptedAt: DateTimeText
    deadlineAt: DateTimeText
    expiresAt: DateTimeText
    runStatus: RunStatus
    cleanupStatus: Literal["pending", "cleaning", "completed", "failed"]
    reasonCode: NotRequired[str]


class CancellationRequested(TypedDict):
    runId: str
    cancelStatus: Literal["requested"]


class RunProgress(TypedDict):
    runId: str
    stage: Literal["queued", "running", "delivering"]


class OutputText(TypedDict):
    runId: str
    text: str


class ArtifactStarted(TypedDict):
    artifactId: str
    filename: str
    contentType: str
    sizeBytes: Annotated[int, Field(ge=0)]
    sha256: str


class ArtifactChunk(TypedDict):
    artifactId: str
    index: Annotated[int, Field(ge=0)]
    data: Base64Text


class ArtifactCompleted(ArtifactStarted):
    chunkCount: Annotated[int, Field(ge=0)]


class ArtifactFailed(TypedDict):
    artifactId: str
    error: Literal["artifact_delivery_failed"]


class RunFinished(TypedDict):
    runId: str
    runStatus: TerminalRunStatus
    deliveryStatus: Literal["completed", "failed"]
    reasonCode: NotRequired[str]


@dataclass(frozen=True)
class Event[Payload]:
    name: str
    schema: type[Payload]

    def frame(self, payload: Payload) -> str:
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        return f"event: {self.name}\ndata: {body}\n\n"


RUN_ACCEPTED = Event("runAccepted", OperationReceipt)
RUN_PROGRESS = Event("runProgress", RunProgress)
OUTPUT_TEXT = Event("outputText", OutputText)
ARTIFACT_STARTED = Event("artifactStarted", ArtifactStarted)
ARTIFACT_CHUNK = Event("artifactChunk", ArtifactChunk)
ARTIFACT_COMPLETED = Event("artifactCompleted", ArtifactCompleted)
ARTIFACT_FAILED = Event("artifactFailed", ArtifactFailed)
RUN_FINISHED = Event("runFinished", RunFinished)

EVENTS = (
    RUN_ACCEPTED, RUN_PROGRESS, OUTPUT_TEXT, ARTIFACT_STARTED, ARTIFACT_CHUNK,
    ARTIFACT_COMPLETED, ARTIFACT_FAILED, RUN_FINISHED,
)
