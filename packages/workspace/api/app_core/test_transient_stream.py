import json
import os
from pathlib import Path
from types import SimpleNamespace
from unittest import skipUnless
from unittest.mock import AsyncMock, patch
from uuid import uuid4

from django.test import SimpleTestCase

from . import agent_run_stream as stream


@skipUnless(os.environ.get("TEST_REASONING_REDIS_URL"), "requires isolated TEST_REASONING_REDIS_URL")
class LiveRangeRedisTests(SimpleTestCase):
    def setUp(self):
        self.redis = stream.redis.Redis.from_url(os.environ["TEST_REASONING_REDIS_URL"], decode_responses=True)
        self.run = "live-range-" + uuid4().hex
        self.prefix = f"workspace-agent:agent-run:{{{self.run}}}:live"
        self.keys = [self.prefix + suffix for suffix in (":text", ":meta", ":reasoning")]
        self.meta = {"messageId": "message", "turnId": "turn", "afterSequence": "4", "revision": "1",
                     "textResetRevision": "0", "reasoningResetRevision": "0", "reasoning": "null"}
        self.write("汉", "")
        self.addCleanup(self.redis.close)
        self.addCleanup(lambda: self.redis.delete(*self.keys))

    def write(self, text, reasoning):
        with self.redis.pipeline(transaction=True) as pipeline:
            pipeline.set(self.keys[0], text).hset(self.keys[1], mapping=self.meta).set(self.keys[2], reasoning).execute()

    async def read(self, cursor=None):
        async with stream.async_redis.Redis.from_url(os.environ["TEST_REASONING_REDIS_URL"], decode_responses=True) as client:
            with patch.object(stream, "_redis_stream_client", client):
                return await stream.load_live_text_update(self.run, cursor)

    async def test_append_reads_only_utf8_suffix_and_can_skip_publication_revisions(self):
        first = await self.read()
        self.assertEqual((first.kind, first.value["text"], first.cursor["textOffset"]), ("live", "汉", 3))
        with self.redis.pipeline(transaction=True) as pipeline:
            pipeline.append(self.keys[0], "🚀续").hset(self.keys[1], "revision", "3").execute()
        update = await self.read(first.cursor)
        self.assertEqual(update.kind, "liveDelta")
        self.assertEqual(update.value, {"messageId": "message", "turnId": "turn", "afterSequence": 4,
            "baseRevision": 1, "revision": 3, "textOffset": 3, "text": "🚀续", "reasoning": None})
        self.assertEqual(update.cursor["textOffset"], len("汉🚀续".encode()))
        self.assertIsNone((await self.read(update.cursor)).kind)
        self.assertEqual((await self.read()).value["text"], "汉🚀续")

    async def test_same_length_replacement_and_reasoning_change_require_complete_snapshot(self):
        first = await self.read()
        self.meta.update(revision="2", textResetRevision="2")
        self.write("字", "")
        replacement = await self.read(first.cursor)
        self.assertEqual((replacement.kind, replacement.value["text"]), ("live", "字"))
        self.meta.update(revision="3", reasoningResetRevision="3",
            reasoning=json.dumps({"blockId": "reasoning:one", "requestId": "one"}, separators=(",", ":")))
        self.write("字", "想")
        thinking = await self.read(replacement.cursor)
        self.assertEqual(thinking.kind, "live")
        self.assertEqual(thinking.value["reasoning"]["text"], "想")
        with self.redis.pipeline(transaction=True) as pipeline:
            pipeline.append(self.keys[2], "更多").hset(self.keys[1], "revision", "5").execute()
        appended = await self.read(thinking.cursor)
        self.assertEqual(appended.kind, "liveDelta")
        self.assertEqual(appended.value["reasoning"], {"blockId": "reasoning:one", "requestId": "one", "offset": 3, "text": "更多"})
        self.meta.update(revision="6", reasoningResetRevision="6",
            reasoning=json.dumps({"blockId": "reasoning:two", "requestId": "two"}, separators=(",", ":")))
        self.write("字", "另一段")
        changed = await self.read(appended.cursor)
        self.assertEqual(changed.kind, "live")
        self.assertEqual(changed.value["reasoning"]["requestId"], "two")
        self.meta.update(revision="7", reasoningResetRevision="7", reasoning="null")
        self.write("字", "")
        cleared = await self.read(changed.cursor)
        self.assertEqual(cleared.kind, "live")
        self.assertIsNone(cleared.value["reasoning"])

    async def test_missing_or_replaced_identity_never_reuses_another_base(self):
        first = await self.read()
        self.meta.update(turnId="next", messageId="next-message", afterSequence="5", revision="1")
        self.write("新", "")
        self.assertEqual((await self.read(first.cursor)).kind, "live")
        self.redis.delete(*self.keys)
        missing = await self.read(first.cursor)
        self.assertEqual((missing.kind, missing.value, missing.cursor), ("live", None, None))

    async def test_unknown_metadata_is_rejected_even_when_revision_has_not_changed(self):
        first = await self.read()
        self.redis.hset(self.keys[1], "unknown", "unsupported")
        with self.assertRaisesRegex(ValueError, "live_read_invalid"):
            await self.read(first.cursor)


class TransientStreamTests(SimpleTestCase):
    agent_run = SimpleNamespace(id="run-1", session_id="session-1", status="running")

    def signal(self, revision=2):
        return {**self.fixtures()["live"], "revision": revision}

    @staticmethod
    def fixtures():
        path = Path(__file__).resolve().parents[4] / "tests/fixtures/agent-run-transient-signals.json"
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
