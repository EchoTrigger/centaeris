"""Observable material query contracts of the platform service."""

from datetime import UTC, datetime
from types import SimpleNamespace
from unittest.mock import patch

from django.test import SimpleTestCase, TestCase

from . import material_access, material_reads
from .material_contract import BoundInput, KnowledgeError, sha256_bytes
from .models import DerivedRepresentation, KnowledgeSegment, ProcessingSpecification


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
            "hit": patch.object(material_reads.material_evidence, "search_evidence", side_effect=lambda segment, _item, _representation, score, full_window=False: {"segmentId": segment.segmentId, "score": score}),
        }
        for name, patcher in patches.items():
            setattr(self, name, patcher.start())
            self.addCleanup(patcher.stop)
        self.candidates = self.segments.return_value.order_by.return_value
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


class MaterialSearchRepresentationTests(TestCase):
    @classmethod
    def setUpTestData(cls):
        cls.spec = ProcessingSpecification.objects.create(specDigest="sha256:" + "c" * 64, payload={})
        cls.inputs = []
        for index in range(4):
            identity = {
                "ownerKind": "sourceObject", "ownerId": f"source_{index}",
                "generation": 1, "sha256": "sha256:" + "a" * 64,
            }
            representation = DerivedRepresentation.objects.create(
                representationId=f"representation_{index}",
                ownerKind=identity["ownerKind"], ownerId=identity["ownerId"],
                ownerContentGeneration=identity["generation"], ownerSha256=identity["sha256"],
                processingSpecification=cls.spec, pageCount=1,
                canonicalTextKey=f"private/canonical_{index}", canonicalTextSizeBytes=20_000,
                canonicalTextSha256="sha256:" + "b" * 64,
                manifest={"pages": [{"text": "manifest text" * 200}]},
            )
            for ordinal, prefix in enumerate(("z", "a", "b")):
                content = "policy\n" + "😀" * 3_000 if prefix != "b" else "unrelated"
                KnowledgeSegment.objects.create(
                    segmentId=f"{prefix}_doc_{index}", representation=representation,
                    ordinal=ordinal, boundedText=content, textSha256=sha256_bytes(content.encode()),
                    locator={"kind": "textSpan", "startByte": 10, "endByte": 10 + len(content.encode()),
                             "startLine": 2, "endLine": 3, "pageStart": 1, "pageEnd": 1},
                )
            if index < 3:
                cls.inputs.append(BoundInput(
                    resolved={
                        "inputRef": f"input_{index}", "displayName": f"document_{index}",
                        "objectRef": identity["ownerId"], "ownerKind": identity["ownerKind"],
                        "evidenceKind": "workspaceSource", "sha256": identity["sha256"],
                        "citationAllowed": index != 1,
                    },
                    content_source=None,
                    owner=SimpleNamespace(updatedAt=datetime(2026, 1, index + 1, tzinfo=UTC)),
                    input_identity=identity, representation_id=representation.representationId,
                ))

    def search(self, **options):
        access = material_access.MaterialAccess(object(), {}, self.spec.pk, "digest")
        with patch.object(material_access, "bind_inputs", return_value=self.inputs):
            return material_reads.search_materials(
                access, [], query="policy", ranking=options.pop("ranking", "relevance"),
                date_range=options.pop("date_range", None), limit=options.pop("limit", 20), **options,
            )

    def test_search_decodes_each_authorized_manifest_once_and_preserves_evidence(self):
        manifest = DerivedRepresentation._meta.get_field("manifest")
        with patch.object(manifest, "from_db_value", wraps=manifest.from_db_value) as decoded:
            with self.assertNumQueries(2):
                result = self.search(limit=4)
        self.assertEqual(decoded.call_count, 3)
        self.assertEqual([hit["segmentId"] for hit in result["hits"]], ["a_doc_2", "z_doc_2", "a_doc_1", "z_doc_1"])
        hit = result["hits"][0]
        self.assertEqual((hit["inputRef"], hit["ownerRef"], hit["representationId"], hit["specDigest"]),
                         ("input_2", "source_2", "representation_2", self.spec.pk))
        self.assertEqual(hit["score"], 3)
        self.assertEqual(hit["evidenceSha256"], sha256_bytes(hit["content"].encode()))
        self.assertEqual(hit["locator"]["endByte"], 10 + len(hit["content"].encode()))
        self.assertTrue(hit["citationAllowed"])
        self.assertFalse(result["hits"][2]["citationAllowed"])
        self.assertLessEqual(len(hit["content"].encode()), 8 * 1024)

    def test_recent_date_filter_and_full_window_keep_the_authorized_scope(self):
        result = self.search(
            ranking="recent", full_window=True,
            date_range={"updatedFrom": "2026-01-02T00:00:00Z", "updatedTo": "2026-01-03T00:00:00Z"},
        )
        self.assertEqual([hit["segmentId"] for hit in result["hits"]], ["a_doc_2", "z_doc_2", "a_doc_1", "z_doc_1"])
        self.assertEqual(result["hits"][0]["content"], "policy\n" + "😀" * 3_000)
        self.assertEqual(result["hits"][0]["locator"]["endByte"], 10 + len(result["hits"][0]["content"].encode()))
