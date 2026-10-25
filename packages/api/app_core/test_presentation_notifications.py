import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock
from django.test import SimpleTestCase
from .presentation_notifications import PresentationHint


class PresentationNotificationsTests(SimpleTestCase):
    def listener(self, ready, **options):
        from .presentation_notifications import listen_presentation_changes
        return listen_presentation_changes(ready, connection_parameters=lambda: {"dbname": "fixture"}, **options)

    async def test_listen_ready_follows_subscription_and_iterator_close_releases_connection(self):
        gate = asyncio.Event()
        async def subscribe(_sql):
            await gate.wait()
        connection = SimpleNamespace(execute=subscribe, close=AsyncMock())
        ready = asyncio.Event()
        connect = AsyncMock(return_value=connection)
        stream = self.listener(ready, connect=connect)
        task = asyncio.create_task(anext(stream))
        await asyncio.sleep(0)
        self.assertFalse(ready.is_set())
        gate.set()
        self.assertEqual(await task, PresentationHint(None, True))
        self.assertTrue(ready.is_set())
        self.assertTrue(connect.call_args.kwargs["autocommit"])
        self.assertEqual(connect.call_args.kwargs["connect_timeout"], 5)
        await stream.aclose()
        connection.close.assert_awaited_once()

    async def test_initial_connect_and_five_reconnects_have_bounded_backoff(self):
        ready, connect, wait = asyncio.Event(), AsyncMock(side_effect=OSError("fixture offline")), AsyncMock()
        stream = self.listener(ready, connect=connect, wait=wait)
        with self.assertRaisesRegex(RuntimeError, "listener unavailable"):
            await anext(stream)
        self.assertEqual(connect.await_count, 6)
        self.assertEqual([call.args[0] for call in wait.await_args_list], [.5, 1, 2, 4, 8])
        self.assertFalse(ready.is_set())

    async def test_notification_keys_and_empty_wait_reconciliation(self):
        from .presentation_changes import CHANNEL
        batches = [[SimpleNamespace(channel="other", payload="ignored"),
                    SimpleNamespace(channel=CHANNEL, payload="agent:agent:history")], []]
        timeouts = []
        async def notifies(*, timeout):
            timeouts.append(timeout)
            for item in batches.pop(0):
                yield item
        connection = SimpleNamespace(execute=AsyncMock(), close=AsyncMock(), notifies=notifies)
        stream = self.listener(asyncio.Event(), connect=AsyncMock(return_value=connection))
        self.assertEqual(await anext(stream), PresentationHint(None, True))
        self.assertEqual(await anext(stream), PresentationHint("agent:agent:history", True))
        self.assertEqual(await anext(stream), PresentationHint(None, True))
        self.assertEqual(timeouts, [30, 30])
        await stream.aclose()
        connection.close.assert_awaited_once()

    async def test_failed_connection_closes_before_reconnect_and_reconciliation(self):
        order = []
        async def broken_notifies(*, timeout):
            raise OSError("fixture lost connection")
            yield
        first = SimpleNamespace(execute=AsyncMock(), notifies=broken_notifies, close=AsyncMock(side_effect=lambda: order.append("close-first")))
        second = SimpleNamespace(execute=AsyncMock(), close=AsyncMock())
        async def connect(**_params):
            order.append("connect")
            return first if order.count("connect") == 1 else second
        stream = self.listener(asyncio.Event(), connect=connect, wait=AsyncMock())
        self.assertEqual(await anext(stream), PresentationHint(None, True))
        self.assertEqual(await anext(stream), PresentationHint(None, True))
        self.assertEqual(order, ["connect", "close-first", "connect"])
        await stream.aclose()
        second.close.assert_awaited_once()

    async def test_cancellation_closes_socket_without_reconnect(self):
        waiting = asyncio.Event()
        async def notifies(*, timeout):
            waiting.set()
            await asyncio.Event().wait()
            yield
        connection = SimpleNamespace(execute=AsyncMock(), close=AsyncMock(), notifies=notifies)
        connect = AsyncMock(return_value=connection)
        stream = self.listener(asyncio.Event(), connect=connect)
        await anext(stream)
        task = asyncio.create_task(anext(stream))
        await waiting.wait()
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        connection.close.assert_awaited_once()
        connect.assert_awaited_once()

    async def test_failed_close_stops_before_opening_another_listener_connection(self):
        async def broken_notifies(*, timeout):
            raise OSError("fixture lost socket")
            yield
        connection = SimpleNamespace(execute=AsyncMock(), notifies=broken_notifies,
                                     close=AsyncMock(side_effect=OSError("fixture close failed")))
        connect = AsyncMock(return_value=connection)
        stream = self.listener(asyncio.Event(), connect=connect, wait=AsyncMock())
        await anext(stream)
        with self.assertRaisesRegex(RuntimeError, "close failed"):
            await anext(stream)
        connect.assert_awaited_once()

    async def test_a_hung_connect_is_cancelled_at_the_five_second_deadline_and_budget_is_not_reset(self):
        from unittest.mock import patch
        from . import presentation_notifications as notifications
        deadlines = []
        original_wait = asyncio.wait_for
        async def accelerated_wait(awaitable, timeout):
            deadlines.append(timeout)
            return await original_wait(awaitable, .01)
        async def hung_connect(**_parameters):
            await asyncio.Event().wait()
        connect, pause = AsyncMock(side_effect=hung_connect), AsyncMock()
        stream = self.listener(asyncio.Event(), connect=connect, wait=pause)
        with patch.object(notifications.asyncio, "wait_for", side_effect=accelerated_wait):
            with self.assertRaisesRegex(RuntimeError, "listener unavailable"):
                await anext(stream)
        self.assertEqual(deadlines, [5] * 6)
        self.assertEqual(connect.await_count, 6)
        self.assertEqual(pause.await_count, 5)

    async def test_source_and_projected_channels_keep_distinct_relay_requirements(self):
        from .presentation_changes import CHANNEL, PROJECTED_CHANNEL
        async def notifies(*, timeout):
            yield SimpleNamespace(channel=CHANNEL, payload="agent:agent:history")
            yield SimpleNamespace(channel=PROJECTED_CHANNEL, payload="agent:agent:history")
        connection = SimpleNamespace(execute=AsyncMock(), close=AsyncMock(), notifies=notifies)
        stream = self.listener(asyncio.Event(), connect=AsyncMock(return_value=connection))
        try:
            startup = await anext(stream)
            self.assertEqual(connection.execute.await_count, 2)
            subscribed = [call.args[0].as_string() for call in connection.execute.await_args_list]
            self.assertTrue(any(CHANNEL in statement for statement in subscribed))
            self.assertTrue(any(PROJECTED_CHANNEL in statement for statement in subscribed))
            self.assertIsNone(startup.key)
            self.assertTrue(startup.needs_drain)
            source = await anext(stream)
            projected = await anext(stream)
            self.assertEqual((source.key, source.needs_drain), ("agent:agent:history", True))
            self.assertEqual((projected.key, projected.needs_drain), ("agent:agent:history", False))
        finally:
            await stream.aclose()
