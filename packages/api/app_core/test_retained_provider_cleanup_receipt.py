"""Retained Run cleanup releases its controller while preserving the provider."""
import copy

from django.test import SimpleTestCase
from unittest.mock import patch

from .environment_profile import environment_profile_digest
from .runtime_client import get_agent_run_cleanup_receipt
from .test_provider_cleanup_receipt import (
    provider_profile, provider_receipt, runtime_receipt_http,
)


def retained_profile():
    return provider_profile() | {'lifetime': 'retained'}


def retained_receipt():
    profile = retained_profile()
    common = {key: value for key, value in provider_receipt().items()
        if key not in {'operationCount', 'settlementDigest'}}
    return common | {'schema': 'runtime.agent_run.cleanup_receipt.retained_provider.v1',
        'profileDigest': environment_profile_digest(profile), 'scope': 'runEffectsOnly',
        'providerUseId': 'provider-use-1', 'providerUseDigest': 'sha256:' + 'e' * 64}


class RetainedProviderReceiptContractTests(SimpleTestCase):
    def query(self, receipt, *, profile=None):
        selected = retained_profile() if profile is None else profile
        with patch('app_core.runtime_client.request_json', return_value=receipt) as transport:
            result = get_agent_run_cleanup_receipt('run-1', 'sha256:' + 'a' * 64, 'receipt-1',
                frozen_profile=selected, frozen_profile_digest=environment_profile_digest(selected))
        return result, transport

    def test_exact_retained_evidence_releases_run_effects_without_physical_settlement(self):
        expected = retained_receipt()
        actual, transport = self.query(expected)
        self.assertEqual(actual, expected)
        transport.assert_called_once_with('/internal/agent-runs/cleanup-receipt', {
            'schema': 'runtime.agent_run.cleanup_receipt.get.v2', 'agentRunId': 'run-1',
            'authorizationDigest': expected['authorizationDigest'], 'cleanupReceiptId': 'receipt-1'}, timeout=10)

    def test_retained_proof_cannot_satisfy_a_per_run_physical_cleanup_contract(self):
        receipt = retained_receipt() | {'profileDigest': environment_profile_digest(provider_profile())}
        with self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
            self.query(receipt, profile=provider_profile())

    def test_retained_receipt_rejects_incomplete_binding_and_invented_removal_facts(self):
        valid = retained_receipt()
        changes = ({'scope': 'noProviderEffectsForRun'}, {'scope': 'environmentRetired'},
            {'providerUseId': ''}, {'providerUseId': 'other/path'}, {'providerUseDigest': None},
            {'providerUseDigest': 'sha256:' + 'E' * 64}, {'operationCount': 1},
            {'settlementDigest': 'sha256:' + 'd' * 64}, {'removed': True}, {'extra': True},
            {'agentRunId': 'run-2'}, {'authorizationDigest': 'sha256:' + 'b' * 64},
            {'profileDigest': 'sha256:' + 'b' * 64}, {'controllerEpoch': True},
            {'controllerEpoch': 0}, {'cleanupComplete': 1})
        for change in changes:
            with self.subTest(change=change), self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                self.query(valid | change)
        for key in valid:
            with self.subTest(missing=key), self.assertRaisesMessage(ValueError, 'agent_run_cleanup_receipt_invalid'):
                self.query({name: value for name, value in valid.items() if name != key})

    def test_retained_receipt_is_validated_over_the_real_http_read(self):
        expected = retained_receipt()
        profile = retained_profile()
        with runtime_receipt_http(expected) as requests:
            actual = get_agent_run_cleanup_receipt('run-1', expected['authorizationDigest'], 'receipt-1',
                frozen_profile=profile, frozen_profile_digest=environment_profile_digest(profile))
        self.assertEqual(actual, expected)
        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0][0], '/internal/agent-runs/cleanup-receipt')

    def test_retained_controller_release_keeps_existing_current_owner_fence(self):
        from .test_provider_cleanup_receipt import ProviderCleanupReleaseTests
        fixture = ProviderCleanupReleaseTests(methodName='test_valid_provider_receipt_releases_only_through_existing_current_authority_check')
        fixture.setUp()
        profile = retained_profile()
        fixture.run.environment.environment_profile = profile
        fixture.run.environment.environment_profile_digest = environment_profile_digest(profile)
        fixture.run.authorization.payload = {'environmentProfile': copy.deepcopy(profile),
            'environmentProfileDigest': environment_profile_digest(profile)}
        response, _, release = fixture.release(retained_receipt())
        self.assertEqual(response.status_code, 200, response.content)
        release.assert_called_once_with('run-1', 7, 'worker-1')
        response, _, release = fixture.release(retained_receipt(), release_error=ValueError('lease_lost'))
        self.assertEqual(response.status_code, 409)
