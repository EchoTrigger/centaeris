"""Provider maintenance keeps verified instance identity and ordinary controller fences."""
import copy
import hashlib
import base64
import json
from unittest.mock import patch

from django.conf import settings
from django.contrib.auth.models import User
from django.db import connection
from django.test import TransactionTestCase, override_settings

from .environment_profile import bind_environment_profile
from .models import AgentRun, EnvironmentMaintenanceGrant, EnvironmentMaintenanceOperation, Workspace
from .provider_environment_maintenance_authorization import SCHEMA, sign_provider_maintenance_authorization
from .runtime_schema_test_helpers import isolate_runtime_schema
from .test_e2b_environment_profile import profile
from .testing import create_session

TOKEN = 'provider-maintenance-test-' + 't' * 32


@override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)
class ProviderEnvironmentMaintenanceTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        isolate_runtime_schema(self)
        self.user = User.objects.create_user(username='provider-maintenance-owner')
        self.workspace = Workspace.objects.create(name='Provider maintenance', createdBy=self.user)
        self.workspace.members.add(self.user)
        self.session = create_session(workspace=self.workspace, owner=self.user)
        self.environment = self.session.current_environment
        self.bound = bind_environment_profile(self.environment.pk, {'schema': 'workspace.environment_profile.bind.v1',
            'operationId': 'profile', 'sessionId': self.session.pk, 'profile': profile() | {'lifetime': 'retained'}})
        self.attempt, self.instance, self.sandbox = 'provider_attempt_test', 'instance_' + 'a' * 64, 'sandbox-provider-test'
        self.target = {'provider': 'e2b', 'creationAttemptId': self.attempt, 'instanceId': self.instance,
            'sandboxId': self.sandbox, 'verificationDigest': 'sha256:' + 'd' * 64,
            'providerConfigDigest': self.bound['profile']['providerConfigDigest']}
        self.body = {'schema': 'workspace.environment_maintenance.issue.v1', 'operationId': 'prepare',
            'controllerPurpose': 'execute', 'expectedControllerEpoch': 1,
            'request': {'program': '/bin/true', 'args': [], 'cwd': '/app', 'timeoutMs': 1000}}
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA runtime')
            cursor.execute('CREATE TABLE runtime.runtime_events(event_id text PRIMARY KEY,session_id text,task_id text,event_type text,at_ms bigint,visibility text,payload_json text)')
            cursor.execute('CREATE TABLE runtime.provider_creation_operations('
                'attempt_id text PRIMARY KEY,execution_id text,instance_id text,environment_id text,session_id text,'
                'workspace_id text,agent_run_id text,processing_job_id text,authorization_digest text,profile_digest text,'
                'provider_config_digest text,provider text,dispatch_state text,creation_owner_json text,'
                'maintenance_operation_id text,verified_sandbox_id text,verification_digest text,verified_at_ms bigint,settlement_id text)')
            cursor.execute('CREATE TABLE runtime.provider_environment_bindings('
                'environment_id text PRIMARY KEY,creation_attempt_id text,recorded_at_ms bigint)')
            cursor.execute('CREATE TABLE runtime.provider_environment_provision_completions('
                'operation_id text PRIMARY KEY,creation_attempt_id text,environment_id text,session_id text,workspace_id text,'
                'instance_id text,sandbox_id text,profile_digest text,provider_config_digest text,authorization_digest text,'
                'controller_epoch bigint,controller_lease_digest text,request_digest text,preparation_digest text,recorded_at_ms bigint)')
            cursor.execute('CREATE TABLE runtime.provider_environment_retirements(environment_id text PRIMARY KEY)')
            cursor.execute('CREATE TABLE runtime.provider_environment_power_operations('
                'environment_id text,operation_sequence bigint,action text,ack_json text)')
            cursor.execute("CREATE VIEW runtime.provider_environment_power_admission AS SELECT DISTINCT ON(environment_id) "
                "environment_id,(action<>'resume' OR ack_json IS NULL) AS blocked FROM runtime.provider_environment_power_operations "
                "ORDER BY environment_id,operation_sequence DESC")
            cursor.execute('CREATE TABLE runtime.provider_environment_maintenance_completions('
                'operation_id text PRIMARY KEY,environment_id text,creation_attempt_id text,instance_id text,sandbox_id text,'
                'profile_digest text,provider_config_digest text,verification_digest text,controller_epoch bigint,'
                'controller_lease_digest text,request_digest text,completion_json text,recorded_at_ms bigint)')
        response = self.post('provision-authorizations', {'schema': 'workspace.environment_provision.issue.v1',
            'operationId': 'origin', 'sessionId': self.session.pk, 'expectedControllerEpoch': 0})
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        payload = grant['payload']
        origin = EnvironmentMaintenanceOperation.objects.get(pk='origin')
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.provider_creation_operations VALUES(' + ','.join(['%s'] * 19) + ')',
                [self.attempt, 'execution-provider-test', self.instance, self.environment.pk, self.session.pk,
                 self.workspace.pk, None, None, origin.current_grant.digest, self.bound['profileDigest'],
                 self.target['providerConfigDigest'], 'e2b', 'dispatched',
                 json.dumps({'kind': 'maintenanceOperation', 'operationId': 'origin'}), 'origin',
                 self.sandbox, self.target['verificationDigest'], 1, None])
            cursor.execute('INSERT INTO runtime.provider_environment_bindings VALUES(%s,%s,1)',
                [self.environment.pk, self.attempt])
            cursor.execute('INSERT INTO runtime.provider_environment_provision_completions VALUES(' + ','.join(['%s'] * 15) + ')',
                ['origin', self.attempt, self.environment.pk, self.session.pk, self.workspace.pk,
                 self.instance, self.sandbox, self.bound['profileDigest'], self.target['providerConfigDigest'],
                 origin.current_grant.digest, payload['controllerEpoch'], payload['controllerLeaseDigest'],
                 payload['requestDigest'], 'sha256:' + 'e' * 64, 1])
        released = self.release(grant, operation='origin')
        self.assertEqual(released.status_code, 200, released.content)

    def post(self, endpoint, body):
        return self.client.post(f'/internal/environments/{self.environment.pk}/{endpoint}', json.dumps(body),
            content_type='application/json', HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN=TOKEN)

    def issue(self, body=None):
        return self.post('maintenance-authorizations', self.body if body is None else body)

    def release(self, grant, operation='prepare'):
        return self.post(f'maintenance-operations/{operation}/release', {
            'schema': 'workspace.environment_maintenance.release.v1',
            'controllerEpoch': grant['payload']['controllerEpoch'],
            'controllerLeaseDigest': grant['payload']['controllerLeaseDigest']})

    def complete(self, grant, **changes):
        payload = grant['payload']
        row = {'operation_id': 'prepare', 'environment_id': self.environment.pk, 'creation_attempt_id': self.attempt,
            'instance_id': self.instance, 'sandbox_id': self.sandbox, 'profile_digest': self.bound['profileDigest'],
            'provider_config_digest': self.target['providerConfigDigest'], 'verification_digest': self.target['verificationDigest'],
            'controller_epoch': payload['controllerEpoch'], 'controller_lease_digest': payload['controllerLeaseDigest'],
            'request_digest': payload['requestDigest'],
            'completion_json': json.dumps({'kind': 'operationEnded', 'taskIds': ['task-one'], 'cleanupComplete': True}),
            'recorded_at_ms': 1, **changes}
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.provider_environment_maintenance_completions VALUES('
                + ','.join(['%s'] * len(row)) + ')', list(row.values()))

    def test_issuer_uses_verified_provider_target_without_native_inventory_and_replays(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        payload = grant['payload']
        self.assertEqual(payload['schema'], SCHEMA)
        self.assertEqual(payload['providerTarget'], self.target)
        self.assertEqual(payload['profileDigest'], self.bound['profileDigest'])
        self.assertEqual(payload['controllerOwner'], {'kind': 'maintenanceOperation', 'operationId': 'prepare'})
        self.assertTrue({'instanceId', 'containerId', 'hostId', 'agentRunId'}.isdisjoint(payload))
        self.assertEqual(grant['signature'],
            sign_provider_maintenance_authorization(payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY))
        self.assertEqual(self.issue().json(), grant)
        operation = EnvironmentMaintenanceOperation.objects.get(pk='prepare')
        self.assertEqual(operation.provider_target, self.target)
        self.assertTrue(all(getattr(operation, name) is None for name in
            ('instance_id', 'container_id', 'host_id', 'allocation_execution_id')))
        self.assertFalse(AgentRun.objects.exists())
        self.assertEqual(self.issue(self.body | {'operationId': 'competing'}).status_code, 409)
        changed = copy.deepcopy(self.body)
        changed['request']['program'] = '/bin/false'
        self.assertEqual(self.issue(changed).status_code, 409)
        self.assertEqual(self.issue(self.body | {'providerTarget': self.target}).status_code, 400)

    def test_unverified_mismatched_or_unreleased_creation_never_acquires_controller(self):
        changes = {'verified_sandbox_id': None, 'verification_digest': None, 'verified_at_ms': None,
            'provider_config_digest': 'sha256:' + 'f' * 64, 'profile_digest': 'sha256:' + 'f' * 64,
            'environment_id': 'another-environment', 'session_id': 'another-session',
            'workspace_id': 'another-workspace', 'provider': 'docker', 'dispatch_state': 'prepared',
            'settlement_id': 'settled', 'creation_owner_json': json.dumps({'kind': 'maintenanceOperation', 'operationId': 'other'}),
            'authorization_digest': 'sha256:' + 'f' * 64}
        for field, value in changes.items():
            with self.subTest(field=field):
                with connection.cursor() as cursor:
                    cursor.execute(f'SELECT {field} FROM runtime.provider_creation_operations')
                    original = cursor.fetchone()[0]
                    cursor.execute(f'UPDATE runtime.provider_creation_operations SET {field}=%s', [value])
                try:
                    self.assertEqual(self.issue().status_code, 409)
                finally:
                    with connection.cursor() as cursor:
                        cursor.execute(f'UPDATE runtime.provider_creation_operations SET {field}=%s', [original])
        with connection.cursor() as cursor:
            cursor.execute("UPDATE app_core_environmentmaintenanceoperation SET state='active',released_at=NULL WHERE operation_id='origin'")
        self.assertEqual(self.issue().status_code, 409)
        self.assertFalse(EnvironmentMaintenanceOperation.objects.filter(pk='prepare').exists())
        self.environment.refresh_from_db()
        self.assertTrue(self.environment.controller_released)
        self.assertEqual(self.environment.controller_epoch, 1)

    def test_retirement_and_incomplete_power_transitions_block_new_maintenance(self):
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.provider_environment_retirements VALUES(%s)', [self.environment.pk])
        self.assertEqual(self.issue().status_code, 409)
        with connection.cursor() as cursor:
            cursor.execute('DELETE FROM runtime.provider_environment_retirements')
            cursor.execute("INSERT INTO runtime.provider_environment_power_operations VALUES(%s,1,'pause','{}')", [self.environment.pk])
        self.assertEqual(self.issue().status_code, 409)
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.provider_environment_power_operations VALUES(%s,2,'resume',NULL)", [self.environment.pk])
        self.assertEqual(self.issue().status_code, 409)
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.provider_environment_power_operations SET ack_json='{}' WHERE operation_sequence=2")
        self.assertEqual(self.issue().status_code, 200)

    def test_exact_provider_completion_releases_and_replays_after_the_next_owner(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        self.assertEqual(self.release(grant).status_code, 409)
        self.complete(grant)
        released = self.release(grant)
        self.assertEqual(released.status_code, 200, released.content)
        self.assertTrue(released.json()['released'])
        self.assertEqual(self.issue(self.body | {'operationId': 'next', 'expectedControllerEpoch': 2}).status_code, 200)
        self.assertEqual(self.release(grant).json(), released.json())

    def test_native_completion_cannot_release_provider_task_responsibility(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        payload = grant['payload']
        with connection.cursor() as cursor:
            cursor.execute('CREATE TABLE runtime.environment_maintenance_completions('
                'operation_id text,environment_id text,instance_id text,container_id text,profile_digest text,'
                'controller_epoch bigint,controller_lease_digest text,request_digest text,completion_json text)')
            cursor.execute('INSERT INTO runtime.environment_maintenance_completions VALUES(' + ','.join(['%s'] * 9) + ')',
                ['prepare', self.environment.pk, None, None, self.bound['profileDigest'], payload['controllerEpoch'],
                 payload['controllerLeaseDigest'], payload['requestDigest'],
                 json.dumps({'kind': 'operationEnded', 'taskIds': [], 'cleanupComplete': True})])
        self.assertEqual(self.release(grant).status_code, 409)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)

    def test_completion_must_match_exact_target_fence_and_finished_cleanup(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        changes = {'environment_id': 'other', 'creation_attempt_id': 'other', 'instance_id': 'instance_' + 'f' * 64,
            'sandbox_id': 'other', 'profile_digest': 'sha256:' + 'f' * 64,
            'provider_config_digest': 'sha256:' + 'f' * 64, 'verification_digest': 'sha256:' + 'f' * 64,
            'controller_epoch': 3, 'controller_lease_digest': 'sha256:' + 'f' * 64,
            'request_digest': 'sha256:' + 'f' * 64, 'recorded_at_ms': 0,
            'completion_json': json.dumps({'kind': 'operationEnded', 'taskIds': [], 'cleanupComplete': False})}
        for field, value in changes.items():
            with self.subTest(field=field):
                self.complete(grant, **{field: value})
                try:
                    self.assertEqual(self.release(grant).status_code, 409)
                    with patch('app_core.environment_maintenance._now_ms', return_value=grant['payload']['expiresAtMs'] + 1):
                        cleanup = self.issue(self.body | {'controllerPurpose': 'cleanup', 'expectedControllerEpoch': 2})
                    self.assertEqual(cleanup.status_code, 409)
                    self.assertEqual(cleanup.json()['error'], 'maintenance_completion_conflict')
                finally:
                    with connection.cursor() as cursor:
                        cursor.execute('DELETE FROM runtime.provider_environment_maintenance_completions')
        self.complete(grant)
        self.assertEqual(self.release(grant).status_code, 200)

    def test_expired_execute_replay_never_restarts_and_cleanup_recovers_the_same_target(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        original = response.json()
        with patch('app_core.environment_maintenance._now_ms', return_value=original['payload']['expiresAtMs'] + 1):
            self.assertEqual(self.issue().json(), original)
            cleanup = self.issue(self.body | {'controllerPurpose': 'cleanup', 'expectedControllerEpoch': 2})
        self.assertEqual(cleanup.status_code, 200, cleanup.content)
        current = cleanup.json()
        self.assertEqual(current['payload']['providerTarget'], original['payload']['providerTarget'])
        self.assertEqual(current['payload']['controllerPurpose'], 'cleanup')
        self.assertEqual(current['payload']['controllerEpoch'], 3)
        self.assertEqual(self.issue(self.body | {'controllerPurpose': 'cleanup', 'expectedControllerEpoch': 2}).json(), current)
        self.assertEqual(EnvironmentMaintenanceGrant.objects.filter(operation_id='prepare').count(), 2)
        self.complete(current)
        self.assertEqual(self.release(current).status_code, 200)
        self.assertEqual(self.issue().json(), original)


    def test_released_real_run_origin_can_handoff_to_independent_provider_maintenance(self):
        from .agent_run_authorization_factory import create_agent_run_authorization
        from .environment_lifecycle import acquire_environment_controller, acquire_environment_cleanup_controller, release_environment_controller
        from .models import ModelConfig
        self.session = create_session(workspace=self.workspace, owner=self.user)
        self.environment = self.session.current_environment
        self.bound = bind_environment_profile(self.environment.pk, {'schema': 'workspace.environment_profile.bind.v1',
            'operationId': 'run-profile', 'sessionId': self.session.pk, 'profile': profile() | {'lifetime': 'retained'}})
        model = ModelConfig.objects.create(displayName='Provider fixture model')
        run = AgentRun.objects.create(workspace=self.workspace, session=self.session, user=self.user,
            modelConfig=model, prompt='run-origin', status='running')
        authorization = create_agent_run_authorization(run, execution_profile={
            'schema': 'runtime.execution_profile.v2', 'environmentProfile': self.bound['profile']})
        job, owner = f'agent_run.lifecycle:{run.pk}', 'worker:provider-maintenance-run'
        with connection.cursor() as cursor:
            cursor.execute('CREATE TABLE runtime.runtime_jobs(job_id text PRIMARY KEY,job_kind text,status text,'
                'lease_purpose text,lease_owner text,lease_expires_at_ms bigint,idempotency_key text,session_id text,payload_ref text)')
            cursor.execute("INSERT INTO runtime.runtime_jobs VALUES(%s,'agent_run.lifecycle','running','execute',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                [job, owner, f'{job}:{authorization.digest}', self.session.pk, f'record:agent_run:{run.pk}'])
        acquired = acquire_environment_controller(run.pk, owner)
        self.attempt, self.instance, self.sandbox = 'provider_run_attempt', 'instance_' + 'b' * 64, 'sandbox-provider-run'
        self.target = self.target | {'creationAttemptId': self.attempt, 'instanceId': self.instance, 'sandboxId': self.sandbox}
        with connection.cursor() as cursor:
            cursor.execute('INSERT INTO runtime.provider_creation_operations VALUES(' + ','.join(['%s'] * 19) + ')',
                [self.attempt, 'execution-provider-run', self.instance, self.environment.pk, self.session.pk,
                 self.workspace.pk, run.pk, job, authorization.digest, self.bound['profileDigest'],
                 self.target['providerConfigDigest'], 'e2b', 'dispatched',
                 json.dumps({'kind': 'agentRun', 'runId': run.pk}), None,
                 self.sandbox, self.target['verificationDigest'], 1, None])
            cursor.execute('INSERT INTO runtime.provider_environment_bindings VALUES(%s,%s,1)',
                [self.environment.pk, self.attempt])
        self.assertEqual(self.issue(self.body | {'expectedControllerEpoch': acquired['controllerEpoch']}).status_code, 409)
        AgentRun.objects.filter(pk=run.pk).update(status='completed')
        cleanup = acquire_environment_cleanup_controller(run.pk, owner)
        release_environment_controller(run.pk, cleanup['controllerEpoch'], owner)
        request = self.body | {'expectedControllerEpoch': cleanup['controllerEpoch']}
        for field, value in [('processing_job_id', ''), ('authorization_digest', 'sha256:' + 'f' * 64)]:
            with self.subTest(field=field):
                with connection.cursor() as cursor:
                    cursor.execute(f'SELECT {field} FROM runtime.provider_creation_operations WHERE attempt_id=%s', [self.attempt])
                    previous = cursor.fetchone()[0]
                    cursor.execute(f'UPDATE runtime.provider_creation_operations SET {field}=%s WHERE attempt_id=%s',
                        [value, self.attempt])
                try:
                    self.assertEqual(self.issue(request).status_code, 409)
                finally:
                    with connection.cursor() as cursor:
                        cursor.execute(f'UPDATE runtime.provider_creation_operations SET {field}=%s WHERE attempt_id=%s',
                            [previous, self.attempt])
        response = self.issue(request)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()['payload']['providerTarget'], self.target)
        self.assertEqual(response.json()['payload']['controllerEpoch'], cleanup['controllerEpoch'] + 1)

    def test_concurrent_provider_tasks_have_only_one_controller_and_one_grant(self):
        from concurrent.futures import ThreadPoolExecutor
        from threading import Barrier
        from django.db import close_old_connections
        from .environment_maintenance import issue_maintenance_authorization
        barrier = Barrier(2)
        def acquire(operation):
            close_old_connections()
            try:
                barrier.wait(timeout=5)
                try:
                    return issue_maintenance_authorization(self.environment.pk, self.body | {'operationId': operation})
                except ValueError:
                    return None
            finally:
                connection.close()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(acquire, ['task-one', 'task-two']))
        successful = [result for result in results if result is not None]
        self.assertEqual(len(successful), 1)
        self.assertEqual(EnvironmentMaintenanceOperation.objects.filter(operation_kind='task').count(), 1)
        self.assertEqual(EnvironmentMaintenanceGrant.objects.filter(operation__operation_kind='task').count(), 1)
        self.assertEqual(successful[0]['payload']['providerTarget'], self.target)


    def test_signed_grant_for_another_provider_target_cannot_release_this_operation(self):
        from .environment_maintenance import _digest
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        self.complete(grant)
        stored = EnvironmentMaintenanceOperation.objects.get(pk='prepare').current_grant
        changed = copy.deepcopy(grant['payload'])
        changed['providerTarget']['sandboxId'] = 'another-verified-sandbox'
        signature = sign_provider_maintenance_authorization(changed, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY)
        with connection.cursor() as cursor:
            cursor.execute('UPDATE app_core_environmentmaintenancegrant SET payload=%s,digest=%s,signature=%s WHERE id=%s',
                [json.dumps(changed), _digest(changed), signature, stored.pk])
        response = self.release(grant)
        self.assertEqual(response.status_code, 409, response.content)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)

    def test_live_verified_target_must_still_match_the_frozen_task_at_release(self):
        response = self.issue()
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        self.complete(grant)
        with connection.cursor() as cursor:
            cursor.execute('UPDATE runtime.provider_creation_operations SET verified_sandbox_id=%s WHERE attempt_id=%s',
                ['another-verified-sandbox', self.attempt])
        self.assertEqual(self.release(grant).status_code, 409)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)
        with connection.cursor() as cursor:
            cursor.execute('UPDATE runtime.provider_creation_operations SET verified_sandbox_id=%s WHERE attempt_id=%s',
                [self.sandbox, self.attempt])
        self.assertEqual(self.release(grant).status_code, 200)


    def test_direct_completion_requires_exact_durable_dispatch_and_full_result_events(self):
        from .provider_environment_maintenance_completion import SCHEMA as COMPLETE_SCHEMA, DISPATCH_SCHEMA, dispatch_record
        grant=self.issue().json()
        payload=grant['payload']
        expected={key:payload[key] for key in ('environmentId','providerTarget','profileDigest','requestDigest','controllerEpoch','controllerLeaseDigest')}
        expected['operationId']='prepare'
        expected['authorizationDigest']='sha256:'+hashlib.sha256(json.dumps(payload,ensure_ascii=False,sort_keys=True,separators=(',',':')).encode()).hexdigest()
        data=b'\x00\xffverifier output'
        completion={'schema':COMPLETE_SCHEMA,**expected,'output':{'kind':'command','exitCode':0,
            'stdoutBytesBase64':base64.b64encode(data).decode(),'stdoutByteLength':len(data),
            'stderrBytesBase64':'','stderrByteLength':0}}
        self.assertEqual(self.release(grant).status_code,409)
        def event(name, schema, at_ms, value):
            with connection.cursor() as cursor:
                cursor.execute('INSERT INTO runtime.runtime_events VALUES(%s,%s,NULL,%s,%s,%s,%s)',
                    [name+':prepare',self.session.pk,schema,at_ms,'internal',json.dumps(value)])
        event('provider_maintenance_dispatched',DISPATCH_SCHEMA,2,dispatch_record(completion))
        self.assertEqual(self.release(grant).status_code,409)
        event('provider_maintenance_completed',COMPLETE_SCHEMA,3,completion|{'requestDigest':'sha256:'+'f'*64})
        self.assertEqual(self.release(grant).status_code,409)
        with connection.cursor() as cursor:
            cursor.execute('UPDATE runtime.runtime_events SET payload_json=%s WHERE event_id=%s',
                [json.dumps(completion),'provider_maintenance_completed:prepare'])
        released=self.release(grant)
        self.assertEqual(released.status_code,200,released.content)
        self.assertEqual(self.release(grant).json(),released.json())

    def _record_direct_events(self, grant, output):
        from .provider_environment_maintenance_completion import SCHEMA as COMPLETE_SCHEMA, DISPATCH_SCHEMA, dispatch_record
        payload = grant['payload']
        expected = {key: payload[key] for key in ('environmentId', 'providerTarget', 'profileDigest',
            'requestDigest', 'controllerEpoch', 'controllerLeaseDigest')}
        operation = payload['controllerOwner']['operationId']
        expected.update(operationId=operation, authorizationDigest='sha256:' + hashlib.sha256(
            json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()).hexdigest())
        completion = {'schema': COMPLETE_SCHEMA, **expected, 'output': output}
        with connection.cursor() as cursor:
            for prefix, schema, at_ms, value in (
                    ('provider_maintenance_dispatched:', DISPATCH_SCHEMA, 2, dispatch_record(completion)),
                    ('provider_maintenance_completed:', COMPLETE_SCHEMA, 3, completion)):
                cursor.execute('INSERT INTO runtime.runtime_events VALUES(%s,%s,NULL,%s,%s,%s,%s)',
                    [prefix + operation, self.session.pk, schema, at_ms, 'internal', json.dumps(value)])

    def test_confirmed_read_failure_releases_controller_and_allows_the_next_operation(self):
        from pathlib import Path
        fixture = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/provider_environment_maintenance_completion/v1/file-not-found.json'
        failure = json.loads(fixture.read_text())
        for index, request in enumerate(({'kind': 'readFile', 'path': '/logs/missing', 'maxBytes': 1024},
                                        failure['request']['request'])):
            with self.subTest(kind=request['kind']):
                self.environment.refresh_from_db()
                operation = 'missing-' + str(index)
                body = self.body | {'operationId': operation, 'expectedControllerEpoch': self.environment.controller_epoch,
                    'request': {'request': request, 'contentLength': 0,
                        'contentDigest': 'sha256:' + hashlib.sha256(b'').hexdigest()}}
                response = self.issue(body)
                self.assertEqual(response.status_code, 200, response.content)
                grant = response.json()
                self.assertEqual(self.release(grant, operation).status_code, 409)
                self._record_direct_events(grant, failure['output'])
                released = self.release(grant, operation)
                self.assertEqual(released.status_code, 200, released.content)
                self.assertEqual(self.release(grant, operation).json(), released.json())
                self.environment.refresh_from_db()
                self.assertTrue(self.environment.controller_released)
                self.assertEqual(EnvironmentMaintenanceOperation.objects.get(pk=operation).state, 'released')

    def test_file_failure_cannot_release_a_command(self):
        grant = self.issue().json()
        self._record_direct_events(grant, {'kind': 'filesystemFailed', 'code': 'fileNotFound'})
        self.assertEqual(self.release(grant).status_code, 409)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)

    def test_file_failure_cannot_release_a_write(self):
        body = self.body | {'request': {'request': {'kind': 'writeFile', 'path': '/app/file'},
            'contentLength': 0, 'contentDigest': 'sha256:' + hashlib.sha256(b'').hexdigest()}}
        response = self.issue(body)
        self.assertEqual(response.status_code, 200, response.content)
        grant = response.json()
        self._record_direct_events(grant, {'kind': 'filesystemFailed', 'code': 'fileNotFound'})
        self.assertEqual(self.release(grant).status_code, 409)
        self.environment.refresh_from_db()
        self.assertFalse(self.environment.controller_released)
