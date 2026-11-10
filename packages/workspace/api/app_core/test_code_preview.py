from django.test import SimpleTestCase

from .http.downloads import _is_code_preview


class CodePreviewClassificationTests(SimpleTestCase):
    def test_source_names_override_opaque_upload_content_types(self):
        self.assertTrue(_is_code_preview("src/preview.tsx", "application/octet-stream"))
        self.assertTrue(_is_code_preview("Dockerfile", "application/octet-stream"))
        self.assertTrue(_is_code_preview("query", "application/json; charset=utf-8"))
        for filename, content_type in [
            ("main.go", "text/x-go"),
            ("index.php", "application/x-httpd-php"),
            ("config.yaml", "application/yaml"),
            ("document.xml", "application/xml"),
        ]:
            with self.subTest(filename=filename):
                self.assertTrue(_is_code_preview(filename, content_type))

    def test_documents_and_generic_binary_files_are_not_source_code(self):
        for filename in ["report.docx", "slides.pptx", "archive.zip", "image.png"]:
            with self.subTest(filename=filename):
                self.assertFalse(_is_code_preview(filename, "application/octet-stream"))
