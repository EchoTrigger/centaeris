import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from django.test import SimpleTestCase

from . import agent_run_stream as stream


class TransientStreamTests(SimpleTestCase):
    agent_run = SimpleNamespace(id="run-1", session_id="session-1", status="running")

    def signal(self, revision=2):
        return {**self.fixtures()["live"], "revision": revision}

    @staticmethod
    def fixtures():
        path = Path(__file__).resolve().parents[3] / "tests/fixtures/agent-run-transient-signals.json"
        return json.loads(path.read_text(encoding="utf-8"))

    def test_live_wake_contains_only_position_and_identity(self):
        signal = self.signal()
        self.assertEqual(stream._decode_signal(self.agent_run, {"signal": json.dumps(signal)}), signal)
        for extra in ({"text": "duplicated answer"}, {"reasoning": None}, {"unknown": True}):
            with self.subTest(extra=extra), self.assertRaisesRegex(RuntimeError, "fields mismatch"):
                stream._decode_signal(self.agent_run, {"signal": json.dumps({**signal, **extra})})
        wake = self.fixtures()["commitWake"]
        self.assertEqual(stream._decode_signal(self.agent_run, {"signal": json.dumps(wake)}), wake)

    async def test_live_wakes_reload_latest_complete_snapshot_and_keep_revision_identity(self):
        live = {
            "afterSequence": 4,
            "revision": 3,
            "turnId": "turn-1",
            "messageId": "message:turn-1:assistant",
            "text": "完整 answer",
            "reasoning": {"blockId": "reasoning:request-1", "requestId": "request-1", "text": "完整 thinking"},
        }
        wake_batch = [("signals", [
            ("2-0", {"signal": json.dumps(self.signal(2))}),
            ("3-0", {"signal": json.dumps(self.signal(3))}),
        ])]
        with (
            patch.object(stream, "_capture_signal_tail", AsyncMock(return_value="1-0")),
            patch.object(stream, "_load_postgres_page", AsyncMock(return_value=([], 4))),
            patch.object(stream, "_load_terminal_sequence", AsyncMock(return_value=None)),
            patch.object(stream, "_load_committed_overlay_projection", AsyncMock(return_value=(0, False))),
            patch.object(stream, "_load_live_text_state_async", AsyncMock(side_effect=[None, live])) as snapshot,
            patch.object(stream._redis_stream_client, "xread", AsyncMock(return_value=wake_batch)),
        ):
            source = stream.stream_agent_run_session_items_async(self.agent_run, 4)
            item = await anext(source)
            await source.aclose()
        self.assertEqual(item, stream._encode_sse({
            "schema": stream.STREAM_ITEM_SCHEMA, "kind": "live", "agentRunId": self.agent_run.id, **live,
        }))
        self.assertFalse(item.startswith("id:"))
        self.assertEqual(snapshot.await_count, 2)

    async def test_reconnect_restores_complete_live_snapshot_without_replaying_signals(self):
        live = {
            "afterSequence": 4, "revision": 17, "turnId": "turn-1",
            "messageId": "message:turn-1:assistant", "text": "answer",
            "reasoning": {"blockId": "reasoning:request-1", "requestId": "request-1", "text": "thinking"},
        }
        with (
            patch.object(stream, "_capture_signal_tail", AsyncMock(return_value="99-0")),
            patch.object(stream, "_load_postgres_page", AsyncMock(return_value=([], 4))),
            patch.object(stream, "_load_terminal_sequence", AsyncMock(return_value=None)),
            patch.object(stream, "_load_committed_overlay_projection", AsyncMock(return_value=(0, False))),
            patch.object(stream, "_load_live_text_state_async", AsyncMock(return_value=live)),
            patch.object(stream._redis_stream_client, "xread", AsyncMock()) as xread,
        ):
            source = stream.stream_agent_run_session_items_async(self.agent_run, 4)
            item = await anext(source)
            await source.aclose()
        self.assertEqual(item, stream._encode_sse({
            "schema": stream.STREAM_ITEM_SCHEMA, "kind": "live", "agentRunId": self.agent_run.id, **live,
        }))
        xread.assert_not_awaited()

    async def test_redis_oom_still_reconciles_durable_terminal_with_committed_cursor(self):
        event = {
            "sequence": 5, "eventId": "event-5", "sessionId": self.agent_run.session_id,
            "agentRunId": self.agent_run.id, "type": "agent_run_completed",
            "payload": {"doneReason": "finalized"},
        }
        terminal = {"sequence": 5, "eventId": "event-5", "payload": event}
        with (
            patch.object(stream, "_capture_signal_tail", AsyncMock(return_value="99-0")),
            patch.object(stream, "_load_postgres_page", AsyncMock(side_effect=[([], 4), ([terminal], 5)])),
            patch.object(stream, "_load_terminal_sequence", AsyncMock(return_value=None)),
            patch.object(stream, "_load_live_text_state_async", AsyncMock(side_effect=stream.redis.ResponseError("OOM command not allowed"))),
            patch.object(stream.asyncio, "sleep", AsyncMock()),
        ):
            items = [item async for item in stream.stream_agent_run_session_items_async(self.agent_run, 4)]
        self.assertEqual(items, [stream._encode_sse({
            "schema": stream.STREAM_ITEM_SCHEMA, "kind": "committed", "agentRunId": self.agent_run.id,
            "sourceSequence": 5, "event": event,
        }, stream.encode_stream_cursor(self.agent_run.id, 5))])
