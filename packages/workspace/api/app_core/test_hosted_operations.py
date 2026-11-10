import json
import threading
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

from django.contrib.auth.models import User
from django.core.files.uploadedfile import SimpleUploadedFile
from django.db import close_old_connections, connection
from django.test import Client, TestCase, TransactionTestCase, override_settings
from django.utils import timezone

from app_core.models import AgentRun, HostedOperationReceipt, ModelConfig, Session, Workspace, WorkspaceMembership


PROFILE = {"schema": "runtime.execution_profile.v1",
           "imageDigest": "sha256:" + "a" * 64}


class OperationFixture:
    def setUp(self):
        super().setUp()
        self.user = User.objects.create_user(username="operation-user")
        self.workspace = Workspace.objects.create(name="Operations", createdBy=self.user)
        self.workspace.members.add(self.user)
        self.model = ModelConfig.objects.create(displayName="Model")
        self.client.force_login(self.user)
        self.create_url = f"/api/workspaces/{self.workspace.id}/sessions"
        self.submit_url = self.create_url + "/new/messages"
        profile = patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE)
        schedule = patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted")
        self.profile = profile.start()
        self.schedule = schedule.start()
        self.addCleanup(profile.stop)
        self.addCleanup(schedule.stop)

    def post(self, url=None, **changes):
        payload = {"operationId": "submit-1", "text": "hello",
                   "modelConfigRef": self.model.id, **changes}
        return self.client.post(url or self.submit_url, json.dumps(payload), content_type="application/json")

    def create(self, operation_id="create-1", **changes):
        return self.client.post(self.create_url, json.dumps({"operationId": operation_id,
                                **changes}), content_type="application/json")

    def query(self, command="submitMessage", operation_id="submit-1"):
        return self.client.get(f"/api/workspaces/{self.workspace.id}/operations/{command}/{operation_id}")


