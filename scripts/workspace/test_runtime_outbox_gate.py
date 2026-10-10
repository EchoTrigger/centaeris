import os
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import runtime_outbox_gate as gate


class OutboxGateTests(unittest.TestCase):
    def test_database_target_ignores_deployment_settings(self):
        with patch.dict(os.environ, {'DATABASE_URL': 'postgresql://production', 'POSTGRES_PASSWORD': 'production', 'CENTAERIS_TEST_POSTGRES_URL': 'postgresql://production'}, clear=True):
            self.assertEqual(gate.database_settings(), {'host': 'localhost', 'port': '55432', 'dbname': 'centaeris', 'user': 'centaeris', 'password': 'centaeris'})

    def test_empty_discovery_cannot_pass(self):
        with self.assertRaises(RuntimeError):
            gate.discovered_tests('0 tests, 0 benchmarks')
        self.assertEqual(gate.discovered_tests('postgres_store::tests::postgres_outbox_case: test\n'), ['postgres_store::tests::postgres_outbox_case'])

    def test_default_gate_discovers_current_schema_cases_before_opening_database(self):
        calls = []
        def discover(command, **kwargs):
            calls.append(command)
            return SimpleNamespace(stdout=f"{command[5]}: test\n")
        def stop_before_database(**kwargs):
            raise RuntimeError("discovery complete; no database opened")
        driver = SimpleNamespace(connect=stop_before_database, sql=SimpleNamespace())
        with patch.dict(sys.modules, {"psycopg": driver}), patch.object(gate.subprocess, "run", discover):
            with self.assertRaisesRegex(RuntimeError, "no database opened"):
                gate.main()
        self.assertTrue(any("postgres_current_schema" in command for command in calls))
        self.assertTrue(all(command[-2:] == ["--ignored", "--list"] for command in calls))

    def test_resource_regressions_are_discovered_before_opening_database(self):
        calls = []
        def discover(command, **kwargs):
            calls.append((command, kwargs["env"]["RUNTIME_WORKER_PENDING_LIMIT"]))
            return SimpleNamespace(stdout=f"{command[5]}: test\n")
        def stop_before_database(**kwargs):
            raise RuntimeError("discovery complete; no database opened")
        driver = SimpleNamespace(connect=stop_before_database, sql=SimpleNamespace())
        with patch.dict(sys.modules, {"psycopg": driver}), \
             patch.dict(os.environ, {"RUNTIME_WORKER_PENDING_LIMIT": "17"}), \
             patch.object(gate.subprocess, "run", discover):
            with self.assertRaisesRegex(RuntimeError, "no database opened"):
                gate.main()
        filters = {command[5] for command, _limit in calls}
        self.assertTrue({
            "postgres_store::integration_tests::postgres_expired_lease_budget_persists_backoff_fencing_and_isolation",
            "postgres_store::integration_tests::postgres_lease_budget_forward_migration_preserves_published_job_facts",
            "postgres_store::resource_commit::tests::critical_resource_transaction_overrides_asynchronous_session_without_changing_default",
            "postgres_completion_delivery_upgrade",
            "postgres_store::integration_tests::residency_tests",
        }.issubset(filters), filters)
        for command, limit in calls:
            self.assertEqual(limit, "17")


if __name__ == '__main__':
    unittest.main()
