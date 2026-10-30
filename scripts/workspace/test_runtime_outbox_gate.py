import os
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

import runtime_outbox_gate as gate


def discovery_output(command):
    selected = [name for name in gate.required_tests() if command[5] in name]
    return SimpleNamespace(stdout=''.join(name + ': test\n' for name in selected or [command[5]]))


class OutboxGateTests(unittest.TestCase):
    def test_default_runner_discovers_and_executes_shared_admission_regressions(self):
        group = 'postgres_store::integration_tests::shared_admission_tests'
        expected = {
            group + '::postgres_four_independent_live_environments_share_observed_resources',
            group + '::postgres_unmeasured_residents_keep_full_declarations_and_block_third_environment',
            group + '::postgres_real_resident_pressure_does_not_wake_itself_on_preparation_unlock',
            group + '::postgres_preparation_unlock_before_registration_rechecks_instead_of_losing_wake',
            group + '::postgres_preparation_unlock_before_yield_is_durable_and_contention_fact_is_idempotent',
            group + '::postgres_mixed_preparation_and_pressure_wake_rechecks_budget_without_self_loop',
        }
        required = gate.required_tests()
        self.assertTrue(expected.issubset(set(required)))
        for executable in (None, 'runtime-tests.exe'):
            with self.subTest(executable=executable):
                discovered, executed = set(), set()

                def run(command, **kwargs):
                    selected_filter = command[5] if executable is None else command[1]
                    selected = {name for name in required if selected_filter in name}
                    if '--list' in command:
                        discovered.update(selected)
                        return SimpleNamespace(stdout=''.join(name + ': test\n' for name in sorted(selected or {selected_filter})))
                    self.assertEqual(command[-2:], ['--ignored', '--test-threads=1'])
                    self.assertTrue(selected.issubset(discovered))
                    executed.update(selected)
                    return SimpleNamespace(returncode=0)

                driver = SimpleNamespace(connect=MagicMock(), sql=MagicMock())
                args = [] if executable is None else ['--test-executable', executable]
                with patch.dict(sys.modules, {'psycopg': driver}), \
                     patch.object(gate.subprocess, 'run', run), \
                     patch.object(gate, 'drop_owned_database'), patch('builtins.print'):
                    gate.main(args)
                self.assertTrue(expected.issubset(discovered))
                self.assertTrue(expected.issubset(executed))
                driver.connect.assert_called_once()

    def test_host_retirement_cases_are_required_and_discovered_before_database_creation(self):
        group = 'postgres_store::integration_tests::host_retirement_tests::'
        expected = {name for name in gate.required_tests() if name.startswith(group)}
        self.assertTrue(expected, 'host retirement must be part of the required gate')
        discovered = set()

        def discover(command, **kwargs):
            output = discovery_output(command)
            discovered.update(gate.discovered_tests(output.stdout))
            return output

        driver = SimpleNamespace(connect=lambda **kwargs: (_ for _ in ()).throw(RuntimeError('discovery complete')), sql=SimpleNamespace())
        with patch.dict(sys.modules, {'psycopg': driver}), patch.object(gate.subprocess, 'run', discover):
            with self.assertRaisesRegex(RuntimeError, 'discovery complete'):
                gate.main()
        self.assertTrue(expected.issubset(discovered))

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
            "postgres_store::integration_tests::cold_append_tests::postgres_cold_append_bounds_overlapping_model_observations",
        }.issubset(filters), filters)

    def test_owner_and_provision_pg_groups_are_required_and_discovered_before_database_creation(self):
        owner = 'postgres_store::integration_tests::environment_owner_schema_tests'
        provision = 'postgres_store::integration_tests::provision_authority_tests'
        expected = {
            owner + '::postgres_environment_owner_accepts_maintenance_origin_without_a_fake_run',
            owner + '::postgres_environment_provision_receipt_is_immutable_and_reopen_checks_authority_facts',
            owner + '::postgres_environment_owner_v12_upgrade_preserves_run_facts_and_migration_history',
            provision + '::postgres_provision_pins_real_authority_and_commits_holding_before_host_without_a_run',
            provision + '::postgres_provision_lost_ack_retains_reservation_and_recovers_exact_instance',
            provision + '::postgres_provision_stale_or_changed_authority_never_enters_host',
        }
        self.assertTrue(expected.issubset(set(gate.required_tests())))
        discovered = set()
        def discover(command, **kwargs):
            output = discovery_output(command)
            discovered.update(gate.discovered_tests(output.stdout))
            return output
        driver = SimpleNamespace(connect=lambda **kwargs: (_ for _ in ()).throw(RuntimeError('discovery complete')), sql=SimpleNamespace())
        with patch.dict(sys.modules, {'psycopg': driver}), patch.object(gate.subprocess, 'run', discover):
            with self.assertRaisesRegex(RuntimeError, 'discovery complete'):
                gate.main()
        self.assertTrue(expected.issubset(discovered))

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



    def test_allocation_settlement_discovery_selects_required_cases_exactly(self):
        group = 'postgres_store::integration_tests::resource_lifecycle_tests::'
        expected = {name for name in gate.required_tests() if name.startswith(group)}
        self.assertTrue(expected)
        calls = []
        def discover(command, **kwargs):
            calls.append(command)
            return discovery_output(command)
        driver = SimpleNamespace(connect=lambda **kwargs: (_ for _ in ()).throw(RuntimeError('after discovery')), sql=SimpleNamespace())
        with patch.dict(sys.modules, {'psycopg': driver}), patch.object(gate.subprocess, 'run', discover):
            with self.assertRaisesRegex(RuntimeError, 'after discovery'):
                gate.main()
        selected = {command[5] for command in calls if '--exact' in command}
        self.assertTrue(expected.issubset(selected))


