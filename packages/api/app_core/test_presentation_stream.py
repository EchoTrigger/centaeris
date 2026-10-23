"""A reconnectable presentation feed carries committed content and authority together."""
import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from asgiref.sync import async_to_sync
from django.test import SimpleTestCase, TransactionTestCase

from .presentation_stream import PresentationScope, encode_cursor, stream_presentation
from .test_hosted_operations import OperationFixture


def data(frame):
    return json.loads(next(line[6:] for line in frame.splitlines() if line.startswith("data: ")))


class PresentationStreamTests(SimpleTestCase):
    def setUp(self):
        self.scope = PresentationScope("workspace", "agent", "session")
        self.state = {"sourceHighWater": "2", "agentRun": {
            "agentRunId": "run", "status": "completed", "startedAt": None, "completedAt": None,
        }}
        self.page = {"schema": "transcript.page.v1", "sessionId": "session",
            "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation",
            "sourceHighWater": "2", "blocks": [], "hasOlder": False, "olderCursor": None,
            "resumeCursors": [{"streamId": "workspace-transcript.v1", "cursor": "2"}]}
        self.loader = type("Loader", (), {})()
        self.loader.authorized = AsyncMock(return_value=True)
        self.loader.navigation = AsyncMock(return_value=[])
        self.loader.state = AsyncMock(return_value=self.state)
        self.loader.page = AsyncMock(return_value=self.page)
        self.loader.patches = AsyncMock(return_value={"schema": "transcript.patch.page.v1",
            "sessionId": "session", "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation", "throughSourceHighWater": "2",
            "patches": [], "nextSourceHighWater": "2", "hasMore": False})
        self.loader.content = AsyncMock()
        self.loader.operations = AsyncMock(return_value=[])
        self.loader.live = AsyncMock(return_value=None)
        self.loader.revisions = AsyncMock(return_value=dict(navigation=0, state=0, context=0, history=0, work=0, live=0))
        async def wait_for_change(_previous):
            await asyncio.Event().wait()
        self.loader.wait_for_change = AsyncMock(side_effect=wait_for_change)

    async def until(self, source, predicate):
        frames = []
        for _ in range(20):
            frame = await asyncio.wait_for(anext(source), .5)
            frames.append(frame)
            if frame.startswith("data:") or "\ndata:" in frame:
                if predicate(data(frame), frame):
                    return frames
        self.fail("expected presentation frame was not emitted")

    async def test_idle_completed_stream_does_not_reload_unchanged_business_data(self):
        source = stream_presentation(self.scope, self.loader, heartbeat_seconds=.01)
        async def consume():
            async for _frame in source:
                pass
        task = asyncio.create_task(consume())
        try:
            await asyncio.sleep(2.1)
        finally:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            await source.aclose()
        self.assertEqual(self.loader.navigation.await_count, 1)
        self.assertEqual(self.loader.state.await_count, 1)

    async def test_view_revision_changes_wake_without_reloading_transcript_or_navigation(self):
        changed = asyncio.Event()
        versions = self.loader.revisions.return_value
        async def wait(_previous):
            await changed.wait()
            changed.clear()
        self.loader.wait_for_change.side_effect = wait
        source = stream_presentation(self.scope, self.loader)
        try:
            initial = await self.until(source, lambda item, _frame: item["kind"] == "viewRevision")
            self.assertEqual(data(initial[-1])["contextRevision"], "0")
            versions["context"] = 1
            changed.set()
            update = await self.until(source, lambda item, _frame: item["kind"] == "viewRevision")
            self.assertEqual(data(update[-1])["contextRevision"], "1")
            self.assertEqual(self.loader.state.await_count, 1)
            self.assertEqual(self.loader.navigation.await_count, 1)
            self.assertEqual(self.loader.page.await_count, 1)
        finally:
            await source.aclose()

    async def test_initial_tail_and_terminal_checkpoint_share_one_stream(self):
        source = stream_presentation(self.scope, self.loader)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        items = [data(frame) for frame in frames]
        self.assertEqual([item["kind"] for item in items], ["navigation", "checkpoint", "snapshot", "checkpoint"])
        self.assertEqual(items[1]["agentRun"]["status"], "completed")
        self.assertNotIn("id:", frames[1])
        self.assertEqual(items[2]["page"], self.page)

    async def test_valid_resume_replays_patch_without_a_new_snapshot_and_restates_terminal(self):
        cursor = encode_cursor(self.scope, "generation", "1")
        source = stream_presentation(self.scope, self.loader, cursor)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        kinds = [data(frame)["kind"] for frame in frames]
        self.assertIn("patches", kinds)
        self.assertNotIn("snapshot", kinds)
        self.loader.page.assert_not_awaited()
        self.assertEqual(data(frames[-1])["agentRun"]["status"], "completed")

    async def test_scope_mismatch_resets_instead_of_applying_another_sessions_cursor(self):
        wrong = encode_cursor(PresentationScope("workspace", "agent", "different"), "generation", "1")
        source = stream_presentation(self.scope, self.loader, wrong)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        resets = [data(frame) for frame in frames if data(frame)["kind"] == "reset"]
        self.assertEqual(resets[0]["reason"], "cursorInvalid")
        self.assertTrue(any(data(frame)["kind"] == "snapshot" for frame in frames))

    async def test_reference_content_is_delivered_before_cursor_ack_and_tool_detail_is_not_loaded(self):
        ref = {"refId": "session-event:event", "revision": "1", "byteLength": "6"}
        self.page["blocks"] = [{"body": {"kind": "assistantText", "content": {"inlineContent": None, "sourceRef": ref}}},
            {"body": {"kind": "tool", "callId": "call", "summary": "read", "summaryRef": None,
                       "outputRef": {**ref, "refId": "tool-output:call"}}}]
        self.loader.content.side_effect = [
            {"schema": "transcript.content.range.v1", "sessionId": "session", "projectionVersion": "transcript.projection.v1",
             "projectionGeneration": "generation", **ref, "startOffset": "0", "endOffset": "3", "content": "one", "hasMore": True},
            {"schema": "transcript.content.range.v1", "sessionId": "session", "projectionVersion": "transcript.projection.v1",
             "projectionGeneration": "generation", **ref, "startOffset": "3", "endOffset": "6", "content": "two", "hasMore": False}]
        source = stream_presentation(self.scope, self.loader)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        content = [data(frame)["range"]["content"] for frame in frames if data(frame)["kind"] == "content"]
        self.assertEqual(content, ["one", "two"])
        self.assertEqual(self.loader.content.await_count, 2)

    async def test_hung_projection_does_not_hide_terminal_and_emits_heartbeat_then_closes(self):
        stopped = asyncio.Event()
        async def blocked(*args):
            try:
                await asyncio.Event().wait()
            finally:
                stopped.set()
        self.loader.page.side_effect = blocked
        source = stream_presentation(self.scope, self.loader, heartbeat_seconds=.005, read_timeout_seconds=.025)
        frames = []
        try:
            with self.assertRaises(TimeoutError):
                async for frame in source:
                    frames.append(frame)
        finally:
            await source.aclose()
        self.assertTrue(any(frame.startswith(":") for frame in frames))
        self.assertEqual(data(frames[1])["agentRun"]["status"], "completed")
        self.assertTrue(stopped.is_set())

    async def test_revoke_closes_a_blocked_producer_without_publishing_more_data(self):
        self.loader.authorized.side_effect = [True, True, False]
        self.loader.page.side_effect = lambda *args: asyncio.Event().wait()
        source = stream_presentation(self.scope, self.loader, heartbeat_seconds=.005,
                                     authority_seconds=.005, read_timeout_seconds=.1)
        frames = [frame async for frame in source]
        self.assertFalse(any('"kind":"snapshot"' in frame for frame in frames))

    async def test_idle_revocation_closes_without_a_display_change_or_notification(self):
        permitted = True
        self.loader.authorized.side_effect = lambda: permitted
        source = stream_presentation(self.scope, self.loader, heartbeat_seconds=.002,
                                     authority_seconds=.01, read_timeout_seconds=.1)
        try:
            await self.until(source, lambda item, _frame: item["kind"] == "viewRevision")
            checks = self.loader.authorized.await_count
            permitted = False
            async with asyncio.timeout(.2):
                remaining = [frame async for frame in source]
            self.assertTrue(all(frame.startswith(":") for frame in remaining))
            self.assertGreater(self.loader.authorized.await_count, checks)
            self.loader.wait_for_change.assert_awaited_once()
        finally:
            await source.aclose()

    async def test_revocation_during_page_preparation_prevents_prepared_payload_emission(self):
        permitted = True
        preparing, release = asyncio.Event(), asyncio.Event()
        async def page(*_args):
            preparing.set()
            await release.wait()
            return self.page
        self.loader.page.side_effect = page
        self.loader.authorized.side_effect = lambda: permitted
        source = stream_presentation(self.scope, self.loader, read_timeout_seconds=.5)
        try:
            self.assertEqual(data(await anext(source))["kind"], "navigation")
            self.assertEqual(data(await anext(source))["kind"], "checkpoint")
            prepared = asyncio.create_task(anext(source))
            await asyncio.wait_for(preparing.wait(), .2)
            permitted = False
            release.set()
            with self.assertRaises(StopAsyncIteration):
                await asyncio.wait_for(prepared, .2)
        finally:
            release.set()
            await source.aclose()

    async def test_cancellation_closes_pending_load_without_cancelling_any_run(self):
        started = asyncio.Event()
        cancelled = asyncio.Event()
        async def blocked(*args):
            started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()
        self.loader.page.side_effect = blocked
        source = stream_presentation(self.scope, self.loader, heartbeat_seconds=.005)
        await anext(source)
        await anext(source)
        await anext(source)
        await started.wait()
        await source.aclose()
        self.assertTrue(cancelled.is_set())

    async def test_projection_rebuild_resets_old_generation_and_replaces_tail(self):
        from .runtime_client import TranscriptRuntimeError
        self.loader.patches.side_effect = TranscriptRuntimeError({"error": "transcript_view_invalidated"})
        cursor = encode_cursor(self.scope, "retired-generation", "1")
        source = stream_presentation(self.scope, self.loader, cursor)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        self.assertEqual(next(data(frame)["reason"] for frame in frames if data(frame)["kind"] == "reset"), "projectionInvalidated")
        self.assertEqual(data(frames[-1])["projectionGeneration"], "generation")

    async def test_reconnect_at_current_water_still_reasserts_authoritative_terminal(self):
        cursor = encode_cursor(self.scope, "generation", "2")
        source = stream_presentation(self.scope, self.loader, cursor)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        self.assertEqual(data(frames[-1])["agentRun"]["status"], "completed")
        self.loader.patches.assert_awaited_once_with("generation", "2", "2")

    async def test_missing_content_never_acknowledges_received_projection(self):
        self.page["blocks"] = [{"body": {"kind": "assistantText", "content": {"inlineContent": None,
            "sourceRef": {"refId": "session-event:event", "revision": "1", "byteLength": "6"}}}}]
        self.loader.content.side_effect = RuntimeError("synthetic_content_failure")
        source = stream_presentation(self.scope, self.loader)
        frames = []
        with self.assertRaisesRegex(RuntimeError, "synthetic_content_failure"):
            async for frame in source:
                frames.append(frame)
        self.assertFalse(any(frame.startswith("id:") for frame in frames))
        self.assertEqual(data(frames[1])["agentRun"]["status"], "completed")

    async def test_slow_consumer_leaves_only_one_pending_frame_and_does_not_load_ahead(self):
        source = stream_presentation(self.scope, self.loader, heartbeat_seconds=.005)
        await anext(source)
        await asyncio.sleep(.01)
        self.loader.state.assert_not_awaited()
        self.loader.page.assert_not_awaited()
        await source.aclose()

    async def test_display_budget_stops_at_a_whole_utf8_prefix_and_acknowledges_without_retrying(self):
        ref = {"refId": "session-event:event", "revision": "1", "byteLength": "9"}
        self.page["blocks"] = [{"body": {"kind": "assistantText", "content": {"inlineContent": None, "sourceRef": ref}}}]
        self.loader.content.return_value = {"schema": "transcript.content.range.v1", "sessionId": "session",
            "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation", **ref,
            "startOffset": "0", "endOffset": "9", "content": "中中中", "hasMore": False}
        with patch("app_core.presentation_stream.CONTENT_DISPLAY_BYTES", 5):
            source = stream_presentation(self.scope, self.loader)
            try:
                frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
            finally:
                await source.aclose()
        ranges = [data(frame)["range"] for frame in frames if data(frame)["kind"] == "content"]
        self.assertEqual([(item["content"], item["endOffset"], item["hasMore"]) for item in ranges], [("中", "3", True)])
        self.loader.content.assert_awaited_once()

    async def test_navigation_chunks_are_contiguous_and_final_frame_commits_one_snapshot(self):
        self.loader.navigation.return_value = [{"id": str(index), "title": "x" * 30} for index in range(5)]
        with patch("app_core.presentation_stream.NAVIGATION_CHUNK_BYTES", 120):
            source = stream_presentation(self.scope, self.loader)
            try:
                frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
            finally:
                await source.aclose()
        chunks = [data(frame) for frame in frames if data(frame)["kind"] == "navigation"]
        self.assertGreater(len(chunks), 1)
        self.assertEqual(len({chunk["snapshotId"] for chunk in chunks}), 1)
        self.assertTrue(chunks[0]["hasMore"])
        self.assertFalse(chunks[-1]["hasMore"])
        self.assertEqual([row["id"] for chunk in chunks for row in chunk["sessions"]], [str(index) for index in range(5)])
        self.assertEqual([chunk["offset"] for chunk in chunks], [0, 2, 4])

    async def test_tool_operations_are_recovered_for_snapshot_and_precede_checkpoint(self):
        self.page["blocks"] = [{"body": {"kind": "tool", "callId": "call", "summaryRef": None}}]
        detail = {"kind": "call", "callId": "call", "toolName": "bash", "normalizedInput": {"command": "pwd"},
                  "resultState": None, "operations": [], "previewTruncated": False, "sourceEventId": "event-call"}
        self.loader.operations.return_value = [detail]
        source = stream_presentation(self.scope, self.loader)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        self.assertEqual(data(frames[-2]), {"schema": "workspace.presentation.v1", **self.scope.wire(), "kind": "operation", "detail": detail})
        self.loader.operations.assert_awaited_once_with("call", "2")

    async def test_shared_wire_fixture_matches_actual_initial_stream_emission(self):
        fixture = json.loads((Path(__file__).resolve().parents[3] / "tests/fixtures/workspace-presentation.json").read_text(encoding="utf-8"))
        self.loader.navigation.return_value = fixture["initialFrames"][0]["sessions"]
        self.loader.page.return_value = fixture["initialFrames"][2]["page"]
        source = stream_presentation(self.scope, self.loader)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        self.assertEqual([data(frame) for frame in frames], fixture["initialFrames"])
        self.assertEqual(frames[-1].splitlines()[0], "id: " + fixture["initialCursor"])

    async def test_projection_catchup_retries_the_frozen_generation_and_water(self):
        from .runtime_client import TranscriptRuntimeError
        requests = []
        async def page(water, generation=None):
            requests.append((water, generation))
            if len(requests) == 1:
                raise TranscriptRuntimeError({"error": "transcript_projection_not_ready", "projectionGeneration": "generation",
                    "sourceHighWater": "2", "projectedHighWater": "1"})
            return self.page
        self.loader.page.side_effect = page
        source = stream_presentation(self.scope, self.loader)
        try:
            await self.until(source, lambda item, frame: item["kind"] == "checkpoint" and frame.startswith("id:"))
        finally:
            await source.aclose()
        self.assertEqual(requests, [("2", None), ("2", "generation")])

    async def test_large_live_snapshot_below_transport_budget_is_delivered(self):
        self.state["agentRun"]["status"] = "running"
        self.loader.live.return_value = {"afterSequence": 2, "revision": 1, "turnId": "turn",
            "messageId": "live-message", "text": "x" * (1024 * 1024 + 1)}
        source = stream_presentation(self.scope, self.loader)
        try:
            frames = await self.until(source, lambda item, frame: item["kind"] == "live")
        finally:
            await source.aclose()
        self.assertEqual(data(frames[-1])["live"]["text"], self.loader.live.return_value["text"])

    async def test_live_snapshot_above_transport_budget_fails_explicitly_without_truncation(self):
        self.state["agentRun"]["status"] = "running"
        self.loader.live.return_value = {"afterSequence": 2, "revision": 1, "turnId": "turn",
            "messageId": "live-message", "text": "x" * (4 * 1024 * 1024)}
        source = stream_presentation(self.scope, self.loader)
        frames = []
        with self.assertRaisesRegex(RuntimeError, "presentation_frame_budget_exceeded"):
            async for frame in source:
                frames.append(frame)
        self.assertFalse(any(data(frame)["kind"] == "live" for frame in frames))


