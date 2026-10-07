import asyncio
import json
import threading
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from asgiref.sync import async_to_sync
from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.handlers.asgi import ASGIHandler
from django.test import Client, SimpleTestCase, TestCase, override_settings
from pydantic import ValidationError

from api.ninja_api import api
from app_core import agent_run_stream
from app_core.http import storage_stream
from app_core.http.schema import ModelResponse
from app_core.http.stream_response import OwnedAsyncStreamingHttpResponse
from app_core.http.internal_model import model_runs
from app_core.model_adapter import stream_model_async
from app_core.model_adapter.quota import async_model_attempt
from app_core.runtime_client import request_execution_profile, request_model_catalog
from app_core.models import (
    Session,
    ModelConfig,
    ModelQuotaDomain,
    ModelRunLog,
    ModelProvider,
    ProviderCredential,
    AgentRun,
    SessionEvent,
    Workspace,
)
from app_core.testing import create_session
from app_core.agent_run_authorization_factory import create_agent_run_authorization


class ModelCatalogRuntimeClientTests(SimpleTestCase):
    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_model_catalog_requires_exact_runtime_v1_envelope(self, urlopen):
        response = urlopen.return_value.__enter__.return_value
        response.read.return_value = json.dumps(
            {
                "schema": "workspace.model_catalog.result.v1",
                "catalog": {
                    "schema": "centaeris.model_catalog.v1",
                    "providers": [],
                },
            }
        ).encode()
        self.assertEqual(request_model_catalog()["providers"], [])
        request = urlopen.call_args.args[0]
        self.assertEqual(request.method, "GET")
        self.assertEqual(request.get_header("X-internal-token"), settings.INTERNAL_API_TOKEN)

        response.read.return_value = b'{"schema":"banana","catalog":{}}'
        with self.assertRaisesRegex(RuntimeError, "workspace_model_catalog_response_invalid"):
            request_model_catalog()

    @patch("app_core.runtime_client.urllib.request.urlopen")
    def test_execution_profile_requires_exact_runtime_v1_envelope(self, urlopen):
        response = urlopen.return_value.__enter__.return_value
        digest = f"sha256:{'a' * 64}"
        response.read.return_value = json.dumps(
            {
                "schema": "runtime.execution_profile.v1",
                "imageCapability": "workspace_general_v1",
                "imageDigest": digest,
            }
        ).encode()

        self.assertEqual(request_execution_profile()["imageDigest"], digest)
        request = urlopen.call_args.args[0]
        self.assertEqual(request.method, "GET")
        self.assertEqual(request.full_url, f"{settings.RUNTIME_URL}/internal/execution-profile")
        self.assertEqual(request.get_header("X-internal-token"), settings.INTERNAL_API_TOKEN)

        response.read.return_value = json.dumps(
            {
                "schema": "runtime.execution_profile.v1",
                "imageCapability": "workspace_general_v1",
                "imageDigest": f"sha256:{'A' * 64}",
            }
        ).encode()
        with self.assertRaisesRegex(RuntimeError, "runtime_execution_profile_response_invalid"):
            request_execution_profile()


class _RecordingHandle:
    def __init__(self, body: bytes):
        self.body = body
        self.offset = 0
        self.events = []
        self.close_count = 0

    def read(self, size: int) -> bytes:
        self.events.append(("read", threading.get_ident()))
        chunk = self.body[self.offset : self.offset + size]
        self.offset += len(chunk)
        return chunk

    def close(self) -> None:
        self.events.append(("close", threading.get_ident()))
        self.close_count += 1


class _BlockingCloseHandle(_RecordingHandle):
    def __init__(self, body: bytes):
        super().__init__(body)
        self.close_started = threading.Event()
        self.allow_close = threading.Event()

    def close(self) -> None:
        self.close_started.set()
        self.allow_close.wait(timeout=5)
        super().close()


class _FailingCloseHandle(_RecordingHandle):
    def close(self) -> None:
        self.close_count += 1
        raise OSError("close failed")


