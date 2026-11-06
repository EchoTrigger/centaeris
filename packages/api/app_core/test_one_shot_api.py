import asyncio
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
import json
import time
from unittest.mock import patch
import uuid

from asgiref.sync import async_to_sync

from django.contrib.auth.models import User
from django.db import close_old_connections
from django.test import Client, TransactionTestCase
from django.utils import timezone

from .http.one_shot import CreateRunRequest
from .models import AgentRun, OneShotOperation, Session, Workspace, WorkspaceMembership, ModelConfig
from .one_shot_credentials import create_credential
from .one_shot_runs import accept_run
from .one_shot_stream import operation_receipt


PROFILE = {"schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "a" * 64}


class OneShotApiTests(TransactionTestCase):
    serialized_rollback = True
    def setUp(self):
        self.user = User.objects.create_user(username="one-shot-owner")
        self.workspace = Workspace.objects.create(name="API", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.model = ModelConfig.objects.create(displayName="Model")
        self.credential, self.token = create_credential(self.user, self.workspace.pk, self.model.pk, name="Backend")
        self.operation_id = f"{int(time.time())}.{uuid.uuid4()}"
        self.body = {"prompt": "Generate a Word document"}
        self.client = Client()
        self.auth = {"HTTP_AUTHORIZATION": f"Bearer {self.token}", "HTTP_IDEMPOTENCY_KEY": self.operation_id}
        for name, result in (("request_execution_profile", PROFILE), ("schedule_agent_run_lifecycle", "inserted")):
            mock = patch(f"app_core.one_shot_runs.{name}", return_value=result)
            setattr(self, name, mock.start())
            self.addCleanup(mock.stop)

    def post(self, body=None, **headers):
        return self.client.post("/api/v1/agent-runs", json.dumps(body or self.body), content_type="application/json",
                                **(self.auth | headers))

    def query(self):
        return self.client.get(f"/api/v1/agent-runs/operations/{self.operation_id}", HTTP_AUTHORIZATION=f"Bearer {self.token}")

    def test_acceptance_has_signed_private_runtime_and_receipt(self):
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        self.assertEqual(run.session.purpose, "oneShot")
        self.assertFalse(run.runmaterialscope.personal_library)
        self.assertEqual(run.authorization.payload["schema"], "workspace.agent_run_authorization.v3")
        self.assertEqual(run.authorization.payload["executionPolicy"],
            {"purpose": "oneShot", "deadlineAtMs": int(operation.deadlineAt.timestamp() * 1000)})
        self.assertEqual(run.authorization.payload["pluginActivation"]["packages"], [])
        self.assertEqual(self.query().json(), operation_receipt(operation))
        self.assertNotIn("sessionId", self.query().json())
        self.assertEqual(self.query().headers["Cache-Control"], "no-store")

    def test_repeat_is_receipt_only_and_changed_input_conflicts(self):
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        self.request_execution_profile.side_effect = AssertionError("replay queried execution profile")
        replay = self.post()
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(replay.json(), operation_receipt(operation))
        conflict = self.post({"prompt": "different"})
        self.assertEqual(conflict.status_code, 409, conflict.content)
        self.assertEqual(conflict.json(), {"error": "operation_conflict"})
        self.assertEqual(AgentRun.objects.count(), 1)
        self.schedule_agent_run_lifecycle.assert_called_once()

    def test_lost_initial_response_does_not_start_again(self):
        response = self.post()
        self.assertEqual(response.status_code, 200)
        self.assertTrue(response.is_async)
        asyncio.run(response.aclose())
        self.assertEqual(self.query().status_code, 200)
        self.assertEqual(self.post().status_code, 200)
        self.assertEqual(OneShotOperation.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)

    def test_rejected_request_never_accepts_or_schedules(self):
        for body in ({"prompt": " "}, {"prompt": "test", "sessionId": "old"},
                     {"prompt": "test", "sourceObjectRefs": ["missing"]}):
            with self.subTest(body=body):
                self.assertIn(self.post(body).status_code, (400, 404))
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(Session.objects.exists())
        self.schedule_agent_run_lifecycle.assert_not_called()

    def test_key_expiry_rejects_even_after_receipt_removed(self):
        for key, expected in ((f"{int(time.time()) - 86401}.{uuid.uuid4()}", 410),
                              (f"{int(time.time()) + 600}.{uuid.uuid4()}", 400), ("missing", 400)):
            response = self.post(HTTP_IDEMPOTENCY_KEY=key)
            self.assertEqual(response.status_code, expected, response.content)
        self.assertFalse(OneShotOperation.objects.exists())

    def test_transaction_rollback_does_not_occupy_identity(self):
        with patch("app_core.one_shot_runs.create_agent_run_authorization", side_effect=RuntimeError("unavailable")):
            response = self.post()
        self.assertEqual(response.status_code, 503)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(Session.objects.exists())
        self.assertFalse(OneShotOperation.objects.exists())

    def test_expiry_is_checked_again_at_atomic_acceptance(self):
        with patch("app_core.one_shot_runs.operation_expiry", return_value=timezone.now() - timedelta(seconds=1)):
            response = self.post()
        self.assertEqual(response.status_code, 410, response.content)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(Session.objects.exists())
        self.assertFalse(OneShotOperation.objects.exists())
        self.schedule_agent_run_lifecycle.assert_not_called()

    def test_schedule_failure_preserves_receipt_and_original_run(self):
        self.schedule_agent_run_lifecycle.side_effect = RuntimeError("offline")
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        self.assertEqual(AgentRun.objects.get().transitionReason, "agent_run_lifecycle_schedule_pending")
        self.assertEqual(self.query().json(), operation_receipt(operation))
        self.assertEqual(self.post().json()["runId"], run.pk)
        self.schedule_agent_run_lifecycle.assert_called_once()

    def test_api_credential_cannot_authenticate_product_routes_or_other_identity(self):
        self.assertEqual(self.client.get(f"/api/workspaces/{self.workspace.pk}/sessions", **self.auth).status_code, 401)
        self.assertEqual(self.query().status_code, 404)
        self.assertEqual(self.post(HTTP_AUTHORIZATION="Bearer invalid").status_code, 401)
        self.client.force_login(self.user)
        self.assertEqual(self.post().status_code, 400)

    def test_management_is_admin_scoped_and_secret_displayed_once(self):
        url = f"/api/workspaces/{self.workspace.pk}/api-credentials"
        self.client.force_login(self.user)
        response = self.client.post(url, json.dumps({"name": "Another", "modelConfigRef": self.model.pk}), content_type="application/json")
        self.assertEqual(response.status_code, 201, response.content)
        self.assertIn("secret", response.json())
        listed = self.client.get(url)
        self.assertNotIn(response.json()["secret"], listed.content.decode())
        member = WorkspaceMembership.objects.get(user=self.user, workspace=self.workspace)
        member.role = "member"
        member.save(update_fields=["role"])
        self.assertEqual(self.client.get(url).status_code, 403)
        self.assertEqual(self.client.post(url, json.dumps({"name": "No", "modelConfigRef": self.model.pk}), content_type="application/json").status_code, 403)

    def test_cancel_and_revoke_use_original_run(self):
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        with patch("app_core.http.one_shot.request_agent_run_cancellation", return_value={"disposition": "requested", "terminalState": None}) as cancel:
            response = self.client.post(f"/api/v1/agent-runs/{run.pk}/cancel", data=b"", content_type="application/json", **self.auth)
        self.assertEqual(response.status_code, 202, response.content)
        self.assertEqual(cancel.call_args.args[0].pk, run.pk)
        self.client.force_login(self.user)
        self.assertEqual(self.client.delete(f"/api/workspaces/{self.workspace.pk}/api-credentials/{self.credential.pk}").status_code, 200)
        self.client.logout()
        self.assertEqual(self.query().status_code, 403)
        self.assertEqual(self.post().status_code, 403)
        self.assertEqual(AgentRun.objects.count(), 1)

    def test_concurrent_identity_accepts_one_run(self):
        def submit():
            close_old_connections()
            try:
                return accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))[0].pk
            finally:
                close_old_connections()
        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(lambda _: submit(), range(2)))
        self.assertEqual(results[0], results[1])
        self.assertEqual(AgentRun.objects.count(), 1)
        self.schedule_agent_run_lifecycle.assert_called_once()

    def test_stream_retains_known_terminal_reason_in_receipt(self):
        from .one_shot_stream import _run_state
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        AgentRun.objects.filter(pk=run.pk).update(status="failed", transitionReason="model_connection_error", completedAt=timezone.now())
        result = async_to_sync(_run_state)(operation.pk)
        self.assertEqual(operation_receipt(result)["reasonCode"], "model_connection_error")
        self.assertEqual(self.query().json()["runStatus"], "failed")

    def test_worker_cleanup_reconciliation_runs_one_bounded_data_collection(self):
        with patch("app_core.one_shot_cleanup.collect_one_shot_cleanup") as collect:
            response = self.client.post(
                "/internal/agent-run-lifecycle/cleanup-reconcile",
                json.dumps({"schema": "runtime.agent_run_cleanup.reconcile.v1", "limit": 1, "after": None}),
                content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token",
            )
        self.assertEqual(response.status_code, 200, response.content)
        collect.assert_called_once_with(limit=1)
