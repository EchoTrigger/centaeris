"""Characterize existing local primitives before any Artifact backend change."""
import hashlib
from concurrent.futures import ThreadPoolExecutor
from io import BytesIO
from pathlib import Path
import tempfile

from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.test import SimpleTestCase, override_settings

from .artifact_publish import ArtifactPublishError, _HashingReader, verify_stored_object
from .assets import store_immutable_bytes_at_key, delete_stored_object_for_gc


class ArtifactLocalStorageContractTests(SimpleTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="artifact-local-contract-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        settings = override_settings(MEDIA_ROOT=directory.name)
        settings.enable()
        self.addCleanup(settings.disable)

    def test_same_content_converges_without_overwrite_or_temporary_files(self):
        data = bytes(range(256)) * 300
        def upload(_):
            return store_immutable_bytes_at_key(data, "artifacts/immutable.bin")
        with ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(upload, range(4)))
        self.assertEqual(sum(item["created"] for item in results), 1)
        self.assertEqual({item["storageKey"] for item in results}, {"artifacts/immutable.bin"})
        self.assertEqual(list(self.root.rglob("*.tmp")), [])
        verify_stored_object("artifacts/immutable.bin", len(data), results[0]["sha256"])

    def test_conflicting_content_does_not_delete_the_winner(self):
        store_immutable_bytes_at_key(b"AAA", "artifacts/fixed.bin")
        with self.assertRaisesRegex(RuntimeError, "identity_conflict"):
            store_immutable_bytes_at_key(b"BBB", "artifacts/fixed.bin")
        self.assertEqual((self.root / "artifacts/fixed.bin").read_bytes(), b"AAA")

    def test_django_save_is_not_an_immutable_put(self):
        # Artifact currently uses this API, unlike the immutable evidence helper.
        first = default_storage.save("artifacts/result.bin", ContentFile(b"AAA"))
        second = default_storage.save("artifacts/result.bin", ContentFile(b"BBB"))
        self.assertNotEqual(first, second)
        self.assertEqual((self.root / first).read_bytes(), b"AAA")
        self.assertEqual((self.root / second).read_bytes(), b"BBB")

    def test_missing_corrupt_and_deleted_bytes_have_explicit_results(self):
        key = "artifacts/result.bin"
        sha = "sha256:" + hashlib.sha256(b"AAA").hexdigest()
        with self.assertRaisesRegex(ArtifactPublishError, "artifact_storage_missing"):
            verify_stored_object(key, 3, sha)
        default_storage.save(key, ContentFile(b"BBB"))
        with self.assertRaisesRegex(ArtifactPublishError, "artifact_storage_integrity_mismatch"):
            verify_stored_object(key, 3, sha)
        delete_stored_object_for_gc(key)
        delete_stored_object_for_gc(key)
        self.assertFalse(default_storage.exists(key))

    def test_truncated_upload_and_wrong_digest_cannot_verify(self):
        reader = _HashingReader(BytesIO(b"ab"), 3)
        self.assertEqual(reader.read(3), b"ab")
        with self.assertRaisesRegex(ArtifactPublishError, "body_truncated"):
            reader.read(1)
        reader = _HashingReader(BytesIO(b"abc"), 3)
        self.assertEqual(reader.read(3), b"abc")
        with self.assertRaisesRegex(ArtifactPublishError, "integrity_mismatch"):
            reader.require_complete("sha256:" + "0" * 64)
