"""Provider cleanup evidence is bound to frozen API records before business release."""
import copy
import json
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from django.db import connection
from django.test import RequestFactory, SimpleTestCase, TransactionTestCase, override_settings
from django.utils import timezone

from .environment_profile import environment_profile_digest
from .http import internal
from .runtime_client import get_agent_run_cleanup_receipt
from .test_execution_publication_fencing import ExecutionPublicationFixture


def provider_profile():
    fixture = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/e2b_environment_profile/v1/valid.json'
    return json.loads(fixture.read_text(encoding='utf-8'))


def provider_receipt():
    profile = provider_profile()
    return {'schema': 'runtime.agent_run.cleanup_receipt.v2', 'receiptId': 'receipt-1',
        'agentRunId': 'run-1', 'environmentId': 'environment-1', 'authorizationDigest': 'sha256:' + 'a' * 64,
        'controllerEpoch': 7, 'cleanupComplete': True, 'provider': 'e2b',
        'profileDigest': environment_profile_digest(profile), 'providerConfigDigest': profile['providerConfigDigest'],
        'settlementDigest': 'sha256:' + 'd' * 64, 'operationCount': 1}


def no_provider_effects_receipt():
    physical = provider_receipt()
    return {name: value for name, value in physical.items()
        if name not in {'operationCount', 'settlementDigest'}} | {
        'schema': 'runtime.agent_run.cleanup_receipt.no_provider_effects.v1',
        'scope': 'noProviderEffectsForRun'}


@contextmanager
def runtime_receipt_http(receipt, *, status=200):
    """Exercise the production JSON read without a provider or model call."""
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get('Content-Length', '0'))
            assert 0 < length <= 4096
            requests.append((self.path, json.loads(self.rfile.read(length)),
                self.headers.get('X-Internal-Token')))
            body = json.dumps(receipt).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Connection', 'close')
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = True
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with override_settings(RUNTIME_URL=f'http://127.0.0.1:{server.server_port}'), \
                patch.dict('os.environ', {'NO_PROXY': 'localhost,127.0.0.1', 'no_proxy': 'localhost,127.0.0.1'}):
            yield requests
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        assert not thread.is_alive()


