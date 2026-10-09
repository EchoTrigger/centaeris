"""A paused SSE request must leave the real Django pool available to other requests."""
import asyncio
from copy import deepcopy

from asgiref.sync import ThreadSensitiveContext, sync_to_async
from django.db import connections
from django.db.backends.postgresql.base import DatabaseWrapper
from django.test import TransactionTestCase

from .agent_run_stream import _load_terminal_sequence
from .http.streaming import _prepare_agent_run_stream
from .http.stream_response import stream_database_call


class StreamDatabaseLeaseTests(TransactionTestCase):
    # Match the suite snapshot so post_migrate cannot duplicate restored content types.
    serialized_rollback = True

    def setUp(self):
        connections.close_all()
        previous = DatabaseWrapper._connection_pools.pop("default", None)
        if previous is not None:
            previous.close()
        self.config = connections.settings["default"]
        self.options = deepcopy(self.config.get("OPTIONS", {}))
        self.config["OPTIONS"] = {
            **self.options,
            "pool": {"min_size": 0, "max_size": 1, "timeout": .3},
        }

    def tearDown(self):
        connections.close_all()
        pool = DatabaseWrapper._connection_pools.pop("default", None)
        if pool is not None:
            pool.close()
        self.config["OPTIONS"] = self.options

    def _other_request_can_query_while_stream_waits(self, stream_query):
        async def exercise():
            ready = asyncio.Event()
            release = asyncio.Event()

            async def stream_request():
                async with ThreadSensitiveContext():
                    try:
                        await stream_query()
                        ready.set()
                        await release.wait()
                    finally:
                        await sync_to_async(connections.close_all, thread_sensitive=True)()

            def lifecycle_query():
                try:
                    with connections["default"].cursor() as cursor:
                        cursor.execute("SELECT 1")
                        return cursor.fetchone()[0]
                finally:
                    connections.close_all()

            task = asyncio.create_task(stream_request())
            try:
                await asyncio.wait_for(ready.wait(), 3)
                self.assertFalse(task.done(), "the SSE request must still be open")
                async with ThreadSensitiveContext():
                    value = await asyncio.wait_for(
                        sync_to_async(lifecycle_query, thread_sensitive=True)(), 2
                    )
                self.assertEqual(value, 1)
                self.assertFalse(task.done(), "query capacity must not require closing SSE")
            finally:
                release.set()
                await asyncio.wait_for(task, 3)

        asyncio.run(exercise())

    def test_postgres_stream_poll_releases_pool_lease_before_redis_wait(self):
        self._other_request_can_query_while_stream_waits(
            lambda: _load_terminal_sequence("missing-stream-run")
        )

    def test_stream_preparation_releases_pool_lease_before_response_wait(self):
        self._other_request_can_query_while_stream_waits(
            lambda: _prepare_agent_run_stream(
                user_id=0, session_id="missing-session", agent_run_id="missing-run",
                last_event_id="",
            )
        )

    def test_failed_stream_database_call_returns_pool_lease_and_preserves_error(self):
        @stream_database_call
        def failed_query():
            with connections["default"].cursor() as cursor:
                cursor.execute("SELECT 1")
                cursor.fetchone()
            raise ValueError("stream-query-failed")

        async def query():
            with self.assertRaisesRegex(ValueError, "stream-query-failed"):
                await failed_query()

        self._other_request_can_query_while_stream_waits(query)
