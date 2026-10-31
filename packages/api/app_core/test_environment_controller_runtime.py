"""Actual Rust authority pins overlap API admission on a migrated PostgreSQL DB."""
import json
from unittest.mock import patch

from django.conf import settings
from django.db import DatabaseError, connection
from django.test import LiveServerTestCase, override_settings

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, Session
from .model_adapter.common import fake_tool_call, zero_usage
from .runtime_client import schedule_agent_run_lifecycle
from . import test_native_consumption_production as native_fixture


class EnvironmentControllerRuntimeTests(LiveServerTestCase):
    serialized_rollback = True
    setUp = native_fixture.NativeConsumptionProductionTests.setUp
    start_runtime = native_fixture.NativeConsumptionProductionTests.start_runtime
    stop_runtime = native_fixture.NativeConsumptionProductionTests.stop_runtime
    runtime_command = native_fixture.NativeConsumptionProductionTests.runtime_command
    load_worker = native_fixture.NativeConsumptionProductionTests.load_worker
    claim_due_job = native_fixture.NativeConsumptionProductionTests.claim_due_job
    assert_lifecycle_cleanup = native_fixture.NativeConsumptionProductionTests.assert_lifecycle_cleanup

    def resolve(self, run, owner):
        return self.client.post("/internal/agent-run-lifecycle/resolve",
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            data=json.dumps({"schema": "runtime.agent_run_lifecycle.resolve.v1",
                "jobId": "agent_run.lifecycle:" + run.pk, "agentRunId": run.pk,
                "authorizationDigest": run.authorization.digest, "leaseOwner": owner,
                "executionMode": "retained"}))

    def claim(self, worker, run):
        self.assertEqual(schedule_agent_run_lifecycle(run), "inserted")
        job = self.claim_due_job(worker, "agent_run.lifecycle:" + run.pk,
            "agent_run.lifecycle", "worker:controller-pin")
        worker.start_job(job["jobId"], job["leaseOwner"])
        return job

    def test_other_session_resolves_while_rust_physical_callback_pins_authority(self):
        runtime_url = self.start_runtime()
        worker = self.load_worker(runtime_url)
        session = Session.objects.create(workspace=self.workspace, owner=self.user,
            agent=self.agent, origin="automation")
        second = AgentRun.objects.create(workspace=self.workspace, session=session,
            user=self.user, modelConfig=self.model, prompt="Independent session objective",
            definition_version=self.agent.definition.published_version,
            agent_instructions=self.agent.definition.published_version.instructions)
        create_agent_run_authorization(second, image_digest=self.run.authorization.payload["environmentProfile"]["imageDigest"])
        self.assertEqual(second.membership_ref, self.run.membership_ref)
        self.assertNotEqual(second.session_id, self.run.session_id)
        with override_settings(RUNTIME_URL=runtime_url):
            first_job = self.claim(worker, self.run)
            second_job = self.claim(worker, second)
            first = self.resolve(self.run, first_job["leaseOwner"])
            self.assertEqual(first.status_code, 200, first.content)
            pin = self.runtime_command("physical-pin:" + json.dumps({
                "agentRunStart": first.json()["agentRunStart"], "leaseOwner": first_job["leaseOwner"]}),
                "native-consume-production-physical-pinned:")
            try:
                self.assertEqual(pin["agentRunId"], self.run.pk)
                # Independent writers prove these are actual Runtime transaction pins.
                observer = connection.copy(alias="runtime-pin-observer")
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
                    "native-consume-production-physical-released:")
                self.assertTrue(released["released"])
            # The same public request remains idempotent after the real pin releases.
            self.assertEqual(self.resolve(second, second_job["leaseOwner"]).json(), admitted.json())

    def test_repeated_controller_busy_yields_same_job_without_exhausting_failure_budget(self):
        runtime_url = self.start_runtime()
        worker = self.load_worker(runtime_url)
        job_id = "agent_run.lifecycle:" + self.run.pk
        with override_settings(RUNTIME_URL=runtime_url):
            self.assertEqual(schedule_agent_run_lifecycle(self.run), "inserted")
            with connection.cursor() as cursor:
                cursor.execute("UPDATE runtime.runtime_jobs SET retry_count=max_retries WHERE job_id=%s RETURNING max_retries", [job_id])
                max_retries = cursor.fetchone()[0]
            observer = connection.copy(alias="controller-busy-observer")
            observer.ensure_connection()
            observer.set_autocommit(True)
            try:
                with observer.cursor() as cursor:
                    cursor.execute("BEGIN")
                    cursor.execute("SELECT id FROM app_core_sessionenvironment WHERE id=%s FOR UPDATE", [self.run.environment_id])
                owners = set()
                for attempt in range(20):
                    with self.subTest(attempt=attempt):
                        claimed = self.claim_due_job(worker, job_id, "agent_run.lifecycle", "worker:busy-repeat")
                        self.assertEqual(claimed["retryCount"], max_retries)
                        self.assertNotIn(claimed["leaseOwner"], owners)
                        owners.add(claimed["leaseOwner"])
                        before_yield = worker.now_ms()
                        worker.execute_claimed_job(claimed, claimed["leaseOwner"])
                        persisted = worker.runtime_request("/internal/jobs/" + job_id)["job"]
                        self.assertEqual(persisted["jobId"], job_id)
                        self.assertEqual(persisted["status"], "queued", persisted)
                        self.assertEqual(persisted["retryCount"], max_retries)
                        self.assertIsNone(persisted["leaseOwner"])
                        self.assertIsNone(persisted["lastError"])
                        self.assertGreater(persisted["runAtMs"], before_yield)
                        self.assertEqual(persisted["payloadRef"], "record:agent_run:" + self.run.pk)
                with connection.cursor() as cursor:
                    cursor.execute("SELECT payload_json::jsonb->>'transitionReason' FROM runtime.runtime_events WHERE task_id=%s AND event_type='runtime_job_yielded'", [job_id])
                    self.assertEqual([row[0] for row in cursor.fetchall()], ["environment_controller_wait"] * 20)
                self.assertFalse(self.run.events.filter(payload__type__in=["agent_run_completed", "agent_run_failed", "agent_run_interrupted"]).exists())
            finally:
                with observer.cursor() as cursor:
                    cursor.execute("ROLLBACK")
                observer.close()
            # Releasing the writer lets the same durable Job and Run finish.
            claimed = self.claim_due_job(worker, job_id, "agent_run.lifecycle", "worker:busy-recovered")
            def response(_model, request):
                self.requests.append(request)
                if len(self.requests) == 1:
                    return fake_tool_call("busy-recovered-reply", "send_message", {
                        "body": "Completed after authority contention cleared.",
                        "session_refs": [], "file_refs": []})
                return {"text": "Completed after authority contention cleared.",
                    "toolCalls": [], "usage": zero_usage()}
            with patch("app_core.model_adapter.fake_model_response", side_effect=response):
                worker.execute_claimed_job(claimed, claimed["leaseOwner"])
            persisted = worker.runtime_request("/internal/jobs/" + job_id)["job"]
            self.assertEqual(persisted["status"], "succeeded", persisted)
            self.assertEqual(persisted["retryCount"], max_retries)
            self.run.refresh_from_db()
            self.assertEqual(self.run.status, "completed", list(self.run.events.filter(
                payload__type__in=["agent_run_completed", "agent_run_failed", "agent_run_interrupted"]
                ).values_list("payload", flat=True)))
            self.assert_lifecycle_cleanup(self.run)
