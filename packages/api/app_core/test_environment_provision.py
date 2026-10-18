"""Independent provisioning has durable authority before an instance exists."""
import json
from unittest.mock import patch

from django.contrib.auth.models import User
from django.db import connection
from django.test import TransactionTestCase, override_settings

from .environment_profile import bind_environment_profile
from .models import AgentRun, EnvironmentMaintenanceOperation, Workspace
from .test_unified_environment_profile import profile
from .testing import create_session

TOKEN = 'provision-test-' + 'p' * 32


@override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)
class EnvironmentProvisionTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        user = User.objects.create_user(username='provision-owner')
        self.workspace = Workspace.objects.create(name='Provision', createdBy=user)
        self.session = create_session(workspace=self.workspace, owner=user)
        self.environment = self.session.current_environment
        self.bound = bind_environment_profile(self.environment.pk, {'schema': 'workspace.environment_profile.bind.v1',
            'operationId': 'profile', 'sessionId': self.session.pk, 'profile': profile()})
        self.url = f'/internal/environments/{self.environment.pk}/provision-authorizations'
        self.body = {'schema': 'workspace.environment_provision.issue.v1', 'operationId': 'provision-one',
            'sessionId': self.session.pk, 'expectedControllerEpoch': 0}
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA IF NOT EXISTS runtime')
            cursor.execute('CREATE TABLE runtime.execution_instances(environment_id text, allocation_execution_id text)')
            cursor.execute('CREATE TABLE runtime.environment_provision_completions(operation_id text PRIMARY KEY,'
                'environment_id text,session_id text,workspace_id text,host_id text,instance_id text,container_id text,'
                'profile_digest text,controller_epoch bigint,controller_lease_digest text,request_digest text,'
                'completion_json text,recorded_at_ms bigint)')
        self.addCleanup(self.drop_runtime)

    def drop_runtime(self):
        with connection.cursor() as cursor:
            cursor.execute('DROP TABLE runtime.environment_provision_completions')
            cursor.execute('DROP TABLE runtime.execution_instances')

    def issue(self, body=None):
        return self.client.post(self.url, json.dumps(self.body if body is None else body),
            content_type='application/json', HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)

    def release(self, grant):
        return self.client.post(f'/internal/environments/{self.environment.pk}/maintenance-operations/provision-one/release',
            json.dumps({'schema': 'workspace.environment_maintenance.release.v1',
                'controllerEpoch': grant['payload']['controllerEpoch'],
                'controllerLeaseDigest': grant['payload']['controllerLeaseDigest']}),
            content_type='application/json', HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)

    def test_provision_derives_the_owner_and_profile_without_a_run_or_invented_target(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        payload = grant['payload']
        self.assertEqual(payload['operationKind'], 'provision')
        self.assertEqual(payload['userId'], str(self.session.owner_id))
        self.assertEqual(payload['agentId'], self.session.agent_id)
        self.assertEqual(payload['environmentProfile'], profile())
        self.assertEqual(payload['environmentProfileDigest'], self.bound['profileDigest'])
        self.assertNotIn('instanceId', payload)
        self.assertNotIn('containerId', payload)
        self.assertFalse(AgentRun.objects.exists())
        operation = EnvironmentMaintenanceOperation.objects.get(pk='provision-one')
        self.assertIsNone(operation.instance_id)
        self.assertIsNone(operation.container_id)
        self.assertEqual(operation.request['kind'], 'provisionEnvironment')
        with patch('app_core.environment_maintenance._now_ms', return_value=payload['expiresAtMs'] + 1):
            self.assertEqual(self.issue().json(), grant)
        self.assertEqual(self.issue(self.body | {'operationId': 'competing'}).status_code, 409)
        self.assertEqual(self.issue(self.body | {'userId': 'caller'}).status_code, 400)

    def test_release_requires_an_exact_immutable_runtime_provision_receipt(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        self.assertEqual(self.release(grant).status_code, 409)
        payload = grant['payload']
        instance, container = 'instance_' + 'a' * 64, 'b' * 64
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.environment_provision_completions VALUES('
                + ','.join(['%s'] * 13) + ')', ['provision-one', self.environment.pk, self.session.pk,
                    self.workspace.pk, 'host-one', instance, container, payload['environmentProfileDigest'],
                    payload['controllerEpoch'], payload['controllerLeaseDigest'], payload['requestDigest'],
                    json.dumps({'kind': 'provisioned', 'instanceId': instance, 'containerId': container,
                        'profileDigest': payload['environmentProfileDigest']}), 1])
        released = self.release(grant)
        self.assertEqual(released.status_code, 200, released.content)
        self.assertTrue(released.json()['released'])
        self.assertEqual(self.release(grant).json(), released.json())
        self.environment.refresh_from_db()
        self.assertTrue(self.environment.controller_released)

    def test_definition_provision_uses_reviewed_published_activation_instead_of_workspace_defaults(self):
        from .models import Agent, AgentDefinition, AgentDefinitionVersion, WorkspaceMembership
        user = self.session.owner
        WorkspaceMembership.objects.create(workspace=self.workspace, user=user, role='owner')
        definition = AgentDefinition.objects.create(workspace=self.workspace, name='Provision agent', created_by=user,
            availability_scope='workspace')
        version = AgentDefinitionVersion.objects.create(definition=definition, version=1, name='Provision agent',
            published_by=user)
        definition.published_version = version
        definition.save(update_fields=['published_version'])
        Agent.objects.filter(pk=self.session.agent_id).update(definition=definition)
        with patch('app_core.environment_provision.plugin_activation_for_workspace',
                side_effect=AssertionError('mutable workspace defaults used')):
            response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()['payload']['pluginActivation'], version.plugin_activation)
