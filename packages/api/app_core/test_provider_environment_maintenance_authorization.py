"""Strict provider maintenance authority uses its own domain and physical target."""
import copy
import json
from pathlib import Path
from unittest import TestCase

from .environment_maintenance_authorization import sign_maintenance_authorization


class ProviderMaintenanceAuthorizationTests(TestCase):
    def payload(self):
        old = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/environment_maintenance_authorization/v1/valid.json'
        payload = json.loads(old.read_text(encoding='utf-8'))['payload']
        payload.pop('instanceId')
        payload.pop('containerId')
        payload['schema'] = 'workspace.provider_environment_maintenance_authorization.v1'
        payload['providerTarget'] = {'provider': 'e2b', 'creationAttemptId': 'provider_create_' + 'a' * 64,
            'instanceId': 'instance_' + 'a' * 64, 'sandboxId': 'sandbox-example_01',
            'verificationDigest': 'sha256:' + 'b' * 64, 'providerConfigDigest': 'sha256:' + 'f' * 64}
        return payload

    def signer(self):
        from .provider_environment_maintenance_authorization import sign_provider_maintenance_authorization
        return sign_provider_maintenance_authorization

    def test_typed_target_rejects_aliases_unknown_fields_and_noncanonical_identities(self):
        from .provider_environment_maintenance_authorization import validate_provider_target
        target = self.payload()['providerTarget']
        validate_provider_target(target)
        for change in ({'provider': 'docker'}, {'containerId': 'b' * 64}, {'hostId': 'host'},
                {'sandboxId': 'sandbox/foreign'}, {'sandboxId': 'sandbox_未知'}, {'sandboxId': ''},
                {'creationAttemptId': 'a' * 257}, {'creationAttemptId': 'mutable name'},
                {'instanceId': 'arbitrary'}, {'verificationDigest': 'sha256:' + 'A' * 64},
                {'providerConfigDigest': 'not-a-digest'}):
            with self.subTest(change=change), self.assertRaises(ValueError):
                validate_provider_target(target | change)
        for field in target:
            missing = target.copy()
            missing.pop(field)
            with self.subTest(missing=field), self.assertRaises(ValueError):
                validate_provider_target(missing)

    def test_shared_runtime_vector_matches_the_real_signer(self):
        path = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json'
        vector = json.loads(path.read_text(encoding='utf-8'))
        self.assertEqual(vector['payload'], self.payload())
        self.assertEqual(self.signer()(vector['payload'], 'test-key'), vector['signature'])

    def test_all_common_fences_and_provider_target_fields_change_the_signature(self):
        signer = self.signer()
        payload = self.payload()
        original = signer(payload, 'test-key')
        changes = {'workspaceId': 'workspace_other', 'sessionId': 'session_other', 'environmentId': 'environment_other',
            'profileDigest': 'sha256:' + 'f' * 64,
            'controllerOwner': {'kind': 'maintenanceOperation', 'operationId': 'another-operation'},
            'controllerEpoch': payload['controllerEpoch'] + 1, 'controllerLeaseDigest': 'sha256:' + 'f' * 64,
            'controllerPurpose': 'cleanup', 'requestDigest': 'sha256:' + 'f' * 64,
            'expiresAtMs': payload['expiresAtMs'] + 1}
        for field, value in changes.items():
            with self.subTest(field=field):
                self.assertNotEqual(original, signer(payload | {field: value}, 'test-key'))
        for field, value in {'creationAttemptId': 'another-attempt', 'instanceId': 'instance_' + 'f' * 64,
                'sandboxId': 'another-sandbox', 'verificationDigest': 'sha256:' + 'f' * 64,
                'providerConfigDigest': 'sha256:' + 'e' * 64}.items():
            with self.subTest(target=field):
                self.assertNotEqual(original, signer(payload | {'providerTarget': payload['providerTarget'] | {field: value}}, 'test-key'))

    def test_unknown_fields_fake_run_owners_and_unbounded_grants_are_rejected(self):
        payload = self.payload()
        changes = ({'containerId': 'b' * 64}, {'instanceId': 'instance_' + 'b' * 64}, {'agentRunId': 'fake'},
            {'schema': 'workspace.environment_maintenance_authorization.v1'},
            {'controllerOwner': {'kind': 'agentRun', 'runId': 'fake'}}, {'operationKind': 'provision'},
            {'controllerEpoch': True}, {'controllerEpoch': 0}, {'controllerEpoch': 2**63},
            {'expiresAtMs': True}, {'expiresAtMs': 0}, {'expiresAtMs': 2**63},
            {'controllerPurpose': 'maintenance'})
        for change in changes:
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.signer()(payload | change, 'test-key')
        for field in payload:
            missing = copy.deepcopy(payload)
            missing.pop(field)
            with self.subTest(missing=field), self.assertRaises(ValueError):
                self.signer()(missing, 'test-key')

    def test_native_signature_bytes_remain_exact_and_provider_canonicalization_is_stable(self):
        path = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/environment_maintenance_authorization/v1/valid.json'
        vector = json.loads(path.read_text(encoding='utf-8'))
        self.assertEqual(sign_maintenance_authorization(vector['payload'], 'test-key'), vector['signature'])
        payload = self.payload()
        reversed_payload = dict(reversed(list(payload.items())))
        reversed_payload['providerTarget'] = dict(reversed(list(payload['providerTarget'].items())))
        self.assertEqual(self.signer()(payload, 'test-key'), self.signer()(reversed_payload, 'test-key'))
        with self.assertRaises(ValueError):
            self.signer()(payload, '')
