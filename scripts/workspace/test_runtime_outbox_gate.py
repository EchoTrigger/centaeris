import os
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import runtime_outbox_gate as gate


def discovery_output(command):
    selected = [name for name in gate.required_tests() if command[5] in name]
    return SimpleNamespace(stdout=''.join(name + ': test\n' for name in selected or [command[5]]))


class OutboxGateTests(unittest.TestCase):
    def test_recovery_pagination_and_cas_paths_are_selected(self):
        filters = set()
        def discover(command, **kwargs):
            filters.add(command[5])
            return discovery_output(command)
        driver = SimpleNamespace(connect=lambda **kwargs: (_ for _ in ()).throw(RuntimeError("discovery complete")), sql=SimpleNamespace())
        with patch.dict(sys.modules, {"psycopg": driver}), patch.object(gate.subprocess, "run", discover):
            with self.assertRaisesRegex(RuntimeError, "discovery complete"):
                gate.main()
        self.assertTrue({
            "postgres_store::integration_tests::postgres_runtime_store_recovery_pages_preserve_bytes_and_fixed_watermark",
            "postgres_store::integration_tests::postgres_model_request_cold_recovery_preserves_snapshot",
        }.issubset(filters), filters)

    def test_missing_required_identity_refuses_before_database_creation(self):
        omitted = gate.required_tests()[0]
        def discover(command, **kwargs):
            output = discovery_output(command).stdout.replace(omitted + ': test\n', 'unrelated_test: test\n')
            return SimpleNamespace(stdout=output)
        driver = SimpleNamespace(connect=lambda **kwargs: self.fail('database opened before discovery proof'), sql=SimpleNamespace())
        with patch.dict(sys.modules, {"psycopg": driver}), patch.object(gate.subprocess, "run", discover):
            with self.assertRaisesRegex(RuntimeError, omitted):
                gate.main()

    def test_retained_and_cleanup_migration_identities_are_selected(self):
        calls = []
        def discover(command, **kwargs):
            calls.append(command)
            env = kwargs['env']
            self.assertRegex(env['CENTAERIS_TEST_POSTGRES_DATABASE'], r'^test_outbox_[0-9a-f]{32}$')
            self.assertRegex(env['CENTAERIS_TEST_POSTGRES_OWNERSHIP_TOKEN'], r'^[0-9a-f]{32}$')
            return discovery_output(command)
        driver = SimpleNamespace(connect=lambda **kwargs: (_ for _ in ()).throw(RuntimeError("no database opened")), sql=SimpleNamespace())
        with patch.dict(sys.modules, {"psycopg": driver}), patch.object(gate.subprocess, "run", discover):
            with self.assertRaisesRegex(RuntimeError, "no database opened"):
                gate.main()
        filters = {command[5] for command in calls}
        self.assertTrue({
            "postgres_store::integration_tests::retained_instance_tests",
            "postgres_store::integration_tests::cleanup_migration_tests",
        }.issubset(filters), filters)

    def test_database_target_ignores_deployment_settings(self):
        with patch.dict(os.environ, {'DATABASE_URL': 'postgresql://production', 'POSTGRES_PASSWORD': 'production', 'CENTAERIS_TEST_POSTGRES_URL': 'postgresql://production'}, clear=True):
            self.assertEqual(gate.database_settings(), {'host': 'localhost', 'port': '55432', 'dbname': 'centaeris', 'user': 'centaeris', 'password': 'centaeris'})

    def test_required_discovery_rejects_missing_identity_even_with_same_total(self):
        names = gate.required_tests()
        gate.validate_required_tests(names)
        with self.assertRaisesRegex(RuntimeError, names[0]):
            gate.validate_required_tests(['unrelated_test', *names[1:]])

    def test_empty_discovery_cannot_pass(self):
        with self.assertRaises(RuntimeError):
            gate.discovered_tests('0 tests, 0 benchmarks')
        self.assertEqual(gate.discovered_tests('postgres_store::tests::postgres_outbox_case: test\n'), ['postgres_store::tests::postgres_outbox_case'])

    def test_default_gate_discovers_current_schema_cases_before_opening_database(self):
        calls = []
        def discover(command, **kwargs):
            calls.append(command)
            return discovery_output(command)
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
            return discovery_output(command)
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
