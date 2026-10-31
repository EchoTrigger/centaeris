"""Immutable, independently authorized benchmark environment binding."""
import copy
from contextlib import nullcontext
import json
from unittest.mock import patch

from django.conf import settings
from django.db import IntegrityError, connection, transaction
from django.test import SimpleTestCase, TestCase, override_settings

from .environment_profile import environment_profile_digest, validate_environment_profile
from .models import AgentRun, Session, SessionEnvironment
from .runtime_contract import authorization_digest, authorization_signature, validate_agent_run_authorization_payload
from .test_hosted_operations import OperationFixture

MAINTENANCE = 'test-maintenance-' + 'm' * 32


def profile():
    return {'schema': 'runtime.environment_profile.v1',
            'imageDigest': 'sha256:' + 'b' * 64, 'workspaceRoot': '/app',
            'user': '0:0', 'network': 'publicInternet',
            'resources': {'cpuMilli': 1000, 'memoryBytes': 2 * 1024**3,
                          'pidsLimit': 512, 'workspaceBytes': 64 * 1024**2,
                          'storageBytes': 10 * 1024**3}}


class EnvironmentProfileContractTests(SimpleTestCase):
    def test_one_generic_profile_supports_root_and_regular_user_without_capability_fields(self):
        from pathlib import Path
        root = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/agent_run_authorization/v1'
        for name, expected_name in (("valid", "expected"), ("custom", "custom-expected")):
            value = json.loads((root / (name + '.json')).read_text(encoding='utf-8'))
            expected = json.loads((root / (expected_name + '.json')).read_text(encoding='utf-8'))
            self.assertEqual(environment_profile_digest(value['environmentProfile']), expected['profileDigest'])
            self.assertEqual(authorization_digest(value), expected['digest'])
            self.assertEqual(authorization_signature(value, 'test-key'), expected['signature'])

    def test_exact_profile_and_digest(self):
        value = profile()
        validate_environment_profile(value)
        self.assertEqual(environment_profile_digest(value), environment_profile_digest(copy.deepcopy(value)))
        changed = copy.deepcopy(value)
        changed['workspaceRoot'] = '/other'
        self.assertNotEqual(environment_profile_digest(value), environment_profile_digest(changed))

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
                validate_environment_profile(value)


