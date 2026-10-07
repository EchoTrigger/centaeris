"""Characterize document evidence before moving it behind platform MCP."""

from datetime import UTC, datetime
from types import SimpleNamespace
from unittest.mock import patch

from django.test import SimpleTestCase

from . import material_reads as materials
from . import material_contract, material_evidence


class MaterialReadContractTests(SimpleTestCase):
    def setUp(self):
        self.bound = material_contract.BoundInput(
            resolved={
                "virtualPath": "sources/document/report.txt",
                "inputRef": "input_1",
                "displayName": "报告.txt",
                "objectRef": "source_1",
                "ownerKind": "sourceObject",
                "evidenceKind": "workspaceSource",
                "sha256": "sha256:" + "a" * 64,
                "citationAllowed": True,
            },
            storage_key="private/source",
            owner=SimpleNamespace(updatedAt=datetime(2026, 1, 1, tzinfo=UTC)),
            input_identity={"generation": 3},
            representation_id="representation:sha256:" + "b" * 64,
        )

    def read(self, content, *, offset=0, limit=2_000):
        encoded = content.encode("utf-8")
        representation = SimpleNamespace(
            canonicalTextKey="private/canonical.md",
            canonicalTextSizeBytes=len(encoded),
            canonicalTextSha256=material_contract.sha256_bytes(encoded),
            representationId=self.bound.representation_id,
            processingSpecification_id="sha256:" + "c" * 64,
            manifest={"pages": [{
                "canonicalStartByte": 0,
                "canonicalEndByte": len(encoded),
                "pageText": {"page": 2, "route": "ppOcrV6Small"},
            }]},
        )
        with patch.object(materials, "read_stored", return_value=encoded):
            return materials._read_representation(self.bound, representation, offset, limit)

    def test_utf8_page_evidence_has_exact_byte_identity_and_continuation(self):
        result = self.read("第一行😀\nsecond\nthird", limit=1)
        self.assertEqual(result["content"], "第一行😀")
        self.assertEqual(result["outputBytes"], len("第一行😀".encode()))
        self.assertEqual(result["evidenceSha256"], material_contract.sha256_bytes("第一行😀".encode()))
        self.assertEqual(result["locator"], {
            # Locators cover original lines, including their line terminator;
            # evidence hashes cover the returned content without that terminator.
            "kind": "textSpan", "startByte": 0, "endByte": len("第一行😀\n".encode()),
            "startLine": 1, "endLine": 1, "pageStart": 2, "pageEnd": 2,
        })
        self.assertEqual(result["nextOffset"], 1)
        self.assertEqual(result["truncatedBy"], "lines")
        self.assertTrue(result["citationAllowed"])
        self.assertTrue(result["documentUsedOcr"])
        self.assertEqual(result["ownerGeneration"], 3)
        self.assertEqual(result["representationId"], self.bound.representation_id)
        self.assertNotIn("private/", str(result))

    def test_continuation_stops_at_end_without_repeating_lines(self):
        result = self.read("first\n第二行\nlast", offset=1, limit=2)
        self.assertEqual(result["content"], "第二行\nlast")
        self.assertEqual(result["locator"]["startByte"], len("first\n".encode()))
        self.assertEqual(result["locator"]["endByte"], len("first\n第二行\nlast".encode()))
        self.assertFalse(result["truncated"])
        self.assertIsNone(result["truncatedBy"])
        self.assertIsNone(result["nextOffset"])

    def test_empty_and_exhausted_windows_do_not_make_citations(self):
        for content, offset in (("", 0), ("first", 1)):
            with self.subTest(content=content):
                result = self.read(content, offset=offset)
                self.assertEqual(result["content"], "")
                self.assertFalse(result["citationAllowed"])
                self.assertIsNone(result["locator"])
                self.assertIsNone(result["nextOffset"])

    def test_oversized_first_line_does_not_offer_a_nonadvancing_retry(self):
        result = self.read("界" * (material_contract.MAX_READ_BYTES // 3 + 1))
        self.assertEqual(result["content"], "")
        self.assertTrue(result["firstLineExceedsLimit"])
        self.assertEqual(result["truncatedBy"], "bytes")
        self.assertIsNone(result["nextOffset"])
        self.assertFalse(result["citationAllowed"])

    def test_byte_bound_preserves_complete_lines(self):
        line = "x" * (material_contract.MAX_READ_BYTES - 1)
        result = self.read(line + "\n😀")
        self.assertEqual(result["content"], line)
        self.assertEqual(result["nextOffset"], 1)
        self.assertEqual(result["truncatedBy"], "bytes")

    def test_disallowed_citation_does_not_hide_readable_content(self):
        self.bound.resolved["citationAllowed"] = False
        result = self.read("evidence")
        self.assertEqual(result["content"], "evidence")
        self.assertFalse(result["citationAllowed"])

    def test_out_of_range_offset_is_rejected(self):
        with self.assertRaisesRegex(materials.KnowledgeError, "knowledge_read_offset_exceeds_content"):
            self.read("one", offset=2)

    def test_storage_integrity_failure_is_not_returned_as_evidence(self):
        with patch.object(materials, "read_stored", side_effect=materials.KnowledgeError("knowledge_storage_integrity_mismatch")):
            representation = SimpleNamespace(
                canonicalTextKey="private/canonical.md",
                canonicalTextSizeBytes=1,
                canonicalTextSha256="sha256:" + "c" * 64,
            )
            with self.assertRaisesRegex(materials.KnowledgeError, "knowledge_storage_integrity_mismatch"):
                materials._read_representation(self.bound, representation, 0, 1)

    def test_search_snippet_is_utf8_bounded_and_keeps_source_identity(self):
        segment = SimpleNamespace(
            boundedText="😀" * 3_000,
            segmentId="segment_1",
            representation_id=self.bound.representation_id,
            representation=SimpleNamespace(processingSpecification_id="sha256:" + "c" * 64),
            locator={"kind": "textSpan", "startByte": 10, "endByte": 12_010,
                     "startLine": 2, "endLine": 2, "pageStart": 2, "pageEnd": 2},
        )
        result = material_evidence.search_evidence(segment, self.bound, 3)
        self.assertLessEqual(len(result["content"].encode()), material_contract.MAX_SEARCH_SNIPPET_BYTES)
        self.assertEqual(result["locator"]["endByte"], 10 + len(result["content"].encode()))
        self.assertEqual(result["evidenceSha256"], material_contract.sha256_bytes(result["content"].encode()))
        self.assertEqual(result["ownerRef"], "source_1")
        self.assertEqual(segment.locator["endByte"], 12_010)
