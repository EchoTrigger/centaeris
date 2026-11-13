import copy
from datetime import timedelta
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
import threading
import urllib.error
import uuid
from unittest.mock import patch

from django.contrib.auth.models import User
from django.test import SimpleTestCase, TestCase, override_settings
from django.utils import timezone

from .agent_run_api_credentials import create_credential
from .agent_run_api_protocol import CreateRunRequest
from .agent_run_api_runs import accept_run
from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, ModelConfig, Workspace, WorkspaceMembership
from .platform_reconciler import InternalApi, Reconciler, run
from .testing import create_session


class LifecycleReconciliationTests(TestCase):
    def setUp(self):
        self.user = User.objects.create_user(username="reconcile-owner")
        self.workspace = Workspace.objects.create(name="Reconcile", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(
            workspace=self.workspace, user=self.user, role="owner")
        self.model = ModelConfig.objects.create(displayName="Reconcile model")

    def interactive_run(self, *, status="running"):
        session = create_session(workspace=self.workspace, owner=self.user)
        agent_run = AgentRun.objects.create(workspace=self.workspace, session=session,
            user=self.user, modelConfig=self.model, prompt="hello", status=status)
        create_agent_run_authorization(agent_run, image_digest="sha256:" + "a" * 64)
        return agent_run

    def api_run(self):
        credential, _ = create_credential(self.user, self.workspace.pk, self.model.pk, name="Reconcile")
        profile = {"schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "a" * 64}
        with patch("app_core.agent_run_api_runs.request_execution_profile", return_value=profile), patch(
            "app_core.agent_run_api_runs.schedule_agent_run_lifecycle", return_value="inserted"
        ):
            _, agent_run = accept_run(credential,
                f"{int(timezone.now().timestamp())}.{uuid.uuid4()}", CreateRunRequest(prompt="hello"))
        return agent_run

    @staticmethod
    def job(agent_run, *, status="running"):
        return {"jobId": f"agent_run.lifecycle:{agent_run.pk}", "jobKind": "agent_run.lifecycle",
            "idempotencyKey": f"agent_run.lifecycle:{agent_run.pk}:{agent_run.authorization.digest}",
            "sessionId": agent_run.session_id, "payloadRef": f"record:agent_run:{agent_run.pk}",
            "status": status}

    def reconcile(self, job):
        with patch("app_core.http.internal.get_runtime_job", return_value=job), patch(
            "app_core.http.internal.schedule_agent_run_lifecycle"
        ) as schedule:
            response = self.client.post("/internal/agent-run-lifecycle/reconcile",
                data=json.dumps({"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 100}),
                content_type="application/json", HTTP_X_INTERNAL_TOKEN="test-internal-token")
        self.assertEqual(response.status_code, 200, response.content)
        return response.json(), schedule

    def test_healthy_jobs_keep_runtime_wakeups_without_rescheduling(self):
        agent_run = self.interactive_run()
        initial_reason = agent_run.transitionReason
        for status in ("queued", "leased", "running"):
            with self.subTest(status=status):
                result, schedule = self.reconcile(self.job(agent_run, status=status))
                self.assertEqual(result["scheduled"], 0)
                self.assertEqual(result["pending"], 0)
                schedule.assert_not_called()
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "running")
        self.assertEqual(agent_run.transitionReason, initial_reason)

    def test_existing_job_still_requires_current_interactive_membership(self):
        agent_run = self.interactive_run()
        self.membership.delete()
        with self.assertLogs("app_core.http.internal", level="ERROR"):
            result, schedule = self.reconcile(self.job(agent_run))
        self.assertEqual(result["pending"], 1)
        schedule.assert_not_called()
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.transitionReason, "agent_run_lifecycle_reconcile_pending")

    def test_existing_job_must_match_the_authorized_run(self):
        agent_run = self.interactive_run()
        for field in ("jobId", "jobKind", "idempotencyKey", "sessionId", "payloadRef", "status"):
            with self.subTest(field=field), self.assertLogs("app_core.http.internal", level="ERROR"):
                result, schedule = self.reconcile(self.job(agent_run) | {field: "unrelated"})
            self.assertEqual(result["pending"], 1)
            schedule.assert_not_called()

    @override_settings(EXECUTION_QUEUE_WAIT_SECONDS=0)
    def test_existing_queued_job_still_receives_queue_expiry_cancellation(self):
        agent_run = self.interactive_run(status="queued")
        with patch("app_core.http.internal.request_agent_run_cancellation") as cancel:
            result, schedule = self.reconcile(self.job(agent_run, status="queued"))
        self.assertEqual(result["pending"], 0)
        schedule.assert_not_called()
        cancel.assert_called_once()
        self.assertEqual(cancel.call_args.args[0].pk, agent_run.pk)
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "queued")
        self.assertEqual(agent_run.transitionReason, "execution_queue_expired")

    def test_existing_api_job_still_cancels_after_authority_revocation(self):
        agent_run = self.api_run()
        self.membership.delete()
        self.assert_api_cancellation(agent_run, "agent_run_api_authority_revoked")

    def test_existing_api_job_still_cancels_at_its_deadline(self):
        agent_run = self.api_run()
        deadline = timezone.now() + timedelta(minutes=11)
        with patch("app_core.http.internal.timezone.now", return_value=deadline):
            self.assert_api_cancellation(agent_run, "agent_run_api_deadline_expired")

    def assert_api_cancellation(self, agent_run, reason):
        with patch("app_core.http.internal.request_agent_run_cancellation") as cancel:
            result, schedule = self.reconcile(self.job(agent_run, status="queued"))
        self.assertEqual(result["pending"], 0)
        schedule.assert_not_called()
        cancel.assert_called_once()
        self.assertEqual(cancel.call_args.args[0].pk, agent_run.pk)
        self.assertEqual(cancel.call_args.kwargs, {"require_current_authority": False})
        agent_run.refresh_from_db()
        self.assertEqual(agent_run.status, "queued")
        self.assertEqual(agent_run.transitionReason, reason)


class PlatformReconcilerTests(SimpleTestCase):
    cursor = {"createdAt": "2026-11-11T00:00:00Z", "id": "agent_run_fixture"}

    @staticmethod
    def active(next_cursor=None):
        return {"scheduled": 0, "terminalized": 0, "pending": 0,
                "activeNext": next_cursor, "deadLetterNext": None}

    @staticmethod
    def cleanup(next_cursor=None):
        return {"schema": "runtime.agent_run_cleanup.reconciled.v1", "checked": 0,
                "pending": [], "next": next_cursor}

    def test_active_page_progress_survives_cleanup_failure_and_restart_starts_a_new_pass(self):
        calls = []
        fail_cleanup = True

        def post(path, body):
            nonlocal fail_cleanup
            calls.append((path, copy.deepcopy(body)))
            if path.endswith("/reconcile"):
                return self.active(self.cursor if len(calls) == 1 else None)
            if fail_cleanup:
                fail_cleanup = False
                raise RuntimeError("unavailable")
            return self.cleanup()

        reconciler = Reconciler(post)
        with self.assertRaises(RuntimeError):
            reconciler.once()
        reconciler.once()
        Reconciler(post).once()
        active = [body for path, body in calls if path.endswith("/reconcile")]
        self.assertIsNone(active[0]["activeAfter"])
        self.assertEqual(active[1]["activeAfter"], self.cursor)
        self.assertIsNone(active[2]["activeAfter"])

    def test_cleanup_page_is_retried_from_its_last_successful_cursor(self):
        bodies = []
        pages = iter([self.cleanup(self.cursor), RuntimeError("offline"), self.cleanup()])

        def post(path, body):
            if path.endswith("/reconcile"):
                return self.active()
            bodies.append(copy.deepcopy(body))
            result = next(pages)
            if isinstance(result, Exception):
                raise result
            return result

        reconciler = Reconciler(post)
        reconciler.once()
        with self.assertRaises(RuntimeError):
            reconciler.once()
        reconciler.once()
        self.assertEqual([body["after"] for body in bodies], [None, self.cursor, self.cursor])

    def test_unsupported_response_does_not_advance_a_cursor(self):
        for response in (self.active() | {"unexpected": True}, self.active() | {"scheduled": True},
                         self.active({"id": "missing-date"})):
            reconciler = Reconciler(lambda *_: response)
            with self.subTest(response=response), self.assertRaises(ValueError):
                reconciler.once()
            self.assertIsNone(reconciler.active_after)

    def test_failed_control_plane_request_logs_no_secret_or_exception_text(self):
        stopped = threading.Event()

        def post(*_):
            stopped.set()
            raise urllib.error.URLError("private-token-and-url")

        output = io.StringIO()
        with patch("sys.stderr", output):
            run(Reconciler(post), stopped)
        self.assertEqual(output.getvalue(), "Platform reconciliation unavailable\n")

    def test_internal_api_request_reaches_its_target_with_proxy_environment_present(self):
        received = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                received.append((self.path, self.headers["X-Internal-Token"],
                                 json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                body = b'{"checked":0}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            with patch.dict(os.environ, {"HTTP_PROXY": "http://127.0.0.1:1", "HTTPS_PROXY": "http://127.0.0.1:1",
                                         "ALL_PROXY": "http://127.0.0.1:1", "NO_PROXY": ""}):
                client = InternalApi(f"http://127.0.0.1:{server.server_port}", "fixture-token")
                self.assertEqual(client.post("/internal/reconcile", {"limit": 100}), {"checked": 0})
            self.assertEqual(received, [("/internal/reconcile", "fixture-token", {"limit": 100})])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
