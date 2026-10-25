"""Provider provision authority reads exact verified Runtime facts without inventing a Run."""
import json

from django.conf import settings
from django.contrib.auth.models import User
from django.db import connection
from django.test import TransactionTestCase, override_settings

from .environment_maintenance_authorization import sign_provision_authorization
from .environment_profile import bind_environment_profile
from .models import AgentRun, EnvironmentMaintenanceOperation, Workspace
from .runtime_schema_test_helpers import isolate_runtime_schema
from .test_e2b_environment_profile import profile
from .testing import create_session

TOKEN = 'provider-provision-test-' + 'p' * 32


@override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)
class ProviderEnvironmentProvisionTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        isolate_runtime_schema(self)
        self.user = User.objects.create_user(username='provider-provision-owner')
        self.workspace = Workspace.objects.create(name='Provider provision', createdBy=self.user)
        self.workspace.members.add(self.user)
        self._bind(profile() | {'lifetime': 'retained'})
        self.attempt = 'provider_attempt_test'
        self.instance = 'instance_' + 'a' * 64
        self.sandbox = 'sandbox-provider-test'
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA runtime')
            # Controlled read-side facts; immutable Runtime writers have separate tests.
            cursor.execute('CREATE TABLE runtime.provider_creation_operations('
                'attempt_id text PRIMARY KEY,execution_id text,instance_id text,environment_id text,session_id text,'
                'workspace_id text,agent_run_id text,processing_job_id text,authorization_digest text,profile_digest text,'
                'provider_config_digest text,provider text,dispatch_state text,creation_owner_json text,'
                'maintenance_operation_id text,verified_sandbox_id text,verification_digest text,verified_at_ms bigint)')
            cursor.execute('CREATE TABLE runtime.provider_environment_bindings('
                'environment_id text PRIMARY KEY,creation_attempt_id text,recorded_at_ms bigint)')
            cursor.execute('CREATE TABLE runtime.provider_environment_provision_completions('
                'operation_id text PRIMARY KEY,creation_attempt_id text,environment_id text,session_id text,workspace_id text,'
                'instance_id text,sandbox_id text,profile_digest text,provider_config_digest text,authorization_digest text,'
                'controller_epoch bigint,controller_lease_digest text,request_digest text,preparation_digest text,recorded_at_ms bigint)')

    def _bind(self, selected):
        self.session = create_session(workspace=self.workspace, owner=self.user)
        self.environment = self.session.current_environment
        self.bound = bind_environment_profile(self.environment.pk, {'schema': 'workspace.environment_profile.bind.v1',
            'operationId': 'profile', 'sessionId': self.session.pk, 'profile': selected})
        self.url = f'/internal/environments/{self.environment.pk}/provision-authorizations'
        self.body = {'schema': 'workspace.environment_provision.issue.v1', 'operationId': 'provider-provision-one',
            'sessionId': self.session.pk, 'expectedControllerEpoch': 0}

    def issue(self, body=None):
        return self.client.post(self.url, json.dumps(self.body if body is None else body),
            content_type='application/json', HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)

    def release(self, grant):
        return self.client.post(
            f'/internal/environments/{self.environment.pk}/maintenance-operations/{self.body["operationId"]}/release',
            json.dumps({'schema': 'workspace.environment_maintenance.release.v1',
                'controllerEpoch': grant['payload']['controllerEpoch'],
                'controllerLeaseDigest': grant['payload']['controllerLeaseDigest']}),
            content_type='application/json', HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)

    def _coexisting_native_inventory(self):
        with connection.cursor() as cursor:
            cursor.execute('CREATE TABLE runtime.execution_instances(environment_id text,allocation_execution_id text)')
            cursor.execute('CREATE TABLE runtime.environment_provision_completions(operation_id text PRIMARY KEY,'
                'environment_id text,session_id text,workspace_id text,host_id text,instance_id text,container_id text,'
                'profile_digest text,controller_epoch bigint,controller_lease_digest text,request_digest text,'
                'completion_json text,recorded_at_ms bigint)')

    def _grant_with_coexisting_native_inventory(self):
        self._coexisting_native_inventory()
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()

    def _provider_receipt(self, grant):
        payload = grant['payload']
        operation = EnvironmentMaintenanceOperation.objects.get(pk=self.body['operationId'])
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.provider_creation_operations VALUES('
                + ','.join(['%s'] * 18) + ')', [self.attempt, 'execution-provider-test', self.instance,
                    self.environment.pk, self.session.pk, self.workspace.pk, None, None, operation.current_grant.digest,
                    payload['environmentProfileDigest'], payload['environmentProfile']['providerConfigDigest'], 'e2b',
                    'dispatched', json.dumps({'kind': 'maintenanceOperation', 'operationId': operation.pk}),
                    operation.pk, self.sandbox, 'sha256:' + 'd' * 64, 1])
            cursor.execute('INSERT INTO runtime.provider_environment_bindings VALUES(%s,%s,1)',
                [self.environment.pk, self.attempt])
            cursor.execute('INSERT INTO runtime.provider_environment_provision_completions VALUES('
                + ','.join(['%s'] * 15) + ')', [operation.pk, self.attempt, self.environment.pk, self.session.pk,
                    self.workspace.pk, self.instance, self.sandbox, payload['environmentProfileDigest'],
                    payload['environmentProfile']['providerConfigDigest'], operation.current_grant.digest,
                    payload['controllerEpoch'], payload['controllerLeaseDigest'], payload['requestDigest'],
                    'sha256:' + 'e' * 64, 1])

    def test_provider_provision_issues_without_native_inventory_or_a_run_and_keeps_generic_signature(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        payload = grant['payload']
        self.assertEqual(payload['schema'], 'workspace.environment_provision_authorization.v1')
        self.assertEqual(payload['environmentProfile'], self.bound['profile'])
        self.assertEqual(payload['environmentProfileDigest'], self.bound['profileDigest'])
        self.assertEqual(grant['signature'],
            sign_provision_authorization(payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY))
        self.assertFalse(AgentRun.objects.exists())
        self.assertTrue({'instanceId', 'containerId', 'hostId', 'sandboxId'}.isdisjoint(payload))
        operation = EnvironmentMaintenanceOperation.objects.get(pk=self.body['operationId'])
        self.assertIsNone(operation.instance_id)
        self.assertIsNone(operation.container_id)
        self.assertEqual(self.issue().json(), grant)
        self.assertEqual(self.issue(self.body | {'operationId': 'competing'}).status_code, 409)

    def test_per_run_profile_does_not_authorize_an_independent_provider_instance(self):
        self._coexisting_native_inventory()
        self._bind(profile())
        response = self.issue()
        self.assertEqual(response.status_code, 409, response.content)
        self.assertFalse(EnvironmentMaintenanceOperation.objects.exists())
        self.environment.refresh_from_db()
        self.assertTrue(self.environment.controller_released)
        self.assertEqual(self.environment.controller_epoch, 0)

    def test_native_completion_does_not_release_provider_provision_responsibility(self):
        grant = self._grant_with_coexisting_native_inventory()
        self.assertEqual(self.release(grant).status_code, 409)
        payload = grant['payload']
        container = 'b' * 64
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.environment_provision_completions VALUES('
                + ','.join(['%s'] * 13) + ')', [self.body['operationId'], self.environment.pk, self.session.pk,
                    self.workspace.pk, 'native-host', self.instance, container, payload['environmentProfileDigest'],
                    payload['controllerEpoch'], payload['controllerLeaseDigest'], payload['requestDigest'],
                    json.dumps({'kind': 'provisioned', 'instanceId': self.instance, 'containerId': container,
                        'profileDigest': payload['environmentProfileDigest']}), 1])
        response = self.release(grant)
        self.assertEqual(response.status_code, 409, response.content)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)
        self.assertEqual(EnvironmentMaintenanceOperation.objects.get(pk=self.body['operationId']).state, 'active')

    def test_matching_verified_provider_completion_releases_and_replays_without_a_run(self):
        grant = self._grant_with_coexisting_native_inventory()
        self.assertEqual(self.release(grant).status_code, 409)
        self._provider_receipt(grant)
        response = self.release(grant)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertTrue(response.json()['released'])
        self.assertEqual(self.release(grant).json(), response.json())
        self.environment.refresh_from_db()
        self.assertTrue(self.environment.controller_released)
        self.assertFalse(AgentRun.objects.exists())
        competing = self.issue(self.body | {'operationId': 'provider-provision-another',
            'expectedControllerEpoch': grant['payload']['controllerEpoch']})
        self.assertEqual(competing.status_code, 409, competing.content)
        self.assertEqual(competing.json(), {'error': 'provision_allocation_exists'})
        self.assertEqual(EnvironmentMaintenanceOperation.objects.count(), 1)

    def test_verified_target_does_not_accept_a_non_ascii_sandbox_identity(self):
        grant = self._grant_with_coexisting_native_inventory()
        self._provider_receipt(grant)
        with connection.cursor() as cursor:
            cursor.execute('UPDATE runtime.provider_creation_operations SET verified_sandbox_id=%s', ['sandbox_未知'])
            cursor.execute('UPDATE runtime.provider_environment_provision_completions SET sandbox_id=%s', ['sandbox_未知'])
        response = self.release(grant)
        self.assertEqual(response.status_code, 409, response.content)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)

    def test_preparation_completion_cannot_precede_the_same_target_verification(self):
        grant = self._grant_with_coexisting_native_inventory()
        self._provider_receipt(grant)
        with connection.cursor() as cursor:
            cursor.execute('UPDATE runtime.provider_environment_provision_completions SET recorded_at_ms=0')
        response = self.release(grant)
        self.assertEqual(response.status_code, 409, response.content)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)

    def test_completion_requires_the_exact_verified_attempt_owner_profile_target_and_fence(self):
        grant = self._grant_with_coexisting_native_inventory()
        self._provider_receipt(grant)
        wrong_digest = 'sha256:' + 'f' * 64
        changes = [
            ('provider_creation_operations', 'attempt_id', self.attempt, 'verified_sandbox_id', 'another-sandbox'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'verification_digest', None),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'verified_at_ms', None),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'creation_owner_json',
                json.dumps({'kind': 'agentRun', 'runId': 'another-run'})),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'creation_owner_json',
                json.dumps({'kind': 'maintenanceOperation', 'operationId': self.body['operationId'], 'runId': 'another-run'})),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'maintenance_operation_id', 'another-operation'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'agent_run_id', 'another-run'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'processing_job_id', 'another-job'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'authorization_digest', wrong_digest),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'profile_digest', wrong_digest),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'provider_config_digest', wrong_digest),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'instance_id', 'instance_' + 'f' * 64),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'environment_id', 'another-environment'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'session_id', 'another-session'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'workspace_id', 'another-workspace'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'provider', 'docker'),
            ('provider_creation_operations', 'attempt_id', self.attempt, 'dispatch_state', 'prepared'),
            ('provider_environment_bindings', 'environment_id', self.environment.pk, 'creation_attempt_id', 'another-attempt'),
        ]
        changes.extend(('provider_environment_provision_completions', 'operation_id', self.body['operationId'], field, value)
            for field, value in {
                'creation_attempt_id': 'another-attempt', 'environment_id': 'another-environment',
                'session_id': 'another-session', 'workspace_id': 'another-workspace', 'instance_id': 'instance_' + 'f' * 64,
                'sandbox_id': 'another-sandbox', 'profile_digest': wrong_digest, 'provider_config_digest': wrong_digest,
                'authorization_digest': wrong_digest, 'controller_epoch': grant['payload']['controllerEpoch'] + 1,
                'controller_lease_digest': wrong_digest, 'request_digest': wrong_digest,
                'preparation_digest': 'unconfirmed', 'recorded_at_ms': -1,
            }.items())
        for table, identity_field, identity, field, value in changes:
            with self.subTest(table=table, field=field):
                with connection.cursor() as cursor:
                    cursor.execute(f'SELECT {field} FROM runtime.{table} WHERE {identity_field}=%s', [identity])
                    previous = cursor.fetchone()[0]
                    cursor.execute(f'UPDATE runtime.{table} SET {field}=%s WHERE {identity_field}=%s', [value, identity])
                try:
                    response = self.release(grant)
                    self.assertEqual(response.status_code, 409, response.content)
                    self.environment.refresh_from_db()
                    self.assertFalse(self.environment.controller_released)
                finally:
                    with connection.cursor() as cursor:
                        cursor.execute(f'UPDATE runtime.{table} SET {field}=%s WHERE {identity_field}=%s', [previous, identity])
        self.assertEqual(self.release(grant).status_code, 200)
