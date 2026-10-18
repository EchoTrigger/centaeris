"""Immutable, independently authorized benchmark environment binding."""
import copy
from contextlib import nullcontext
import json
from unittest.mock import patch

from django.conf import settings
from django.db import IntegrityError, connection, transaction
from django.test import SimpleTestCase, TestCase, override_settings

from .benchmark_profile import benchmark_profile_digest, validate_benchmark_profile
from .models import AgentRun, Session, SessionEnvironment
from .runtime_contract import authorization_digest, authorization_signature, validate_agent_run_authorization_payload
from .test_hosted_operations import OperationFixture

MAINTENANCE = 'test-maintenance-' + 'm' * 32


def profile():
    return {'schema': 'runtime.benchmark_execution_profile.v1',
            'imageDigest': 'sha256:' + 'b' * 64, 'workspaceRoot': '/app',
            'user': '0:0', 'network': 'publicInternet',
            'resources': {'cpuMilli': 1000, 'memoryBytes': 2 * 1024**3,
                          'pidsLimit': 512, 'dataTmpfsBytes': 64 * 1024**2,
                          'storageBytes': 10 * 1024**3}}


class BenchmarkProfileContractTests(SimpleTestCase):
    def test_shared_v2_fixture_digest_signature_and_explicit_null_rejection(self):
        from pathlib import Path
        from .runtime_contract import verify_agent_run_authorization_signature
        root = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/agent_run_authorization/v2'
        payload = json.loads((root / 'valid.json').read_text(encoding='utf-8'))
        expected = json.loads((root / 'expected.json').read_text(encoding='utf-8'))
        self.assertEqual(benchmark_profile_digest(payload['benchmarkProfile']), expected['profileDigest'])
        self.assertEqual(authorization_digest(payload), expected['digest'])
        self.assertEqual(authorization_signature(payload, 'test-key'), expected['signature'])
        verify_agent_run_authorization_signature(payload, 'test-key', expected['signature'])
        with self.assertRaises(ValueError):
            validate_agent_run_authorization_payload({**payload, 'benchmarkProfile': None})
        with self.assertRaises(ValueError):
            validate_agent_run_authorization_payload({**payload, 'benchmarkProfileDigest': None})

    def test_v2_cannot_authorize_assets_plugins_or_snapshot_rehydration(self):
        from pathlib import Path
        from .test_agent_run_authorization import fixture
        from .plugin_catalog import activation_digest
        payload = json.loads((Path(__file__).resolve().parents[3] /
            'tests/workspace/fixtures/agent_run_authorization/v2/valid.json').read_text(encoding='utf-8'))
        for field, value in (('assetRefs', fixture()['assetRefs']),
                             ('sessionWorkspace', fixture()['sessionWorkspace'])):
            invalid = copy.deepcopy(payload)
            invalid[field] = value
            with self.assertRaisesRegex(ValueError, 'combination_unsupported'):
                validate_agent_run_authorization_payload(invalid)
        package = {'name': 'synthetic', 'version': '1.0.0', 'packageDigest': 'sha256:' + 'a' * 64,
                   'skills': [], 'cli': [], 'mcpServers': [], 'hooks': []}
        payload['pluginActivation'] = {'schema': 'plugin_activation_snapshot_v1',
            'digest': activation_digest([package]), 'packages': [package]}
        with self.assertRaisesRegex(ValueError, 'combination_unsupported'):
            validate_agent_run_authorization_payload(payload)

    def test_exact_profile_and_digest(self):
        value = profile()
        validate_benchmark_profile(value)
        self.assertEqual(benchmark_profile_digest(value), benchmark_profile_digest(copy.deepcopy(value)))
        changed = copy.deepcopy(value)
        changed['workspaceRoot'] = '/other'
        self.assertNotEqual(benchmark_profile_digest(value), benchmark_profile_digest(changed))

    def test_unknown_fields_unsafe_paths_users_network_and_resources_fail(self):
        mutations = [lambda p: p.update(extra=True),
                     lambda p: p.update(schema='old'),
                     lambda p: p.update(workspaceRoot='relative'),
                     lambda p: p.update(workspaceRoot='/app/../root'),
                     lambda p: p.update(workspaceRoot='/app//nested'),
                     lambda p: p.update(workspaceRoot='/run/centaeris'),
                     lambda p: p.update(workspaceRoot='/app\\bad'),
                     lambda p: p.update(workspaceRoot='/app\x7f'),
                     lambda p: p.update(workspaceRoot='/app\x85'),
                     lambda p: p.update(workspaceRoot='/cafe\u0301'),
                     lambda p: p.update(user='root'),
                     lambda p: p.update(user='00:0'),
                     lambda p: p.update(network='bridge'),
                     lambda p: p['resources'].update(storageBytes=0),
                     lambda p: p['resources'].update(memoryBytes=True),
                     lambda p: p['resources'].update(cpuMilli=2**32),
                     lambda p: p['resources'].update(extra=1)]
        for mutate in mutations:
            value = profile()
            mutate(value)
            with self.subTest(value=value), self.assertRaises(ValueError):
                validate_benchmark_profile(value)


