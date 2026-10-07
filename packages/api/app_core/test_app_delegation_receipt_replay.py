from unittest.mock import patch

from django.test import TestCase

from app_core.models import Agent, AgentRun, HostedOperationReceipt, ModelConfig, Session
from app_core.test_agent_definitions import PROFILE
from app_core.test_app_delegations import AppDelegationSessionFixture


class AppDelegationReceiptReplayTests(AppDelegationSessionFixture, TestCase):
    def delete_session(self, session_id):
        with patch("app_core.http.workspaces.request_agent_run_cancellation", return_value={"disposition": "requested"}):
            deleted = self.member_client.delete(f"/api/sessions/{session_id}")
        self.assertEqual(deleted.status_code, 200, deleted.content)

    def assert_deleted_operation(self, browser, app):
        self.assertEqual(browser.status_code, 410, browser.content)
        self.assertEqual(browser.json(), {"error": "operation_resource_unavailable"})
        self.assertEqual(app.status_code, 410, app.content)
        self.assertEqual(app.json(), browser.json())

    def test_message_post_retry_after_session_deletion_preserves_receipt_and_current_grant(self):
        path = self.base + f"/sessions/{self.session.id}/messages"
        payload = {"operationId": "deleted-message-replay", "text": "Fixture",
                   "modelConfigRef": ModelConfig.objects.create(displayName="Fixture").id}
        with patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE) as profile, \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted") as schedule:
            accepted = self.api("post", path, payload)
            self.assertEqual(accepted.status_code, 202, accepted.content)
            run_id = accepted.json()["agentRunId"]
            self.delete_session(self.session.id)
            browser = self.send("post", path, payload, client=self.member_client)
            self.assert_deleted_operation(browser, self.api("post", path, payload))
            fresh = self.api("post", path, {**payload, "operationId": "fresh-deleted-message"})
            self.assertEqual(fresh.status_code, 404, fresh.content)
            self.assertEqual(self.api("get", f"/api/sessions/{self.session.id}").status_code, 404)
            profile.assert_called_once()
            schedule.assert_called_once()

        self.assertEqual(AgentRun.objects.count(), 1)
        run = AgentRun.objects.get(pk=run_id)
        receipt = HostedOperationReceipt.objects.get(operationId=payload["operationId"])
        self.assertEqual((run.user_id, run.acting_app_id, run.app_delegation_id),
                         (self.member.id, self.application["id"], self.grant.id))
        self.assertEqual((receipt.agentRunId, receipt.acting_app_id, receipt.app_delegation_id),
                         (run_id, self.application["id"], self.grant.id))
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        self.assertEqual(self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}").status_code, 204)
        revoked = self.api("post", path, payload)
        self.assertEqual(revoked.status_code, 403, revoked.content)
        self.assertEqual(revoked.json(), {"error": "delegation_not_available"})

    def test_create_session_post_retry_after_agent_deletion_preserves_receipt_and_current_grant(self):
        path = self.base + "/sessions"
        payload = {"operationId": "deleted-agent-replay", "agentId": self.agent.id}
        accepted = self.api("post", path, payload)
        self.assertEqual(accepted.status_code, 201, accepted.content)
        deleted = self.member_client.delete(f"/api/agents/{self.agent.id}")
        self.assertEqual(deleted.status_code, 200, deleted.content)
        browser = self.send("post", path, payload, client=self.member_client)
        self.assert_deleted_operation(browser, self.api("post", path, payload))
        fresh = self.api("post", path, {**payload, "operationId": "fresh-deleted-agent"})
        self.assertEqual(fresh.status_code, 404, fresh.content)
        self.assertEqual(self.api("get", f"/api/agents/{self.agent.id}").status_code, 404)
        receipt = HostedOperationReceipt.objects.get(operationId=payload["operationId"])
        self.assertEqual((receipt.sessionId, receipt.acting_app_id, receipt.app_delegation_id),
                         (accepted.json()["sessionId"], self.application["id"], self.grant.id))
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        self.assertEqual(self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}").status_code, 204)
        revoked = self.api("post", path, payload)
        self.assertEqual(revoked.status_code, 403, revoked.content)
        self.assertEqual(revoked.json(), {"error": "delegation_not_available"})

    def test_deleted_other_assistants_message_receipt_stays_inaccessible_on_both_urls(self):
        private = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent=private)
        payload = {"operationId": "foreign-deleted-message", "text": "Fixture",
                   "modelConfigRef": ModelConfig.objects.create(displayName="Fixture").id}
        path = self.base + f"/sessions/{session.id}/messages"
        with patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted"):
            accepted = self.send("post", path, payload, client=self.member_client)
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.delete_session(session.id)
        for target in [path, self.base + f"/sessions/{self.session.id}/messages"]:
            rejected = self.api("post", target, payload)
            self.assertEqual(rejected.status_code, 404, rejected.content)
        receipt = HostedOperationReceipt.objects.get(operationId=payload["operationId"])
        self.assertIsNone(receipt.acting_app_id)
        self.assertIsNone(receipt.app_delegation_id)
        self.assertEqual(AgentRun.objects.count(), 1)
