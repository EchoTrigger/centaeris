"""Observable material query contracts of the platform service."""

from types import SimpleNamespace
from unittest.mock import patch

from django.test import SimpleTestCase

from . import material_access, material_reads
from .material_contract import KnowledgeError


class MaterialQueryContractTests(SimpleTestCase):
    def setUp(self):
        self.spec = {"testSpecification": True}
        self.access = material_access.MaterialAccess(object(), self.spec, "spec", "digest")
        self.item = SimpleNamespace(
            representation_id="representation_1",
        )
        self.common = {
            "agentRunId": "run", "authorizationDigest": "digest",
            "processingSpecification": self.spec, "specDigest": "spec",
            "inputs": [{"inputRef": "input", "representationId": "representation_1"}],
        }
        patches = {
            "authorization": patch.object(material_access, "authorize_material_access", return_value=self.access),
            "bindings": patch.object(material_access, "bind_inputs", return_value=[self.item]),
            "representations": patch.object(material_reads, "_representations", return_value=([object()], [])),
            "read": patch.object(material_reads, "_read_representation", return_value={"content": "evidence"}),
        }
        for name, patcher in patches.items():
            setattr(self, name, patcher.start())
            self.addCleanup(patcher.stop)

    def read_request(self, **changes):
        options = {"offset": None, "limit": None, **changes}
        inputs = options.pop("inputs", self.common["inputs"])
        return material_reads.read_materials(self.access, inputs, **options)

    def test_unknown_fields_and_wrong_schema_fail_closed(self):
        for change in ({"unexpected": True}, {"schema": "unknown"}):
            with self.subTest(change=change), self.assertRaises(TypeError):
                self.read_request(**change)

    def test_pending_read_preserves_missing_bindings_without_partial_evidence(self):
        missing = [{"inputRef": "input", "representationId": "representation_1"}]
        self.representations.return_value = ([None], missing)
        self.assertEqual(self.read_request(), {"disposition": "pending", "missing": missing})
        self.read.assert_not_called()

    def test_ready_read_keeps_response_envelope_and_default_pagination(self):
        self.read.side_effect = lambda _item, _representation, offset, limit, full_window=False: {
            "content": "evidence", "startLine": offset + 1, "maxLines": limit,
        }
        self.assertEqual(self.read_request(), {
            "disposition": "ready",
            "items": [{"content": "evidence", "startLine": 1, "maxLines": 2_000}],
            "processingSpecification": self.spec,
        })

    def test_read_rejects_invalid_bounds_and_batch_pagination(self):
        for changes in ({"offset": -1}, {"offset": True}, {"limit": True}, {"limit": 0}, {"limit": 2_001}):
            with self.subTest(changes=changes), self.assertRaises(KnowledgeError):
                self.read_request(**changes)
        self.bindings.return_value = [self.item, self.item]
        with self.assertRaisesRegex(KnowledgeError, "knowledge_read_batch_pagination_invalid"):
            self.read_request(offset=0)
        self.read.assert_not_called()

    def test_service_read_needs_no_http_envelope(self):
        direct = material_reads.read_materials(self.access, self.common["inputs"])
        wrapped = self.read_request()
        self.assertNotIn("schema", direct)
        self.assertEqual(wrapped, {**direct})