@override_settings(ENVIRONMENT_MAINTENANCE_TOKEN=MAINTENANCE)
class EnvironmentProfileHttpTests(OperationFixture, TestCase):
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
        self.url = f'/internal/environments/{self.environment.pk}/environment-profile'

    def bind(self, *, body=None, token=MAINTENANCE):
        payload = body if body is not None else {
            'schema': 'workspace.environment_profile.bind.v1', 'operationId': 'bind-profile',
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
        self.assertIsNone(self.environment.environment_profile)

    def test_first_binding_and_lost_response_replay_are_identical(self):
        first = self.bind()
        self.assertEqual(first.status_code, 200, first.content)
        self.assertEqual(first.json(), {'schema': 'workspace.environment_profile.bound.v1',
            'operationId': 'bind-profile', 'sessionId': self.session_id,
            'environmentId': self.environment.pk, 'profile': profile(),
            'profileDigest': environment_profile_digest(profile())})
        self.assertEqual(self.bind().json(), first.json())
        self.environment.refresh_from_db()
        self.assertEqual(self.environment.environment_profile, profile())
        self.assertEqual(self.environment.environment_profile_operation_id, 'bind-profile')

    def test_rebinding_operation_conflict_and_foreign_session_are_rejected(self):
        self.assertEqual(self.bind().status_code, 200)
        body = {'schema': 'workspace.environment_profile.bind.v1', 'operationId': 'other',
                'sessionId': self.session_id, 'profile': profile()}
        self.assertEqual(self.bind(body=body).status_code, 409)
        body['operationId'] = 'bind-profile'
        body['profile']['workspaceRoot'] = '/other'
        self.assertEqual(self.bind(body=body).status_code, 409)
        body['sessionId'] = self.create('other-session').json()['sessionId']
        self.assertEqual(self.bind(body=body).status_code, 404)

    def test_strict_body_and_public_message_cannot_select_profile(self):
        for changes in ({'extra': 1}, {'operationId': 'bad id'}, {'schema': 'old'}):
            body = {'schema': 'workspace.environment_profile.bind.v1', 'operationId': 'bind-profile',
                    'sessionId': self.session_id, 'profile': profile(), **changes}
            self.assertEqual(self.bind(body=body).status_code, 400)
        response = self.post(self.create_url + f'/{self.session_id}/messages', environmentProfile=profile())
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
        self.url = f'/internal/environments/{self.environment.pk}/environment-profile'
        self.assertEqual(self.bind().status_code, 409)
        self.environment.refresh_from_db()
        self.assertIsNotNone(self.environment.environment_profile)

    def test_bound_and_default_environments_have_the_same_authorization_shape(self):
        self.assertEqual(self.bind().status_code, 200)
        selected = self.post(self.create_url + f'/{self.session_id}/messages')
        self.assertEqual(selected.status_code, 202, selected.content)
        configured = AgentRun.objects.get(pk=selected.json()['agentRunId']).authorization
        ordinary = self.post(operationId='ordinary')
        self.assertEqual(ordinary.status_code, 202, ordinary.content)
        default = AgentRun.objects.get(pk=ordinary.json()['agentRunId']).authorization
        self.assertEqual(set(configured.payload), set(default.payload))
        self.assertEqual(configured.payload['environmentProfile'], profile())
        for grant in (configured, default):
            self.assertEqual(grant.payload['schema'], 'workspace.agent_run_authorization.v1')
            validate_agent_run_authorization_payload(grant.payload)
            self.assertEqual(authorization_digest(grant.payload), grant.digest)
            self.assertEqual(authorization_signature(grant.payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY), grant.signature)

    def test_profile_is_reused_by_later_runs_and_lifecycle_can_select_a_new_environment(self):
        self.assertEqual(self.bind().status_code, 200)
        target = self.create_url + f'/{self.session_id}/messages'
        first = self.post(target)
        self.assertEqual(first.status_code, 202, first.content)
        AgentRun.objects.filter(pk=first.json()['agentRunId']).update(status='completed')
        second = self.post(target, operationId='second')
        self.assertEqual(second.status_code, 202, second.content)
        self.assertEqual(AgentRun.objects.get(pk=second.json()['agentRunId']).authorization.payload['environmentProfile'], profile())
        other = SessionEnvironment.objects.create(session=self.session)
        self.session.current_environment = other
        self.session.save(update_fields=['current_environment'])
        self.session.refresh_from_db()
        self.assertEqual(self.session.current_environment_id, other.pk)

    def test_new_environment_cannot_insert_an_already_bound_profile(self):
        from django.utils import timezone
        with self.assertRaisesRegex(ValueError, 'binding'):
            SessionEnvironment.objects.create(session=self.session, environment_profile=profile(),
                environment_profile_digest=environment_profile_digest(profile()),
                environment_profile_operation_id='raw-profile', environment_profile_bound_at=timezone.now())

    def test_model_save_cannot_replace_or_clear_bound_profile(self):
        self.assertEqual(self.bind().status_code, 200)
        self.environment.refresh_from_db()
        self.environment.environment_profile = None
        with self.assertRaisesRegex(ValueError, 'immutable'):
            self.environment.save(update_fields=['environment_profile'])

    def test_postgres_bulk_and_raw_profile_mutations_are_rejected(self):
        if connection.vendor != 'postgresql':
            self.skipTest('PostgreSQL trigger enforcement requires isolated PostgreSQL')
        self.assertEqual(self.bind().status_code, 200)
        for mutation in ('bulk', 'raw'):
            with self.subTest(mutation=mutation), self.assertRaises(IntegrityError), transaction.atomic():
                if mutation == 'bulk':
                    SessionEnvironment.objects.filter(pk=self.environment.pk).update(environment_profile=None)
                else:
                    with connection.cursor() as cursor:
                        cursor.execute('UPDATE app_core_sessionenvironment SET environment_profile=NULL WHERE id=%s', [self.environment.pk])
