import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock, patch

import redis
from django.test import SimpleTestCase

from . import agent_run_stream as stream


class SessionStreamAuthorityTests(SimpleTestCase):
    def setUp(self):
        self.run = SimpleNamespace(id="run", session_id="session", status="running")
        for name, replacement in (
            ("_capture_signal_tail", AsyncMock(return_value="0-0")),
            ("_load_postgres_page", AsyncMock(return_value=([], 0))),
            ("_load_terminal_sequence", AsyncMock(return_value=None)),
            ("_load_live_text_state_async", AsyncMock(return_value=None)),
            ("_load_committed_overlay_projection", AsyncMock(return_value=(0, False))),
        ):
            patcher = patch.object(stream, name, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)

    def record(self, sequence, event_type="user_message"):
        event_id = f"event-{sequence}"
        return {
            "sequence": sequence,
            "eventId": event_id,
            "payload": {
                "sequence": sequence, "eventId": event_id,
                "sessionId": self.run.session_id, "agentRunId": self.run.id,
                "type": event_type,
            },
        }

    async def test_denied_terminal_backlog_is_never_loaded_or_emitted(self):
        self.run.status = "completed"
        check = AsyncMock(return_value=False)
        items = [item async for item in stream.stream_agent_run_session_items_async(
            self.run, 0, authority_check=check,
        )]
        self.assertEqual(items, [])
        stream._load_postgres_page.assert_not_awaited()

    async def test_terminal_backlog_rechecks_before_each_item(self):
        self.run.status = "completed"
        stream._load_postgres_page.return_value = ([
            self.record(1), self.record(2, "agent_run_completed"),
        ], 2)
        check = AsyncMock(side_effect=[True, True, False])
        items = [item async for item in stream.stream_agent_run_session_items_async(
            self.run, 0, authority_check=check,
        )]
        self.assertEqual(len(items), 1)
        self.assertIn('"sourceSequence":1', items[0])
        self.assertEqual(check.await_count, 3)

    async def test_busy_live_stream_rechecks_before_each_emission(self):
        def live(revision):
            return {
                "afterSequence": 0, "revision": revision, "turnId": "turn",
                "messageId": "message", "text": f"revision {revision}",
            }

        signal = {
            "schema": stream.SIGNAL_SCHEMA, "kind": "live", "agentRunId": self.run.id,
            **live(2),
        }
        stream._load_live_text_state_async.side_effect = [live(1), live(2)]
        check = AsyncMock(side_effect=[True, True, False])
        with patch.object(stream._redis_stream_client, "xread", AsyncMock(return_value=[
            ("signals", [("1-0", {"signal": json.dumps(signal)})]),
        ])):
            items = [item async for item in stream.stream_agent_run_session_items_async(
                self.run, 0, authority_check=check,
            )]
        self.assertEqual(len(items), 1)
        self.assertIn('"revision":1', items[0])
        self.assertEqual(check.await_count, 3)

    async def assert_blocked_producer_revocation(self, producer_name, producer_target):
        started = asyncio.Event()
        cancelled = asyncio.Event()
        real_wait = asyncio.wait
        elapsed = 0.0
        current = True
        checks = 0

        async def blocked(*args, **kwargs):
            started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()

        async def check():
            nonlocal checks
            checks += 1
            return current

        async def controlled_wait(tasks, *, timeout, **kwargs):
            nonlocal elapsed, current
            # Authority calls complete through the real event loop. The producer
            # barrier represents a full polling interval without a wall-clock race.
            if checks and current:
                await started.wait()
                elapsed += timeout
                current = False
                return set(), set(tasks)
            return await real_wait(tasks, timeout=timeout, **kwargs)

        with patch.object(producer_target, producer_name, blocked), \
                patch.object(stream.asyncio, "wait", controlled_wait):
            items = [item async for item in stream.stream_agent_run_session_items_async(
                self.run, 0, authority_check=check,
            )]
            await cancelled.wait()
        self.assertEqual(items, [])
        self.assertLessEqual(elapsed, 15.0)
        self.assertEqual(elapsed, stream.AUTHORITY_RECHECK_SECONDS)

    async def test_idle_redis_read_cannot_delay_revocation(self):
        await self.assert_blocked_producer_revocation("xread", stream._redis_stream_client)

    async def test_redis_down_retry_cannot_delay_revocation(self):
        stream._capture_signal_tail.side_effect = redis.ConnectionError("synthetic outage")
        await self.assert_blocked_producer_revocation("sleep", stream.asyncio)

    async def test_blocked_initial_redis_read_cannot_delay_revocation(self):
        await self.assert_blocked_producer_revocation("_capture_signal_tail", stream)

    async def test_blocked_postgres_read_cannot_delay_revocation(self):
        await self.assert_blocked_producer_revocation("_load_postgres_page", stream)

    async def test_authority_failure_closes_without_protocol_event(self):
        for result in (False, None, "true", RuntimeError("synthetic DB failure")):
            with self.subTest(result=result):
                check = AsyncMock(side_effect=result) if isinstance(result, Exception) else AsyncMock(return_value=result)
                items = [item async for item in stream.stream_agent_run_session_items_async(
                    self.run, 0, authority_check=check,
                )]
                self.assertEqual(items, [])
        stream._load_postgres_page.assert_not_awaited()

    async def test_authority_timeout_is_bounded_and_cancelled(self):
        started = asyncio.Event()
        cancelled = asyncio.Event()
        elapsed = 0.0

        async def blocked_check():
            started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()

        async def timeout_wait(tasks, *, timeout, **kwargs):
            nonlocal elapsed
            await started.wait()
            elapsed += timeout
            return set(), set(tasks)

        with patch.object(stream.asyncio, "wait", timeout_wait):
            items = [item async for item in stream.stream_agent_run_session_items_async(
                self.run, 0, authority_check=blocked_check,
            )]
        await cancelled.wait()
        self.assertEqual(items, [])
        self.assertEqual(elapsed, stream.AUTHORITY_CHECK_TIMEOUT_SECONDS)
        self.assertLessEqual(elapsed, 15.0)

    async def test_idle_poll_plus_authority_timeout_stays_within_revocation_bound(self):
        producer_started = asyncio.Event()
        producer_cancelled = asyncio.Event()
        authority_started = asyncio.Event()
        authority_cancelled = asyncio.Event()
        real_wait = asyncio.wait
        checks = 0
        elapsed = 0.0

        async def blocked_read(*args, **kwargs):
            producer_started.set()
            try:
                await asyncio.Event().wait()
            finally:
                producer_cancelled.set()

        async def check():
            nonlocal checks
            checks += 1
            if checks == 1:
                return True
            authority_started.set()
            try:
                await asyncio.Event().wait()
            finally:
                authority_cancelled.set()

        async def controlled_wait(tasks, *, timeout, **kwargs):
            nonlocal elapsed
            if checks == 0:
                return await real_wait(tasks, timeout=timeout, **kwargs)
            if elapsed == 0:
                await producer_started.wait()
            else:
                await authority_started.wait()
            elapsed += timeout
            return set(), set(tasks)

        with patch.object(stream._redis_stream_client, "xread", blocked_read), \
                patch.object(stream.asyncio, "wait", controlled_wait):
            items = [item async for item in stream.stream_agent_run_session_items_async(
                self.run, 0, authority_check=check,
            )]
            await producer_cancelled.wait()
            await authority_cancelled.wait()
        self.assertEqual(items, [])
        self.assertEqual(checks, 2)
        self.assertEqual(elapsed, stream.AUTHORITY_RECHECK_SECONDS + stream.AUTHORITY_CHECK_TIMEOUT_SECONDS)
        self.assertLessEqual(elapsed, 15.0)

    async def test_synchronously_failing_authority_closes_without_loading(self):
        def check():
            raise RuntimeError("synthetic authority API failure")

        items = [item async for item in stream.stream_agent_run_session_items_async(
            self.run, 0, authority_check=check,
        )]
        self.assertEqual(items, [])
        stream._load_postgres_page.assert_not_awaited()

    async def test_cancelled_transport_cancels_pending_authority(self):
        started = asyncio.Event()
        cancelled = asyncio.Event()

        async def check():
            started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()

        iterator = stream.stream_agent_run_session_items_async(self.run, 0, authority_check=check)
        pending = asyncio.create_task(anext(iterator))
        await started.wait()
        pending.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await pending
        await cancelled.wait()
        await iterator.aclose()
        stream._load_postgres_page.assert_not_awaited()

    async def test_cancelled_transport_cancels_pending_producer(self):
        started = asyncio.Event()
        cancelled = asyncio.Event()

        async def blocked_read(*args, **kwargs):
            started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.set()

        with patch.object(stream._redis_stream_client, "xread", blocked_read):
            iterator = stream.stream_agent_run_session_items_async(
                self.run, 0, authority_check=AsyncMock(return_value=True),
            )
            pending = asyncio.create_task(anext(iterator))
            await started.wait()
            pending.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await pending
            await cancelled.wait()
            await iterator.aclose()

    async def test_optional_callback_closes_owned_source(self):
        closed = asyncio.Event()

        async def source(*args):
            try:
                yield "item"
            finally:
                closed.set()

        with patch.object(stream, "_stream_agent_run_session_items_async", source):
            iterator = stream.stream_agent_run_session_items_async(self.run, 0)
            self.assertEqual(await anext(iterator), "item")
            await iterator.aclose()
        self.assertTrue(closed.is_set())

    async def test_optional_callback_preserves_existing_terminal_stream(self):
        self.run.status = "completed"
        stream._load_postgres_page.return_value = ([self.record(1, "agent_run_completed")], 1)
        items = [item async for item in stream.stream_agent_run_session_items_async(self.run, 0)]
        self.assertEqual(len(items), 1)
        self.assertIn('"sourceSequence":1', items[0])


