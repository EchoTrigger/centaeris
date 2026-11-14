import json
from unittest.mock import patch

from django.contrib.auth.models import User
from django.db import DatabaseError, connection
from django.test import Client, TestCase
from django.utils import timezone

from app_core.models import (
    AgentRun, AgentRunEnvironmentRelease, HostedOperationReceipt, Issue, IssueExecution, ModelConfig,
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

    def test_optional_instructions_are_frozen_and_replay_requires_the_same_request(self):
        accepted = self.accept(instructions="  Inspect only the public API.  ")
        run = AgentRun.objects.get(pk=accepted["agentRunId"])
        self.assertEqual(run.prompt, f"{self.issue.title}\n\n{self.issue.description}\n\nInspect only the public API.")
        self.assertEqual(self.accept(instructions="Inspect only the public API."), accepted)
        self.assertEqual(self.post(instructions="Different work").status_code, 409)
        self.assertEqual(self.post(operationId="invalid", instructions="x" * 8193).status_code, 400)
        self.assertEqual(IssueExecution.objects.count(), 1)

    def test_followup_reuses_the_session_and_list_reports_the_latest_run(self):
        accepted = self.accept()
        first = AgentRun.objects.get(pk=accepted["agentRunId"])
        AgentRun.objects.filter(pk=first.pk).update(status="completed")
        AgentRunEnvironmentRelease.objects.create(agent_run=first, environment=first.environment,
            authorization_digest=first.authorization.digest, controller_epoch=1,
            controller_lease_digest="sha256:" + "a" * 64)
        followup = self.client.post(f"/api/workspaces/{self.workspace.pk}/sessions/{first.session_id}/messages",
            json.dumps({"operationId": "followup", "text": "Compare the revised requirements.",
                "modelConfigRef": self.model.pk}), content_type="application/json")
        self.assertEqual(followup.status_code, 202, followup.content)
        self.assertEqual(followup.json()["sessionId"], first.session_id)
        self.assertNotEqual(followup.json()["agentRunId"], first.pk)
        row = self.client.get(self.url).json()["executions"][0]
        self.assertEqual((row["agentRunId"], row["latestAgentRunId"], row["status"]),
            (first.pk, followup.json()["agentRunId"], "queued"))
        self.assertFalse(row["waitingForInput"])
        self.assertEqual((Session.objects.count(), IssueExecution.objects.count()), (1, 1))
        self.issue.refresh_from_db()
        self.assertEqual((self.issue.assignee_id, self.issue.status), (None, "todo"))

    def test_waiting_state_uses_current_checkpoint_and_answer_receipt(self):
        from .test_presentation_stream import create_question_authority_tables
        accepted = self.accept()
        run = AgentRun.objects.get(pk=accepted["agentRunId"])
        AgentRun.objects.filter(pk=run.pk).update(status="running")
        create_question_authority_tables()
        checkpoint = {"schema": "runtime.await_question.v2", "continuationId": "continuation",
            "agentRunId": run.pk, "authorizationDigest": run.authorization.digest,
            "turnId": run.turn_id, "questionId": "question",
            "question": {"question": "Which revision should I use?", "options": ["Current", "Previous"]},
            "toolCall": {"id": "question", "name": "request_user_input", "argsJson": "{}"}}
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.checkpoints(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) "
                "VALUES('issue-question','wait',%s,%s,'paused_question','question',1,%s)",
                [run.session_id, run.turn_id, json.dumps(checkpoint)])
        self.assertTrue(self.client.get(self.url).json()["executions"][0]["waitingForInput"])
        answer_url = f"/api/sessions/{run.session_id}/agent-runs/{run.pk}/questions/question/responses"
        body = {"operationId": "answer", "continuationId": "continuation", "action": "answer", "answer": "Current"}
        with patch("app_core.http.workspaces.request_agent_run_question_response") as respond:
            response = self.client.post(answer_url, json.dumps(body), content_type="application/json")
            self.assertEqual(response.status_code, 202, response.content)
            self.assertEqual(response.json()["agentRunId"], run.pk)
            respond.assert_called_once()
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) "
                "VALUES(%s,%s,%s,'runtime.question.response.v1',2,'internal',%s)",
                ["runtime_question_response:continuation", run.session_id, run.turn_id,
                    json.dumps({"schema": "runtime.question.response.v1", "agentRunId": run.pk,
                        "authorizationDigest": run.authorization.digest, "continuationId": "continuation", "questionId": "question"})])
        self.assertFalse(self.client.get(self.url).json()["executions"][0]["waitingForInput"])
        self.assertEqual((AgentRun.objects.count(), IssueExecution.objects.count()), (1, 1))

    def test_origin_is_current_and_requires_both_session_and_issue_access(self):
        accepted = self.accept()
        origin_url = f"/api/workspaces/{self.workspace.pk}/sessions/{accepted['sessionId']}/issue-origin"
        execution = IssueExecution.objects.get(session_id=accepted["sessionId"])
        Issue.objects.filter(pk=self.issue.pk).update(title="Updated title")
        self.assertEqual(self.client.get(origin_url).json(), {"origin": {"issueId": self.issue.pk,
            "executionId": str(execution.pk), "title": "Updated title"}})
        self.assertEqual(self.client.get(origin_url + "?extra=true").status_code, 400)
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(origin_url).status_code, 404)
        self.client.force_login(self.user)
        Issue.objects.filter(pk=self.issue.pk).update(visibility="participants", creator=self.other)
        self.assertEqual(self.client.get(origin_url).json(), {"origin": None})
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).delete()
        self.assertEqual(self.client.get(origin_url).status_code, 404)

    def test_ordinary_conversations_have_no_issue_origin(self):
        session = Session.objects.create(workspace=self.workspace, owner=self.user)
        url = f"/api/workspaces/{self.workspace.pk}/sessions/{session.pk}/issue-origin"
        self.assertEqual(self.client.get(url).json(), {"origin": None})
        Session.objects.filter(pk=session.pk).update(archivedAt=timezone.now())
        self.assertEqual(self.client.get(url).status_code, 404)

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
