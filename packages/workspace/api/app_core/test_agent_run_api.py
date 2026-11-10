import asyncio
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
import hashlib
import json
import time
from unittest.mock import patch
import uuid

from asgiref.sync import async_to_sync

from django.contrib.auth.models import User
from django.db import close_old_connections
from django.db.migrations.executor import MigrationExecutor
from django.test import Client, TransactionTestCase
from django.utils import timezone

from .http.agent_run_api import CreateRunRequest
from .migration_testing import isolated_migration_database
from .models import AgentRun, AgentRunApiConversation, AgentRunApiCredential, AgentRunApiOperation, AgentRunEnvironmentRelease, Session, SessionEnvironment, Workspace, WorkspaceMembership, ModelConfig
from .agent_run_api_cleanup import collect_agent_run_api_cleanup
from .agent_run_api_credentials import create_credential
from .agent_run_api_runs import accept_run
from .agent_run_api_stream import operation_receipt


PROFILE = {"schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "a" * 64}


class AgentRunApiApiTests(TransactionTestCase):
    serialized_rollback = True
    def setUp(self):
        self.user = User.objects.create_user(username="agent-run-api-owner")
        self.workspace = Workspace.objects.create(name="API", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.model = ModelConfig.objects.create(displayName="Model")
        self.credential, self.token = create_credential(self.user, self.workspace.pk, self.model.pk, name="Backend")
        self.operation_id = f"{int(time.time())}.{uuid.uuid4()}"
        self.body = {"prompt": "Generate a Word document"}
        self.client = Client()
        self.auth = {"HTTP_AUTHORIZATION": f"Bearer {self.token}", "HTTP_IDEMPOTENCY_KEY": self.operation_id}
        for name, result in (("request_execution_profile", PROFILE), ("schedule_agent_run_lifecycle", "inserted")):
            mock = patch(f"app_core.agent_run_api_runs.{name}", return_value=result)
            setattr(self, name, mock.start())
            self.addCleanup(mock.stop)

    def post(self, body=None, **headers):
        return self.client.post("/api/v1/agent/execute", json.dumps(body or self.body), content_type="application/json",
                                **(self.auth | headers))

    def query(self):
        return self.client.get(f"/api/v1/agent/operations/{self.operation_id}", HTTP_AUTHORIZATION=f"Bearer {self.token}")

    def message(self, conversation_id=None, *, key=None, token=None, client=None):
        body = {"prompt": "Reply OK"}
        if conversation_id:
            body["conversationId"] = conversation_id
        key = key or f"{int(time.time())}.{uuid.uuid4()}"
        response = (client or self.client).post("/api/v1/agent/messages", json.dumps(body), content_type="application/json",
            HTTP_AUTHORIZATION=f"Bearer {token or self.token}", HTTP_IDEMPOTENCY_KEY=key)
        if response.streaming:
            asyncio.run(response.aclose())
        return response, AgentRunApiOperation.objects.filter(credential=self.credential, operationId=key).first()

    def close_conversation(self, conversation_id, *, token=None):
        return self.client.post(f"/api/v1/agent/conversations/{conversation_id}/close", data=b"",
            content_type="application/json", HTTP_AUTHORIZATION=f"Bearer {token or self.token}")

    def settle_message(self, operation, *, released=False):
        now = timezone.now()
        run = AgentRun.objects.get(pk=operation.agentRunId)
        AgentRun.objects.filter(pk=run.pk).update(
            status="completed" if released else "cancelled", completedAt=now,
            preAdmissionCancelledAt=None if released else now,
        )
        AgentRunApiOperation.objects.filter(pk=operation.pk).update(deliveryClosedAt=now)
        if released:
            epoch = run.environment.controller_epoch + 1
            lease = "fixture-cleanup"
            SessionEnvironment.objects.filter(pk=run.environment_id).update(
                controller_epoch=epoch, controller_run=run, controller_lease_owner=lease,
                controller_released=True, controller_purpose="cleanup",
            )
            AgentRunEnvironmentRelease.objects.create(
                agent_run=run, environment=run.environment, authorization_digest=run.authorization.digest,
                controller_epoch=epoch,
                controller_lease_digest="sha256:" + hashlib.sha256(b"workspace:environment-controller-lease:v1\0fixture-cleanup").hexdigest(),
            )

    def test_message_reuses_private_session_and_receipt_and_rejects_concurrent_turn(self):
        _, first = self.message(key=self.operation_id)
        self.assertTrue(first.conversationId)
        self.assertEqual(self.query().json()["conversationId"], first.conversationId)
        self.assertEqual(self.query().json()["cleanupStatus"], "retained")
        self.assertEqual(self.message(first.conversationId)[0].json()["error"], "session_has_active_agent_run")
        self.settle_message(first)
        _, second = self.message(first.conversationId)
        self.assertEqual(second.sessionId, first.sessionId)
        replay, _ = self.message(key=self.operation_id)
        self.assertEqual(replay.json()["runId"], first.agentRunId)
        self.assertEqual(self.post().status_code, 409)
        self.assertEqual(AgentRun.objects.count(), 2)

    def test_close_is_scoped_and_idempotent(self):
        _, operation = self.message()
        _, other = create_credential(self.user, self.workspace.pk, self.model.pk, name="Other")
        self.assertEqual(self.close_conversation(operation.conversationId, token=other).status_code, 404)
        self.assertEqual(self.message(operation.conversationId, token=other)[0].status_code, 404)
        first = self.close_conversation(operation.conversationId)
        self.assertEqual(first.status_code, 200)
        self.assertEqual(first.json(), self.close_conversation(operation.conversationId).json())
        self.assertEqual(AgentRun.objects.get(pk=operation.agentRunId).status, "queued")
        self.assertFalse(self.message()[1].conversationId == operation.conversationId)

    def test_workspace_admin_can_create_and_change_reset_policy(self):
        self.client.force_login(self.user)
        base = f"/api/workspaces/{self.workspace.pk}/api-credentials"
        created = self.client.post(base, json.dumps({
            "name": "Conversation", "modelConfigRef": self.model.pk, "resetPolicy": "after15Minutes",
        }), content_type="application/json")
        self.assertEqual(created.status_code, 201, created.content)
        self.assertEqual(created.json()["credential"]["resetPolicy"], "after15Minutes")
        url = f"{base}/{self.credential.pk}"
        updated = self.client.patch(url, json.dumps({"resetPolicy": "onReopen"}), content_type="application/json")
        self.assertEqual(updated.status_code, 200, updated.content)
        self.assertEqual(updated.json()["credential"]["resetPolicy"], "onReopen")
        rejected = self.client.patch(url, json.dumps({"resetPolicy": "on_reopen"}), content_type="application/json")
        self.assertEqual(rejected.status_code, 400, rejected.content)
        self.credential.refresh_from_db()
        self.assertEqual(self.credential.resetPolicy, "onReopen")

    def test_reset_policies_apply_at_reopen_and_timed_boundaries(self):
        for policy, minutes in (("never", None), ("onReopen", 0), ("after10Minutes", 10),
                                ("after15Minutes", 15), ("after30Minutes", 30), ("after60Minutes", 60)):
            for should_reset in (False, True) if minutes else (minutes == 0,):
                with self.subTest(policy=policy, should_reset=should_reset):
                    self.credential.resetPolicy = policy
                    self.credential.save(update_fields=["resetPolicy"])
                    _, first = self.message()
                    self.settle_message(first)
                    self.close_conversation(first.conversationId)
                    elapsed = timedelta(minutes=minutes if minutes is not None else 60)
                    if not should_reset and minutes:
                        elapsed -= timedelta(seconds=30)
                    AgentRunApiConversation.objects.filter(pk=first.conversationId).update(closedAt=timezone.now() - elapsed)
                    response, following = self.message(first.conversationId)
                    self.assertEqual(response.status_code, 200)
                    self.assertEqual(following.conversationId != first.conversationId, should_reset)
                    self.assertIsNone(AgentRunApiConversation.objects.get(pk=following.conversationId).closedAt)

    def test_concurrent_reset_selects_one_successor_and_replays_winner(self):
        self.credential.resetPolicy = "onReopen"
        self.credential.save(update_fields=["resetPolicy"])
        _, first = self.message()
        self.close_conversation(first.conversationId)
        keys = [f"{int(time.time())}.{uuid.uuid4()}" for _ in range(2)]

        def submit(key):
            close_old_connections()
            try:
                response, _ = self.message(first.conversationId, key=key, client=Client())
                return response.status_code, None if response.streaming else response.json()
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(submit, keys))
        self.assertCountEqual([status for status, _ in results], [200, 409])
        winner = next(key for key, (status, _) in zip(keys, results) if status == 200)
        loser = next(body for status, body in results if status == 409)
        replay, _ = self.message(first.conversationId, key=winner)
        self.assertEqual(loser, {"error": "conversation_reset", "conversationId": replay.json()["conversationId"]})
        self.assertEqual(AgentRunApiConversation.objects.count(), 2)
        self.assertEqual(AgentRun.objects.count(), 2)

    def test_cleanup_waits_for_all_deliveries_and_retirement_then_keeps_receipts(self):
        _, first = self.message()
        self.settle_message(first, released=True)
        _, second = self.message(first.conversationId)
        with patch("app_core.agent_run_api_cleanup._purge_runtime") as purge, patch("app_core.agent_run_api_cleanup.redis.Redis.from_url"):
            self.assertEqual(collect_agent_run_api_cleanup().cleaned, [])
            self.credential.resetPolicy = "onReopen"
            self.credential.save(update_fields=["resetPolicy"])
            self.close_conversation(first.conversationId)
            _, replacement = self.message(first.conversationId)
            self.assertEqual(collect_agent_run_api_cleanup().cleaned, [])
            purge.assert_not_called()
            self.settle_message(second, released=True)
            report = collect_agent_run_api_cleanup()
            self.assertEqual(report.failures, [])
            self.assertEqual(report.cleaned, [second.agentRunId])
        self.assertFalse(Session.objects.filter(pk=first.sessionId).exists())
        self.assertTrue(Session.objects.filter(pk=replacement.sessionId).exists())
        self.assertEqual(AgentRunApiOperation.objects.filter(sessionId=first.sessionId, cleanupStatus="completed").count(), 2)
        self.assertFalse(AgentRunEnvironmentRelease.objects.filter(agent_run_id__in=[first.agentRunId, second.agentRunId]).exists())

    def test_acceptance_has_signed_private_runtime_and_receipt(self):
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        self.assertEqual(run.session.purpose, "agentRunApi")
        self.assertFalse(run.runmaterialscope.personal_library)
        self.assertEqual(run.authorization.payload["schema"], "workspace.agent_run_authorization.v3")
        self.assertEqual(run.authorization.payload["executionPolicy"],
            {"purpose": "agentRunApi", "deadlineAtMs": int(operation.deadlineAt.timestamp() * 1000)})
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
        self.assertEqual(AgentRunApiOperation.objects.count(), 1)
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
        self.assertFalse(AgentRunApiOperation.objects.exists())

    def test_transaction_rollback_does_not_occupy_identity(self):
        with patch("app_core.agent_run_api_runs.create_agent_run_authorization", side_effect=RuntimeError("unavailable")):
            response = self.post()
        self.assertEqual(response.status_code, 503)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(Session.objects.exists())
        self.assertFalse(AgentRunApiOperation.objects.exists())

    def test_expiry_is_checked_again_at_atomic_acceptance(self):
        with patch("app_core.agent_run_api_runs.operation_expiry", return_value=timezone.now() - timedelta(seconds=1)):
            response = self.post()
        self.assertEqual(response.status_code, 410, response.content)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(Session.objects.exists())
        self.assertFalse(AgentRunApiOperation.objects.exists())
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

    def test_management_lists_metadata_and_only_the_issuer_can_copy(self):
        url = f"/api/workspaces/{self.workspace.pk}/api-credentials"
        self.client.force_login(self.user)
        response = self.client.post(url, json.dumps({"name": "Another", "modelConfigRef": self.model.pk}), content_type="application/json")
        self.assertEqual(response.status_code, 201, response.content)
        self.assertIn("secret", response.json())
        listed = self.client.get(url)
        created = response.json()
        secret_url = f"{url}/{created['credential']['id']}/secret"
        self.assertNotIn(created["secret"], listed.content.decode())
        self.assertTrue(created["credential"]["canCopy"])
        stored = AgentRunApiCredential.objects.get(pk=created["credential"]["id"])
        self.assertNotIn(created["secret"], stored.encryptedSecret)
        copy = self.client.get(secret_url)
        self.assertEqual(copy.json(), {"secret": created["secret"]})
        self.assertEqual(copy.headers["Cache-Control"], "no-store")
        AgentRunApiCredential.objects.filter(pk=stored.pk).update(encryptedSecret="")
        self.assertEqual(self.client.get(secret_url).status_code, 404)

        other = User.objects.create_user(username="another-admin")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=other, role="admin")
        self.client.force_login(other)
        self.assertFalse(self.client.get(url).json()["credentials"][0]["canCopy"])
        self.assertEqual(self.client.get(f"{url}/{self.credential.pk}/secret").status_code, 404)
        self.client.force_login(self.user)
        member = WorkspaceMembership.objects.get(user=self.user, workspace=self.workspace)
        member.role = "member"
        member.save(update_fields=["role"])
        self.assertEqual(self.client.get(url).status_code, 403)
        self.assertEqual(self.client.post(url, json.dumps({"name": "No", "modelConfigRef": self.model.pk}), content_type="application/json").status_code, 403)

    def test_delete_erases_credential_and_detaches_incomplete_cleanup(self):
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        with patch("app_core.http.agent_run_api.request_agent_run_cancellation", return_value={"disposition": "requested", "terminalState": None}) as cancel:
            response = self.client.post(f"/api/v1/agent/executions/{run.pk}/cancel", data=b"", content_type="application/json", **self.auth)
        self.assertEqual(response.status_code, 202, response.content)
        self.assertEqual(cancel.call_args.args[0].pk, run.pk)
        self.client.force_login(self.user)
        path = f"/api/workspaces/{self.workspace.pk}/api-credentials"
        self.assertEqual(self.client.delete(f"{path}/{self.credential.pk}").status_code, 204)
        self.assertEqual(self.client.get(path).json(), {"credentials": []})
        self.assertEqual(self.client.get(f"{path}/{self.credential.pk}/secret").status_code, 404)
        self.assertFalse(AgentRunApiCredential.objects.filter(pk=self.credential.pk).exists())
        operation.refresh_from_db()
        self.assertIsNone(operation.credential_id)
        self.client.logout()
        self.assertEqual(self.query().status_code, 401)
        self.assertEqual(self.post().status_code, 401)
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
        from .agent_run_api_stream import _run_state
        operation, run = accept_run(self.credential, self.operation_id, CreateRunRequest(**self.body))
        AgentRun.objects.filter(pk=run.pk).update(status="failed", transitionReason="model_connection_error", completedAt=timezone.now())
        result = async_to_sync(_run_state)(operation.pk)
        self.assertEqual(operation_receipt(result)["reasonCode"], "model_connection_error")
        self.assertEqual(self.query().json()["runStatus"], "failed")

    def test_worker_cleanup_reconciliation_runs_one_bounded_data_collection(self):
        with patch("app_core.agent_run_api_cleanup.collect_agent_run_api_cleanup") as collect:
            response = self.client.post(
                "/internal/agent-run-lifecycle/cleanup-reconcile",
                json.dumps({"schema": "runtime.agent_run_cleanup.reconcile.v1", "limit": 1, "after": None}),
                content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token",
            )
        self.assertEqual(response.status_code, 200, response.content)
        collect.assert_called_once_with(limit=1)


