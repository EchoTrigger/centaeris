"""The root Session authorizes durable child Agent presentation reads."""
import urllib.error
from unittest.mock import patch

from django.contrib.auth.models import User
from django.test import TestCase
from django.utils import timezone

from .models import Workspace
from .testing import create_session


class SessionSubagentReadTests(TestCase):
    def setUp(self):
        self.owner = User.objects.create_user(username="subagent-owner")
        self.workspace = Workspace.objects.create(name="Child Agents", createdBy=self.owner)
        self.workspace.members.add(self.owner)
        self.session = create_session(workspace=self.workspace, owner=self.owner)
        self.client.force_login(self.owner)
        self.url = f"/api/sessions/{self.session.id}/subagents"
        self.agent = {
            "runtimeJobId": "subagent.run:child", "callId": "call-agent",
            "childSessionId": "runtime-child", "parentAgentRunId": "parent-run",
            "parentTurnId": "parent-turn", "description": "Read source",
            "prompt": "Inspect README.md", "status": "succeeded",
            "createdAtMs": 1, "updatedAtMs": 2,
        }
        self.list_receipt = {
            "schema": "workspace.session.subagents.v1", "sessionId": self.session.id,
            "agents": [self.agent],
        }

    def test_history_list_is_scoped_by_parent_owner_and_workspace(self):
        with patch("app_core.session_subagents.request_json", return_value=self.list_receipt) as runtime:
            response = self.client.get(self.url)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), self.list_receipt)
        self.assertEqual(response["Cache-Control"], "no-store")
        self.assertEqual(runtime.call_args.args, ("/internal/sessions/subagents", {
            "schema": "runtime.session.subagents.read.v1", "sessionId": self.session.id,
            "workspaceId": self.workspace.id, "ownerId": self.owner.id,
        }))

    def test_child_content_uses_job_identity_without_a_platform_child_session(self):
        message = {
            "messageId": "child-message", "role": "assistant", "content": "Read complete",
            "createdAtMs": 2, "reasoningContent": None, "toolCalls": [],
            "toolCallId": None, "toolName": None, "resultState": None,
        }
        receipt = {"schema": "workspace.session.subagent.v1", "sessionId": self.session.id,
            "agent": self.agent, "messages": [message]}
        with patch("app_core.session_subagents.request_json", return_value=receipt) as runtime:
            response = self.client.get(self.url + "/subagent.run%3Achild")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), receipt)
        self.assertEqual(runtime.call_args.args[1]["runtimeJobId"], "subagent.run:child")
        self.assertEqual(response["Cache-Control"], "no-store")

    def test_membership_revocation_and_other_owner_cannot_read_child_content(self):
        with patch("app_core.session_subagents.request_json") as runtime:
            self.workspace.members.remove(self.owner)
            self.assertEqual(self.client.get(self.url).status_code, 404)
            self.workspace.members.add(self.owner)
            stranger = User.objects.create_user(username="subagent-stranger")
            self.workspace.members.add(stranger)
            self.client.force_login(stranger)
            self.assertEqual(self.client.get(self.url + "/subagent.run%3Achild").status_code, 404)
            runtime.assert_not_called()

    def test_purged_parent_never_reaches_runtime(self):
        self.session.status = "deleted"
        self.session.deletedAt = timezone.now()
        self.session.purgedAt = timezone.now()
        self.session.save(update_fields=["status", "deletedAt", "purgedAt"])
        with patch("app_core.session_subagents.request_json") as runtime:
            self.assertEqual(self.client.get(self.url).status_code, 404)
            runtime.assert_not_called()

    def test_invalid_runtime_receipts_fail_closed(self):
        for change in (
            {"sessionId": "another-parent"}, {"unexpected": True},
            {"schema": "workspace.session.subagents.v0"},
            {"agents": [{**self.agent, "status": "completed"}]},
        ):
            with self.subTest(change=change), patch("app_core.session_subagents.request_json",
                return_value={**self.list_receipt, **change}):
                response = self.client.get(self.url)
                self.assertEqual(response.status_code, 503)
                self.assertEqual(response.json(), {"error": "session_subagent_unavailable"})

    def test_missing_job_and_runtime_timeout_are_not_successful_empty_results(self):
        for error, status in (
            (urllib.error.HTTPError("http://runtime", 404, "missing", {}, None), 404),
            (TimeoutError("runtime timed out"), 503),
        ):
            with self.subTest(status=status), patch("app_core.session_subagents.request_json", side_effect=error):
                self.assertEqual(self.client.get(self.url + "/subagent.run%3Achild").status_code, status)

    def test_query_cannot_select_an_arbitrary_child(self):
        with patch("app_core.session_subagents.request_json") as runtime:
            response = self.client.get(self.url, {"childSessionId": "other-child"})
            self.assertEqual(response.status_code, 400)
            runtime.assert_not_called()