class ProviderCleanupReceiptContractTests(SimpleTestCase):
    def query(self, result, **kwargs):
        profile = provider_profile()
        with patch('app_core.runtime_client.request_json', return_value=result) as transport:
            value = get_agent_run_cleanup_receipt('run-1', 'sha256:' + 'a' * 64, 'receipt-1',
                frozen_profile=profile, frozen_profile_digest=environment_profile_digest(profile), **kwargs)
        return value, transport

    def test_frozen_provider_profile_selects_exact_query_and_validates_complete_evidence(self):
        expected = provider_receipt()
        value, transport = self.query(expected)
        self.assertEqual(value, expected)
        transport.assert_called_once_with('/internal/agent-runs/cleanup-receipt', {
            'schema': 'runtime.agent_run.cleanup_receipt.get.v2', 'agentRunId': 'run-1',
            'authorizationDigest': expected['authorizationDigest'], 'cleanupReceiptId': 'receipt-1'}, timeout=10)

    def test_provider_receipt_rejects_unknown_fields_identity_and_invalid_facts(self):
        expected = provider_receipt()
        for change in ({'schema': 'runtime.agent_run.cleanup_receipt.v1'}, {'extra': True},
                {'receiptId': 'other'}, {'agentRunId': 'other'}, {'environmentId': 'bad/path'},
                {'authorizationDigest': 'sha256:' + 'b' * 64}, {'provider': 'docker'},
                {'profileDigest': 'sha256:' + 'b' * 64}, {'providerConfigDigest': 'sha256:' + 'b' * 64},
                {'settlementDigest': 'sha256:' + 'D' * 64}, {'settlementDigest': None},
                {'cleanupComplete': False}, {'cleanupComplete': 1}, {'controllerEpoch': True},
                {'controllerEpoch': 0}, {'controllerEpoch': 2**63}, {'operationCount': 0},
                {'operationCount': True}, {'operationCount': 2**32}, {'operationCount': '1'}):
            with self.subTest(change=change), self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                self.query(expected | change)
        for field in expected:
            with self.subTest(missing=field), self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                self.query({name: value for name, value in expected.items() if name != field})

    def test_invalid_or_unbound_profile_rejects_before_runtime_request(self):
        profile = provider_profile()
        for frozen, digest in ((profile, 'sha256:' + 'b' * 64), (profile, None),
                (profile | {'provider': 'other'}, environment_profile_digest(profile)),
                (profile | {'extra': True}, environment_profile_digest(profile)),
                (None, environment_profile_digest(profile))):
            with self.subTest(profile=frozen, digest=digest), patch('app_core.runtime_client.request_json') as transport:
                with self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                    get_agent_run_cleanup_receipt('run-1', 'sha256:' + 'a' * 64, 'receipt-1',
                        frozen_profile=frozen, frozen_profile_digest=digest)
                transport.assert_not_called()

    def test_owned_no_provider_effects_is_an_explicit_variant_of_the_frozen_provider_read(self):
        receipt = no_provider_effects_receipt()
        actual, transport = self.query(receipt)
        self.assertEqual(actual, receipt)
        self.assertEqual(transport.call_args.args[1]['schema'], 'runtime.agent_run.cleanup_receipt.get.v2')

    def test_no_provider_effects_rejects_fake_physical_facts_and_no_operation_release_scope(self):
        receipt = no_provider_effects_receipt()
        for change in ({'scope': 'noEffectsForRun'}, {'scope': None}, {'operationCount': 0},
                {'settlementDigest': 'sha256:' + 'd' * 64}, {'allocation': {'kind': 'neverAllocated'}},
                {'sandboxId': 'fake'}, {'profileDigest': 'sha256:' + 'b' * 64},
                {'providerConfigDigest': 'sha256:' + 'b' * 64}, {'cleanupComplete': 1}):
            with self.subTest(change=change), self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                self.query(receipt | change)
        for field in receipt:
            with self.subTest(missing=field), self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                self.query({name: value for name, value in receipt.items() if name != field})

    def test_legacy_receipt_protocol_is_exact_and_never_inferred_from_a_response(self):
        legacy = {'schema': 'runtime.agent_run.cleanup_receipt.v1', 'receiptId': 'receipt-1',
            'agentRunId': 'run-1', 'environmentId': 'environment-1', 'allocation': {'kind': 'neverAllocated'},
            'authorizationDigest': 'sha256:' + 'a' * 64, 'controllerEpoch': 7, 'cleanupComplete': True}
        with patch('app_core.runtime_client.request_json', return_value=legacy) as transport:
            self.assertEqual(get_agent_run_cleanup_receipt('run-1', legacy['authorizationDigest'], 'receipt-1'), legacy)
        self.assertEqual(transport.call_args.args[1]['schema'], 'runtime.agent_run.cleanup_receipt.get.v1')
        with patch('app_core.runtime_client.request_json', return_value=provider_receipt()):
            with self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                get_agent_run_cleanup_receipt('run-1', legacy['authorizationDigest'], 'receipt-1')


