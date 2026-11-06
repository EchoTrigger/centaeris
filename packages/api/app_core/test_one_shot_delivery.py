import asyncio
import base64
from datetime import timedelta
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.test import SimpleTestCase, override_settings
from django.utils import timezone

from . import one_shot_stream as stream
from .artifact_publish import ArtifactPublishError, _require_one_shot_artifact_budget


class OneShotStreamTests(SimpleTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="one-shot-delivery-")
        self.addCleanup(directory.cleanup)
        settings = override_settings(MEDIA_ROOT=directory.name)
        settings.enable()
        self.addCleanup(settings.disable)
        self.now = timezone.now()
        self.operation = SimpleNamespace(
            pk=1, operationId="operation", agentRunId="run", acceptedAt=self.now,
            deadlineAt=self.now + timedelta(minutes=10), expiresAt=self.now + timedelta(hours=24),
            runStatus="completed", cleanupStatus="pending", reasonCode="",
            deliveryLeaseUntil=self.now + timedelta(seconds=60),
        )
        self.run = SimpleNamespace(id="run", session_id="private-session")
        self.check = AsyncMock(return_value=True)
        for name, replacement in (
            ("_run_state", AsyncMock(return_value=self.operation)),
            ("_close_delivery", AsyncMock()),
            ("_artifacts", AsyncMock(return_value=[])),
            ("_load_postgres_page", AsyncMock(return_value=([], 0))),
            ("_load_terminal_sequence", AsyncMock(return_value=0)),
        ):
            mock = patch.object(stream, name, replacement)
            mock.start()
            self.addCleanup(mock.stop)

    def record(self, sequence, event_type, payload):
        event_id = f"event-{sequence}"
        return {"sequence": sequence, "eventId": event_id, "payload": {
            "sequence": sequence, "eventId": event_id, "sessionId": "private-session",
            "agentRunId": "run", "type": event_type, "payload": payload,
        }}

    def frames(self, raw):
        result = []
        for frame in raw:
            if frame.startswith(":"):
                continue
            event, data = frame.strip().split("\n", 1)
            result.append((event.removeprefix("event: "), json.loads(data.removeprefix("data: "))))
        return result

    async def test_original_response_delivers_verified_bytes_and_only_final_committed_text(self):
        content = b"PK\x03\x04word-document"
        key = default_storage.save("private/report.docx", ContentFile(content))
        artifact = SimpleNamespace(
            id="artifact", safeFilename="report.docx", contentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            sizeBytes=len(content), sha256="sha256:" + hashlib.sha256(content).hexdigest(), storageKey=key,
        )
        stream._artifacts.return_value = [artifact]
        stream._load_postgres_page.return_value = ([
            self.record(1, "assistant_message", {"modelMarkdown": "intermediate", "reasoning": "private reasoning"}),
            self.record(2, "tool_result", {"result": "private tool result"}),
            self.record(3, "assistant_message", {"modelMarkdown": "文档已生成", "artifactRefs": ["artifact:private"]}),
            self.record(4, "agent_run_completed", {}),
        ], 4)
        raw = [frame async for frame in stream.stream_one_shot(self.operation, self.run, self.check)]
        frames = self.frames(raw)
        self.assertEqual(frames[0][0], "runAccepted")
        self.assertEqual([body["text"] for event, body in frames if event == "outputText"], ["文档已生成"])
        delivered = b"".join(base64.b64decode(body["data"]) for event, body in frames if event == "artifactChunk")
        target = Path(default_storage.location) / "received.docx"
        target.write_bytes(delivered)
        self.assertEqual(target.read_bytes(), content)
        self.assertEqual(frames[-1], ("runFinished", {"runId": "run", "runStatus": "completed", "deliveryStatus": "completed"}))
        self.assertNotIn("private reasoning", "".join(raw))
        self.assertNotIn("private tool result", "".join(raw))
        self.assertNotIn("private-session", "".join(raw))
        stream._close_delivery.assert_awaited_once_with(1)

    async def test_corrupt_artifact_reports_delivery_failure_separately_from_run_success(self):
        key = default_storage.save("private/corrupt.bin", ContentFile(b"wrong"))
        stream._artifacts.return_value = [SimpleNamespace(
            id="artifact", safeFilename="report.bin", contentType="application/octet-stream",
            sizeBytes=5, sha256="sha256:" + "0" * 64, storageKey=key,
        )]
        frames = self.frames([frame async for frame in stream.stream_one_shot(self.operation, self.run, self.check)])
        self.assertEqual([event for event, _ in frames].count("artifactFailed"), 1)
        self.assertNotIn("artifactCompleted", [event for event, _ in frames])
        self.assertEqual(frames[-1][1], {"runId": "run", "runStatus": "completed", "deliveryStatus": "failed"})

    async def test_terminal_projection_cannot_skip_events_committed_after_first_page_read(self):
        stream._load_postgres_page.side_effect = [
            ([], 0),
            ([self.record(1, "assistant_message", {"modelMarkdown": "committed answer"}),
              self.record(2, "agent_run_completed", {})], 2),
        ]
        stream._load_terminal_sequence.return_value = 2
        with patch.object(stream.asyncio, "sleep", AsyncMock()):
            frames = self.frames([frame async for frame in stream.stream_one_shot(self.operation, self.run, self.check)])
        self.assertEqual([body["text"] for event, body in frames if event == "outputText"], ["committed answer"])

    async def test_revocation_stops_before_loading_results_and_closes_delivery(self):
        self.check.return_value = False
        frames = [frame async for frame in stream.stream_one_shot(self.operation, self.run, self.check)]
        self.assertEqual(frames, [])
        stream._load_postgres_page.assert_not_awaited()
        stream._close_delivery.assert_awaited_once_with(1)

    async def test_disconnect_closes_read_lease_without_cancelling_run(self):
        response = stream.stream_one_shot(self.operation, self.run, self.check)
        self.assertTrue((await anext(response)).startswith("event: runAccepted"))
        await response.aclose()
        stream._close_delivery.assert_awaited_once_with(1)
        stream._load_postgres_page.assert_not_awaited()

    async def test_expired_delivery_does_not_read_or_emit_results(self):
        self.operation.deliveryLeaseUntil = self.now - timedelta(seconds=1)
        self.assertEqual([frame async for frame in stream.stream_one_shot(self.operation, self.run, self.check)], [])
        stream._load_postgres_page.assert_not_awaited()


class OneShotArtifactBudgetTests(SimpleTestCase):
    def test_per_file_total_and_count_limits_include_reserved_uploads(self):
        for reserved, size, code in (
            ([], 8 * 1024 * 1024 + 1, "artifact_size_limit_exceeded"),
            ([1] * 4, 0, "artifact_count_limit_exceeded"),
            ([8 * 1024 * 1024] * 2, 1, "artifact_total_size_limit_exceeded"),
        ):
            with self.subTest(code=code), patch("app_core.artifact_publish.Artifact.objects") as artifacts:
                artifacts.filter.return_value.values_list.return_value = reserved
                with self.assertRaisesRegex(ArtifactPublishError, code):
                    _require_one_shot_artifact_budget(SimpleNamespace(), size)

    def test_exact_limits_are_accepted(self):
        with patch("app_core.artifact_publish.Artifact.objects") as artifacts:
            artifacts.filter.return_value.values_list.return_value = [8 * 1024 * 1024]
            _require_one_shot_artifact_budget(SimpleNamespace(), 8 * 1024 * 1024)
