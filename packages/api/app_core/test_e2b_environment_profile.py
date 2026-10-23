"""Provider-specific hosted profiles preserve signed identity and honest limits."""
import copy
import json
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from django.test import SimpleTestCase, TestCase

from .environment_profile import environment_profile_digest, validate_environment_profile
from .runtime_client import request_execution_profile
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture
from .test_hosted_operations import OperationFixture


def profile():
    root = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/e2b_environment_profile/v1'
    return json.loads((root / 'valid.json').read_text(encoding='utf-8'))


class E2BEnvironmentProfileTests(SimpleTestCase):
    def test_managed_workspace_initialization_uses_the_shared_explicit_profile(self):
        root = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/e2b_environment_profile/v1'
        managed = json.loads((root / 'managed-files.json').read_text(encoding='utf-8'))
        from .test_agent_run_authorization import corpus, payload_for
        case = next(case for case in corpus() if case['id'] == 'e2b_managed_files')
        signed = payload_for(case)
        self.assertTrue(case['supportedSignerOutput'])
        self.assertEqual(signed['environmentProfile'], managed)
        self.assertEqual(signed['environmentProfileDigest'], environment_profile_digest(managed))
        validate_environment_profile(managed)
        original = managed.copy()
        original.pop('workspaceInitialization')
        validate_environment_profile(original)
        self.assertNotIn('workspaceInitialization', original)
        self.assertNotEqual(environment_profile_digest(managed), environment_profile_digest(original))
        response = {'schema': 'runtime.execution_profile.v2', 'environmentProfile': managed}
        with patch('app_core.runtime_client.request_json', return_value=response):
            self.assertEqual(request_execution_profile(), response)

    def test_managed_workspace_initialization_rejects_unknown_null_and_unmanaged_roots(self):
        for value in (None, True, False, 0, [], {}, '', 'preserve', 'managed_files'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                validate_environment_profile(profile() | {'workspaceInitialization': value})
        for changed in ({'workspaceRoot': '/home/user', 'user': 'agent'},
                        {'workspaceRoot': '/mnt/data', 'user': 'user'}):
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                validate_environment_profile(profile() | {'workspaceInitialization': 'managedFiles'} | changed)

    def test_managed_initialization_is_part_of_the_signed_run_profile(self):
        from .runtime_contract import verify_agent_run_authorization_signature
        selected = profile() | {'workspaceInitialization': 'managedFiles', 'workspaceRoot': '/mnt/data', 'user': 'agent'}
        value = fixture() | {'environmentProfile': selected, 'environmentProfileDigest': environment_profile_digest(selected)}
        signature = authorization_signature(value, 'test-key')
        verify_agent_run_authorization_signature(value, 'test-key', signature)
        changed = copy.deepcopy(value)
        del changed['environmentProfile']['workspaceInitialization']
        changed['environmentProfileDigest'] = environment_profile_digest(changed['environmentProfile'])
        with self.assertRaises(ValueError):
            verify_agent_run_authorization_signature(changed, 'test-key', signature)

    def test_explicit_provider_profile_validates_and_binds_template_build(self):
        value = profile()
        validate_environment_profile(value)
        digest = environment_profile_digest(value)
        self.assertEqual(digest, environment_profile_digest(copy.deepcopy(value)))
        value['templateRef']['buildId'] = '191c5835-5da7-4cf5-8a7c-cc3f7c4ebfa4'
        self.assertNotEqual(digest, environment_profile_digest(value))

    def test_rejects_unenforced_resource_claims_and_unsafe_provider_configuration(self):
        mutations = [
            lambda p: p.update(resources={'storageBytes': 4 * 1024**3}),
            lambda p: p.update(dataTmpfsBytes=4 * 1024**3),
            lambda p: p.update(pidsLimit=512),
            lambda p: p.update(provider='docker'),
            lambda p: p.update(providerConfigDigest='secret'),
            lambda p: p.update(providerConfigDigest='sha256:' + 'C' * 64),
            lambda p: p['templateRef'].update(extra=True),
            lambda p: p['templateRef'].update(buildId='latest'),
            lambda p: p['templateRef'].update(buildId='E8E0D749-B6A4-4985-A50C-A239C77536F1'),
            lambda p: p['templateRef'].update(templateId='template:latest'),
            lambda p: p.update(lifetime='forever'),
            lambda p: p.update(timeoutSeconds=0),
            lambda p: p.update(timeoutSeconds=86401),
            lambda p: p.update(timeoutSeconds=True),
            lambda p: p.update(inputBudgetBytes=0),
            lambda p: p.update(inputBudgetBytes=64 * 1024**2 + 1),
            lambda p: p.update(inputBudgetBytes=True),
            lambda p: p.update(user='user; command'),
            lambda p: p.update(network='host'),
            lambda p: p.update(workspaceRoot='/home/../root'),
        ]
        for mutate in mutations:
            value = profile()
            mutate(value)
            with self.subTest(value=value), self.assertRaises(ValueError):
                validate_environment_profile(value)

    def test_profile_variant_is_signed_without_reinterpreting_legacy_grants(self):
        legacy = fixture()
        previous = (authorization_digest(legacy), authorization_signature(legacy, 'test-key'))
        value = legacy | {'environmentProfile': profile(), 'environmentProfileDigest': environment_profile_digest(profile())}
        self.assertNotEqual(authorization_digest(value), previous[0])
        self.assertNotEqual(authorization_signature(value, 'test-key'), previous[1])
        self.assertEqual((authorization_digest(legacy), authorization_signature(legacy, 'test-key')), previous)

    def test_input_budget_is_transfer_admission_and_not_half_of_a_tmpfs_quota(self):
        value = fixture()
        selected = profile() | {'inputBudgetBytes': 1}
        value.update(environmentProfile=selected, environmentProfileDigest=environment_profile_digest(selected))
        for item in value['assetRefs']:
            item['sizeBytes'] = 1
        authorization_digest(value)
        value['assetRefs'][0]['sizeBytes'] = 2
        with self.assertRaisesMessage(ValueError, 'declared inputs exceed inputBudgetBytes'):
            authorization_digest(value)

    def test_workspace_snapshot_transfer_uses_provider_budget_without_tmpfs_claim(self):
        from .http.internal import _workspace_tmpfs_fits, SESSION_WORKSPACE_RESTORE_OVERHEAD_BYTES
        run = SimpleNamespace(authorization=SimpleNamespace(payload={'assetRefs': [], 'environmentProfile': profile()}))
        self.assertTrue(_workspace_tmpfs_fits(run, profile()['inputBudgetBytes'] - SESSION_WORKSPACE_RESTORE_OVERHEAD_BYTES))
        self.assertFalse(_workspace_tmpfs_fits(run, profile()['inputBudgetBytes']))

    def test_runtime_advertises_the_complete_exact_profile(self):
        response = {'schema': 'runtime.execution_profile.v2', 'environmentProfile': profile()}
        with patch('app_core.runtime_client.request_json', return_value=response):
            self.assertEqual(request_execution_profile(), response)
        for changed in (response | {'imageDigest': 'sha256:' + 'a' * 64},
                        response | {'environmentProfile': profile() | {'extra': True}},
                        response | {'schema': 'runtime.execution_profile.v3'}):
            with self.subTest(response=changed), patch('app_core.runtime_client.request_json', return_value=changed):
                with self.assertRaisesMessage(RuntimeError, 'runtime_execution_profile_response_invalid'):
                    request_execution_profile()

    def test_frozen_lifetime_rejects_worker_mode_reinterpretation(self):
        from .environment_authorization import build_environment_authorization
        run = SimpleNamespace(pk='run_1', environment_id='environment_1', session_id='session_1',
            environment=SimpleNamespace(id='environment_1', session_id='session_1', environment_profile=profile()),
            session=SimpleNamespace(id='session_1'))
        authorization = SimpleNamespace(digest='sha256:' + 'a' * 64)
        authority = {'environmentId': 'environment_1', 'controllerId': 'run_1', 'controllerEpoch': 1,
            'controllerLeaseDigest': 'sha256:' + 'b' * 64, 'controllerPurpose': 'execute'}
        self.assertEqual(build_environment_authorization(run, authorization, authority)['payload']['executionMode'], 'perRun')
        with self.assertRaisesMessage(ValueError, 'environment_lifetime_mismatch'):
            build_environment_authorization(run, authorization, authority, execution_mode='retained')

    def test_lifetime_conflict_is_rejected_before_controller_acquisition(self):
        from .http.internal import _resolve_agent_run
        run = SimpleNamespace(pk='run_1', environment=SimpleNamespace(environment_profile=profile()))
        body = {'schema': 'runtime.agent_run_lifecycle.resolve.v1', 'jobId': 'agent_run.lifecycle:run_1',
            'leaseOwner': 'worker_1', 'agentRunId': 'run_1', 'authorizationDigest': 'sha256:' + 'a' * 64,
            'executionMode': 'retained'}
        with patch('app_core.http.internal._retained_run_projection', return_value=(run, None)), \
                patch('app_core.http.internal.get_environment_release_receipt', return_value=None), \
                patch('app_core.http.internal.acquire_environment_controller', side_effect=AssertionError('must reject first')) as acquire:
            response = _resolve_agent_run(body)
        self.assertEqual(response.status_code, 409)
        self.assertEqual(json.loads(response.content), {'error': 'environment_lifetime_mismatch'})
        acquire.assert_not_called()


class E2BEnvironmentProfilePersistenceTests(OperationFixture, TestCase):
    def test_managed_initialization_is_frozen_before_signing_and_survives_later_defaults(self):
        from .models import AgentRun
        selected = profile() | {'workspaceInitialization': 'managedFiles', 'workspaceRoot': '/mnt/data', 'user': 'agent'}
        self.profile.return_value = {'schema': 'runtime.execution_profile.v2', 'environmentProfile': selected}
        accepted = self.post()
        self.assertEqual(accepted.status_code, 202, accepted.content)
        first = AgentRun.objects.get(pk=accepted.json()['agentRunId'])
        first.environment.refresh_from_db()
        self.assertEqual(first.environment.environment_profile, selected)
        self.assertEqual(first.authorization.payload['environmentProfile'], selected)
        AgentRun.objects.filter(pk=first.pk).update(status='completed')
        self.profile.return_value = {'schema': 'runtime.execution_profile.v2', 'environmentProfile': profile()}
        later = self.post(self.create_url + f'/{first.session_id}/messages', operationId='managed-later')
        self.assertEqual(later.status_code, 202, later.content)
        second = AgentRun.objects.get(pk=later.json()['agentRunId'])
        self.assertEqual(second.authorization.payload['environmentProfile'], selected)

    def test_first_use_freezes_provider_build_and_policy_before_signing(self):
        from .models import AgentRun
        self.profile.return_value = {'schema': 'runtime.execution_profile.v2', 'environmentProfile': profile()}
        accepted = self.post()
        self.assertEqual(accepted.status_code, 202, accepted.content)
        first = AgentRun.objects.get(pk=accepted.json()['agentRunId'])
        first.environment.refresh_from_db()
        self.assertEqual(first.environment.environment_profile, profile())
        self.assertEqual(first.authorization.payload['environmentProfile'], profile())
        AgentRun.objects.filter(pk=first.pk).update(status='completed')
        changed = profile() | {'lifetime': 'retained', 'timeoutSeconds': 2}
        self.profile.return_value = {'schema': 'runtime.execution_profile.v2', 'environmentProfile': changed}
        later = self.post(self.create_url + f'/{first.session_id}/messages', operationId='later')
        self.assertEqual(later.status_code, 202, later.content)
        second = AgentRun.objects.get(pk=later.json()['agentRunId'])
        self.assertEqual(second.authorization.payload['environmentProfile'], profile())

    def test_network_mismatch_does_not_leave_profile_or_authorization(self):
        from .agent_run_authorization_factory import create_agent_run_authorization
        from .models import AgentRun, Session
        session = Session.objects.get(pk=self.create().json()['sessionId'])
        run = AgentRun.objects.create(session=session, workspace=self.workspace, user=self.user,
            modelConfig=self.model, prompt='network mismatch')
        selected = profile() | {'network': 'publicInternet'}
        with self.assertRaisesMessage(ValueError, 'environment_profile_network_mismatch'):
            create_agent_run_authorization(run, execution_profile={
                'schema': 'runtime.execution_profile.v2', 'environmentProfile': selected})
        run.environment.refresh_from_db()
        self.assertIsNone(run.environment.environment_profile)
        self.assertEqual(run.environment.environment_profile_digest, '')
        self.assertFalse(AgentRun.objects.filter(pk=run.pk, authorization__isnull=False).exists())

    def test_legacy_environment_and_signed_grants_are_not_rebound_to_new_provider(self):
        from .models import AgentRun
        accepted = self.post()
        first = AgentRun.objects.get(pk=accepted.json()['agentRunId'])
        original = (copy.deepcopy(first.authorization.payload), first.authorization.digest, first.authorization.signature)
        AgentRun.objects.filter(pk=first.pk).update(status='completed')
        self.profile.return_value = {'schema': 'runtime.execution_profile.v2', 'environmentProfile': profile()}
        later = self.post(self.create_url + f'/{first.session_id}/messages', operationId='legacy-later')
        self.assertEqual(later.status_code, 202, later.content)
        second = AgentRun.objects.get(pk=later.json()['agentRunId'])
        first.authorization.refresh_from_db()
        self.assertEqual((first.authorization.payload, first.authorization.digest, first.authorization.signature), original)
        self.assertEqual(second.authorization.payload['environmentProfile'], original[0]['environmentProfile'])