class NinjaContractTests(TestCase):
    def test_schema_accepts_only_camel_case_wire_names(self):
        model = ModelResponse.model_validate(
            {
                "id": "model_1",
                "displayName": "Model",
                "providerId": "provider_1",
                "providerDisplayName": "Provider",
                "modelName": "fake-model",
                "contextTokens": 8192,
                "maxOutputTokens": 1024,
                "thinkingMode": None,
                "thinkingModes": [],
            }
        )
        self.assertEqual(model.display_name, "Model")
        with self.assertRaises(ValidationError):
            ModelResponse.model_validate(
                {
                    "id": "model_1",
                    "display_name": "Model",
                    "providerId": "provider_1",
                    "providerDisplayName": "Provider",
                    "modelName": "fake-model",
                    "contextTokens": 8192,
                    "maxOutputTokens": 1024,
                    "thinkingMode": None,
                    "thinkingModes": [],
                }
            )
        with self.assertRaises(ValidationError):
            ModelResponse.model_validate(
                {
                    "id": "model_1",
                    "displayName": "Model",
                    "providerId": "provider_1",
                    "providerDisplayName": "Provider",
                    "modelName": "fake-model",
                    "contextTokens": 8192,
                    "maxOutputTokens": 1024,
                    "thinkingMode": None,
                    "thinkingModes": [],
                    "banana": True,
                }
            )

    def test_internal_operations_are_absent_from_openapi(self):
        paths = api.get_openapi_schema()["paths"]
        self.assertTrue(paths)
        self.assertFalse(any(path.startswith("/internal/") for path in paths))
        self.assertIsNone(api.docs_url)
        self.assertIsNone(api.openapi_url)

    def test_ordinary_public_json_operations_publish_success_schemas(self):
        schema = api.get_openapi_schema()
        raw_response_paths = {
            "/api/agents/{agent_id}/messages/{message_id}/files/{input_ref}/{action}",
            "/api/agents/{agent_id}/inputs/{input_id}/attachments/{input_ref}/{action}",
            "/api/agents/{agent_id}/attachments/{input_ref}/{action}",
            "/api/sessions/{session_id}/outputs/{artifact_id}/{action}",
            "/api/office-preview/{owner_kind}/{object_id}",
            "/api/spreadsheet-preview/{owner_kind}/{object_id}",
            "/api/artifacts/{artifact_id}/download",
            "/api/source-objects/{source_object_id}/download",
            "/api/library/{library_object_id}/download",
            "/api/library/{library_object_id}/preview",
            "/api/citations/{citation_id}/preview",
            "/api/sessions/{session_id}/agent-runs/{agent_run_id}/events",
            "/api/sessions/{session_id}/transcript",
            "/api/sessions/{session_id}/transcript/patches",
            "/api/sessions/{session_id}/transcript/active-agent-run",
        }
        for path, operations in schema["paths"].items():
            if not path.startswith("/api/") or path in raw_response_paths:
                continue
            for method, operation in operations.items():
                if any(str(code) == "204" for code in operation["responses"]):
                    continue
                success_responses = {
                    code: response
                    for code, response in operation["responses"].items()
                    if str(code).startswith("2") and str(code) != "204"
                }
                self.assertTrue(success_responses, f"{method.upper()} {path}")
                for code, response in success_responses.items():
                    self.assertIn(
                        "application/json",
                        response.get("content", {}),
                        f"{method.upper()} {path} {code}",
                    )

    def test_cookie_mutation_requires_csrf(self):
        user = get_user_model().objects.create_user(
            username="csrf-operation@example.test",
            password="CorrectBatteryHorse!2026",
        )
        client = Client(enforce_csrf_checks=True)
        client.force_login(user)

        rejected = client.post("/api/logout")
        self.assertEqual(rejected.status_code, 403)
        self.assertEqual(rejected.json(), {"error": "csrf_failed"})

        token = client.get("/api/csrf").json()["csrfToken"]
        accepted = client.post("/api/logout", HTTP_X_CSRFTOKEN=token)
        self.assertEqual(accepted.status_code, 200)

    def test_logout_is_csrf_protected_and_idempotent_for_anonymous_session(self):
        client = Client(enforce_csrf_checks=True)

        rejected = client.post("/api/logout")
        self.assertEqual(rejected.status_code, 403)
        self.assertEqual(rejected.json(), {"error": "csrf_failed"})

        token = client.get("/api/csrf").json()["csrfToken"]
        accepted = client.post("/api/logout", HTTP_X_CSRFTOKEN=token)
        self.assertEqual(accepted.status_code, 200)
        self.assertEqual(accepted.json(), {"ok": True})

    def test_malformed_json_uses_operation_error_contract(self):
        user = get_user_model().objects.create_user(
            username="malformed-source@example.test",
            password="CorrectBatteryHorse!2026",
            is_staff=True,
        )
        workspace = Workspace.objects.create(name="Malformed workspace", createdBy=user)
        workspace.members.add(user)
        self.client.force_login(user)

        response = self.client.post(
            f"/api/workspaces/{workspace.id}/sources",
            data="{",
            content_type="application/json",
        )

        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.json(), {"error": "invalid_source"})

    def test_login_malformed_json_requires_csrf_then_returns_invalid_json(self):
        client = Client(enforce_csrf_checks=True)
        token = client.get("/api/csrf").json()["csrfToken"]

        without_csrf = Client(enforce_csrf_checks=True).post(
            "/api/login",
            data="{",
            content_type="application/json",
        )
        malformed = client.post(
            "/api/login",
            data="{",
            content_type="application/json",
            HTTP_X_CSRFTOKEN=token,
        )

        self.assertEqual(without_csrf.status_code, 403)
        self.assertEqual(without_csrf.json(), {"error": "csrf_failed"})
        self.assertEqual(malformed.status_code, 400)
        self.assertEqual(malformed.json(), {"error": "invalid_json"})

    def test_asset_validation_preserves_authorization_status_contracts(self):
        user = get_user_model().objects.create_user(
            username="asset-schema@example.test",
            password="CorrectBatteryHorse!2026",
        )
        workspace = Workspace.objects.create(name="Asset schema", createdBy=user)
        workspace.members.add(user)
        session = create_session(workspace=workspace, owner=user)
        self.client.force_login(user)

        attach = self.client.post(
            f"/api/sessions/{session.id}/assets",
            data="{}",
            content_type="application/json",
        )
        detach = self.client.delete(
            f"/api/sessions/{session.id}/assets",
            data="{}",
            content_type="application/json",
        )

        self.assertEqual(attach.status_code, 403)
        self.assertEqual(attach.json(), {"error": "asset_not_accessible"})
        self.assertEqual(detach.status_code, 404)
        self.assertEqual(detach.json(), {"error": "asset_link_not_found"})

    def test_public_json_rejects_python_field_names_and_unknown_fields(self):
        user = get_user_model().objects.create_user(
            username="schema-admin@example.test",
            password="CorrectBatteryHorse!2026",
            is_staff=True,
        )
        workspace = Workspace.objects.create(name="Schema workspace", createdBy=user)
        workspace.members.add(user)
        self.client.force_login(user)

        snake_case = self.client.post(
            f"/api/workspaces/{workspace.id}/sources",
            data=json.dumps(
                {"source_type": "uploadedFile", "name": "Source"}
            ),
            content_type="application/json",
        )
        unknown = self.client.post(
            f"/api/workspaces/{workspace.id}/sources",
            data=json.dumps(
                {
                    "sourceType": "uploadedFile",
                    "name": "Source",
                    "banana": True,
                }
            ),
            content_type="application/json",
        )

        self.assertEqual(snake_case.status_code, 400)
        self.assertEqual(snake_case.json(), {"error": "invalid_source"})
        self.assertEqual(unknown.status_code, 400)
        self.assertEqual(unknown.json(), {"error": "invalid_source"})

    def test_response_schema_loud_fails_before_unknown_field_can_leak(self):
        user = get_user_model().objects.create_user(
            username="response-schema@example.test",
            password="CorrectBatteryHorse!2026",
        )
        workspace = Workspace.objects.create(
            name="Response schema workspace",
            createdBy=user,
        )
        workspace.members.add(user)
        self.client.force_login(user)

        leaked_projection = {
            "id": str(workspace.id),
            "name": workspace.name,
            "description": "",
            "status": "active",
            "role": "member",
            "banana": "must-not-cross-the-transport-boundary",
        }
        with patch(
            "app_core.http.workspaces.serialize_workspace",
            return_value=leaked_projection,
        ):
            response = self.client.get("/api/workspaces")

        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.json(), {"error": "internal_error"})
        self.assertNotContains(response, "banana", status_code=500)


    def test_csrf_protected_session_delete_remains_available_to_ninja(self):
        user = get_user_model().objects.create_user(
            username="csrf-json@example.test",
            password="CorrectBatteryHorse!2026",
        )
        workspace = Workspace.objects.create(name="CSRF workspace", createdBy=user)
        workspace.members.add(user)
        session = create_session(workspace=workspace, owner=user)
        client = Client(enforce_csrf_checks=True)
        client.force_login(user)
        token = client.get("/api/csrf").json()["csrfToken"]

        response = client.delete(
            f"/api/sessions/{session.id}",
            HTTP_X_CSRFTOKEN=token,
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"deleted": True})

    def test_browser_redis_pools_share_one_process_connection_budget(self):
        self.assertEqual(
            agent_run_stream._redis_sync_pool.max_connections,
            settings.REDIS_BROWSER_SYNC_MAX_CONNECTIONS,
        )
        self.assertIs(
            agent_run_stream._redis_sync_client.connection_pool,
            agent_run_stream._redis_sync_pool,
        )
        self.assertEqual(
            agent_run_stream._redis_stream_pool.max_connections,
            settings.REDIS_BROWSER_LIVE_MAX_CONNECTIONS,
        )
        self.assertIs(
            agent_run_stream._redis_stream_client.connection_pool,
            agent_run_stream._redis_stream_pool,
        )
        self.assertGreaterEqual(agent_run_stream._redis_sync_pool.max_connections, 1)
        self.assertGreaterEqual(agent_run_stream._redis_stream_pool.max_connections, 1)
        self.assertEqual(
            agent_run_stream._redis_sync_pool.max_connections
            + agent_run_stream._redis_stream_pool.max_connections,
            settings.REDIS_BROWSER_MAX_CONNECTIONS,
        )

    def test_session_stream_uses_exact_signal_field_and_open_state_has_no_overlay(self):
        agent_run = SimpleNamespace(id="agent_run_1", session_id="session_1")
        signal = {
            "schema": "agent_run.transient.signal.v1",
            "kind": "live",
            "agentRunId": agent_run.id,
            "afterSequence": 2,
            "revision": 1,
            "turnId": "turn_1",
            "messageId": "message:turn_1:assistant",
            "text": "streaming",
        }
        self.assertEqual(
            agent_run_stream._decode_signal(
                agent_run, {"signal": json.dumps(signal)}
            ),
            signal,
        )
        with self.assertRaisesRegex(RuntimeError, "fields mismatch"):
            agent_run_stream._decode_signal(agent_run, {"item": json.dumps(signal)})

        wake = {
            "schema": "agent_run.transient.signal.v1",
            "kind": "commit_wake",
            "agentRunId": agent_run.id,
            "highWaterSequence": 3,
        }
        self.assertEqual(
            agent_run_stream._decode_signal(agent_run, {"signal": json.dumps(wake)}),
            wake,
        )
        wake["event"] = {"banana": True}
        with self.assertRaisesRegex(RuntimeError, "fields mismatch"):
            agent_run_stream._decode_signal(agent_run, {"signal": json.dumps(wake)})

        self.assertIsNone(
            agent_run_stream._live_state(
                {
                "messageId": signal["messageId"],
                "turnId": signal["turnId"],
                "afterSequence": "2",
                "revision": "0",
                },
                "",
            )
        )


