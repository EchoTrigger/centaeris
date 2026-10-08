import hashlib
import asyncio
import io
import json
import shutil
import tempfile
import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
from pathlib import Path
from unittest.mock import AsyncMock, Mock, patch

import httpx
from asgiref.sync import async_to_sync
from openai import AuthenticationError, InternalServerError, OpenAI, RateLimitError
from django.conf import settings
from django.contrib.auth.models import User
from django.core.files.base import ContentFile
from django.core.files.storage import FileSystemStorage, default_storage
from django.core.files.uploadedfile import SimpleUploadedFile
from django.db import connection, transaction
from django.core.management import call_command
from django.core.management.base import CommandError
from django.test import TestCase, TransactionTestCase, override_settings
from django.utils import timezone

from .assets import (
    DeferredInputResolutionError,
    MAX_DIRECT_INPUT_BYTES,
    captured_input_fields,
    source_object_is_granted,
    tombstone_stored_object,
)
from .http.serialization import serialize_artifact as serializeArtifact
from .model_adapter import (
    ModelProviderError,
    encrypt_credential_secret,
    fake_model_response,
    stream_model_async,
)
from .model_adapter.openai_completions import (
    build_open_ai_completions_request,
    parse_open_ai_completions_response,
    stream_open_ai_completions,
)
from .model_adapter.quota import async_model_attempt, model_attempt
from .model_adapter.anthropic_messages import (
    build_anthropic_messages_request,
    parse_anthropic_message,
    stream_anthropic_messages,
)
from .model_adapter.openai_responses import (
    build_open_ai_responses_request,
    parse_open_ai_responses_response,
    stream_open_ai_responses,
)
from .models import (
    Agent,
    Artifact,
    ArtifactPublication,
    Session,
    SessionProject,
    DerivedRepresentation,
    DerivedResource,
    KnowledgeSegment,
    AgentRunAuthorization,
    ModelConfig,
    ModelQuotaDomain,
    ModelProvider,
    ModelRunLog,
    McpBearerCredential,
    McpCredentialAuditEvent,
    ProviderCredential,
    SessionAssetLink,
    SessionCitationProjection,
    SessionEvent,
    AgentRun,
    Source,
    SourceGrant,
    SourceObject,
    UserLibraryObject,
    UserLibraryLink,
    Workspace,
    WorkspaceGroup,
    WorkspaceMembership,
    WorkspacePluginEnablement,
)
from .material_identity import (
    validate_processing_specification,
    processing_spec_digest,
    representation_id,
)
from .material_contract import KnowledgeError
from .plugin_catalog import (
    activation_digest,
    load_plugin_catalog,
    validate_plugin_activation,
)
from .runtime_contract import (
    MODEL_RUN_SCHEMA,
    authorization_digest,
    authorization_signature,
    validate_agent_run_authorization_payload,
)
from .deferred_input import resolve_deferred_input
from .agent_run_authorization_factory import (
    create_agent_run_authorization as create_agent_run_authorization_with_image,
)
from .runtime_client import TranscriptRuntimeError, build_agent_run_start
from .session_event import (
    project_committed_agent_run,
    rebuild_agent_run_citation_projection,
)
from .testing import create_session
from . import agent_run_stream


TEST_EXECUTION_IMAGE_DIGEST = f"sha256:{'a' * 64}"
TEST_EXECUTION_PROFILE = {
    "schema": "runtime.execution_profile.v1",
    "imageCapability": "workspace_general_v1",
    "imageDigest": TEST_EXECUTION_IMAGE_DIGEST,
}
_execution_profile_patcher = None


def setUpModule():
    global _execution_profile_patcher
    _execution_profile_patcher = patch(
        "app_core.http.workspaces.request_execution_profile",
        return_value=TEST_EXECUTION_PROFILE,
    )
    _execution_profile_patcher.start()


def tearDownModule():
    if _execution_profile_patcher is not None:
        _execution_profile_patcher.stop()


def create_agent_run_authorization(agent_run, message_asset_refs=None):
    return create_agent_run_authorization_with_image(
        agent_run,
        message_asset_refs=message_asset_refs,
        image_digest=TEST_EXECUTION_IMAGE_DIGEST,
    )


def streaming_response_bytes(response) -> bytes:
    content = response.streaming_content
    if hasattr(content, "__aiter__"):

        async def consume() -> bytes:
            return b"".join([chunk async for chunk in content])

        return async_to_sync(consume)()
    return b"".join(content)


def async_iterator_bytes(iterator) -> bytes:
    async def consume() -> bytes:
        return b"".join([chunk async for chunk in iterator])

    return async_to_sync(consume)()


async def async_values(values):
    for value in values:
        yield value


def prepared_prompt_for_test(
    model: ModelConfig,
    messages: list[dict] | None = None,
    toolDefinitions: list[dict] | None = None,
    toolChoice: dict | None = None,
) -> dict:
    return {
        "schema": "prepared_prompt.v1",
        "messages": messages
        if messages is not None
        else [{"messageId": "msg-user", "role": "user", "content": "health check"}],
        "toolDefinitions": toolDefinitions if toolDefinitions is not None else [],
        "toolChoice": toolChoice if toolChoice is not None else {"type": "none"},
        "maxOutputTokens": model.maxOutputTokens,
    }


def session_record(
    agent_run, sequence: int, eventType: str, payload: dict, turnId: str | None = None
) -> dict:
    return {
        "sequence": sequence,
        "event": {
            "schemaVersion": "session.event.v1",
            "eventVersion": 1,
            "sequence": sequence,
            "type": eventType,
            "eventId": f"event:{agent_run.id}:{sequence}",
            "sessionId": agent_run.session_id,
            "turnId": turnId or agent_run.turn_id,
            "agentRunId": agent_run.id,
            "createdAtMs": sequence,
            "payload": payload,
        },
    }


def append_started(agent_run, attachments: list[dict] | None = None) -> None:
    append_session_records(
        agent_run,
        [
            session_record(agent_run, 1, "agent_run_started", {"userObjective": agent_run.prompt}),
            session_record(
                agent_run,
                2,
                "user_message",
                {
                    "messageId": f"message:{agent_run.turn_id}:user",
                    "text": agent_run.prompt,
                    "attachments": attachments or [],
                },
            ),
        ],
    )


def append_completed(agent_run, text: str = "done") -> None:
    append_session_records(
        agent_run,
        [
            session_record(
                agent_run,
                3,
                "assistant_message",
                {
                    "messageId": f"message:{agent_run.turn_id}:assistant",
                    "modelMarkdown": text,
                    "artifactRefs": [],
                    "status": "done",
                },
            ),
            session_record(agent_run, 4, "agent_run_completed", {"doneReason": "finalized"}),
        ],
    )
    project_committed_agent_run(agent_run, "completed")


def append_session_records(agent_run, items) -> None:
    for item in items:
        event = item["event"]
        existing = SessionEvent.objects.filter(eventId=event["eventId"]).first()
        if existing is not None:
            if existing.payload != event:
                raise ValueError("session record fixture conflict")
            continue
        latest_sequence = (
            SessionEvent.objects.filter(session=agent_run.session)
            .order_by("-sequence")
            .values_list("sequence", flat=True)
            .first()
            or 0
        )
        SessionEvent.objects.create(
            eventId=event["eventId"],
            workspace=agent_run.workspace,
            session=agent_run.session,
            agent_run=agent_run,
            sequence=latest_sequence + 1,
            agent_run_sequence=item["sequence"],
            projects_to_agent_run_stream=event["type"] not in {
                "session_meta",
                "model_request_started",
                "provider_usage",
                "checkpoint_ref",
                "file_fact",
            },
            payload={**event, "sequence": latest_sequence + 1},
            createdAtMs=event["createdAtMs"],
        )

