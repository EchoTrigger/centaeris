"""The production Rust dispatcher advances Runs without a Python Worker."""
import json
import os
import threading
import time
from unittest.mock import patch
import psycopg

from django.db import connection
from django.test import LiveServerTestCase, override_settings

from . import runtime_client
from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, Session
from .model_adapter.common import zero_usage
from .test_environment_runtime_fixture import EnvironmentRuntimeFixture


class NativeLifecycleRuntimeTests(EnvironmentRuntimeFixture, LiveServerTestCase):
    serialized_rollback = True

    def start_native(self):
        self.assertEqual(self.runtime_command("start-lifecycle", "environment-runtime-lifecycle:"),
                         {"started": True})

    def job(self):
        with connection.cursor() as cursor:
            cursor.execute("SELECT status,lease_owner,lease_expires_at_ms,retry_count,heartbeat_at_ms "
                           "FROM runtime.runtime_jobs WHERE job_id=%s", ["agent_run.lifecycle:" + self.run.pk])
            return cursor.fetchone()

    def wait_for(self, predicate):
        deadline = time.monotonic() + 20
        while True:
            value = predicate()
            if value:
                return value
            self.assertIsNone(self.runtime_process.poll(), "".join(self.runtime_lines))
            self.assertLess(time.monotonic(), deadline, "".join(self.runtime_lines))
            time.sleep(0.02)

    def completed(self):
        row = self.job()
        self.assertNotIn(row[0], {"failed", "dead_lettered", "cancelled"}, "".join(self.runtime_lines))
        return row if row[0] == "succeeded" else None

    @staticmethod
    def response(_model, _request):
        return {"text": "Completed by the native lifecycle executor.", "toolCalls": [], "usage": zero_usage()}

    def test_native_execution_renews_its_lease_and_releases_before_job_completion(self):
        entered, release = threading.Event(), threading.Event()

        def response(model, request):
            entered.set()
            if not release.wait(15):
                raise RuntimeError("test model was not released")
            return self.response(model, request)

        with patch.dict(os.environ, {"RUNTIME_JOB_LEASE_MS": "1000"}):
            runtime_url = self.start_runtime()
        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=response):
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            self.start_native()
            try:
                self.assertTrue(entered.wait(10), "".join(self.runtime_lines))
                first = self.job()
                renewed = self.wait_for(lambda: (row if (row := self.job())[4] > first[4] else None))
                self.assertEqual(renewed[0:2], first[0:2])
                self.assertGreater(renewed[2], first[2])
            finally:
                release.set()
            self.assertEqual(self.wait_for(self.completed)[3], 0)
            self.run.refresh_from_db()
            self.assertEqual(self.run.status, "completed")
            self.assert_lifecycle_cleanup(self.run)

    def test_native_control_settles_queued_cancellation_without_model_execution(self):
        runtime_url = self.start_runtime()
        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response") as model:
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            runtime_client.request_agent_run_cancellation(self.run)
            self.start_native()
            self.wait_for(self.completed)
            self.run.refresh_from_db()
            self.assertEqual(self.run.status, "cancelled")
            model.assert_not_called()
            with connection.cursor() as cursor:
                cursor.execute("SELECT payload_json FROM runtime.runtime_events WHERE event_id=%s",
                               ["agent_run_no_effects:" + self.run.pk])
                self.assertIsNotNone(cursor.fetchone())

    def test_native_completed_session_purge_erases_body_after_release(self):
        runtime_url = self.start_runtime()
        authorization = self.run.authorization.payload
        with override_settings(RUNTIME_URL=runtime_url), patch(
            "app_core.model_adapter.fake_model_response", side_effect=self.response,
        ):
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            self.start_native()
            self.wait_for(self.completed)
            self.run.refresh_from_db()
            self.assertEqual(self.run.status, "completed")
            self.assert_lifecycle_cleanup(self.run)
            retired = self.runtime_command("finish-host-retirements", "environment-runtime-host-retirements:")
            self.assertGreater(retired["processed"], 0)
            self.client.force_login(self.user)
            deleted = self.client.delete(f"/api/sessions/{self.source.pk}")
            self.assertEqual(deleted.status_code, 200, deleted.content.decode() + "".join(self.runtime_lines))
            self.assertEqual(deleted.json(), {"deleted": True})
            self.source.refresh_from_db()
            self.run.refresh_from_db()
            self.assertEqual(self.source.status, "deleted")
            self.assertIsNotNone(self.source.purgedAt)
            self.assertEqual((self.run.prompt, self.run.agent_instructions), ("", ""))
            self.assertEqual(self.run.authorization.payload, authorization)
            self.assertEqual(self.client.get(f"/api/sessions/{self.source.pk}").status_code, 404)
            with connection.cursor() as cursor:
                for table in ("session_runtime_snapshots", "runtime_events", "runtime_jobs"):
                    cursor.execute(f"SELECT count(*) FROM runtime.{table} WHERE session_id=%s", [self.source.pk])
                    self.assertEqual(cursor.fetchone()[0], 0, table)
                cursor.execute("SELECT count(*) FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=%s", [self.run.pk])
                self.assertGreater(cursor.fetchone()[0], 0)
            repeated = self.client.delete(f"/api/sessions/{self.source.pk}")
            self.assertEqual(repeated.status_code, 200, repeated.content)

    def test_native_parallel_children_keep_parent_authority_through_partial_completion(self):
        entered = [threading.Event() for _ in range(3)]
        release = [threading.Event() for _ in range(3)]
        lock = threading.Lock()
        parent_requests = 0
        child_requests = [0, 0, 0]
        actual_requests = []

        def response(model, request):
            nonlocal parent_requests
            messages = request["preparedPrompt"]["messages"]
            user_content = "\n".join(message["content"] for message in messages
                                     if message["role"] == "user")
            with lock:
                actual_requests.append({"agentRunId": request["agentRunId"], "userContent": user_content})
            for index in range(3):
                if f"Objective: native-child-{index}" not in user_content:
                    continue
                with lock:
                    child_requests[index] += 1
                entered[index].set()
                if not release[index].wait(20):
                    raise RuntimeError(f"native child {index} was not released")
                return {"text": f"native-child-result-{index}", "toolCalls": [], "usage": zero_usage()}
            with lock:
                parent_requests += 1
                turn = parent_requests
            if turn == 1:
                references = self.runtime_command("seed-read-only-subagents", "environment-runtime-read-only-subagents:")
                self.assertEqual(len(references), 3)
                calls = [{"id": f"wait-native-child-{index}", "name": "task_output",
                          "argsJson": json.dumps({"output_ref": reference})}
                         for index, reference in enumerate(references)]
                return {"text": "", "toolCalls": calls, "usage": zero_usage()}
            self.assertEqual(turn, 2)
            results = "\n".join(message["content"] for message in messages if message["role"] == "tool")
            for index in range(3):
                self.assertIn(f"native-child-result-{index}", results)
            return self.response(model, request)

        def children():
            with connection.cursor() as cursor:
                cursor.execute("SELECT job_id,status,lease_owner,last_error FROM runtime.runtime_jobs "
                               "WHERE job_kind='subagent.run' AND session_id=%s ORDER BY job_id",
                               [self.source.pk])
                return cursor.fetchall()

        def diagnostics():
            with lock:
                requests = list(actual_requests)
            return (f"parent={self.job()!r}; children={children()!r}; "
                    f"entered={[event.is_set() for event in entered]!r}; "
                    f"modelRequests={requests!r}\n" + "".join(self.runtime_lines))

        def one_child_completed():
            rows = children()
            return rows if sum(row[1] == "succeeded" for row in rows) == 1 else None

        def parent_waiting():
            terminal = self.run.events.filter(payload__type__in=[
                "agent_run_completed", "agent_run_failed", "agent_run_interrupted",
            ]).exists()
            if terminal:
                results = list(self.run.events.filter(payload__type="tool_result")
                    .values_list("payload__payload", flat=True))
                self.fail(f"Parent terminated before its wait: requests={parent_requests}, "
                          f"children={children()!r}, toolResults={results!r}, "
                          f"runtime={''.join(self.runtime_lines)}")
            with connection.cursor() as cursor:
                cursor.execute("SELECT count(*) FROM runtime.runtime_job_waiters "
                               "WHERE session_id=%s AND agent_run_id=%s", [self.source.pk, self.run.pk])
                return cursor.fetchone()[0] == 3

        def controller():
            self.run.environment.refresh_from_db()
            environment = self.run.environment
            return (environment.controller_run_id, environment.controller_epoch,
                    environment.controller_lease_owner, environment.controller_purpose,
                    environment.controller_released)

        def consumed_wake():
            with connection.cursor() as cursor:
                cursor.execute("SELECT max(at_ms) FROM runtime.runtime_events WHERE task_id=%s "
                               "AND event_type='runtime_job_wake_consumed' "
                               "AND payload_json::jsonb->>'transitionReason'='owned_wait_observed_pending_wake'",
                               ["agent_run.lifecycle:" + self.run.pk])
                return cursor.fetchone()[0]

        with patch.dict(os.environ, {"RUNTIME_JOB_LEASE_MS": "1000"}):
            runtime_url = self.start_runtime()
        with override_settings(RUNTIME_URL=runtime_url), patch(
            "app_core.model_adapter.fake_model_response", side_effect=response,
        ):
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            self.start_native()
            try:
                self.wait_for(parent_waiting)
                first = self.job()
                self.assertEqual(first[0], "running")
                authority = controller()
                renewed = self.wait_for(lambda: (row if (row := self.job())[4] > first[4] else None))
                self.assertEqual(renewed[0:2], first[0:2])
                self.assertEqual([row[1] for row in children()], ["queued"] * 3)
                self.assertEqual(self.runtime_command("start-subagent-worker", "environment-runtime-subagent-worker:"),
                                 {"started": True})
                for child_entered in entered:
                    ready = child_entered.wait(10)
                    self.assertTrue(ready, diagnostics() if not ready else "")
                self.assertEqual([row[1] for row in children()], ["running"] * 3)
                release[0].set()
                partial = self.wait_for(one_child_completed)
                wake_at = self.wait_for(consumed_wake)
                after_wake = self.wait_for(lambda: (row if (row := self.job())[4] > wake_at else None))
                self.assertEqual(after_wake[0:2], first[0:2])
                self.assertEqual(controller(), authority)
                self.assertEqual(sorted(row[1] for row in partial), ["running", "running", "succeeded"])
                self.assertTrue(all(row[2] for row in partial if row[1] == "running"))
                with connection.cursor() as cursor:
                    cursor.execute("SELECT count(*) FROM runtime.runtime_events WHERE task_id=%s "
                                   "AND event_type='runtime_job_yielded'", ["agent_run.lifecycle:" + self.run.pk])
                    self.assertEqual(cursor.fetchone()[0], 0)
                self.assertEqual(parent_requests, 1)
            finally:
                for child_release in release:
                    child_release.set()
            self.wait_for(self.completed)
            self.assertEqual([row[1] for row in children()], ["succeeded"] * 3)
            self.assertEqual(child_requests, [1, 1, 1])
            self.run.refresh_from_db()
            terminal = list(self.run.events.filter(payload__type__in=[
                "agent_run_completed", "agent_run_failed", "agent_run_interrupted",
            ]).values_list("payload", flat=True))
            self.assertEqual(self.run.status, "completed", f"terminal={terminal!r}; {diagnostics()}")
            self.assertEqual(parent_requests, 2)
            self.assert_lifecycle_cleanup(self.run)

    def test_native_default_writable_children_return_every_result(self):
        parent_requests = 0
        child_requests = [0, 0, 0]

        def response(_model, request):
            nonlocal parent_requests
            messages = request["preparedPrompt"]["messages"]
            objective = "\n".join(message["content"] for message in messages if message["role"] == "user")
            for index in range(3):
                if f"Objective: default-native-child-{index}" in objective:
                    child_requests[index] += 1
                    return {"text": f"default-child-result-{index}", "toolCalls": [], "usage": zero_usage()}
            parent_requests += 1
            if parent_requests == 1:
                calls = [{"id": f"start-default-child-{index}", "name": "agent",
                          "argsJson": json.dumps({"prompt": f"default-native-child-{index}",
                                                  "description": f"Default child {index}"})}
                         for index in range(3)]
                return {"text": "", "toolCalls": calls, "usage": zero_usage()}
            if parent_requests == 2:
                starts = [json.loads(message["content"]) for message in messages if message["role"] == "tool"]
                self.assertEqual(len(starts), 3)
                calls = []
                for index, start in enumerate(starts):
                    reference = start["outputRef"]
                    calls.append({"id": f"wait-default-child-{index}", "name": "task_output",
                                  "argsJson": json.dumps({"output_ref": {
                                      "schema": reference["schema"], "kind": reference["kind"],
                                      "runtime_job_id": reference["runtimeJobId"],
                                      "child_session_id": reference["childSessionId"],
                                      "result_ref": reference["resultRef"],
                                  }})})
                return {"text": "", "toolCalls": calls, "usage": zero_usage()}
            self.assertEqual(parent_requests, 3)
            results = "\n".join(message["content"] for message in messages if message["role"] == "tool")
            for index in range(3):
                self.assertIn(f"default-child-result-{index}", results)
            return self.response(_model, request)

        runtime_url = self.start_runtime()
        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=response):
            self.runtime_command("start-subagent-worker", "environment-runtime-subagent-worker:")
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            self.start_native()
            self.wait_for(self.completed)
            self.assertEqual(child_requests, [1, 1, 1])
            self.assertEqual(parent_requests, 3)
            with connection.cursor() as cursor:
                cursor.execute("SELECT status FROM runtime.runtime_jobs WHERE job_kind='subagent.run' AND session_id=%s", [self.source.pk])
                self.assertEqual([row[0] for row in cursor.fetchall()], ["succeeded"] * 3)
            self.assert_lifecycle_cleanup(self.run)

    def test_native_retained_execution_completes_cleanup_without_destroying_the_environment(self):
        with patch.dict(os.environ, {"AGENT_RUN_EXECUTION_MODE": "retained"}):
            runtime_url = self.start_runtime()
        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=self.response):
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            self.start_native()
            self.wait_for(self.completed)
            self.run.refresh_from_db()
            self.assertEqual(self.run.status, "completed")
            self.run.environment.refresh_from_db()
            self.assertTrue(self.run.environment.controller_released)
            proof = self.runtime_command("verify-retained-cleanup:" + self.run.pk,
                                         "environment-runtime-retained-cleanup:")
            self.assertIs(proof["cleanupComplete"], True)
            self.assertIs(proof["physicalPresent"], True)
            self.assertIs(proof["residentChargeRetained"], True)

    def test_native_reclaims_an_expired_attempt_without_replaying_execution(self):
        runtime_url = self.start_runtime()
        executing_owners = []

        def response(model, request):
            executing_owners.append(self.job()[1])
            return self.response(model, request)

        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=response):
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            job_id = "agent_run.lifecycle:" + self.run.pk
            job = runtime_client.request_json("/internal/jobs/claim", {
                "schema": "runtime.job.claim.v1", "workerId": "worker:expired-attempt",
                "jobId": job_id, "jobKind": "agent_run.lifecycle",
                "nowMs": int(time.time() * 1000), "leaseMs": 60000, "limit": 1,
            }, timeout=5)["jobs"][0]
            with connection.cursor() as cursor:
                cursor.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=1 WHERE job_id=%s", [job_id])
            self.start_native()
            self.wait_for(self.completed)
            self.assertEqual(len(executing_owners), 1)
            self.assertNotEqual(executing_owners[0], job["leaseOwner"])
            self.assertEqual(self.job()[3], 0)
            with connection.cursor() as cursor:
                cursor.execute("SELECT count(*) FROM runtime.runtime_events WHERE task_id=%s "
                               "AND event_type='runtime_job_lease_reclaimed'", [job_id])
                self.assertEqual(cursor.fetchone()[0], 1)
            self.assert_lifecycle_cleanup(self.run)

    def test_native_controller_wait_yields_and_resumes_with_a_new_owner(self):
        runtime_url = self.start_runtime()
        database = connection.settings_dict
        observer = psycopg.connect(dbname=database["NAME"], user=database["USER"],
                                   password=database["PASSWORD"], host=database["HOST"], port=database["PORT"])
        executing_owners = []

        def response(model, request):
            executing_owners.append(self.job()[1])
            return self.response(model, request)

        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=response):
            self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
            try:
                with observer.cursor() as cursor:
                    cursor.execute("SELECT id FROM app_core_workspace WHERE id=%s FOR UPDATE", [self.workspace.pk])
                self.start_native()

                def yielded():
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT payload_json::jsonb->>'leaseOwner' FROM runtime.runtime_events "
                                       "WHERE task_id=%s AND event_type='runtime_job_yielded' "
                                       "AND payload_json::jsonb->>'transitionReason'='environment_controller_wait' LIMIT 1",
                                       ["agent_run.lifecycle:" + self.run.pk])
                        row = cursor.fetchone()
                        return row[0] if row else None

                old_owner = self.wait_for(yielded)
                self.assertEqual(self.job()[3], 0)
            finally:
                observer.rollback()
                observer.close()
            self.wait_for(self.completed)
            self.assertTrue(executing_owners[0])
            self.assertNotEqual(executing_owners[0], old_owner)
            self.assert_lifecycle_cleanup(self.run)

    def test_native_control_progresses_while_twelve_ordinary_runs_are_active(self):
        entered, release = threading.Event(), threading.Event()
        lock = threading.Lock()
        active = 0
        count = 12

        def response(model, request):
            nonlocal active
            with lock:
                active += 1
                if active == count:
                    entered.set()
            if not release.wait(30):
                raise RuntimeError("test models were not released")
            return self.response(model, request)

        capacity = {f"RESIDENT_{scope}_{resource}": str(value)
                    for scope in ("GLOBAL", "TENANT")
                    for resource, value in (("MEMORY_BYTES", 64 * 1024 ** 3),
                                            ("WORKSPACE_BYTES", 64 * 1024 ** 3),
                                            ("CPU_MILLI", 32000), ("PIDS", 16384))}
        with patch.dict(os.environ, {**capacity, "AGENT_RUN_LIFECYCLE_RECHECK_MS": "100"}):
            runtime_url = self.start_runtime()
        runs = []
        for _ in range(count):
            session = Session.objects.create(workspace=self.workspace, owner=self.user)
            run = AgentRun.objects.create(workspace=self.workspace, session=session,
                user=self.user, modelConfig=self.model, prompt="Independent native Run.",
                agent_instructions="Finish the objective.")
            create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
            runs.append(run)
        with override_settings(RUNTIME_URL=runtime_url), patch("app_core.model_adapter.fake_model_response", side_effect=response):
            for run in runs:
                self.assertEqual(runtime_client.schedule_agent_run_lifecycle(run), "inserted")
            self.start_native()
            try:
                self.assertTrue(entered.wait(20), "".join(self.runtime_lines))
                self.assertEqual(runtime_client.schedule_agent_run_lifecycle(self.run), "inserted")
                runtime_client.request_agent_run_cancellation(self.run)
                self.wait_for(self.completed)
                self.assertFalse(release.is_set())
                self.assertEqual(active, count)
            finally:
                release.set()

            def all_completed():
                with connection.cursor() as cursor:
                    cursor.execute("SELECT count(*) FROM runtime.runtime_jobs WHERE job_id=ANY(%s) AND status='succeeded'",
                                   [["agent_run.lifecycle:" + run.pk for run in runs]])
                    return cursor.fetchone()[0] == count

            self.wait_for(all_completed)
            for run in runs:
                run.refresh_from_db()
                self.assertEqual(run.status, "completed")
                self.assert_lifecycle_cleanup(run)