class AgentRunStreamContractTests(SimpleTestCase):
    agent_run = SimpleNamespace(
        id="agent_run_1", session_id="session_1", status="running"
    )

    @staticmethod
    def _record(sequence, event_type, *, turn_id="turn_1", payload=None):
        event_id = f"event_{sequence}"
        return {
            "sequence": sequence,
            "eventId": event_id,
            "payload": {
                "schemaVersion": "session.event.v1",
                "eventVersion": 1,
                "sequence": sequence,
                "type": event_type,
                "eventId": event_id,
                "sessionId": "session_1",
                "agentRunId": "agent_run_1",
                "turnId": turn_id,
                "createdAtMs": sequence,
                "payload": payload or {},
            },
        }

    @staticmethod
    def _live(after_sequence, revision=2):
        return {
            "afterSequence": after_sequence,
            "revision": revision,
            "turnId": "turn_1",
            "messageId": "message:turn_1:assistant",
            "text": "working",
        }

    def test_cursor_v1_is_exact_bound_and_rejects_unknown_values(self):
        cursor = agent_run_stream.encode_stream_cursor("agent_run_1", 128)
        self.assertEqual(
            agent_run_stream.parse_last_event_cursor(cursor, "agent_run_1"), 128
        )
        self.assertEqual(
            agent_run_stream.parse_last_event_cursor("0-0", "agent_run_1"), 0
        )
        with self.assertRaisesRegex(ValueError, "last_event_id_invalid"):
            agent_run_stream.parse_last_event_cursor("1723456789-0", "agent_run_1")
        with self.assertRaisesRegex(ValueError, "last_event_id_invalid"):
            agent_run_stream.parse_last_event_cursor(cursor, "banana")

        invalid_payloads = [
            {
                "schema": "session.stream.cursor.v1",
                "agentRunId": "agent_run_1",
                "sourceSequence": invalid_sequence,
            }
            for invalid_sequence in (True, 0, -1)
        ]
        invalid_payloads.append(
            {
                "schema": "session.stream.cursor.v1",
                "agentRunId": "agent_run_1",
                "sourceSequence": 1,
                "banana": True,
            }
        )
        for payload in invalid_payloads:
            raw = json.dumps(payload, separators=(",", ":"), sort_keys=True).encode()
            invalid = (
                "v1."
                + agent_run_stream.base64.urlsafe_b64encode(raw)
                .decode()
                .rstrip("=")
            )
            with self.assertRaisesRegex(ValueError, "last_event_id_invalid"):
                agent_run_stream.parse_last_event_cursor(invalid, "agent_run_1")
        for invalid in ("banana.a", "v1.a", "v1.***", "v1." + "a" * 513):
            with self.assertRaisesRegex(ValueError, "last_event_id_invalid"):
                agent_run_stream.parse_last_event_cursor(invalid, "agent_run_1")

    def test_future_cursor_loud_fails_against_session_high_water(self):
        with patch.object(agent_run_stream, "load_session_high_water", return_value=12):
            with self.assertRaisesRegex(ValueError, "last_event_id_invalid"):
                agent_run_stream.require_cursor_not_future(self.agent_run, 13)

    def test_phase_commit_suppresses_late_old_live_but_preserves_equal_anchor(self):
        state = agent_run_stream._new_tail_state(10)
        phase = self._record(11, "phase_event", payload={"stage": "model_process_summary"})

        async def scenario():
            with patch.object(
                agent_run_stream,
                "_load_postgres_page",
                new=AsyncMock(return_value=([phase], 11)),
            ), patch.object(
                agent_run_stream,
                "_load_terminal_sequence",
                new=AsyncMock(return_value=None),
            ), patch.object(
                agent_run_stream,
                "_load_committed_overlay_projection",
                new=AsyncMock(return_value=(11, False)),
            ):
                committed = [
                    item
                    async for item in agent_run_stream._drain_postgres(
                        self.agent_run, state
                    )
                ]
                live = [
                    item
                    async for item in agent_run_stream._stream_live_candidate(
                        self.agent_run, state, self._live(10)
                    )
                ]
                next_live = [
                    item
                    async for item in agent_run_stream._stream_live_candidate(
                        self.agent_run, state, self._live(11, revision=3)
                    )
                ]
                return committed, live, next_live

        committed, live, next_live = async_to_sync(scenario)()
        self.assertIn('"type":"phase_event"', committed[0])
        self.assertEqual(live, [])
        self.assertIn('"kind":"live"', next_live[0])
        self.assertIn('"afterSequence":11', next_live[0])

    def test_unrelated_committed_event_does_not_advance_overlay_barrier(self):
        barriers = {}
        tool_call = self._record(11, "tool_call")["payload"]

        agent_run_stream.advance_overlay_barrier(barriers, tool_call, 11)

        self.assertEqual(barriers, {})
        self.assertFalse(
            agent_run_stream.live_overlay_is_superseded(self._live(10), barriers)
        )

    def test_overlay_barrier_replay_is_idempotent(self):
        barriers = {}
        phase = self._record(11, "phase_event")["payload"]

        agent_run_stream.advance_overlay_barrier(barriers, phase, 11)
        agent_run_stream.advance_overlay_barrier(barriers, phase, 11)

        self.assertEqual(barriers, {"turn_1": 11})

    def test_hidden_global_sequence_gap_does_not_make_live_stale(self):
        state = agent_run_stream._new_tail_state(10)

        async def scenario():
            with patch.object(
                agent_run_stream,
                "_load_postgres_page",
                new=AsyncMock(return_value=([], 13)),
            ), patch.object(
                agent_run_stream,
                "_load_terminal_sequence",
                new=AsyncMock(return_value=None),
            ), patch.object(
                agent_run_stream,
                "_load_committed_overlay_projection",
                new=AsyncMock(return_value=(0, False)),
            ):
                self.assertEqual(
                    [
                        item
                        async for item in agent_run_stream._drain_postgres(
                            self.agent_run, state
                        )
                    ],
                    [],
                )
                return [
                    item
                    async for item in agent_run_stream._stream_live_candidate(
                        self.agent_run, state, self._live(13)
                    )
                ]

        live = async_to_sync(scenario)()
        self.assertEqual(len(live), 1)
        self.assertIn('"afterSequence":13', live[0])

    def test_sealed_live_is_suppressed_and_postgres_failure_is_not_hidden(self):
        state = agent_run_stream._new_tail_state(10)
        state["session_high_water"] = 10

        async def scenario():
            with patch.object(
                agent_run_stream,
                "_load_committed_overlay_projection",
                new=AsyncMock(return_value=(0, True)),
            ):
                live = [
                    item
                    async for item in agent_run_stream._stream_live_candidate(
                        self.agent_run, state, self._live(10)
                    )
                ]
            with patch.object(
                agent_run_stream,
                "_load_postgres_page",
                new=AsyncMock(side_effect=RuntimeError("postgres unavailable")),
            ):
                with self.assertRaisesRegex(RuntimeError, "postgres unavailable"):
                    _ = [
                        item
                        async for item in agent_run_stream._drain_postgres(
                            self.agent_run, state
                        )
                    ]
            return live

        self.assertEqual(async_to_sync(scenario)(), [])

    def test_redis_unavailable_still_streams_postgres_terminal(self):
        terminal = self._record(
            11,
            "agent_run_completed",
            payload={"doneReason": "finalized"},
        )

        async def scenario():
            with patch.object(
                agent_run_stream,
                "_capture_signal_tail",
                new=AsyncMock(side_effect=agent_run_stream.redis.ConnectionError("down")),
            ), patch.object(
                agent_run_stream,
                "_load_postgres_page",
                new=AsyncMock(return_value=([terminal], 11)),
            ):
                return [
                    item
                    async for item in agent_run_stream.stream_agent_run_session_items_async(
                        self.agent_run, 10
                    )
                ]

        items = async_to_sync(scenario)()
        self.assertEqual(len(items), 1)
        self.assertIn('"type":"agent_run_completed"', items[0])
        self.assertTrue(items[0].startswith("id: v1."))

    def test_live_traffic_reconciles_postgres_when_commit_wake_is_missing(self):
        terminal = self._record(
            11,
            "agent_run_completed",
            payload={"doneReason": "finalized"},
        )
        signal = {
            "schema": "agent_run.transient.signal.v1",
            "kind": "live",
            "agentRunId": self.agent_run.id,
            "afterSequence": 10,
            "revision": 2,
            "turnId": "turn_1",
            "messageId": "message:turn_1:assistant",
            "text": "working",
        }

        async def scenario():
            with patch.object(
                agent_run_stream,
                "_capture_signal_tail",
                new=AsyncMock(return_value="1-0"),
            ), patch.object(
                agent_run_stream,
                "_load_live_text_state_async",
                new=AsyncMock(return_value=None),
            ), patch.object(
                agent_run_stream,
                "_load_postgres_page",
                new=AsyncMock(side_effect=[([], 10), ([terminal], 11)]),
            ), patch.object(
                agent_run_stream,
                "_load_terminal_sequence",
                new=AsyncMock(return_value=None),
            ), patch.object(
                agent_run_stream._redis_stream_client,
                "xread",
                new=AsyncMock(
                    return_value=[
                        (
                            "signals",
                            [("2-0", {"signal": json.dumps(signal)})],
                        )
                    ]
                ),
            ):
                return [
                    item
                    async for item in agent_run_stream.stream_agent_run_session_items_async(
                        self.agent_run, 10
                    )
                ]

        items = async_to_sync(scenario)()
        self.assertEqual(len(items), 1)
        self.assertIn('"type":"agent_run_completed"', items[0])

    def test_monotonic_deadline_reconciles_without_any_redis_signal(self):
        terminal = self._record(
            11,
            "agent_run_completed",
            payload={"doneReason": "finalized"},
        )

        async def scenario():
            with patch.object(
                agent_run_stream,
                "_capture_signal_tail",
                new=AsyncMock(return_value="1-0"),
            ), patch.object(
                agent_run_stream,
                "_load_live_text_state_async",
                new=AsyncMock(return_value=None),
            ), patch.object(
                agent_run_stream,
                "_load_postgres_page",
                new=AsyncMock(side_effect=[([], 10), ([terminal], 11)]),
            ), patch.object(
                agent_run_stream,
                "_load_terminal_sequence",
                new=AsyncMock(return_value=None),
            ), patch.object(
                agent_run_stream.time,
                "monotonic",
                side_effect=[0.0, 5.0],
            ), patch.object(
                agent_run_stream._redis_stream_client,
                "xread",
                new=AsyncMock(return_value=[]),
            ) as xread:
                items = [
                    item
                    async for item in agent_run_stream.stream_agent_run_session_items_async(
                        self.agent_run, 10
                    )
                ]
                return items, xread.await_count

        items, read_count = async_to_sync(scenario)()
        self.assertEqual(read_count, 0)
        self.assertEqual(len(items), 1)
        self.assertIn('"type":"agent_run_completed"', items[0])


