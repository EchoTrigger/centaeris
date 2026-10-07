import threading
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from django.db import close_old_connections
from django.test import Client, TransactionTestCase

from app_core.models import AgentRun, HostedOperationReceipt, ModelConfig
from app_core.test_app_delegations import AppDelegationSessionFixture
from app_core.test_agent_definitions import PROFILE


class AppDelegationConcurrencyTests(AppDelegationSessionFixture, TransactionTestCase):
    serialized_rollback = True

    def submit(self, operation="delegated-race"):
        close_old_connections()
        try:
            return self.api("post", self.base + f"/sessions/{self.session.id}/messages", {
                "operationId": operation, "text": "Fixture", "modelConfigRef": self.model.id,
            })
        finally:
            close_old_connections()

    def test_revocation_committed_during_preparation_rejects_acceptance(self):
        self.model = ModelConfig.objects.create(displayName="Fixture")
        prepared, revoked = threading.Event(), threading.Event()

        def profile():
            prepared.set()
            if not revoked.wait(timeout=10):
                raise AssertionError("revocation did not commit")
            return PROFILE

        with patch("app_core.http.workspaces.request_execution_profile", side_effect=profile), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle") as schedule:
            with ThreadPoolExecutor(max_workers=1) as pool:
                submitted = pool.submit(self.submit)
                try:
                    self.assertTrue(prepared.wait(timeout=10))
                    response = self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}")
                    self.assertEqual(response.status_code, 204, response.content)
                finally:
                    revoked.set()
                denied = submitted.result(timeout=20)
        self.assertEqual(denied.status_code, 403, denied.content)
        self.assertEqual(denied.json(), {"error": "delegation_not_available"})
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())
        schedule.assert_not_called()

    def test_acceptance_lock_orders_revocation_after_the_accepted_run(self):
        from app_core.app_delegations import require_request_delegation
        self.model = ModelConfig.objects.create(displayName="Fixture")
        prepared, accepted_lock, revocation_waiting = threading.Event(), threading.Event(), threading.Event()

        def profile():
            prepared.set()
            return PROFILE

        def acceptance_check(*args, **kwargs):
            grant = require_request_delegation(*args, **kwargs)
            if prepared.is_set():
                accepted_lock.set()
                if not revocation_waiting.wait(timeout=10):
                    raise AssertionError("revoker did not reach workspace lock")
            return grant

        def revoke():
            close_old_connections()
            try:
                self.assertTrue(accepted_lock.wait(timeout=10))
                return self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}")
            finally:
                close_old_connections()

        # Mark the revoker's select_for_update get, not a timer or a sleep.
        from app_core.models import Workspace
        from django.db.models.query import QuerySet
        original_queryset_get = QuerySet.get
        def queryset_get(query, *args, **kwargs):
            if query.model is Workspace and query.query.select_for_update and kwargs.get("id") == self.workspace.id:
                revocation_waiting.set()
            return original_queryset_get(query, *args, **kwargs)

        with patch("app_core.http.workspaces.request_execution_profile", side_effect=profile), \
             patch("app_core.http.workspaces.require_request_delegation", side_effect=acceptance_check), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted"), \
             patch.object(QuerySet, "get", queryset_get):
            with ThreadPoolExecutor(max_workers=2) as pool:
                submitted, revoking = pool.submit(self.submit), pool.submit(revoke)
                accepted, revoked = submitted.result(timeout=20), revoking.result(timeout=20)
        self.assertEqual(accepted.status_code, 202, accepted.content)
        self.assertEqual(revoked.status_code, 204, revoked.content)
        run = AgentRun.objects.get(id=accepted.json()["agentRunId"])
        self.assertEqual(run.app_delegation_id, self.grant.id)
        self.assertEqual(HostedOperationReceipt.objects.get(operationId="delegated-race").agentRunId, run.id)
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.id}").status_code, 403)

    def test_browser_and_app_concurrent_identical_commands_share_one_receipt(self):
        barrier = threading.Barrier(2)
        payload = {"operationId": "shared-concurrent-session", "agentId": self.agent.id}
        browser = Client()
        browser.force_login(self.member)

        def accept(acting_app):
            close_old_connections()
            try:
                barrier.wait(timeout=10)
                return (self.api("post", self.base + "/sessions", payload) if acting_app
                        else self.send("post", self.base + "/sessions", payload, client=browser))
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as pool:
            responses = list(pool.map(accept, [False, True]))
        self.assertEqual([r.status_code for r in responses], [201, 201])
        self.assertEqual(responses[0].json(), responses[1].json())
        self.assertEqual(HostedOperationReceipt.objects.filter(operationId=payload["operationId"]).count(), 1)