class HostedOperationTests(OperationFixture, TestCase):
    def test_create_response_loss_replays_immutable_receipt(self):
        first = self.create()
        self.assertEqual(first.status_code, 201, first.content)
        session = Session.objects.get(id=first.json()["sessionId"])
        session.title = "later title"
        session.save(update_fields=["title"])
        second = self.create()
        self.assertEqual(second.json(), first.json())
        self.assertEqual(first.json(), {"operationId": "create-1", "command": "createSession",
            "status": "accepted", "sessionId": session.id, "agentRunId": None, "turnId": None})
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(self.query("createSession", "create-1").json(), first.json())

    def test_submit_response_loss_and_terminal_replay_ignore_mutable_dependencies(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        from .models import RunMaterialScope
        self.assertTrue(RunMaterialScope.objects.get(agent_run_id=first.json()["agentRunId"]).personal_library)
        self.model.enabled = False
        self.model.save(update_fields=["enabled"])
        self.profile.side_effect = RuntimeError("offline")
        for status in ("completed", "failed", "cancelled"):
            with self.subTest(status=status):
                AgentRun.objects.filter(id=first.json()["agentRunId"]).update(status=status)
                second = self.post()
                self.assertEqual(second.status_code, 202, second.content)
                self.assertEqual(second.json(), first.json())
        self.assertEqual(first.json()["status"], "accepted")
        self.assertNotIn("session", first.json())
        self.assertEqual(self.query().json(), first.json())
        self.assertEqual(AgentRun.objects.count(), 1)
        self.schedule.assert_called_once()

    def test_conflicting_message_and_target_do_not_create_another_run(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        for kwargs in ({"text": "different"}, {"url": self.create_url + f"/{first.json()['sessionId']}/messages"}):
            response = self.post(**kwargs)
            self.assertEqual(response.status_code, 409, response.content)
            self.assertEqual(response.json(), {"error": "operation_conflict"})
        self.assertEqual(AgentRun.objects.count(), 1)

    def test_deleted_resource_and_revoked_membership_cannot_replay_or_recreate(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        Session.objects.filter(id=first.json()["sessionId"]).update(status="deleted", deletedAt=timezone.now())
        for response in (self.post(), self.post(text="changed"), self.query()):
            self.assertEqual(response.status_code, 410, response.content)
            self.assertEqual(response.json(), {"error": "operation_resource_unavailable"})
        WorkspaceMembership.objects.filter(user=self.user, workspace=self.workspace).delete()
        self.assertEqual(self.post().status_code, 404)
        self.assertEqual(self.query().status_code, 404)
        self.assertEqual(AgentRun.objects.count(), 1)

    def test_missing_and_invalid_operation_identity_are_rejected(self):
        for identity in (None, "", "with space", "中文", "x" * 129, "id\n"):
            response = self.post(operationId=identity)
            self.assertEqual(response.status_code, 400, response.content)
        response = self.client.post(self.submit_url, json.dumps({"text": "hello",
                                    "modelConfigRef": self.model.id}), content_type="application/json")
        self.assertEqual(response.status_code, 400)
        self.assertEqual(self.query().status_code, 404)
        self.assertFalse(AgentRun.objects.exists())

    def test_query_is_private_and_rejects_unknown_parameters(self):
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        query = self.query()
        self.assertEqual(query.headers.get("Cache-Control"), "no-store")
        invalid = self.client.get(f"/api/workspaces/{self.workspace.id}/operations/submitMessage/submit-1?extra=true")
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json(), {"error": "request_invalid"})

    def test_same_identity_is_scoped_to_user_workspace_and_command(self):
        original = self.create("shared-id")
        self.assertEqual(original.status_code, 201, original.content)
        submitted = self.post(operationId="shared-id")
        self.assertEqual(submitted.status_code, 202, submitted.content)
        other_workspace = Workspace.objects.create(name="Other", createdBy=self.user)
        other_workspace.members.add(self.user)
        other_workspace_result = self.client.post(f"/api/workspaces/{other_workspace.id}/sessions",
            json.dumps({"operationId": "shared-id"}), content_type="application/json")
        self.assertEqual(other_workspace_result.status_code, 201, other_workspace_result.content)
        other_user = User.objects.create_user(username="other-operation-user")
        self.workspace.members.add(other_user)
        self.client.force_login(other_user)
        self.assertEqual(self.query(operation_id="shared-id").status_code, 404)
        other_user_result = self.create("shared-id")
        self.assertEqual(other_user_result.status_code, 201, other_user_result.content)
        self.assertEqual(len({result.json()["sessionId"] for result in
                            (original, submitted, other_workspace_result, other_user_result)}), 4)

    def test_created_session_preserves_receipt_and_rejects_project_conflicts(self):
        first = self.create()
        self.assertEqual(first.status_code, 201, first.content)
        replay = self.create(projectId="different-project")
        self.assertEqual(replay.status_code, 409, replay.content)
        self.assertEqual(replay.json()["error"], "operation_conflict")
        self.assertEqual(self.query("createSession", "create-1").json(), first.json())
        self.assertEqual(Session.objects.count(), 1)

    def test_rolled_back_acceptance_does_not_occupy_identity(self):
        with patch("app_core.http.workspaces.create_agent_run_authorization", side_effect=RuntimeError("rollback")):
            failed = self.post()
        self.assertEqual(failed.status_code, 500, failed.content)
        self.assertEqual(self.query().status_code, 404)
        self.assertFalse(Session.objects.exists())
        retry = self.post(text="corrected request")
        self.assertEqual(retry.status_code, 202, retry.content)

    def test_schedule_failure_preserves_accepted_identity(self):
        self.schedule.side_effect = RuntimeError("scheduler unavailable")
        first = self.post()
        self.assertEqual(first.status_code, 202, first.content)
        self.assertEqual(self.post().json(), first.json())
        self.assertEqual(self.query().json(), first.json())
        self.assertEqual(AgentRun.objects.get().transitionReason, "agent_run_lifecycle_schedule_pending")
        self.schedule.assert_called_once()

    def test_upload_identity_is_content_bound_without_repeated_storage(self):
        def upload(content):
            return self.client.post(self.submit_url, {"operationId": "upload-1",
                "modelConfigRef": self.model.id, "text": "read", "files": SimpleUploadedFile("note.txt", content,
                content_type="text/plain")})
        first = upload(b"AAA")
        self.assertEqual(first.status_code, 202, first.content)
        with patch("app_core.http.workspaces._store_upload_batch", side_effect=AssertionError("duplicate upload")):
            second = upload(b"AAA")
            changed = upload(b"BBB")
        self.assertEqual(second.json(), first.json())
        self.assertEqual(changed.status_code, 409, changed.content)
        self.schedule.assert_called_once()


class HostedOperationConcurrencyTests(OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def test_independent_sessions_admit_concurrently(self):
        work_sessions = []
        clients = [Client(), Client()]
        for index, client in enumerate(clients):
            created = self.create(f"work-session-{index}")
            self.assertEqual(created.status_code, 201, created.content)
            work_sessions.append(created.json()["sessionId"])
            client.force_login(self.user)
        barrier = threading.Barrier(2)
        self.profile.side_effect = lambda: (barrier.wait(timeout=10), PROFILE)[1]

        def submit(index):
            close_old_connections()
            try:
                response = clients[index].post(self.create_url + f"/{work_sessions[index]}/messages",
                    json.dumps({"operationId": f"work-run-{index}", "text": "hello",
                                "modelConfigRef": self.model.id}), content_type="application/json")
                return response.status_code, response.json()
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(submit, range(2)))
        self.profile.side_effect = None
        self.assertEqual([status for status, _ in results], [202, 202], results)
        self.assertEqual({body["sessionId"] for _, body in results}, set(work_sessions))
        self.assertEqual(len({body["agentRunId"] for _, body in results}), 2)
        self.assertEqual(AgentRun.objects.filter(session_id__in=work_sessions, status__in=("queued", "running")).count(), 2)
        self.assertEqual(self.schedule.call_count, 2)

    def test_same_identity_with_competing_payloads_accepts_one_and_conflicts_the_other(self):
        clients = [Client(), Client()]
        for client in clients:
            client.force_login(self.user)
        barrier = threading.Barrier(2)
        self.profile.side_effect = lambda: (barrier.wait(timeout=10), PROFILE)[1]

        def submit(index):
            close_old_connections()
            try:
                payload = {"operationId": "conflicting-race", "text": f"different intent {index}",
                           "modelConfigRef": self.model.id}
                response = clients[index].post(self.submit_url, json.dumps(payload), content_type="application/json")
                return response.status_code, response.json()
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(submit, range(2)))
        self.assertEqual(sorted(status for status, _ in results), [202, 409], results)
        self.assertEqual(next(body for status, body in results if status == 409), {"error": "operation_conflict"})
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.schedule.assert_called_once()

    def test_simultaneous_submissions_create_one_run_and_schedule_once(self):
        clients = [Client(), Client()]
        for client in clients:
            client.force_login(self.user)
        barrier = threading.Barrier(2)
        self.profile.side_effect = lambda: (barrier.wait(timeout=10), PROFILE)[1]
        payload = json.dumps({"operationId": "race-1", "text": "hello",
                              "modelConfigRef": self.model.id})
        def submit(client):
            close_old_connections()
            try:
                response = client.post(self.submit_url, payload, content_type="application/json")
                return response.status_code, response.json()
            finally:
                close_old_connections()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(submit, clients))
        self.assertEqual([status for status, _ in results], [202, 202], results)
        self.assertEqual(results[0][1], results[1][1])
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.schedule.assert_called_once()

    def test_concurrent_upload_replay_cleans_losing_upload_storage(self):
        from app_core.http.library import _store_upload_batch

        clients = [Client(), Client()]
        for client in clients:
            client.force_login(self.user)
        barrier = threading.Barrier(2)
        self.profile.side_effect = lambda: (barrier.wait(timeout=10), PROFILE)[1]
        def submit(client):
            close_old_connections()
            try:
                response = client.post(self.submit_url, {"operationId": "upload-race", "text": "read",
                    "modelConfigRef": self.model.id,
                    "files": SimpleUploadedFile("race.txt", b"one body", content_type="text/plain")})
                return response.status_code, response.json()
            finally:
                close_old_connections()
        with tempfile.TemporaryDirectory() as storage, override_settings(MEDIA_ROOT=storage):
            with patch("app_core.http.workspaces._store_upload_batch", wraps=_store_upload_batch) as store:
                with ThreadPoolExecutor(max_workers=2) as pool:
                    results = list(pool.map(submit, clients))
            files = [path for path in Path(storage).rglob("*") if path.is_file()]
            self.assertEqual(len(files), 1)
            self.assertEqual(files[0].read_bytes(), b"one body")
        self.assertEqual([status for status, _ in results], [202, 202], results)
        self.assertEqual(results[0][1], results[1][1])
        self.assertEqual(AgentRun.objects.count(), 1)
        self.assertEqual(store.call_count, 2)
        self.schedule.assert_called_once()