class AsyncStorageStreamTests(TestCase):
    @override_settings(STORAGE_STREAM_CHUNK_BYTES=2)
    def test_handle_operations_keep_lane_affinity_and_close_exactly_once(self):
        handle = _RecordingHandle(b"abcdef")
        pool = storage_stream._StorageLanePool(1)
        open_threads = []

        def open_handle(key, mode):
            open_threads.append(threading.get_ident())
            return handle

        async def scenario():
            stream = await storage_stream.open_storage_stream("opaque/key")
            iterator = stream.__aiter__()
            first = await anext(iterator)
            await iterator.aclose()
            return first

        with (
            patch.object(storage_stream, "_lane_pool", pool),
            patch.object(storage_stream.default_storage, "exists", return_value=True),
            patch.object(storage_stream.default_storage, "open", side_effect=open_handle),
        ):
            first = async_to_sync(scenario)()

        self.assertEqual(first, b"ab")
        self.assertEqual(handle.close_count, 1)
        operation_threads = open_threads + [
            thread_id for _, thread_id in handle.events
        ]
        self.assertEqual(len(set(operation_threads)), 1)

    @override_settings(STORAGE_STREAM_CHUNK_BYTES=2)
    def test_capacity_exhaustion_returns_exact_retryable_503(self):
        handles = [_RecordingHandle(b"first"), _RecordingHandle(b"second")]
        pool = storage_stream._StorageLanePool(1)

        def open_handle(key, mode):
            return handles.pop(0)

        async def consume(stream):
            return b"".join([chunk async for chunk in stream])

        async def scenario():
            occupied = await storage_stream.open_storage_stream("first")
            saturated = await storage_stream.stored_file_response(
                "second",
                "application/octet-stream",
                "second.bin",
            )
            occupied_body = await consume(occupied)
            recovered = await storage_stream.open_storage_stream("second")
            recovered_body = await consume(recovered)
            return saturated, occupied_body, recovered_body

        with (
            patch.object(storage_stream, "_lane_pool", pool),
            patch.object(storage_stream.default_storage, "exists", return_value=True),
            patch.object(storage_stream.default_storage, "open", side_effect=open_handle),
        ):
            saturated, occupied_body, recovered_body = async_to_sync(scenario)()

        self.assertEqual(saturated.status_code, 503)
        self.assertEqual(
            json.loads(saturated.content),
            {"error": "storage_stream_capacity_exhausted"},
        )
        self.assertEqual(occupied_body, b"first")
        self.assertEqual(recovered_body, b"second")

    @override_settings(STORAGE_STREAM_CHUNK_BYTES=2)
    def test_asgi_send_cancellation_closes_handle_and_releases_lane(self):
        handles = [_RecordingHandle(b"first"), _RecordingHandle(b"second")]
        first_handle = handles[0]
        pool = storage_stream._StorageLanePool(1)

        async def scenario():
            response = await storage_stream.stored_file_response(
                "first",
                "application/octet-stream",
                "first.bin",
            )

            async def cancelled_send(message):
                if message["type"] == "http.response.body" and message.get("body"):
                    raise asyncio.CancelledError

            try:
                await ASGIHandler().send_response(response, cancelled_send)
            except asyncio.CancelledError:
                pass
            recovered = await storage_stream.open_storage_stream("second")
            recovered_body = b"".join([chunk async for chunk in recovered])
            return recovered_body

        with (
            patch.object(storage_stream, "_lane_pool", pool),
            patch.object(storage_stream.default_storage, "exists", return_value=True),
            patch.object(
                storage_stream.default_storage,
                "open",
                side_effect=lambda key, mode: handles.pop(0),
            ),
        ):
            recovered_body = async_to_sync(scenario)()

        self.assertEqual(first_handle.close_count, 1)
        self.assertEqual(recovered_body, b"second")

    @override_settings(STORAGE_STREAM_CHUNK_BYTES=2)
    def test_cancellation_during_open_closes_handle_and_releases_lane(self):
        first_handle = _RecordingHandle(b"first")
        handles = [first_handle, _RecordingHandle(b"second")]
        open_started = threading.Event()
        allow_open = threading.Event()
        pool = storage_stream._StorageLanePool(1)

        def open_handle(key, mode):
            if key == "first":
                open_started.set()
                allow_open.wait(timeout=5)
            return handles.pop(0)

        async def scenario():
            opening = asyncio.create_task(
                storage_stream.open_storage_stream("first")
            )
            started = await asyncio.to_thread(open_started.wait, 2)
            self.assertTrue(started)
            opening.cancel()
            allow_open.set()
            with self.assertRaises(asyncio.CancelledError):
                await opening
            recovered = await storage_stream.open_storage_stream("second")
            return b"".join([chunk async for chunk in recovered])

        with (
            patch.object(storage_stream, "_lane_pool", pool),
            patch.object(storage_stream.default_storage, "exists", return_value=True),
            patch.object(
                storage_stream.default_storage,
                "open",
                side_effect=open_handle,
            ),
        ):
            recovered_body = async_to_sync(scenario)()

        self.assertEqual(first_handle.close_count, 1)
        self.assertEqual(recovered_body, b"second")

    @override_settings(STORAGE_STREAM_CHUNK_BYTES=2)
    def test_cancellation_during_close_returns_healthy_lane(self):
        first_handle = _BlockingCloseHandle(b"a")
        handles = [first_handle, _RecordingHandle(b"second")]
        pool = storage_stream._StorageLanePool(1)

        async def scenario():
            stream = await storage_stream.open_storage_stream("first")
            self.assertEqual(await anext(stream), b"a")
            eof_task = asyncio.create_task(anext(stream))
            close_started = await asyncio.to_thread(
                first_handle.close_started.wait,
                2,
            )
            self.assertTrue(close_started)
            eof_task.cancel()
            first_handle.allow_close.set()
            with self.assertRaises(asyncio.CancelledError):
                await eof_task
            recovered = await storage_stream.open_storage_stream("second")
            return b"".join([chunk async for chunk in recovered])

        with (
            patch.object(storage_stream, "_lane_pool", pool),
            patch.object(storage_stream.default_storage, "exists", return_value=True),
            patch.object(
                storage_stream.default_storage,
                "open",
                side_effect=lambda key, mode: handles.pop(0),
            ),
        ):
            recovered_body = async_to_sync(scenario)()

        self.assertEqual(first_handle.close_count, 1)
        self.assertEqual(recovered_body, b"second")

    @override_settings(STORAGE_STREAM_CHUNK_BYTES=2)
    def test_close_failure_replaces_retired_lane(self):
        first_handle = _FailingCloseHandle(b"first")
        handles = [first_handle, _RecordingHandle(b"second")]
        pool = storage_stream._StorageLanePool(1)

        async def scenario():
            first = await storage_stream.open_storage_stream("first")
            first_body = b"".join([chunk async for chunk in first])
            recovered = await storage_stream.open_storage_stream("second")
            recovered_body = b"".join([chunk async for chunk in recovered])
            return first_body, recovered_body

        with (
            patch.object(storage_stream, "_lane_pool", pool),
            patch.object(storage_stream.default_storage, "exists", return_value=True),
            patch.object(
                storage_stream.default_storage,
                "open",
                side_effect=lambda key, mode: handles.pop(0),
            ),
        ):
            first_body, recovered_body = async_to_sync(scenario)()

        self.assertEqual(first_handle.close_count, 1)
        self.assertEqual(first_body, b"first")
        self.assertEqual(recovered_body, b"second")


