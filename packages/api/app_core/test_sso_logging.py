import logging
from unittest import TestCase

from app_core.sso_logging import OAuthCallbackFilter


class OAuthCallbackLoggingTests(TestCase):
    def test_uvicorn_callback_query_is_removed_before_formatting(self):
        record = logging.LogRecord("uvicorn.access", logging.INFO, "", 0,
            '%s - "%s %s HTTP/%s" %d',
            ("client", "GET", "/api/sso/test-oauth/callback?code=private-code&state=private-state", "1.1", 302), None)
        self.assertTrue(OAuthCallbackFilter().filter(record))
        self.assertIn("/api/sso/test-oauth/callback HTTP/1.1", record.getMessage())
        self.assertNotIn("private-", record.getMessage())

    def test_ordinary_access_records_are_unchanged(self):
        record = logging.LogRecord("uvicorn.access", logging.INFO, "", 0,
            '%s - "%s %s HTTP/%s" %d', ("client", "GET", "/api/me", "1.1", 200), None)
        original = record.getMessage()
        OAuthCallbackFilter().filter(record)
        self.assertEqual(record.getMessage(), original)
