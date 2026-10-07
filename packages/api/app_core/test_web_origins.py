import os
from pathlib import Path
import runpy
from unittest.mock import patch

from django.test import SimpleTestCase


class WebOriginSettingsTests(SimpleTestCase):
    def load(self, extra):
        with patch.dict(os.environ, {"WEB_ORIGIN": "http://localhost:6767", "WEB_EXTRA_ORIGINS": extra}):
            return runpy.run_path(str(Path(__file__).resolve().parents[1] / "api/settings.py"))

    def test_both_explicit_origins_share_cors_and_csrf_policy(self):
        settings = self.load("http://127.0.0.1:6767")
        expected = ["http://localhost:6767", "http://127.0.0.1:6767"]
        self.assertEqual(settings["CORS_ALLOWED_ORIGINS"], expected)
        self.assertEqual(settings["CSRF_TRUSTED_ORIGINS"], expected)
        self.assertEqual(settings["WEB_ORIGIN"], expected[0])

    def test_empty_extra_preserves_the_single_canonical_origin(self):
        self.assertEqual(self.load("")["CORS_ALLOWED_ORIGINS"], ["http://localhost:6767"])

    def test_wildcards_paths_credentials_and_empty_entries_fail_closed(self):
        for value in ["*", "http://*.example.test", "http://localhost:6767/path",
                      "http://user@localhost:6767", "http://localhost:6767,"]:
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                self.load(value)
