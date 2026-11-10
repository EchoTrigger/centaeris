from unittest.mock import patch

from django.core.files.uploadedfile import SimpleUploadedFile
from django.test import SimpleTestCase

from app_core import assets


class UploadFailureContractTests(SimpleTestCase):
    def test_save_and_cleanup_failure_preserve_outer_code_and_cause(self):
        save_error = OSError("save failed")
        cleanup_error = OSError("cleanup failed")
        with patch.object(assets.default_storage, "save", side_effect=save_error), patch.object(assets, "delete_stored_object_for_gc", side_effect=cleanup_error):
            with self.assertRaisesRegex(RuntimeError, "^upload_storage_cleanup_failed$") as raised:
                assets.store_upload(SimpleUploadedFile("fixture.txt", b"test"), "test")
        self.assertIs(raised.exception.__cause__, cleanup_error)
        self.assertIs(cleanup_error.__context__, save_error)

    def test_successful_cleanup_preserves_original_save_failure(self):
        error = OSError("save failed")
        with patch.object(assets.default_storage, "save", side_effect=error), patch.object(assets, "delete_stored_object_for_gc") as cleanup:
            with self.assertRaises(OSError) as raised:
                assets.store_upload(SimpleUploadedFile("fixture.txt", b"test"), "test")
        self.assertIs(raised.exception, error)
        cleanup.assert_called_once()
