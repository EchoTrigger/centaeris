"""Availability hints observe only the quota's actual lock namespace."""
from contextlib import ExitStack

import psycopg
from django.db import connection
from django.test import TransactionTestCase

from .model_adapter import quota


class QuotaAvailabilityTests(TransactionTestCase):
    # Match the suite snapshot so post_migrate cannot duplicate restored content types.
    serialized_rollback = True

    def test_other_domains_bigint_locks_and_out_of_range_slots_do_not_block(self):
        key = 314160
        with psycopg.connect(**connection.get_connection_params(), autocommit=True) as holder:
            holder.execute("SELECT pg_advisory_lock(%s, %s)", (-key - 1, 1))
            holder.execute("SELECT pg_advisory_lock(%s, %s)", (-key, 2))
            holder.execute("SELECT pg_advisory_lock(%s::bigint)", ((-key << 32) | 1,))
            self.assertTrue(quota._slot_may_be_available(key, 1))
            holder.execute("SELECT pg_advisory_lock(%s, %s)", (-key, 1))
            self.assertFalse(quota._slot_may_be_available(key, 1))
            holder.execute("SELECT pg_advisory_unlock(%s, %s)", (-key, 1))
            self.assertTrue(quota._slot_may_be_available(key, 1))

    def test_first_middle_last_slot_release_is_visible_at_maximum_limit(self):
        key = 314161
        with ExitStack() as stack:
            holder = stack.enter_context(psycopg.connect(**connection.get_connection_params(), autocommit=True))
            for slot in range(1, 65):
                holder.execute("SELECT pg_advisory_lock(%s, %s)", (-key, slot))
            self.assertFalse(quota._slot_may_be_available(key, 64))
            for slot in (1, 32, 64):
                holder.execute("SELECT pg_advisory_unlock(%s, %s)", (-key, slot))
                self.assertTrue(quota._slot_may_be_available(key, 64))
                holder.execute("SELECT pg_advisory_lock(%s, %s)", (-key, slot))
                self.assertFalse(quota._slot_may_be_available(key, 64))