class ScopedOutboxGateTests(unittest.TestCase):
    def test_scoped_compiled_runner_checks_selected_manifest_before_opening_database(self):
        selected = [name for name in gate.required_tests() if 'runsc_rotation_tests::' in name]
        self.assertTrue(selected)
        calls = []
        def discover(command, **kwargs):
            calls.append(command)
            self.assertEqual(command[:2], ['runtime-tests.exe', 'runsc_rotation_tests::'])
            self.assertIn('--ignored', command)
            self.assertIn('--list', command)
            return SimpleNamespace(stdout=''.join(name + ': test\n' for name in selected))
        driver = SimpleNamespace(connect=lambda **kwargs: (_ for _ in ()).throw(RuntimeError('discovery complete')), sql=SimpleNamespace())
        with patch.dict(sys.modules, {'psycopg': driver}), patch.object(gate.subprocess, 'run', discover):
            with self.assertRaisesRegex(RuntimeError, 'discovery complete'):
                gate.main(['--test-filter', 'runsc_rotation_tests::', '--test-executable', 'runtime-tests.exe'])
        self.assertEqual(len(calls), 1)

    def test_scoped_runner_refuses_missing_selected_identity(self):
        selected = [name for name in gate.required_tests() if 'runsc_rotation_tests::' in name]
        self.assertTrue(selected)
        driver = SimpleNamespace(connect=lambda **kwargs: self.fail('database opened before selected discovery'), sql=SimpleNamespace())
        with patch.dict(sys.modules, {'psycopg': driver}), patch.object(gate.subprocess, 'run', return_value=SimpleNamespace(stdout=''.join(name + ': test\n' for name in selected[1:]))):
            with self.assertRaisesRegex(RuntimeError, selected[0]):
                gate.main(['--test-filter', 'runsc_rotation_tests::'])


class OwnedDatabaseCleanupTests(unittest.TestCase):
    def test_cleanup_requires_exact_database_ownership_comment(self):
        calls = []
        class Admin:
            def execute(self, query, params=None):
                calls.append((query, params))
                return SimpleNamespace(fetchone=lambda: ('centaeris-test-owner:other-owner',))
        with self.assertRaisesRegex(RuntimeError, 'ownership'):
            gate.drop_owned_database(Admin(), 'test_outbox_owned', 'expected-owner', SimpleNamespace())
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][1], ('test_outbox_owned',))

    def test_cleanup_drops_only_the_database_with_matching_owner(self):
        calls = []
        class Statement:
            def __init__(self, text): self.text = text
            def format(self, identifier): return self.text.replace('{}', identifier)
        class Admin:
            def execute(self, query, params=None):
                calls.append((query, params))
                return SimpleNamespace(fetchone=lambda: ('centaeris-test-owner:expected-owner',))
        sql = SimpleNamespace(SQL=Statement, Identifier=lambda name: '"' + name + '"')
        gate.drop_owned_database(Admin(), 'test_outbox_owned', 'expected-owner', sql)
        self.assertEqual(calls[-1], ('DROP DATABASE "test_outbox_owned" WITH (FORCE)', None))


if __name__ == '__main__':
    unittest.main()
