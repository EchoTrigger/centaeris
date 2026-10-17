"""Add benchmark bindings without changing existing v1 grants or Run history."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .benchmark_profile import benchmark_profile_digest
from .migration_testing import isolated_migration_database
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture
from .test_benchmark_profile import profile


class BenchmarkProfileMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [('app_core', '0019_workspace_snapshot_encoding')]
    current = [('app_core', '0020_benchmark_execution_profile')]

    def test_forward_preserves_v1_history_and_bound_profile_blocks_reversal(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            old = MigrationExecutor(database).loader.project_state(self.old).apps
            manager = lambda name: old.get_model('app_core', name).objects.using(alias)
            user = old.get_model('auth', 'User').objects.using(alias).create(username='benchmark-migration')
            workspace = manager('Workspace').create(name='Benchmark', createdBy_id=user.pk)
            membership = manager('WorkspaceMembership').create(workspace_id=workspace.pk, user_id=user.pk, role='owner')
            agent = manager('Agent').create(workspace_id=workspace.pk, owner_id=user.pk, name='Benchmark')
            model = manager('ModelConfig').create(displayName='Benchmark')
            with transaction.atomic(using=alias):
                session = manager('Session').create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
                environment = manager('SessionEnvironment').create(session_id=session.pk)
                manager('Session').filter(pk=session.pk).update(current_environment_id=environment.pk)
            run = manager('AgentRun').create(workspace_id=workspace.pk, user_id=user.pk,
                session_id=session.pk, environment_id=environment.pk, membership_ref=membership.pk,
                modelConfig_id=model.pk, prompt='historical', status='completed')
            payload = fixture() | {'id': 'auth-migration', 'workspaceId': workspace.pk,
                'userId': str(user.pk), 'sessionId': session.pk, 'agentId': agent.pk,
                'agentRunId': run.pk, 'modelConfigRef': model.pk}
            original = (payload, authorization_digest(payload), authorization_signature(payload, 'test-key'))
            manager('AgentRunAuthorization').create(id=payload['id'], agent_run_id=run.pk,
                payload=payload, digest=original[1], signature=original[2])
            MigrationExecutor(database).migrate(self.current)
            upgraded = MigrationExecutor(database).loader.project_state(self.current).apps
            grants = upgraded.get_model('app_core', 'AgentRunAuthorization').objects.using(alias)
            grant = grants.get(agent_run_id=run.pk)
            self.assertEqual((grant.payload, grant.digest, grant.signature), original)
            environments = upgraded.get_model('app_core', 'SessionEnvironment').objects.using(alias)
            converted = environments.get(pk=environment.pk)
            self.assertIsNone(converted.benchmark_profile)
            self.assertIsNone(converted.benchmark_setup_completed_at)
            # Empty migration can be rolled back; no signed benchmark fact is erased.
            MigrationExecutor(database).migrate(self.old)
            MigrationExecutor(database).migrate(self.current)
            state = MigrationExecutor(database).loader.project_state(self.current).apps
            with transaction.atomic(using=alias):
                fresh = state.get_model('app_core', 'Session').objects.using(alias).create(
                    workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
                fresh_environment = state.get_model('app_core', 'SessionEnvironment').objects.using(alias).create(session_id=fresh.pk)
                state.get_model('app_core', 'Session').objects.using(alias).filter(pk=fresh.pk).update(current_environment_id=fresh_environment.pk)
            state.get_model('app_core', 'SessionEnvironment').objects.using(alias).filter(pk=fresh_environment.pk).update(
                benchmark_profile=profile(), benchmark_profile_digest=benchmark_profile_digest(profile()),
                benchmark_profile_operation_id='migration-bind', benchmark_profile_bound_at=timezone.now())
            with self.assertRaisesRegex(RuntimeError, 'history_requires_forward_migration'):
                MigrationExecutor(database).migrate(self.old)
            final = grants.get(agent_run_id=run.pk)
            self.assertEqual((final.payload, final.digest, final.signature), original)
