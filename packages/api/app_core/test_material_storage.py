"""Storage integrity must be checked before evidence leaves the platform."""

from io import BytesIO
from unittest.mock import patch

from django.test import SimpleTestCase

from . import material_storage
from .material_contract import KnowledgeError, sha256_bytes


class MaterialStorageTests(SimpleTestCase):
    def test_exact_verified_content_is_returned(self):
        content = "证据😀".encode()
        with patch.object(material_storage, "default_storage") as storage:
            storage.exists.return_value = True
            storage.open.return_value = BytesIO(content)
            self.assertEqual(material_storage.read_stored("private/key", len(content), sha256_bytes(content)), content)

    def test_missing_object_fails_before_read(self):
        with patch.object(material_storage, "default_storage") as storage:
            storage.exists.return_value = False
            with self.assertRaisesRegex(KnowledgeError, "knowledge_storage_missing"):
                material_storage.read_stored("private/key", 3, sha256_bytes(b"abc"))
            storage.open.assert_not_called()

    def test_short_long_and_same_length_corruption_are_rejected(self):
        for content in (b"ab", b"abcd", b"xyz"):
            with self.subTest(content=content), patch.object(material_storage, "default_storage") as storage:
                storage.exists.return_value = True
                storage.open.return_value = BytesIO(content)
                with self.assertRaisesRegex(KnowledgeError, "knowledge_storage_integrity_mismatch"):
                    material_storage.read_stored("private/key", 3, sha256_bytes(b"abc"))