class PresentationRouteContractTests(SimpleTestCase):
    """Exercise the actual ASGI HTTP route while isolating its storage boundary."""
    def test_openapi_declares_presentation_as_event_stream(self):
        from api.ninja_api import api
        operation = api.get_openapi_schema()["paths"]["/api/workspaces/{workspace_id}/presentation/events"]["get"]
        responses = {str(code): value for code, value in operation["responses"].items()}
        self.assertEqual(responses["200"].get("content"), {
            "text/event-stream": {"schema": {"type": "string"}},
        })
        for status in ("400", "401", "404", "503"):
            self.assertIn("application/json", responses[status]["content"])

    def test_openapi_declares_operation_download_json_fields(self):
        from api.ninja_api import api
        schema = api.get_openapi_schema()
        operation = schema["paths"]["/api/sessions/{session_id}/presentation/operation"]["get"]
        responses = {str(code): value for code, value in operation["responses"].items()}
        content = responses["200"].get("content", {})
        self.assertEqual(set(content), {"application/json"})
        reference = content["application/json"]["schema"]["$ref"]
        body = schema["components"]["schemas"][reference.rsplit("/", 1)[-1]]
        fields = {"kind", "callId", "toolName", "normalizedInput", "resultState",
            "operations", "previewTruncated", "sourceEventId"}
        self.assertEqual(set(body["properties"]), fields)
        self.assertEqual(set(body["required"]), fields)
        self.assertEqual(body["properties"]["kind"]["enum"], ["call", "result"])
        self.assertIs(body["additionalProperties"], False)
        for status in ("400", "401", "404", "503"):
            self.assertIn("application/json", responses[status]["content"])

    async def test_authenticated_http_stream_delivers_snapshot_and_acknowledges_only_after_apply_payloads(self):
        from .http.presentation import PresentationLoader
        from .http.security import ProductSessionAuth
        fixture = PresentationStreamTests()
        fixture.setUp()
        def authenticate(_auth, request, _key):
            request.user = SimpleNamespace(id=1, is_authenticated=True)
            return request.user
        from .presentation_observer import PresentationObserverHub
        from .presentation_notifications import PresentationHint
        async def hints(ready):
            ready.set()
            await asyncio.Event().wait()
            yield PresentationHint(None, True)
        hub = PresentationObserverHub(AsyncMock(return_value=dict(navigation=0, state=0, context=0, history=0, work=0)), hints, lambda _: ())
        with patch.object(ProductSessionAuth, "authenticate", authenticate), \
                patch("app_core.http.presentation.create_presentation_hub", return_value=hub), \
                patch.object(PresentationLoader, "authorized", AsyncMock(return_value=True)), \
                patch.object(PresentationLoader, "navigation", fixture.loader.navigation), \
                patch.object(PresentationLoader, "state", fixture.loader.state), \
                patch.object(PresentationLoader, "page", fixture.loader.page):
            response = await self.async_client.get("/api/workspaces/workspace/presentation/events", {"agentId": "agent_x", "sessionId": "session"})
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response["X-Accel-Buffering"], "no")
            self.assertEqual(response["Content-Type"], "text/event-stream")
            try:
                frames = []
                async for frame in response:
                    frames.append(frame.decode())
                    if frame.startswith(b"id:"):
                        break
            finally:
                await response.aclose()
        self.assertEqual([data(frame)["kind"] for frame in frames], ["navigation", "checkpoint", "snapshot", "checkpoint"])

    async def test_anonymous_http_stream_requires_authentication(self):
        response = await self.async_client.get("/api/workspaces/workspace/presentation/events", {"agentId": "agent_x"})
        self.assertEqual(response.status_code, 401)

    async def test_operation_download_http_is_attachment_and_preserves_full_detail(self):
        from .http.security import ProductSessionAuth
        from .http.response_schema import PresentationOperationResponse
        detail = {"kind": "result", "callId": "call", "toolName": "edit", "normalizedInput": {"path": "a.py", "options": [True, None, 1]},
            "resultState": "successWithOutput", "operations": [{"diffPreview": "full diff"}],
            "previewTruncated": False, "sourceEventId": "event"}
        loader = SimpleNamespace(authorized=AsyncMock(return_value=True))
        def authenticate(_auth, request, _key):
            request.user = SimpleNamespace(id=1, is_authenticated=True)
            return request.user
        with patch.object(ProductSessionAuth, "authenticate", authenticate), \
                patch("app_core.http.presentation._operation_download", AsyncMock(return_value=(loader, detail))):
            response = await self.async_client.get("/api/sessions/session/presentation/operation", {"sourceEventId": "event"})
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response["Content-Type"], "application/json")
            self.assertEqual(response["Content-Disposition"], 'attachment; filename="tool-operation.json"')
            content = b"".join([chunk async for chunk in response])
            self.assertEqual(json.loads(content), detail)
            self.assertEqual(PresentationOperationResponse.model_validate(json.loads(content)).model_dump(by_alias=True), detail)