@override_settings(INTERNAL_API_TOKEN="test-internal-token")
class ApiVerticalSliceTests(TransactionTestCase):
    serialized_rollback = True
    def test_me_returns_json_unauthorized_instead_of_redirecting_to_django_login(self):
        response = self.client.get("/api/me")

        self.assertEqual(response.status_code, 401)
        self.assertEqual(response.json(), {"error": "authentication_required"})
        self.assertNotIn("Location", response)

    def test_cors_allows_configured_web_origin(self):
        response = self.client.options(
            "/api/sessions/session_1/agent-runs/agent_run_1/events",
            HTTP_ORIGIN="http://localhost:3000",
            HTTP_ACCESS_CONTROL_REQUEST_METHOD="GET",
            HTTP_ACCESS_CONTROL_REQUEST_HEADERS="last-event-id",
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response["Access-Control-Allow-Origin"], "http://localhost:3000"
        )
        self.assertEqual(response["Access-Control-Allow-Credentials"], "true")
        self.assertEqual(
            response["Access-Control-Allow-Headers"],
            "Content-Type, Last-Event-ID, X-CSRFToken",
        )
        self.assertEqual(
            [
                item.strip()
                for item in response["Access-Control-Allow-Methods"].split(",")
            ],
            ["DELETE", "GET", "PATCH", "POST", "PUT", "OPTIONS"],
        )

    def test_new_agent_run_fails_before_database_writes_when_runtime_profile_is_unavailable(self):
        user = User.objects.create_user(
            username="profile-unavailable@example.test", password="password"
        )
        workspace = Workspace.objects.create(name="Profile unavailable", createdBy=user)
        workspace.members.add(user)
        agent = Agent.objects.create(
            workspace=workspace,
            owner=user,
            name="Centaeris",
        )
        model = ModelConfig.objects.create(
            displayName="Profile model",
            modelName="profile-model",
        )
        self.client.force_login(user)

        with patch(
            "app_core.http.workspaces.request_execution_profile",
            side_effect=RuntimeError("runtime_execution_profile_request_failed"),
        ):
            response = self.client.post(
                f"/api/workspaces/{workspace.id}/sessions/new/messages",
                data=json.dumps(
                    {
                        "operationId": "test-operation-338",
                        "text": "hello",
                        "agentId": agent.id,
                        "modelConfigRef": model.id,
                    }
                ),
                content_type="application/json",
            )

        self.assertEqual(response.status_code, 503)
        self.assertEqual(
            response.json(), {"error": "runtime_execution_profile_unavailable"}
        )
        self.assertFalse(Session.objects.filter(workspace=workspace).exists())
        self.assertFalse(AgentRun.objects.filter(workspace=workspace).exists())

    def test_user_can_login_start_session_send_message_and_read_sse_final(self):
        user = User.objects.create_user(
            username="admin@example.com",
            email="admin@example.com",
            password="password",
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        Agent.objects.create(
            id="centaeris",
            workspace=workspace,
            owner=user,
            name="Centaeris",
        )
        model = ModelConfig.objects.create(
            displayName="Fake Model",
            modelName="fake-model",
        )

        loginResponse = self.client.post(
            "/api/login",
            data=json.dumps({"email": "admin@example.com", "password": "password"}),
            content_type="application/json",
        )
        self.assertEqual(loginResponse.status_code, 200)

        workspaceResponse = self.client.get("/api/workspaces")
        self.assertEqual(workspaceResponse.status_code, 200)
        self.assertEqual(workspaceResponse.json()["workspaces"][0]["id"], workspace.id)

        modelsResponse = self.client.get("/api/models")
        self.assertEqual(modelsResponse.status_code, 200)
        self.assertEqual(modelsResponse.json()["models"][0]["id"], model.id)

        def schedule_agent_run_lifecycle(agent_run):
            append_started(agent_run)
            append_completed(agent_run, "这是最小纵切响应。")
            return "inserted"

        with patch(
            "app_core.http.workspaces.schedule_agent_run_lifecycle",
            side_effect=schedule_agent_run_lifecycle,
        ) as execute:
            messageResponse = self.client.post(
                f"/api/workspaces/{workspace.id}/sessions/new/messages",
                data=json.dumps({
                    "operationId": "test-operation-400",
                    "text": "帮我看牙科 SOP",
                    "agentId": "centaeris",
                    "modelConfigRef": model.id,
                }),
                content_type="application/json",
            )
        self.assertEqual(messageResponse.status_code, 202)
        message = messageResponse.json()
        sessionId = message["sessionId"]
        agentRunId = message["agentRunId"]
        self.assertEqual(message["turnId"], AgentRun.objects.get(id=agentRunId).turn_id)
        self.assertNotEqual(message["turnId"], agentRunId)
        self.assertEqual(message["status"], "accepted")
        self.assertNotIn("authorizationRef", message)
        self.assertEqual(
            list(
                SessionEvent.objects.filter(agent_run_id=agentRunId)
                .order_by("sequence")
                .values_list("payload__type", flat=True)
            ),
            ["agent_run_started", "user_message", "assistant_message", "agent_run_completed"],
        )
        self.assertEqual(execute.call_count, 1)
        eventsResponse = self.client.get(
            f"/api/sessions/{sessionId}/agent-runs/{agentRunId}/events"
        )
        repeatedEventsResponse = self.client.get(
            f"/api/sessions/{sessionId}/agent-runs/{agentRunId}/events"
        )
        self.assertEqual(eventsResponse.status_code, 200)
        stream = streaming_response_bytes(eventsResponse).decode("utf-8")
        repeatedStream = streaming_response_bytes(repeatedEventsResponse).decode(
            "utf-8"
        )
        self.assertIn('"schema":"session.stream.item.v1"', stream)
        self.assertIn('"type":"assistant_message"', stream)
        self.assertIn("这是最小纵切响应", stream)
        self.assertEqual(stream, repeatedStream)
        self.assertEqual(execute.call_count, 1)

    def test_history_cursor_resumes_postgres_commits_without_redis_replay(self):
        user = User.objects.create_user(
            username="pg-cursor@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="PG cursor", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="pg-cursor-model", displayName="PG cursor model"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="resume from Postgres",
            status="running",
        )
        create_agent_run_authorization(agent_run)
        append_started(agent_run)
        self.client.force_login(user)

        active = self.client.get(
            f"/api/sessions/{session.id}/transcript/active-agent-run",
            {"sourceHighWater": "2"},
        )
        self.assertEqual(active.status_code, 200, active.content)
        cursor = active.json()["agentRun"]["streamCursor"]
        self.assertEqual(
            agent_run_stream.parse_last_event_cursor(cursor, agent_run.id), 2
        )

        append_session_records(
            agent_run,
            [
                session_record(
                    agent_run,
                    3,
                    "phase_event",
                    {"stage": "model_process_summary", "message": "committed later"},
                ),
                session_record(
                    agent_run,
                    4,
                    "agent_run_completed",
                    {"doneReason": "finalized"},
                ),
            ],
        )
        response = self.client.get(
            f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/events",
            HTTP_LAST_EVENT_ID=cursor,
        )
        stream = streaming_response_bytes(response).decode("utf-8")

        self.assertEqual(response.status_code, 200)
        self.assertNotIn('"sourceSequence":1', stream)
        self.assertNotIn('"sourceSequence":2', stream)
        self.assertIn('"sourceSequence":3', stream)
        self.assertIn('"sourceSequence":4', stream)
        self.assertNotIn("id: 3-0", stream)
        legacy_cursor = self.client.get(
            f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/events",
            HTTP_LAST_EVENT_ID="123-0",
        )
        self.assertEqual(legacy_cursor.status_code, 400)
        self.assertEqual(legacy_cursor.json(), {"error": "last_event_id_invalid"})
        future = agent_run_stream.encode_stream_cursor(agent_run.id, 5)
        self.assertEqual(
            self.client.get(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/events",
                HTTP_LAST_EVENT_ID=future,
            ).status_code,
            400,
        )

    def test_model_request_storage_refs_never_reach_sse(self):
        user = User.objects.create_user(
            username="model-request-ref@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Model request ref", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="model-request-ref-model", displayName="Model request ref"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hide storage refs",
            status="running",
        )
        append_started(agent_run)
        append_session_records(
            agent_run,
            [
                session_record(
                    agent_run,
                    3,
                    "model_request_started",
                    {
                        "requestId": "request-secret",
                        "observations": {
                            "manifestDigest": "sha256:" + "a" * 64,
                        },
                    },
                ),
                session_record(
                    agent_run,
                    4,
                    "assistant_message",
                    {
                        "messageId": f"message:{agent_run.turn_id}:assistant",
                        "modelMarkdown": "done",
                        "artifactRefs": [],
                        "status": "done",
                    },
                ),
                session_record(
                    agent_run,
                    5,
                    "agent_run_completed",
                    {"doneReason": "finalized"},
                ),
            ],
        )
        project_committed_agent_run(agent_run, "completed")
        self.client.force_login(user)

        stream_response = self.client.get(
            f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/events"
        )
        stream = streaming_response_bytes(stream_response).decode("utf-8")

        self.assertEqual(stream_response.status_code, 200)
        self.assertNotIn("request-secret", stream)
        self.assertNotIn("manifestDigest", stream)
        self.assertNotIn("contentDigest", stream)
        self.assertIn('"type":"assistant_message"', stream)

    def test_sessions_are_bound_to_an_immutable_agent_identity(self):
        user = User.objects.create_user(
            username="agent-sessions@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Agent sessions", createdBy=user)
        workspace.members.add(user)
        self.client.force_login(user)

        first = create_session(
            workspace=workspace,
            owner=user,
            agent_id="centaeris",
        )
        second = create_session(
            workspace=workspace,
            owner=user,
            agent_id="research-agent",
        )

        response = self.client.get(
            f"/api/workspaces/{workspace.id}/sessions?agentId=centaeris"
        )
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(
            [(item["id"], item["agentId"]) for item in response.json()["sessions"]],
            [(first.id, "centaeris")],
        )
        response = self.client.get(
            f"/api/workspaces/{workspace.id}/sessions?agentId=research-agent"
        )
        self.assertEqual(
            [(item["id"], item["agentId"]) for item in response.json()["sessions"]],
            [(second.id, "research-agent")],
        )

        first.agent_id = "research-agent"
        with self.assertRaisesRegex(ValueError, "agent_id is immutable"):
            first.save()

        create_session(
            workspace=workspace,
            owner=user,
            agent_id="猫" * 64,
        )
        with self.assertRaisesRegex(ValueError, "agent_id_invalid"):
            create_session(
                workspace=workspace,
                owner=user,
                agent_id="猫" * 65,
            )

    def test_session_message_requires_or_matches_agent_identity(self):
        user = User.objects.create_user(
            username="agent-message@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Agent message", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="agent-message-model", displayName="Fake")
        session = create_session(
            workspace=workspace,
            owner=user,
            agent_id="centaeris",
        )
        self.client.force_login(user)

        missing = self.client.post(
            f"/api/workspaces/{workspace.id}/sessions/new/messages",
            data=json.dumps({"operationId": "test-operation-650", "text": "hello", "modelConfigRef": model.id}),
            content_type="application/json",
        )
        self.assertEqual(missing.status_code, 400, missing.content)
        self.assertEqual(missing.json(), {"error": "agent_id_required"})

        mismatch = self.client.post(
            f"/api/workspaces/{workspace.id}/sessions/{session.id}/messages",
            data=json.dumps(
                {
                    "operationId": "test-operation-658",
                    "text": "hello",
                    "agentId": "research-agent",
                    "modelConfigRef": model.id,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(mismatch.status_code, 409, mismatch.content)
        self.assertEqual(mismatch.json(), {"error": "session_agent_mismatch"})
        self.assertFalse(session.agent_runs.exists())

    def test_agent_run_lifecycle_scheduling_gap_keeps_agent_run_input_without_forging_session_event(self):
        user = User.objects.create_user(username="schedule-gap@example.com", password="password")
        workspace = Workspace.objects.create(name="Schedule gap", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="schedule-gap-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        session.agent.instructions = "Keep the answer grounded."
        session.agent.save(update_fields=["instructions", "updatedAt"])
        self.client.force_login(user)

        with patch(
            "app_core.http.workspaces.schedule_agent_run_lifecycle",
            side_effect=RuntimeError("runtime_job_schedule_failed"),
        ):
            response = self.client.post(
                f"/api/workspaces/{workspace.id}/sessions/{session.id}/messages",
                data=json.dumps({"operationId": "test-operation-687", "text": "hello", "modelConfigRef": model.id}),
                content_type="application/json",
            )

        self.assertEqual(response.status_code, 202)
        agent_run = AgentRun.objects.get(session=session)
        self.assertEqual(agent_run.prompt, "hello")
        self.assertEqual(agent_run.agent_instructions, "Keep the answer grounded.")
        self.assertEqual(agent_run.transitionReason, "agent_run_lifecycle_schedule_pending")
        self.assertTrue(AgentRunAuthorization.objects.filter(agent_run=agent_run).exists())
        self.assertFalse(SessionEvent.objects.filter(agent_run=agent_run).exists())


    def test_active_transcript_run_projects_committed_terminal_before_worker_transition(self):
        user = User.objects.create_user(
            username="terminal-window@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Terminal window", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="terminal-window-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
            status="running",
        )
        create_agent_run_authorization(agent_run)
        append_started(agent_run)
        append_completed(agent_run, "done")
        self.client.force_login(user)

        active = self.client.get(
            f"/api/sessions/{session.id}/transcript/active-agent-run",
            {"sourceHighWater": "4"},
        )
        events = self.client.get(
            f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/events"
        )

        self.assertEqual(active.status_code, 200, active.content)
        self.assertIsNone(active.json()["agentRun"])
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "completed")
        stream = streaming_response_bytes(events).decode("utf-8")
        for event_type in (
            "agent_run_started",
            "user_message",
            "assistant_message",
            "agent_run_completed",
        ):
            self.assertIn(f'"type":"{event_type}"', stream)

    def test_pre_admission_cancellation_projects_without_session_history(self):
        user = User.objects.create_user(username="pre-cancel@example.com", password="password")
        workspace = Workspace.objects.create(name="Cancellation", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="pre-cancel-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        run = AgentRun.objects.create(workspace=workspace, session=session, user=user,
                                      modelConfig=model, prompt="never admitted", status="running",
                                      transitionReason="execution_queue_expired")
        create_agent_run_authorization(run)
        body = {"schema": "runtime.agent_run.transition.v1", "agentRunId": run.id,
                "state": "cancelled", "transitionReason": "agent_run_cancelled"}
        def transition():
            return self.client.post("/internal/agent-runs/transition", data=json.dumps(body),
                                    content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token")
        self.assertEqual(transition().status_code, 409)  # Caller claims are not evidence.
        committed_at = timezone.now()
        AgentRun.objects.filter(id=run.id).update(preAdmissionCancelledAt=committed_at)
        resolved = self.client.post("/internal/agent-run-lifecycle/resolve",
            data=json.dumps({"schema": "runtime.agent_run_lifecycle.resolve.v1",
                "agentRunId": run.id, "jobId": f"agent_run.lifecycle:{run.id}",
                "authorizationDigest": run.authorization.digest}),
            content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token")
        self.assertEqual(resolved.status_code, 200)
        self.assertEqual(resolved.json()["terminalState"], "cancelled")
        for _ in range(2):
            self.assertEqual(transition().status_code, 200)
        run.refresh_from_db()
        self.assertEqual(run.status, "cancelled")
        self.assertEqual(run.transitionReason, "execution_queue_expired")
        self.assertEqual(run.completedAt, committed_at)
        self.assertFalse(SessionEvent.objects.filter(agent_run=run).exists())
        with patch("app_core.http.internal.get_runtime_job") as get_job:
            response = self.client.post("/internal/agent-run-lifecycle/reconcile",
                data=json.dumps({"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 100}),
                content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token")
        self.assertEqual(response.status_code, 200)
        get_job.assert_not_called()

    def test_agent_run_lifecycle_reconciler_terminates_failed_job_without_deleting_session(self):
        user = User.objects.create_user(
            username="dead-letter@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Dead letter", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="dead-letter-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="survive infrastructure failure",
        )
        create_agent_run_authorization(agent_run)

        with patch(
            "app_core.http.internal.get_runtime_job",
            return_value={
                "jobId": f"agent_run.lifecycle:{agent_run.id}",
                "status": "failed",
            },
        ):
            response = self.client.post(
                "/internal/agent-run-lifecycle/reconcile",
                data=json.dumps(
                    {"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 100}
                ),
                content_type="application/json",
                HTTP_X_INTERNAL_TOKEN="test-internal-token",
            )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {"scheduled": 0, "terminalized": 1, "pending": 0, "activeNext": None, "deadLetterNext": None},
        )
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "failed")
        self.assertEqual(agent_run.transitionReason, "agent_run_lifecycle_dead_lettered")
        self.assertIsNotNone(agent_run.completedAt)
        self.assertTrue(
            Session.objects.filter(id=session.id, status="active").exists()
        )
        self.assertEqual(SessionEvent.objects.filter(agent_run=agent_run).count(), 0)

    def test_agent_run_lifecycle_dead_letter_cannot_reverse_committed_final(self):
        user = User.objects.create_user(username="committed-final@example.com", password="password")
        workspace = Workspace.objects.create(name="Committed final", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="committed-final-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="keep committed final")
        create_agent_run_authorization(agent_run)
        append_started(agent_run)
        append_session_records(agent_run, [
            session_record(agent_run, 3, "assistant_message", {"messageId": f"message:{agent_run.turn_id}:assistant", "modelMarkdown": "committed", "artifactRefs": [], "status": "done"}),
            session_record(agent_run, 4, "agent_run_completed", {"doneReason": "finalized"}),
        ])

        with patch("app_core.http.internal.get_runtime_job", return_value={"jobId": f"agent_run.lifecycle:{agent_run.id}", "status": "dead_lettered"}):
            response = self.client.post(
                "/internal/agent-run-lifecycle/reconcile",
                data=json.dumps({"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 100}),
                content_type="application/json",
                HTTP_X_INTERNAL_TOKEN="test-internal-token",
            )

        self.assertEqual(response.status_code, 200)
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "completed")
        assistant = SessionEvent.objects.get(agent_run=agent_run, payload__type="assistant_message")
        self.assertEqual(assistant.payload["payload"]["modelMarkdown"], "committed")

    def test_agent_run_lifecycle_reconciler_repairs_late_session_final_after_dead_letter(self):
        user = User.objects.create_user(username="late-final@example.com", password="password")
        workspace = Workspace.objects.create(name="Late final", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="late-final-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="preserve late final",
            status="failed",
            transitionReason="agent_run_lifecycle_dead_lettered",
        )
        create_agent_run_authorization(agent_run)
        append_started(agent_run)
        append_session_records(agent_run, [
            session_record(agent_run, 3, "assistant_message", {"messageId": f"message:{agent_run.turn_id}:assistant", "modelMarkdown": "late but committed", "artifactRefs": [], "status": "done"}),
            session_record(agent_run, 4, "agent_run_completed", {"doneReason": "finalized"}),
        ])

        response = self.client.post(
            "/internal/agent-run-lifecycle/reconcile",
            data=json.dumps({"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 100}),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"scheduled": 0, "terminalized": 1, "pending": 0, "activeNext": None, "deadLetterNext": None})
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "completed")
        self.assertEqual(agent_run.transitionReason, "runtime_session_terminal_committed")
        assistant = SessionEvent.objects.get(agent_run=agent_run, payload__type="assistant_message")
        self.assertEqual(assistant.payload["payload"]["modelMarkdown"], "late but committed")



    def test_agent_run_lifecycle_scan_rejects_invalid_cursors(self):
        for cursor in ({}, {"createdAt": "yesterday", "id": "run"},
                       {"createdAt": "2026-09-06T00:00:00", "id": "run"},
                       {"createdAt": "2026-09-06T00:00:00Z", "id": ""},
                       {"createdAt": "2026-09-06T00:00:00Z", "id": "run", "extra": 1}):
            for field in ("activeAfter", "deadLetterAfter"):
                with self.subTest(cursor=cursor, field=field):
                    response = self.client.post("/internal/agent-run-lifecycle/reconcile",
                        data=json.dumps({"schema": "runtime.agent_run_lifecycle.reconcile.v1",
                                         "limit": 2, field: cursor}),
                        content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token")
                    self.assertEqual(response.status_code, 400)

    def test_agent_run_lifecycle_scan_advances_past_long_running_first_page(self):
        user = User.objects.create_user(username="scan-pages@example.com")
        workspace = Workspace.objects.create(name="Scan pages", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="scan-pages-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        runs = []
        for index in range(3):
            run = AgentRun.objects.create(workspace=workspace, session=session,
                user=user, modelConfig=model, prompt=f"waiting {index}")
            create_agent_run_authorization(run)
            runs.append(run)
        # Equal timestamps exercise the identity tie-breaker, not just time paging.
        AgentRun.objects.filter(id__in=[r.id for r in runs]).update(createdAt=runs[0].createdAt)
        cursor = None
        with patch("app_core.http.internal.get_runtime_job", return_value=None), patch(
            "app_core.http.internal.schedule_agent_run_lifecycle"
        ) as schedule:
            for _ in range(2):
                body = {"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 2}
                if cursor is not None:
                    body["activeAfter"] = cursor
                response = self.client.post("/internal/agent-run-lifecycle/reconcile",
                    data=json.dumps(body), content_type="application/json",
                    HTTP_X_INTERNAL_TOKEN="test-internal-token")
                self.assertEqual(response.status_code, 200, response.content)
                cursor = response.json().get("activeNext")
            self.assertEqual({call.args[0].id for call in schedule.call_args_list},
                             {run.id for run in runs})
            self.assertEqual(schedule.call_count, 3)
        self.assertIsNone(cursor)

    def test_agent_run_lifecycle_late_final_scan_advances_after_projection_failures(self):
        user = User.objects.create_user(username="dead-scan-pages@example.com")
        workspace = Workspace.objects.create(name="Dead scan pages", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="dead-scan-pages-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        runs = []
        for index in range(3):
            run = AgentRun.objects.create(workspace=workspace, session=session, user=user,
                modelConfig=model, prompt=f"late {index}", status="failed",
                transitionReason="agent_run_lifecycle_dead_lettered")
            create_agent_run_authorization(run)
            append_started(run)
            append_session_records(run, [
                session_record(run, 3, "agent_run_completed", {"doneReason": "finalized"})
            ])
            runs.append(run)
        cursor = None
        with patch("app_core.http.internal.project_committed_agent_run",
                   side_effect=RuntimeError("projection temporarily unavailable")) as project:
            for _ in range(2):
                response = self.client.post("/internal/agent-run-lifecycle/reconcile",
                    data=json.dumps({"schema": "runtime.agent_run_lifecycle.reconcile.v1",
                                     "limit": 2, "deadLetterAfter": cursor}),
                    content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token")
                self.assertEqual(response.status_code, 200, response.content)
                cursor = response.json()["deadLetterNext"]
            self.assertEqual({call.args[0].id for call in project.call_args_list},
                             {run.id for run in runs})
            self.assertEqual(project.call_count, 3)
        self.assertIsNone(cursor)

    def test_agent_run_lifecycle_late_final_scan_is_independent_from_full_active_limit(self):
        user = User.objects.create_user(
            username="late-final-fairness@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Late final fairness", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="late-final-fairness-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        for index in range(2):
            active = AgentRun.objects.create(
                workspace=workspace,
                session=session,
                user=user,
                modelConfig=model,
                prompt=f"long running {index}",
            )
            create_agent_run_authorization(active)
        late = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="repair me despite a full active page",
            status="failed",
            transitionReason="agent_run_lifecycle_dead_lettered",
        )
        create_agent_run_authorization(late)
        append_started(late)
        append_session_records(
            late,
            [
                session_record(
                    late,
                    3,
                    "assistant_message",
                    {
                        "messageId": f"message:{late.turn_id}:assistant",
                        "modelMarkdown": "late final wins",
                        "artifactRefs": [],
                        "status": "done",
                    },
                ),
                session_record(late, 4, "agent_run_completed", {"doneReason": "finalized"}),
            ],
        )

        with (
            patch("app_core.http.internal.get_runtime_job", return_value=None),
            patch("app_core.http.internal.schedule_agent_run_lifecycle") as schedule,
        ):
            response = self.client.post(
                "/internal/agent-run-lifecycle/reconcile",
                data=json.dumps(
                    {"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 2}
                ),
                content_type="application/json",
                HTTP_X_INTERNAL_TOKEN="test-internal-token",
            )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(), {"scheduled": 2, "terminalized": 1, "pending": 0, "activeNext": None, "deadLetterNext": None}
        )
        self.assertEqual(schedule.call_count, 2)
        late.refresh_from_db()
        self.assertEqual(late.status, "completed")
        self.assertEqual(late.transitionReason, "runtime_session_terminal_committed")

    def test_agent_run_lifecycle_resolve_reports_committed_session_terminal_without_projecting_run(
        self,
    ):
        user = User.objects.create_user(
            username="resolved-terminal@example.com", password="password"
        )
        workspace = Workspace.objects.create(
            name="Materialized terminal", createdBy=user
        )
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="resolved-terminal-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="already committed",
        )
        authorization = create_agent_run_authorization(agent_run)
        append_started(agent_run)
        append_session_records(
            agent_run,
            [
                session_record(
                    agent_run,
                    3,
                    "assistant_message",
                    {
                        "messageId": f"message:{agent_run.turn_id}:assistant",
                        "modelMarkdown": "committed before worker completion",
                        "artifactRefs": [],
                        "status": "done",
                    },
                ),
                session_record(agent_run, 4, "agent_run_completed", {"doneReason": "finalized"}),
            ],
        )

        response = self.client.post(
            "/internal/agent-run-lifecycle/resolve",
            data=json.dumps(
                {
                    "schema": "runtime.agent_run_lifecycle.resolve.v1",
                    "jobId": f"agent_run.lifecycle:{agent_run.id}",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {
                "schema": "runtime.agent_run_lifecycle.resolved.v1",
                "disposition": "terminal",
                "terminalState": "completed",
                "agentRunStart": build_agent_run_start(agent_run),
            },
        )
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "queued")

    def test_session_workspace_internal_cas_uses_lease_fence_and_supports_replay(self):
        user = User.objects.create_user(
            username="session-workspace@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Session workspace", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="session-workspace-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="persist workspace",
            status="running",
        )
        authorization = create_agent_run_authorization(agent_run)
        lease_owner = "worker:session-workspace-test"

        def lease_request(
            schema,
            current_agent_run,
            current_authorization,
            current_lease_owner=lease_owner,
        ):
            return {
                "schema": schema,
                "jobId": f"agent_run.lifecycle:{current_agent_run.id}",
                "leaseOwner": current_lease_owner,
                "agentRunId": current_agent_run.id,
                "authorizationDigest": current_authorization.digest,
            }

        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA IF NOT EXISTS runtime")
            cursor.execute(
                "CREATE TABLE IF NOT EXISTS runtime.runtime_jobs("
                "job_id text PRIMARY KEY, job_kind text NOT NULL, status text NOT NULL, "
                "lease_owner text, lease_expires_at_ms bigint, idempotency_key text NOT NULL, "
                "session_id text, payload_ref text)"
            )
            cursor.execute(
                "CREATE TABLE IF NOT EXISTS runtime.checkpoints("
                "checkpoint_id text PRIMARY KEY, kind text NOT NULL, "
                "session_id text NOT NULL, turn_id text NOT NULL, status text NOT NULL, "
                "done_reason text, updated_at_ms bigint NOT NULL, payload_json text NOT NULL)"
            )

        def start_lease(current_agent_run, current_authorization, current_lease_owner):
            with connection.cursor() as cursor:
                cursor.execute(
                    "INSERT INTO runtime.runtime_jobs("
                    "job_id,job_kind,status,lease_owner,lease_expires_at_ms,"
                    "idempotency_key,session_id,payload_ref) "
                    "VALUES(%s,'agent_run.lifecycle','running',%s,"
                    "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,"
                    "%s,%s,%s) ON CONFLICT(job_id) DO UPDATE SET "
                    "status='running',lease_owner=EXCLUDED.lease_owner,"
                    "lease_expires_at_ms=EXCLUDED.lease_expires_at_ms,"
                    "idempotency_key=EXCLUDED.idempotency_key,"
                    "session_id=EXCLUDED.session_id,"
                    "payload_ref=EXCLUDED.payload_ref",
                    [
                        f"agent_run.lifecycle:{current_agent_run.id}",
                        current_lease_owner,
                        f"agent_run.lifecycle:{current_agent_run.id}:{current_authorization.digest}",
                        current_agent_run.session_id,
                        f"record:agent_run:{current_agent_run.id}",
                    ],
                )

        start_lease(agent_run, authorization, lease_owner)
        resolved = self.client.post(
            "/internal/agent-runs/session-workspace/resolve",
            data=json.dumps(
                lease_request(
                    "runtime.session_workspace.resolve.v1", agent_run, authorization
                )
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(resolved.status_code, 200, resolved.content)
        self.assertEqual(resolved.json()["disposition"], "empty")

        content = b"session workspace snapshot"
        manifest = {
            "schema": "workspace.snapshot.v1",
            "files": [
                {
                    "path": "notes.txt",
                    "sizeBytes": len(content),
                    "sha256": f"sha256:{hashlib.sha256(content).hexdigest()}",
                    "executable": False,
                }
            ],
        }
        manifest_bytes = json.dumps(
            manifest, ensure_ascii=False, separators=(",", ":")
        ).encode()
        snapshot = len(manifest_bytes).to_bytes(4, "big") + manifest_bytes + content
        metadata = lease_request(
            "runtime.session_workspace.commit.v1", agent_run, authorization
        ) | {
            "snapshotSha256": f"sha256:{hashlib.sha256(snapshot).hexdigest()}",
            "snapshotSizeBytes": len(snapshot),
            "expandedSizeBytes": len(content),
            "fileCount": 1,
        }
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        body = len(encoded).to_bytes(4, "big") + encoded + snapshot
        checkpoint_id = "checkpoint:workspace-api-test"
        staged_metadata = lease_request(
            "runtime.execution_workspace.stage.v1", agent_run, authorization
        ) | {
            "checkpointId": checkpoint_id,
            "snapshotSha256": metadata["snapshotSha256"],
            "snapshotSizeBytes": metadata["snapshotSizeBytes"],
            "expandedSizeBytes": metadata["expandedSizeBytes"],
            "fileCount": metadata["fileCount"],
        }
        staged_encoded = json.dumps(staged_metadata, separators=(",", ":")).encode()
        staged = self.client.post(
            "/internal/agent-runs/execution-workspace/stage",
            data=len(staged_encoded).to_bytes(4, "big") + staged_encoded + snapshot,
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(staged.status_code, 201, staged.content)
        staged_snapshot = {
            name: staged.json()[name]
            for name in [
                "objectRef",
                "snapshotSha256",
                "snapshotSizeBytes",
                "expandedSizeBytes",
                "fileCount",
            ]
        }
        checkpoint = {
            "schema": "runtime.recovery_checkpoint.v1",
            "checkpointId": checkpoint_id,
            "sessionId": agent_run.session_id,
            "agentRunId": agent_run.id,
            "executionId": "execution_test",
            "authorizationDigest": authorization.digest,
            "sessionSequence": 1,
            "modelRequestId": "request_test",
            "workspaceSnapshot": staged_snapshot,
            "createdAtMs": 1,
        }
        with connection.cursor() as cursor:
            cursor.execute(
                "INSERT INTO runtime.checkpoints("
                "checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) "
                "VALUES(%s,'recovery',%s,%s,'committed',NULL,1,%s)",
                [
                    checkpoint_id,
                    agent_run.session_id,
                    agent_run.turn_id,
                    json.dumps(checkpoint, separators=(",", ":")),
                ],
            )
        recovered = self.client.post(
            "/internal/agent-runs/execution-workspace/download",
            data=json.dumps(
                lease_request(
                    "runtime.execution_workspace.download.v1", agent_run, authorization
                )
                | {"checkpointId": checkpoint_id}
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(recovered.status_code, 200)
        self.assertEqual(streaming_response_bytes(recovered), snapshot)

        reused_checkpoint_id = "checkpoint:workspace-api-test-reused"
        reused_checkpoint = checkpoint | {
            "checkpointId": reused_checkpoint_id,
            "modelRequestId": "request_test_reused",
            "createdAtMs": 2,
        }
        with connection.cursor() as cursor:
            cursor.execute(
                "INSERT INTO runtime.checkpoints("
                "checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) "
                "VALUES(%s,'recovery',%s,%s,'committed',NULL,2,%s)",
                [
                    reused_checkpoint_id,
                    agent_run.session_id,
                    agent_run.turn_id,
                    json.dumps(reused_checkpoint, separators=(",", ":")),
                ],
            )

        def download_reused_checkpoint():
            return self.client.post(
                "/internal/agent-runs/execution-workspace/download",
                data=json.dumps(
                    lease_request(
                        "runtime.execution_workspace.download.v1", agent_run, authorization
                    )
                    | {"checkpointId": reused_checkpoint_id}
                ),
                content_type="application/json",
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            )

        reused = download_reused_checkpoint()
        self.assertEqual(reused.status_code, 200)
        self.assertEqual(streaming_response_bytes(reused), snapshot)

        original_ref = staged_snapshot["objectRef"]
        invalid_refs = [
            original_ref.replace(
                f"workspaces/{agent_run.workspace_id}/", "workspaces/ws_other/", 1
            ),
            original_ref.replace(
                hashlib.sha256(checkpoint_id.encode("utf-8")).hexdigest(), "g" * 64, 1
            ),
            original_ref.rsplit("/", 1)[0] + f"/{'0' * 64}.snapshot",
        ]
        for invalid_ref in invalid_refs:
            invalid_checkpoint = reused_checkpoint | {
                "workspaceSnapshot": staged_snapshot | {"objectRef": invalid_ref}
            }
            with connection.cursor() as cursor:
                cursor.execute(
                    "UPDATE runtime.checkpoints SET payload_json=%s WHERE checkpoint_id=%s",
                    [
                        json.dumps(invalid_checkpoint, separators=(",", ":")),
                        reused_checkpoint_id,
                    ],
                )
            rejected = download_reused_checkpoint()
            self.assertEqual(rejected.status_code, 400, invalid_ref)

        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE runtime.checkpoints SET payload_json=%s WHERE checkpoint_id=%s",
                [
                    json.dumps(reused_checkpoint, separators=(",", ":")),
                    reused_checkpoint_id,
                ],
            )
        malformed_manifest = manifest | {
            "files": [manifest["files"][0] | {"sha256": f"sha256:{'b' * 64}"}]
        }
        malformed_manifest_bytes = json.dumps(
            malformed_manifest, separators=(",", ":")
        ).encode()
        malformed_snapshot = (
            len(malformed_manifest_bytes).to_bytes(4, "big")
            + malformed_manifest_bytes
            + content
        )
        malformed_metadata = metadata | {
            "snapshotSha256": f"sha256:{hashlib.sha256(malformed_snapshot).hexdigest()}",
            "snapshotSizeBytes": len(malformed_snapshot),
        }
        malformed_encoded = json.dumps(malformed_metadata, separators=(",", ":")).encode()
        malformed = self.client.post(
            "/internal/agent-runs/session-workspace/commit",
            data=(
                len(malformed_encoded).to_bytes(4, "big")
                + malformed_encoded
                + malformed_snapshot
            ),
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(malformed.status_code, 400, malformed.content)
        committed = self.client.post(
            "/internal/agent-runs/session-workspace/commit",
            data=body,
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(committed.status_code, 201, committed.content)
        self.assertEqual(committed.json()["disposition"], "committed")
        self.assertNotIn("storageKey", committed.json())

        advanced = self.client.post(
            "/internal/agent-runs/session-workspace/resolve",
            data=json.dumps(
                lease_request(
                    "runtime.session_workspace.resolve.v1", agent_run, authorization
                )
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(advanced.status_code, 200, advanced.content)
        self.assertEqual(advanced.json()["disposition"], "advanced")

        replay = self.client.post(
            "/internal/agent-runs/session-workspace/commit",
            data=body,
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(replay.json()["disposition"], "idempotent")

        session.refresh_from_db()
        self.assertEqual(session.workspaceGeneration, 1)
        self.assertEqual(session.workspaceLastAdvancedAgentRun_id, agent_run.id)
        self.assertTrue(default_storage.exists(session.workspaceStorageKey))

        next_agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="restore workspace",
            status="running",
        )
        next_authorization = create_agent_run_authorization(next_agent_run)
        replacement_owner = "worker:session-workspace-replacement"
        start_lease(next_agent_run, next_authorization, lease_owner)
        start_lease(next_agent_run, next_authorization, replacement_owner)
        download = self.client.post(
            "/internal/agent-runs/session-workspace/download",
            data=json.dumps(
                lease_request(
                    "runtime.session_workspace.download.v1",
                    next_agent_run,
                    next_authorization,
                    replacement_owner,
                )
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(download.status_code, 200)
        self.assertEqual(download["X-Content-Sha256"], metadata["snapshotSha256"])
        self.assertEqual(streaming_response_bytes(download), snapshot)

        reclaimed = self.client.post(
            "/internal/agent-runs/session-workspace/resolve",
            data=json.dumps(
                lease_request(
                    "runtime.session_workspace.resolve.v1",
                    next_agent_run,
                    next_authorization,
                )
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(reclaimed.status_code, 409)
        self.assertEqual(reclaimed.json(), {"error": "session_workspace_lease_lost"})

        reclaimed_owner = self.client.post(
            "/internal/agent-runs/session-workspace/resolve",
            data=json.dumps(
                lease_request(
                    "runtime.session_workspace.resolve.v1",
                    next_agent_run,
                    next_authorization,
                    replacement_owner,
                )
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(reclaimed_owner.status_code, 200, reclaimed_owner.content)

    def test_interrupted_session_projects_cancelled_without_final_assistant(self):
        user = User.objects.create_user(username="cancelled@example.com", password="password")
        workspace = Workspace.objects.create(name="Cancelled", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="cancelled-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="cancel me")
        append_started(agent_run)
        append_session_records(agent_run, [session_record(agent_run, 3, "agent_run_interrupted", {"reasonType": "cancelled", "message": "Run cancelled by user.", "retryable": False})])

        projected = project_committed_agent_run(agent_run, "cancelled")

        self.assertEqual(projected.status, "cancelled")
        self.assertEqual(
            list(SessionEvent.objects.filter(agent_run=agent_run).order_by("sequence").values_list("payload__type", flat=True)),
            ["agent_run_started", "user_message", "agent_run_interrupted"],
        )

    def test_failed_session_without_assistant_text_keeps_raw_error(self):
        user = User.objects.create_user(username="failed@example.com", password="password")
        workspace = Workspace.objects.create(name="Failed", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="failed-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="fail me")
        append_started(agent_run)
        append_session_records(agent_run, [session_record(agent_run, 3, "agent_run_failed", {"reasonType": "runtime_error", "message": "provider unavailable"})])

        projected = project_committed_agent_run(agent_run, "failed")

        self.assertEqual(projected.status, "failed")
        terminal = SessionEvent.objects.get(agent_run=agent_run, payload__type="agent_run_failed")
        self.assertEqual(terminal.payload["payload"]["message"], "provider unavailable")
        self.assertFalse(SessionEvent.objects.filter(agent_run=agent_run, payload__type="assistant_message").exists())

    def test_workspace_finalization_failure_keeps_sealed_assistant_and_raw_error(self):
        user = User.objects.create_user(username="workspace-finalization@example.com", password="password")
        workspace = Workspace.objects.create(name="Workspace finalization", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="workspace-finalization-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="write report")
        append_started(agent_run)
        append_session_records(agent_run, [
            session_record(agent_run, 3, "assistant_message", {"messageId": f"message:{agent_run.turn_id}:assistant", "modelMarkdown": "report completed", "artifactRefs": [], "status": "done"}),
            session_record(agent_run, 4, "agent_run_failed", {"reasonType": "runtime_error", "message": "snapshot collect failed"}),
        ])

        projected = project_committed_agent_run(agent_run, "failed")

        self.assertEqual(projected.status, "failed")
        assistant = SessionEvent.objects.get(agent_run=agent_run, payload__type="assistant_message")
        terminal = SessionEvent.objects.get(agent_run=agent_run, payload__type="agent_run_failed")
        self.assertEqual(assistant.payload["payload"]["modelMarkdown"], "report completed")
        self.assertEqual(terminal.payload["payload"]["message"], "snapshot collect failed")

    def test_interrupted_session_preserves_partial_assistant(self):
        user = User.objects.create_user(username="cancelled-partial@example.com", password="password")
        workspace = Workspace.objects.create(name="Cancelled partial", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="cancelled-partial-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="cancel me")
        append_started(agent_run)
        append_session_records(agent_run, [
            session_record(agent_run, 3, "assistant_message", {"messageId": f"message:{agent_run.turn_id}:assistant", "modelMarkdown": "partial answer", "artifactRefs": [], "status": "error"}),
            session_record(agent_run, 4, "agent_run_interrupted", {"reasonType": "cancelled", "message": "Run cancelled by user.", "retryable": False}),
        ])

        projected = project_committed_agent_run(agent_run, "cancelled")

        self.assertEqual(projected.status, "cancelled")
        assistant = SessionEvent.objects.get(agent_run=agent_run, payload__type="assistant_message")
        self.assertEqual(assistant.payload["payload"]["modelMarkdown"], "partial answer")
        self.assertEqual(assistant.payload["payload"]["status"], "error")




    def test_agent_run_cancel_requests_durable_runtime_cancellation(self):
        user = User.objects.create_user(
            username="cancel-request@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Cancel request", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="cancel-request-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="cancel me",
            status="running",
        )
        create_agent_run_authorization(agent_run)
        self.client.force_login(user)

        with patch(
            "app_core.http.workspaces.request_agent_run_cancellation",
            return_value={
                "schema": "runtime.agent_run.cancel.result.v1",
                "agentRunId": agent_run.id,
                "disposition": "requested",
                "terminalState": None,
            },
        ) as cancel:
            response = self.client.post(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/cancel"
            )

        self.assertEqual(response.status_code, 202)
        self.assertEqual(
            response.json(),
            {
                "agentRunId": agent_run.id,
                "status": "running",
                "disposition": "requested",
            },
        )
        cancel.assert_called_once()
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.transitionReason, "agent_run_cancel_requested")

    def test_active_agent_run_supplement_uses_durable_runtime_admission(self):
        user = User.objects.create_user(
            username="supplement-request@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Supplement request", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="supplement-request-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="inspect runtime",
            status="running",
        )
        create_agent_run_authorization(agent_run)
        self.client.force_login(user)

        with patch(
            "app_core.http.workspaces.request_agent_run_supplement",
            return_value={
                "schema": "runtime.agent_run.supplement.result.v1",
                "accepted": True,
                "disposition": "accepted",
                "agentRunId": agent_run.id,
                "sessionId": session.id,
                "supplementId": "supplement-1",
                "queuedCount": 1,
                "queueRevision": 1,
            },
        ) as supplement:
            response = self.client.post(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/supplements",
                data=json.dumps(
                    {
                        "supplementId": "supplement-1",
                        "message": "check the cancellation edge",
                    }
                ),
                content_type="application/json",
            )

        self.assertEqual(response.status_code, 202)
        self.assertEqual(
            response.json(),
            {
                "agentRunId": agent_run.id,
                "sessionId": session.id,
                "supplementId": "supplement-1",
                "disposition": "accepted",
                "queuedCount": 1,
            },
        )
        supplement.assert_called_once_with(
            agent_run, "supplement-1", "check the cancellation edge"
        )

        with patch("app_core.http.workspaces.request_agent_run_supplement") as rejected:
            malformed = self.client.post(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/supplements",
                data=json.dumps(
                    {
                        "supplementId": "supplement-2",
                        "message": "banana",
                        "legacyQueue": True,
                    }
                ),
                content_type="application/json",
            )
            oversized_id = self.client.post(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/supplements",
                data=json.dumps(
                    {"supplementId": "x" * 65, "message": "banana"}
                ),
                content_type="application/json",
            )
        self.assertEqual(malformed.status_code, 400)
        self.assertEqual(oversized_id.status_code, 400)
        rejected.assert_not_called()

        agent_run.status = "completed"
        agent_run.save(update_fields=["status"])
        inactive = self.client.post(
            f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/supplements",
            data=json.dumps(
                {"supplementId": "supplement-2", "message": "banana"}
            ),
            content_type="application/json",
        )
        self.assertEqual(inactive.status_code, 404)

    def test_turn_supplement_projects_as_additional_user_message(self):
        user = User.objects.create_user(
            username="supplement-history@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Supplement history", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="supplement-history-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="inspect runtime",
        )
        append_started(agent_run)
        append_session_records(
            agent_run,
            [
                session_record(
                    agent_run,
                    3,
                    "turn_supplement",
                    {
                        "supplementId": "supplement-1",
                        "messageId": f"message:{agent_run.turn_id}:supplement:supplement-1",
                        "message": "check the cancellation edge",
                    },
                ),
                session_record(
                    agent_run,
                    4,
                    "assistant_message",
                    {
                        "messageId": f"message:{agent_run.turn_id}:assistant",
                        "modelMarkdown": "done",
                        "artifactRefs": [],
                        "status": "done",
                    },
                ),
                session_record(agent_run, 5, "agent_run_completed", {"doneReason": "finalized"}),
            ],
        )

        project_committed_agent_run(agent_run, "completed")

        self.assertEqual(
            list(
                SessionEvent.objects.filter(agent_run=agent_run)
                .order_by("sequence")
                .values_list("payload__type", flat=True)
            ),
            [
                "agent_run_started",
                "user_message",
                "turn_supplement",
                "assistant_message",
                "agent_run_completed",
            ],
        )

    def test_agent_run_cancel_race_projects_already_committed_terminal(self):
        user = User.objects.create_user(
            username="cancel-race@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Cancel race", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="cancel-race-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="cancel me",
            status="running",
        )
        create_agent_run_authorization(agent_run)
        append_started(agent_run)
        append_session_records(
            agent_run,
            [
                session_record(
                    agent_run,
                    3,
                    "agent_run_interrupted",
                    {
                        "reasonType": "cancelled",
                        "message": "Run cancelled by user.",
                        "retryable": False,
                    },
                )
            ],
        )
        self.client.force_login(user)

        with patch(
            "app_core.http.workspaces.request_agent_run_cancellation",
            return_value={
                "schema": "runtime.agent_run.cancel.result.v1",
                "agentRunId": agent_run.id,
                "disposition": "terminal",
                "terminalState": "cancelled",
            },
        ):
            response = self.client.post(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/cancel"
            )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {
                "agentRunId": agent_run.id,
                "status": "cancelled",
                "disposition": "terminal",
            },
        )

        # A projected terminal run must still reach runtime for waiter cleanup.
        with patch(
            "app_core.http.workspaces.request_agent_run_cancellation",
            return_value={"disposition": "terminal", "terminalState": "cancelled"},
        ) as cancel:
            repeated = self.client.post(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/cancel"
            )
        cancel.assert_called_once()
        self.assertEqual(repeated.status_code, 200)
        self.assertEqual(repeated.json(), response.json())
        with patch(
            "app_core.http.workspaces.request_agent_run_cancellation",
            side_effect=RuntimeError("runtime unavailable"),
        ):
            unavailable = self.client.post(
                f"/api/sessions/{session.id}/agent-runs/{agent_run.id}/cancel"
            )
        self.assertEqual(unavailable.status_code, 503)
        self.assertEqual(unavailable.json(), {"error": "agent_run_cancel_unavailable"})
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "cancelled")

    def test_session_rejects_a_second_active_run(self):
        user = User.objects.create_user(
            username="member@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        self.client.force_login(user)
        payload = {"text": "hello", "modelConfigRef": model.id}

        with patch(
            "app_core.http.workspaces.schedule_agent_run_lifecycle",
            return_value="inserted",
        ) as schedule:
            first = self.client.post(
                f"/api/workspaces/{workspace.id}/sessions/{session.id}/messages",
                data=json.dumps({**payload, "operationId": "first-run"}),
                content_type="application/json",
            )
            second = self.client.post(
                f"/api/workspaces/{workspace.id}/sessions/{session.id}/messages",
                data=json.dumps({**payload, "operationId": "second-run"}),
                content_type="application/json",
            )

        self.assertEqual(first.status_code, 202)
        self.assertEqual(second.status_code, 409)
        self.assertEqual(second.json(), {"error": "session_has_active_agent_run"})
        self.assertEqual(AgentRun.objects.filter(session=session).count(), 1)
        schedule.assert_called_once()

    def test_models_only_returns_enabled_models(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        ModelConfig.objects.create(
            id="enabled-model", displayName="Enabled", enabled=True
        )
        ModelConfig.objects.create(
            id="disabled-model", displayName="Disabled", enabled=False
        )
        self.client.force_login(user)

        response = self.client.get("/api/models")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            [model["id"] for model in response.json()["models"]], ["enabled-model"]
        )

    def test_session_direct_delete_lifecycle(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        activeSession = create_session(
            workspace=workspace, owner=user, title="Active"
        )
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=activeSession,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        self.client.force_login(user)

        activeList = self.client.get(f"/api/workspaces/{workspace.id}/sessions")
        unsupportedFilter = self.client.get(
            f"/api/workspaces/{workspace.id}/sessions?banana=1"
        )

        self.assertEqual(
            [item["id"] for item in activeList.json()["sessions"]], [activeSession.id]
        )
        self.assertEqual(activeList.json()["sessions"][0]["origin"], "user")
        self.assertEqual(unsupportedFilter.status_code, 400)
        self.assertEqual(
            unsupportedFilter.json(), {"error": "session_filter_unsupported"}
        )
        self.assertEqual(
            self.client.patch(
                f"/api/sessions/{activeSession.id}",
                data=json.dumps({}),
                content_type="application/json",
            ).json(),
            {"error": "session_metadata_invalid"},
        )

        with patch(
            "app_core.http.workspaces.request_agent_run_cancellation",
            return_value={
                "schema": "runtime.agent_run.cancel.result.v1",
                "agentRunId": agent_run.id,
                "disposition": "requested",
                "terminalState": None,
            },
        ) as cancel:
            deleted = self.client.delete(f"/api/sessions/{activeSession.id}")
        self.assertEqual(deleted.status_code, 200)
        self.assertEqual(deleted.json(), {"deleted": True})
        cancel.assert_called_once()
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.transitionReason, "agent_run_cancel_requested")
        activeSession.refresh_from_db()
        self.assertEqual(activeSession.status, "deleted")
        self.assertIsNotNone(activeSession.deletedAt)
        self.assertEqual(activeSession.purgedAt, activeSession.deletedAt)
        self.assertEqual(
            self.client.get(f"/api/sessions/{activeSession.id}").status_code, 404
        )
        self.assertEqual(
            self.client.get(f"/api/workspaces/{workspace.id}/sessions").json()[
                "sessions"
            ],
            [],
        )
        self.assertEqual(
            self.client.get(f"/api/sessions/{activeSession.id}/transcript").status_code,
            404,
        )
        repeated = self.client.delete(f"/api/sessions/{activeSession.id}")
        self.assertEqual(repeated.status_code, 410)
        self.assertEqual(repeated.json(), {"error": "session_deleted"})
        self.assertEqual(
            [
                session["id"]
                for session in self.client.get(
                    f"/api/workspaces/{workspace.id}/trash",
                    {"kind": "session"},
                ).json()["items"]
            ],
            [],
        )
        pending_restore = self.client.post(f"/api/sessions/{activeSession.id}/restore")
        self.assertEqual(pending_restore.status_code, 410)
        self.assertEqual(pending_restore.json(), {"error": "session_expired"})
        agent_run.status = "cancelled"
        agent_run.save(update_fields=["status", "updatedAt"])
        restored = self.client.post(f"/api/sessions/{activeSession.id}/restore")
        self.assertEqual(restored.status_code, 410)
        self.assertEqual(restored.json(), {"error": "session_expired"})
        self.assertEqual(
            self.client.delete(f"/api/workspaces/{workspace.id}/sessions").status_code,
            405,
        )
        self.assertEqual(Session.objects.get(id=activeSession.id).status, "deleted")

        with self.assertRaisesRegex(ValueError, "Session.status.*banana"):
            create_session(
                workspace=workspace,
                owner=user,
                status="banana",
            )
        with self.assertRaisesRegex(ValueError, "Session deletion state is invalid"):
            create_session(
                workspace=workspace,
                owner=user,
                status="deleted",
            )

    @patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted")
    def test_session_project_groups_first_message_without_becoming_a_source_folder(
        self, _schedule_agent_run_lifecycle
    ):
        user = User.objects.create_user(
            username="projects@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        Agent.objects.create(
            id="centaeris",
            workspace=workspace,
            owner=user,
            name="Centaeris",
        )
        model = ModelConfig.objects.create(id="project-model", displayName="Fake")
        self.client.force_login(user)

        created = self.client.post(
            f"/api/workspaces/{workspace.id}/session-projects",
            data=json.dumps({"agentId": "centaeris", "name": "  Lumi  "}),
            content_type="application/json",
        )

        self.assertEqual(created.status_code, 201, created.content)
        project = created.json()["project"]
        self.assertEqual(project["name"], "Lumi")
        self.assertEqual(project["agentId"], "centaeris")
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{workspace.id}/session-projects",
                {"agentId": "centaeris"},
            ).json()["projects"],
            [project],
        )
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{workspace.id}/session-projects"
            ).json(),
            {"error": "session_project_filter_invalid"},
        )

        message = self.client.post(
            f"/api/workspaces/{workspace.id}/sessions/new/messages",
            data=json.dumps(
                {
                    "operationId": "test-operation-2061",
                    "text": "project child",
                    "agentId": "centaeris",
                    "projectId": project["id"],
                    "modelConfigRef": model.id,
                }
            ),
            content_type="application/json",
        )

        self.assertEqual(message.status_code, 202, message.content)
        self.assertEqual(
            Session.objects.get(id=message.json()["sessionId"]).project_id,
            project["id"],
        )
        self.assertEqual(SessionProject.objects.get(id=project["id"]).name, "Lumi")

        rejected = self.client.post(
            f"/api/workspaces/{workspace.id}/sessions",
            data=json.dumps({"operationId": "test-operation-2082", "agentId": "centaeris", "projectId": "banana"}),
            content_type="application/json",
        )
        self.assertEqual(rejected.status_code, 404)
        self.assertEqual(rejected.json(), {"error": "session_project_not_found"})

    def test_session_metadata_pins_without_changing_recency(self):
        user = User.objects.create_user(
            username="metadata@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="metadata-model", displayName="Fake")
        recent = create_session(
            workspace=workspace, owner=user, title="Recent"
        )
        pinned = create_session(
            workspace=workspace, owner=user, title="Pinned"
        )
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=pinned,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        append_started(agent_run)
        append_completed(agent_run)
        pinned.refresh_from_db()
        self.assertTrue(pinned.isUnread)
        original_updated_at = pinned.updatedAt
        self.client.force_login(user)

        updated = self.client.patch(
            f"/api/sessions/{pinned.id}",
            data=json.dumps(
                {"title": "  Renamed  ", "isPinned": True, "isUnread": False}
            ),
            content_type="application/json",
        )

        self.assertEqual(updated.status_code, 200)
        self.assertEqual(
            updated.json()["session"],
            {
                "id": pinned.id,
                "workspaceId": workspace.id,
                "agentId": "centaeris",
                "projectId": None,
                "title": "Renamed",
                "origin": "user",
                "initialInputOrigin": None,
                "status": "active",
                "deletedAt": None,
                "isPinned": True,
                "isUnread": False,
                "hasActiveAgentRun": False,
                "updatedAt": original_updated_at.isoformat(),
            },
        )
        pinned.refresh_from_db()
        self.assertEqual(pinned.updatedAt, original_updated_at)
        self.assertFalse(pinned.isUnread)

        listed = self.client.get(f"/api/workspaces/{workspace.id}/sessions")

        self.assertEqual(listed.status_code, 200)
        self.assertEqual(
            [item["id"] for item in listed.json()["sessions"]],
            [pinned.id, recent.id],
        )
        self.assertEqual(
            self.client.patch(
                f"/api/sessions/{pinned.id}",
                data=json.dumps({"title": " "}),
                content_type="application/json",
            ).json(),
            {"error": "session_title_invalid"},
        )

    def test_session_origin_rejects_unknown_values(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)

        with self.assertRaisesRegex(ValueError, "Session.origin.*banana"):
            create_session(
                workspace=workspace,
                owner=user,
                origin="banana",
            )

    def test_session_list_flags_sessions_with_active_agent_runs(self):
        user = User.objects.create_user(
            username="active-flag@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="active-flag-model", displayName="Fake")
        running = create_session(workspace=workspace, owner=user, title="Running")
        idle = create_session(workspace=workspace, owner=user, title="Idle")
        queued_run = AgentRun.objects.create(
            workspace=workspace,
            session=running,
            user=user,
            modelConfig=model,
            prompt="hello",
            status="queued",
        )
        self.client.force_login(user)

        listed = self.client.get(f"/api/workspaces/{workspace.id}/sessions")

        self.assertEqual(listed.status_code, 200)
        by_id = {item["id"]: item for item in listed.json()["sessions"]}
        self.assertTrue(by_id[running.id]["hasActiveAgentRun"])
        self.assertFalse(by_id[idle.id]["hasActiveAgentRun"])
        queued_run.status = "completed"
        queued_run.save(update_fields=["status"])
        listed_again = self.client.get(f"/api/workspaces/{workspace.id}/sessions")
        self.assertFalse(
            listed_again.json()["sessions"][0]["hasActiveAgentRun"]
        )

    def test_internal_model_run_requires_token_and_writes_usage(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)

        unauthorized = self.client.post(
            "/internal/model-runs",
            data=json.dumps(
                {
                    "schema": MODEL_RUN_SCHEMA,
                    "agentRunId": agent_run.id,
                    "modelConfigRef": model.id,
                    "maxOutputTokens": model.maxOutputTokens,
                    "authorizationRef": authorization.id,
                    "authorizationDigest": authorization.digest,
                    "preparedPrompt": prepared_prompt_for_test(model),
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(unauthorized.status_code, 401)

        missingSchema = self.client.post(
            "/internal/model-runs",
            data=json.dumps(
                {"agentRunId": "agent_run_1", "modelConfigRef": model.id, "prompt": "hello"}
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(missingSchema.status_code, 400)

        response = self.client.post(
            "/internal/model-runs",
            data=json.dumps(
                {
                    "schema": MODEL_RUN_SCHEMA,
                    "agentRunId": agent_run.id,
                    "modelConfigRef": model.id,
                    "maxOutputTokens": model.maxOutputTokens,
                    "authorizationRef": authorization.id,
                    "authorizationDigest": authorization.digest,
                    "preparedPrompt": prepared_prompt_for_test(model),
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["text"], "这是最小纵切响应。")
        self.assertIsNone(response.json()["reasoningContent"])
        log = ModelRunLog.objects.get(agentRunId=agent_run.id)
        self.assertEqual(log.status, "success")
        self.assertEqual(log.totalTokens, 0)

    def test_internal_model_run_requires_authorized_thinking_mode(self):
        user = User.objects.create_user(username="thinking@example.com", password="password")
        workspace = Workspace.objects.create(name="Thinking", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="thinking-model",
            displayName="Thinking",
            thinkingMode="vendor-high",
            thinkingModes=["vendor-high"],
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            thinkingMode="vendor-high",
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)
        body = {
            "schema": MODEL_RUN_SCHEMA,
            "agentRunId": agent_run.id,
            "modelConfigRef": model.id,
            "maxOutputTokens": model.maxOutputTokens,
            "authorizationRef": authorization.id,
            "authorizationDigest": authorization.digest,
            "preparedPrompt": prepared_prompt_for_test(model),
        }

        missing = self.client.post(
            "/internal/model-runs",
            data=json.dumps(body),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(missing.status_code, 409)

        accepted = self.client.post(
            "/internal/model-runs",
            data=json.dumps(body | {"thinkingMode": "vendor-high"}),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(accepted.status_code, 200, accepted.content)

    def test_internal_model_run_rejects_model_mismatch_with_agent_run_authorization(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        otherModel = ModelConfig.objects.create(
            id="other-model", displayName="Other"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)

        response = self.client.post(
            "/internal/model-runs",
            data=json.dumps(
                {
                    "schema": MODEL_RUN_SCHEMA,
                    "agentRunId": agent_run.id,
                    "modelConfigRef": otherModel.id,
                    "authorizationRef": authorization.id,
                    "authorizationDigest": authorization.digest,
                    "preparedPrompt": prepared_prompt_for_test(model),
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )

        self.assertEqual(response.status_code, 409)
        self.assertFalse(ModelRunLog.objects.exists())

    def test_internal_model_run_preserves_fake_tool_calls(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-tool-model",
            displayName="Fake Tool Call",
            modelName="fake-tool-call",
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)

        response = self.client.post(
            "/internal/model-runs",
            data=json.dumps(
                {
                    "schema": MODEL_RUN_SCHEMA,
                    "agentRunId": agent_run.id,
                    "modelConfigRef": model.id,
                    "maxOutputTokens": model.maxOutputTokens,
                    "authorizationRef": authorization.id,
                    "authorizationDigest": authorization.digest,
                    "preparedPrompt": prepared_prompt_for_test(
                        model,
                        messages=[
                            {
                                "messageId": "msg-user",
                                "role": "user",
                                "content": "write a file",
                            }
                        ],
                        toolDefinitions=[
                            {
                                "name": "write",
                                "description": "write",
                                "inputSchema": {"type": "object"},
                            }
                        ],
                        toolChoice={"type": "auto"},
                    ),
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )

        self.assertEqual(response.status_code, 200)
        toolCalls = response.json()["toolCalls"]
        self.assertEqual(toolCalls[0]["name"], "write")
        self.assertEqual(toolCalls[0]["id"], "call_write")

    def test_internal_model_run_streams_multiple_deltas_and_terminal_usage(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)
        response = self.client.post(
            "/internal/model-runs",
            data=json.dumps(
                {
                    "schema": MODEL_RUN_SCHEMA,
                    "agentRunId": agent_run.id,
                    "modelConfigRef": model.id,
                    "maxOutputTokens": model.maxOutputTokens,
                    "authorizationRef": authorization.id,
                    "authorizationDigest": authorization.digest,
                    "preparedPrompt": prepared_prompt_for_test(model),
                }
            ),
            content_type="application/json",
            HTTP_ACCEPT="text/event-stream",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        stream = streaming_response_bytes(response).decode("utf-8")
        self.assertGreaterEqual(stream.count("event: delta"), 2)
        self.assertEqual(stream.count("event: result"), 1)
        self.assertIn("这是最小纵切响应。", stream)
        log = ModelRunLog.objects.get(agentRunId=agent_run.id)
        self.assertEqual(log.status, "success")
        self.assertEqual(log.totalTokens, 0)

    def test_internal_model_run_rejects_non_exact_prepared_prompt_before_provider(self):
        user = User.objects.create_user(
            username="strict@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Strict", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="strict-model", displayName="Strict"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="strict",
        )
        authorization = create_agent_run_authorization(agent_run)

        def submit(preparedPrompt: dict, **extraFields):
            return self.client.post(
                "/internal/model-runs",
                data=json.dumps(
                    {
                        "schema": MODEL_RUN_SCHEMA,
                        "agentRunId": agent_run.id,
                        "modelConfigRef": model.id,
                        "maxOutputTokens": model.maxOutputTokens,
                        "authorizationRef": authorization.id,
                        "authorizationDigest": authorization.digest,
                        "preparedPrompt": preparedPrompt,
                        **extraFields,
                    }
                ),
                content_type="application/json",
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            )

        legacy = submit(prepared_prompt_for_test(model), userMessage="legacy")
        self.assertEqual(legacy.status_code, 400)
        self.assertEqual(legacy.json()["error"], "model_run_fields_invalid")

        unknownNested = prepared_prompt_for_test(model) | {"banana": True}
        nested = submit(unknownNested)
        self.assertEqual(nested.status_code, 400)
        self.assertEqual(nested.json()["error"], "prepared_prompt_fields_invalid")

        duplicateMessages = prepared_prompt_for_test(
            model,
            messages=[
                {"messageId": "duplicate", "role": "user", "content": "one"},
                {"messageId": "duplicate", "role": "assistant", "content": "two"},
            ],
        )
        duplicate = submit(duplicateMessages)
        self.assertEqual(duplicate.status_code, 400)
        self.assertEqual(
            duplicate.json()["error"], "prepared_prompt_message_id_duplicate"
        )

        mismatchedPair = prepared_prompt_for_test(
            model,
            messages=[
                {
                    "messageId": "assistant",
                    "role": "assistant",
                    "content": "",
                    "toolCalls": [
                        {"id": "call-1", "name": "read", "argsJson": '{"path":"a.md"}'}
                    ],
                },
                {
                    "messageId": "tool",
                    "role": "tool",
                    "content": "result",
                    "toolCallId": "call-2",
                },
            ],
        )
        mismatch = submit(mismatchedPair)
        self.assertEqual(mismatch.status_code, 400)
        self.assertEqual(
            mismatch.json()["error"], "prepared_prompt_tool_pairing_invalid"
        )
        self.assertFalse(ModelRunLog.objects.filter(agentRunId=agent_run.id).exists())

    def test_fake_bash_reports_remote_success_and_structured_failure(self):
        model = ModelConfig(modelName="fake-bash")
        first = fake_model_response(
            model, {"preparedPrompt": prepared_prompt_for_test(model)}
        )
        self.assertEqual(first["toolCalls"][0]["name"], "bash")

        success = fake_model_response(
            model,
            {
                "preparedPrompt": prepared_prompt_for_test(
                    model,
                    messages=[
                        {
                            "messageId": "msg-assistant",
                            "role": "assistant",
                            "content": "",
                            "toolCalls": [
                                {
                                    "id": "call-bash",
                                    "name": "bash",
                                    "argsJson": '{"command":"pwd"}',
                                }
                            ],
                        },
                        {
                            "messageId": "msg-tool",
                            "role": "tool",
                            "content": '{"stdout":"remote-bash-ok"}',
                            "toolCallId": "call-bash",
                        },
                    ],
                )
            },
        )
        failure = fake_model_response(
            model,
            {
                "preparedPrompt": prepared_prompt_for_test(
                    model,
                    messages=[
                        {
                            "messageId": "msg-assistant",
                            "role": "assistant",
                            "content": "",
                            "toolCalls": [
                                {
                                    "id": "call-bash",
                                    "name": "bash",
                                    "argsJson": '{"command":"pwd"}',
                                }
                            ],
                        },
                        {
                            "messageId": "msg-tool",
                            "role": "tool",
                            "content": '{"executed":false}',
                            "toolCallId": "call-bash",
                        },
                    ],
                )
            },
        )
        self.assertEqual(success["text"], "远程 Bash 执行成功。")
        self.assertEqual(failure["text"], "Bash 结构化失败；未执行本地回退。")

        evidence = fake_model_response(
            ModelConfig(modelName="fake-evidence"),
            {
                "agentRunId": "agent_run_current",
                "preparedPrompt": prepared_prompt_for_test(
                    ModelConfig(modelName="fake-evidence"),
                    messages=[
                        {
                            "messageId": "msg-user-previous",
                            "role": "user",
                            "content": "previous",
                        },
                        {
                            "messageId": "msg-assistant",
                            "role": "assistant",
                            "content": "previous final",
                        },
                        {
                            "messageId": "msg-user-current",
                            "role": "user",
                            "content": "current",
                        },
                    ],
                ),
            },
        )
        self.assertEqual(evidence["toolCalls"][0]["name"], "bash")

    def test_direct_provider_request_preserves_tool_messages_and_output_limit(self):
        provider = ModelProvider.objects.create(
            displayName="Direct test",
            api="openai-completions",
            apiBase="https://models.example.com/v1",
        )
        model = ModelConfig.objects.create(
            displayName="DeepSeek",
            provider=provider,
            modelName="deepseek-v4-pro",
            resolvedApi="openai-completions",
            resolvedApiBase=provider.apiBase,
        )
        deepseekModel = model
        toolArgs = json.dumps({"path": "report.md", "content": "hello"})
        preparedPrompt = prepared_prompt_for_test(
            model,
            messages=[
                {
                    "messageId": "msg-user",
                    "role": "user",
                    "content": "write a file",
                },
                {
                    "messageId": "msg-assistant",
                    "role": "assistant",
                    "content": "",
                    "toolCalls": [
                        {"id": "call_1", "name": "write", "argsJson": toolArgs}
                    ],
                },
                {
                    "messageId": "msg-tool",
                    "role": "tool",
                    "content": '{"ok":true}',
                    "toolCallId": "call_1",
                },
            ],
            toolDefinitions=[
                {
                    "name": "write",
                    "description": "write",
                    "inputSchema": {"type": "object"},
                }
            ],
            toolChoice={"type": "auto"},
        )
        preparedPrompt["maxOutputTokens"] = 128
        request = build_open_ai_completions_request(
            model,
            {
                "preparedPrompt": preparedPrompt,
            },
        )
        self.assertEqual(request["max_tokens"], 128)

        self.assertEqual(request["model"], "deepseek-v4-pro")
        self.assertEqual(request["tools"][0]["function"]["name"], "write")
        self.assertEqual(
            request["messages"][1]["tool_calls"][0]["function"]["name"], "write"
        )
        self.assertEqual(
            request["messages"][1]["tool_calls"][0]["function"]["arguments"], toolArgs
        )
        self.assertEqual(request["messages"][2]["tool_call_id"], "call_1")

        chunks = [
            {
                "choices": [{"finish_reason": "stop", "delta": {"content": "ok"}}],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2,
                },
            }
        ]
        client = Mock()
        client.chat.completions.create = AsyncMock(return_value=async_values(chunks))
        client.close = AsyncMock()
        with patch(
            "app_core.model_adapter.openai_completions.async_open_ai_completions_client",
            new=AsyncMock(return_value=client),
        ):
            async_iterator_bytes(
                stream_open_ai_completions(
                    deepseekModel,
                    {"preparedPrompt": prepared_prompt_for_test(deepseekModel)},
                    {},
                    lambda event_type, payload: b"",
                )
            )
        self.assertTrue(client.chat.completions.create.call_args.kwargs["stream"])
        self.assertEqual(
            client.chat.completions.create.call_args.kwargs["stream_options"],
            {"include_usage": True},
        )


    def test_runtime_client_schedules_stable_agent_run_lifecycle_job(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        from .runtime_client import schedule_agent_run_lifecycle

        session = create_session(workspace=workspace, owner=user)
        self.assertTrue(session.id.startswith("session_"))
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)

        captured = {}

        class FakeResponse:
            status = 202

            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

            def read(self):
                return json.dumps(
                    {
                        "disposition": "inserted",
                        "job": {
                            "jobId": f"agent_run.lifecycle:{agent_run.id}",
                            "jobKind": "agent_run.lifecycle",
                            "idempotencyKey": f"agent_run.lifecycle:{agent_run.id}:{authorization.digest}",
                            "sessionId": session.id,
                            "payloadRef": f"record:agent_run:{agent_run.id}",
                            "status": "queued",
                        },
                    }
                ).encode("utf-8")

        def fake_urlopen(request, timeout):
            captured["body"] = json.loads(request.data.decode("utf-8"))
            captured["url"] = request.full_url
            return FakeResponse()

        with patch("urllib.request.urlopen", fake_urlopen):
            self.assertEqual(schedule_agent_run_lifecycle(agent_run), "inserted")

        self.assertEqual(captured["body"]["schema"], "runtime.job.schedule.v1")
        self.assertEqual(captured["body"]["jobId"], f"agent_run.lifecycle:{agent_run.id}")
        self.assertEqual(captured["body"]["sessionId"], session.id)
        self.assertEqual(captured["body"]["workspaceId"], workspace.id)
        self.assertEqual(captured["body"]["payloadRef"], f"record:agent_run:{agent_run.id}")
        self.assertEqual(
            captured["body"]["idempotencyKey"],
            f"agent_run.lifecycle:{agent_run.id}:{authorization.digest}",
        )
        self.assertTrue(captured["url"].endswith("/internal/jobs/schedule"))

    def test_runtime_client_materializes_signed_agent_run_only_for_lifecycle_worker(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        from .runtime_client import build_agent_run_start

        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
            agent_instructions="Prefer primary sources.",
        )
        authorization = create_agent_run_authorization(agent_run)
        agentRunStart = build_agent_run_start(agent_run)
        self.assertEqual(agentRunStart["schema"], "workspace.agent_run.start.v2")
        self.assertEqual(agentRunStart["initialInput"], {"type": "userMessage"})
        self.assertEqual(agentRunStart["agentInstructions"], "Prefer primary sources.")
        self.assertEqual(agentRunStart["authorization"], authorization.payload)
        self.assertEqual(agentRunStart["authorizationDigest"], authorization.digest)
        self.assertEqual(agentRunStart["modelContextTokens"], model.contextTokens)
        self.assertEqual(agentRunStart["modelMaxOutputTokens"], model.maxOutputTokens)

    def test_runtime_transition_records_running_state_and_projects_semantic_terminal(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        append_started(agent_run)
        response = self.client.post(
            "/internal/agent-runs/transition",
            data=json.dumps(
                {
                    "schema": "runtime.agent_run.transition.v1",
                    "agentRunId": agent_run.id,
                    "state": "running",
                    "transitionReason": "agent_run_lifecycle_step_started",
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(response.status_code, 200)

        agent_run.refresh_from_db()
        resumed = self.client.post(
            "/internal/agent-runs/transition",
            data=json.dumps(
                {
                    "schema": "runtime.agent_run.transition.v1",
                    "agentRunId": agent_run.id,
                    "state": "running",
                    "transitionReason": "runtime_job_wait",
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(resumed.status_code, 200)
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.transitionReason, "runtime_job_wait")

        append_session_records(
            agent_run,
            [
                session_record(
                    agent_run,
                    3,
                    "assistant_message",
                    {
                        "messageId": f"message:{agent_run.turn_id}:assistant",
                        "modelMarkdown": "done",
                        "artifactRefs": [],
                        "status": "done",
                    },
                ),
                session_record(agent_run, 4, "agent_run_completed", {"doneReason": "finalized"}),
            ],
        )
        response = self.client.post(
            "/internal/agent-runs/transition",
            data=json.dumps(
                {
                    "schema": "runtime.agent_run.transition.v1",
                    "agentRunId": agent_run.id,
                    "state": "completed",
                    "transitionReason": "runtime_session_terminal_committed",
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(response.status_code, 200)

        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "completed")
        self.assertEqual(agent_run.transitionReason, "runtime_session_terminal_committed")







    def test_deleted_unpurged_session_metadata_remains_readable(self):
        user = User.objects.create_user(
            username="deleted-session-reader@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Deleted session", createdBy=user)
        workspace.members.add(user)
        session = create_session(workspace=workspace, owner=user)
        session.status = "deleted"
        session.deletedAt = timezone.now()
        session.save(update_fields=["status", "deletedAt", "updatedAt"])
        self.client.force_login(user)

        response = self.client.get(f"/api/sessions/{session.id}")

        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["session"]["id"], session.id)
        self.assertEqual(response.json()["session"]["status"], "deleted")


    def test_transcript_page_freezes_source_high_water_and_reauthorizes_older_pages(self):
        owner = User.objects.create_user(username="transcript-owner@example.com", password="password")
        stranger = User.objects.create_user(username="transcript-stranger@example.com", password="password")
        workspace = Workspace.objects.create(name="Transcript", createdBy=owner)
        workspace.members.add(owner)
        model = ModelConfig.objects.create(displayName="Transcript")
        session = create_session(workspace=workspace, owner=owner)
        run = AgentRun.objects.create(
            workspace=workspace, session=session, user=owner, modelConfig=model, prompt="hello"
        )
        append_started(run)
        page = {
            "schema": "transcript.page.v1",
            "sessionId": session.id,
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-1",
            "sourceHighWater": "2",
            "blocks": [],
            "olderCursor": "bound-cursor",
            "hasOlder": True,
            "resumeCursors": [
                {"streamId": "workspace-transcript.v1", "cursor": "2"}
            ],
        }
        self.client.force_login(owner)
        with patch("app_core.http.workspaces.request_transcript_page", return_value=page) as request_page:
            response = self.client.get(f"/api/sessions/{session.id}/transcript")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), page)
        self.assertEqual(
            request_page.call_args.args[0],
            {
                "schema": "runtime.transcript.page.read.v1",
                "sessionId": session.id,
                "projectionVersion": "transcript.projection.v1",
                "projectionGeneration": None,
                "sourceHighWater": "2",
                "olderCursor": None,
            },
        )

        with patch("app_core.http.workspaces.request_transcript_page", return_value=page) as request_page:
            frozen_tail = self.client.get(
                f"/api/sessions/{session.id}/transcript",
                {"projectionGeneration": "generation-1", "sourceHighWater": "2"},
            )
        self.assertEqual(frozen_tail.status_code, 200, frozen_tail.content)
        self.assertIsNone(request_page.call_args.args[0]["olderCursor"])
        self.assertEqual(request_page.call_args.args[0]["sourceHighWater"], "2")

        with patch("app_core.http.workspaces.request_transcript_page", return_value=page) as request_page:
            older = self.client.get(
                f"/api/sessions/{session.id}/transcript",
                {
                    "projectionGeneration": "generation-1",
                    "sourceHighWater": "2",
                    "olderCursor": "bound-cursor",
                },
            )
        self.assertEqual(older.status_code, 200, older.content)
        self.assertEqual(request_page.call_args.args[0]["sourceHighWater"], "2")
        self.assertEqual(request_page.call_args.args[0]["olderCursor"], "bound-cursor")

        not_ready_payload = {
            "error": "transcript_projection_not_ready",
            "projectionGeneration": "generation-1",
            "sourceHighWater": "2",
            "projectedHighWater": "1",
        }
        with patch(
            "app_core.http.workspaces.request_transcript_page",
            side_effect=TranscriptRuntimeError(not_ready_payload),
        ):
            not_ready = self.client.get(
                f"/api/sessions/{session.id}/transcript",
                {
                    "projectionGeneration": "generation-1",
                    "sourceHighWater": "2",
                },
            )
        self.assertEqual(not_ready.status_code, 409, not_ready.content)
        self.assertEqual(not_ready.json(), not_ready_payload)
        self.assertEqual(not_ready.headers["Cache-Control"], "no-store")

        workspace.members.remove(owner)
        with patch("app_core.http.workspaces.request_transcript_page") as revoked_runtime:
            revoked = self.client.get(
                f"/api/sessions/{session.id}/transcript",
                {
                    "projectionGeneration": "generation-1",
                    "sourceHighWater": "2",
                    "olderCursor": "bound-cursor",
                },
            )
        self.assertEqual(revoked.status_code, 404)
        revoked_runtime.assert_not_called()

        self.client.force_login(stranger)
        with patch("app_core.http.workspaces.request_transcript_page") as denied_runtime:
            denied = self.client.get(f"/api/sessions/{session.id}/transcript")
        self.assertEqual(denied.status_code, 404)
        denied_runtime.assert_not_called()

    def test_transcript_content_rejects_unversioned_spill_and_reauthorizes(self):
        owner = User.objects.create_user(username="content-owner@example.com", password="password")
        workspace = Workspace.objects.create(name="Content", createdBy=owner)
        workspace.members.add(owner)
        model = ModelConfig.objects.create(displayName="Content")
        session = create_session(workspace=workspace, owner=owner)
        run = AgentRun.objects.create(
            workspace=workspace, session=session, user=owner, modelConfig=model, prompt="hello"
        )
        append_started(run)
        output = ("x" * (64 * 1024 - 2) + "世").encode("utf-8")
        prefix = b"log:"
        path = ".agent-tool-results/result.log"
        file_bytes = prefix + output
        manifest = {
            "schema": "workspace.snapshot.v1",
            "files": [{
                "path": path,
                "sizeBytes": len(file_bytes),
                "sha256": f"sha256:{hashlib.sha256(file_bytes).hexdigest()}",
                "executable": False,
            }],
        }
        manifest_bytes = json.dumps(
            manifest, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")
        snapshot = len(manifest_bytes).to_bytes(4, "big") + manifest_bytes + file_bytes
        storage_key = f"tests/transcript-content/{session.id}.snapshot"
        default_storage.save(storage_key, ContentFile(snapshot))
        session.workspaceStorageKey = storage_key
        session.workspaceSnapshotSha256 = f"sha256:{hashlib.sha256(snapshot).hexdigest()}"
        session.workspaceSnapshotSizeBytes = len(snapshot)
        session.workspaceExpandedSizeBytes = len(file_bytes)
        session.workspaceFileCount = 1
        session.workspaceGeneration = 1
        session.workspaceLastAdvancedAgentRun = run
        session.save(update_fields=[
            "workspaceStorageKey", "workspaceSnapshotSha256", "workspaceSnapshotSizeBytes",
            "workspaceExpandedSizeBytes", "workspaceFileCount", "workspaceGeneration",
            "workspaceLastAdvancedAgentRun", "updatedAt",
        ])
        append_session_records(run, [
            session_record(run, 3, "tool_call", {
                "callId": "call-range", "toolName": "shell", "providerId": "builtin",
                "normalizedInput": {}, "toolContractDigest": "sha256:" + "a" * 64,
                "displayTarget": "range",
            }),
            session_record(run, 4, "tool_result", {
                "callId": "call-range", "toolName": "shell", "resultState": "successWithOutput",
                "modelContent": "preview", "outputComplete": True,
                "fullOutputPath": path, "outputStartByte": len(prefix),
                "outputByteLength": len(output), "summary": "output", "operations": [],
                "modelInputImages": [], "latencyMs": 1,
            }),
        ])
        query = {
            "projectionGeneration": "generation-1",
            "refId": "tool-output:call-range",
            "revision": "2",
            "byteLength": str(len(output)),
            "offset": "0",
        }
        self.client.force_login(owner)
        try:
            # Prove index availability, not the natural plan for four rows.
            # Roll back rows, column statistics and the local setting before
            # HTTP checks; pg_class row/page estimates need not roll back.
            with connection.cursor() as cursor:
                cursor.execute("SHOW enable_seqscan")
                previous_seqscan = cursor.fetchone()[0]
            with transaction.atomic():
                other = create_session(workspace=workspace, owner=owner)
                other_run = AgentRun.objects.create(
                    workspace=workspace, session=other, user=owner, modelConfig=model, prompt="index fixture"
                )
                events = []
                for fixture_run in (run, other_run):
                    for sequence in range(5, 517):
                        call_id = "call-range" if fixture_run == other_run and sequence == 5 else f"index-call-{sequence}"
                        event_type = "tool_result" if sequence % 4 == 1 else "user_message"
                        payload = ({"callId": call_id, "modelContent": "indexed output"}
                                   if event_type == "tool_result" else {"text": "ordinary event"})
                        event = session_record(fixture_run, sequence, event_type, payload)["event"]
                        events.append(SessionEvent(
                            eventId=event["eventId"], workspace=workspace, session=fixture_run.session,
                            agent_run=fixture_run, sequence=sequence, agent_run_sequence=sequence,
                            projects_to_agent_run_stream=True, payload=event, createdAtMs=sequence,
                        ))
                SessionEvent.objects.bulk_create(events)
                candidates = (SessionEvent.objects.filter(session=session).extra(
                    where=["app_core_sessionevent.payload ->> 'type' = 'tool_result'",
                           "app_core_sessionevent.payload #>> '{payload,callId}' = %s"],
                    params=["call-range"],
                ).only("payload")[:2])
                with connection.cursor() as cursor:
                    cursor.execute("ANALYZE app_core_sessionevent")
                    cursor.execute("SET LOCAL enable_seqscan = off")
                    cursor.execute("SHOW enable_seqscan")
                    self.assertEqual(cursor.fetchone()[0], "off")
                    cursor.execute(
                        "SELECT indisvalid, indisready FROM pg_index WHERE indexrelid = %s::regclass",
                        ["session_event_tool_result_lookup"],
                    )
                    self.assertEqual(cursor.fetchone(), (True, True))
                plan = json.loads(candidates.explain(format="JSON", verbose=True))[0]["Plan"]

                def plan_nodes(node):
                    yield node
                    for child in node.get("Plans", []):
                        yield from plan_nodes(child)

                lookup = [node for node in plan_nodes(plan)
                          if node.get("Index Name") == "session_event_tool_result_lookup"]
                self.assertEqual(len(lookup), 1, plan)
                self.assertIn("session_id", lookup[0]["Index Cond"])
                self.assertIn("{payload,callId}", lookup[0]["Index Cond"])
                self.assertIn(session.id, lookup[0]["Index Cond"])
                self.assertIn("call-range", lookup[0]["Index Cond"])
                self.assertTrue(any("eventId" in output for output in plan["Output"]), plan)
                self.assertEqual(len(list(candidates)), 1)
                transaction.set_rollback(True)
            with connection.cursor() as cursor:
                cursor.execute("SHOW enable_seqscan")
                self.assertEqual(cursor.fetchone()[0], previous_seqscan)
                cursor.execute(
                    "SELECT count(*) FROM app_core_sessionevent WHERE session_id = %s", [session.id]
                )
                self.assertEqual(cursor.fetchone()[0], 4)
            first = self.client.get(f"/api/sessions/{session.id}/transcript/content", query)
            self.assertEqual(first.status_code, 409, first.content)
            self.assertEqual(first.json(), {"error": "transcript_content_unavailable"})
            workspace.members.remove(owner)
            denied = self.client.get(f"/api/sessions/{session.id}/transcript/content", query)
            self.assertEqual(denied.status_code, 404)
        finally:
            default_storage.delete(storage_key)

    def test_transcript_text_range_delegates_to_core_after_membership_check(self):
        owner = User.objects.create_user(username="text-reader@example.com", password="password")
        workspace = Workspace.objects.create(name="Text", createdBy=owner)
        workspace.members.add(owner)
        session = create_session(workspace=workspace, owner=owner)
        self.client.force_login(owner)
        query = {"projectionGeneration": "generation-1", "refId": "session-event:answer:modelMarkdown",
                 "revision": "1", "byteLength": "70000", "offset": "0"}
        with patch("app_core.http.workspaces.request_transcript_content", return_value={"content": "正文"}) as read:
            response = self.client.get(f"/api/sessions/{session.id}/transcript/content", query)
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(read.call_args.args[0]["sessionId"], session.id)
            self.assertEqual(read.call_args.args[0]["refId"], query["refId"])
        workspace.members.remove(owner)
        with patch("app_core.http.workspaces.request_transcript_content") as read:
            response = self.client.get(f"/api/sessions/{session.id}/transcript/content", query)
            self.assertEqual(response.status_code, 404)
            read.assert_not_called()

    def test_transcript_patch_read_freezes_through_waterline_and_reauthorizes(self):
        owner = User.objects.create_user(username="patch-owner@example.com", password="password")
        workspace = Workspace.objects.create(name="Patches", createdBy=owner)
        workspace.members.add(owner)
        model = ModelConfig.objects.create(displayName="Patches")
        session = create_session(workspace=workspace, owner=owner)
        run = AgentRun.objects.create(
            workspace=workspace, session=session, user=owner, modelConfig=model, prompt="hello"
        )
        append_started(run)
        append_completed(run)
        payload = {
            "schema": "transcript.patch.page.v1",
            "sessionId": session.id,
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-1",
            "throughSourceHighWater": "4",
            "patches": [],
            "nextSourceHighWater": "4",
            "hasMore": False,
        }
        self.client.force_login(owner)
        with patch("app_core.http.workspaces.request_transcript_patches", return_value=payload) as request_patches:
            response = self.client.get(
                f"/api/sessions/{session.id}/transcript/patches",
                {
                    "projectionGeneration": "generation-1",
                    "afterSourceHighWater": "2",
                },
            )
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), payload)
        self.assertEqual(
            request_patches.call_args.args[0],
            {
                "schema": "runtime.transcript.patch.read.v1",
                "sessionId": session.id,
                "projectionVersion": "transcript.projection.v1",
                "projectionGeneration": "generation-1",
                "afterSourceHighWater": "2",
                "throughSourceHighWater": "4",
            },
        )

    def test_failed_unadmitted_run_has_no_endless_event_stream(self):
        from asgiref.sync import async_to_sync
        owner = User.objects.create_user(username="unadmitted@example.com", password="password")
        workspace = Workspace.objects.create(name="Unadmitted", createdBy=owner)
        workspace.members.add(owner)
        session = create_session(workspace=workspace, owner=owner)
        run = AgentRun.objects.create(workspace=workspace, session=session, user=owner,
            modelConfig=ModelConfig.objects.create(displayName="Unadmitted"), prompt="keep my input",
            status="failed")
        self.client.force_login(owner)
        response = self.client.get(f"/api/sessions/{session.id}/agent-runs/{run.id}/events")
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json(), {"error": "agent_run_not_admitted"})
        self.assertEqual(async_to_sync(agent_run_stream._load_terminal_sequence)(run.id), 0)
        self.assertFalse(SessionEvent.objects.filter(agent_run=run).exists())

    def test_transcript_citations_bind_tool_sequence_to_its_run_and_session(self):
        owner = User.objects.create_user(username="citation-binding@example.com", password="password")
        workspace = Workspace.objects.create(name="Citation binding", createdBy=owner)
        workspace.members.add(owner)
        session = create_session(workspace=workspace, owner=owner)
        other_session = create_session(workspace=workspace, owner=owner)
        model = ModelConfig.objects.create(displayName="Citation binding")
        runs = [AgentRun.objects.create(workspace=workspace, session=target, user=owner,
            modelConfig=model, prompt="read") for target in [session, session, other_session]]
        for run in runs:
            append_started(run)
            append_session_records(run, [session_record(run, 3, "tool_call", {
                "callId": "same-call", "toolName": "read_material", "providerId": "builtin",
                "normalizedInput": {}, "toolContractDigest": "sha256:" + "a" * 64,
                "displayTarget": "material",
            })])
        def snapshot(run):
            return {"schema": "workspace.citations.v1", "sessionId": run.session_id,
                "agentRunId": run.id, "throughSequence": 3, "citations": [
                    {"citationId": f"citation:{run.id}", "inputRef": "input", "displayName": "Material",
                     "sourceToolCallId": "same-call", "sourceUrl": f"/api/citations/citation:{run.id}"},
                    {"citationId": "citation:unrelated", "inputRef": "other", "displayName": "Other",
                     "sourceToolCallId": "other-call", "sourceUrl": "/api/citations/citation:unrelated"}]}
        self.client.force_login(owner)
        url = f"/api/sessions/{session.id}/transcript/citations"
        with patch("app_core.http.citations.citation_snapshot", side_effect=snapshot) as snapshots:
            response = self.client.get(url, {"sequence": ["1", "3", "6", "3"]})
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), {"sessionId": session.id, "bindings": [
            {"sourceSequence": str(sequence), "sourceToolCallId": "same-call",
             "snapshot": {**snapshot(run), "citations": snapshot(run)["citations"][:1]}}
            for sequence, run in [(3, runs[0]), (6, runs[1])]]})
        self.assertEqual(snapshots.call_count, 2)
        for query in [{"sequence": ["3"] * 129}, {"sequence": "03"}, {"sequence": "3", "extra": "x"}]:
            self.assertEqual(self.client.get(url, query).status_code, 400)
        workspace.members.remove(owner)
        self.assertEqual(self.client.get(url, {"sequence": "3"}).status_code, 404)

    def test_transcript_work_times_restore_the_original_run_clock(self):
        owner = User.objects.create_user(username="work-clock@example.com", password="password")
        workspace = Workspace.objects.create(name="Work clock", createdBy=owner)
        workspace.members.add(owner)
        session = create_session(workspace=workspace, owner=owner)
        run = AgentRun.objects.create(workspace=workspace, session=session, user=owner,
            modelConfig=ModelConfig.objects.create(displayName="Clock"), prompt="hello")
        append_started(run)
        append_completed(run)
        artifact = Artifact.objects.create(workspace=workspace, session=session, agent_run=run,
            createdBy=owner, displayName="Report", safeFilename="report.docx", sizeBytes=4,
            sha256="sha256:" + "a" * 64, storageKey="report", status="published", publishedAt=timezone.now())
        self.client.force_login(owner)
        url = f"/api/sessions/{session.id}/transcript/turn-metadata"
        response = self.client.get(url, {"sequence": ["2", "3"]})
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), {"turns": [
            {"anchorSequence": str(sequence), "startedAtMs": 2, "completedAtMs": 4, "artifacts": [{"artifactRef": f"artifact:{artifact.id}", "filename": "report.docx",
                "downloadUrl": f"/api/artifacts/{artifact.id}/download"}]}
            for sequence in [2, 3]
        ]})
        self.assertEqual(self.client.get(url, {"sequence": ["2"] * 129}).status_code, 400)
        workspace.members.remove(owner)
        self.assertEqual(self.client.get(url, {"sequence": "2"}).status_code, 404)

    def test_transcript_active_run_uses_the_frozen_session_waterline_without_loading_history(self):
        owner = User.objects.create_user(username="active-transcript@example.com", password="password")
        workspace = Workspace.objects.create(name="Active transcript", createdBy=owner)
        workspace.members.add(owner)
        model = ModelConfig.objects.create(displayName="Active transcript")
        session = create_session(workspace=workspace, owner=owner)
        run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=owner,
            modelConfig=model,
            prompt="hello",
            status="running",
        )
        append_started(run)
        self.client.force_login(owner)

        response = self.client.get(
            f"/api/sessions/{session.id}/transcript/active-agent-run",
            {"sourceHighWater": "2"},
        )

        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(
            response.json(),
            {
                "schema": "workspace.transcript.active_agent_run.v1",
                "sessionId": session.id,
                "agentRun": {
                    "agentRunId": run.id,
                    "status": "running",
                    "streamCursor": agent_run_stream.encode_stream_cursor(run.id, 2),
                },
            },
        )
        self.assertEqual(response.headers["Cache-Control"], "no-store")

        workspace.members.remove(owner)
        revoked = self.client.get(
            f"/api/sessions/{session.id}/transcript/active-agent-run",
            {"sourceHighWater": "2"},
        )
        self.assertEqual(revoked.status_code, 404)


    def test_artifact_publisher_streams_bytes_and_is_publication_idempotent(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        firstRun = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="one",
        )
        authorization = create_agent_run_authorization(firstRun)
        content = bytes([0, 159, 146, 150])

        def payload(callId="call_1", filename="report.bin", value=content):
            publicationId = (
                "pub_"
                + hashlib.sha256(
                    json.dumps([firstRun.id, callId], separators=(",", ":")).encode()
                ).hexdigest()
            )
            metadata = {
                "schema": "artifact.publication.v1",
                "publicationId": publicationId,
                "agentRunId": firstRun.id,
                "authorizationDigest": authorization.digest,
                "toolCallId": callId,
                "filename": filename,
                "sizeBytes": len(value),
                "sha256": f"sha256:{hashlib.sha256(value).hexdigest()}",
            }
            encoded = json.dumps(metadata, separators=(",", ":")).encode()
            return metadata, len(encoded).to_bytes(4, "big") + encoded + value

        metadata, body = payload()
        headers = {"HTTP_X_INTERNAL_TOKEN": "test-internal-token"}
        # A rejected upload must remain retryable and must never reach the library.
        corrupt = self.client.post(
            "/internal/artifacts/publish",
            data=body[:-1] + bytes([body[-1] ^ 1]),
            content_type="application/octet-stream",
            **headers,
        )
        self.assertEqual(corrupt.status_code, 409, corrupt.content)
        self.assertEqual(corrupt.json()["error"], "artifact_integrity_mismatch")
        self.assertEqual(Artifact.objects.get().status, "staging")
        self.assertFalse(UserLibraryObject.objects.exists())

        # Fresh publication only reads stored bytes to make the durable library copy.
        with patch.object(default_storage, "open", wraps=default_storage.open) as opened:
            first = self.client.post(
                "/internal/artifacts/publish",
                data=body,
                content_type="application/octet-stream",
                **headers,
            )
        self.assertEqual(opened.call_count, 1)
        self.assertEqual(first.status_code, 201, first.content)
        artifactRef = first.json()["artifactRef"]
        self.assertEqual(first.json()["publicationId"], metadata["publicationId"])
        self.assertNotIn("storageKey", first.json())
        self.assertNotIn("path", first.json())

        repeated = self.client.post(
            "/internal/artifacts/publish",
            data=body,
            content_type="application/octet-stream",
            **headers,
        )
        self.assertEqual(repeated.status_code, 201)
        self.assertEqual(repeated.json()["artifactRef"], artifactRef)
        status = self.client.post(
            "/internal/artifacts/status",
            data=json.dumps(
                {
                    "schema": "artifact.publication.status.v1",
                    "publicationId": metadata["publicationId"],
                    "agentRunId": firstRun.id,
                    "authorizationDigest": authorization.digest,
                    "toolCallId": "call_1",
                }
            ),
            content_type="application/json",
            **headers,
        )
        self.assertEqual(status.status_code, 200)
        self.assertEqual(status.json()["artifactRef"], artifactRef)

        duplicateMetadata, duplicateBody = payload("call_2")
        duplicate = self.client.post(
            "/internal/artifacts/publish",
            data=duplicateBody,
            content_type="application/octet-stream",
            **headers,
        )
        self.assertEqual(duplicate.status_code, 201)
        self.assertEqual(duplicate.json()["artifactRef"], artifactRef)
        self.assertNotEqual(
            duplicateMetadata["publicationId"], metadata["publicationId"]
        )
        self.assertEqual(Artifact.objects.count(), 1)
        self.assertEqual(ArtifactPublication.objects.count(), 2)

        changedMetadata, changedBody = payload("call_3", value=b"changed")
        conflict = self.client.post(
            "/internal/artifacts/publish",
            data=changedBody,
            content_type="application/octet-stream",
            **headers,
        )
        self.assertEqual(conflict.status_code, 409)
        self.assertEqual(conflict.json()["error"], "artifact_filename_conflict")
        self.assertFalse(
            ArtifactPublication.objects.filter(
                publicationId=changedMetadata["publicationId"]
            ).exists()
        )

        wrongDigest = dict(metadata, authorizationDigest=f"sha256:{'a' * 64}")
        encoded = json.dumps(wrongDigest, separators=(",", ":")).encode()
        denied = self.client.post(
            "/internal/artifacts/publish",
            data=len(encoded).to_bytes(4, "big") + encoded + content,
            content_type="application/octet-stream",
            **headers,
        )
        self.assertEqual(denied.status_code, 403)

        artifact_id = artifactRef.split(":", 1)[1]
        libraries = UserLibraryObject.objects.filter(
            owner=user,
            objectKind="savedArtifact",
        )
        self.assertEqual(libraries.count(), 1)
        library = libraries.get()
        self.assertEqual(
            library.sha256, f"sha256:{hashlib.sha256(content).hexdigest()}"
        )
        self.assertEqual(library.status, "ready")
        self.assertTrue(default_storage.exists(library.storageKey))
        with default_storage.open(library.storageKey, "rb") as saved:
            self.assertEqual(saved.read(), content)
        self.assertTrue(library.storageKey.startswith(f"users/{user.id}/library/"))
        self.assertTrue(
            UserLibraryLink.objects.filter(
                libraryObject=library,
                sourceKind="artifact",
                sourceRefId=artifact_id,
            ).exists()
        )
        self.assertFalse(
            SessionAssetLink.objects.filter(userLibraryObject=library).exists()
        )

    def test_artifact_publisher_reuses_existing_library_copy_on_replay(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="fake-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="replay",
        )
        authorization = create_agent_run_authorization(agent_run)
        content = bytes([0, 159, 146, 150])
        publicationId = "pub_" + hashlib.sha256(
            json.dumps([agent_run.id, "call_1"], separators=(",", ":")).encode()
        ).hexdigest()
        metadata = {
            "schema": "artifact.publication.v1",
            "publicationId": publicationId,
            "agentRunId": agent_run.id,
            "authorizationDigest": authorization.digest,
            "toolCallId": "call_1",
            "filename": "report.bin",
            "sizeBytes": len(content),
            "sha256": f"sha256:{hashlib.sha256(content).hexdigest()}",
        }
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        body = len(encoded).to_bytes(4, "big") + encoded + content
        headers = {"HTTP_X_INTERNAL_TOKEN": "test-internal-token"}
        first = self.client.post(
            "/internal/artifacts/publish",
            data=body,
            content_type="application/octet-stream",
            **headers,
        )
        self.assertEqual(first.status_code, 201, first.content)
        library_ids = list(
            UserLibraryObject.objects.filter(
                owner=user,
                objectKind="savedArtifact",
            ).values_list("id", flat=True)
        )
        self.assertEqual(len(library_ids), 1)
        replay = self.client.post(
            "/internal/artifacts/publish",
            data=body,
            content_type="application/octet-stream",
            **headers,
        )
        self.assertEqual(replay.status_code, 201, replay.content)
        self.assertEqual(
            UserLibraryObject.objects.filter(
                owner=user,
                objectKind="savedArtifact",
            ).count(),
            1,
        )
        self.assertEqual(
            list(
                UserLibraryObject.objects.filter(
                    owner=user,
                    objectKind="savedArtifact",
                ).values_list("id", flat=True)
            ),
            library_ids,
        )
        status = self.client.post(
            "/internal/artifacts/status",
            data=json.dumps(
                {
                    "schema": "artifact.publication.status.v1",
                    "publicationId": publicationId,
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                    "toolCallId": "call_1",
                }
            ),
            content_type="application/json",
            **headers,
        )
        self.assertEqual(status.status_code, 200)

    def test_artifact_publisher_leaves_transient_storage_failure_retryable(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)
        content = b"report"
        publicationId = (
            "pub_"
            + hashlib.sha256(
                json.dumps([agent_run.id, "call_1"], separators=(",", ":")).encode()
            ).hexdigest()
        )
        metadata = {
            "schema": "artifact.publication.v1",
            "publicationId": publicationId,
            "agentRunId": agent_run.id,
            "authorizationDigest": authorization.digest,
            "toolCallId": "call_1",
            "filename": "report.bin",
            "sizeBytes": len(content),
            "sha256": f"sha256:{hashlib.sha256(content).hexdigest()}",
        }
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        body = len(encoded).to_bytes(4, "big") + encoded + content
        with patch(
            "app_core.artifact_publish.default_storage.save",
            side_effect=OSError("disk failed"),
        ):
            response = self.client.post(
                "/internal/artifacts/publish",
                data=body,
                content_type="application/octet-stream",
                HTTP_X_INTERNAL_TOKEN="test-internal-token",
            )
        self.assertEqual(response.status_code, 500)
        artifact = Artifact.objects.get(agent_run=agent_run)
        self.assertEqual(artifact.status, "staging")
        self.assertEqual(
            ArtifactPublication.objects.get(publicationId=publicationId).status,
            "staging",
        )
        self.assertEqual(
            UserLibraryObject.objects.filter(
                owner=user, objectKind="savedArtifact"
            ).count(),
            0,
        )
        retry = self.client.post(
            "/internal/artifacts/publish",
            data=body,
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(retry.status_code, 201, retry.content)
        artifact.refresh_from_db()
        self.assertEqual(artifact.status, "published")
        self.assertEqual(
            UserLibraryObject.objects.filter(
                owner=user, objectKind="savedArtifact"
            ).count(),
            1,
        )

    def test_artifact_publish_leaves_publication_staging_when_library_copy_fails(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(id="fake-model", displayName="Fake")
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="library-fail",
        )
        authorization = create_agent_run_authorization(agent_run)
        content = b"library-report"
        publicationId = (
            "pub_"
            + hashlib.sha256(
                json.dumps([agent_run.id, "call_1"], separators=(",", ":")).encode()
            ).hexdigest()
        )
        metadata = {
            "schema": "artifact.publication.v1",
            "publicationId": publicationId,
            "agentRunId": agent_run.id,
            "authorizationDigest": authorization.digest,
            "toolCallId": "call_1",
            "filename": "report.bin",
            "sizeBytes": len(content),
            "sha256": f"sha256:{hashlib.sha256(content).hexdigest()}",
        }
        encoded = json.dumps(metadata, separators=(",", ":")).encode()
        body = len(encoded).to_bytes(4, "big") + encoded + content
        save_calls = {"count": 0}
        original_save = default_storage.save

        def failing_second_save(name, *args, **kwargs):
            save_calls["count"] += 1
            if save_calls["count"] == 2:
                raise OSError("library disk failed")
            return original_save(name, *args, **kwargs)

        with patch(
            "app_core.artifact_publish.default_storage.save",
            side_effect=failing_second_save,
        ):
            response = self.client.post(
                "/internal/artifacts/publish",
                data=body,
                content_type="application/octet-stream",
                HTTP_X_INTERNAL_TOKEN="test-internal-token",
            )
        self.assertEqual(response.status_code, 500)
        artifact = Artifact.objects.get(agent_run=agent_run)
        self.assertEqual(artifact.status, "staging")
        self.assertEqual(
            ArtifactPublication.objects.get(publicationId=publicationId).status,
            "staging",
        )
        self.assertEqual(
            UserLibraryObject.objects.filter(
                owner=user, objectKind="savedArtifact"
            ).count(),
            0,
        )
        retry = self.client.post(
            "/internal/artifacts/publish",
            data=body,
            content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )
        self.assertEqual(retry.status_code, 201, retry.content)
        artifact.refresh_from_db()
        self.assertEqual(artifact.status, "published")
        self.assertEqual(
            ArtifactPublication.objects.get(publicationId=publicationId).status,
            "published",
        )
        self.assertEqual(
            UserLibraryObject.objects.filter(
                owner=user, objectKind="savedArtifact"
            ).count(),
            1,
        )

    def test_agent_run_authorization_is_immutable_and_strict(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(
            workspace=workspace,
            owner=user,
            agent_id="banana-agent",
        )
        advancedRun = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="previous",
        )
        session.workspaceGeneration = 7
        session.workspaceStorageKey = "workspaces/ws_1/sessions/sess_1/snapshot"
        session.workspaceSnapshotSha256 = f"sha256:{'c' * 64}"
        session.workspaceSnapshotSizeBytes = 13
        session.workspaceExpandedSizeBytes = 7
        session.workspaceFileCount = 1
        session.workspaceLastAdvancedAgentRun = advancedRun
        session.save()
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)
        self.assertEqual(authorization.payload["agentId"], "banana-agent")
        self.assertEqual(
            authorization.payload["sessionWorkspace"],
            {
                "generation": 7,
                "snapshotSha256": f"sha256:{'c' * 64}",
                "snapshotSizeBytes": 13,
                "expandedSizeBytes": 7,
                "fileCount": 1,
            },
        )
        self.assertNotIn("workspaceStorageKey", authorization.payload)

        authorization.payload["storageKey"] = "private/source.pdf"
        with self.assertRaisesRegex(ValueError, "immutable"):
            authorization.save()
        with self.assertRaisesRegex(ValueError, "unknown=.*storageKey"):
            validate_agent_run_authorization_payload(authorization.payload)

    def test_agent_run_authorization_digest_is_stable(self):
        payload = json.loads(
            (Path(__file__).resolve().parents[3]
             / "tests/workspace/fixtures/agent_run_authorization/v1/valid.json").read_text(encoding="utf-8")
        )
        digest = authorization_digest(payload)
        self.assertEqual(
            digest,
            "sha256:bd93e8ba466c0d9851e805dd2a8c8a5962b351065c4d982ae961ea0cdb0a6a9f",
        )
        self.assertEqual(
            authorization_digest(dict(reversed(list(payload.items())))), digest
        )
        self.assertEqual(
            authorization_signature(payload, "test-key"),
            "hmac-sha256:86d800a4e2517f1b169894646f9a7bb3297770235a7a3a0cbceffe1897969dd5",
        )
        unknownPayload = dict(payload, schema="banana")
        with self.assertRaisesRegex(ValueError, "agent_run_authorization_schema_mismatch"):
            validate_agent_run_authorization_payload(unknownPayload)

    @patch("app_core.http.workspaces.request_workspace_hook_catalog")
    @patch("app_core.http.workspaces.request_workspace_mcp_catalog")
    def test_workspace_plugin_enablement_freezes_only_the_next_agent_run(
        self, mcp_catalog, hook_catalog
    ):
        mcp_catalog.return_value = {
            "schema": "workspace.mcp.catalog.result.v1",
            "plugins": [{"pluginName": "banana", "servers": []}],
        }
        hook_catalog.return_value = {
            "schema": "workspace.hook.catalog.result.v1",
            "plugins": [{"pluginName": "banana", "hooks": []}],
        }
        user = User.objects.create_user(
            username="plugin-member@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Plugin workspace", createdBy=user)
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=user,
            role="owner",
        )
        model = ModelConfig.objects.create(
            id="plugin-model", displayName="Plugin Model"
        )
        session = create_session(workspace=workspace, owner=user)
        self.client.force_login(user)

        listed = self.client.get(f"/api/workspaces/{workspace.id}/plugins")
        self.assertEqual(listed.status_code, 200, listed.content)
        self.assertEqual(
            [(item["name"], item["enabled"]) for item in listed.json()["plugins"]],
            [("banana", False)],
        )
        banana_listing = next(
            item for item in listed.json()["plugins"] if item["name"] == "banana"
        )
        self.assertEqual(banana_listing["displayName"], "Banana Extension")
        self.assertEqual(banana_listing["capabilities"], ["Synthetic capability"])
        enabled = self.client.patch(
            f"/api/workspaces/{workspace.id}/plugins/banana",
            data=json.dumps({"enabled": True}),
            content_type="application/json",
        )
        self.assertEqual(enabled.status_code, 200, enabled.content)
        self.assertTrue(enabled.json()["plugin"]["enabled"])
        self.assertTrue(
            WorkspacePluginEnablement.objects.filter(
                workspace=workspace, pluginName="banana"
            ).exists()
        )

        first_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="create a report",
        )
        first = create_agent_run_authorization(first_run)
        self.assertEqual(
            [package["name"] for package in first.payload["pluginActivation"]["packages"]],
            ["banana"],
        )

        banana_package = next(
            package
            for package in load_plugin_catalog()["packages"]
            if package["name"] == "banana"
        )
        self.assertEqual(
            first.payload["pluginActivation"]["digest"],
            activation_digest([banana_package]),
        )

        disabled = self.client.patch(
            f"/api/workspaces/{workspace.id}/plugins/banana",
            data=json.dumps({"enabled": False}),
            content_type="application/json",
        )
        self.assertEqual(disabled.status_code, 200, disabled.content)
        second_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="plain answer",
        )
        second = create_agent_run_authorization(second_run)
        self.assertEqual(second.payload["pluginActivation"]["packages"], [])
        self.assertEqual(
            [package["name"] for package in first.payload["pluginActivation"]["packages"]],
            ["banana"],
        )

    @patch("app_core.http.workspaces.request_workspace_hook_catalog")
    @patch("app_core.http.workspaces.request_workspace_mcp_catalog")
    def test_workspace_member_can_list_but_cannot_toggle_plugins(
        self, mcp_catalog, hook_catalog
    ):
        owner = User.objects.create_user(
            username="plugin-owner@example.com", password="password"
        )
        member = User.objects.create_user(
            username="plugin-reader@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Plugin workspace", createdBy=owner)
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=owner,
            role="owner",
        )
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=member,
            role="member",
        )
        mcp_catalog.return_value = {
            "schema": "workspace.mcp.catalog.result.v1",
            "plugins": [
                {"pluginName": package["name"], "servers": []}
                for package in load_plugin_catalog()["packages"]
            ],
        }
        hook_catalog.return_value = {
            "schema": "workspace.hook.catalog.result.v1",
            "plugins": [
                {"pluginName": package["name"], "hooks": []}
                for package in load_plugin_catalog()["packages"]
            ],
        }
        self.client.force_login(member)

        listed = self.client.get(f"/api/workspaces/{workspace.id}/plugins")
        changed = self.client.patch(
            f"/api/workspaces/{workspace.id}/plugins/documents",
            data=json.dumps({"enabled": True}),
            content_type="application/json",
        )

        self.assertEqual(listed.status_code, 200, listed.content)
        self.assertEqual(changed.status_code, 404, changed.content)
        self.assertFalse(
            WorkspacePluginEnablement.objects.filter(workspace=workspace).exists()
        )

    @patch("app_core.http.workspaces.load_plugin_bearer_credential_refs")
    @patch("app_core.http.workspaces.load_plugin_catalog")
    @patch("app_core.http.workspaces.request_workspace_hook_catalog")
    @patch("app_core.http.workspaces.request_workspace_mcp_catalog")
    def test_workspace_plugin_projects_mcp_and_bearer_state_without_secret(
        self, mcp_catalog, hook_catalog, plugin_catalog, credential_refs
    ):
        user = User.objects.create_superuser(
            username="mcp-admin@example.com",
            email="mcp-admin@example.com",
            password="password",
        )
        workspace = Workspace.objects.create(name="MCP workspace", createdBy=user)
        workspace.members.add(user)
        package = {
            "name": "banana",
            "version": "1.0.0",
            "packageDigest": f"sha256:{'a' * 64}",
            "skills": [],
            "cli": [],
            "mcpServers": [
                {"path": "mcp.json", "digest": f"sha256:{'b' * 64}"}
            ],
            "hooks": [],
        }
        plugin_catalog.return_value = {
            "schema": "plugin_activation_snapshot_v1",
            "digest": activation_digest([package]),
            "packages": [package],
        }
        credential_refs.return_value = ["banana-token"]
        McpBearerCredential.objects.create(
            plugin_name="banana",
            credential_ref="banana-token",
            display_name="banana",
            encrypted_secret=encrypt_credential_secret("test-secret"),
            created_by=user,
            updated_by=user,
        )
        mcp_catalog.return_value = {
            "schema": "workspace.mcp.catalog.result.v1",
            "plugins": [
                {
                    "pluginName": "banana",
                    "servers": [
                        {
                            "id": "banana-source",
                            "modelContractDigest": f"sha256:{'c' * 64}",
                            "transport": {
                                "type": "streamableHttp",
                                "endpoint": "https://banana.invalid/mcp",
                            },
                            "auth": {
                                "type": "bearer",
                                "credentialRef": "banana-token",
                            },
                            "startupTimeoutMs": 15000,
                            "toolTimeoutMs": 60000,
                            "tools": [
                                {
                                    "sourceName": "search_article",
                                    "name": "banana_search",
                                    "description": "Search bananas.",
                                    "inputSchema": {"type": "object"},
                                    "concurrencySafe": True,
                                    "scopes": ["banana:read"],
                                }
                            ],
                        }
                    ],
                },
            ],
        }
        hook_catalog.return_value = {
            "schema": "workspace.hook.catalog.result.v1",
            "plugins": [{"pluginName": "banana", "hooks": []}],
        }
        self.client.force_login(user)

        response = self.client.get(f"/api/workspaces/{workspace.id}/plugins/banana")

        self.assertEqual(response.status_code, 200, response.content)
        banana_plugin = response.json()["plugin"]
        self.assertEqual(banana_plugin["name"], "banana")
        self.assertTrue(banana_plugin["mcpServers"][0]["auth"]["credentialConfigured"])
        self.assertEqual(
            banana_plugin["mcpServers"][0]["tools"][0]["name"],
            "banana_search",
        )
        self.assertNotIn("test-secret", response.content.decode("utf-8"))

    @patch("app_core.http.workspaces.request_workspace_hook_catalog")
    @patch("app_core.http.workspaces.request_workspace_mcp_catalog")
    @patch("app_core.http.workspaces.load_plugin_interfaces")
    @patch("app_core.http.workspaces.load_plugin_catalog")
    def test_workspace_plugin_projects_safe_hook_metadata(
        self, plugin_catalog, plugin_interfaces, mcp_catalog, hook_catalog
    ):
        user = User.objects.create_user(
            username="hook-owner@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Hook workspace", createdBy=user)
        workspace.members.add(user)
        package = {
            "name": "wiki",
            "version": "1.0.0",
            "packageDigest": f"sha256:{'a' * 64}",
            "skills": [],
            "cli": [],
            "mcpServers": [],
            "hooks": [
                {"path": "hooks/hooks.json", "digest": f"sha256:{'b' * 64}"}
            ],
        }
        plugin_catalog.return_value = {
            "schema": "plugin_activation_snapshot_v1",
            "digest": activation_digest([package]),
            "packages": [package],
        }
        plugin_interfaces.return_value = {
            "wiki": {"displayName": "Wiki", "shortDescription": "", "capabilities": []}
        }
        mcp_catalog.return_value = {
            "schema": "workspace.mcp.catalog.result.v1",
            "plugins": [{"pluginName": "wiki", "servers": []}],
        }
        hook_catalog.return_value = {
            "schema": "workspace.hook.catalog.result.v1",
            "plugins": [
                {
                    "pluginName": "wiki",
                    "hooks": [
                        {
                            "id": "guard_write",
                            "event": "PreToolUse",
                            "matcher": "write",
                            "timeoutMs": 5000,
                        }
                    ],
                }
            ],
        }
        self.client.force_login(user)

        response = self.client.get(f"/api/workspaces/{workspace.id}/plugins/wiki")

        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["plugin"]["hooks"], hook_catalog.return_value["plugins"][0]["hooks"])
        self.assertNotIn("program", response.content.decode("utf-8"))
        self.assertNotIn("args", response.content.decode("utf-8"))

    def test_session_context_usage_projects_latest_main_request_and_live_compaction(self):
        user = User.objects.create_user(
            username="context-member@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Context workspace", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="context-model",
            displayName="Context Model",
            contextTokens=200000,
            maxOutputTokens=8192,
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="compare authorities",
            status="running",
        )
        main_breakdown = {
            "systemPromptTokens": 2600,
            "systemToolTokens": 18900,
            "mcpToolTokens": 5700,
            "skillsTokens": 2000,
            "messageTokens": 27300,
            "mcpTools": [
                {
                    "providerId": "mcp:banana:banana-source",
                    "name": "banana_search",
                    "tokens": 5700,
                }
            ],
        }
        append_session_records(
            agent_run,
            [
                session_record(
                    agent_run,
                    1,
                    "model_request_started",
                    {
                        "purpose": "main",
                        "contextTokenEstimate": 56500,
                        "contextTokenBreakdown": main_breakdown,
                    },
                ),
                session_record(
                    agent_run,
                    2,
                    "model_request_started",
                    {
                        "purpose": "compaction",
                        "contextTokenEstimate": 100,
                        "contextTokenBreakdown": {
                            "systemPromptTokens": 100,
                            "systemToolTokens": 0,
                            "mcpToolTokens": 0,
                            "skillsTokens": 0,
                            "messageTokens": 0,
                            "mcpTools": [],
                        },
                    },
                ),
            ],
        )
        self.client.force_login(user)

        response = self.client.get(f"/api/sessions/{session.id}/context-usage")

        self.assertEqual(response.status_code, 200, response.content)
        usage = response.json()["contextUsage"]
        self.assertEqual(usage["usedTokens"], 89268)
        self.assertEqual(usage["breakdown"]["messageTokens"], 27300)
        self.assertEqual(usage["breakdown"]["mcpTools"], main_breakdown["mcpTools"])
        self.assertTrue(usage["isCompacting"])

        agent_run.status = "completed"
        agent_run.transitionReason = "agent_run_completed"
        agent_run.completedAt = timezone.now()
        agent_run.save()
        settled = self.client.get(f"/api/sessions/{session.id}/context-usage")
        self.assertFalse(settled.json()["contextUsage"]["isCompacting"])

    def test_workspace_skills_use_runtime_catalog_and_do_not_expose_plugin_paths(self):
        user = User.objects.create_user(
            username="skill-member@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Skill workspace", createdBy=user)
        workspace.members.add(user)
        WorkspacePluginEnablement.objects.create(
            workspace=workspace, pluginName="banana"
        )
        self.client.force_login(user)
        skill = {
            "skillId": "plugin-banana-0:banana",
            "name": "banana",
            "description": "Synthetic extension fixture.",
            "enabled": True,
            "allowImplicitInvocation": True,
            "allowedTools": ["read", "bash"],
        }
        with patch(
            "app_core.http.workspaces.request_workspace_skill_catalog",
            return_value={
                "schema": "workspace.skill.catalog.result.v1",
                "skills": [skill],
            },
        ) as catalog:
            listed = self.client.get(f"/api/workspaces/{workspace.id}/skills")

        self.assertEqual(listed.status_code, 200, listed.content)
        self.assertEqual(listed.json()["skills"], [skill])
        self.assertNotIn("skillMdPath", listed.content.decode("utf-8"))
        self.assertEqual(
            catalog.call_args.args[0]["packages"][0]["name"], "banana"
        )

        with patch(
            "app_core.http.workspaces.request_workspace_skill_detail",
            return_value={
                "schema": "workspace.skill.detail.result.v1",
                "skill": skill,
                "content": "# Documents\n\nUse the document tools.",
            },
        ) as detail:
            response = self.client.get(
                f"/api/workspaces/{workspace.id}/skills/{skill['skillId']}"
            )

        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["content"], "# Documents\n\nUse the document tools.")
        self.assertEqual(detail.call_args.args[1], skill["skillId"])

        with patch(
            "app_core.http.workspaces.request_workspace_skill_detail",
            side_effect=LookupError("skill_not_found"),
        ):
            missing = self.client.get(
                f"/api/workspaces/{workspace.id}/skills/plugin-banana-0:banana"
            )
        self.assertEqual(missing.status_code, 404, missing.content)
        self.assertEqual(missing.json(), {"error": "skill_not_found"})

    def test_plugin_activation_rejects_non_nfc_resource_path(self):
        packages = [
            {
                "name": "banana",
                "version": "1.0.0",
                "packageDigest": f"sha256:{'a' * 64}",
                "skills": [
                    {
                        "path": "skills/cafe\u0301/SKILL.md",
                        "digest": f"sha256:{'b' * 64}",
                    }
                ],
                "cli": [],
                "mcpServers": [],
                "hooks": [],
            }
        ]
        with self.assertRaisesRegex(ValueError, "canonical relative POSIX"):
            validate_plugin_activation(
                {
                    "schema": "plugin_activation_snapshot_v1",
                    "digest": activation_digest(packages),
                    "packages": packages,
                }
            )


    def test_session_events_are_append_only_and_idempotent(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )

        append_started(agent_run)
        append_started(agent_run)
        self.assertEqual(SessionEvent.objects.filter(agent_run=agent_run).count(), 2)
        event = SessionEvent.objects.filter(agent_run=agent_run).first()
        event.payload["type"] = "banana"
        with self.assertRaisesRegex(ValueError, "append-only"):
            event.save()





    def test_api_does_not_expose_session_log_append(self):
        user = User.objects.create_user(
            username="admin@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="默认工作区", createdBy=user)
        workspace.members.add(user)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=user)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=user,
            modelConfig=model,
            prompt="hello",
        )
        response = self.client.post(
            "/internal/session-events", data="{}", content_type="application/json"
        )
        self.assertEqual(response.status_code, 404)
        self.assertEqual(SessionEvent.objects.filter(agent_run=agent_run).count(), 0)


class McpBearerCredentialAcceptanceTests(TestCase):
    def setUp(self):
        self.admin = User.objects.create_superuser(
            username="admin@example.com",
            email="admin@example.com",
            password="password",
        )
        self.member = User.objects.create_user(
            username="member@example.com",
            password="password",
        )
        self.workspace = Workspace.objects.create(
            name="Default", createdBy=self.member
        )
        self.workspace.members.add(self.member)
        WorkspacePluginEnablement.objects.create(
            workspace=self.workspace,
            pluginName="banana",
        )
        self.model = ModelConfig.objects.create(id="fake-model", displayName="Fake")
        self.session = create_session(
            workspace=self.workspace,
            owner=self.member,
        )
        self.agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="hello",
        )
        self.authorization = create_agent_run_authorization(self.agent_run)

    def resolve(self, credential_ref: str):
        return self.client.post(
            "/internal/mcp-bearer-credentials/resolve",
            data=json.dumps(
                {
                    "schema": "runtime.mcp_bearer_credential.resolve.v1",
                    "agentRunId": self.agent_run.id,
                    "authorizationRef": self.authorization.id,
                    "authorizationDigest": self.authorization.digest,
                    "pluginName": "banana",
                    "credentialRef": credential_ref,
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN="test-internal-token",
        )

    def test_superuser_manages_encrypted_bearer_bound_to_frozen_plugin(self):
        self.client.force_login(self.member)
        self.assertEqual(
            self.client.get("/api/admin/mcp-bearer-credentials").status_code,
            403,
        )

        self.client.force_login(self.admin)
        created = self.client.post(
            "/api/admin/mcp-bearer-credentials",
            data=json.dumps(
                {
                    "pluginName": "banana",
                    "credentialRef": "banana-source",
                    "displayName": "Banana source",
                    "secret": "test-secret",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        credential_id = created.json()["credential"]["id"]
        stored = McpBearerCredential.objects.get(id=credential_id)
        self.assertNotEqual(stored.encrypted_secret, "test-secret")
        self.assertNotIn("test-secret", json.dumps(created.json()))
        listed = self.client.get("/api/admin/mcp-bearer-credentials")
        self.assertEqual(listed.status_code, 200)
        self.assertNotIn("test-secret", json.dumps(listed.json()))

        resolved = self.resolve("banana-source")
        self.assertEqual(resolved.status_code, 200)
        self.assertEqual(
            resolved.json(),
            {
                "schema": "runtime.mcp_bearer_credential.resolved.v1",
                "token": "test-secret",
            },
        )

        rotated = self.client.post(
            f"/api/admin/mcp-bearer-credentials/{credential_id}/rotate",
            data=json.dumps({"secret": "replacement-secret"}),
            content_type="application/json",
        )
        self.assertEqual(rotated.status_code, 200)
        self.assertEqual(rotated.json()["credential"]["version"], 2)
        self.assertEqual(
            self.resolve("banana-source").json()["token"], "replacement-secret"
        )

        deleted = self.client.delete(
            f"/api/admin/mcp-bearer-credentials/{credential_id}"
        )
        self.assertEqual(deleted.status_code, 204)
        self.assertEqual(self.resolve("banana-source").status_code, 404)
        self.assertEqual(
            list(
                McpCredentialAuditEvent.objects.order_by("created_at").values_list(
                    "action", flat=True
                )
            ),
            ["created", "resolved", "rotated", "resolved", "deleted"],
        )

    def test_resolution_rejects_unfrozen_plugin_and_missing_internal_auth(self):
        self.client.force_login(self.admin)
        invalid = self.client.post(
            "/api/admin/mcp-bearer-credentials",
            data=json.dumps(
                {
                    "pluginName": "banana",
                    "credentialRef": "banana-source",
                    "displayName": "Banana source",
                    "secret": "banana token",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(invalid.status_code, 400)
        created = self.client.post(
            "/api/admin/mcp-bearer-credentials",
            data=json.dumps(
                {
                    "pluginName": "banana",
                    "credentialRef": "banana-source",
                    "displayName": "Banana source",
                    "secret": "test-secret",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        unauthenticated = self.client.post(
            "/internal/mcp-bearer-credentials/resolve",
            data="{}",
            content_type="application/json",
        )
        self.assertEqual(unauthenticated.status_code, 401)

        WorkspacePluginEnablement.objects.all().delete()
        other_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="hello again",
        )
        other_authorization = create_agent_run_authorization(other_run)
        self.agent_run = other_run
        self.authorization = other_authorization
        rejected = self.resolve("banana-source")
        self.assertEqual(rejected.status_code, 409)
        self.assertEqual(
            rejected.json(), {"error": "mcp_credential_authorization_invalid"}
        )




class ModelQuotaAdmissionTests(TransactionTestCase):
    def test_four_attempts_enter_while_fifth_waits_for_a_slot(self):
        from contextlib import ExitStack

        self.domain.maxConcurrent = 4
        self.domain.save(update_fields=["maxConcurrent", "updatedAt"])
        entered = threading.Event()
        cancel = threading.Event()

        def fifth():
            with model_attempt(self.model, cancel):
                entered.set()

        with ExitStack() as held, ThreadPoolExecutor(max_workers=1) as pool:
            for _ in range(3):
                held.enter_context(model_attempt(self.model))
            try:
                with model_attempt(self.model):
                    result = pool.submit(fifth)
                    self.assertFalse(entered.wait(0.15))
                self.assertTrue(entered.wait(3))
                result.result(timeout=3)
            finally:
                cancel.set()

    def setUp(self):
        self.admin = User.objects.create_superuser(
            username="quota@example.com", email="quota@example.com", password="password"
        )
        self.domain = ModelQuotaDomain.objects.create(
            id="shared-account", maxConcurrent=1, enabled=True
        )
        self.provider = ModelProvider.objects.create(
            displayName="test", api="openai-completions", apiBase="https://models.example.com/v1"
        )
        ProviderCredential.objects.create(
            provider=self.provider,
            quotaDomain=self.domain,
            displayName="test",
            encryptedSecret=encrypt_credential_secret("test-secret"),
            createdBy=self.admin,
            updatedBy=self.admin,
        )
        self.model = ModelConfig.objects.create(
            displayName="test", provider=self.provider, modelName="test",
            resolvedApi=self.provider.api, resolvedApiBase=self.provider.apiBase,
        )

    def test_two_connections_share_one_slot_until_full_attempt_exits(self):
        entered = threading.Event()
        release = threading.Event()
        second_entered = threading.Event()

        def first():
            with model_attempt(self.model):
                entered.set()
                self.assertTrue(release.wait(3))

        def second():
            with model_attempt(self.model):
                second_entered.set()

        with ThreadPoolExecutor(max_workers=2) as pool:
            first_result = pool.submit(first)
            self.assertTrue(entered.wait(3))
            second_result = pool.submit(second)
            self.assertFalse(second_entered.wait(0.15))
            release.set()
            first_result.result(timeout=3)
            second_result.result(timeout=3)
        self.assertTrue(second_entered.is_set())

    def test_sdk_http_attempt_uses_admitted_fake_transport(self):
        from .model_adapter import test_model_config

        provider_called = threading.Event()

        def fake_provider(request):
            provider_called.set()
            self.assertEqual(request.url.path, "/v1/chat/completions")
            return httpx.Response(200, json={
                "id": "chatcmpl-test", "object": "chat.completion", "created": 1,
                "model": "test", "choices": [{
                    "index": 0, "message": {"role": "assistant", "content": "hello"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            })

        transport = httpx.MockTransport(fake_provider)

        def client_for_attempt(_model, _request_body):
            return OpenAI(
                api_key="test", base_url="https://models.example.com/v1",
                http_client=httpx.Client(transport=transport), max_retries=0,
            )

        with patch(
            "app_core.model_adapter.openai_completions.open_ai_completions_client",
            side_effect=client_for_attempt,
        ):
            self.assertEqual(test_model_config(self.model)["text"], "hello")
        self.assertTrue(provider_called.is_set())

    def test_cancelled_queued_sync_request_never_enters_provider_attempt(self):
        entered = threading.Event()
        cancel = threading.Event()

        def queued():
            with self.assertRaisesRegex(ModelProviderError, "model_run_cancelled"):
                with model_attempt(self.model, cancel):
                    entered.set()

        with model_attempt(self.model), ThreadPoolExecutor(max_workers=1) as pool:
            result = pool.submit(queued)
            self.assertFalse(entered.wait(0.15))
            cancel.set()
            result.result(timeout=3)
        self.assertFalse(entered.is_set())

    def test_cancelled_waiter_does_not_keep_a_slot(self):
        async def exercise():
            async with async_model_attempt(self.model):
                waiting = asyncio.create_task(self._waiting_attempt())
                await asyncio.sleep(0.15)
                self.assertFalse(waiting.done())
                waiting.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await waiting
            await asyncio.wait_for(self._waiting_attempt(), 2)

        async_to_sync(exercise)()

    def test_retry_after_cooldown_is_shared_after_slot_release(self):
        now = [1000000]
        with patch("app_core.model_adapter.quota._database_now_ms", side_effect=lambda: now[0]):
            with self.assertRaises(ModelProviderError):
                with model_attempt(self.model):
                    raise ModelProviderError("provider_rate_limited", 429, "2")
            self.domain.refresh_from_db()
            self.assertEqual(self.domain.cooldownUntilMs, 1002000)
            entered = threading.Event()

            def next_attempt():
                with model_attempt(self.model):
                    entered.set()

            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(next_attempt)
                self.assertFalse(entered.wait(0.15))
                now[0] = 1002000
                future.result(timeout=3)
            self.assertTrue(entered.is_set())

    def test_operator_can_bind_an_explicit_domain(self):
        credential = ProviderCredential.objects.get(provider=self.provider)
        credential.quotaDomain = None
        credential.save(update_fields=["quotaDomain"])
        with self.assertRaisesRegex(ModelProviderError, "model_quota_domain_required"):
            with model_attempt(self.model):
                self.fail("unconfigured provider entered")
        call_command(
            "configure_model_quota",
            domain_id="shared-account",
            max_concurrent=1,
            provider_id=self.provider.id,
            enable=True,
            stdout=io.StringIO(),
        )
        with model_attempt(self.model):
            pass

    async def _waiting_attempt(self):
        async with async_model_attempt(self.model):
            return True


class ModelAdminAcceptanceTests(TestCase):
    def setUp(self):
        self.admin = User.objects.create_superuser(
            username="admin@example.com", email="admin@example.com", password="password"
        )
        self.member = User.objects.create_user(
            username="member@example.com", password="password"
        )
        self.deepseekProvider = ModelProvider.objects.create(
            displayName="DeepSeek",
            api="openai-completions",
            apiBase="https://api.deepseek.com",
        )
        self.quotaDomain = ModelQuotaDomain.objects.create(
            id="deepseek-test", maxConcurrent=2, enabled=True
        )
        self.deepseekCredential = ProviderCredential.objects.create(
            provider=self.deepseekProvider,
            quotaDomain=self.quotaDomain,
            displayName="DeepSeek test",
            encryptedSecret=encrypt_credential_secret("test-provider-secret"),
            createdBy=self.admin,
            updatedBy=self.admin,
        )

    def create_model(self, **values):
        provider = values.pop("provider", self.deepseekProvider)
        return ModelConfig.objects.create(
            displayName=values.pop("displayName", "DeepSeek"),
            provider=provider,
            modelName=values.pop("modelName", "deepseek-chat"),
            resolvedApi=values.pop("resolvedApi", provider.api),
            resolvedApiBase=values.pop("resolvedApiBase", provider.apiBase),
            **values,
        )

    def test_unconfigured_quota_domain_rejects_before_provider_attempt(self):
        from .model_adapter import test_model_config

        model = self.create_model()
        self.deepseekCredential.quotaDomain = None
        self.deepseekCredential.save(update_fields=["quotaDomain"])
        with patch(
            "app_core.model_adapter.openai_completions.open_ai_completions_client",
            side_effect=AssertionError("provider was called before quota configuration"),
        ):
            with self.assertRaises(ModelProviderError) as failure:
                test_model_config(model)
        self.assertEqual(failure.exception.reasonType, "model_quota_domain_required")

    def test_provider_retry_after_updates_shared_quota_cooldown(self):
        from .model_adapter import test_model_config

        model = self.create_model()
        request = httpx.Request("POST", "https://api.deepseek.com/chat/completions")
        error = RateLimitError(
            "synthetic rate limit",
            response=httpx.Response(429, headers={"Retry-After": "3"}, request=request),
            body=None,
        )
        client = Mock()
        client.chat.completions.create.side_effect = error
        with patch("app_core.model_adapter.quota._database_now_ms", return_value=1000000), patch(
            "app_core.model_adapter.openai_completions.open_ai_completions_client",
            return_value=client,
        ):
            with self.assertRaises(ModelProviderError) as failure:
                test_model_config(model)
        self.assertEqual(failure.exception.reasonType, "provider_rate_limited")
        self.quotaDomain.refresh_from_db()
        self.assertEqual(self.quotaDomain.cooldownUntilMs, 1003000)

    def test_provider_first_configuration_creates_no_preset_models(self):
        self.client.force_login(self.admin)
        created = self.client.post(
            "/api/admin/model-providers",
            data=json.dumps(
                {
                    "displayName": "Kimi",
                    "api": "openai-completions",
                    "apiBase": "https://api.moonshot.cn/v1",
                    "secret": "test-provider-secret",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        provider_id = created.json()["provider"]["id"]
        self.assertEqual(ModelConfig.objects.filter(provider_id=provider_id).count(), 0)
        rotated = self.client.post(
            f"/api/admin/model-providers/{provider_id}/credential/rotate",
            data=json.dumps({"secret": "replacement-provider-secret"}),
            content_type="application/json",
        )
        self.assertEqual(rotated.status_code, 200)
        self.assertEqual(rotated.json()["provider"]["credentialVersion"], 2)
        model = self.client.post(
            "/api/admin/models",
            data=json.dumps(
                {
                    "providerId": provider_id,
                    "displayName": "Kimi K3",
                    "modelName": "kimi-k3",
                    "apiOverride": None,
                    "contextTokens": 200000,
                    "maxOutputTokens": 32768,
                    "enabled": True,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(model.status_code, 201)
        self.assertEqual(model.json()["model"]["providerId"], provider_id)

    @patch("app_core.http.model_management.request_model_catalog")
    def test_runtime_catalog_templates_create_frozen_provider_models(
        self, request_model_catalog
    ):
        request_model_catalog.return_value = {
            "schema": "centaeris.model_catalog.v1",
            "providers": [
                {
                    "catalogId": "banana",
                    "displayName": "Banana",
                    "tier": "coding_plan",
                    "logoSvg": '<svg viewBox="0 0 24 24"></svg>',
                    "api": "openai-completions",
                    "apiBase": "https://api.example.test/v1",
                    "models": [
                        {
                            "model": "banana-chat",
                            "displayName": "Banana Chat",
                            "contextTokens": 64_000,
                            "maxOutputTokens": 4_096,
                            "thinkingMode": "high",
                            "thinkingModes": ["none", "high"],
                            "apiOverride": None,
                            "apiBaseOverride": None,
                        },
                        {
                            "model": "banana-code",
                            "displayName": "Banana Code",
                            "contextTokens": 128_000,
                            "maxOutputTokens": 8_192,
                            "thinkingMode": "max",
                            "thinkingModes": ["high", "max"],
                            "apiOverride": "anthropic-messages",
                            "apiBaseOverride": "https://code.example.test",
                        },
                    ],
                }
            ],
        }
        self.client.force_login(self.admin)
        templates = self.client.get("/api/admin/model-provider-templates")
        self.assertEqual(templates.status_code, 200)
        self.assertEqual(
            templates.json()["templates"],
            [
                {
                    "id": "banana",
                    "displayName": "Banana",
                    "tier": "coding_plan",
                    "logoSvg": '<svg viewBox="0 0 24 24"></svg>',
                    "api": "openai-completions",
                    "apiBase": "https://api.example.test/v1",
                    "models": [
                        {
                            "modelName": "banana-chat",
                            "displayName": "Banana Chat",
                            "contextTokens": 64_000,
                            "maxOutputTokens": 4_096,
                            "thinkingMode": "high",
                            "thinkingModes": ["none", "high"],
                            "apiOverride": None,
                        },
                        {
                            "modelName": "banana-code",
                            "displayName": "Banana Code",
                            "contextTokens": 128_000,
                            "maxOutputTokens": 8_192,
                            "thinkingMode": "max",
                            "thinkingModes": ["high", "max"],
                            "apiOverride": "anthropic-messages",
                        },
                    ],
                }
            ],
        )
        created = self.client.post(
            "/api/admin/model-provider-templates/banana/instantiate",
            data=json.dumps({"secret": "test-provider-secret"}),
            content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        self.assertEqual(created.json()["provider"]["templateId"], "banana")
        provider_id = created.json()["provider"]["id"]
        self.assertEqual(
            list(
                ModelConfig.objects.filter(provider_id=provider_id, isCurrent=True)
                .order_by("modelName")
                .values_list("modelName", flat=True)
            ),
            ["banana-chat", "banana-code"],
        )
        self.assertNotIn("test-provider-secret", json.dumps(created.json()))
        preset_update = self.client.patch(
            f"/api/admin/model-providers/{provider_id}",
            data=json.dumps({"displayName": "renamed"}),
            content_type="application/json",
        )
        self.assertEqual(preset_update.status_code, 400)
        self.assertEqual(
            preset_update.json()["error"],
            "preset_model_provider_read_only",
        )
        preset_model = ModelConfig.objects.get(
            provider_id=provider_id,
            modelName="banana-code",
            isCurrent=True,
        )
        preset_delete = self.client.delete(f"/api/admin/models/{preset_model.id}")
        self.assertEqual(preset_delete.status_code, 400)
        self.assertEqual(
            preset_delete.json()["error"],
            "preset_provider_models_read_only",
        )
        models = {
            model.modelName: (model.resolvedApi, model.resolvedApiBase)
            for model in ModelConfig.objects.filter(
                provider_id=provider_id,
                isCurrent=True,
            )
        }
        self.assertEqual(
            models,
            {
                "banana-chat": (
                    "openai-completions",
                    "https://api.example.test/v1",
                ),
                "banana-code": (
                    "anthropic-messages",
                    "https://code.example.test",
                ),
            },
        )
        self.assertEqual(preset_model.thinkingMode, "max")
        self.assertEqual(
            preset_model.thinkingModes,
            ["high", "max"],
        )

    @patch("app_core.http.model_management.request_model_catalog")
    def test_catalog_reconciliation_previews_and_revises_presets_without_rotating_key(
        self, request_model_catalog
    ):
        def definition(name, context):
            return {"model": name, "displayName": name, "contextTokens": context,
                    "maxOutputTokens": 4096, "thinkingMode": None, "thinkingModes": [],
                    "apiOverride": None, "apiBaseOverride": None}

        provider = {"catalogId": "banana", "displayName": "Banana",
                    "api": "openai-completions", "apiBase": "https://api.example.test/v1",
                    "models": [definition("old", 64000), definition("keep", 64000)]}
        request_model_catalog.return_value = {"providers": [provider]}
        self.client.force_login(self.admin)
        created = self.client.post(
            "/api/admin/model-provider-templates/banana/instantiate",
            data=json.dumps({"secret": "test-provider-secret"}), content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        provider_id = created.json()["provider"]["id"]
        credential = ProviderCredential.objects.get(provider_id=provider_id)
        version = credential.version
        old = ModelConfig.objects.get(provider_id=provider_id, modelName="keep", isCurrent=True)

        provider["displayName"] = "Banana API"
        provider["api"] = "anthropic-messages"
        provider["apiBase"] = "https://new.example.test/anthropic"
        provider["models"] = [definition("keep", 128000), definition("new", 64000)]
        workspace = Workspace.objects.create(name="Reconciliation", createdBy=self.admin)
        workspace.members.add(self.admin)
        agent = Agent.objects.create(workspace=workspace, owner=self.admin, name="Agent")
        session = Session.objects.create(workspace=workspace, owner=self.admin, agent=agent)
        retiring = ModelConfig.objects.get(provider_id=provider_id, modelName="old", isCurrent=True)
        run = AgentRun.objects.create(workspace=workspace, session=session, user=self.admin,
                                      modelConfig=retiring, prompt="Finish first")
        preview = self.client.get("/api/admin/model-catalog-reconciliation")
        self.assertEqual(preview.status_code, 200)
        self.assertFalse(preview.json()["blocked"])
        self.assertEqual({change["action"] for change in preview.json()["providers"][0]["changes"]},
                         {"rename_provider", "update_provider_route", "retire", "revise", "add"})
        retired = next(change for change in preview.json()["providers"][0]["changes"]
                       if change["action"] == "retire")
        self.assertEqual(retired["activeAgentRunIds"], [run.id])
        run.status = "completed"
        run.save(update_fields=["status", "updatedAt"])
        self.assertEqual(
            self.client.get("/api/admin/model-catalog-reconciliation").json()["digest"],
            preview.json()["digest"],
        )
        run.status = "queued"
        run.save(update_fields=["status", "updatedAt"])
        self.assertEqual(self.client.post(
            "/api/admin/model-catalog-reconciliation",
            data=json.dumps({"expectedDigest": "stale"}), content_type="application/json",
        ).status_code, 409)
        applied = self.client.post(
            "/api/admin/model-catalog-reconciliation",
            data=json.dumps({"expectedDigest": preview.json()["digest"]}),
            content_type="application/json",
        )
        self.assertEqual(applied.status_code, 200)
        self.assertEqual(applied.json()["providers"][0]["changes"], [])
        self.assertEqual(set(ModelConfig.objects.filter(provider_id=provider_id, isCurrent=True)
                             .values_list("modelName", flat=True)), {"keep", "new"})
        revised = ModelConfig.objects.get(provider_id=provider_id, modelName="keep", isCurrent=True)
        self.assertEqual(revised.familyId, old.familyId)
        self.assertEqual(revised.revision, old.revision + 1)
        old.refresh_from_db()
        self.assertFalse(old.isCurrent)
        run.refresh_from_db()
        self.assertEqual(run.modelConfig_id, retiring.id)
        from .model_adapter.common import resolve_model_route
        self.assertEqual(resolve_model_route(run.modelConfig),
                         ("openai-completions", "https://api.example.test/v1"))
        self.assertEqual(ModelConfig.objects.get(provider_id=provider_id, modelName="keep",
                                                 isCurrent=True).resolvedApi,
                         "anthropic-messages")
        credential.refresh_from_db()
        self.assertEqual(credential.version, version)
        updated_provider = ModelProvider.objects.get(id=provider_id)
        self.assertEqual(updated_provider.displayName, "Banana API")
        self.assertEqual(updated_provider.apiBase, "https://new.example.test/anthropic")
        run.status = "completed"
        run.save(update_fields=["status", "updatedAt"])
        with patch("app_core.http.workspaces.schedule_agent_run_lifecycle"):
            continued = self.client.post(
                f"/api/workspaces/{workspace.id}/sessions/{session.id}/messages",
                data=json.dumps({"operationId": "test-operation-5027", "text": "Continue with the updated model", "agentId": agent.id,
                                 "modelConfigRef": revised.id}),
                content_type="application/json",
            )
        self.assertEqual(continued.status_code, 202)
        next_run = AgentRun.objects.get(id=continued.json()["agentRunId"])
        self.assertEqual(next_run.session_id, session.id)
        self.assertEqual(next_run.modelConfig_id, revised.id)

    def test_model_display_name_is_optional_and_public_dto_falls_back_to_model_name(self):
        self.client.force_login(self.admin)
        created = self.client.post(
            "/api/admin/models",
            data=json.dumps(
                {
                    "providerId": self.deepseekProvider.id,
                    "modelName": "deepseek-unnamed",
                    "contextTokens": 200000,
                    "maxOutputTokens": 32768,
                    "enabled": True,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        self.assertEqual(created.json()["model"]["displayName"], "")
        visible = self.client.get("/api/models").json()["models"]
        self.assertEqual(
            visible,
            [
                {
                    "id": created.json()["model"]["id"],
                    "displayName": "deepseek-unnamed",
                    "providerId": self.deepseekProvider.id,
                    "providerDisplayName": "DeepSeek",
                    "modelName": "deepseek-unnamed",
                    "contextTokens": 200000,
                    "maxOutputTokens": 32768,
                    "thinkingMode": None,
                    "thinkingModes": [],
                }
            ],
        )

    def test_superuser_binds_models_to_credentials_and_members_only_see_enabled_models(self):
        self.client.force_login(self.member)
        self.assertEqual(self.client.get("/api/admin/models").status_code, 403)
        self.assertEqual(self.client.get("/api/admin/model-providers").status_code, 403)
        self.assertEqual(
            self.client.post(
                "/api/admin/models", data="{}", content_type="application/json"
            ).status_code,
            403,
        )

        self.client.force_login(self.admin)
        created = self.client.post(
            "/api/admin/models",
            data=json.dumps(
                {
                    "displayName": "Compatible",
                    "providerId": self.deepseekProvider.id,
                    "modelName": "compatible-test",
                    "apiOverride": "openai-responses",
                    "contextTokens": 200000,
                    "maxOutputTokens": 32768,
                    "enabled": True,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        modelId = created.json()["model"]["id"]
        self.assertEqual(created.json()["model"]["api"], "openai-responses")
        serialized = json.dumps(created.json())
        self.assertNotIn("encryptedSecret", serialized)
        self.assertNotIn("test-provider-secret", serialized)

        unknownProvider = self.client.post(
            "/api/admin/models",
            data=json.dumps(
                {
                    "displayName": "Bad",
                    "providerId": "banana",
                    "modelName": "gpt-test",
                    "apiOverride": None,
                    "contextTokens": 200000,
                    "maxOutputTokens": 32768,
                    "enabled": False,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(unknownProvider.status_code, 400)

        disabled = self.client.patch(
            f"/api/admin/models/{modelId}",
            data=json.dumps({"enabled": False}),
            content_type="application/json",
        )
        self.assertEqual(disabled.status_code, 200)
        self.assertFalse(disabled.json()["model"]["enabled"])
        self.assertNotEqual(disabled.json()["model"]["id"], modelId)
        self.assertEqual(disabled.json()["model"]["revision"], 2)
        self.assertFalse(ModelConfig.objects.get(id=modelId).isCurrent)
        self.client.force_login(self.member)
        self.assertEqual(self.client.get("/api/models").json(), {"models": []})
        workspace = Workspace.objects.create(name="Default", createdBy=self.admin)
        workspace.members.add(self.member)
        session = create_session(workspace=workspace, owner=self.member)
        for modelRef in [modelId, "banana"]:
            rejected = self.client.post(
                f"/api/workspaces/{workspace.id}/sessions/{session.id}/messages",
                data=json.dumps({"operationId": "test-operation-5140", "text": "hello", "modelConfigRef": modelRef}),
                content_type="application/json",
            )
            self.assertEqual(rejected.status_code, 400)
            self.assertEqual(rejected.json(), {"error": "model_not_found"})

    def test_model_delete_retires_current_revision_and_preserves_active_agent_runs(self):
        self.client.force_login(self.admin)
        model = self.create_model()
        deleted = self.client.delete(f"/api/admin/models/{model.id}")
        self.assertEqual(deleted.status_code, 204)
        model.refresh_from_db()
        self.assertFalse(model.isCurrent)
        self.assertFalse(model.enabled)

        activeModel = self.create_model(displayName="DeepSeek active")
        workspace = Workspace.objects.create(name="Default", createdBy=self.admin)
        workspace.members.add(self.admin)
        session = create_session(workspace=workspace, owner=self.admin)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=self.admin,
            modelConfig=activeModel,
            prompt="hello",
        )
        retired = self.client.delete(f"/api/admin/models/{activeModel.id}")
        self.assertEqual(retired.status_code, 204)
        activeModel.refresh_from_db()
        self.assertFalse(activeModel.isCurrent)
        self.assertFalse(activeModel.enabled)
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.modelConfig_id, activeModel.id)
        from .model_adapter.common import resolve_model_route
        self.assertEqual(resolve_model_route(agent_run.modelConfig),
                         ("openai-completions", "https://api.deepseek.com"))

    def test_model_provider_endpoints_require_https_and_security_policy_is_removed(self):
        self.client.force_login(self.admin)
        self.assertEqual(self.client.get("/api/admin/model-endpoint-security").status_code, 404)
        self.assertEqual(
            self.client.patch(
                "/api/admin/model-endpoint-security",
                data=json.dumps({"allowInsecureHttpEndpoints": True}),
                content_type="application/json",
            ).status_code,
            404,
        )
        rejected = self.client.post(
            "/api/admin/model-providers",
            data=json.dumps(
                {
                    "displayName": "vLLM",
                    "api": "openai-completions",
                    "apiBase": "http://ollama.local:11434",
                    "secret": "1",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(rejected.status_code, 400)
        self.assertEqual(rejected.json(), {"error": "model_endpoint_https_required"})

    def test_provider_archive_retires_models_and_rejects_active_agent_runs(self):
        self.client.force_login(self.admin)
        model = self.create_model()
        workspace = Workspace.objects.create(name="Default", createdBy=self.admin)
        workspace.members.add(self.admin)
        session = create_session(workspace=workspace, owner=self.admin)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=self.admin,
            modelConfig=model,
            prompt="hello",
        )
        activeAgentRun = self.client.delete(f"/api/admin/model-providers/{self.deepseekProvider.id}")
        self.assertEqual(activeAgentRun.status_code, 409)
        self.assertEqual(activeAgentRun.json()["error"], "model_provider_has_active_agent_runs")
        model.refresh_from_db()
        self.assertTrue(model.isCurrent)
        self.assertTrue(model.enabled)
        agent_run.status = "cancelled"
        agent_run.save(update_fields=["status"])
        deleted = self.client.delete(f"/api/admin/model-providers/{self.deepseekProvider.id}")
        self.assertEqual(deleted.status_code, 204)
        model.refresh_from_db()
        self.assertFalse(model.isCurrent)
        self.assertFalse(model.enabled)
        self.deepseekProvider.refresh_from_db()
        self.assertIsNotNone(self.deepseekProvider.archivedAt)

    def test_frozen_agent_run_can_use_model_disabled_after_context_creation(self):
        workspace = Workspace.objects.create(name="Default", createdBy=self.member)
        workspace.members.add(self.member)
        model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        session = create_session(workspace=workspace, owner=self.member)
        agent_run = AgentRun.objects.create(
            workspace=workspace,
            session=session,
            user=self.member,
            modelConfig=model,
            prompt="hello",
        )
        authorization = create_agent_run_authorization(agent_run)
        model.enabled = False
        model.save()
        response = self.client.post(
            "/internal/model-runs",
            data=json.dumps(
                {
                    "schema": MODEL_RUN_SCHEMA,
                    "agentRunId": agent_run.id,
                    "modelConfigRef": model.id,
                    "maxOutputTokens": model.maxOutputTokens,
                    "authorizationRef": authorization.id,
                    "authorizationDigest": authorization.digest,
                    "preparedPrompt": prepared_prompt_for_test(
                        model,
                        messages=[
                            {
                                "messageId": "msg-user",
                                "role": "user",
                                "content": "hello",
                            }
                        ],
                    ),
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(response.status_code, 200)
        self.assertEqual(ModelRunLog.objects.get(agentRunId=agent_run.id).status, "success")

    def test_open_ai_compatible_stream_preserves_delta_tool_calls_and_usage(self):
        model = self.create_model()
        chunks = [
            {
                "choices": [
                    {"finish_reason": None, "delta": {"reasoning": "inspect request"}}
                ]
            },
            {"choices": [{"delta": {"content": "hello"}, "finish_reason": None}]},
            {
                "choices": [
                    {
                        "delta": {
                            "tool_calls": [
                                {
                                    "index": 0,
                                    "id": "call_1",
                                    "function": {
                                        "name": "read",
                                        "arguments": '{"path":"a',
                                    },
                                }
                            ]
                        },
                        "finish_reason": None,
                    }
                ]
            },
            {
                "choices": [
                    {
                        "delta": {
                            "tool_calls": [
                                {"index": 0, "function": {"arguments": '.md"}'}}
                            ]
                        },
                        "finish_reason": None,
                    }
                ]
            },
            {"choices": [{"delta": {}, "finish_reason": "tool_calls"}]},
            {
                "choices": [],
                "usage": {
                    "prompt_tokens": 4,
                    "completion_tokens": 2,
                    "total_tokens": 6,
                    "prompt_cache_hit_tokens": 3,
                    "prompt_cache_miss_tokens": 1,
                },
            },
        ]

        client = Mock()
        client.chat.completions.create = AsyncMock(return_value=async_values(chunks))
        client.close = AsyncMock()
        with patch(
            "app_core.model_adapter.openai_completions.async_open_ai_completions_client",
            new=AsyncMock(return_value=client),
        ):
            events = async_iterator_bytes(
                stream_model_async(
                    "agent_run_stream",
                    model.id,
                    {"preparedPrompt": prepared_prompt_for_test(model)},
                )
            ).decode()
        self.assertIn('"delta":"hello"', events)
        self.assertNotIn('"delta":"inspect request"', events)
        self.assertIn('"reasoningContent":"inspect request"', events)
        self.assertIn('"name":"read"', events)
        self.assertIn('"total_tokens":6', events)
        self.assertIn('"prompt_cache_hit_tokens":3', events)
        self.assertEqual(
            client.chat.completions.create.call_args.kwargs["model"], "deepseek-chat"
        )
        self.assertTrue(client.chat.completions.create.call_args.kwargs["stream"])
        self.assertEqual(
            client.chat.completions.create.call_args.kwargs["stream_options"],
            {"include_usage": True},
        )
        log = ModelRunLog.objects.get(agentRunId="agent_run_stream")
        self.assertEqual(
            (log.promptTokens, log.completionTokens, log.totalTokens), (4, 2, 6)
        )
        self.assertEqual((log.promptCacheHitTokens, log.promptCacheMissTokens), (3, 1))

    def test_provider_failures_are_bounded_and_partial_stream_never_succeeds(self):
        model = self.create_model()
        request = httpx.Request("POST", "https://api.deepseek.com/chat/completions")
        expected = [
            (
                AuthenticationError(
                    "provider secret response body",
                    response=httpx.Response(401, request=request),
                    body=None,
                ),
                "provider_authentication_failed",
            ),
            (
                RateLimitError(
                    "provider secret response body",
                    response=httpx.Response(429, request=request),
                    body=None,
                ),
                "provider_rate_limited",
            ),
            (
                InternalServerError(
                    "provider secret response body",
                    response=httpx.Response(503, request=request),
                    body=None,
                ),
                "provider_unavailable",
            ),
        ]
        for index, (error, reasonType) in enumerate(expected):
            with self.subTest(reasonType=reasonType):
                agentRunId = f"agent_run_{index}"
                client = Mock()
                client.chat.completions.create = AsyncMock(side_effect=error)
                client.close = AsyncMock()
                with patch(
                    "app_core.model_adapter.openai_completions.async_open_ai_completions_client",
                    new=AsyncMock(return_value=client),
                ):
                    with self.assertRaises(ModelProviderError):
                        async_iterator_bytes(
                            stream_model_async(
                                agentRunId,
                                model.id,
                                {"preparedPrompt": prepared_prompt_for_test(model)},
                            )
                        )
                log = ModelRunLog.objects.get(agentRunId=agentRunId)
                self.assertEqual(log.error, reasonType)
                self.assertNotIn("provider secret response body", log.error)

        partial = [
            {"choices": [{"delta": {"content": "partial"}, "finish_reason": None}]}
        ]
        client = Mock()
        client.chat.completions.create = AsyncMock(return_value=async_values(partial))
        client.close = AsyncMock()
        with patch(
            "app_core.model_adapter.openai_completions.async_open_ai_completions_client",
            new=AsyncMock(return_value=client),
        ):
            with self.assertRaisesRegex(
                ModelProviderError, "provider_stream_interrupted"
            ):
                async_iterator_bytes(
                    stream_model_async(
                        "agent_run_partial",
                        model.id,
                        {"preparedPrompt": prepared_prompt_for_test(model)},
                    )
                )
        partialLog = ModelRunLog.objects.get(agentRunId="agent_run_partial")
        self.assertEqual(partialLog.status, "error")
        self.assertEqual(partialLog.error, "provider_stream_interrupted")
        self.assertIsNone(partialLog.totalTokens)

    def test_admin_model_test_does_not_return_provider_body(self):
        model = self.create_model()
        self.client.force_login(self.admin)
        request = httpx.Request("POST", "https://api.deepseek.com/chat/completions")
        error = AuthenticationError(
            "provider body with credential",
            response=httpx.Response(401, request=request),
            body=None,
        )
        client = Mock()
        client.chat.completions.create.side_effect = error
        with patch(
            "app_core.model_adapter.openai_completions.open_ai_completions_client", return_value=client
        ):
            response = self.client.post(
                f"/api/admin/models/{model.id}/test",
                data="{}",
                content_type="application/json",
            )
        self.assertEqual(response.status_code, 200)
        result = response.json()
        self.assertFalse(result["ok"])
        self.assertEqual(result["httpStatus"], 401)
        self.assertIsInstance(result["latencyMs"], int)
        self.assertIsNone(result["outputPreview"])
        self.assertEqual(result["errorKeyword"], "provider_authentication_failed")
        self.assertNotIn("provider body with credential", json.dumps(result))
        self.assertEqual(client.chat.completions.create.call_args.kwargs["max_tokens"], 32)


class ModelProtocolAdapterTests(TestCase):
    def _model(self, api: str) -> ModelConfig:
        provider = ModelProvider.objects.create(
            displayName=api,
            api=api,
            apiBase="https://models.example.com/v1",
        )
        return ModelConfig.objects.create(
            displayName=api,
            provider=provider,
            modelName="model-test",
            resolvedApi=api,
            resolvedApiBase=provider.apiBase,
        )

    def _tool_prompt(self, model: ModelConfig) -> dict:
        return {
            "preparedPrompt": prepared_prompt_for_test(
                model,
                messages=[
                    {"messageId": "user", "role": "user", "content": "read it"},
                    {
                        "messageId": "assistant",
                        "role": "assistant",
                        "content": "",
                        "toolCalls": [
                            {"id": "call_1", "name": "read", "argsJson": '{"path":"a"}'}
                        ],
                    },
                    {
                        "messageId": "tool",
                        "role": "tool",
                        "toolCallId": "call_1",
                        "content": "ok",
                    },
                ],
                toolDefinitions=[
                    {"name": "read", "description": "read", "inputSchema": {"type": "object"}}
                ],
                toolChoice={"type": "auto"},
            )
        }

    def test_thinking_mode_uses_each_provider_protocol_shape_and_omits_when_unset(self):
        cases = [
            ("openai-completions", build_open_ai_completions_request, "reasoning_effort", "vendor-high"),
            ("openai-responses", build_open_ai_responses_request, "reasoning", {"effort": "vendor-high", "summary": "auto"}),
            ("anthropic-messages", build_anthropic_messages_request, "output_config", {"effort": "vendor-high"}),
        ]
        for api, builder, field, expected in cases:
            model = self._model(api)
            model.thinkingModes = ["vendor-high"]
            model.save(update_fields=["thinkingModes"])
            request_body = self._tool_prompt(model)
            self.assertNotIn(field, builder(model, request_body))
            request_body["thinkingMode"] = "vendor-high"
            self.assertEqual(builder(model, request_body)[field], expected)

    def test_open_ai_completions_normalizes_reasoning_aliases_and_rejects_conflict(self):
        for field in ("reasoning", "reasoning_content"):
            result = parse_open_ai_completions_response(
                {
                    "choices": [
                        {"message": {"content": "done", field: "inspect request"}}
                    ],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2,
                    },
                }
            )
            self.assertEqual(result["text"], "done")
            self.assertEqual(result["reasoningContent"], "inspect request")

        with self.assertRaisesRegex(
            ModelProviderError, "provider_response_invalid"
        ):
            parse_open_ai_completions_response(
                {
                    "choices": [
                        {
                            "message": {
                                "content": "done",
                                "reasoning": "vllm",
                                "reasoning_content": "legacy",
                            }
                        }
                    ]
                }
            )

    def test_open_ai_completions_stream_rejects_reasoning_field_switch(self):
        model = self._model("openai-completions")
        chunks = [
            {"choices": [{"delta": {"reasoning": "inspect"}}]},
            {"choices": [{"delta": {"reasoning_content": "legacy"}}]},
        ]
        client = Mock()
        client.chat.completions.create = AsyncMock(return_value=async_values(chunks))
        client.close = AsyncMock()

        with patch(
            "app_core.model_adapter.openai_completions.async_open_ai_completions_client",
            new=AsyncMock(return_value=client),
        ), self.assertRaisesRegex(ModelProviderError, "provider_response_invalid"):
            async_iterator_bytes(
                stream_open_ai_completions(
                    model,
                    {"preparedPrompt": prepared_prompt_for_test(model)},
                    {},
                    lambda event_type, payload: json.dumps({"type": event_type, **payload}).encode(),
                )
            )

    def test_open_ai_responses_projects_and_normalizes_tool_calls(self):
        model = self._model("openai-responses")
        request = build_open_ai_responses_request(model, self._tool_prompt(model))
        self.assertEqual(request["tools"][0]["name"], "read")
        self.assertEqual(request["input"][2]["type"], "function_call")
        self.assertEqual(request["input"][3]["type"], "function_call_output")
        result = parse_open_ai_responses_response(
            {
                "output": [
                    {"type": "message", "content": [{"type": "output_text", "text": "done"}]},
                    {"type": "function_call", "call_id": "call_2", "name": "read", "arguments": "{}"},
                ],
                "usage": {"input_tokens": 2, "output_tokens": 3, "total_tokens": 5},
            }
        )
        self.assertEqual(result["text"], "done")
        self.assertEqual(result["toolCalls"][0]["id"], "call_2")
        self.assertEqual(result["usage"]["total_tokens"], 5)

    def test_anthropic_messages_projects_and_normalizes_tool_calls(self):
        model = self._model("anthropic-messages")
        request = build_anthropic_messages_request(model, self._tool_prompt(model))
        self.assertEqual(request["tools"][0]["input_schema"], {"type": "object"})
        self.assertEqual(request["messages"][1]["content"][0]["type"], "tool_use")
        self.assertEqual(request["messages"][2]["content"][0]["type"], "tool_result")
        result = parse_anthropic_message(
            {
                "content": [
                    {"type": "text", "text": "done"},
                    {"type": "tool_use", "id": "call_2", "name": "read", "input": {}},
                ],
                "usage": {
                    "input_tokens": 2,
                    "output_tokens": 3,
                    "cache_read_input_tokens": 4,
                },
            }
        )
        self.assertEqual(result["text"], "done")
        self.assertEqual(result["toolCalls"][0]["argsJson"], "{}")
        self.assertEqual(result["usage"]["prompt_tokens"], 6)

    def test_responses_and_anthropic_streams_produce_normalized_terminal_results(self):
        async def response_events():
            yield {"type": "response.output_text.delta", "delta": "done"}
            yield {
                "type": "response.completed",
                "response": {
                    "output": [
                        {"type": "message", "content": [{"type": "output_text", "text": "done"}]}
                    ],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
                },
            }

        async def anthropic_events():
            yield {"type": "message_start", "message": {"usage": {"input_tokens": 1}}}
            yield {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "done"}}
            yield {"type": "message_delta", "usage": {"output_tokens": 1}}
            yield {"type": "message_stop"}

        response_model = self._model("openai-responses")
        response_client = Mock()
        response_client.responses.create = AsyncMock(return_value=response_events())
        response_client.close = AsyncMock()
        response_result = {}
        with patch(
            "app_core.model_adapter.openai_responses.async_open_ai_responses_client",
            new=AsyncMock(return_value=response_client),
        ):
            async_iterator_bytes(
                stream_open_ai_responses(
                    response_model,
                    {"preparedPrompt": prepared_prompt_for_test(response_model)},
                    response_result,
                    lambda _type, payload: payload["delta"].encode(),
                )
            )
        self.assertEqual(response_result["result"]["text"], "done")
        self.assertEqual(response_client.close.await_count, 1)

        anthropic_model = self._model("anthropic-messages")
        anthropic_client = Mock()
        anthropic_client.messages.create = AsyncMock(return_value=anthropic_events())
        anthropic_client.close = AsyncMock()
        anthropic_result = {}
        with patch(
            "app_core.model_adapter.anthropic_messages.async_anthropic_messages_client",
            new=AsyncMock(return_value=anthropic_client),
        ):
            async_iterator_bytes(
                stream_anthropic_messages(
                    anthropic_model,
                    {"preparedPrompt": prepared_prompt_for_test(anthropic_model)},
                    anthropic_result,
                    lambda _type, payload: payload["delta"].encode(),
                )
            )
        self.assertEqual(anthropic_result["result"]["usage"]["total_tokens"], 2)
        self.assertEqual(anthropic_client.close.await_count, 1)


class RuntimeJobProjectionTests(TestCase):
    def test_job_status_is_owner_scoped_and_browser_safe(self):
        owner = User.objects.create_user(
            username="owner@example.com", password="password"
        )
        other = User.objects.create_user(
            username="other@example.com", password="password"
        )
        workspace = Workspace.objects.create(name="Default", createdBy=owner)
        workspace.members.add(owner)
        session = create_session(workspace=workspace, owner=owner)
        runtimeJob = {
            "jobId": "job_projection",
            "jobKind": "worker.noop",
            "status": "running",
            "sessionId": session.id,
            "outputRefs": ["artifact:art_safe", "storage:private/path"],
            "lastError": None,
            "payloadRef": "storage:secret",
            "leaseOwner": "worker:secret",
            "idempotencyKey": "secret-key",
        }
        self.client.force_login(owner)
        with patch("app_core.http.jobs.get_runtime_job", return_value=runtimeJob):
            response = self.client.get("/api/jobs/job_projection")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["job"]["progressTopic"], "正在处理")
        serialized = json.dumps(response.json())
        self.assertNotIn("storage:", serialized)
        self.assertNotIn("worker.noop", serialized)
        self.assertNotIn("leaseOwner", serialized)
        self.assertNotIn("idempotencyKey", serialized)
        runtimeJob["status"] = "failed"
        runtimeJob["lastError"] = "provider body\nsecret"
        with patch("app_core.http.jobs.get_runtime_job", return_value=runtimeJob):
            failed = self.client.get("/api/jobs/job_projection")
        self.assertEqual(failed.json()["job"]["error"], "job_failed")
        self.assertNotIn("provider body", json.dumps(failed.json()))
        self.client.force_login(other)
        with patch("app_core.http.jobs.get_runtime_job", return_value=runtimeJob):
            self.assertEqual(
                self.client.get("/api/jobs/job_projection").status_code, 404
            )


class WorkspaceAssetAcceptanceTests(TestCase):
    def setUp(self):
        self.admin = User.objects.create_user(
            username="admin@example.com",
            password="password",
            is_staff=True,
        )
        self.member = User.objects.create_user(
            username="member@example.com", password="password"
        )
        self.other = User.objects.create_user(
            username="other@example.com", password="password"
        )
        self.workspace = Workspace.objects.create(name="牙科 SOP", createdBy=self.admin)
        self.admin_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.admin,
            role="owner",
        )
        self.member_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )
        self.workspaceGroup = WorkspaceGroup.objects.create(
            workspace=self.workspace,
            name="助理",
            createdBy=self.admin,
        )
        self.workspaceGroup.members.add(self.member_membership)
        self.model = ModelConfig.objects.create(
            id="fake-model", displayName="Fake"
        )
        self.session = create_session(
            workspace=self.workspace, owner=self.member
        )
        self.source = Source.objects.create(
            workspace=self.workspace,
            sourceType="fileTree",
            name="正式资料",
            status="ready",
            createdBy=self.admin,
        )
        self.allowedObject = self.source_object("患者沟通资料/术前须知.md", "allowed")
        self.deniedObject = self.source_object("收费标准/价格.md", "denied")

    def source_object(self, displayPath, suffix):
        return SourceObject.objects.create(
            workspace=self.workspace,
            source=self.source,
            objectType="file",
            displayPath=displayPath,
            displayName=displayPath.rsplit("/", 1)[-1],
            contentType="text/markdown",
            sizeBytes=4,
            sha256=f"sha256:{suffix[0] * 64}",
            storageKey=f"private/{suffix}.md",
            sourceVersion=f"version-{suffix}",
            status="ready",
        )

    def store_bytes(self, storageKey: str, content: bytes) -> tuple[str, int, str]:
        savedKey = default_storage.save(storageKey, ContentFile(content))
        return savedKey, len(content), f"sha256:{hashlib.sha256(content).hexdigest()}"

    def prepare_allowed_source_input(self, content=b"allowed source"):
        storageKey, sizeBytes, sha256 = self.store_bytes(
            f"materialization/{self.allowedObject.id}.md", content
        )
        self.allowedObject.storageKey = storageKey
        self.allowedObject.sizeBytes = sizeBytes
        self.allowedObject.sha256 = sha256
        self.allowedObject.save()
        SourceGrant.objects.create(
            workspace=self.workspace,
            source=self.source,
            workspaceGroup=self.workspaceGroup,
            createdBy=self.admin,
        )
        return SessionAssetLink.objects.create(
            workspace=self.workspace,
            session=self.session,
            sourceObject=self.allowedObject,
            attachedBy=self.member,
            capturedDisplayName=self.allowedObject.displayName,
            capturedContentType=self.allowedObject.contentType,
            **captured_input_fields(self.allowedObject),
        )

    def test_knowledge_processor_device_identity_is_exact(self):
        specification = {
            "schema": "knowledge.processing_specification.v1",
            "processorId": "centaeris.document.cpu",
            "processorVersion": "1.0.0",
            "executionImageDigest": f"sha256:{'1' * 64}",
            "modelDigests": {
                "PP-OCRv6_small_det": f"sha256:{'2' * 64}",
                "PP-OCRv6_small_rec": f"sha256:{'3' * 64}",
            },
            "options": {
                "renderDpi": 220,
                "maxInputBytes": 64 * 1024 * 1024,
                "maxRenderedPixelsPerPage": 16_000_000,
                "maxOutputBytes": 256 * 1024 * 1024,
            },
        }
        validate_processing_specification(specification)
        with self.assertRaisesRegex(KnowledgeError, "knowledge_processing_options_unsupported"):
            validate_processing_specification(
                {**specification, "options": {**specification["options"], "maxPages": 1_000}}
            )
        validate_processing_specification(
            {**specification, "processorId": "centaeris.document.cuda.gpu0"}
        )
        with self.assertRaisesRegex(KnowledgeError, "knowledge_processor_identity_unsupported"):
            validate_processing_specification(
                {**specification, "processorId": "banana"}
            )
        with self.assertRaisesRegex(KnowledgeError, "knowledge_processor_identity_unsupported"):
            validate_processing_specification(
                {**specification, "processorVersion": "banana"}
            )
        with self.assertRaisesRegex(KnowledgeError, "knowledge_model_identity_invalid"):
            validate_processing_specification(
                {
                    **specification,
                    "modelDigests": {
                        "PP-OCRv6_medium_det": f"sha256:{'2' * 64}",
                        "PP-OCRv6_medium_rec": f"sha256:{'3' * 64}",
                    },
                }
            )

    def test_source_grant_exposes_the_whole_tree_without_storage_keys(self):
        SourceGrant.objects.create(
            workspace=self.workspace,
            source=self.source,
            workspaceGroup=self.workspaceGroup,
            createdBy=self.admin,
        )
        self.client.force_login(self.member)

        response = self.client.get(
            f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}/objects"
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            {item["id"] for item in response.json()["objects"]},
            {self.allowedObject.id, self.deniedObject.id},
        )
        self.assertNotIn("storageKey", response.content.decode("utf-8"))
        linked = self.client.post(
            f"/api/sessions/{self.session.id}/assets",
            data=json.dumps(
                {"assetKind": "sourceObject", "assetId": self.deniedObject.id}
            ),
            content_type="application/json",
        )
        self.assertEqual(linked.status_code, 201, linked.content)

    def test_source_grant_rejects_a_group_from_another_workspace(self):
        otherWorkspace = Workspace.objects.create(
            name="其他工作区", createdBy=self.admin
        )
        otherWorkspaceGroup = WorkspaceGroup.objects.create(
            workspace=otherWorkspace,
            name="助理",
            createdBy=self.admin,
        )

        with self.assertRaisesRegex(ValueError, "workspace/group binding mismatch"):
            SourceGrant.objects.create(
                workspace=self.workspace,
                source=self.source,
                workspaceGroup=otherWorkspaceGroup,
                createdBy=self.admin,
            )

    def test_workspace_group_member_without_workspace_access_is_not_granted(self):
        other_workspace = Workspace.objects.create(
            name="其他工作区",
            createdBy=self.admin,
        )
        other_membership = WorkspaceMembership.objects.create(
            workspace=other_workspace,
            user=self.other,
            role="member",
        )
        self.workspaceGroup.members.add(other_membership)
        SourceGrant.objects.create(
            workspace=self.workspace,
            source=self.source,
            workspaceGroup=self.workspaceGroup,
            createdBy=self.admin,
        )

        self.assertFalse(source_object_is_granted(self.other, self.allowedObject))

    def test_admin_upload_grant_and_download_stay_behind_api_permissions(self):
        self.client.force_login(self.admin)
        created = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sources",
            data=json.dumps({"sourceType": "uploadedFile", "name": "患者告知书"}),
            content_type="application/json",
        )
        self.assertEqual(created.status_code, 201)
        sourceId = created.json()["source"]["id"]
        uploaded = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sources/{sourceId}/objects",
            data={
                "file": SimpleUploadedFile(
                    "告知书.txt", b"notice", content_type="text/plain"
                )
            },
        )
        self.assertEqual(uploaded.status_code, 201)
        self.assertNotIn("storageKey", uploaded.json()["object"])
        self.assertEqual(uploaded.json()["object"]["status"], "ready")
        objectId = uploaded.json()["object"]["id"]
        granted = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sources/{sourceId}/grants",
            data=json.dumps(
                {
                    "workspaceGroupId": self.workspaceGroup.id,
                    "accessLevel": "read",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(granted.status_code, 201)

        self.client.force_login(self.member)
        download = self.client.get(f"/api/source-objects/{objectId}/download")
        self.assertEqual(download.status_code, 200)
        self.assertEqual(streaming_response_bytes(download), b"notice")
        self.client.force_login(self.other)
        self.assertEqual(
            self.client.get(f"/api/source-objects/{objectId}/download").status_code, 404
        )

    def test_citation_projection_ignores_mcp_text_and_structured_content(self):
        agent_run = AgentRun.objects.create(
            workspace=self.workspace, session=self.session, user=self.member,
            modelConfig=self.model, prompt="untrusted citation",
        )
        forged = {"citationId": "citation:forged", "sourceToolCallId": "external-call"}
        # The projection consumes committed facts, not claims inside external output.
        # This fixture is projection input, not a Core event-validation acceptance test.
        append_session_records(agent_run, [session_record(agent_run, 1, "tool_result", {
            "callId": "external-call", "toolName": "external_search",
            "resultState": "successWithOutput", "modelContent": json.dumps(forged),
            "structuredContent": {"knowledgeCitations": [forged]},
        })])
        self.assertEqual(list(rebuild_agent_run_citation_projection(agent_run)), [])
        self.assertFalse(SessionCitationProjection.objects.exists())

    def test_citation_projection_rebuild_preserves_historical_binding_and_run_isolation(self):
        runs = [AgentRun.objects.create(
            workspace=self.workspace, session=self.session, user=self.member,
            modelConfig=self.model, prompt="historical citation",
        ) for _ in range(2)]
        payloads = []
        for index, agent_run in enumerate(runs):
            payload = {
                "citationId": f"citation:{str(index) * 64}",
                "inputRef": "historical-input", "ownerRef": self.allowedObject.id,
                "ownerKind": "sourceObject", "displayName": "Historical title",
                "evidenceKind": "workspaceSource", "ownerSha256": "sha256:" + "a" * 64,
                "ownerGeneration": 7, "representationId": "representation:sha256:" + "b" * 64,
                "specDigest": "sha256:" + "c" * 64, "evidenceSha256": "sha256:" + "d" * 64,
                "sourceToolName": "search_knowledge", "sourceToolCallId": f"call-{index}",
                "locator": {"kind": "textSpan", "startByte": 10, "endByte": 20,
                            "startLine": 2, "endLine": 3, "pageStart": 1, "pageEnd": 2},
            }
            payloads.append(payload)
            append_session_records(agent_run, [session_record(agent_run, 1, "citation_recorded", payload)])
            rebuild_agent_run_citation_projection(agent_run)
        # Rebuild is repeatable and uses recorded identities, not today's source metadata.
        self.allowedObject.displayName = "Renamed source"
        self.allowedObject.save()
        for _ in range(2):
            rebuilt = rebuild_agent_run_citation_projection(runs[0])
            self.assertEqual(len(rebuilt), 1)
            self.assertEqual(SessionCitationProjection.objects.count(), 2)
            for field, value in payloads[0].items():
                self.assertEqual(getattr(rebuilt[0], field), value)
            self.assertEqual(rebuilt[0].sequence, 1)
            self.assertEqual(SessionCitationProjection.objects.get(pk=payloads[1]["citationId"]).sequence, 2)

    def test_citation_preview_streams_the_bound_source_through_current_authorization(self):
        content = b"Authorized preview evidence"
        self.prepare_allowed_source_input(content)
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="preview",
        )
        citation = SessionCitationProjection.objects.create(
            citationId="citation:preview-source",
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            sequence=1,
            inputRef="opaque-preview",
            ownerRef=self.allowedObject.id,
            ownerKind="sourceObject",
            displayName=self.allowedObject.displayName,
            evidenceKind="workspaceSource",
            ownerSha256=self.allowedObject.sha256,
            sourceToolCallId="call-preview",
            locator={"startLine": 1, "endLine": 1},
        )
        self.client.force_login(self.member)

        detail = self.client.get(f"/api/citations/{citation.citationId}")
        preview = self.client.get(f"/api/citations/{citation.citationId}/preview")

        self.assertEqual(detail.status_code, 200)
        self.assertEqual(
            detail.json()["citation"]["previewUrl"],
            f"/api/citations/{citation.citationId}/preview",
        )
        self.assertEqual(detail.json()["citation"]["originLabel"], "库")
        self.assertEqual(
            detail.json()["citation"]["downloadUrl"],
            f"/api/source-objects/{self.allowedObject.id}/download",
        )
        self.assertEqual(preview.status_code, 200)
        self.assertNotIn("attachment", preview["Content-Disposition"].lower())
        self.assertEqual(streaming_response_bytes(preview), content)
        self.client.force_login(self.admin)
        self.assertEqual(
            self.client.get(f"/api/citations/{citation.citationId}").status_code,
            404,
        )
        self.assertEqual(
            self.client.get(
                f"/api/citations/{citation.citationId}/preview"
            ).status_code,
            404,
        )
        self.client.force_login(self.member)
        SourceGrant.objects.filter(
            source=self.source, workspaceGroup=self.workspaceGroup
        ).delete()
        self.assertEqual(
            self.client.get(
                f"/api/citations/{citation.citationId}/preview"
            ).status_code,
            404,
        )
        self.client.force_login(self.other)
        self.assertEqual(
            self.client.get(
                f"/api/citations/{citation.citationId}/preview"
            ).status_code,
            404,
        )

    def test_citation_preview_loud_fails_for_stale_and_unsupported_sources(self):
        self.prepare_allowed_source_input(b"Bound bytes")
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="preview",
        )
        citation = SessionCitationProjection.objects.create(
            citationId="citation:preview-stale",
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            sequence=1,
            inputRef="opaque-stale",
            ownerRef=self.allowedObject.id,
            ownerKind="sourceObject",
            displayName=self.allowedObject.displayName,
            evidenceKind="workspaceSource",
            ownerSha256=self.allowedObject.sha256,
            sourceToolCallId="call-preview",
            locator={"startLine": 1, "endLine": 1},
        )
        self.client.force_login(self.member)
        originalSha256 = self.allowedObject.sha256
        self.allowedObject.sha256 = f"sha256:{'f' * 64}"
        self.allowedObject.save(update_fields=["sha256", "updatedAt"])

        stale = self.client.get(f"/api/citations/{citation.citationId}/preview")

        self.assertEqual(stale.status_code, 409)
        self.assertEqual(stale.json(), {"error": "citation_source_stale"})
        self.allowedObject.sha256 = originalSha256
        self.allowedObject.contentType = (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        )
        self.allowedObject.save(update_fields=["sha256", "contentType", "updatedAt"])

        missingNormalizedPreview = self.client.get(
            f"/api/citations/{citation.citationId}/preview"
        )

        self.assertEqual(missingNormalizedPreview.status_code, 415)
        self.assertEqual(
            missingNormalizedPreview.json(),
            {"error": "citation_preview_unsupported"},
        )

    def test_citation_preview_streams_octet_stream_source_code_as_plain_text(self):
        content = b"export const preview = true;\n"
        self.allowedObject.displayPath = "src/preview.ts"
        self.allowedObject.displayName = "preview.ts"
        self.allowedObject.contentType = "application/octet-stream"
        self.prepare_allowed_source_input(content)
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="preview source code",
        )
        citation = SessionCitationProjection.objects.create(
            citationId="citation:preview-source-code",
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            sequence=1,
            inputRef="opaque-source-code",
            ownerRef=self.allowedObject.id,
            ownerKind="sourceObject",
            displayName=self.allowedObject.displayName,
            evidenceKind="workspaceSource",
            ownerSha256=self.allowedObject.sha256,
            sourceToolCallId="call-preview-source-code",
            locator={"startLine": 1, "endLine": 1},
        )
        self.client.force_login(self.member)

        preview = self.client.get(f"/api/citations/{citation.citationId}/preview")

        self.assertEqual(preview.status_code, 200)
        self.assertEqual(preview["Content-Type"], "text/plain; charset=utf-8")
        self.assertNotIn("attachment", preview["Content-Disposition"].lower())
        self.assertEqual(streaming_response_bytes(preview), content)

    def test_citation_preview_supports_the_bound_user_library_object(self):
        content = b"Personal evidence"
        storageKey, sizeBytes, sha256 = self.store_bytes(
            "private/personal-evidence.txt", content
        )
        item = UserLibraryObject.objects.create(
            owner=self.member,
            displayName="个人依据.txt",
            objectKind="file",
            contentType="text/plain",
            sizeBytes=sizeBytes,
            sha256=sha256,
            storageKey=storageKey,
            status="ready",
        )
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="preview personal",
        )
        citation = SessionCitationProjection.objects.create(
            citationId="citation:preview-library",
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            sequence=1,
            inputRef="opaque-library",
            ownerRef=item.id,
            ownerKind="userLibraryObject",
            displayName=item.displayName,
            evidenceKind="userProvided",
            ownerSha256=item.sha256,
            sourceToolCallId="call-preview",
            locator={"startLine": 1, "endLine": 1},
        )
        self.client.force_login(self.member)

        preview = self.client.get(f"/api/citations/{citation.citationId}/preview")

        self.assertEqual(preview.status_code, 200)
        self.assertEqual(streaming_response_bytes(preview), content)

    def test_user_library_only_enters_agent_run_authorization_after_explicit_link(self):
        libraryObject = UserLibraryObject.objects.create(
            owner=self.member,
            displayName="个人病例.txt",
            objectKind="file",
            contentType="text/plain",
            sizeBytes=5,
            sha256=f"sha256:{'a' * 64}",
            storageKey="private/library.txt",
            status="ready",
            contentGeneration=1,
        )
        firstRun = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="first",
        )
        self.assertEqual(create_agent_run_authorization(firstRun).payload["assetRefs"], [])
        self.client.force_login(self.member)
        linked = self.client.post(
            f"/api/sessions/{self.session.id}/assets",
            data=json.dumps(
                {"assetKind": "userLibraryObject", "assetId": libraryObject.id}
            ),
            content_type="application/json",
        )
        self.assertEqual(linked.status_code, 201)

        secondRun = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="second",
        )
        payload = create_agent_run_authorization(secondRun).payload
        self.assertEqual(len(payload["assetRefs"]), 1)
        self.assertNotIn("storageKey", json.dumps(payload))
        self.assertNotIn("storageKey", json.dumps(payload))

        deleted = self.client.delete(
            f"/api/sessions/{self.session.id}/assets",
            data=json.dumps({"assetLinkId": linked.json()["asset"]["id"]}),
            content_type="application/json",
        )
        self.assertEqual(deleted.status_code, 200)
        self.assertTrue(
            UserLibraryObject.objects.filter(
                id=libraryObject.id, status="ready"
            ).exists()
        )
        thirdRun = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="third",
        )
        self.assertEqual(create_agent_run_authorization(thirdRun).payload["assetRefs"], [])

    def test_user_library_upload_and_download_are_owner_scoped(self):
        self.client.force_login(self.member)
        uploaded = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile(
                        "个人笔记.txt", b"private", content_type="text/plain"
                    ),
                    SimpleUploadedFile(
                        "补充资料.md", b"second", content_type="text/markdown"
                    ),
                ]
            },
        )
        self.assertEqual(uploaded.status_code, 201)
        self.assertEqual(len(uploaded.json()["objects"]), 2)
        payload = uploaded.json()["objects"][0]
        self.assertNotIn("storageKey", payload)
        self.assertEqual(payload["status"], "ready")
        self.assertTrue(payload["updatedAt"])
        download = self.client.get(f"/api/library/{payload['id']}/download")
        self.assertEqual(streaming_response_bytes(download), b"private")

        self.client.force_login(self.other)
        self.assertEqual(
            self.client.get(f"/api/library/{payload['id']}/download").status_code, 404
        )
        self.client.force_login(self.admin)
        self.assertEqual(
            self.client.get(f"/api/library/{payload['id']}/download").status_code, 404
        )

    def test_personal_library_and_session_asset_bindings_are_immutable(self):
        item = UserLibraryObject.objects.create(
            owner=self.member,
            displayName="私人.txt",
            objectKind="file",
            contentType="text/plain",
            sizeBytes=1,
            sha256=f"sha256:{'a' * 64}",
            storageKey="private/personal.txt",
            status="ready",
            contentGeneration=1,
        )
        item.owner = self.admin
        with self.assertRaisesRegex(ValueError, "ownership is immutable"):
            item.save()

        foreign_folder = UserLibraryObject.objects.create(
            owner=self.admin,
            displayName="管理员目录",
            objectKind="folder",
            contentType="application/vnd.centaeris.folder",
            sizeBytes=0,
            status="ready",
        )
        with self.assertRaisesRegex(ValueError, "parent ownership mismatch"):
            UserLibraryObject.objects.create(
                owner=self.member,
                parentFolder=foreign_folder,
                displayName="越界.txt",
                objectKind="file",
                contentType="text/plain",
                sizeBytes=1,
                sha256=f"sha256:{'b' * 64}",
                storageKey="private/cross-owner.txt",
                status="ready",
                contentGeneration=1,
            )

        captured = captured_input_fields(UserLibraryObject.objects.get(id=item.id))
        with self.assertRaisesRegex(ValueError, "attachedBy/session owner mismatch"):
            SessionAssetLink.objects.create(
                workspace=self.workspace,
                session=self.session,
                userLibraryObject_id=item.id,
                attachedBy=self.admin,
                capturedDisplayName=item.displayName,
                capturedContentType=item.contentType,
                **captured,
            )
        asset_link = SessionAssetLink.objects.create(
            workspace=self.workspace,
            session=self.session,
            userLibraryObject_id=item.id,
            attachedBy=self.member,
            capturedDisplayName=item.displayName,
            capturedContentType=item.contentType,
            **captured,
        )
        asset_link.capturedDisplayName = "改写.txt"
        with self.assertRaisesRegex(ValueError, "SessionAssetLink is immutable"):
            asset_link.save()

        provenance = UserLibraryLink.objects.create(
            libraryObject_id=item.id,
            sourceKind="upload",
        )
        provenance.sourceKind = "manual"
        with self.assertRaisesRegex(ValueError, "UserLibraryLink is immutable"):
            provenance.save()

    def test_user_library_upload_classifies_images(self):
        self.client.force_login(self.member)
        uploaded = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile("image.png", b"image", content_type="image/png")
                ]
            },
        )
        self.assertEqual(uploaded.status_code, 201)
        self.assertEqual(uploaded.json()["objects"][0]["objectKind"], "image")

    def test_user_library_upload_resolves_name_collisions_by_hash_and_number(self):
        self.client.force_login(self.member)

        def upload(content, name="报告.txt"):
            return self.client.post(
                "/api/library",
                data={
                    "files": [
                        SimpleUploadedFile(name, content, content_type="text/plain")
                    ]
                },
            )

        original = upload(b"first").json()["objects"][0]
        second = upload(b"second").json()["objects"][0]
        reused = upload(b"second").json()["objects"][0]
        third = upload(b"third").json()["objects"][0]
        same_hash_different_name = upload(b"first", "副本.txt").json()["objects"][0]

        self.assertEqual(original["displayName"], "报告.txt")
        self.assertEqual(second["displayName"], "报告(1).txt")
        self.assertEqual(reused["id"], second["id"])
        self.assertEqual(third["displayName"], "报告(2).txt")
        self.assertEqual(same_hash_different_name["id"], original["id"])
        self.assertEqual(same_hash_different_name["displayName"], "报告.txt")
        self.assertEqual(UserLibraryObject.objects.filter(owner=self.member).count(), 3)

    def test_library_upload_hash_reuse_crosses_folders_but_not_owners_or_trash(self):
        self.client.force_login(self.member)
        folder = self.client.post("/api/library/folders", data=json.dumps({"displayName": "Folder"}),
                                  content_type="application/json").json()["object"]

        def upload(name, parent=""):
            response = self.client.post("/api/library", data={"parentFolderId": parent,
                "files": [SimpleUploadedFile(name, b"shared-content", content_type="text/plain")]})
            self.assertEqual(response.status_code, 201)
            return response.json()["objects"][0]

        original = upload("original.txt", folder["id"])
        reused = upload("renamed.txt")
        self.assertEqual(reused["id"], original["id"])
        stored = UserLibraryObject.objects.get(id=reused["id"])
        self.assertEqual(stored.parentFolder_id, folder["id"])
        self.assertEqual(stored.displayName, "original.txt")
        self.client.force_login(self.admin)
        other = upload("other.txt")
        self.assertNotEqual(other["id"], original["id"])
        self.client.force_login(self.member)
        deleted = self.client.delete(f"/api/library/{original['id']}")
        self.assertEqual(deleted.status_code, 200)
        fresh = upload("fresh.txt")
        self.assertNotEqual(fresh["id"], original["id"])
        self.assertEqual(UserLibraryObject.objects.get(id=original["id"]).status, "deleted")

    def test_session_upload_reuses_matching_library_name_without_duplicate_link(self):
        self.client.force_login(self.member)
        library = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile("输入.txt", b"same", content_type="text/plain")
                ]
            },
        ).json()["objects"][0]

        def upload_to_session():
            return self.client.post(
                f"/api/sessions/{self.session.id}/uploads",
                data={
                    "files": [
                        SimpleUploadedFile(
                            "输入.txt", b"same", content_type="text/plain"
                        )
                    ]
                },
            )

        first = upload_to_session()
        second = upload_to_session()

        self.assertEqual(first.status_code, 201)
        self.assertEqual(second.status_code, 201)
        self.assertEqual(first.json()["libraryObjects"][0]["id"], library["id"])
        self.assertEqual(second.json()["libraryObjects"][0]["id"], library["id"])
        self.assertEqual(
            SessionAssetLink.objects.filter(
                session=self.session,
                userLibraryObject_id=library["id"],
            ).count(),
            1,
        )

    def test_user_library_batch_upload_rejects_the_retired_single_file_contract(self):
        self.client.force_login(self.member)
        rejected = self.client.post(
            "/api/library",
            data={
                "file": SimpleUploadedFile(
                    "旧入口.txt", b"retired", content_type="text/plain"
                )
            },
        )

        self.assertEqual(rejected.status_code, 400)
        self.assertEqual(rejected.json(), {"error": "files_required"})
        self.assertFalse(
            UserLibraryObject.objects.filter(displayName="旧入口.txt").exists()
        )

        mixed = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile("新入口.txt", b"new", content_type="text/plain")
                ],
                "file": SimpleUploadedFile(
                    "被忽略.txt", b"ignored", content_type="text/plain"
                ),
            },
        )
        self.assertEqual(mixed.status_code, 400)
        self.assertEqual(mixed.json(), {"error": "upload_fields_invalid"})
        self.assertFalse(
            UserLibraryObject.objects.filter(
                displayName__in=["新入口.txt", "被忽略.txt"]
            ).exists()
        )

    def test_user_library_batch_upload_rejects_more_than_fifty_files_before_storage(self):
        self.client.force_login(self.member)
        rejected = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile(
                        f"文件-{index}.txt", b"x", content_type="text/plain"
                    )
                    for index in range(51)
                ]
            },
        )

        self.assertEqual(rejected.status_code, 400)
        self.assertEqual(rejected.json(), {"error": "upload_batch_too_large"})
        self.assertEqual(UserLibraryObject.objects.count(), 0)

    def test_user_library_batch_upload_rolls_back_every_object_when_database_creation_fails(self):
        from .assets import delete_stored_object_for_gc
        from .http.library import _create_uploaded_library_object

        self.client.force_login(self.member)
        created = []

        def create_then_fail(user, metadata, parentFolder=None):
            if created:
                raise RuntimeError("forced_batch_failure")
            item = _create_uploaded_library_object(user, metadata, parentFolder)
            created.append(item.id)
            return item

        with (
            patch(
                "app_core.http.library._create_uploaded_library_object",
                side_effect=create_then_fail,
            ),
            patch(
                "app_core.http.library.delete_stored_object_for_gc",
                wraps=delete_stored_object_for_gc,
            ) as deleteObject,
        ):
            response = self.client.post(
                "/api/library",
                data={
                    "files": [
                        SimpleUploadedFile(
                            "第一份.txt", b"first", content_type="text/plain"
                        ),
                        SimpleUploadedFile(
                            "第二份.txt", b"second", content_type="text/plain"
                        ),
                    ]
                },
            )

        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.json(), {"error": "internal_error"})
        self.assertEqual(deleteObject.call_count, 2)
        self.assertFalse(
            UserLibraryObject.objects.filter(
                displayName__in=["第一份.txt", "第二份.txt"]
            ).exists()
        )

    def test_user_library_batch_upload_cleans_stored_bytes_when_the_second_storage_write_fails(self):
        from .assets import delete_stored_object_for_gc, store_upload

        self.client.force_login(self.member)
        stored = []

        def store_then_fail(upload, area):
            if stored:
                raise RuntimeError("forced_storage_failure")
            metadata = store_upload(upload, area)
            stored.append(metadata)
            return metadata

        with (
            patch("app_core.http.library.store_upload", side_effect=store_then_fail),
            patch(
                "app_core.http.library.delete_stored_object_for_gc",
                wraps=delete_stored_object_for_gc,
            ) as deleteObject,
        ):
            response = self.client.post(
                "/api/library",
                data={
                    "files": [
                        SimpleUploadedFile(
                            "第一份.txt", b"first", content_type="text/plain"
                        ),
                        SimpleUploadedFile(
                            "第二份.txt", b"second", content_type="text/plain"
                        ),
                    ]
                },
            )

        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.json(), {"error": "internal_error"})
        self.assertEqual(deleteObject.call_count, 1)
        self.assertFalse(default_storage.exists(stored[0]["storageKey"]))
        self.assertFalse(
            UserLibraryObject.objects.filter(
                displayName__in=["第一份.txt", "第二份.txt"]
            ).exists()
        )

    def test_user_library_upload_supports_folders_and_inline_preview(self):
        self.client.force_login(self.member)
        folder = self.client.post(
            "/api/library/folders",
            data=json.dumps({"displayName": "项目资料"}),
            content_type="application/json",
        )
        uploaded = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile(
                        "预览.txt", b"preview", content_type="text/plain"
                    )
                ],
                "parentFolderId": folder.json()["object"]["id"],
            },
        )
        self.assertEqual(uploaded.status_code, 201)
        item = uploaded.json()["objects"][0]
        self.assertEqual(item["parentFolderId"], folder.json()["object"]["id"])
        preview = self.client.get(f"/api/library/{item['id']}/preview")
        self.assertEqual(preview.status_code, 200)
        self.assertNotIn("attachment", preview["Content-Disposition"].lower())

    def test_user_library_source_code_preview_is_safe_inline_text(self):
        self.client.force_login(self.member)
        content = b"export const preview = true;\n"
        uploaded = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile(
                        "preview.tsx",
                        content,
                        content_type="application/octet-stream",
                    )
                ]
            },
        )
        self.assertEqual(uploaded.status_code, 201)
        item = uploaded.json()["objects"][0]

        preview = self.client.get(f"/api/library/{item['id']}/preview")

        self.assertEqual(preview.status_code, 200)
        self.assertEqual(preview["Content-Type"], "text/plain; charset=utf-8")
        self.assertNotIn("attachment", preview["Content-Disposition"].lower())
        self.assertEqual(streaming_response_bytes(preview), content)

    def test_library_folders_and_notes_preserve_acyclic_lifecycle(self):
        self.client.force_login(self.member)
        folder = self.client.post(
            "/api/library/folders",
            data=json.dumps({"displayName": "项目资料"}),
            content_type="application/json",
        )
        self.assertEqual(folder.status_code, 201)
        folderId = folder.json()["object"]["id"]
        childFolder = self.client.post(
            "/api/library/folders",
            data=json.dumps({"displayName": "会议纪要", "parentFolderId": folderId}),
            content_type="application/json",
        )
        self.assertEqual(childFolder.status_code, 201)
        note = self.client.post(
            "/api/library/notes",
            data=json.dumps(
                {
                    "displayName": "讨论",
                    "markdown": "# 结论",
                    "parentFolderId": folderId,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(note.status_code, 201)
        noteId = note.json()["object"]["id"]
        self.assertEqual(
            self.client.get("/api/library").json()["objects"][0]["id"], folderId
        )
        self.assertEqual(
            self.client.get(f"/api/library?parentFolderId={folderId}").json()[
                "objects"
            ][0]["id"],
            noteId,
        )
        self.assertEqual(
            self.client.get(f"/api/library/{noteId}/note").json()["markdown"], "# 结论"
        )
        updated = self.client.put(
            f"/api/library/{noteId}/note",
            data=json.dumps(
                {"displayName": "更新后的讨论", "markdown": "更新后的讨论\n\n# 正文"}
            ),
            content_type="application/json",
        )
        self.assertEqual(updated.status_code, 200)
        self.assertEqual(updated.json()["markdown"], "更新后的讨论\n\n# 正文")
        self.assertEqual(updated.json()["object"]["displayName"], "更新后的讨论")
        moved = self.client.patch(
            f"/api/library/{noteId}",
            data=json.dumps({"parentFolderId": None}),
            content_type="application/json",
        )
        self.assertEqual(moved.status_code, 200)
        self.assertIsNone(moved.json()["object"]["parentFolderId"])
        self.assertEqual(
            self.client.delete(f"/api/library/{folderId}").status_code, 409
        )
        self.assertEqual(
            self.client.delete(
                f"/api/library/{childFolder.json()['object']['id']}"
            ).status_code,
            200,
        )
        self.assertEqual(
            self.client.delete(f"/api/library/{folderId}").status_code, 200
        )
        self.assertEqual(self.client.delete(f"/api/library/{noteId}").status_code, 200)
        self.assertEqual(UserLibraryObject.objects.get(id=noteId).status, "deleted")

    def test_library_trash_restore_renumbers_and_does_not_revive_frozen_links(self):
        self.client.force_login(self.member)
        uploaded = self.client.post(
            f"/api/sessions/{self.session.id}/uploads",
            data={
                "files": [
                    SimpleUploadedFile(
                        "恢复.txt", b"original", content_type="text/plain"
                    )
                ]
            },
        )
        item_id = uploaded.json()["libraryObjects"][0]["id"]
        link_id = uploaded.json()["assets"][0]["id"]
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="old frozen link",
        )
        authorization = create_agent_run_authorization(agent_run)
        self.assertEqual(self.client.delete(f"/api/library/{item_id}").status_code, 200)

        deleted = UserLibraryObject.objects.get(id=item_id)
        self.assertEqual(deleted.deletedFromStatus, "ready")
        self.assertEqual(deleted.deletionGeneration, 1)
        trash = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"kind": "library"},
        )
        self.assertEqual([item["id"] for item in trash.json()["items"]], [item_id])
        self.assertIsNotNone(trash.json()["items"][0]["deletedAt"])

        replacement = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile(
                        "恢复.txt", b"replacement", content_type="text/plain"
                    )
                ]
            },
        )
        self.assertEqual(replacement.json()["objects"][0]["displayName"], "恢复.txt")

        restored = self.client.post(f"/api/library/{item_id}/restore")
        self.assertEqual(restored.status_code, 200)
        self.assertEqual(restored.json()["object"]["id"], item_id)
        self.assertEqual(restored.json()["object"]["displayName"], "恢复(1).txt")
        self.assertEqual(restored.json()["object"]["status"], "ready")
        self.assertEqual(restored.json()["object"]["deletionGeneration"], 1)
        restored_item = UserLibraryObject.objects.get(id=item_id)
        self.assertEqual(restored_item.contentGeneration, 2)
        self.assertEqual(restored_item.deletedFromStatus, "")
        self.assertIsNone(restored_item.deletedAt)
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash",
                {"kind": "library"},
            ).json()["items"],
            [],
        )

        with self.assertRaisesRegex(DeferredInputResolutionError, "stale_generation"):
            resolve_deferred_input(agent_run, link_id, authorization.digest)

        self.client.force_login(self.admin)
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash",
                {"kind": "library"},
            ).json()["items"],
            [],
        )

    def test_library_trash_expires_and_supports_permanent_delete(self):
        self.client.force_login(self.member)
        created = self.client.post(
            "/api/library/folders",
            data=json.dumps({"displayName": "Expired folder"}),
            content_type="application/json",
        )
        item_id = created.json()["object"]["id"]
        self.assertEqual(self.client.delete(f"/api/library/{item_id}").status_code, 200)
        UserLibraryObject.objects.filter(id=item_id).update(
            deletedAt=timezone.now() - timedelta(days=31),
        )
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/trash",
                {"kind": "library"},
            ).json()["items"],
            [],
        )
        expired = self.client.post(f"/api/library/{item_id}/restore")
        self.assertEqual(expired.status_code, 410)
        self.assertEqual(expired.json(), {"error": "library_object_expired"})
        permanent = self.client.delete(f"/api/library/{item_id}/trash")
        self.assertEqual(permanent.status_code, 200)
        self.assertIsNotNone(UserLibraryObject.objects.get(id=item_id).purgedAt)

    def test_library_trash_uses_fixed_cursor_pages(self):
        deleted_at = timezone.now()
        items = [
            UserLibraryObject.objects.create(
                owner=self.member,
                displayName=f"deleted-{index}.txt",
                objectKind="file",
                contentType="text/plain",
                status="deleted",
                deletedAt=deleted_at,
                deletedFromStatus="ready",
                deletionGeneration=1,
            )
            for index in range(51)
        ]
        self.client.force_login(self.member)

        first = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"kind": "library"},
        ).json()
        second = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": first["nextCursor"], "kind": "library"},
        ).json()

        self.assertEqual(len(first["items"]), 50)
        self.assertTrue(first["hasMore"])
        self.assertEqual(len(second["items"]), 1)
        self.assertFalse(second["hasMore"])
        self.assertEqual(
            {item["id"] for item in first["items"]}
            | {item["id"] for item in second["items"]},
            {item.id for item in items},
        )
        invalid = self.client.get(
            f"/api/workspaces/{self.workspace.id}/trash",
            {"cursor": "banana", "kind": "library"},
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json(), {"error": "trash_cursor_invalid"})

    def test_library_update_conflicts_and_restore_requires_live_parent_and_bytes(self):
        self.client.force_login(self.member)
        parent = self.client.post(
            "/api/library/folders",
            data=json.dumps({"displayName": "目标"}),
            content_type="application/json",
        ).json()["object"]
        duplicate_folder = self.client.post(
            "/api/library/folders",
            data=json.dumps({"displayName": "目标"}),
            content_type="application/json",
        )
        self.assertEqual(duplicate_folder.status_code, 409)
        self.assertEqual(duplicate_folder.json(), {"error": "library_name_conflict"})
        child = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile("冲突.txt", b"child", content_type="text/plain")
                ],
                "parentFolderId": parent["id"],
            },
        ).json()["objects"][0]
        root = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile("冲突.txt", b"root", content_type="text/plain")
                ]
            },
        ).json()["objects"][0]

        conflict = self.client.patch(
            f"/api/library/{root['id']}",
            data=json.dumps({"parentFolderId": parent["id"]}),
            content_type="application/json",
        )
        self.assertEqual(conflict.status_code, 409)
        self.assertEqual(conflict.json(), {"error": "library_name_conflict"})
        updated = self.client.patch(
            f"/api/library/{root['id']}",
            data=json.dumps(
                {"displayName": "改名.txt", "parentFolderId": parent["id"]}
            ),
            content_type="application/json",
        )
        self.assertEqual(updated.status_code, 200)
        self.assertEqual(updated.json()["object"]["displayName"], "改名.txt")
        self.assertEqual(updated.json()["object"]["parentFolderId"], parent["id"])
        self.assertEqual(
            self.client.patch(
                f"/api/library/{root['id']}",
                data=json.dumps({}),
                content_type="application/json",
            ).status_code,
            400,
        )

        self.assertEqual(self.client.delete(f"/api/library/{child['id']}").status_code, 200)
        self.assertEqual(self.client.delete(f"/api/library/{parent['id']}").status_code, 409)
        self.assertEqual(self.client.delete(f"/api/library/{root['id']}").status_code, 200)
        self.assertEqual(self.client.delete(f"/api/library/{parent['id']}").status_code, 200)
        parent_blocked = self.client.post(f"/api/library/{child['id']}/restore")
        self.assertEqual(parent_blocked.status_code, 409)
        self.assertEqual(
            parent_blocked.json(), {"error": "library_restore_parent_unavailable"}
        )
        self.assertEqual(
            self.client.post(f"/api/library/{parent['id']}/restore").status_code,
            200,
        )
        self.assertEqual(
            self.client.post(f"/api/library/{child['id']}/restore").status_code,
            200,
        )

        self.assertEqual(self.client.delete(f"/api/library/{child['id']}").status_code, 200)
        deleted_child = UserLibraryObject.objects.get(id=child["id"])
        resource = DerivedResource.objects.filter(
            ownerKind="userLibraryObject",
            ownerId=child["id"],
            resourceKind="storageObject",
            resourceKey=deleted_child.storageKey,
            deletionGeneration=deleted_child.deletionGeneration,
        ).order_by("-ownerContentGeneration").first()
        self.assertIsNotNone(resource)
        resource.state = "cleaning"
        resource.leaseOwner = "gc_test"
        resource.leaseExpiresAt = timezone.now() + timedelta(minutes=5)
        resource.save(update_fields=["state", "leaseOwner", "leaseExpiresAt"])
        busy = self.client.post(f"/api/library/{child['id']}/restore")
        self.assertEqual(busy.status_code, 409)
        self.assertEqual(busy.json(), {"error": "library_object_restore_busy"})
        resource.state = "pending"
        resource.leaseOwner = ""
        resource.leaseExpiresAt = None
        resource.save(update_fields=["state", "leaseOwner", "leaseExpiresAt"])

        stored_key = deleted_child.storageKey
        default_storage.delete(stored_key)
        missing = self.client.post(f"/api/library/{child['id']}/restore")
        self.assertEqual(missing.status_code, 409)
        self.assertEqual(missing.json(), {"error": "library_object_not_restorable"})

    def test_library_restore_rolls_back_resource_reactivation_on_database_failure(self):
        self.client.force_login(self.member)
        uploaded = self.client.post(
            "/api/library",
            data={
                "files": [
                    SimpleUploadedFile(
                        "回滚.txt", b"rollback", content_type="text/plain"
                    )
                ]
            },
        ).json()["objects"][0]
        self.assertEqual(
            self.client.delete(f"/api/library/{uploaded['id']}").status_code,
            200,
        )
        resource = DerivedResource.objects.get(
            ownerKind="userLibraryObject",
            ownerId=uploaded["id"],
            resourceKind="storageObject",
        )
        self.assertEqual(resource.state, "pending")
        original_save = UserLibraryObject.save

        def fail_restore(item, *args, **kwargs):
            if item.id == uploaded["id"] and item.status != "deleted":
                raise RuntimeError("forced_library_restore_failure")
            return original_save(item, *args, **kwargs)

        with patch.object(UserLibraryObject, "save", new=fail_restore):
            restored = self.client.post(f"/api/library/{uploaded['id']}/restore")

        self.assertEqual(restored.status_code, 500)
        self.assertEqual(restored.json(), {"error": "internal_error"})
        item = UserLibraryObject.objects.get(id=uploaded["id"])
        resource.refresh_from_db()
        self.assertEqual(item.status, "deleted")
        self.assertEqual(item.contentGeneration, 1)
        self.assertEqual(resource.state, "pending")

    def test_library_folder_cannot_become_session_asset(self):
        self.client.force_login(self.member)
        folder = self.client.post(
            "/api/library/folders",
            data=json.dumps({"displayName": "不应附加"}),
            content_type="application/json",
        )
        linked = self.client.post(
            f"/api/sessions/{self.session.id}/assets",
            data=json.dumps(
                {
                    "assetKind": "userLibraryObject",
                    "assetId": folder.json()["object"]["id"],
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(linked.status_code, 403)

    def test_revoked_source_grant_remains_a_deferred_frozen_reference(self):
        grant = SourceGrant.objects.create(
            workspace=self.workspace,
            source=self.source,
            workspaceGroup=self.workspaceGroup,
            createdBy=self.admin,
        )
        SessionAssetLink.objects.create(
            workspace=self.workspace,
            session=self.session,
            sourceObject=self.allowedObject,
            attachedBy=self.member,
            capturedDisplayName=self.allowedObject.displayName,
            capturedContentType=self.allowedObject.contentType,
            **captured_input_fields(self.allowedObject),
        )
        grant.delete()
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="after revoke",
        )
        self.assertEqual(len(create_agent_run_authorization(agent_run).payload["assetRefs"]), 1)

    @patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted")
    def test_new_session_first_message_atomically_creates_uploaded_asset_and_run(
        self, schedule_agent_run_lifecycle
    ):
        self.client.force_login(self.member)

        response = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions/new/messages",
            data={
                "operationId": "test-operation-7102",
                "text": "read the attached PDF",
                "agentId": "centaeris",
                "modelConfigRef": self.model.id,
                "files": SimpleUploadedFile(
                    "policy.pdf",
                    b"%PDF-1.4 real text fixture",
                    content_type="application/pdf",
                ),
            },
        )

        self.assertEqual(response.status_code, 202, response.content)
        session = Session.objects.get(id=response.json()["sessionId"])
        agent_run = AgentRun.objects.select_related("authorization").get(
            id=response.json()["agentRunId"]
        )
        library_object = UserLibraryObject.objects.get(
            owner=self.member,
            displayName="policy.pdf",
        )
        link = SessionAssetLink.objects.get(
            session=session,
            userLibraryObject=library_object,
        )
        self.assertEqual(agent_run.session, session)
        self.assertEqual(agent_run.authorization.payload["messageAssetRefs"], [link.id])
        self.assertEqual(
            [item["inputRef"] for item in agent_run.authorization.payload["assetRefs"]],
            [link.id],
        )
        self.assertTrue(
            UserLibraryLink.objects.filter(
                libraryObject=library_object,
                sourceKind="upload",
            ).exists()
        )
        schedule_agent_run_lifecycle.assert_called_once_with(agent_run)

        append_started(
            agent_run,
            [
                {
                    "inputRef": link.id,
                    "displayName": "policy.pdf",
                    "contentType": "application/pdf",
                }
            ],
        )
        append_completed(agent_run)
        user_event = SessionEvent.objects.get(
            agent_run=agent_run,
            payload__type="user_message",
        ).payload
        self.assertEqual(
            user_event["payload"]["attachments"],
            [
                {
                    "inputRef": link.id,
                    "displayName": "policy.pdf",
                    "contentType": "application/pdf",
                }
            ],
        )

    def test_new_session_rejects_detached_reference_without_leaving_a_record(self):
        self.client.force_login(self.member)
        session_count = Session.objects.count()
        agent_run_count = AgentRun.objects.count()

        response = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions/new/messages",
            data=json.dumps(
                {
                    "operationId": "test-operation-7174",
                    "text": "must not create a session",
                    "agentId": "centaeris",
                    "modelConfigRef": self.model.id,
                    "attachmentRefs": ["banana"],
                }
            ),
            content_type="application/json",
        )

        self.assertEqual(response.status_code, 403)
        self.assertEqual(response.json(), {"error": "attachment_not_accessible"})
        self.assertEqual(Session.objects.count(), session_count)
        self.assertEqual(AgentRun.objects.count(), agent_run_count)

    @patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted")
    def test_message_freezes_supported_thinking_mode_and_rejects_unknown_value(self, _schedule):
        self.model.thinkingModes = ["low", "vendor-high"]
        self.model.thinkingMode = "vendor-high"
        self.model.save(update_fields=["thinkingModes", "thinkingMode"])
        self.client.force_login(self.member)

        rejected = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions/new/messages",
            data=json.dumps({
                "operationId": "test-operation-7199",
                "text": "reject unknown effort",
                "agentId": "centaeris",
                "modelConfigRef": self.model.id,
                "thinkingMode": "banana",
            }),
            content_type="application/json",
        )
        self.assertEqual(rejected.status_code, 400)
        self.assertEqual(rejected.json(), {"error": "model_thinking_mode_unsupported"})

        accepted = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions/new/messages",
            data=json.dumps({
                "operationId": "test-operation-7212",
                "text": "use selected effort",
                "agentId": "centaeris",
                "modelConfigRef": self.model.id,
                "thinkingMode": "low",
            }),
            content_type="application/json",
        )
        self.assertEqual(accepted.status_code, 202, accepted.content)
        agent_run = AgentRun.objects.select_related("authorization").get(
            id=accepted.json()["agentRunId"]
        )
        self.assertEqual(agent_run.thinkingMode, "low")
        self.assertEqual(agent_run.authorization.payload["thinkingMode"], "low")

    def test_new_session_first_message_rolls_back_upload_when_authorization_fails(self):
        from .assets import delete_stored_object_for_gc

        self.client.force_login(self.member)
        before = {
            "sessions": Session.objects.count(),
            "agent_runs": AgentRun.objects.count(),
            "authorizations": AgentRunAuthorization.objects.count(),
            "libraryObjects": UserLibraryObject.objects.count(),
            "libraryLinks": UserLibraryLink.objects.count(),
            "assetLinks": SessionAssetLink.objects.count(),
        }

        with (
            patch(
                "app_core.http.workspaces.create_agent_run_authorization",
                side_effect=RuntimeError("forced_authorization_failure"),
            ),
            patch(
                "app_core.http.library.delete_stored_object_for_gc",
                wraps=delete_stored_object_for_gc,
            ) as deleteObject,
        ):
            response = self.client.post(
                f"/api/workspaces/{self.workspace.id}/sessions/new/messages",
                data={
                    "operationId": "test-operation-7252",
                    "text": "must roll back",
                    "agentId": "centaeris",
                    "modelConfigRef": self.model.id,
                    "files": SimpleUploadedFile(
                        "rollback.pdf",
                        b"%PDF-1.4 rollback fixture",
                        content_type="application/pdf",
                    ),
                },
            )

        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.json(), {"error": "internal_error"})
        self.assertEqual(deleteObject.call_count, 1)
        self.assertEqual(Session.objects.count(), before["sessions"])
        self.assertEqual(AgentRun.objects.count(), before["agent_runs"])
        self.assertEqual(AgentRunAuthorization.objects.count(), before["authorizations"])
        self.assertEqual(
            UserLibraryObject.objects.count(), before["libraryObjects"]
        )
        self.assertEqual(UserLibraryLink.objects.count(), before["libraryLinks"])
        self.assertEqual(SessionAssetLink.objects.count(), before["assetLinks"])

    @patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted")
    def test_agent_run_start_defers_tampered_attachment_validation_until_read(
        self, schedule_agent_run_lifecycle
    ):
        self.prepare_allowed_source_input(b"expected")
        with default_storage.open(self.allowedObject.storageKey, "wb") as output:
            output.write(b"tampered")
        self.client.force_login(self.member)

        response = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions/{self.session.id}/messages",
            data=json.dumps({"operationId": "test-operation-7287", "text": "must fail", "modelConfigRef": self.model.id}),
            content_type="application/json",
        )

        self.assertEqual(response.status_code, 202)
        self.assertTrue(AgentRun.objects.exists())
        schedule_agent_run_lifecycle.assert_called_once()

    @patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted")
    def test_message_attachment_identity_is_frozen_before_delayed_first_resolve(
        self, _scheduleRunLifecycle
    ):
        original = b"original"
        replacement = b"replacement"
        link = self.prepare_allowed_source_input(original)
        self.client.force_login(self.member)
        response = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sessions/{self.session.id}/messages",
            data=json.dumps(
                {
                    "operationId": "test-operation-7305",
                    "text": "read frozen attachment",
                    "modelConfigRef": self.model.id,
                    "attachmentRefs": [link.id],
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(response.status_code, 202)
        agent_run = AgentRun.objects.get(id=response.json()["agentRunId"])
        frozen = agent_run.authorization.payload["assetRefs"][0]
        self.assertEqual(frozen["inputIdentity"]["generation"], 1)
        self.assertEqual(frozen["inputIdentity"]["sha256"], self.allowedObject.sha256)

        with default_storage.open(self.allowedObject.storageKey, "wb") as output:
            output.write(replacement)
        self.allowedObject.contentGeneration += 1
        self.allowedObject.sizeBytes = len(replacement)
        self.allowedObject.sha256 = f"sha256:{hashlib.sha256(replacement).hexdigest()}"
        self.allowedObject.sourceVersion = "replacement"
        self.allowedObject.save(
            update_fields=[
                "contentGeneration",
                "sizeBytes",
                "sha256",
                "sourceVersion",
                "updatedAt",
            ]
        )

        resolved = self.client.post(
            "/internal/agent-runs/resolve-input",
            data=json.dumps(
                {
                    "schema": "runtime.deferred_input.resolve.v1",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": agent_run.authorization.digest,
                    "inputRef": link.id,
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(resolved.status_code, 409)
        self.assertEqual(resolved.json(), {"error": "stale_generation"})

    def test_session_upload_rejects_oversize_before_storage(self):
        self.client.force_login(self.member)
        with patch("app_core.http.library.MAX_DIRECT_INPUT_BYTES", 4):
            response = self.client.post(
                f"/api/sessions/{self.session.id}/uploads",
                data={
                    "files": SimpleUploadedFile(
                        "too-large.txt",
                        b"12345",
                        content_type="text/plain",
                    )
                },
            )
        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.json(), {"error": "attachment_too_large"})
        self.assertFalse(SessionAssetLink.objects.filter(session=self.session).exists())

    def test_existing_oversize_asset_cannot_be_attached(self):
        item = UserLibraryObject.objects.create(
            owner=self.member,
            displayName="too-large.bin",
            objectKind="file",
            contentType="application/octet-stream",
            sizeBytes=MAX_DIRECT_INPUT_BYTES + 1,
            sha256=f"sha256:{'a' * 64}",
            storageKey="users/too-large.bin",
            status="ready",
            contentGeneration=1,
        )
        self.client.force_login(self.member)
        response = self.client.post(
            f"/api/sessions/{self.session.id}/assets",
            data=json.dumps({"assetKind": "userLibraryObject", "assetId": item.id}),
            content_type="application/json",
        )
        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.json(), {"error": "attachment_too_large"})
        self.assertFalse(SessionAssetLink.objects.filter(session=self.session).exists())

    def test_authorized_input_read_uses_bound_async_storage_stream(self):
        content = b"authorized source bytes"
        link = self.prepare_allowed_source_input(content)
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="stream input",
        )
        authorization = create_agent_run_authorization(agent_run)
        resolved = self.client.post(
            "/internal/agent-runs/resolve-input",
            data=json.dumps(
                {
                    "schema": "runtime.deferred_input.resolve.v1",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                    "inputRef": link.id,
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        ).json()["resolvedInput"]

        response = self.client.post(
            "/internal/agent-runs/read-input",
            data=json.dumps(
                {
                    "schema": "runtime.deferred_input.read.v1",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                    "inputRef": link.id,
                    "sourceVersion": resolved["sourceVersion"],
                    "sha256": resolved["sha256"],
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )

        self.assertEqual(response.status_code, 200)
        self.assertTrue(response.is_async)
        self.assertEqual(response["Content-Length"], str(len(content)))
        self.assertEqual(response["X-Content-Sha256"], resolved["sha256"])
        self.assertEqual(response["X-Source-Version"], resolved["sourceVersion"])
        self.assertEqual(streaming_response_bytes(response), content)

        descriptor = {
            name: resolved[name]
            for name in [
                "inputRef",
                "virtualPath",
                "sizeBytes",
                "sha256",
                "sourceVersion",
            ]
        }
        validation = self.client.post(
            "/internal/agent-runs/validate-inputs",
            data=json.dumps(
                {
                    "schema": "runtime.projected_input.validate.v1",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                    "inputs": [descriptor],
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(validation.status_code, 200)
        self.assertEqual(
            validation.json(),
            {
                "schema": "runtime.projected_input.validate.v1",
                "inputs": [{"inputRef": link.id, "state": "active"}],
            },
        )
        descriptor["sizeBytes"] = 64 * 1024 * 1024 + 1
        invalid = self.client.post(
            "/internal/agent-runs/validate-inputs",
            data=json.dumps(
                {
                    "schema": "runtime.projected_input.validate.v1",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                    "inputs": [descriptor],
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(
            invalid.json(), {"error": "projected_input_validation_invalid"}
        )

    def test_agent_run_allocates_flat_stable_virtual_paths_with_collision_suffixes(self):
        links = []
        for index, content in enumerate([b"first", b"second"], start=1):
            storageKey, sizeBytes, sha256 = self.store_bytes(
                f"materialization/collision-{index}.txt", content
            )
            owner = UserLibraryObject.objects.create(
                owner=self.member,
                displayName="报告.txt",
                objectKind="file",
                contentType="text/plain",
                sizeBytes=sizeBytes,
                sha256=sha256,
                storageKey=storageKey,
                status="ready",
                contentGeneration=1,
            )
            links.append(
                SessionAssetLink.objects.create(
                    workspace=self.workspace,
                    session=self.session,
                    userLibraryObject=owner,
                    attachedBy=self.member,
                    capturedDisplayName=owner.displayName,
                    capturedContentType=owner.contentType,
                    **captured_input_fields(owner),
                )
            )
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="read both",
        )
        authorization = create_agent_run_authorization(
            agent_run, message_asset_refs=[link.id for link in links]
        )

        paths = [
            resolve_deferred_input(agent_run, link.id, authorization.digest)["virtualPath"]
            for link in links
        ]

        self.assertEqual(
            paths,
            [f"报告_{link.id}.txt" for link in links],
        )
        self.assertEqual(len({path.casefold() for path in paths}), 2)
        self.assertTrue(all("/" not in path for path in paths))

    def test_session_batch_upload_rolls_back_objects_and_links_when_the_second_link_fails(self):
        from .assets import delete_stored_object_for_gc

        self.client.force_login(self.member)
        originalSave = SessionAssetLink.save
        savedLinks = []

        def save_then_fail(link, *args, **kwargs):
            if savedLinks:
                raise RuntimeError("forced_session_link_failure")
            originalSave(link, *args, **kwargs)
            savedLinks.append(link.id)

        with (
            patch.object(SessionAssetLink, "save", new=save_then_fail),
            patch(
                "app_core.http.library.delete_stored_object_for_gc",
                wraps=delete_stored_object_for_gc,
            ) as deleteObject,
        ):
            response = self.client.post(
                f"/api/sessions/{self.session.id}/uploads",
                data={
                    "files": [
                        SimpleUploadedFile(
                            "第一份.txt", b"first", content_type="text/plain"
                        ),
                        SimpleUploadedFile(
                            "第二份.txt", b"second", content_type="text/plain"
                        ),
                    ]
                },
            )

        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.json(), {"error": "internal_error"})
        self.assertEqual(deleteObject.call_count, 2)
        self.assertFalse(SessionAssetLink.objects.filter(id__in=savedLinks).exists())
        self.assertFalse(
            UserLibraryObject.objects.filter(
                displayName__in=["第一份.txt", "第二份.txt"]
            ).exists()
        )
        self.assertFalse(
            UserLibraryLink.objects.filter(
                libraryObject__displayName__in=["第一份.txt", "第二份.txt"]
            ).exists()
        )

    def test_library_delete_tombstones_without_rewriting_session_and_reports_removed_input(
        self,
    ):
        self.client.force_login(self.member)
        uploaded = self.client.post(
            f"/api/sessions/{self.session.id}/uploads",
            data={
                "files": [
                    SimpleUploadedFile(
                        "待删除.txt", b"keep until GC", content_type="text/plain"
                    )
                ]
            },
        )
        self.assertEqual(uploaded.status_code, 201)
        attachment = UserLibraryObject.objects.get(
            id=uploaded.json()["libraryObjects"][0]["id"]
        )
        link = SessionAssetLink.objects.get(
            userLibraryObject=attachment, session=self.session
        )
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="delete after read",
        )
        authorization = create_agent_run_authorization(agent_run)
        resolved_input = resolve_deferred_input(agent_run, link.id, authorization.digest)

        deleted = self.client.delete(f"/api/library/{attachment.id}")

        self.assertEqual(deleted.status_code, 200)
        attachment.refresh_from_db()
        self.assertEqual(attachment.status, "deleted")
        self.assertIsNotNone(attachment.deletedAt)
        self.assertEqual(attachment.deletionGeneration, 1)
        self.assertTrue(default_storage.exists(attachment.storageKey))
        self.assertTrue(SessionAssetLink.objects.filter(id=link.id).exists())
        self.assertTrue(AgentRunAuthorization.objects.filter(id=authorization.id).exists())
        with self.assertRaisesRegex(DeferredInputResolutionError, "asset_removed"):
            resolve_deferred_input(agent_run, link.id, authorization.digest)

        validation = self.client.post(
            "/internal/agent-runs/validate-inputs",
            data=json.dumps(
                {
                    "schema": "runtime.projected_input.validate.v1",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                    "inputs": [
                        {
                            name: resolved_input[name]
                            for name in [
                                "inputRef",
                                "virtualPath",
                                "sizeBytes",
                                "sha256",
                                "sourceVersion",
                            ]
                        }
                    ],
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(validation.status_code, 200)
        self.assertEqual(
            validation.json(),
            {
                "schema": "runtime.projected_input.validate.v1",
                "inputs": [{"inputRef": link.id, "state": "asset_removed"}],
            },
        )

    def test_source_delete_preserves_frozen_history_and_reports_source_deleted(self):
        link = self.prepare_allowed_source_input(b"source history")
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="remember the deleted source",
        )
        authorization = create_agent_run_authorization(agent_run)
        resolved_input = resolve_deferred_input(agent_run, link.id, authorization.digest)
        citation = SessionCitationProjection.objects.create(
            citationId="citation:deleted-source",
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            sequence=1,
            inputRef=link.id,
            ownerRef=self.allowedObject.id,
            ownerKind="sourceObject",
            displayName=self.allowedObject.displayName,
            evidenceKind="workspaceSource",
            ownerSha256=self.allowedObject.sha256,
            sourceToolCallId="call-deleted-source",
            locator={"startLine": 1, "endLine": 1},
        )

        self.client.force_login(self.admin)
        deleted = self.client.delete(
            f"/api/workspaces/{self.workspace.id}/sources/{self.source.id}"
        )

        self.assertEqual(deleted.status_code, 200, deleted.content)
        self.assertTrue(SessionAssetLink.objects.filter(id=link.id).exists())
        self.assertTrue(AgentRunAuthorization.objects.filter(id=authorization.id).exists())
        self.assertTrue(
            SessionCitationProjection.objects.filter(citationId=citation.citationId).exists()
        )
        self.assertTrue(default_storage.exists(self.allowedObject.storageKey))
        with self.assertRaisesRegex(DeferredInputResolutionError, "source_deleted"):
            resolve_deferred_input(agent_run, link.id, authorization.digest)

        validation = self.client.post(
            "/internal/agent-runs/validate-inputs",
            data=json.dumps(
                {
                    "schema": "runtime.projected_input.validate.v1",
                    "agentRunId": agent_run.id,
                    "authorizationDigest": authorization.digest,
                    "inputs": [
                        {
                            name: resolved_input[name]
                            for name in [
                                "inputRef",
                                "virtualPath",
                                "sizeBytes",
                                "sha256",
                                "sourceVersion",
                            ]
                        }
                    ],
                }
            ),
            content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )
        self.assertEqual(validation.status_code, 200)
        self.assertEqual(
            validation.json(),
            {
                "schema": "runtime.projected_input.validate.v1",
                "inputs": [{"inputRef": link.id, "state": "source_deleted"}],
            },
        )

        self.client.force_login(self.member)
        download = self.client.get(
            f"/api/source-objects/{self.allowedObject.id}/download"
        )
        preview = self.client.get(f"/api/citations/{citation.citationId}/preview")
        self.assertEqual(download.status_code, 410)
        self.assertEqual(download.json(), {"error": "source_deleted"})
        self.assertEqual(preview.status_code, 410)
        self.assertEqual(preview.json(), {"error": "source_deleted"})

    def test_deleted_resource_gc_cleans_original_storage(self):
        link = self.prepare_allowed_source_input()
        storageKey = self.allowedObject.storageKey

        tombstone_stored_object(self.allowedObject)

        resource = DerivedResource.objects.get(ownerId=self.allowedObject.id)
        self.assertEqual(resource.resourceKind, "storageObject")
        self.assertEqual(resource.state, "pending")
        dryRun = io.StringIO()
        call_command(
            "gc_deleted_resources", older_than_seconds=0, dry_run=True, stdout=dryRun
        )
        self.assertIn(
            f"sourceObject:{self.allowedObject.id} generation=1 storageObject",
            dryRun.getvalue(),
        )
        resource.refresh_from_db()
        self.assertEqual(resource.state, "pending")
        self.assertEqual(resource.cleanupAttempts, 0)
        self.assertTrue(default_storage.exists(storageKey))

        output = io.StringIO()
        call_command("gc_deleted_resources", older_than_seconds=0, stdout=output)

        self.assertFalse(default_storage.exists(storageKey))
        self.assertTrue(SessionAssetLink.objects.filter(id=link.id).exists())
        resource.refresh_from_db()
        self.assertEqual(resource.state, "cleaned")
        again = io.StringIO()
        call_command("gc_deleted_resources", older_than_seconds=0, stdout=again)
        self.assertIn("Cleaned 0 deleted resources", again.getvalue())

    def test_deleted_resource_gc_expires_all_trash_domains_after_thirty_days(self):
        deleted_at = timezone.now() - timedelta(days=31)
        agent = Agent.objects.create(
            workspace=self.workspace,
            owner=self.member,
            name="Expired Agent",
        )
        agent.status = "deleted"
        agent.deletedAt = deleted_at
        agent.save(update_fields=["status", "deletedAt", "updatedAt"])
        self.session.status = "deleted"
        self.session.deletedAt = deleted_at
        self.session.save(update_fields=["status", "deletedAt", "updatedAt"])
        self.source.deletedFromStatus = self.source.status
        self.source.status = "deleted"
        self.source.deletedAt = deleted_at
        self.source.save(
            update_fields=["status", "deletedFromStatus", "deletedAt", "updatedAt"]
        )
        library = UserLibraryObject.objects.create(
            owner=self.member,
            displayName="Expired",
            objectKind="folder",
            contentType="application/vnd.centaeris.folder",
            sizeBytes=0,
            status="deleted",
            deletedAt=deleted_at,
            deletedFromStatus="ready",
            deletionGeneration=1,
        )

        output = io.StringIO()
        call_command("gc_deleted_resources", stdout=output)

        agent.refresh_from_db()
        self.session.refresh_from_db()
        self.source.refresh_from_db()
        library.refresh_from_db()
        self.allowedObject.refresh_from_db()
        self.assertIsNotNone(agent.purgedAt)
        self.assertIsNotNone(self.session.purgedAt)
        self.assertIsNotNone(self.source.purgedAt)
        self.assertIsNotNone(library.purgedAt)
        self.assertEqual(self.allowedObject.status, "deleted")
        self.assertIn("Expired 1 agents, 1 sessions, 1 sources, 1 library objects", output.getvalue())

    def test_deleted_resource_gc_records_failure_and_retries_original_storage(self):
        storageKey, sizeBytes, sha256 = self.store_bytes("gc/retry.txt", b"retry")
        item = UserLibraryObject.objects.create(
            owner=self.member,
            displayName="retry.txt",
            objectKind="file",
            contentType="text/plain",
            sizeBytes=sizeBytes,
            sha256=sha256,
            storageKey=storageKey,
            status="ready",
        )
        tombstone_stored_object(item)
        item.purgedAt = timezone.now()
        item.save(update_fields=["purgedAt", "updatedAt"])
        with patch(
            "app_core.deleted_resource_gc.delete_stored_object_for_gc",
            side_effect=OSError("storage failed"),
        ):
            with self.assertRaises(CommandError):
                call_command(
                    "gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO()
                )
        resource = DerivedResource.objects.get(
            ownerId=item.id, resourceKind="storageObject"
        )
        self.assertEqual(resource.state, "failed")
        self.assertEqual(resource.cleanupAttempts, 1)
        self.assertIn("storage failed", resource.lastFailure)

        call_command("gc_deleted_resources", older_than_seconds=0, stdout=io.StringIO())

        resource.refresh_from_db()
        self.assertEqual(resource.state, "cleaned")
        self.assertEqual(resource.cleanupAttempts, 2)
        self.assertFalse(default_storage.exists(storageKey))

    def test_orphaned_library_gc_reclaims_only_ownerless_old_bytes(self):
        isolated = FileSystemStorage(location=tempfile.mkdtemp(prefix="centaeris-orphan-gc-"))
        with (
            patch("app_core.deleted_resource_gc.default_storage", isolated),
            patch("app_core.assets.default_storage", isolated),
        ):
            orphanKey = f"users/{self.member.id}/library/unknown/orphan.txt"
            isolated.save(orphanKey, ContentFile(b"orphan"))
            retainedKey = f"users/{self.member.id}/library/known/kept.txt"
            keptKey = isolated.save(retainedKey, ContentFile(b"kept"))
            owned = UserLibraryObject.objects.create(
                owner=self.member,
                displayName="kept.txt",
                objectKind="savedArtifact",
                contentType="text/plain",
                sizeBytes=4,
                sha256=f"sha256:{hashlib.sha256(b'kept').hexdigest()}",
                storageKey=keptKey,
                status="ready",
            )
            past = timezone.now() - timedelta(days=2)
            with patch(
                "app_core.deleted_resource_gc.default_storage.get_modified_time",
                return_value=past,
            ):
                dryRun = io.StringIO()
                call_command(
                    "gc_deleted_resources",
                    older_than_seconds=0,
                    dry_run=True,
                    orphaned_library=True,
                    stdout=dryRun,
                )
                self.assertIn("Would clean orphaned library key", dryRun.getvalue())
                self.assertIn(orphanKey, dryRun.getvalue())
                self.assertNotIn(retainedKey, dryRun.getvalue())
                self.assertTrue(isolated.exists(orphanKey))
                self.assertTrue(isolated.exists(keptKey))

                output = io.StringIO()
                call_command(
                    "gc_deleted_resources",
                    older_than_seconds=0,
                    orphaned_library=True,
                    stdout=output,
                )
            self.assertFalse(isolated.exists(orphanKey))
            self.assertTrue(isolated.exists(keptKey))
            owned.refresh_from_db()
            self.assertEqual(owned.status, "ready")
            self.assertIn("Cleaned 1 orphaned library keys", output.getvalue())

    def test_orphaned_library_gc_skips_fresh_bytes_and_honors_retention(self):
        isolated = FileSystemStorage(location=tempfile.mkdtemp(prefix="centaeris-orphan-gc-"))
        with (
            patch("app_core.deleted_resource_gc.default_storage", isolated),
            patch("app_core.assets.default_storage", isolated),
        ):
            freshKey = f"users/{self.member.id}/library/fresh/recent.txt"
            isolated.save(freshKey, ContentFile(b"recent"))
            now = timezone.now()
            with patch(
                "app_core.deleted_resource_gc.default_storage.get_modified_time",
                return_value=now,
            ):
                output = io.StringIO()
                call_command(
                    "gc_deleted_resources",
                    older_than_seconds=86400,
                    orphaned_library=True,
                    stdout=output,
                )
            self.assertTrue(isolated.exists(freshKey))
            self.assertIn("Cleaned 0 orphaned library keys", output.getvalue())

    def test_deleted_library_object_remains_a_stable_historical_link(self):
        item = UserLibraryObject.objects.create(
            owner=self.member,
            displayName="历史.txt",
            objectKind="file",
            contentType="text/plain",
            sizeBytes=7,
            sha256=f"sha256:{'a' * 64}",
            storageKey="users/history.txt",
            status="ready",
            contentGeneration=1,
        )
        link = SessionAssetLink.objects.create(
            workspace=self.workspace,
            session=self.session,
            userLibraryObject=item,
            attachedBy=self.member,
            capturedDisplayName=item.displayName,
            capturedContentType=item.contentType,
            **captured_input_fields(item),
        )
        tombstone_stored_object(item)
        self.client.force_login(self.member)
        response = self.client.get(f"/api/sessions/{self.session.id}/assets")

        asset = next(
            asset for asset in response.json()["assets"] if asset["id"] == link.id
        )
        self.assertEqual(asset["asset"]["id"], item.id)
        self.assertEqual(asset["asset"]["status"], "deleted")

    def test_staging_artifact_has_no_download_and_published_artifact_uses_storage(self):
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="artifact",
        )
        artifact = Artifact.objects.create(
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            createdBy=self.member,
            displayName="draft.md",
            safeFilename="draft.md",
            contentType="text/markdown",
            sizeBytes=0,
            sha256=f"sha256:{'0' * 64}",
            storageKey="staging/draft.md",
            status="staging",
        )
        self.assertIsNone(serializeArtifact(artifact)["downloadUrl"])
        self.client.force_login(self.member)
        self.assertEqual(
            self.client.get(f"/api/artifacts/{artifact.id}/download").status_code, 404
        )

    def test_published_artifact_downloads_without_resurrecting_library_bytes(self):
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="artifact",
        )
        create_agent_run_authorization(agent_run)
        content = bytes([0, 159, 146, 150])
        storageKey, sizeBytes, sha256 = self.store_bytes("artifacts/report.bin", content)
        artifact = Artifact.objects.create(
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            createdBy=self.member,
            displayName="report.bin",
            safeFilename="report.bin",
            contentType="application/octet-stream",
            sizeBytes=sizeBytes,
            sha256=sha256,
            storageKey=storageKey,
            status="published",
            publishedAt=timezone.now(),
        )
        serialized = serializeArtifact(artifact)
        self.assertNotIn("storageKey", serialized)

        sourceCount = Source.objects.count()
        libraryCount = UserLibraryObject.objects.filter(objectKind="savedArtifact").count()
        self.client.force_login(self.member)
        download = self.client.get(f"/api/artifacts/{artifact.id}/download")
        self.assertEqual(download.status_code, 200)
        self.assertEqual(streaming_response_bytes(download), content)
        self.assertEqual(
            UserLibraryObject.objects.filter(objectKind="savedArtifact").count(),
            libraryCount,
        )
        self.assertEqual(Source.objects.count(), sourceCount)

        self.client.force_login(self.other)
        self.assertEqual(
            self.client.get(f"/api/artifacts/{artifact.id}/download").status_code, 404
        )
        self.client.force_login(self.admin)
        self.assertEqual(
            self.client.get(f"/api/artifacts/{artifact.id}/download").status_code, 404
        )

    def test_unknown_source_status_fails_loudly(self):
        for status in ("disabled", "banana"):
            self.source.status = status
            with self.assertRaisesRegex(ValueError, "unsupported Source.status"):
                self.source.save()

    def test_platform_mcp_scoped_credentials_and_current_source_revocation(self):
        from asgiref.sync import async_to_sync
        from .test_platform_mcp import call_material_tool, processing_specification

        link = self.prepare_allowed_source_input()
        run = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.member, modelConfig=self.model, prompt="read")
        authorization = create_agent_run_authorization(run)
        specification = processing_specification()
        body = {"schema": "workspace.mcp.credential.issue.v1", "agentRunId": run.id, "authorizationDigest": authorization.digest, "processingSpecification": specification, "specDigest": processing_spec_digest(specification)}
        for headers in ({}, {"HTTP_X_INTERNAL_TOKEN": settings.INTERNAL_API_TOKEN, "HTTP_ORIGIN": "http://localhost"}):
            rejected = self.client.post("/internal/mcp/credential", data=json.dumps(body), content_type="application/json", **headers)
            self.assertEqual(rejected.status_code, 401)
        headers = {"HTTP_X_INTERNAL_TOKEN": settings.INTERNAL_API_TOKEN}
        invalid = self.client.post("/internal/mcp/credential", data=json.dumps({**body, "inputRefs": [link.id]}), content_type="application/json", **headers)
        self.assertEqual(invalid.status_code, 400)
        response = self.client.post("/internal/mcp/credential", data=json.dumps(body), content_type="application/json", **headers)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response["Cache-Control"], "no-store")
        token = response.json()["accessToken"]
        listed = async_to_sync(call_material_tool)(token, "list_materials", {})
        self.assertEqual(listed.status_code, 200, listed.text)
        self.assertEqual([item["inputRef"] for item in listed.json()["result"]["structuredContent"]["items"]], [link.id])
        pending = async_to_sync(call_material_tool)(token, "read_material", {"input_ref": link.id})
        self.assertEqual(pending.json()["result"]["structuredContent"]["disposition"], "pending")
        denied = async_to_sync(call_material_tool)(token, "read_material", {"input_ref": "foreign"})
        self.assertTrue(denied.json()["result"]["isError"])
        SourceGrant.objects.filter(source=self.source).delete()
        revoked = async_to_sync(call_material_tool)(token, "list_materials", {})
        self.assertTrue(revoked.json()["result"]["isError"])
        self.member_membership.delete()
        revoked = async_to_sync(call_material_tool)(token, "list_materials", {})
        self.assertEqual(revoked.status_code, 401)

    def test_platform_task_survives_waiter_cancel_and_revocation_but_not_source_loss(self):
        from .material_access import MaterialAccessContext, bind_inputs
        from .material_operations import create_operations, operation_result
        from .material_task_source import admit_task, resolve_task_source
        from .platform_mcp_auth import authorize_context
        from .test_platform_mcp import processing_specification

        link = self.prepare_allowed_source_input()
        spec = processing_specification()
        digest = processing_spec_digest(spec)
        accesses = []
        operations = []
        for _ in range(2):
            run = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.member, modelConfig=self.model, prompt="read")
            auth = create_agent_run_authorization(run)
            access = authorize_context(MaterialAccessContext(run.id, auth.digest, spec, digest))
            identity = auth.payload["assetRefs"][0]["inputIdentity"]
            request = {"inputRef": link.id, "representationId": representation_id(identity, digest)}
            task = admit_task(access, request)
            operations.append(create_operations(access, [request])[0])
            accesses.append(access)
        self.assertEqual(task.operations.count(), 2)
        for access, operation in zip(accesses, operations):
            self.assertEqual(operation_result(access, operation["operationId"], cancel=True)["status"], "cancelled")
        self.assertEqual(resolve_task_source(task).owner.pk, self.allowedObject.pk)
        SourceGrant.objects.filter(source=self.source).delete()
        for access in accesses:
            with self.assertRaises(KnowledgeError):
                bind_inputs(access.agent_run, access.authorization_digest, [request], digest)
        self.member_membership.delete()
        # Platform processing authority comes from the material lifecycle, not
        # either run's now-revoked membership or permission.
        self.assertEqual(resolve_task_source(task).owner.pk, self.allowedObject.pk)
        Source.objects.filter(pk=self.source.pk).update(status="processing")
        with self.assertRaisesRegex(KnowledgeError, "material_source_unavailable"):
            resolve_task_source(task)

    def test_platform_task_resolves_published_artifact_but_rejects_tombstone(self):
        from .material_access import MaterialAccessContext
        from .material_task_source import admit_task, resolve_task_source
        from .platform_mcp_auth import authorize_context
        from .test_platform_mcp import processing_specification

        producer = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.member, modelConfig=self.model, prompt="produce")
        key, size, digest = self.store_bytes("platform-artifact.txt", b"artifact material")
        artifact = Artifact.objects.create(workspace=self.workspace, session=self.session, agent_run=producer,
            createdBy=self.member, displayName="artifact.txt", safeFilename="artifact.txt", contentType="text/plain",
            sizeBytes=size, sha256=digest, storageKey=key, status="published", publishedAt=timezone.now())
        link = SessionAssetLink.objects.create(workspace=self.workspace, session=self.session, artifact=artifact,
            attachedBy=self.member, capturedDisplayName=artifact.displayName, capturedContentType=artifact.contentType,
            **captured_input_fields(artifact))
        reader = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.member, modelConfig=self.model, prompt="read")
        auth = create_agent_run_authorization(reader)
        spec = processing_specification()
        spec_digest = processing_spec_digest(spec)
        access = authorize_context(MaterialAccessContext(reader.id, auth.digest, spec, spec_digest))
        identity = auth.payload["assetRefs"][0]["inputIdentity"]
        task = admit_task(access, {"inputRef": link.id, "representationId": representation_id(identity, spec_digest)})
        self.assertEqual(resolve_task_source(task).owner.pk, artifact.pk)
        tombstone_stored_object(artifact)
        with self.assertRaisesRegex(KnowledgeError, "material_source_unavailable"):
            resolve_task_source(task)

    def test_material_operations_are_durable_scoped_and_cancel_only_one_waiter(self):
        from asgiref.sync import async_to_sync
        from .material_access import MaterialAccessContext
        from .material_leases import claim
        from .models import MaterialOperation, MaterialProcessingTask
        from .platform_mcp_auth import issue_credential
        from .test_platform_mcp import call_material_tool, processing_specification

        link = self.prepare_allowed_source_input()
        tokens = []
        for _ in range(2):
            run = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.member, modelConfig=self.model, prompt="read")
            authorization = create_agent_run_authorization(run)
            specification = processing_specification()
            tokens.append(issue_credential(MaterialAccessContext(run.id, authorization.digest, specification, processing_spec_digest(specification)))["accessToken"])
        def call(token, name, args):
            response = async_to_sync(call_material_tool)(token, name, args)
            self.assertEqual(response.status_code, 200, response.text)
            return response.json()["result"]
        first = call(tokens[0], "read_material", {"input_ref": link.id})["structuredContent"]["operations"][0]
        replay = call(tokens[0], "read_material", {"input_ref": link.id})["structuredContent"]["operations"][0]
        second = call(tokens[1], "read_material", {"input_ref": link.id})["structuredContent"]["operations"][0]
        self.assertEqual(first, replay)
        self.assertNotEqual(first["operationId"], second["operationId"])
        self.assertEqual(MaterialProcessingTask.objects.count(), 1)
        self.assertEqual(MaterialOperation.objects.count(), 2)
        self.assertTrue(call(tokens[1], "get_operation", {"operation_id": first["operationId"]})["isError"])
        for _ in range(2):
            self.assertEqual(call(tokens[0], "cancel_operation", {"operation_id": first["operationId"]})["structuredContent"]["status"], "cancelled")
        task = MaterialProcessingTask.objects.get()
        self.assertEqual(set(task.payload), {"schema", "inputIdentity", "specDigest", "sizeBytes"})
        self.assertEqual(task.executionBackend, "platform")
        self.assertIsNotNone(claim(task.pk, "acceptance-worker", 300))
        self.assertEqual(call(tokens[1], "get_operation", {"operation_id": second["operationId"]})["structuredContent"]["status"], "running")
        self.assertEqual(call(tokens[0], "get_operation", {"operation_id": first["operationId"]})["structuredContent"]["status"], "cancelled")
        MaterialProcessingTask.objects.filter(pk=task.pk).update(status="failed", errorCode="processor_failed")
        self.assertEqual(call(tokens[1], "get_operation", {"operation_id": second["operationId"]})["structuredContent"]["status"], "failed")

    def test_platform_mcp_empty_scope_still_verifies_signature_and_run_binding(self):
        from .material_access import MaterialAccessContext
        from .platform_mcp_auth import CredentialRejected, issue_credential, verify_credential
        from .test_platform_mcp import processing_specification

        run = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.member, modelConfig=self.model, prompt="read")
        authorization = create_agent_run_authorization(run)
        specification = processing_specification()
        context = MaterialAccessContext(run.id, authorization.digest, specification, processing_spec_digest(specification))
        token = issue_credential(context)["accessToken"]
        self.assertEqual(verify_credential(token), context)
        original_signature = authorization.signature
        type(authorization).objects.filter(pk=authorization.pk).update(signature="invalid")
        with self.assertRaises(CredentialRejected):
            verify_credential(token)
        type(authorization).objects.filter(pk=authorization.pk).update(signature=original_signature)
        # Inject inconsistent persisted state to exercise defense in depth.
        AgentRun.objects.filter(pk=run.pk).update(user=self.admin)
        with self.assertRaises(CredentialRejected):
            verify_credential(token)

    def test_platform_commit_read_and_search_use_one_derived_representation(self):
        link = self.prepare_allowed_source_input(b"policy source")
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.member,
            modelConfig=self.model,
            prompt="research",
        )
        authorization = create_agent_run_authorization(agent_run)
        specification = {
            "schema": "knowledge.processing_specification.v1",
            "processorId": "centaeris.document.cpu",
            "processorVersion": "1.0.0",
            "executionImageDigest": f"sha256:{'1' * 64}",
            "modelDigests": {
                "PP-OCRv6_small_det": f"sha256:{'2' * 64}",
                "PP-OCRv6_small_rec": f"sha256:{'3' * 64}",
            },
            "options": {
                "renderDpi": 220,
                "maxInputBytes": 64 * 1024 * 1024,
                "maxRenderedPixelsPerPage": 16_000_000,
                "maxOutputBytes": 256 * 1024 * 1024,
            },
        }
        specDigest = processing_spec_digest(specification)
        identity = authorization.payload["assetRefs"][0]["inputIdentity"]
        representation = representation_id(identity, specDigest)
        inputBinding = {"inputRef": link.id, "representationId": representation}
        from io import BytesIO
        from .material_access import MaterialAccessContext, authorize_material_access
        from .material_contract import KnowledgeError
        from .material_operations import create_operations
        from .material_leases import claim
        from .material_reads import read_materials, search_materials
        from .platform_material_commit import ProcessingOutput, commit_processing_output
        operation_access = authorize_material_access(MaterialAccessContext(agent_run.id, authorization.digest, specification, specDigest))
        pending = read_materials(operation_access, [inputBinding], offset=0, limit=20)
        self.assertEqual(pending["disposition"], "pending")
        pending_operation = create_operations(operation_access, pending["missing"])[0]
        lease = claim(representation, "acceptance-worker", 300)

        pageText = "policy text for research"
        secondPageText = "delivery terms on the second page"
        canonical = (
            f"# {self.allowedObject.displayName}\n\n## Page 1\n\n{pageText}\n\n"
            f"## Page 2\n\n{secondPageText}\n\n"
        ).encode()
        startByte = canonical.index(pageText.encode())
        secondStartByte = canonical.index(secondPageText.encode())
        pageHash = f"sha256:{hashlib.sha256(pageText.encode()).hexdigest()}"
        secondPageHash = (
            f"sha256:{hashlib.sha256(secondPageText.encode()).hexdigest()}"
        )
        manifest = {
            "schema": "knowledge.derived_manifest.v1",
            "pageCount": 2,
            "pages": [
                {
                    "pageText": {
                        "schema": "knowledge.page_text.v1",
                        "page": 1,
                        "route": "nativeText",
                        "widthMillipoints": 1_000,
                        "heightMillipoints": 1_000,
                        "text": pageText,
                        "textSha256": pageHash,
                        "spans": [
                            {"text": pageText, "bbox": [0, 0, 10_000, 10_000]}
                        ],
                    },
                    "canonicalStartByte": startByte,
                    "canonicalEndByte": startByte + len(pageText.encode()),
                    "canonicalStartLine": 5,
                    "canonicalEndLine": 5,
                },
                {
                    "pageText": {
                        "schema": "knowledge.page_text.v1",
                        "page": 2,
                        "route": "nativeText",
                        "widthMillipoints": 1_000,
                        "heightMillipoints": 1_000,
                        "text": secondPageText,
                        "textSha256": secondPageHash,
                        "spans": [
                            {
                                "text": secondPageText,
                                "bbox": [0, 0, 10_000, 10_000],
                            }
                        ],
                    },
                    "canonicalStartByte": secondStartByte,
                    "canonicalEndByte": secondStartByte
                    + len(secondPageText.encode()),
                    "canonicalStartLine": 9,
                    "canonicalEndLine": 9,
                },
            ],
        }
        output = ProcessingOutput(representation, len(canonical),
                                  f"sha256:{hashlib.sha256(canonical).hexdigest()}",
                                  0, None, manifest)
        commit_processing_output(lease, output, BytesIO(canonical))
        self.assertEqual(DerivedRepresentation.objects.count(), 1)
        self.assertEqual(KnowledgeSegment.objects.count(), 2)
        from .material_operations import operation_result
        self.assertEqual(operation_result(operation_access, pending_operation["operationId"])["status"], "completed")

        ready = read_materials(operation_access, [inputBinding], offset=0, limit=20, full_window=True)
        self.assertEqual(ready["disposition"], "ready")
        self.assertIn(pageText, ready["items"][0]["content"])
        locator = ready["items"][0]["locator"]
        self.assertEqual(locator["kind"], "textSpan")
        self.assertEqual(locator["pageStart"], 1)
        self.assertEqual(locator["pageEnd"], 2)
        self.assertNotIn("page", locator)

        searched = search_materials(operation_access, [inputBinding], query="policy",
                                    ranking="relevance", date_range=None, limit=8)
        self.assertEqual(len(searched["hits"]), 1)
        self.assertNotIn("storageKey", json.dumps(searched))
        hit = searched["hits"][0]
        # The real MCP transport must produce exactly the shared service evidence.
        from asgiref.sync import async_to_sync
        from .material_access import MaterialAccessContext
        from .platform_mcp_auth import issue_credential
        from .test_platform_mcp import call_material_tool
        token = issue_credential(MaterialAccessContext(agent_run.id, authorization.digest, specification, specDigest))["accessToken"]
        mcp_read = async_to_sync(call_material_tool)(token, "read_material", {"input_ref": link.id, "offset": 0, "limit": 20})
        self.assertEqual(mcp_read.json()["result"]["structuredContent"]["items"], ready["items"])
        mcp_search = async_to_sync(call_material_tool)(token, "search_materials", {"query": "policy", "limit": 8})
        self.assertEqual(mcp_search.json()["result"]["structuredContent"]["hits"], searched["hits"])
        self.assertEqual(
            hit["evidenceSha256"],
            f"sha256:{hashlib.sha256(hit['content'].encode()).hexdigest()}",
        )
        self.assertEqual(
            hit["locator"]["endByte"] - hit["locator"]["startByte"],
            len(hit["content"].encode()),
        )
        citation = SessionCitationProjection.objects.create(
            citationId=f"citation:{'c' * 64}",
            workspace=self.workspace,
            session=self.session,
            agent_run=agent_run,
            sequence=1,
            inputRef=link.id,
            ownerRef=self.allowedObject.id,
            ownerKind="sourceObject",
            displayName=self.allowedObject.displayName,
            evidenceKind="workspaceSource",
            ownerSha256=self.allowedObject.sha256,
            ownerGeneration=self.allowedObject.contentGeneration,
            representationId=representation,
            specDigest=specDigest,
            evidenceSha256=hit["evidenceSha256"],
            sourceToolName="search_knowledge",
            sourceToolCallId="call-search",
            locator=hit["locator"],
        )
        self.client.force_login(self.member)
        derivedPreview = self.client.get(
            f"/api/citations/{citation.citationId}/preview"
        )
        self.assertEqual(derivedPreview.status_code, 200)
        self.assertEqual(
            derivedPreview["Content-Type"].split(";", 1)[0], "text/markdown"
        )
        self.assertEqual(streaming_response_bytes(derivedPreview), canonical)

        # Real authorization, ASGI MCP, durable evidence, Session success, then preview.
        from .material_receipts import PROVIDER_ID, MaterialEvidenceReceipt
        call_arguments = {"query": "policy", "limit": 8}
        append_session_records(agent_run, [session_record(agent_run, 1, "tool_call", {
            "callId": "material-call", "toolName": "search_materials", "providerId": PROVIDER_ID,
            "normalizedInput": call_arguments, "toolContractDigest": "sha256:" + "f" * 64,
            "displayTarget": "policy",
        })])
        bound_response = async_to_sync(call_material_tool)(token, "search_materials", call_arguments, call_id="material-call")
        bound_result = bound_response.json()["result"]
        self.assertFalse(bound_result.get("isError", False))
        receipt_output = bound_result["structuredContent"]
        self.assertEqual(MaterialEvidenceReceipt.objects.count(), 1)
        self.assertEqual(list(rebuild_agent_run_citation_projection(agent_run)), [])
        model_content = json.dumps(receipt_output)
        append_session_records(agent_run, [session_record(agent_run, 2, "tool_result", {
            "callId": "material-call", "toolName": "search_materials", "resultState": "successWithOutput",
            "modelContent": model_content, "outputComplete": True,
            "fullOutputPath": None, "outputStartByte": None, "outputByteLength": len(model_content.encode()),
            "summary": "Found material evidence", "operations": [], "modelInputImages": [], "latencyMs": 1,
        })])
        projected = rebuild_agent_run_citation_projection(agent_run)
        self.assertEqual(len(projected), 1)
        self.assertEqual(projected[0].citationId, receipt_output["citationIds"][0])
        receipt_preview = self.client.get(f"/api/citations/{projected[0].citationId}/preview")
        self.assertEqual(receipt_preview.status_code, 200)
        self.assertEqual(streaming_response_bytes(receipt_preview), canonical)
        self.assertEqual(len(rebuild_agent_run_citation_projection(agent_run)), 1)
        tool_sequence = SessionEvent.objects.get(agent_run=agent_run, payload__type="tool_call").sequence
        binding_url = f"/api/sessions/{self.session.id}/transcript/citations"
        for _ in range(2):  # Reload/reconnect uses the same receipt, never a fabricated ID.
            binding = self.client.get(binding_url, {"sequence": str(tool_sequence)})
            self.assertEqual(binding.status_code, 200, binding.content)
            bound = binding.json()["bindings"][0]
            self.assertEqual(bound["sourceToolCallId"], "material-call")
            self.assertEqual(bound["snapshot"]["agentRunId"], agent_run.id)
            self.assertEqual([item["citationId"] for item in bound["snapshot"]["citations"]], receipt_output["citationIds"])
            detail = self.client.get(bound["snapshot"]["citations"][0]["sourceUrl"])
            self.assertEqual(detail.status_code, 200)
            self.assertEqual(detail.json()["citation"]["locator"], hit["locator"])

        from dataclasses import replace
        bad_manifest = json.loads(json.dumps(manifest))
        bad_manifest["pages"][0]["pageText"]["spans"][0]["bbox"] = [10, 10, 10, 20]
        with self.assertRaises(KnowledgeError) as rejected:
            commit_processing_output(lease, replace(output, manifest=bad_manifest), BytesIO(canonical))
        self.assertEqual(rejected.exception.code, "knowledge_page_text_spans_invalid")
