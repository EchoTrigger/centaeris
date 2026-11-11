"""Actual Rust authority pins overlap API admission on a migrated PostgreSQL DB."""
import json
import time
from unittest.mock import patch

from django.conf import settings
from django.db import DatabaseError, connection
from django.test import LiveServerTestCase, override_settings

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, Session
from .model_adapter.common import zero_usage
from .runtime_client import schedule_agent_run_lifecycle, request_json
from .test_environment_runtime_fixture import EnvironmentRuntimeFixture


class EnvironmentControllerRuntimeTests(EnvironmentRuntimeFixture, LiveServerTestCase):
    serialized_rollback = True

    def resolve(self, run, owner):
        return self.client.post("/internal/agent-run-lifecycle/resolve",
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            data=json.dumps({"schema": "runtime.agent_run_lifecycle.resolve.v1",
                "jobId": "agent_run.lifecycle:" + run.pk, "agentRunId": run.pk,
                "authorizationDigest": run.authorization.digest, "leaseOwner": owner,
                "executionMode": "retained"}))

    def claim(self, run):
        self.assertEqual(schedule_agent_run_lifecycle(run), "inserted")
        job = self.claim_due_job("agent_run.lifecycle:" + run.pk,
            "agent_run.lifecycle", "worker:controller-pin")
        request_json("/internal/jobs/start", {"schema":"runtime.job.start.v1", "jobId":job["jobId"], "leaseOwner":job["leaseOwner"], "atMs":int(time.time()*1000)}, timeout=5)
        return job

    def test_other_session_resolves_while_rust_physical_callback_pins_authority(self):
        runtime_url = self.start_runtime()
        session = Session.objects.create(workspace=self.workspace, owner=self.user,
            origin="automation")
        second = AgentRun.objects.create(workspace=self.workspace, session=session,
            user=self.user, modelConfig=self.model, prompt="Independent session objective",
            agent_instructions="Session instructions")
        create_agent_run_authorization(second, image_digest=self.run.authorization.payload["environmentProfile"]["imageDigest"])
        self.assertEqual(second.membership_ref, self.run.membership_ref)
        self.assertNotEqual(second.session_id, self.run.session_id)
        with override_settings(RUNTIME_URL=runtime_url):
            first_job = self.claim(self.run)
            second_job = self.claim(second)
            first = self.resolve(self.run, first_job["leaseOwner"])
            self.assertEqual(first.status_code, 200, first.content)
            pin = self.runtime_command("physical-pin:" + json.dumps({
                "agentRunStart": first.json()["agentRunStart"], "leaseOwner": first_job["leaseOwner"]}),
                "environment-runtime-physical-pinned:")
            try:
                self.assertEqual(pin["agentRunId"], self.run.pk)
                # Independent writers prove these are actual Runtime transaction pins.
                observer = connection.copy()
                try:
                    observer.ensure_connection()
                    observer.set_autocommit(True)
                    for table, column, identity in (
                        ("app_core_workspace", "id", self.workspace.pk),
                        ("app_core_workspacemembership", "id", self.run.membership_ref),
                        ("app_core_sessionenvironment", "id", self.run.environment_id),
                        ("runtime.runtime_jobs", "job_id", first_job["jobId"]),
                    ):
                        with self.subTest(pinned=table), observer.cursor() as cursor:
                            with self.assertRaises(DatabaseError) as error:
                                cursor.execute(f"SELECT {column} FROM {table} WHERE {column}=%s FOR UPDATE NOWAIT", [identity])
                            self.assertEqual(error.exception.__cause__.sqlstate, "55P03")
                finally:
                    observer.close()
                # A representative regression must fail the same public admission.
                for table in ("app_core_workspace", "app_core_workspacemembership"):
                    def exclusive_read(execute, sql, params, many, context):
                        if f"FROM {table} " in sql:
                            sql = sql.replace("FOR SHARE NOWAIT", "FOR UPDATE NOWAIT")
                        return execute(sql, params, many, context)
                    with self.subTest(regression=table), connection.execute_wrapper(exclusive_read):
                        rejected = self.resolve(second, second_job["leaseOwner"])
                        self.assertEqual(rejected.status_code, 409, rejected.content)
                        self.assertEqual(rejected.json(), {"error": "environment_controller_busy"})
                    second.environment.refresh_from_db()
                    self.assertEqual(second.environment.controller_epoch, 0)
                admitted = self.resolve(second, second_job["leaseOwner"])
                self.assertEqual(admitted.status_code, 200, admitted.content)
                self.assertEqual(admitted.json()["disposition"], "ready")
                second.environment.refresh_from_db()
                self.assertEqual(second.environment.controller_epoch, 1)
                same_session = self.resolve(self.run, first_job["leaseOwner"])
                self.assertEqual(same_session.status_code, 409, same_session.content)
                self.assertEqual(same_session.json(), {"error": "environment_controller_busy"})
                self.assertIsNone(self.runtime_process.poll())
            finally:
                released = self.runtime_command("release-physical-pin",
                    "environment-runtime-physical-released:")
                self.assertTrue(released["released"])
            # The same public request remains idempotent after the real pin releases.
            self.assertEqual(self.resolve(second, second_job["leaseOwner"]).json(), admitted.json())

    def test_repeated_controller_busy_yields_same_job_without_exhausting_failure_budget(self):
        runtime_url = self.start_runtime()
        job_id = "agent_run.lifecycle:" + self.run.pk

        def response(_model, request):
            self.requests.append(request)
            return {"text": "Completed after authority contention cleared.",
                    "toolCalls": [], "usage": zero_usage()}

        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=response):
            self.assertEqual(schedule_agent_run_lifecycle(self.run), "inserted")
            with connection.cursor() as cursor:
                cursor.execute("UPDATE runtime.runtime_jobs SET retry_count=max_retries WHERE job_id=%s RETURNING max_retries", [job_id])
                max_retries = cursor.fetchone()[0]
            observer = connection.copy()
            observer.ensure_connection()
            observer.set_autocommit(False)
            try:
                with observer.cursor() as cursor:
                    cursor.execute("SELECT id FROM app_core_workspace WHERE id=%s FOR UPDATE", [self.workspace.pk])
                self.runtime_command("start-lifecycle", "environment-runtime-lifecycle:")
                deadline = time.monotonic() + 40
                while True:
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT payload_json::jsonb->>'leaseOwner' FROM runtime.runtime_events WHERE task_id=%s AND event_type='runtime_job_yielded' AND payload_json::jsonb->>'transitionReason'='environment_controller_wait'", [job_id])
                        owners = [row[0] for row in cursor.fetchall()]
                    if len(owners) >= 20:
                        break
                    self.assertLess(time.monotonic(), deadline, "".join(self.runtime_lines))
                    time.sleep(0.02)
                self.assertEqual(len(set(owners)), len(owners))
                persisted = request_json("/internal/jobs/" + job_id, timeout=5)["job"]
                self.assertEqual(persisted["retryCount"], max_retries)
                self.assertIsNone(persisted["lastError"])
                self.assertFalse(self.run.events.filter(payload__type__in=["agent_run_completed", "agent_run_failed", "agent_run_interrupted"]).exists())
            finally:
                observer.rollback()
                observer.close()
            deadline = time.monotonic() + 20
            while True:
                persisted = request_json("/internal/jobs/" + job_id, timeout=5)["job"]
                if persisted["status"] == "succeeded":
                    break
                self.assertIn(persisted["status"], {"queued", "leased", "running"}, persisted)
                self.assertLess(time.monotonic(), deadline, "".join(self.runtime_lines))
                time.sleep(0.02)
            self.assertEqual(persisted["retryCount"], max_retries)
            self.run.refresh_from_db()
            self.assertEqual(self.run.status, "completed")
            self.assert_lifecycle_cleanup(self.run)