class ProviderCleanupReleaseTests(SimpleTestCase):
    def setUp(self):
        profile = provider_profile()
        digest = environment_profile_digest(profile)
        self.run = SimpleNamespace(pk='run-1', environment_id='environment-1',
            environment=SimpleNamespace(environment_profile=profile, environment_profile_digest=digest),
            authorization=SimpleNamespace(payload={'environmentProfile': copy.deepcopy(profile), 'environmentProfileDigest': digest}))
        self.body = {'schema': 'runtime.agent_run_lifecycle.release.v1', 'jobId': 'agent_run.lifecycle:run-1',
            'leaseOwner': 'worker-1', 'agentRunId': 'run-1', 'authorizationDigest': 'sha256:' + 'a' * 64,
            'controllerEpoch': 7, 'cleanupReceiptId': 'receipt-1'}

    def release(self, receipt, *, body=None, release_error=None, already_released=None):
        request = RequestFactory().post('/internal/agent-run-lifecycle/release',
            data=json.dumps(body or self.body), content_type='application/json')
        with patch.object(internal, '_retained_run_projection', return_value=(self.run, 'cancelled')), \
                patch.object(internal, 'get_environment_release_receipt', return_value=already_released), \
                patch('app_core.runtime_client.request_json', return_value=receipt) as transport, \
                patch.object(internal, 'release_environment_controller', return_value={'releaseReceiptId': 'released-1'},
                    side_effect=release_error) as durable_release:
            response = internal.release_agent_run_lifecycle(request)
        return response, transport, durable_release

    def test_valid_provider_receipt_releases_only_through_existing_current_authority_check(self):
        response, transport, durable_release = self.release(provider_receipt())
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(json.loads(response.content)['releaseReceiptId'], 'released-1')
        self.assertEqual(transport.call_args.args[1]['schema'], 'runtime.agent_run.cleanup_receipt.get.v2')
        durable_release.assert_called_once_with('run-1', 7, 'worker-1')

    def test_wrong_environment_or_future_evidence_cannot_release(self):
        for change in ({'environmentId': 'other'}, {'controllerEpoch': 8},
                {'profileDigest': 'sha256:' + 'b' * 64}, {'providerConfigDigest': 'sha256:' + 'b' * 64},
                {'operationCount': 0}):
            with self.subTest(change=change):
                response, _, durable_release = self.release(provider_receipt() | change)
                self.assertEqual(response.status_code, 409)
                durable_release.assert_not_called()

    def test_old_immutable_evidence_uses_new_current_epoch_but_lost_lease_still_rejects(self):
        receipt = provider_receipt() | {'controllerEpoch': 2}
        response, _, durable_release = self.release(receipt)
        self.assertEqual(response.status_code, 200, response.content)
        durable_release.assert_called_once_with('run-1', 7, 'worker-1')
        response, _, _ = self.release(receipt, release_error=ValueError('environment_controller_lease_rejected'))
        self.assertEqual(response.status_code, 409)

    def test_frozen_profile_or_signed_binding_drift_rejects_before_receipt_read(self):
        original = copy.deepcopy(self.run)
        mutations = (
            lambda run: setattr(run.environment, 'environment_profile_digest', 'sha256:' + 'b' * 64),
            lambda run: run.authorization.payload.update(environmentProfileDigest='sha256:' + 'b' * 64),
            lambda run: run.authorization.payload['environmentProfile'].update(timeoutSeconds=2),
        )
        for mutate in mutations:
            self.run = copy.deepcopy(original)
            mutate(self.run)
            with self.subTest(run=self.run):
                response, transport, durable_release = self.release(provider_receipt())
                self.assertEqual(response.status_code, 409)
                transport.assert_not_called()
                durable_release.assert_not_called()

    def test_worker_provider_hint_is_rejected_and_released_replay_needs_no_runtime(self):
        response, transport, durable_release = self.release(provider_receipt(), body=self.body | {'provider': 'e2b'})
        self.assertEqual(response.status_code, 400)
        transport.assert_not_called()
        durable_release.assert_not_called()
        response, transport, durable_release = self.release(None, already_released={'releaseReceiptId': 'original-release'})
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(json.loads(response.content)['releaseReceiptId'], 'original-release')
        transport.assert_not_called()
        durable_release.assert_not_called()


class ProviderCleanupPersistenceTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        from .agent_run_authorization_factory import create_agent_run_authorization
        def authorize(run, **unused):
            return create_agent_run_authorization(run, execution_profile={
                'schema': 'runtime.execution_profile.v2', 'environmentProfile': provider_profile()})
        with patch('app_core.test_execution_publication_fencing.create_agent_run_authorization', side_effect=authorize):
            super().setUp()
        from .models import AgentRun
        AgentRun.objects.filter(pk=self.run.pk).update(preAdmissionCancelledAt=timezone.now())
        self.grant = self.resolve()['agentRunCleanup']['environmentAuthorization']['payload']

    def resolve(self):
        body = {'schema': 'runtime.agent_run_lifecycle.resolve.v1', 'jobId': self.job_id,
            'leaseOwner': self.owner, 'agentRunId': self.run.pk,
            'authorizationDigest': self.authorization.digest, 'executionMode': 'perRun'}
        response = self.client.post('/internal/agent-run-lifecycle/resolve', data=json.dumps(body),
            content_type='application/json', HTTP_X_INTERNAL_TOKEN='test-internal-token')
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()

    def receipt(self):
        profile = self.run.authorization.payload['environmentProfile']
        return provider_receipt() | {'agentRunId': self.run.pk, 'environmentId': self.run.environment_id,
            'authorizationDigest': self.authorization.digest, 'controllerEpoch': self.grant['controllerEpoch'],
            'profileDigest': self.run.authorization.payload['environmentProfileDigest'],
            'providerConfigDigest': profile['providerConfigDigest']}

    def release(self):
        body = {'schema': 'runtime.agent_run_lifecycle.release.v1', 'jobId': self.job_id,
            'leaseOwner': self.owner, 'agentRunId': self.run.pk,
            'authorizationDigest': self.authorization.digest, 'controllerEpoch': self.grant['controllerEpoch'],
            'cleanupReceiptId': 'receipt-1'}
        return self.client.post('/internal/agent-run-lifecycle/release', data=json.dumps(body),
            content_type='application/json', HTTP_X_INTERNAL_TOKEN='test-internal-token')

    def test_prior_provider_evidence_releases_current_epoch_once_and_replays_without_runtime(self):
        from .models import AgentRunEnvironmentRelease
        receipt = self.receipt()
        self._lease(self.replacement)
        self.owner = self.replacement
        self.grant = self.resolve()['agentRunCleanup']['environmentAuthorization']['payload']
        self.assertGreater(self.grant['controllerEpoch'], receipt['controllerEpoch'])
        with patch('app_core.runtime_client.request_json', return_value=receipt):
            response = self.release()
        self.assertEqual(response.status_code, 200, response.content)
        stored = AgentRunEnvironmentRelease.objects.get(agent_run=self.run)
        self.assertEqual(stored.controller_epoch, self.grant['controllerEpoch'])
        self.run.environment.refresh_from_db()
        self.assertTrue(self.run.environment.controller_released)
        with patch('app_core.runtime_client.request_json', side_effect=AssertionError('released replay queried Runtime')):
            self.assertEqual(self.release().json(), response.json())
        self.assertEqual(AgentRunEnvironmentRelease.objects.filter(agent_run=self.run).count(), 1)

    def test_invalid_provider_evidence_keeps_controller_and_does_not_write_release(self):
        from .models import AgentRunEnvironmentRelease
        for change in ({'environmentId': 'another-environment'}, {'providerConfigDigest': 'sha256:' + 'b' * 64},
                {'controllerEpoch': self.grant['controllerEpoch'] + 1}, {'operationCount': 0}):
            with self.subTest(change=change), patch('app_core.runtime_client.request_json', return_value=self.receipt() | change):
                self.assertEqual(self.release().status_code, 409)
                self.assertFalse(AgentRunEnvironmentRelease.objects.filter(agent_run=self.run).exists())
                self.run.environment.refresh_from_db()
                self.assertFalse(self.run.environment.controller_released)

    def test_lease_loss_during_provider_receipt_read_cannot_release(self):
        from .models import AgentRunEnvironmentRelease
        def receipt_read(*args, **kwargs):
            self.assertFalse(connection.in_atomic_block)
            self._lease(self.replacement)
            return self.receipt()
        with patch('app_core.runtime_client.request_json', side_effect=receipt_read):
            self.assertEqual(self.release().status_code, 409)
        self.assertFalse(AgentRunEnvironmentRelease.objects.filter(agent_run=self.run).exists())
        self.run.environment.refresh_from_db()
        self.assertFalse(self.run.environment.controller_released)


    def test_owned_terminal_before_provider_create_reads_absence_receipt_over_http_and_releases_once(self):
        from .models import AgentRunEnvironmentRelease
        physical = self.receipt()
        receipt = {name: value for name, value in physical.items()
            if name not in {'operationCount', 'settlementDigest'}} | {
            'schema': 'runtime.agent_run.cleanup_receipt.no_provider_effects.v1',
            'scope': 'noProviderEffectsForRun'}
        self.run.environment.refresh_from_db()
        self.assertEqual(self.run.environment.controller_run_id, self.run.pk)
        self.assertEqual(self.run.environment.controller_purpose, 'cleanup')
        self.assertFalse(self.run.environment.controller_released)
        with runtime_receipt_http(receipt) as reads:
            response = self.release()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(len(reads), 1)
        path, query, token = reads[0]
        self.assertEqual(path, '/internal/agent-runs/cleanup-receipt')
        self.assertEqual(query, {'schema': 'runtime.agent_run.cleanup_receipt.get.v2',
            'agentRunId': self.run.pk, 'authorizationDigest': self.authorization.digest,
            'cleanupReceiptId': 'receipt-1'})
        self.assertEqual(token, 'test-internal-token')
        released = AgentRunEnvironmentRelease.objects.get(agent_run=self.run)
        self.assertEqual(released.controller_epoch, self.grant['controllerEpoch'])
        self.run.environment.refresh_from_db()
        self.assertTrue(self.run.environment.controller_released)
        with patch('app_core.runtime_client.request_json', side_effect=AssertionError('released replay queried Runtime')):
            self.assertEqual(self.release().json(), response.json())
        self.assertEqual(AgentRunEnvironmentRelease.objects.filter(agent_run=self.run).count(), 1)

    def test_missing_runtime_receipt_is_not_inferred_as_no_provider_effects(self):
        from .models import AgentRunEnvironmentRelease
        with runtime_receipt_http({'error': 'cleanup_receipt_not_found'}, status=404) as reads:
            response = self.release()
        self.assertEqual(response.status_code, 503, response.content)
        self.assertEqual(len(reads), 1)
        self.assertFalse(AgentRunEnvironmentRelease.objects.filter(agent_run=self.run).exists())
        self.run.environment.refresh_from_db()
        self.assertFalse(self.run.environment.controller_released)