class ModelRunQueueCancellationTests(SimpleTestCase):
    def test_dropped_sync_proxy_request_signals_queued_provider_work(self):
        started = threading.Event()
        observed_cancel = threading.Event()

        def blocked_run_model(*, cancel_event, **_kwargs):
            started.set()
            if cancel_event.wait(2):
                observed_cancel.set()
            return {}

        request = SimpleNamespace(
            read=lambda _limit: b'{"agentRunId":"run"}', headers={}
        )

        async def exercise():
            task = asyncio.create_task(model_runs(request))
            self.assertTrue(await asyncio.to_thread(started.wait, 2))
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task

        with patch(
            "app_core.http.internal_model._validate_model_run",
            new=AsyncMock(return_value="model"),
        ), patch(
            "app_core.http.internal_model.run_model", side_effect=blocked_run_model
        ):
            async_to_sync(exercise)()
        self.assertTrue(observed_cancel.is_set())


class AsyncProviderStreamTests(TestCase):
    def test_disconnect_closes_without_success_audit(self):
        model = ModelConfig.objects.create(
            displayName="Streaming fake",
            modelName="fake-streaming",
        )
        prepared_prompt = {
            "schema": "prepared_prompt.v1",
            "messages": [
                {
                    "messageId": "message_user",
                    "role": "user",
                    "content": "hello",
                }
            ],
            "toolDefinitions": [],
            "toolChoice": {"type": "none"},
            "maxOutputTokens": model.maxOutputTokens,
        }

        async def disconnect():
            stream = stream_model_async(
                "agent_run_cancelled",
                model.id,
                {"preparedPrompt": prepared_prompt},
            )
            await anext(stream)
            await stream.aclose()

        async_to_sync(disconnect)()

        audit = ModelRunLog.objects.get(agentRunId="agent_run_cancelled")
        self.assertEqual(audit.status, "error")
        self.assertEqual(audit.error, "provider_stream_cancelled")

    def test_terminal_send_cancellation_records_one_error_outcome(self):
        model = ModelConfig.objects.create(
            displayName="Terminal boundary fake",
            modelName="fake-terminal-boundary",
        )
        prepared_prompt = {
            "schema": "prepared_prompt.v1",
            "messages": [
                {
                    "messageId": "message_user",
                    "role": "user",
                    "content": "hello",
                }
            ],
            "toolDefinitions": [],
            "toolChoice": {"type": "none"},
            "maxOutputTokens": model.maxOutputTokens,
        }

        async def cancel_terminal_send():
            response = OwnedAsyncStreamingHttpResponse(
                stream_model_async(
                    "agent_run_terminal_cancelled",
                    model.id,
                    {"preparedPrompt": prepared_prompt},
                ),
                content_type="text/event-stream",
            )

            async def send(message):
                if (
                    message["type"] == "http.response.body"
                    and b"event: result" in message.get("body", b"")
                ):
                    raise asyncio.CancelledError

            try:
                await ASGIHandler().send_response(response, send)
            except asyncio.CancelledError:
                pass

        async_to_sync(cancel_terminal_send)()

        audits = list(ModelRunLog.objects.filter(agentRunId="agent_run_terminal_cancelled"))
        self.assertEqual(len(audits), 1)
        self.assertEqual(audits[0].status, "error")
        self.assertEqual(audits[0].error, "provider_stream_cancelled")

    def test_provider_disconnect_closes_sdk_client_exactly_once(self):
        admin = get_user_model().objects.create_superuser(
            username="provider-admin@example.com",
            password="password",
        )
        credential = ProviderCredential.objects.create(
            provider=ModelProvider.objects.create(
                displayName="Provider cancellation",
                api="openai-completions",
                apiBase="https://api.deepseek.com",
            ),
            displayName="Provider cancellation credential",
            quotaDomain=ModelQuotaDomain.objects.create(
                id="provider-cancellation", maxConcurrent=1, enabled=True
            ),
            encryptedSecret="unused-by-patched-client",
            createdBy=admin,
            updatedBy=admin,
        )
        model = ModelConfig.objects.create(
            displayName="Provider cancellation boundary",
            provider=credential.provider,
            modelName="provider-cancellation-boundary",
            resolvedApi="openai-completions",
            resolvedApiBase=credential.provider.apiBase,
        )
        prepared_prompt = {
            "schema": "prepared_prompt.v1",
            "messages": [
                {
                    "messageId": "message_user",
                    "role": "user",
                    "content": "hello",
                }
            ],
            "toolDefinitions": [],
            "toolChoice": {"type": "none"},
            "maxOutputTokens": model.maxOutputTokens,
        }

        async def provider_events():
            yield {
                "choices": [
                    {
                        "delta": {"content": "provider delta"},
                        "finish_reason": None,
                    }
                ]
            }
            yield {
                "choices": [
                    {
                        "delta": {},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2,
                },
            }

        client = SimpleNamespace(
            chat=SimpleNamespace(
                completions=SimpleNamespace(
                    create=AsyncMock(return_value=provider_events()),
                ),
            ),
            close=AsyncMock(),
        )

        async def cancel_provider_send():
            response = OwnedAsyncStreamingHttpResponse(
                stream_model_async(
                    "agent_run_provider_cancelled",
                    model.id,
                    {"preparedPrompt": prepared_prompt},
                ),
                content_type="text/event-stream",
            )

            async def send(message):
                if (
                    message["type"] == "http.response.body"
                    and b"event: delta" in message.get("body", b"")
                ):
                    raise asyncio.CancelledError

            try:
                await ASGIHandler().send_response(response, send)
            except asyncio.CancelledError:
                pass

        with patch(
            "app_core.model_adapter.openai_completions.async_open_ai_completions_client",
            new=AsyncMock(return_value=client),
        ):
            async_to_sync(cancel_provider_send)()

        self.assertEqual(client.close.await_count, 1)
        async def next_attempt():
            async with async_model_attempt(model):
                return True

        async def verify_released():
            return await asyncio.wait_for(next_attempt(), timeout=2)

        self.assertTrue(async_to_sync(verify_released)())
        audit = ModelRunLog.objects.get(agentRunId="agent_run_provider_cancelled")
        self.assertEqual(audit.status, "error")
        self.assertEqual(audit.error, "provider_stream_cancelled")
