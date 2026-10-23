"""Existing API fixtures isolate and restore a previously populated Runtime schema."""
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from django.db import connection
from django.test import TransactionTestCase, override_settings

from . import test_environment_maintenance as maintenance_fixture
from . import test_agent_message_runtime as message_fixture
from . import test_agent_work_return_runtime as return_fixture
from . import test_workspace_protection_observations as observation_fixture
from .runtime_schema_test_helpers import isolate_runtime_schema


class ExecutionFixtureSchemaIsolationTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.assertEqual(connection.vendor, 'postgresql')
        isolate_runtime_schema(self)
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA runtime')
            cursor.execute('CREATE TABLE runtime.previous_fact(value text)')
            cursor.execute("INSERT INTO runtime.previous_fact VALUES('preserve-original-namespace')")

    def run_existing_case(self, case):
        # Call unittest's lifecycle within this already prepared Django database.
        # Running another Django _pre_setup would restore serialized data twice.
        case.client = case.client_class()
        result = unittest.TestResult()
        # Retain class-level test settings while avoiding a second database
        # pre-setup/serialized restore inside the surrounding transaction case.
        with override_settings(**(getattr(type(case), '_overridden_settings', None) or {})):
            unittest.TestCase.run(case, result)
        self.assertEqual(result.testsRun, 1)
        diagnostics = '\n'.join(trace for _, trace in result.errors + result.failures)
        self.assertTrue(result.wasSuccessful(), diagnostics)
        with connection.cursor() as cursor:
            cursor.execute('SELECT value FROM runtime.previous_fact')
            self.assertEqual(cursor.fetchall(), [('preserve-original-namespace',)])

    def test_publication_fixture_restores_real_required_job_fields_and_prior_rows(self):
        with connection.cursor() as cursor:
            cursor.execute('CREATE TABLE runtime.runtime_jobs('
                'job_id text PRIMARY KEY,job_kind text NOT NULL,status text NOT NULL,'
                'run_at_ms bigint NOT NULL,lease_purpose text NOT NULL DEFAULT \'execute\','
                'lease_owner text,lease_expires_at_ms bigint,idempotency_key text NOT NULL,'
                'session_id text,payload_ref text)')
            cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,run_at_ms,idempotency_key) "
                "VALUES('previous-job','worker.noop','queued',37,'previous-job')")
        self.run_existing_case(observation_fixture.ExecutionProtectionObservationTests(
            'test_stage_then_replay_measures_upload_without_advancing_session_head'))
        with connection.cursor() as cursor:
            cursor.execute('SELECT job_id,run_at_ms FROM runtime.runtime_jobs ORDER BY job_id')
            self.assertEqual(cursor.fetchall(), [('previous-job', 37)])
            cursor.execute("SELECT is_nullable FROM information_schema.columns "
                "WHERE table_schema='runtime' AND table_name='runtime_jobs' AND column_name='run_at_ms'")
            self.assertEqual(cursor.fetchone(), ('NO',))

    def test_maintenance_fixture_restores_previous_execution_table_and_rows(self):
        with connection.cursor() as cursor:
            cursor.execute('CREATE TABLE runtime.execution_instances('
                'execution_id text PRIMARY KEY,instance_id text NOT NULL UNIQUE,'
                'environment_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,'
                'host_id text NOT NULL,authorization_digest text NOT NULL,binding_digest text NOT NULL,image_digest text NOT NULL)')
            cursor.execute("INSERT INTO runtime.execution_instances VALUES "
                "('previous-execution','previous-instance','previous-environment','previous-session',"
                "'previous-run','previous-host','previous-grant','previous-binding','previous-image')")
            cursor.execute('SELECT to_jsonb(i)::text FROM runtime.execution_instances i')
            original = cursor.fetchall()
        self.run_existing_case(maintenance_fixture.EnvironmentMaintenanceTests(
            'test_issuer_binds_recorded_ordinary_instance_and_replays_original_signature'))
        with connection.cursor() as cursor:
            cursor.execute('SELECT to_jsonb(i)::text FROM runtime.execution_instances i')
            self.assertEqual(cursor.fetchall(), original)

    @staticmethod
    def create_power_projection():
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA IF NOT EXISTS runtime')
            cursor.execute('CREATE TABLE runtime.provider_environment_power_operations('
                'environment_id text,operation_sequence bigint,action text,ack_json text)')
            cursor.execute("INSERT INTO runtime.provider_environment_power_operations VALUES('owned-environment',1,'resume',NULL)")
            cursor.execute('CREATE VIEW runtime.provider_environment_power_admission AS '
                "SELECT DISTINCT ON(p.environment_id) p.environment_id,(p.action<>'resume' OR p.ack_json IS NULL) AS blocked "
                'FROM runtime.provider_environment_power_operations p ORDER BY p.environment_id,p.operation_sequence DESC')

    def test_agent_message_fixture_cleans_power_view_and_restores_previous_rows(self):
        outer = self
        class MessageProbe(message_fixture.AgentMessageRuntimeTests):
            @property
            def live_server_url(self):
                return 'http://127.0.0.1:1'

            def runTest(self):
                self._runtime_fixture()
                outer.create_power_projection()
        self.run_existing_case(MessageProbe('runTest'))

    def test_work_return_fixture_cleans_power_view_and_restores_previous_rows(self):
        outer = self
        class ReturnProbe(return_fixture.WorkReturnRuntimeTests):
            @property
            def live_server_url(self):
                return 'http://127.0.0.1:1'

            def runTest(self):
                def write_only_runtime_fixture(*args, **kwargs):
                    outer.create_power_projection()
                    return SimpleNamespace(returncode=0, stdout='', stderr='')
                # Exercise the real Python fixture setup and cleanup, replacing
                # only Cargo invocation; no Rust terminal result is claimed.
                with patch.object(return_fixture.subprocess, 'run', side_effect=write_only_runtime_fixture) as process:
                    self.commit_facts([])
                self.assertEqual(process.call_count, 1)
        self.run_existing_case(ReturnProbe('runTest'))

    def test_maintenance_fixture_control_lease_cannot_acquire_execute_authority(self):
        class MaintenanceProbe(maintenance_fixture.EnvironmentMaintenanceTests):
            def runTest(self):
                run = self.run_fixture()
                with connection.cursor() as cursor:
                    cursor.execute("UPDATE runtime.runtime_jobs SET lease_purpose='control' WHERE job_id=%s",
                        ['agent_run.lifecycle:' + run.pk])
                from .environment_lifecycle import acquire_environment_controller
                with self.assertRaisesRegex(ValueError, 'environment_controller_lease_rejected'):
                    acquire_environment_controller(run.pk, 'worker-test')
        self.run_existing_case(MaintenanceProbe('runTest'))
