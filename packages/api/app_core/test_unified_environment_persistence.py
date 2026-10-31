"""Persisted profile authority and forward reconstruction on PostgreSQL."""
from django.db.migrations.executor import MigrationExecutor
from django.test import TestCase, TransactionTestCase, override_settings

from .agent_run_authorization_factory import create_agent_run_authorization
from .environment_profile import environment_profile_digest
from .migration_testing import isolated_migration_database
from .models import AgentRun
from .runtime_contract import authorization_digest
from .test_hosted_operations import OperationFixture


class EnvironmentProfilePersistenceTests(OperationFixture, TestCase):
    def test_first_authorization_freezes_defaults_and_later_runs_reuse_exact_profile(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        run = AgentRun.objects.get(pk=first.json()['agentRunId'])
        frozen = run.authorization.payload['environmentProfile']
        self.assertEqual(frozen['workspaceRoot'], '/mnt/data')
        self.assertEqual(frozen['user'], '10001:10001')
        run.environment.refresh_from_db()
        self.assertEqual(run.environment.environment_profile, frozen)
        self.assertEqual(run.environment.environment_profile_digest, environment_profile_digest(frozen))
        AgentRun.objects.filter(pk=run.pk).update(status='completed')
        self.profile.return_value = {'schema': 'runtime.execution_profile.v1', 'imageDigest': 'sha256:' + 'b' * 64}
        with override_settings(SANDBOX_CPU_MILLI=9999):
            second = self.post(self.create_url + f'/{run.session_id}/messages', operationId='second')
        self.assertEqual(second.status_code, 202, second.content)
        self.assertEqual(AgentRun.objects.get(pk=second.json()['agentRunId']).authorization.payload['environmentProfile'], frozen)

    def test_unprofiled_environment_with_run_history_is_not_filled_from_current_defaults(self):
        from .models import Session
        session = Session.objects.get(pk=self.create().json()['sessionId'])
        for text in ('historical', 'candidate'):
            run = AgentRun.objects.create(session=session, workspace=self.workspace, user=self.user,
                modelConfig=self.model, prompt=text, status='completed' if text == 'historical' else 'queued',
                definition_version=self.agent.definition.published_version,
                agent_instructions=self.agent.definition.published_version.instructions)
        with self.assertRaisesMessage(ValueError, 'environment_profile_history_unavailable'):
            create_agent_run_authorization(run, image_digest='sha256:' + 'a' * 64)
        session.current_environment.refresh_from_db()
        self.assertIsNone(session.current_environment.environment_profile)


class EnvironmentProfileReconstructionTests(TransactionTestCase):
    serialized_rollback = True

    def test_reconstructs_only_authoritative_profile_and_preserves_signed_history(self):
        from importlib import import_module
        from .migration_authorization_fixtures import historical_digest, historical_signature
        from .test_environment_network_migration import EnvironmentNetworkMigrationTests
        historical_contract = import_module('app_core.migrations.0023_unified_environment_profile')
        helper = EnvironmentNetworkMigrationTests()
        old = [('app_core', '0022_environment_maintenance_authority')]
        current = [('app_core', '0023_unified_environment_profile')]
        with isolated_migration_database(old) as database:
            fixture = helper.fixture(database, old)
            selected, configured = helper.session(database, fixture)
            helper.bind(database, fixture, configured)
            run, grant = helper.run_and_grant(fixture, selected, configured, 2)
            original_grant = (grant.payload, grant.digest, grant.signature)
            original_profile = (configured.benchmark_profile, configured.benchmark_profile_digest)
            empty_session, empty = helper.session(database, fixture)
            _, historical_ordinary = helper.run_and_grant(fixture, empty_session, empty, 1)
            MigrationExecutor(database).migrate(current)
            state = MigrationExecutor(database).loader.project_state(current).apps
            environment = state.get_model('app_core', 'SessionEnvironment').objects.using(database.alias).get(pk=configured.pk)
            self.assertEqual(environment.historical_profile, {'profile': original_profile[0], 'profileDigest': original_profile[1]})
            self.assertEqual(environment.environment_profile, original_profile[0] | {'schema': 'runtime.environment_profile.v1'})
            historical_contract.validate_historical_profile(environment.environment_profile)
            self.assertEqual(environment.environment_profile_digest, historical_digest(environment.environment_profile))
            self.assertNotEqual(environment.environment_profile_digest, original_profile[1])
            stored = state.get_model('app_core', 'AgentRunAuthorization').objects.using(database.alias).get(pk=grant.pk)
            self.assertEqual((stored.payload, stored.digest, stored.signature), original_grant)
            for authorization in (stored, historical_ordinary):
                self.assertEqual(authorization.digest, historical_digest(authorization.payload))
                self.assertEqual(authorization.signature, historical_signature(authorization.payload, 'test-key'))
                with self.assertRaises(ValueError):
                    authorization_digest(authorization.payload)
            unprofiled = state.get_model('app_core', 'SessionEnvironment').objects.using(database.alias).get(pk=empty.pk)
            self.assertIsNone(unprofiled.environment_profile)
            self.assertEqual(unprofiled.environment_profile_digest, '')


class MaintenanceGrantDigestMigrationTests(TransactionTestCase):
    serialized_rollback = True
    def test_forward_records_original_payload_digest_without_resigning_or_reinterpreting(self):
        import hashlib
        import json
        from .test_environment_network_migration import EnvironmentNetworkMigrationTests
        old = [('app_core', '0024_environment_provision_authority')]
        current = [('app_core', '0025_maintenance_grant_digest')]
        helper = EnvironmentNetworkMigrationTests()
        with isolated_migration_database(old) as database:
            fixture = helper.fixture(database, old)
            session, environment = helper.session(database, fixture)
            operation = fixture.manager('EnvironmentMaintenanceOperation').create(operation_id='old-provision',
                operation_kind='provision', workspace_id=fixture.workspace.pk, session_id=session.pk,
                environment_id=environment.pk, profile_digest='sha256:' + 'a' * 64,
                request={'historicalFact': True}, request_digest='sha256:' + 'b' * 64)
            payload = {'schema': 'historical.fact', 'original': ['unchanged', 3]}
            grant = fixture.manager('EnvironmentMaintenanceGrant').create(operation_id=operation.pk,
                previous_controller_epoch=0, controller_epoch=1, controller_purpose='execute',
                controller_lease_digest='sha256:' + 'c' * 64, expires_at_ms=1000,
                payload=payload, signature='original-signature')
            MigrationExecutor(database).migrate(current)
            apps = MigrationExecutor(database).loader.project_state(current).apps
            stored = apps.get_model('app_core', 'EnvironmentMaintenanceGrant').objects.using(database.alias).get(pk=grant.pk)
            self.assertEqual(stored.payload, payload)
            self.assertEqual(stored.signature, 'original-signature')
            self.assertEqual(stored.digest, 'sha256:' + hashlib.sha256(json.dumps(payload, ensure_ascii=False,
                sort_keys=True, separators=(',', ':')).encode()).hexdigest())
