"""Real PostgreSQL lock ownership and round-trip bounds for quota admission."""
from contextlib import ExitStack

import psycopg
from django.db import connection
from django.test import TransactionTestCase

from .model_adapter.quota import _try_lock, _unlock


class CountingCursor:
    def __init__(self, owner, cursor):
        self.owner, self.cursor = owner, cursor

    def __enter__(self):
        self.cursor.__enter__()
        return self

    def __exit__(self, *args):
        return self.cursor.__exit__(*args)

    def execute(self, *args, **kwargs):
        self.owner.statements += 1
        return self.cursor.execute(*args, **kwargs)

    def fetchone(self):
        return self.cursor.fetchone()


class CountingConnection:
    def __init__(self, conn):
        self.conn = conn
        self.statements = 0

    def cursor(self):
        return CountingCursor(self, self.conn.cursor())


class QuotaSlotQueryTests(TransactionTestCase):
    # Match the suite snapshot so post_migrate cannot duplicate restored content types.
    serialized_rollback = True

    key = 314159

    def open_connections(self, stack):
        params = connection.get_connection_params()
        return [stack.enter_context(psycopg.connect(**params, autocommit=True)) for _ in range(3)]

    def block_slots(self, conn, slots):
        for slot in slots:
            self.assertTrue(conn.execute("SELECT pg_try_advisory_lock(%s, %s)", (-self.key, slot)).fetchone()[0])

    def held_slots(self, observer, conn):
        return [row[0] for row in observer.execute(
            "SELECT objid::bigint FROM pg_locks WHERE locktype = 'advisory' AND pid = %s AND objsubid = 2 ORDER BY objid",
            (conn.info.backend_pid,),
        ).fetchall()]

    def test_full_slot_scan_uses_one_statement_and_acquires_no_lock(self):
        with ExitStack() as stack:
            blocker, applicant, observer = self.open_connections(stack)
            self.block_slots(blocker, range(1, 65))
            counted = CountingConnection(applicant)
            self.assertEqual(_try_lock(counted, self.key, 64), 0)
            self.assertEqual(self.held_slots(observer, applicant), [])
            self.assertEqual(counted.statements, 1)

    def test_single_slot_domain_preserves_exclusion_and_one_statement(self):
        with ExitStack() as stack:
            first, second, observer = self.open_connections(stack)
            admitted, waiting = CountingConnection(first), CountingConnection(second)
            self.assertEqual(_try_lock(admitted, self.key, 1), 1)
            self.assertEqual(self.held_slots(observer, first), [1])
            self.assertEqual(_try_lock(waiting, self.key, 1), 0)
            self.assertEqual(self.held_slots(observer, second), [])
            self.assertEqual((admitted.statements, waiting.statements), (1, 1))
            _unlock(first, self.key, 1)
            self.assertEqual(self.held_slots(observer, first), [])

    def test_first_middle_and_last_free_slot_acquire_exactly_one_lock(self):
        for slot in (1, 32, 64):
            with self.subTest(slot=slot), ExitStack() as stack:
                blocker, applicant, observer = self.open_connections(stack)
                self.block_slots(blocker, range(1, slot))
                counted = CountingConnection(applicant)
                self.assertEqual(_try_lock(counted, self.key, 64), slot)
                self.assertEqual(self.held_slots(observer, applicant), [slot])
                self.assertEqual(counted.statements, 1)
                _unlock(applicant, self.key, slot)
                self.assertEqual(self.held_slots(observer, applicant), [], "one unlock must release ownership without reentrant leaks")
                self.assertTrue(observer.execute("SELECT pg_try_advisory_lock(%s, %s)", (-self.key, slot)).fetchone()[0])

    def test_empty_and_negative_slot_limits_remain_no_admission(self):
        with ExitStack() as stack:
            _blocker, applicant, observer = self.open_connections(stack)
            for limit in (0, -1):
                with self.subTest(limit=limit):
                    counted = CountingConnection(applicant)
                    self.assertEqual(_try_lock(counted, self.key, limit), 0)
                    self.assertEqual(self.held_slots(observer, applicant), [])
                    self.assertEqual(counted.statements, 0)
