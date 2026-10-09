"""Real PostgreSQL contracts for quota waiters and session connection ownership."""
import asyncio
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import threading
import time
from unittest.mock import patch
import psycopg

from asgiref.sync import async_to_sync, sync_to_async
from django.db import connection, connections
from django.test import TransactionTestCase

from .model_adapter import quota
from .model_adapter.common import ModelProviderError
from . import tests as existing_tests
from .models import ModelQuotaDomain


class QuotaWaitingConnectionTests(TransactionTestCase):
    # Match the suite snapshot so post_migrate cannot duplicate restored content types.
    serialized_rollback = True

    setUp = existing_tests.ModelQuotaAdmissionTests.setUp

    def _count(self):
        with connection.cursor() as cursor:
            cursor.execute("""
                SELECT count(*) FROM pg_stat_activity
                WHERE datname = current_database()
                  AND application_name = 'centaeris-api-quota'
            """)
            return cursor.fetchone()[0]

    def _assert_clean(self):
        deadline = time.monotonic() + 3
        while self._count() and time.monotonic() < deadline:
            time.sleep(.01)
        self.assertEqual(self._count(), 0, "quota sessions leaked after cancellation/exit")

    @contextmanager
    def _observed_wait(self):
        observed = threading.Event()
        original = quota._domain_state
        rounds = 0

        def read(key):
            nonlocal rounds
            result = original(key)
            rounds += 1
            if rounds >= 3:
                observed.set()
            return result

        with patch.object(quota, "_domain_state", side_effect=read):
            yield observed

    def _cooldown(self):
        self.domain.cooldownUntilMs = quota._database_now_ms() + 60_000
        self.domain.save(update_fields=["cooldownUntilMs", "updatedAt"])

    def _sync_wait(self, cooldown, release_holder=False):
        entered = threading.Event()
        cancel = threading.Event()
        failures = []

        def waiter():
            try:
                with quota.model_attempt(self.model, cancel):
                    entered.set()
            except ModelProviderError as error:
                failures.append(error.reasonType)
            finally:
                connections.close_all()

        if cooldown:
            self._cooldown()
        holder = None if cooldown else quota.model_attempt(self.model)
        if holder:
            holder.__enter__()
        try:
            with ThreadPoolExecutor(max_workers=1) as pool:
                with self._observed_wait() as observed:
                    result = pool.submit(waiter)
                    try:
                        self.assertTrue(observed.wait(3), "waiter did not reach admission polling")
                        self.assertFalse(entered.is_set())
                        waiting_count = self._count()
                        if release_holder:
                            holder.__exit__(None, None, None)
                            holder = None
                            self.assertTrue(entered.wait(3), "released slot never admitted waiter")
                        else:
                            cancel.set()
                        result.result(timeout=3)
                    finally:
                        cancel.set()
        finally:
            if holder:
                holder.__exit__(None, None, None)
        self._assert_clean()
        if release_holder:
            self.assertEqual(failures, [])
        else:
            self.assertEqual(failures, ["model_run_cancelled"])
            self.assertFalse(entered.is_set())
        self.assertEqual(waiting_count, 0 if cooldown else 1,
                         "waiting requests must not retain a dedicated quota connection")

    def test_sync_saturated_waiter_has_only_the_active_holder_connection(self):
        self._sync_wait(False)

    def test_sync_cooldown_waiter_has_no_dedicated_connection(self):
        self._sync_wait(True)

    def test_sync_waiter_enters_after_holder_release_without_connection_leak(self):
        self._sync_wait(False, release_holder=True)

    def _async_wait(self, cooldown, release_holder=False):
        if cooldown:
            self._cooldown()

        async def exercise():
            entered = asyncio.Event()

            async def waiter():
                async with quota.async_model_attempt(self.model):
                    entered.set()

            holder = None if cooldown else quota.async_model_attempt(self.model)
            if holder:
                await holder.__aenter__()
            try:
                with self._observed_wait() as observed:
                    waiting = asyncio.create_task(waiter())
                    try:
                        self.assertTrue(await asyncio.to_thread(observed.wait, 3))
                        self.assertFalse(entered.is_set())
                        waiting_count = await sync_to_async(self._count, thread_sensitive=True)()
                        if release_holder:
                            await holder.__aexit__(None, None, None)
                            holder = None
                            await asyncio.wait_for(waiting, 3)
                            self.assertTrue(entered.is_set())
                        else:
                            waiting.cancel()
                            with self.assertRaises(asyncio.CancelledError):
                                await asyncio.wait_for(waiting, 3)
                            self.assertFalse(entered.is_set())
                    finally:
                        if not waiting.done():
                            waiting.cancel()
                            try:
                                await waiting
                            except asyncio.CancelledError:
                                pass
            finally:
                if holder:
                    await holder.__aexit__(None, None, None)
            await sync_to_async(self._assert_clean, thread_sensitive=True)()
            self.assertEqual(waiting_count, 0 if cooldown else 1,
                             "waiting requests must not retain a dedicated quota connection")

        async_to_sync(exercise)()

    def _cooldown_after_lock(self):
        original = quota._try_lock

        def wrapped(conn, key, limit):
            # Retain the real session lock; make the second cooldown read reject it.
            slot = original(conn, key, limit)
            if slot:
                try:
                    ModelQuotaDomain.objects.filter(pk=key).update(
                        cooldownUntilMs=quota._database_now_ms() + 60_000
                    )
                finally:
                    connections.close_all()
            return slot

        return wrapped

    def _assert_slot_released(self):
        with psycopg.connect(**connection.get_connection_params(), autocommit=True) as probe:
            self.assertTrue(probe.execute(
                "SELECT pg_try_advisory_lock(%s, %s)", (-self.domain.pk, 1)
            ).fetchone()[0], "second cooldown check retained the acquired session lock")

    def test_sync_new_cooldown_after_acquisition_releases_connection_and_lock(self):
        sleeping = threading.Event()
        resume = threading.Event()
        cancel = threading.Event()
        entered = threading.Event()

        def pause(_seconds):
            sleeping.set()
            if not resume.wait(3):
                raise TimeoutError("test did not release polling boundary")

        def waiter():
            try:
                with self.assertRaisesRegex(ModelProviderError, "model_run_cancelled"):
                    with quota.model_attempt(self.model, cancel):
                        entered.set()
            finally:
                connections.close_all()

        with patch.object(quota, "_try_lock", side_effect=self._cooldown_after_lock()), \
                patch.object(quota.time, "sleep", side_effect=pause), \
                ThreadPoolExecutor(max_workers=1) as pool:
            result = pool.submit(waiter)
            try:
                self.assertTrue(sleeping.wait(3))
                self.assertFalse(entered.is_set())
                self.assertEqual(self._count(), 0)
                self._assert_slot_released()
            finally:
                cancel.set()
                resume.set()
                result.result(timeout=3)
        self._assert_clean()

    def test_async_new_cooldown_after_acquisition_releases_connection_and_lock(self):
        async def exercise():
            sleeping = asyncio.Event()
            resume = asyncio.Event()
            entered = asyncio.Event()

            async def pause(_seconds):
                sleeping.set()
                await resume.wait()

            async def waiter():
                async with quota.async_model_attempt(self.model):
                    entered.set()

            with patch.object(quota, "_try_lock", side_effect=self._cooldown_after_lock()), \
                    patch.object(quota.asyncio, "sleep", side_effect=pause):
                waiting = asyncio.create_task(waiter())
                try:
                    await asyncio.wait_for(sleeping.wait(), 3)
                    self.assertFalse(entered.is_set())
                    self.assertEqual(await sync_to_async(self._count, thread_sensitive=True)(), 0)
                    await sync_to_async(self._assert_slot_released, thread_sensitive=True)()
                finally:
                    waiting.cancel()
                    with self.assertRaises(asyncio.CancelledError):
                        await asyncio.wait_for(waiting, 3)
            await sync_to_async(self._assert_clean, thread_sensitive=True)()

        async_to_sync(exercise)()

    def test_async_saturated_waiter_has_only_the_active_holder_connection(self):
        self._async_wait(False)

    def test_async_cooldown_waiter_has_no_dedicated_connection(self):
        self._async_wait(True)

    def test_async_waiter_enters_after_holder_release_without_connection_leak(self):
        self._async_wait(False, release_holder=True)

    def test_stale_free_slot_hint_cannot_admit_a_sync_waiter(self):
        sleeping = threading.Event()
        resume = threading.Event()
        cancel = threading.Event()
        entered = threading.Event()

        def pause(_seconds):
            sleeping.set()
            if not resume.wait(3):
                raise TimeoutError("test did not release polling boundary")

        def waiter():
            try:
                with self.assertRaisesRegex(ModelProviderError, "model_run_cancelled"):
                    with quota.model_attempt(self.model, cancel):
                        entered.set()
            finally:
                connections.close_all()

        with quota.model_attempt(self.model):
            with patch.object(quota, "_slot_may_be_available", return_value=True, create=True), \
                    patch.object(quota.time, "sleep", side_effect=pause), \
                    ThreadPoolExecutor(max_workers=1) as pool:
                result = pool.submit(waiter)
                try:
                    self.assertTrue(sleeping.wait(3))
                    waiting_count = self._count()
                    self.assertFalse(entered.is_set())
                finally:
                    cancel.set()
                    resume.set()
                    result.result(timeout=3)
        self._assert_clean()
        self.assertEqual(waiting_count, 1, "failed contender must close before sleeping")

    def test_stale_free_slot_hint_cannot_admit_an_async_waiter(self):
        async def exercise():
            sleeping = asyncio.Event()
            resume = asyncio.Event()
            entered = asyncio.Event()

            async def pause(_seconds):
                sleeping.set()
                await resume.wait()

            async def waiter():
                async with quota.async_model_attempt(self.model):
                    entered.set()

            async with quota.async_model_attempt(self.model):
                with patch.object(quota, "_slot_may_be_available", return_value=True, create=True), \
                        patch.object(quota.asyncio, "sleep", side_effect=pause):
                    waiting = asyncio.create_task(waiter())
                    try:
                        await asyncio.wait_for(sleeping.wait(), 3)
                        waiting_count = await sync_to_async(self._count, thread_sensitive=True)()
                        self.assertFalse(entered.is_set())
                    finally:
                        waiting.cancel()
                        with self.assertRaises(asyncio.CancelledError):
                            await asyncio.wait_for(waiting, 3)
            await sync_to_async(self._assert_clean, thread_sensitive=True)()
            self.assertEqual(waiting_count, 1, "failed contender must close before sleeping")

        async_to_sync(exercise)()
