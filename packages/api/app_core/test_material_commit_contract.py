"""Characterize document commit storage effects before service extraction."""

import json
from contextlib import nullcontext
from io import BytesIO
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest.mock import patch

from django.core.files.storage import FileSystemStorage
from django.test import SimpleTestCase

from . import material_access, material_commit, material_storage
from .material_contract import BoundInput, KnowledgeError, sha256_bytes


class MaterialCommitContractTests(SimpleTestCase):
    def setUp(self):
        temporary = TemporaryDirectory(prefix="material-commit-test-")
        self.addCleanup(temporary.cleanup)
        self.storage = FileSystemStorage(location=temporary.name)
        self.content = "证据😀".encode()
        self.preview = b"preview"
        self.representation_id = "representation:sha256:" + "a" * 64
        self.canonical_key = "knowledge/" + "a" * 64 + "/canonical.md"
        self.preview_key = "knowledge/" + "a" * 64 + "/preview.pdf"
        self.spec = {"testSpecification": True}
        self.access = material_access.MaterialAccess(object(), self.spec, "sha256:" + "b" * 64, "digest")
        self.bound = BoundInput(
            {"inputRef": "input_1"}, "source/key", object(),
            {"ownerKind": "sourceObject", "ownerId": "source_1", "generation": 2, "sha256": sha256_bytes(b"source")},
            self.representation_id,
        )
        self.metadata = {
            "schema": "knowledge.processing.commit.v1",
            "jobId": "knowledge.process:" + "a" * 64,
            "agentRunId": "run_1", "authorizationDigest": "digest", "inputRef": "input_1",
            "representationId": self.representation_id,
            "processingSpecification": self.spec, "specDigest": self.access.spec_digest,
            "canonicalSizeBytes": len(self.content), "canonicalSha256": sha256_bytes(self.content),
            "previewSizeBytes": len(self.preview), "previewSha256": sha256_bytes(self.preview),
            "manifest": {"schema": "knowledge.derived_manifest.v1", "pageCount": 1, "pages": [{
                "pageText": {"schema": "knowledge.page_text.v1", "page": 1, "route": "nativeText",
                    "widthMillipoints": 1000, "heightMillipoints": 1000,
                    "text": self.content.decode(), "textSha256": sha256_bytes(self.content),
                    "spans": [{"text": self.content.decode(), "bbox": [0, 0, 10000, 10000]}]},
                "canonicalStartByte": 0, "canonicalEndByte": len(self.content),
                "canonicalStartLine": 1, "canonicalEndLine": 1,
            }]},
        }
        self.row = SimpleNamespace(
            representationId=self.representation_id, processingSpecification_id=self.access.spec_digest,
            ownerKind="sourceObject", ownerId="source_1", ownerContentGeneration=2,
            ownerSha256=self.bound.input_identity["sha256"], pageCount=1,
            canonicalTextKey=self.canonical_key, canonicalTextSizeBytes=len(self.content),
            canonicalTextSha256=sha256_bytes(self.content), previewPdfKey=self.preview_key,
            previewPdfSizeBytes=len(self.preview), previewPdfSha256=sha256_bytes(self.preview),
            workbookPreviewKey="", workbookPreviewSizeBytes=0, workbookPreviewSha256="",
            manifest=self.metadata["manifest"],
        )
        patches = {
            "authorization": patch.object(material_access, "authorize_material_access", return_value=self.access),
            "bindings": patch.object(material_access, "bind_inputs", return_value=[self.bound]),
            "storage_patch": patch.object(material_storage, "default_storage", self.storage),
            "read_storage_patch": patch.object(material_storage, "delete_stored_object", side_effect=self.storage.delete),
            "existing": patch.object(material_commit.DerivedRepresentation.objects, "filter"),
            "create": patch.object(material_commit.DerivedRepresentation.objects, "create", return_value=self.row),
            "specification": patch.object(material_commit.ProcessingSpecification.objects, "get_or_create", return_value=(SimpleNamespace(payload=self.spec), True)),
            "segments": patch.object(material_commit.KnowledgeSegment.objects, "bulk_create"),
            "resources": patch.object(material_commit, "register_derived_resource"),
            "delete": patch.object(material_commit, "delete_stored_object", side_effect=self.storage.delete),
            "atomic": patch.object(material_commit.transaction, "atomic", side_effect=nullcontext),
            "segment_model": patch.object(material_commit, "KnowledgeSegment", side_effect=lambda **fields: fields),
        }
        for name, patcher in patches.items():
            setattr(self, name, patcher.start())
            self.addCleanup(patcher.stop)
        self.existing.return_value.first.return_value = None

    def commit(self, payload=None):
        commit = material_commit.ProcessingOutput(
            self.metadata["representationId"], self.metadata["canonicalSizeBytes"],
            self.metadata["canonicalSha256"], self.metadata["previewSizeBytes"],
            self.metadata["previewSha256"], self.metadata["manifest"])
        return material_commit.commit_bound_material(
            self.bound, self.spec, self.access.spec_digest, commit,
            BytesIO(self.content + self.preview if payload is None else payload))

    def test_success_persists_exact_canonical_and_preview_bytes(self):
        result = self.commit()
        self.assertEqual(result, {"representationId": self.representation_id, "specDigest": self.access.spec_digest, "pageCount": 1})
        with self.storage.open(self.canonical_key) as stream:
            self.assertEqual(stream.read(), self.content)
        with self.storage.open(self.preview_key) as stream:
            self.assertEqual(stream.read(), self.preview)
        self.assertEqual(self.resources.call_count, 2)

    def test_replay_consumes_and_verifies_bytes_without_inserting_again(self):
        first = self.commit()
        self.create.reset_mock()
        self.existing.return_value.first.return_value = self.row
        self.assertEqual(self.commit(), first)
        self.create.assert_not_called()
        self.assertEqual(self.resources.call_count, 2)

    def test_wrong_representation_is_rejected_before_storage(self):
        self.metadata["representationId"] = "representation:sha256:" + "f" * 64
        with self.assertRaisesRegex(KnowledgeError, "material_task_identity_mismatch"):
            self.commit()
        self.assertFalse(self.storage.exists(self.canonical_key))

    def test_manifest_failure_removes_both_new_objects(self):
        self.metadata["manifest"]["pageCount"] = 2
        with self.assertRaisesRegex(KnowledgeError, "knowledge_manifest_page_count_invalid"):
            self.commit()
        self.assertFalse(self.storage.exists(self.canonical_key))
        self.assertFalse(self.storage.exists(self.preview_key))
        self.create.assert_not_called()

    def test_database_failure_removes_new_objects(self):
        self.create.side_effect = RuntimeError("database failed")
        with self.assertRaisesRegex(RuntimeError, "database failed"):
            self.commit()
        self.assertFalse(self.storage.exists(self.canonical_key))
        self.assertFalse(self.storage.exists(self.preview_key))

    def test_failed_replay_does_not_delete_preexisting_objects(self):
        self.commit()
        self.existing.return_value.first.return_value = self.row
        self.row.ownerContentGeneration += 1
        with self.assertRaisesRegex(KnowledgeError, "knowledge_representation_identity_conflict"):
            self.commit()
        self.assertTrue(self.storage.exists(self.canonical_key))
        self.assertTrue(self.storage.exists(self.preview_key))
        self.delete.assert_not_called()

    def test_truncated_stream_is_rejected_without_publishing_representation(self):
        with self.assertRaises(KnowledgeError):
            self.commit(self.content[:-1])
        self.create.assert_not_called()
        self.resources.assert_not_called()
        # A failed storage.save has ambiguous ownership. The platform's durable
        # staging intent owns partial-object recovery; this shared helper must
        # not blindly delete a possibly concurrent writer's object.

    def test_no_preview_uses_no_preview_storage_object(self):
        self.metadata.update(previewSizeBytes=0, previewSha256=None)
        self.commit(self.content)
        self.assertFalse(self.storage.exists(self.preview_key))
        self.assertEqual(self.resources.call_count, 1)

    def assert_hash_failure_cleans_new_objects(self, canonical, preview):
        with self.assertRaisesRegex(KnowledgeError, "knowledge_commit_integrity_mismatch"):
            self.commit(canonical + preview)
        self.assertFalse(self.storage.exists(self.canonical_key))
        self.assertFalse(self.storage.exists(self.preview_key))

    def test_canonical_hash_failure_removes_new_storage_objects(self):
        self.assert_hash_failure_cleans_new_objects(b"x" * len(self.content), self.preview)

    def test_preview_hash_failure_removes_new_storage_objects(self):
        self.assert_hash_failure_cleans_new_objects(self.content, b"x" * len(self.preview))

    def test_service_accepts_a_plain_stream_without_http_metadata(self):
        commit = material_commit.ProcessingOutput(
            representation_id=self.representation_id,
            canonical_size_bytes=len(self.content), canonical_sha256=sha256_bytes(self.content),
            preview_size_bytes=len(self.preview), preview_sha256=sha256_bytes(self.preview),
            manifest=self.metadata["manifest"],
        )
        result = material_commit.commit_bound_material(self.bound, self.spec, self.access.spec_digest, commit, BytesIO(self.content + self.preview))
        self.assertEqual(result, {"representationId": self.representation_id,
            "specDigest": self.access.spec_digest, "pageCount": 1})
        self.authorization.assert_not_called()

    def test_service_rejects_invalid_payload_before_storage(self):
        commit = material_commit.ProcessingOutput(
            representation_id=self.representation_id,
            canonical_size_bytes=True, canonical_sha256=sha256_bytes(self.content),
            preview_size_bytes=0, preview_sha256=None, manifest=self.metadata["manifest"],
        )
        with self.assertRaisesRegex(KnowledgeError, "knowledge_commit_size_invalid"):
            material_commit.commit_bound_material(self.bound, self.spec, self.access.spec_digest, commit, BytesIO(self.content))
        self.bindings.assert_not_called()
        self.assertFalse(self.storage.exists(self.canonical_key))
