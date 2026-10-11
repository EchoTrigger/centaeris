"""Query multiplicity, route errors and authorization order over real HTTP routes."""
from django.test import TestCase

from . import test_agent_messages


class HttpQueryContractTests(TestCase):
    setUp = test_agent_messages.AgentMessageTests.setUp

    def test_optional_scalar_queries_reject_unknown_and_repeated_fields(self):
        routes = [
            (f"/api/agents/{self.agent.id}/work-sessions", "limit", "1", "agent_work_sessions_cursor_invalid"),
            (f"/api/agents/{self.agent.id}/history", "limit", "1", "agent_history_cursor_invalid"),
            (f"/api/agents/{self.agent.id}/inputs", "limit", "1", "agent_inputs_cursor_invalid"),
            (f"/api/agents/{self.agent.id}/messages", "limit", "1", "agent_messages_cursor_invalid"),
            (f"/api/workspaces/{self.workspace.id}/sessions", "agentId", self.agent.id, "session_filter_unsupported"),
        ]
        for path, field, value, error in routes:
            for query in ("unknown=1", f"{field}={value}&{field}={value}", f"{field}={value}&{field}="):
                with self.subTest(path=path, query=query):
                    response = self.client.get(f"{path}?{query}")
                    self.assertEqual(response.status_code, 400, response.content)
                    expected = ({"error": "request_invalid", "fields": ["query.limit"]}
                                if field == "limit" and query.endswith("limit=") else {"error": error})
                    self.assertEqual(response.json(), expected)

    def test_required_query_fields_keep_missing_duplicate_and_empty_distinct(self):
        path = f"/api/workspaces/{self.workspace.id}/session-projects"
        for query in ("", "unknown=1", f"agentId={self.agent.id}&agentId={self.agent.id}"):
            response = self.client.get(f"{path}?{query}")
            self.assertEqual(response.status_code, 400)
            self.assertEqual(response.json(), {"error": "session_project_filter_invalid"})
        self.assertEqual(self.client.get(f"{path}?agentId=").json(), {"error": "agent_id_invalid"})
        response = self.client.get(path, {"agentId": self.agent.id})
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {"projects": []})
        for suffix in ("", "/missing-branch/sessions"):
            path = f"/api/agents/{self.agent.id}/business-branches{suffix}"
            for query in ("", "limit=1", "appId=test&appId=test", "appId=test&unknown=1"):
                with self.subTest(path=path, query=query):
                    response = self.client.get(f"{path}?{query}")
                    self.assertEqual(response.status_code, 400)
                    self.assertEqual(response.json(), {"error": "business_branch_request_invalid"})

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
        base = f"/api/sessions/{self.work.id}/transcript"
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
