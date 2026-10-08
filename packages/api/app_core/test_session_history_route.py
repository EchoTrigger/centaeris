"""The transcript page contract does not publish a full-session history route."""

from django.test import SimpleTestCase


class SessionHistoryRouteTests(SimpleTestCase):
    def test_full_session_history_is_not_an_http_endpoint(self):
        response = self.client.get("/api/sessions/synthetic-session/history")
        self.assertEqual(response.status_code, 404)
