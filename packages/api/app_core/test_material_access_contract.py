"""Characterize the authorization boundary before extracting platform services."""

from types import SimpleNamespace
from unittest.mock import Mock, patch

from django.test import SimpleTestCase

from . import material_access as access
from .material_contract import KnowledgeError


class MaterialAccessContractTests(SimpleTestCase):
    def setUp(self):
        self.identity = {
            "ownerKind": "sourceObject", "ownerId": "source_1",
            "generation": 3, "sha256": "sha256:" + "a" * 64,
        }
        self.spec_digest = "sha256:" + "b" * 64
        self.representation = access.representation_id(self.identity, self.spec_digest)
        self.authorization = SimpleNamespace(
            digest="authorized-digest",
            payload={"assetRefs": [{"inputRef": "input_1", "inputIdentity": self.identity}]},
        )
        self.run = SimpleNamespace(
            authorization=self.authorization, session="session_1", workspace="workspace_1",
        )
        self.binding = {"inputRef": "input_1", "representationId": self.representation}
        self.body = {
            "schema": "knowledge.read.v1", "agentRunId": "run_1",
            "authorizationDigest": self.authorization.digest,
            "processingSpecification": {"testSpecification": True},
            "specDigest": self.spec_digest,
        }
        patches = {
            "query": patch.object(access.AgentRun.objects, "select_related"),
            "membership": patch.object(access, "agent_run_membership_is_current", return_value=True),
            "digest": patch.object(access, "authorization_digest", return_value=self.authorization.digest),
            "specification": patch.object(access, "processing_spec_digest", return_value=self.spec_digest),
            "batch": patch.object(access, "input_storage_batch"),
            "links": patch.object(access.SessionAssetLink.objects, "select_related"),
            "supplements": patch.object(access, "delivered_attachments", return_value=[]),
        }
        for name, patcher in patches.items():
            setattr(self, name, patcher.start())
            self.addCleanup(patcher.stop)
        self.storage = Mock(return_value=({"objectRef": "source_1"}, "private/source"))
        self.batch.return_value = self.storage
        self.query.return_value.get.return_value = self.run
        self.owner = SimpleNamespace(id="source_1")
        self.links.return_value.get.return_value = SimpleNamespace(
            sourceObject=self.owner, userLibraryObject=None, artifact=None,
        )

    def authorize(self):
        return access.authorize_material_access(access.MaterialAccessContext(
            self.body["agentRunId"], self.body["authorizationDigest"],
            self.body["processingSpecification"], self.body["specDigest"]))

    def bind(self, bindings=None):
        return access.bind_inputs(
            self.run, self.authorization.digest,
            [self.binding] if bindings is None else bindings, self.spec_digest,
        )

    def test_current_membership_is_required_on_every_authorization(self):
        self.assertIs(self.authorize().agent_run, self.run)
        self.membership.return_value = False
        with self.assertRaises(KnowledgeError) as caught:
            self.authorize()
        self.assertEqual((caught.exception.code, caught.exception.status), ("knowledge_authorization_mismatch", 403))

    def test_missing_run_fails_closed(self):
        self.query.return_value.get.side_effect = access.AgentRun.DoesNotExist
        with self.assertRaises(KnowledgeError) as caught:
            self.authorize()
        self.assertEqual((caught.exception.code, caught.exception.status), ("knowledge_agent_run_not_found", 404))

    def test_both_stored_and_requested_authorization_digests_must_match(self):
        for target in ("stored", "requested"):
            with self.subTest(target=target):
                self.digest.return_value = "wrong" if target == "stored" else self.authorization.digest
                self.body["authorizationDigest"] = "wrong" if target == "requested" else self.authorization.digest
                with self.assertRaisesRegex(KnowledgeError, "knowledge_authorization_mismatch"):
                    self.authorize()

    def test_processing_specification_digest_cannot_be_substituted(self):
        self.body["specDigest"] = "sha256:" + "c" * 64
        with self.assertRaisesRegex(KnowledgeError, "knowledge_specification_digest_mismatch"):
            self.authorize()

    def test_only_declared_input_refs_can_be_bound(self):
        self.binding["inputRef"] = "another_users_input"
        with self.assertRaises(KnowledgeError) as caught:
            self.bind()
        self.assertEqual((caught.exception.code, caught.exception.status), ("knowledge_input_not_authorized", 403))
        self.storage.assert_not_called()

    def test_changed_generation_cannot_reuse_old_representation(self):
        self.identity["generation"] += 1
        with self.assertRaisesRegex(KnowledgeError, "knowledge_representation_binding_mismatch"):
            self.bind()
        self.storage.assert_not_called()

    def test_duplicate_refs_and_unknown_binding_fields_are_rejected(self):
        for bindings in ([self.binding, self.binding], [{**self.binding, "storageKey": "private/other"}]):
            with self.subTest(bindings=bindings), self.assertRaises(KnowledgeError):
                self.bind(bindings)
        self.storage.assert_not_called()

    def test_revoked_or_deleted_input_is_not_resolved_from_the_snapshot(self):
        self.assertEqual(self.bind()[0].storage_key, "private/source")
        self.storage.side_effect = access.DeferredInputResolutionError("asset_unavailable")
        with self.assertRaisesRegex(KnowledgeError, "asset_unavailable"):
            self.bind()

    def test_signature_or_current_identity_failure_is_not_ignored(self):
        self.storage.side_effect = access.DeferredInputBindingError("current identity changed")
        with self.assertRaisesRegex(KnowledgeError, "knowledge_input_binding_invalid"):
            self.bind()

    def test_input_owner_lookup_is_scoped_to_session_and_workspace(self):
        result = self.bind()[0]
        self.assertIs(result.owner, self.owner)
        self.links.return_value.get.assert_called_once_with(
            id="input_1", session="session_1", workspace="workspace_1",
        )
        self.links.return_value.get.side_effect = access.SessionAssetLink.DoesNotExist
        with self.assertRaisesRegex(KnowledgeError, "knowledge_input_not_authorized"):
            self.bind()

    def test_resolved_owner_must_match_current_link_owner(self):
        self.owner.id = "different_owner"
        with self.assertRaisesRegex(KnowledgeError, "knowledge_input_binding_invalid"):
            self.bind()
