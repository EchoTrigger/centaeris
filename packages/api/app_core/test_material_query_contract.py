"""Observable material query contracts of the platform service."""

from datetime import UTC, datetime
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
            representation_id="representation_1", owner=SimpleNamespace(updatedAt=datetime(2026, 1, 1, tzinfo=UTC)),
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
            "segments": patch.object(material_reads.KnowledgeSegment.objects, "filter"),
            "hit": patch.object(material_reads.material_evidence, "search_evidence", side_effect=lambda segment, _item, score, full_window=False: {"segmentId": segment.segmentId, "score": score}),
        }
        for name, patcher in patches.items():
            setattr(self, name, patcher.start())
            self.addCleanup(patcher.stop)
        self.candidates = self.segments.return_value.select_related.return_value.order_by.return_value
        self.candidates.__getitem__.return_value = []

    def read_request(self, **changes):
        options = {"offset": None, "limit": None, **changes}
        inputs = options.pop("inputs", self.common["inputs"])
        return material_reads.read_materials(self.access, inputs, **options)

    def search_request(self, **changes):
        options = {"query": "policy", "ranking": "relevance", "date_range": None, "limit": 20, **changes}
        if "dateRange" in options:
            options["date_range"] = options.pop("dateRange")
        inputs = options.pop("inputs", self.common["inputs"])
        return material_reads.search_materials(self.access, inputs, **options)

    def test_unknown_fields_and_wrong_schema_fail_closed(self):
        for call in (self.read_request, self.search_request):
            for change in ({"unexpected": True}, {"schema": "unknown"}):
                with self.subTest(call=call.__name__, change=change), self.assertRaises(TypeError):
                    call(**change)

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

    def test_search_rejects_invalid_query_ranking_limit_and_date_range(self):
        for changes in ({"query": " "}, {"query": "界" * 683}, {"ranking": "unknown"}, {"limit": True}, {"limit": 21}, {"dateRange": {}}, {"dateRange": {"updatedFrom": "2026-01-01", "updatedTo": None}}):
            with self.subTest(changes=changes), self.assertRaises(KnowledgeError):
                self.search_request(**changes)
        self.segments.assert_not_called()

    def test_search_ties_are_deterministic_and_nonmatches_are_omitted(self):
        self.candidates.__getitem__.return_value = [
            SimpleNamespace(segmentId=name, boundedText=text, representation_id="representation_1")
            for name, text in (("z", "policy"), ("a", "policy"), ("b", "unrelated"))
        ]
        self.assertEqual(self.search_request()["hits"], [{"segmentId": "a", "score": 3}, {"segmentId": "z", "score": 3}])
        self.assertEqual(self.search_request(limit=1)["hits"], [{"segmentId": "a", "score": 3}])

    def test_pending_search_never_queries_segments(self):
        missing = [{"inputRef": "input", "representationId": "representation_1"}]
        self.representations.return_value = ([None], missing)
        self.assertEqual(self.search_request(), {"disposition": "pending", "missing": missing})
        self.segments.assert_not_called()

    def test_candidate_overflow_fails_instead_of_silently_searching_a_subset(self):
        self.candidates.__getitem__.return_value = [object()] * 10_001
        with self.assertRaisesRegex(KnowledgeError, "knowledge_search_candidate_limit_exceeded"):
            self.search_request()

    def test_empty_input_scope_does_not_expand_to_the_user_library(self):
        self.bindings.return_value = []
        self.representations.return_value = ([], [])
        result = self.search_request(inputs=[])
        self.assertEqual(result["hits"], [])
        self.segments.assert_called_once_with(representation_id__in=[])

    def test_service_read_needs_no_http_envelope(self):
        direct = material_reads.read_materials(self.access, self.common["inputs"])
        wrapped = self.read_request()
        self.assertNotIn("schema", direct)
        self.assertEqual(wrapped, {**direct})

    def test_service_search_needs_no_http_envelope(self):
        direct = material_reads.search_materials(
            self.access, self.common["inputs"], query="policy", ranking="relevance",
            date_range=None, limit=20,
        )
        self.assertNotIn("schema", direct)
        self.assertEqual(self.search_request(), {**direct})