class OperationPresentationTests(SimpleTestCase):
    def test_normal_command_and_successful_diff_are_not_changed_by_the_host_dto(self):
        from .operation_presentation import operation_detail
        payload = {"type": "tool_result", "callId": "call", "toolName": "edit", "normalizedInput": {"path": "a.py"},
            "resultState": "successWithOutput", "operations": [{"callId": "call", "toolName": "edit",
                "status": "completed", "resultState": "successWithOutput", "path": "a.py", "diffPreview": "+fixed"}],
            "modelContent": "large output is never part of the DTO", "fullOutputPath": "/private/path"}
        detail = operation_detail({"eventId": "event", "display": payload})
        self.assertEqual(detail["normalizedInput"], {"path": "a.py"})
        self.assertEqual(detail["operations"], payload["operations"])
        self.assertFalse(detail["previewTruncated"])
        self.assertNotIn("modelContent", detail)
        self.assertNotIn("fullOutputPath", detail)

    def test_large_preview_is_explicit_and_the_download_dto_retains_original_data(self):
        from .operation_presentation import operation_detail
        record = {"eventId": "large", "display": {"type": "tool_call", "callId": "call", "toolName": "bash",
            "normalizedInput": {"command": "x" * 100_000}, "resultState": None, "operations": None}}
        preview = operation_detail(record)
        full = operation_detail(record, preview=False)
        self.assertTrue(preview["previewTruncated"])
        self.assertLessEqual(len(preview["normalizedInput"]["command"].encode()), 65536)
        self.assertEqual(full["normalizedInput"]["command"], "x" * 100_000)
        self.assertFalse(full["previewTruncated"])

    def test_structural_preview_budget_preserves_valid_top_level_types(self):
        from .operation_presentation import operation_detail
        record = {"eventId": "large", "display": {"type": "tool_call", "callId": "call", "toolName": "bash",
            "normalizedInput": {str(index): {str(child): "x" for child in range(128)} for index in range(128)},
            "resultState": None, "operations": None}}
        preview = operation_detail(record)
        self.assertTrue(preview["previewTruncated"])
        self.assertIsInstance(preview["operations"], list)


