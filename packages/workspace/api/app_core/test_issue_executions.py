import json
from unittest.mock import patch

from django.contrib.auth.models import User
from django.db import DatabaseError
from django.test import Client, TestCase
from django.utils import timezone

from app_core.models import (
    AgentRun, HostedOperationReceipt, Issue, IssueExecution, ModelConfig,
    RunMaterialScope, Session, Workspace, WorkspaceMembership,
)
from app_core.test_hosted_operations import PROFILE


class IssueExecutionTests(TestCase):
    def setUp(self):
        self.user = User.objects.create_user(username="issue-executor")
        self.other = User.objects.create_user(username="other-executor")
        self.workspace = Workspace.objects.create(name="Work", createdBy=self.user)
        self.workspace.members.add(self.user, self.other)
        self.model = ModelConfig.objects.create(displayName="Model")
        self.issue = Issue.objects.create(workspace=self.workspace, creator=self.user,
            title="修复😀" * 80, description="Inspect the actual implementation.")
        self.url = f"/api/workspaces/{self.workspace.id}/issues/{self.issue.id}/executions"
        self.client.force_login(self.user)
        for name, value in (("request_execution_profile", PROFILE), ("schedule_agent_run_lifecycle", "inserted")):
            mock = patch(f"app_core.http.workspaces.{name}", return_value=value)
            setattr(self, name, mock.start())
            self.addCleanup(mock.stop)

    def post(self, **changes):
        return self.client.post(self.url, json.dumps({"operationId": "execute-1",
            "version": 1, "modelConfigRef": self.model.id, **changes}), content_type="application/json")

    def accept(self, **changes):
        response = self.post(**changes)
        self.assertEqual(response.status_code, 202, response.content)
        return response.json()

    def test_real_run_snapshot_receipt_personal_library_and_navigation(self):
        receipt = self.accept()
        run = AgentRun.objects.get(pk=receipt["agentRunId"])
        link = IssueExecution.objects.get(agent_run=run)
        self.assertEqual(receipt, {"operationId": "execute-1", "command": "submitMessage",
            "status": "accepted", "sessionId": run.session_id, "agentRunId": run.id, "turnId": run.turn_id})
        self.assertEqual(run.prompt, f"{self.issue.title}\n\n{self.issue.description}")
        self.assertEqual((link.issueVersion, link.issueTitle, link.issueDescription, link.issuer_id),
            (1, self.issue.title, self.issue.description, self.user.pk))
        self.assertEqual((run.session.owner_id, run.session.purpose, run.session.title),
            (self.user.pk, "interactive", self.issue.title[:200]))
        self.assertTrue(RunMaterialScope.objects.get(agent_run=run).personal_library)
        sessions = self.client.get(f"/api/workspaces/{self.workspace.id}/sessions")
        self.assertEqual(sessions.status_code, 200, sessions.content)
        self.assertIn(run.session_id, [item["id"] for item in sessions.json()["sessions"]])
        self.issue.refresh_from_db()
        self.assertEqual((self.issue.status, self.issue.version), ("todo", 1))
        self.schedule_agent_run_lifecycle.assert_called_once()

    def test_replay_ignores_mutable_issue_and_model_but_rejects_changed_input(self):
        receipt = self.accept()
        Issue.objects.filter(pk=self.issue.pk).update(title="Changed", description="Changed", version=2,
            archivedAt=timezone.now())
        self.model.enabled = False
        self.model.save(update_fields=["enabled"])
        self.request_execution_profile.side_effect = RuntimeError("offline")
        self.assertEqual(self.accept(), receipt)
        for changes in ({"version": 2}, {"modelConfigRef": "different"}, {"thinkingMode": "different"}):
            result = self.post(**changes)
            self.assertEqual(result.status_code, 409, result.content)
            self.assertEqual(result.json()["error"], "operation_conflict")
        self.assertEqual(IssueExecution.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.assertEqual(Session.objects.count(), 1)
        self.schedule_agent_run_lifecycle.assert_called_once()

    def test_membership_revocation_and_cross_workspace_access(self):
        self.accept()
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).delete()
        self.assertEqual(self.post().status_code, 404)
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.client.force_login(self.other)
        other_workspace = Workspace.objects.create(name="Other", createdBy=self.other)
        other_workspace.members.add(self.other)
        self.assertEqual(self.client.get(self.url.replace(self.workspace.id, other_workspace.id)).status_code, 404)

    def test_version_archive_and_strict_input_without_orphans(self):
        self.assertEqual(self.post(version=2).status_code, 409)
        for fields in ({"version": "1"}, {"model_config_ref": self.model.id}, {"extra": True},
                       {"operationId": "bad id"}):
            self.assertEqual(self.post(**fields).status_code, 400)
        Issue.objects.filter(pk=self.issue.pk).update(archivedAt=timezone.now())
        response = self.post()
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "issue_archived")
        self.assertFalse(Session.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())

    def test_failed_link_or_authorization_rolls_back_all_records(self):
        from app_core.http.workspaces import AgentSessionCreationError
        for target, error in (("app_core.http.issue_executions.IssueExecution.objects.create", DatabaseError("link failed")),
            ("app_core.http.workspaces.create_agent_run_authorization", AgentSessionCreationError(403, "denied"))):
            with self.subTest(target=target), patch(target, side_effect=error):
                client = Client(raise_request_exception=False)
                client.force_login(self.user)
                result = client.post(self.url, json.dumps({"operationId": "execute-1", "version": 1,
                    "modelConfigRef": self.model.id}), content_type="application/json")
                self.assertEqual(result.status_code, 500 if isinstance(error, DatabaseError) else 403)
            self.assertFalse(Session.objects.exists())
            self.assertFalse(AgentRun.objects.exists())
            self.assertFalse(HostedOperationReceipt.objects.exists())
            self.assertFalse(IssueExecution.objects.exists())
        self.schedule_agent_run_lifecycle.assert_not_called()

    def test_list_is_private_paginated_and_uses_actual_run_status(self):
        first = self.accept()
        self.client.force_login(self.other)
        other = self.accept()
        self.client.force_login(self.user)
        second = self.accept(operationId="execute-2")
        AgentRun.objects.filter(pk=first["agentRunId"]).update(status="completed")
        data = self.client.get(self.url + "?limit=1&offset=1").json()
        self.assertEqual((data["total"], len(data["executions"])), (2, 1))
        items = self.client.get(self.url).json()["executions"]
        self.assertEqual({item["sessionId"] for item in items}, {first["sessionId"], second["sessionId"]})
        self.assertNotIn(other["sessionId"], str(items))
        self.assertEqual(next(item["status"] for item in items if item["sessionId"] == first["sessionId"]), "completed")
        Session.objects.filter(pk=first["sessionId"]).update(archivedAt=timezone.now())
        self.assertEqual(self.client.get(self.url).json()["total"], 1)
        self.assertEqual(self.post().status_code, 410)
        self.assertEqual(self.client.get(self.url + "?extra=true").status_code, 400)

    def test_csrf_is_required(self):
        client = Client(enforce_csrf_checks=True)
        client.force_login(self.user)
        result = client.post(self.url, json.dumps({"operationId": "csrf", "version": 1,
            "modelConfigRef": self.model.id}), content_type="application/json")
        self.assertEqual(result.status_code, 403, result.content)

    def test_profile_failure_creates_nothing_and_schedule_failure_keeps_receipt(self):
        self.request_execution_profile.side_effect = RuntimeError("offline")
        self.assertEqual(self.post().status_code, 503)
        self.assertFalse(Session.objects.exists())
        self.request_execution_profile.side_effect = None
        self.schedule_agent_run_lifecycle.side_effect = RuntimeError("queue offline")
        receipt = self.accept()
        run = AgentRun.objects.get(pk=receipt["agentRunId"])
        self.assertEqual(run.transitionReason, "agent_run_lifecycle_schedule_pending")
        self.assertEqual(self.accept(), receipt)
        self.schedule_agent_run_lifecycle.assert_called_once()
