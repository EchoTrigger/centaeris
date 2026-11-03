"""Query multiplicity, route errors and authorization order over real HTTP routes."""
from django.test import TestCase

from .session_record_test_fixtures import SessionRecordFixture


class HttpQueryContractTests(SessionRecordFixture, TestCase):

    def test_session_list_rejects_unknown_and_repeated_filters(self):
        path = f"/api/workspaces/{self.workspace.pk}/sessions"
        for query in ("unknown=1", "sessionId=one&sessionId=two", "sessionId="):
            response = self.client.get(f"{path}?{query}")
            self.assertEqual(response.status_code, 400, response.content)
            self.assertEqual(response.json(), {"error": "session_filter_unsupported"})

    def test_workspace_projects_accept_no_filters(self):
        path = f"/api/workspaces/{self.workspace.pk}/session-projects"
        for query in ("unknown=1", "projectId=one&projectId=two", "projectId="):
            response = self.client.get(f"{path}?{query}")
            self.assertEqual(response.status_code, 400, response.content)
            self.assertEqual(response.json(), {"error": "session_project_filter_unsupported"})
        response = self.client.get(path)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), {"projects": []})

    def test_authorization_still_precedes_query_rejection(self):
        path = f"/api/workspaces/{self.workspace.id}/session-projects?unknown=1"
        self.client.logout()
        self.assertEqual(self.client.get(path).status_code, 401)
        self.client.force_login(self.user)
        self.membership.delete()
        response = self.client.get(path)
        self.assertEqual(response.status_code, 404)
        self.assertEqual(response.json(), {"error": "workspace_not_found"})

    def test_transcript_scalar_queries_and_repeatable_sequences_keep_distinct_contracts(self):
        base = f"/api/sessions/{self.session.id}/transcript"
        for suffix, query, code in [
            ("", "sourceHighWater=0&sourceHighWater=0&projectionGeneration=test", "transcript_page_query_invalid"),
            ("/patches", "afterSourceHighWater=0&projectionGeneration=test&projectionGeneration=test", "transcript_patch_query_invalid"),
            ("/patches", "afterSourceHighWater=0", "transcript_patch_query_invalid"),
            ("/content", "projectionGeneration=test&refId=tool-output:test&revision=0&byteLength=0&offset=0&offset=0", "transcript_content_query_invalid"),
            ("/active-agent-run", "sourceHighWater=0&sourceHighWater=0", "transcript_active_agent_run_query_invalid"),
        ]:
            with self.subTest(suffix=suffix, query=query):
                response = self.client.get(f"{base}{suffix}?{query}")
                self.assertEqual(response.status_code, 400, response.content)
                self.assertEqual(response.json(), {"error": code})
        response = self.client.get(f"{base}/turn-metadata?sequence=1&sequence=2")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), {"turns": []})
        response = self.client.get(f"{base}/citations?sequence=1&sequence=2")
        self.assertEqual(response.status_code, 200, response.content)