class PresentationHttpTests(OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        self.create_url = f"/api/workspaces/{self.workspace.id}/sessions"
        with patch("app_core.http.workspaces.request_execution_profile", return_value={"schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "a" * 64}):
            response = self.client.post(self.create_url, json.dumps({"operationId": "presentation-session", "agentId": self.agent.id}), content_type="application/json")
        self.session_id = response.json()["sessionId"]
        self.url = f"/api/workspaces/{self.workspace.id}/presentation/events"

    def consume(self, response):
        async def collect():
            result = []
            try:
                async for frame in response:
                    frame = frame.decode()
                    if frame.startswith(":"):
                        continue
                    result.append(data(frame))
                    if frame.startswith("id:"):
                        return result
                return result
            finally:
                await response.aclose()
        return async_to_sync(collect)()

    def test_real_route_authorizes_and_streams_runtime_projection_inline(self):
        page = {"schema": "transcript.page.v1", "sessionId": self.session_id,
            "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation",
            "sourceHighWater": "0", "blocks": [{"blockId": "body", "blockRevision": "1",
                "orderKey": {"sourceSequence": "0", "ordinal": 0}, "body": {"kind": "assistantText",
                    "content": {"inlineContent": "Ready", "sourceRef": None}, "status": "completed"}}],
            "hasOlder": False, "olderCursor": None, "resumeCursors": [{"streamId": "workspace-transcript.v1", "cursor": "0"}]}
        with patch("app_core.http.presentation.request_transcript_page", return_value=page):
            response = self.client.get(self.url, {"agentId": self.agent.id, "sessionId": self.session_id})
            self.assertEqual(response.status_code, 200)
            self.assertEqual(response["X-Accel-Buffering"], "no")
            items = self.consume(response)
        self.assertEqual(next(item for item in items if item["kind"] == "snapshot")["page"], page)
        navigation = next(item for item in items if item["kind"] == "navigation")
        self.assertEqual(navigation["sessions"][0]["id"], self.session_id)

    def test_anonymous_unknown_query_and_cross_agent_session_are_rejected(self):
        from .models import Agent
        from django.test import Client
        self.assertEqual(Client().get(self.url, {"agentId": self.agent.id}).status_code, 401)
        self.assertEqual(self.client.get(self.url, {"agentId": self.agent.id, "unknown": "1"}).status_code, 400)
        self.assertEqual(self.client.get(self.url, {"agentId": [self.agent.id, self.agent.id]}).status_code, 400)
        other = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Other")
        self.assertEqual(self.client.get(self.url, {"agentId": other.id, "sessionId": self.session_id}).status_code, 404)

    def test_membership_revocation_after_headers_closes_before_first_payload(self):
        from .models import WorkspaceMembership
        response = self.client.get(self.url, {"agentId": self.agent.id, "sessionId": self.session_id})
        self.assertEqual(response.status_code, 200)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).delete()
        self.assertEqual(self.consume(response), [])
        retry = self.client.get(self.url, {"agentId": self.agent.id, "sessionId": self.session_id})
        self.assertEqual(retry.status_code, 404)

    def test_navigation_only_connection_has_no_transcript_read(self):
        with patch("app_core.http.presentation.request_transcript_page") as page:
            response = self.client.get(self.url, {"agentId": self.agent.id})
            self.assertEqual(response.status_code, 200)
            async def read_navigation():
                try:
                    return data((await anext(response.__aiter__())).decode())
                finally:
                    await response.aclose()
            item = async_to_sync(read_navigation)()
        self.assertEqual(item["kind"], "navigation")
        self.assertIsNone(item["sessionId"])
        self.assertEqual(item["sessions"][0]["id"], self.session_id)
        page.assert_not_called()

    def test_navigation_authority_cost_is_independent_of_session_rows(self):
        from django.db import connection
        from django.test import RequestFactory
        from django.test.utils import CaptureQueriesContext
        from .http.presentation import PresentationLoader
        from .models import Session
        request = RequestFactory().get("/")
        request.user = self.user
        loader = PresentationLoader(request, PresentationScope(self.workspace.pk, self.agent.pk))
        counts = []
        for count in (1, 40):
            while Session.objects.filter(agent=self.agent).count() < count:
                Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
            with CaptureQueriesContext(connection) as queries, patch.object(Session, "from_db", wraps=Session.from_db) as hydrate:
                self.assertTrue(async_to_sync(loader.authorized)())
            # Checking an Agent scope must not hydrate every conversation merely
            # because its complete navigation list can be displayed later.
            self.assertEqual(hydrate.call_count, 0)
            counts.append(len(queries))
        self.assertEqual(counts[0], counts[1])
        rows = async_to_sync(loader.navigation)()
        self.assertEqual(len(rows), 40, "only the actual navigation read materializes the complete scoped list")

    def test_navigation_authority_leaves_notification_projection_to_navigation(self):
        from django.test import RequestFactory
        from .http.presentation import PresentationLoader
        from .models import AgentRun, Session
        from .tests import append_started, append_session_records, session_record
        session = Session.objects.get(pk=self.session_id)
        run = AgentRun.objects.create(workspace=self.workspace, session=session, user=self.user,
            modelConfig=self.model, prompt="Synthetic attention")
        append_started(run)
        append_session_records(run, [session_record(run, 3, "agent_run_completed", {"doneReason": "finalized"})])
        request = RequestFactory().get("/")
        request.user = self.user
        loader = PresentationLoader(request, PresentationScope(self.workspace.pk, self.agent.pk))
        self.assertTrue(async_to_sync(loader.authorized)())
        session.refresh_from_db()
        self.assertEqual((session.latestAttentionSequence, session.attentionRevision), (0, 0))
        rows = async_to_sync(loader.navigation)()
        session.refresh_from_db()
        self.assertEqual((session.latestAttentionSequence, session.attentionRevision), (3, 1))
        self.assertTrue(next(row for row in rows if row["id"] == session.pk)["isUnread"])

    def test_closing_browser_delivery_does_not_cancel_the_accepted_run(self):
        from .models import AgentRun, SessionEvent
        accepted = self.post(self.create_url + f"/{self.session_id}/messages").json()
        before = SessionEvent.objects.count()
        page = {"schema": "transcript.page.v1", "sessionId": self.session_id,
            "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation",
            "sourceHighWater": "0", "blocks": [], "hasOlder": False, "olderCursor": None,
            "resumeCursors": [{"streamId": "workspace-transcript.v1", "cursor": "0"}]}
        with patch("app_core.http.presentation.request_transcript_page", return_value=page):
            response = self.client.get(self.url, {"agentId": self.agent.id, "sessionId": self.session_id})
            self.assertEqual(response.status_code, 200)
            self.consume(response)
        run = AgentRun.objects.get(pk=accepted["agentRunId"])
        self.assertEqual(run.status, "queued")
        self.assertIsNone(run.preAdmissionCancelledAt)
        self.assertEqual(SessionEvent.objects.count(), before)

    def test_operation_download_rejects_another_session_and_preserves_canonical_nul(self):
        from .models import AgentRun, SessionEvent
        accepted = self.post(self.create_url + f"/{self.session_id}/messages").json()
        run = AgentRun.objects.get(pk=accepted["agentRunId"])
        event = SessionEvent.objects.create(eventId="operation-nul", workspace=self.workspace, session_id=self.session_id,
            agent_run=run, sequence=1, agent_run_sequence=1, projects_to_agent_run_stream=True, createdAtMs=1,
            payload={"schemaVersion": "session.event.v1", "eventVersion": 1, "eventId": "operation-nul",
                "sequence": 1, "sessionId": self.session_id, "agentRunId": run.pk, "turnId": run.turn_id,
                "createdAtMs": 1, "type": "tool_call", "payload": {"callId": "call", "toolName": "bash",
                    "normalizedInput": {"command": "printf\0exact"}}})
        path = f"/api/sessions/{self.session_id}/presentation/operation"
        response = self.client.get(path, {"sourceEventId": event.pk})
        self.assertEqual(response.status_code, 200)
        async def read():
            return b"".join([chunk async for chunk in response])
        self.assertEqual(json.loads(async_to_sync(read)())["normalizedInput"]["command"], "printf\0exact")
        other = self.create("another-session").json()["sessionId"]
        self.assertEqual(self.client.get(f"/api/sessions/{other}/presentation/operation", {"sourceEventId": event.pk}).status_code, 404)

    def test_checkpoint_does_not_attach_a_newer_terminal_status_to_an_older_source_water(self):
        from concurrent.futures import ThreadPoolExecutor
        from django.db import close_old_connections
        from django.db.models.query import QuerySet
        from django.utils import timezone
        from django.test import RequestFactory
        from .models import AgentRun, SessionEvent
        from .http.presentation import PresentationLoader
        accepted = self.post(self.create_url + f"/{self.session_id}/messages").json()
        run = AgentRun.objects.get(pk=accepted["agentRunId"])
        request = RequestFactory().get("/")
        request.user = self.user
        loader = PresentationLoader(request, PresentationScope(self.workspace.pk, self.agent.pk, self.session_id))
        original_first = QuerySet.first
        raced = False

        def commit_terminal():
            close_old_connections()
            try:
                from django.db import transaction
                with transaction.atomic():
                    AgentRun.objects.filter(pk=run.pk).update(status="completed", completedAt=timezone.now())
                    SessionEvent.objects.create(eventId="racing-terminal", workspace=self.workspace, session_id=self.session_id,
                        agent_run=run, sequence=1, agent_run_sequence=1, projects_to_agent_run_stream=True,
                        createdAtMs=1, payload={"type": "agent_run_completed", "payload": {}})
            finally:
                close_old_connections()

        def first(query):
            nonlocal raced
            result = original_first(query)
            if query.model is SessionEvent and not raced:
                raced = True
                with ThreadPoolExecutor(max_workers=1) as pool:
                    pool.submit(commit_terminal).result(timeout=5)
            return result

        with patch.object(QuerySet, "first", first):
            state = async_to_sync(loader.state)()
        self.assertTrue(raced)
        self.assertEqual(state["sourceHighWater"], "0")
        self.assertEqual(state["agentRun"]["status"], "queued")
        self.assertEqual(AgentRun.objects.get(pk=run.pk).status, "completed")