class SessionStreamAuthorityHttpTests(SimpleTestCase):
    async def test_route_binds_browser_and_delegation_authority_to_actual_request(self):
        from .http import streaming

        for delegation in (None, SimpleNamespace(id="delegation", credential_version=3)):
            with self.subTest(delegation=delegation):
                request = SimpleNamespace(
                    user=SimpleNamespace(id=42), app_delegation=delegation, headers={},
                    app_delegation_credential_version=delegation.credential_version if delegation else None,
                    business_branch_id=None,
                )
                run = SimpleNamespace(id="run", session_id="session", status="completed")

                async def source():
                    yield "unused"

                factory = Mock(return_value=source())
                authority = Mock(return_value=True)
                with patch.object(streaming, "_prepare_agent_run_stream", AsyncMock(return_value=(run, 0))), \
                        patch.object(streaming, "stream_agent_run_session_items_async", factory), \
                        patch.object(streaming, "session_authority_is_current", authority):
                    response = await streaming.agent_run_events(request, "session", "run")
                    await response.aclose()
                    callback = factory.call_args.kwargs.get("authority_check")
                    self.assertIsNotNone(callback)
                    # Later request mutation cannot change the stream's bound identity.
                    request.user.id = 99
                    request.app_delegation = SimpleNamespace(id="other-delegation")
                    request.app_delegation_credential_version = 99
                    request.business_branch_id = "another-branch"
                    self.assertTrue(await callback())
                authority.assert_called_once_with(
                    42, "session", delegation_id=delegation.id if delegation else None,
                    scope="events:read",
                    credential_version=delegation.credential_version if delegation else None,
                    business_branch_id=None,
                )
