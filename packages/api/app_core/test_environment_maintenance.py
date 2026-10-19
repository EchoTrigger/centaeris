"""Independent maintenance grants bind recorded instances and durable closure."""
import copy
import json
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from unittest.mock import patch

from django.conf import settings
from django.contrib.auth.models import User
from django.db import IntegrityError, close_old_connections, connection, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase, override_settings

from .models import SessionEnvironment, Workspace
from .testing import create_session


TOKEN = 'maintenance-fixture-' + 'm' * 32


@override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)
class EnvironmentMaintenanceTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        user = User.objects.create_user(username='maintenance-owner')
        self.workspace = Workspace.objects.create(name='Maintenance', createdBy=user)
        self.session = create_session(workspace=self.workspace, owner=user)
        self.environment = self.session.current_environment
        self.instance, self.container = 'instance_' + 'a' * 64, 'b' * 64
        from .environment_profile import bind_environment_profile
        from .test_unified_environment_profile import profile
        bound = bind_environment_profile(self.environment.pk, {'schema': 'workspace.environment_profile.bind.v1',
            'operationId': 'maintenance-profile', 'sessionId': self.session.pk, 'profile': profile()})
        self.profile = bound['profileDigest']
        self.url = f'/internal/environments/{self.environment.pk}/maintenance-authorizations'
        self.body = {'schema': 'workspace.environment_maintenance.issue.v1',
            'operationId': 'prepare', 'controllerPurpose': 'execute', 'expectedControllerEpoch': 0,
            'request': {'program': '/bin/true', 'args': [], 'cwd': '/app', 'timeoutMs': 1000}}
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA IF NOT EXISTS runtime')
            cursor.execute('CREATE TABLE runtime.execution_instances(execution_id text PRIMARY KEY, '
                'allocation_execution_id text, instance_id text, environment_id text, session_id text, '
                'host_id text, image_digest text, profile_digest text, allocation_mode text)')
            cursor.execute('CREATE TABLE runtime.resident_sandboxes(execution_id text PRIMARY KEY, '
                'instance_id text, environment_id text, host_id text, workspace_id text, '
                "container_id text, state text, allocation_mode text DEFAULT 'retained')")
            cursor.execute('CREATE TABLE runtime.environment_retirements(environment_id text PRIMARY KEY, state text)')
            cursor.execute('CREATE TABLE runtime.environment_maintenance_completions('
                'operation_id text PRIMARY KEY, environment_id text, instance_id text, container_id text, '
                'profile_digest text, controller_epoch bigint, controller_lease_digest text, '
                'request_digest text, completion_json text, recorded_at_ms bigint)')
            cursor.execute('INSERT INTO runtime.execution_instances VALUES '
                "('origin','origin',%s,%s,%s,'host','sha256:image',%s,'retained')",
                [self.instance, self.environment.pk, self.session.pk, self.profile])
            cursor.execute('INSERT INTO runtime.resident_sandboxes(execution_id,instance_id,environment_id,host_id,workspace_id,container_id,state) VALUES '
                "('origin',%s,%s,'host',%s,%s,'resident')",
                [self.instance, self.environment.pk, self.workspace.pk, self.container])
        self.addCleanup(self.drop_runtime)

    def drop_runtime(self):
        with connection.cursor() as cursor:
            for table in ('environment_maintenance_completions', 'environment_retirements',
                          'resident_sandboxes', 'execution_instances'):
                cursor.execute('DROP TABLE IF EXISTS runtime.' + table)

    def issue(self, body=None, token=TOKEN):
        headers = {} if token is None else {'HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN': token}
        return self.client.post(self.url, json.dumps(self.body if body is None else body),
            content_type='application/json', **headers)

    def release(self, grant):
        return self.client.post(f'/internal/environments/{self.environment.pk}/maintenance-operations/prepare/release',
            json.dumps({'schema': 'workspace.environment_maintenance.release.v1',
                'controllerEpoch': grant['payload']['controllerEpoch'],
                'controllerLeaseDigest': grant['payload']['controllerLeaseDigest']}),
            content_type='application/json', HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)

    def complete(self, grant, **changes):
        payload = grant['payload']
        row = {'operation_id': 'prepare', 'environment_id': self.environment.pk,
            'instance_id': self.instance, 'container_id': self.container, 'profile_digest': self.profile,
            'controller_epoch': payload['controllerEpoch'],
            'controller_lease_digest': payload['controllerLeaseDigest'],
            'request_digest': payload['requestDigest'],
            'completion_json': json.dumps({'kind': 'operationEnded', 'taskIds': ['task-one'], 'cleanupComplete': True}),
            'recorded_at_ms': 1, **changes}
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.environment_maintenance_completions VALUES('
                + ','.join(['%s'] * len(row)) + ')', list(row.values()))

    def test_authentication_precedes_parsing_and_callers_cannot_select_signed_target(self):
        for token in (None, '', 'wrong', '\u2603', settings.INTERNAL_API_TOKEN):
            with self.subTest(token=token):
                self.assertEqual(self.issue(token=token).status_code, 403)
        for extra in ({'instanceId': self.instance}, {'profileDigest': self.profile},
                      {'expiresAtMs': 1}, {'payload': {}}, {'leaseOwner': 'caller'}):
            with self.subTest(extra=extra):
                self.assertEqual(self.issue(self.body | extra).status_code, 400)

    def test_issuer_binds_recorded_ordinary_instance_and_replays_original_signature(self):
        first = self.issue()
        self.assertEqual(first.status_code, 200, first.content)
        grant = first.json()
        payload = grant['payload']
        self.assertEqual(payload['profileDigest'], self.profile)
        self.assertEqual(payload['containerId'], self.container)
        self.assertEqual(payload['controllerOwner'], {'kind': 'maintenanceOperation', 'operationId': 'prepare'})
        self.assertNotIn('agentRunId', payload)
        self.assertEqual(self.issue().json(), grant)
        changed = copy.deepcopy(self.body)
        changed['request']['program'] = '/bin/false'
        self.assertEqual(self.issue(changed).status_code, 409)
        from .models import AgentRun, EnvironmentMaintenanceGrant
        self.assertFalse(AgentRun.objects.exists())
        self.assertEqual(EnvironmentMaintenanceGrant.objects.count(), 1)
        self.environment.refresh_from_db()
        self.assertIsNone(self.environment.controller_run_id)
        self.assertEqual(self.environment.controller_maintenance_operation_id, 'prepare')
        self.assertFalse(self.environment.controller_released)

    def test_command_deadline_covers_timeout_and_cleanup_without_extending_exact_replay(self):
        request = self.body | {'request': self.body['request'] | {'timeoutMs': 3_600_000}}
        with patch('app_core.environment_maintenance._now_ms', return_value=1000):
            response = self.issue(request)
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        self.assertEqual(grant['payload']['expiresAtMs'], 1000 + 3_600_000 + 30_000)
        with patch('app_core.environment_maintenance._now_ms', return_value=9_999_999):
            self.assertEqual(self.issue(request).json(), grant)

    def test_invalid_command_timeout_or_caller_deadline_does_not_acquire(self):
        for timeout in (0, -1, True, 1.5, 3_600_001, None):
            with self.subTest(timeout=timeout):
                request = self.body | {'request': self.body['request'] | {'timeoutMs': timeout}}
                self.assertEqual(self.issue(request).status_code, 400)
        self.assertEqual(self.issue(self.body | {'expiresAtMs': 99999999}).status_code, 400)
        self.environment.refresh_from_db()
        self.assertEqual(self.environment.controller_epoch, 0)

    def test_unknown_missing_or_retiring_allocation_never_gets_a_grant(self):
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.resident_sandboxes SET state='reserved'")
        self.assertEqual(self.issue().status_code, 409)

        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.resident_sandboxes SET state='resident',container_id=NULL")
        self.assertEqual(self.issue().status_code, 409)
        with connection.cursor() as cursor:
            cursor.execute('UPDATE runtime.resident_sandboxes SET container_id=%s', [self.container])
            cursor.execute("INSERT INTO runtime.environment_retirements VALUES(%s,'pending')", [self.environment.pk])
        self.assertEqual(self.issue().status_code, 409)

    def test_physical_instance_with_a_different_profile_is_never_reauthorized(self):
        with connection.cursor() as cursor:
            cursor.execute('UPDATE runtime.execution_instances SET profile_digest=%s', ['sha256:' + 'c' * 64])
        response = self.issue()
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()['error'], 'maintenance_allocation_profile_mismatch')

    def test_ambiguous_allocation_origins_are_not_signed(self):
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.execution_instances SELECT 'other','other',%s,"
                'environment_id,session_id,host_id,image_digest,profile_digest,allocation_mode '
                "FROM runtime.execution_instances WHERE execution_id='origin'", ['instance_' + 'd' * 64])
        self.assertEqual(self.issue().status_code, 409)

    def test_expired_execute_never_restarts_and_cleanup_keeps_append_only_grants(self):
        first = self.issue()
        self.assertEqual(first.status_code, 200, first.content)
        original = first.json()
        from .models import EnvironmentMaintenanceGrant
        with patch('app_core.environment_maintenance._now_ms', return_value=original['payload']['expiresAtMs'] + 1):
            self.assertEqual(self.issue().json(), original)
            blocked = self.body | {'operationId': 'another', 'expectedControllerEpoch': 1}
            self.assertEqual(self.issue(blocked).status_code, 409)
            cleanup = self.issue(self.body | {'controllerPurpose': 'cleanup', 'expectedControllerEpoch': 1})
            self.assertEqual(cleanup.status_code, 200, cleanup.content)
            current = cleanup.json()
            self.assertEqual(current['payload']['controllerPurpose'], 'cleanup')
            self.assertEqual(current['payload']['controllerEpoch'], 2)
            self.assertEqual(current['payload']['requestDigest'], original['payload']['requestDigest'])
            self.assertEqual(self.issue(self.body | {'controllerPurpose': 'cleanup', 'expectedControllerEpoch': 1}).json(), current)
        self.assertEqual(EnvironmentMaintenanceGrant.objects.count(), 2)
        self.assertEqual(self.issue().json(), original)
        self.assertEqual(EnvironmentMaintenanceGrant.objects.order_by('controller_epoch').first().payload, original['payload'])

    def test_release_requires_exact_runtime_completion_and_replays_after_next_owner(self):
        first = self.issue()
        self.assertEqual(first.status_code, 200, first.content)
        grant = first.json()
        self.assertEqual(self.release(grant).status_code, 409)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)
        self.complete(grant)
        released = self.release(grant)
        self.assertEqual(released.status_code, 200, released.content)
        self.assertTrue(released.json()['released'])
        self.assertEqual(self.issue(self.body | {'operationId': 'next', 'expectedControllerEpoch': 1}).status_code, 200)
        self.assertEqual(self.release(grant).json(), released.json())

    def test_existing_completion_prevents_cleanup_epoch_renewal_after_expiry(self):
        grant = self.issue().json()
        self.complete(grant)
        with patch('app_core.environment_maintenance._now_ms', return_value=grant['payload']['expiresAtMs'] + 1):
            result = self.issue(self.body | {'controllerPurpose': 'cleanup', 'expectedControllerEpoch': 1})
        self.assertEqual(result.status_code, 409, result.content)
        self.assertEqual(result.json()['error'], 'maintenance_completion_available')
        self.environment.refresh_from_db()
        self.assertEqual(self.environment.controller_epoch, 1)
        self.assertEqual(self.release(grant).status_code, 200)

    def test_conflicting_completion_cannot_be_replaced_by_a_cleanup_epoch(self):
        grant = self.issue().json()
        self.complete(grant, container_id='f' * 64)
        with patch('app_core.environment_maintenance._now_ms', return_value=grant['payload']['expiresAtMs'] + 1):
            result = self.issue(self.body | {'controllerPurpose': 'cleanup', 'expectedControllerEpoch': 1})
        self.assertEqual(result.status_code, 409, result.content)
        self.assertEqual(result.json()['error'], 'maintenance_completion_conflict')
        self.environment.refresh_from_db()
        self.assertEqual(self.environment.controller_epoch, 1)
        self.assertEqual(self.release(grant).status_code, 409)

    def test_mismatched_or_incomplete_runtime_receipt_does_not_release(self):
        first = self.issue()
        self.assertEqual(first.status_code, 200, first.content)
        grant = first.json()
        for change in ({'container_id': 'f' * 64}, {'controller_epoch': 2},
                       {'completion_json': json.dumps({'kind': 'operationEnded', 'taskIds': [], 'cleanupComplete': False})}):
            with self.subTest(change=change):
                self.complete(grant, **change)
                self.assertEqual(self.release(grant).status_code, 409)
                with connection.cursor() as cursor:
                    cursor.execute('DELETE FROM runtime.environment_maintenance_completions')

    def test_concurrent_distinct_operations_have_only_one_controller(self):
        barrier = Barrier(2)
        def acquire(operation):
            close_old_connections()
            try:
                from .environment_maintenance import issue_maintenance_authorization
                barrier.wait(timeout=5)
                try:
                    return issue_maintenance_authorization(self.environment.pk,
                        self.body | {'operationId': operation})
                except ValueError:
                    return None
            finally:
                close_old_connections()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(acquire, ('one', 'two')))
        self.assertEqual(sum(row is not None for row in results), 1)

    def test_maintenance_authority_cannot_escape_an_outer_transaction(self):
        with transaction.atomic():
            from .environment_maintenance import issue_maintenance_authorization
            with self.assertRaisesRegex(RuntimeError, 'outermost'):
                issue_maintenance_authorization(self.environment.pk, self.body)

    def run_fixture(self):
        from .agent_run_authorization_factory import create_agent_run_authorization
        from .models import AgentRun, ModelConfig
        self.workspace.members.add(self.session.owner)
        run = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.session.owner,
            modelConfig=ModelConfig.objects.create(displayName='Maintenance fencing'), prompt='next', status='running')
        grant = create_agent_run_authorization(run, image_digest='sha256:' + 'e' * 64)
        job = f'agent_run.lifecycle:{run.pk}'
        with connection.cursor() as cursor:
            cursor.execute('CREATE TABLE IF NOT EXISTS runtime.runtime_jobs(job_id text PRIMARY KEY,job_kind text,status text,'
                'lease_owner text,lease_expires_at_ms bigint,idempotency_key text,session_id text,payload_ref text)')
            cursor.execute('INSERT INTO runtime.runtime_jobs VALUES(%s,%s,%s,%s,'
                '(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)',
                [job, 'agent_run.lifecycle', 'running', 'worker-test', f'{job}:{grant.digest}',
                 self.session.pk, f'record:agent_run:{run.pk}'])
        def cleanup():
            with connection.cursor() as cursor:
                cursor.execute('DELETE FROM runtime.runtime_jobs WHERE job_id=%s', [job])
        self.addCleanup(cleanup)
        return run

    def test_run_and_maintenance_controllers_are_mutually_exclusive(self):
        grant = self.issue().json()
        run = self.run_fixture()
        from .environment_lifecycle import acquire_environment_controller
        with self.assertRaisesRegex(ValueError, 'owned'):
            acquire_environment_controller(run.pk, 'worker-test')
        with transaction.atomic(), self.assertRaises(IntegrityError):
            SessionEnvironment.objects.filter(pk=self.environment.pk).update(controller_run=run)
        self.complete(grant)
        self.assertEqual(self.release(grant).status_code, 200)
        authority = acquire_environment_controller(run.pk, 'worker-test')
        self.environment.refresh_from_db()
        self.assertIsNone(self.environment.controller_maintenance_operation_id)
        self.assertEqual(self.environment.controller_run_id, run.pk)
        self.assertEqual(self.issue(self.body | {'operationId': 'next',
            'expectedControllerEpoch': authority['controllerEpoch']}).status_code, 409)

    def test_forward_migration_preserves_existing_run_controller(self):
        from .migration_testing import isolated_migration_database
        from .test_environment_network_migration import EnvironmentNetworkMigrationTests
        helper = EnvironmentNetworkMigrationTests()
        previous = [('app_core', '0021_environment_network')]
        current = [('app_core', '0022_environment_maintenance_authority')]
        with isolated_migration_database(previous) as database:
            fixture = helper.fixture(database, previous)
            session, environment = helper.session(database, fixture)
            run, _ = helper.run_and_grant(fixture, session, environment, 1)
            fixture.manager('SessionEnvironment').filter(pk=environment.pk).update(controller_run_id=run.pk,
                controller_epoch=7, controller_lease_owner='worker-original', controller_purpose='execute', controller_released=False)
            MigrationExecutor(database).migrate(current)
            state = MigrationExecutor(database).loader.project_state(current).apps
            converted = state.get_model('app_core', 'SessionEnvironment').objects.using(database.alias).get(pk=environment.pk)
            self.assertEqual((converted.controller_run_id, converted.controller_epoch, converted.controller_lease_owner,
                converted.controller_purpose, converted.controller_released), (run.pk, 7, 'worker-original', 'execute', False))
            self.assertIsNone(converted.controller_maintenance_operation_id)

    def test_authority_history_prevents_reverse_migration(self):
        from .migration_testing import isolated_migration_database
        from .test_environment_network_migration import EnvironmentNetworkMigrationTests
        current = [('app_core', '0022_environment_maintenance_authority')]
        helper = EnvironmentNetworkMigrationTests()
        with isolated_migration_database(current) as database:
            fixture = helper.fixture(database, current)
            session, environment = helper.session(database, fixture)
            fixture.manager('EnvironmentMaintenanceOperation').create(operation_id='historical', workspace_id=fixture.workspace.pk,
                session_id=session.pk, environment_id=environment.pk, instance_id=self.instance, container_id=self.container,
                host_id='host', allocation_execution_id='origin', profile_digest=self.profile,
                request={'program': '/bin/true'}, request_digest='sha256:' + 'a' * 64)
            with self.assertRaisesRegex(RuntimeError, 'requires_forward_migration'):
                MigrationExecutor(database).migrate([('app_core', '0021_environment_network')])

    def test_original_request_and_grants_cannot_be_replaced(self):
        original = self.issue().json()
        from .models import EnvironmentMaintenanceGrant, EnvironmentMaintenanceOperation
        grant = EnvironmentMaintenanceGrant.objects.get(controller_epoch=1)
        grant.signature = 'changed'
        with self.assertRaisesRegex(ValueError, 'immutable'):
            grant.save()
        operation = EnvironmentMaintenanceOperation.objects.get(pk='prepare')
        operation.request = {'operation': 'changed'}
        with self.assertRaisesRegex(ValueError, 'immutable'):
            operation.save()
        values = {field.attname: getattr(operation, field.attname) for field in operation._meta.concrete_fields}
        with transaction.atomic(), self.assertRaises((IntegrityError, ValueError)):
            EnvironmentMaintenanceOperation(**values).save()
        self.assertEqual(EnvironmentMaintenanceGrant.objects.get(controller_epoch=1).signature, original['signature'])

