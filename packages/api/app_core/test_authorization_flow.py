"""Observable authorization boundaries shared by input and startup consumers."""
from types import SimpleNamespace
from unittest.mock import patch
from django.test import SimpleTestCase, override_settings
from . import deferred_input as inputs
from .runtime_client import _validate_agent_run_binding
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture, KEY

@override_settings(AGENT_RUN_AUTHORIZATION_SIGNING_KEY=KEY)
class AuthorizationFlowTests(SimpleTestCase):
    def setUp(self):
        payload = fixture()
        self.run = SimpleNamespace(id=payload["agentRunId"], workspace_id=payload["workspaceId"],
            session_id=payload["sessionId"], user_id=payload["userId"], modelConfig_id=payload["modelConfigRef"],
            thinkingMode=payload["thinkingMode"], session=SimpleNamespace(agent_id=payload["agentId"]))
        self.run.authorization = SimpleNamespace(payload=payload, digest=authorization_digest(payload),
            signature=authorization_signature(payload, KEY))
        self.digest = self.run.authorization.digest
        self.membership = self.enterContext(patch.object(inputs, "agent_run_membership_is_current", return_value=True))
        self.enterContext(patch("app_core.runtime_client.agent_run_membership_is_current", return_value=True))
        self.delivered = self.enterContext(patch("app_core.agent_input_attachments.delivered_attachment", return_value=None))

    def resolve(self):
        return inputs.resolved_input_storage(self.run, "undeclared", self.digest)

    def test_valid_authorization_reaches_resource_scope_check(self):
        with self.assertRaisesRegex(inputs.DeferredInputResolutionError, "asset_unavailable"):
            self.resolve()
        self.delivered.assert_called_once_with(self.run, "undeclared")

    def test_membership_failure_precedes_corrupt_payload(self):
        self.membership.return_value = False
        self.run.authorization.payload = {}
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "Membership"):
            self.resolve()

    def test_digest_failure_precedes_signature_failure(self):
        self.run.authorization.digest = "wrong"
        self.run.authorization.signature = "wrong"
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "digest mismatch"):
            self.resolve()

    def test_signature_failure_precedes_binding_failure(self):
        self.run.id = "other_run"
        self.run.authorization.signature = "wrong"
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "signature mismatch"):
            self.resolve()

    def test_each_run_identity_is_bound(self):
        for field in ("id", "workspace_id", "session_id", "user_id", "modelConfig_id"):
            with self.subTest(field=field):
                original = getattr(self.run, field)
                setattr(self.run, field, "other")
                with self.assertRaisesRegex(inputs.DeferredInputBindingError, "binding mismatch"):
                    self.resolve()
                setattr(self.run, field, original)
        self.run.session.agent_id = "other"
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "binding mismatch"):
            self.resolve()

    def test_startup_also_binds_thinking_mode(self):
        self.assertIs(_validate_agent_run_binding(self.run), self.run.authorization)
        self.run.thinkingMode = None
        with self.assertRaisesRegex(RuntimeError, "binding mismatch"):
            _validate_agent_run_binding(self.run)
        # Input authority does not depend on the current reasoning selection.
        self.test_valid_authorization_reaches_resource_scope_check()

    def prepare_storage(self):
        declared = self.run.authorization.payload["assetRefs"][0]
        identity = declared["inputIdentity"]
        self.resolved = {"displayName": declared["displayName"], "contentType": declared["contentType"],
            "ownerKind": identity["ownerKind"], "objectRef": identity["ownerId"],
            "sourceVersion": str(identity["generation"]), "sha256": identity["sha256"],
            "sizeBytes": declared["sizeBytes"]}
        link = SimpleNamespace(sourceObject=SimpleNamespace(storageKey="private/test"), userLibraryObject=None, artifact=None)
        query = self.enterContext(patch.object(inputs.SessionAssetLink.objects, "select_related"))
        query.return_value.get.return_value = link
        self.resource = self.enterContext(patch.object(inputs, "resolved_input_for_link", return_value=self.resolved))
        self.enterContext(patch.object(inputs, "allocated_virtual_paths", return_value={"input_1": "/mnt/inputs/test"}))
        self.blob_exists = self.enterContext(patch.object(inputs.default_storage, "exists", return_value=True))
        return inputs.input_storage_batch(self.run, self.digest)

    def test_batch_rechecks_membership_and_current_resource(self):
        read = self.prepare_storage()
        self.assertEqual(read("input_1")[1], "private/test")
        self.resolved["sourceVersion"] = "2"
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "identity changed"):
            read("input_1")
        self.membership.return_value = False
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "Membership"):
            read("input_1")

    def test_batch_revalidates_changed_authorization_and_identity(self):
        read = self.prepare_storage()
        read("input_1")
        self.run.id = "other"
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "binding mismatch"):
            read("input_1")
        self.run.id = self.run.authorization.payload["agentRunId"]
        self.run.authorization.signature = "bad"
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "signature mismatch"):
            read("input_1")

    def test_batch_computes_one_digest_but_never_caches_resource_authority(self):
        read = self.prepare_storage()
        with patch.object(inputs, "authorization_digest", wraps=authorization_digest) as digest:
            read("input_1")
            read("input_1")
            self.assertEqual(digest.call_count, 1)
        self.assertEqual(self.membership.call_count, 2)
        self.assertEqual(self.resource.call_count, 2)
        # A fresh request must verify authorization again.
        with patch.object(inputs, "authorization_digest", wraps=authorization_digest) as digest:
            inputs.input_storage_batch(self.run, self.digest)("input_1")
            self.assertEqual(digest.call_count, 1)

    def test_batch_detects_in_place_payload_tampering(self):
        read = self.prepare_storage()
        read("input_1")
        self.run.authorization.payload["userId"] = "other_user"
        with self.assertRaisesRegex(inputs.DeferredInputBindingError, "digest mismatch"):
            read("input_1")

    def test_batch_does_not_cache_blob_availability(self):
        read = self.prepare_storage()
        read("input_1")
        self.blob_exists.return_value = False
        with self.assertRaisesRegex(inputs.DeferredInputResolutionError, "asset_unavailable"):
            read("input_1")