@override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=MAINTENANCE)
class BenchmarkProfileHttpTests(OperationFixture, TestCase):
    def setUp(self):
        super().setUp()
        if connection.vendor == 'sqlite':
            # SQLite exercises HTTP/binding semantics only; the catalog's PostgreSQL
            # advisory lock is independently covered by the real PG gate.
            for target in ('app_core.http.workspaces.plugin_lifecycle_lock',
                           'app_core.plugin_catalog.plugin_lifecycle_lock',
                           'app_core.agent_run_authorization_factory.plugin_lifecycle_lock'):
                lock = patch(target, side_effect=lambda: nullcontext())
                lock.start()
                self.addCleanup(lock.stop)
            admission = patch('app_core.http.workspaces.queued_admission_error', return_value=None)
            admission.start()
            self.addCleanup(admission.stop)
        self.session_id = self.create().json()['sessionId']
        self.session = Session.objects.get(pk=self.session_id)
        self.environment = self.session.current_environment
        self.url = f'/internal/environments/{self.environment.pk}/benchmark-profile'

    def bind(self, *, body=None, token=MAINTENANCE):
        payload = body if body is not None else {
            'schema': 'workspace.benchmark_profile.bind.v1', 'operationId': 'bind-profile',
            'sessionId': self.session_id, 'profile': profile()}
        headers = {} if token is None else {'HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN': token}
        return self.client.post(self.url, json.dumps(payload), content_type='application/json', **headers)

    def test_maintenance_authority_required_and_internal_token_cannot_bind(self):
        for token in (None, '', 'wrong', settings.INTERNAL_API_TOKEN):
            response = self.bind(token=token)
            self.assertEqual(response.status_code, 403, response.content)
        with override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=''):
            self.assertEqual(self.bind().status_code, 403)
        with override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=settings.INTERNAL_API_TOKEN):
            self.assertEqual(self.bind(token=settings.INTERNAL_API_TOKEN).status_code, 403)
        self.environment.refresh_from_db()
        self.assertIsNone(self.environment.benchmark_profile)

    def test_first_binding_and_lost_response_replay_are_identical(self):
        first = self.bind()
        self.assertEqual(first.status_code, 200, first.content)
        self.assertEqual(first.json(), {'schema': 'workspace.benchmark_profile.bound.v1',
            'operationId': 'bind-profile', 'sessionId': self.session_id,
            'environmentId': self.environment.pk, 'profile': profile(),
            'profileDigest': benchmark_profile_digest(profile())})
        self.assertEqual(self.bind().json(), first.json())
        self.environment.refresh_from_db()
        self.assertEqual(self.environment.benchmark_profile, profile())
        self.assertEqual(self.environment.benchmark_profile_operation_id, 'bind-profile')

    def test_rebinding_operation_conflict_and_foreign_session_are_rejected(self):
        self.assertEqual(self.bind().status_code, 200)
        body = {'schema': 'workspace.benchmark_profile.bind.v1', 'operationId': 'other',
                'sessionId': self.session_id, 'profile': profile()}
        self.assertEqual(self.bind(body=body).status_code, 409)
        body['operationId'] = 'bind-profile'
        body['profile']['workspaceRoot'] = '/other'
        self.assertEqual(self.bind(body=body).status_code, 409)
        body['sessionId'] = self.create('other-session').json()['sessionId']
        self.assertEqual(self.bind(body=body).status_code, 404)

    def test_strict_body_and_public_message_cannot_select_profile(self):
        for changes in ({'extra': 1}, {'operationId': 'bad id'}, {'schema': 'old'}):
            body = {'schema': 'workspace.benchmark_profile.bind.v1', 'operationId': 'bind-profile',
                    'sessionId': self.session_id, 'profile': profile(), **changes}
            self.assertEqual(self.bind(body=body).status_code, 400)
        response = self.post(self.create_url + f'/{self.session_id}/messages', benchmarkProfile=profile())
        self.assertEqual(response.status_code, 400, response.content)
        self.assertFalse(AgentRun.objects.exists())

    def test_before_any_run_required_but_exact_replay_remains_available(self):
        self.assertEqual(self.bind().status_code, 200)
        accepted = self.post(self.create_url + f'/{self.session_id}/messages')
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.assertEqual(self.bind().status_code, 200)
        other_session = self.create('other-session').json()['sessionId']
        accepted = self.post(self.create_url + f'/{other_session}/messages', operationId='other-run')
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.session_id = other_session
        self.environment = Session.objects.get(pk=other_session).current_environment
        self.url = f'/internal/environments/{self.environment.pk}/benchmark-profile'
        self.assertEqual(self.bind().status_code, 409)
        self.environment.refresh_from_db()
        self.assertIsNone(self.environment.benchmark_profile)

    def test_benchmark_signed_policy_binds_common_network_and_task_profile(self):
        self.assertEqual(self.bind().status_code, 200)
        accepted = self.post(self.create_url + f'/{self.session_id}/messages')
        self.assertEqual(accepted.status_code, 202, accepted.content)
        run = AgentRun.objects.get(pk=accepted.json()['agentRunId'])
        payload = run.authorization.payload
        self.assertEqual(payload['schema'], 'workspace.agent_run_authorization.v3')
        self.assertEqual(payload['network'], profile()['network'])
        self.assertEqual(payload['imageCapability'], 'benchmark_task_v1')
        self.assertEqual(payload['imageDigest'], profile()['imageDigest'])
        self.assertEqual(payload['benchmarkProfile'], profile())
        self.assertEqual(payload['benchmarkProfileDigest'], benchmark_profile_digest(profile()))
        self.assertEqual(payload['resources'], {k: v for k, v in profile()['resources'].items() if k != 'storageBytes'})
        validate_agent_run_authorization_payload(payload)
        self.assertEqual(authorization_digest(payload), run.authorization.digest)
        self.assertEqual(authorization_signature(payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY), run.authorization.signature)
        for field, value in [('imageDigest', 'sha256:' + 'c' * 64), ('benchmarkProfileDigest', 'sha256:' + 'd' * 64)]:
            altered = copy.deepcopy(payload)
            altered[field] = value
            with self.assertRaises(ValueError):
                validate_agent_run_authorization_payload(altered)
        ordinary = self.post(operationId='ordinary')
        self.assertEqual(ordinary.status_code, 202, ordinary.content)
        old = AgentRun.objects.get(pk=ordinary.json()['agentRunId']).authorization.payload
        self.assertEqual(old['schema'], 'workspace.agent_run_authorization.v3')
        self.assertEqual(old['network'], 'disabled')
        self.assertNotIn('benchmarkProfile', old)
        self.assertEqual(old['imageCapability'], 'workspace_general_v1')
        with self.assertRaises(ValueError):
            validate_agent_run_authorization_payload({**old, 'benchmarkProfile': profile()})

    def test_v3_signature_uses_separate_domain_and_covers_profile(self):
        import hashlib
        import hmac
        self.assertEqual(self.bind().status_code, 200)
        accepted = self.post(self.create_url + f'/{self.session_id}/messages')
        grant = AgentRun.objects.get(pk=accepted.json()['agentRunId']).authorization
        expected = 'hmac-sha256:' + hmac.new(settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY.encode(),
            ('workspace:agent-run-authorization:v3\0' + grant.digest).encode(), hashlib.sha256).hexdigest()
        self.assertEqual(grant.signature, expected)

    def test_bound_session_cannot_switch_to_a_new_environment(self):
        self.assertEqual(self.bind().status_code, 200)
        other = SessionEnvironment.objects.create(session=self.session)
        self.session.current_environment = other
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.session.save(update_fields=['current_environment'])
        if connection.vendor == 'postgresql':
            with self.assertRaises(IntegrityError), transaction.atomic():
                Session.objects.filter(pk=self.session_id).update(current_environment=other)

    def test_new_environment_cannot_insert_an_already_bound_profile(self):
        from django.utils import timezone
        with self.assertRaisesRegex(ValueError, 'binding'):
            SessionEnvironment.objects.create(session=self.session, benchmark_profile=profile(),
                benchmark_profile_digest=benchmark_profile_digest(profile()),
                benchmark_profile_operation_id='raw-profile', benchmark_profile_bound_at=timezone.now())

    def test_model_save_cannot_replace_or_clear_bound_profile(self):
        self.assertEqual(self.bind().status_code, 200)
        self.environment.refresh_from_db()
        self.environment.benchmark_profile = None
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.environment.save(update_fields=['benchmark_profile'])

    def setup_complete(self, **changes):
        body = {'schema': 'workspace.benchmark_setup.complete.v1', 'operationId': 'setup-complete',
                'sessionId': self.session_id, 'agentRunId': 'run_missing',
                'instanceId': 'instance_' + '1' * 64, 'containerId': '2' * 64, **changes}
        return self.client.post(self.url.replace('/benchmark-profile', '/benchmark-setup-complete'),
            json.dumps(body), content_type='application/json', HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN=MAINTENANCE)

    def test_setup_uses_independent_maintenance_auth_before_parsing(self):
        url = self.url.replace('/benchmark-profile', '/benchmark-setup-complete')
        for headers in ({}, {'HTTP_X_INTERNAL_TOKEN': settings.INTERNAL_API_TOKEN},
                        {'HTTP_X_ENVIRONMENT_MAINTENANCE_TOKEN': settings.INTERNAL_API_TOKEN}):
            response = self.client.post(url, '{invalid-json', content_type='application/json', **headers)
            self.assertEqual(response.status_code, 403, response.content)
        self.assertEqual(self.setup_complete(extra=1).status_code, 400)

    def test_setup_without_bound_profile_or_matching_first_run_is_rejected(self):
        self.assertEqual(self.setup_complete().status_code, 409)
        self.assertEqual(self.bind().status_code, 200)
        self.assertEqual(self.setup_complete().status_code, 409)
        self.environment.refresh_from_db()
        self.assertEqual(self.environment.benchmark_setup_operation_id, '')

    def test_setup_complete_one_way_identity_and_exact_replay_after_terminal(self):
        self.assertEqual(self.bind().status_code, 200)
        accepted = self.post(self.create_url + f'/{self.session_id}/messages')
        self.assertEqual(accepted.status_code, 202, accepted.content)
        run_id = accepted.json()['agentRunId']
        first = self.setup_complete(agentRunId=run_id)
        self.assertEqual(first.status_code, 200, first.content)
        self.assertEqual(first.json()['instanceId'], 'instance_' + '1' * 64)
        self.assertEqual(self.setup_complete(agentRunId=run_id).json(), first.json())
        self.assertEqual(self.setup_complete(agentRunId=run_id, containerId='3' * 64).status_code, 409)
        AgentRun.objects.filter(pk=run_id).update(status='completed')
        self.assertEqual(self.setup_complete(agentRunId=run_id).json(), first.json())
        second = self.post(self.create_url + f'/{self.session_id}/messages', operationId='second')
        self.assertEqual(second.status_code, 409, second.content)
        self.assertEqual(second.json()['error'], 'benchmark_environment_single_run_required')
        self.assertEqual(AgentRun.objects.filter(session_id=self.session_id).count(), 1)
        self.environment.refresh_from_db()
        self.environment.benchmark_setup_container_id = '4' * 64
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.environment.save(update_fields=['benchmark_setup_container_id'])

    def test_postgres_bulk_and_raw_profile_mutations_are_rejected(self):
        if connection.vendor != 'postgresql':
            self.skipTest('PostgreSQL trigger enforcement requires isolated PostgreSQL')
        self.assertEqual(self.bind().status_code, 200)
        for mutation in ('bulk', 'raw'):
            with self.subTest(mutation=mutation), self.assertRaises(IntegrityError), transaction.atomic():
                if mutation == 'bulk':
                    SessionEnvironment.objects.filter(pk=self.environment.pk).update(benchmark_profile=None)
                else:
                    with connection.cursor() as cursor:
                        cursor.execute('UPDATE app_core_sessionenvironment SET benchmark_profile=NULL WHERE id=%s', [self.environment.pk])