class AgentRunApiCredentialUpgradeTests(TransactionTestCase):
    serialized_rollback = True

    def test_revoked_credentials_are_removed_while_pending_cleanup_survives(self):
        previous = [("app_core", "0046_workspace_owned_resources")]
        target = [("app_core", "0047_one_shot_credential_copy_and_delete")]
        now = timezone.now()
        with isolated_migration_database(previous) as database:
            alias = database.alias
            executor = MigrationExecutor(database)
            old = executor.loader.project_state(previous).apps
            user = old.get_model("auth", "User").objects.using(alias).create(username="issuer")
            workspace = old.get_model("app_core", "Workspace").objects.using(alias).create(
                name="API", createdBy_id=user.pk,
            )
            model = old.get_model("app_core", "ModelConfig").objects.using(alias).create(displayName="Model")
            credentials = old.get_model("app_core", "OneShotCredential").objects.using(alias)
            scope = {"workspace_id": workspace.pk, "user_id": user.pk, "membership_ref": "membership",
                     "modelConfig_id": model.pk, "secretDigest": "a" * 64}
            active = credentials.create(name="Existing", **scope)
            revoked = credentials.create(name="Revoked", revokedAt=now, **scope)
            operations = old.get_model("app_core", "OneShotOperation").objects.using(alias)
            for state in ("pending", "completed"):
                operations.create(
                    credential_id=revoked.pk, operationId=state, requestDigest="b" * 64,
                    agentRunId="run_" + state, sessionId="session_" + state,
                    deadlineAt=now, expiresAt=now + timedelta(hours=24), cleanupStatus=state,
                )

            executor.migrate(target)
            upgraded = executor.loader.project_state(target).apps
            saved = upgraded.get_model("app_core", "OneShotCredential").objects.using(alias).get()
            self.assertEqual(saved.pk, active.pk)
            self.assertEqual(saved.encryptedSecret, "")
            pending = upgraded.get_model("app_core", "OneShotOperation").objects.using(alias).get()
            self.assertEqual(pending.cleanupStatus, "pending")
            self.assertIsNone(pending.credential_id)
