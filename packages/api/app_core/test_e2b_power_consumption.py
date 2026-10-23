"""Explicit product pause/resume acceptance; default discovery is offline only."""
import json
import os
import time
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener
from uuid import uuid4
from unittest.mock import Mock, patch

from django.conf import settings
from django.db import connection
from django.test import LiveServerTestCase, SimpleTestCase, override_settings

from . import test_e2b_retained_consumption as retained_fixture
from .e2b_power_timing_test_support import (PowerTimingCollector, PowerTimingMultiplexer,
    require_power_timing_coverage)


def power_acceptance_enabled(environment):
    return all(environment.get(name) == "1" for name in (
        "CENTAERIS_E2B_PRODUCT_ACCEPTANCE", "CENTAERIS_E2B_RETAINED_ACCEPTANCE",
        "CENTAERIS_E2B_POWER_ACCEPTANCE"))


def require_power_acknowledgement(result, request, sequence):
    try:
        action = request["action"]
        if action not in ("pause", "resume") or type(sequence) is not int or sequence < 1:
            raise ValueError()
        expected = {key: request[key] for key in (
            "operationId", "environmentId", "creationAttemptId", "sandboxId", "action")} | {
            "schema": "runtime.provider_environment.power.result.v1", "sequence": sequence,
            "operationStatus": "acknowledged", "requestedMode": "full" if action == "pause" else None}
        if (not isinstance(result, dict) or result != expected
                or type(result.get("sequence")) is not int):
            raise ValueError()
    except (KeyError, TypeError, ValueError):
        raise AssertionError("e2b_power_acknowledgement_invalid") from None


def power_teardown_is_safe(fixture, database, owner):
    if not retained_fixture.retained_teardown_is_safe(fixture, database, owner):
        return False
    try:
        with database.cursor() as cursor:
            cursor.execute("SELECT to_regclass('runtime.provider_environment_power_operations')")
            if cursor.fetchone()[0] is None:
                return False
            cursor.execute("SELECT count(*) FROM runtime.provider_environment_power_operations WHERE ack_json IS NULL")
            pending = cursor.fetchone()[0]
            return type(pending) is int and pending == 0
    except Exception:
        return False


class PowerAcceptanceGuardTests(SimpleTestCase):
    def request(self, action="pause"):
        return {"schema": "runtime.provider_environment.power.v1", "operationId": "power-operation",
            "environmentId": "environment", "creationAttemptId": "original-create",
            "sandboxId": "original-sandbox", "providerConfigDigest": "sha256:" + "c" * 64,
            "expectedControllerEpoch": 2, "predecessorOperationId": None if action == "pause" else "original-pause",
            "action": action}

    def result(self, action="pause"):
        request = self.request(action)
        return {key: request[key] for key in ("operationId", "environmentId", "creationAttemptId", "sandboxId", "action")} | {
            "schema": "runtime.provider_environment.power.result.v1", "sequence": 1,
            "operationStatus": "acknowledged", "requestedMode": "full" if action == "pause" else None}

    def test_live_power_discovery_requires_all_three_explicit_opt_ins(self):
        names = ("CENTAERIS_E2B_PRODUCT_ACCEPTANCE", "CENTAERIS_E2B_RETAINED_ACCEPTANCE",
                 "CENTAERIS_E2B_POWER_ACCEPTANCE")
        selected = dict.fromkeys(names, "1")
        for missing in names:
            self.assertFalse(power_acceptance_enabled({key: value for key, value in selected.items() if key != missing}))
        self.assertFalse(power_acceptance_enabled(selected | {names[-1]: "true"}))
        self.assertTrue(power_acceptance_enabled(selected))

    def test_exact_ack_preserves_requested_mode_without_claiming_provider_observed_mode(self):
        for action in ("pause", "resume"):
            self.assertIsNone(require_power_acknowledgement(self.result(action), self.request(action), 1))

    def test_pending_foreign_or_incomplete_power_reply_never_becomes_a_positive_ack(self):
        for action in ("pause", "resume"):
            for change in ({"operationStatus": "pending"}, {"operationStatus": "prepared"},
                           {"sequence": 2}, {"sequence": True}, {"sandboxId": "foreign"},
                           {"environmentId": "foreign"}, {"operationId": "foreign"},
                           {"creationAttemptId": "foreign"}, {"action": "other"},
                           {"requestedMode": "filesystem"}, {"observedMode": "full"},
                           {"fullMemoryPauseVerified": True}, {"schema": "old"}):
                with self.subTest(action=action, change=change), self.assertRaises(AssertionError):
                    require_power_acknowledgement(self.result(action) | change, self.request(action), 1)
            for missing in self.result(action):
                incomplete = self.result(action)
                del incomplete[missing]
                with self.subTest(action=action, missing=missing), self.assertRaises(AssertionError):
                    require_power_acknowledgement(incomplete, self.request(action), 1)

    def database(self, *, pending=0, table=True):
        cursor = Mock()
        cursor.fetchone.side_effect = [("runtime.provider_environment_power_operations" if table else None,), (pending,)]
        database = Mock()
        database.cursor.return_value.__enter__ = Mock(return_value=cursor)
        database.cursor.return_value.__exit__ = Mock(return_value=False)
        return database, cursor

    def test_power_teardown_requires_retained_ownership_and_every_operation_acknowledged(self):
        database, cursor = self.database()
        fixture = object()
        with patch.object(retained_fixture, "retained_teardown_is_safe", return_value=True) as retained:
            self.assertTrue(power_teardown_is_safe(fixture, database, "owner"))
            retained.assert_called_once_with(fixture, database, "owner")

    def test_unknown_power_or_unverified_database_preserves_all_business_grants(self):
        with patch.object(retained_fixture, "retained_teardown_is_safe", return_value=False):
            database = Mock()
            self.assertFalse(power_teardown_is_safe(object(), database, "owner"))
            database.cursor.assert_not_called()
        with patch.object(retained_fixture, "retained_teardown_is_safe", return_value=True):
            for fields in ({"pending": 1}, {"pending": None}, {"pending": False},
                           {"pending": "0"}, {"table": False}):
                database, _ = self.database(**fields)
                self.assertFalse(power_teardown_is_safe(object(), database, "owner"))
            database = Mock()
            database.cursor.side_effect = RuntimeError("database unavailable")
            self.assertFalse(power_teardown_is_safe(object(), database, "owner"))


def power_http(runtime_url, body, internal_token, maintenance_token):
    headers = {"Content-Type": "application/json", "X-Internal-Token": internal_token}
    if maintenance_token is not None:
        headers["X-Environment-Maintenance-Token"] = maintenance_token
    request = Request(runtime_url + "/internal/environments/power",
        data=json.dumps(body).encode(), headers=headers, method="POST")
    try:
        try:
            response = build_opener(ProxyHandler({})).open(request, timeout=30)
        except HTTPError as error:
            response = error
        with response:
            encoded = response.read(65537)
            if len(encoded) > 65536:
                raise ValueError()
            return response.status, json.loads(encoded)
    except Exception:
        raise AssertionError("e2b_power_http_unavailable") from None


if power_acceptance_enabled(os.environ):
    class E2BPowerConsumptionProductionTests(LiveServerTestCase):
        """Real Core A/B through independently authorized durable provider power."""
        serialized_rollback = False
        def setUp(self):
            self.power_observations = PowerTimingCollector()
            factory = retained_fixture.product_fixture.workspace_observation_collector

            def combined_collector(environment):
                workspace = factory(environment)
                return PowerTimingMultiplexer(workspace, self.power_observations) if workspace is not None else None

            with patch.object(retained_fixture.product_fixture, "workspace_observation_collector", side_effect=combined_collector):
                retained_fixture.E2BRetainedConsumptionProductionTests.setUp(self)

        load_worker = retained_fixture.E2BRetainedConsumptionProductionTests.load_worker
        start_runtime = retained_fixture.E2BRetainedConsumptionProductionTests.start_runtime
        stop_runtime = retained_fixture.E2BRetainedConsumptionProductionTests.stop_runtime
        model_response = retained_fixture.E2BRetainedConsumptionProductionTests.model_response
        admit = retained_fixture.E2BRetainedConsumptionProductionTests.admit
        claim_due_job = retained_fixture.E2BRetainedConsumptionProductionTests.claim_due_job
        assert_artifact = retained_fixture.E2BRetainedConsumptionProductionTests.assert_artifact
        assert_provider_absent = retained_fixture.E2BRetainedConsumptionProductionTests.assert_provider_absent
        assert_provider_present = retained_fixture.E2BRetainedConsumptionProductionTests.assert_provider_present
        creation_target = retained_fixture.E2BRetainedConsumptionProductionTests.creation_target
        execute_retained = retained_fixture.E2BRetainedConsumptionProductionTests.execute_retained

        def _fixture_teardown(self):
            if not power_teardown_is_safe(self, connection, os.environ.get("CENTAERIS_E2B_PRODUCT_DB_OWNER")):
                print("e2b-power-responsibility:pending", flush=True)
                self.fail("Unresolved or unverified power responsibility remains; owned database must remain")
            super()._fixture_teardown()

        def assert_provider_paused(self, sdk, target):
            try:
                info = sdk.Sandbox.get_info(target["sandboxId"], retries=0)
            except Exception:
                raise AssertionError("e2b_power_paused_identity_unavailable") from None
            self.assertEqual(info.sandbox_id, target["sandboxId"])
            self.assertEqual(info.template_id, self.profile["templateRef"]["templateId"])
            self.assertEqual(info.metadata, target["metadata"])
            self.assertEqual(getattr(info.state, "value", info.state), "paused")

        def power_request(self, target, action, predecessor, epoch):
            return {"schema": "runtime.provider_environment.power.v1", "operationId": "e2b-power-" + uuid4().hex,
                "environmentId": target["environmentId"], "creationAttemptId": target["creationAttemptId"],
                "sandboxId": target["sandboxId"], "providerConfigDigest": target["providerConfigDigest"],
                "expectedControllerEpoch": epoch, "predecessorOperationId": predecessor, "action": action}

        def acknowledged_at(self, request, sequence):
            with connection.cursor() as cursor:
                cursor.execute("SELECT operation_sequence,dispatch_state,acknowledged_at_ms,ack_json "
                    "FROM runtime.provider_environment_power_operations WHERE operation_id=%s", [request["operationId"]])
                row = cursor.fetchone()
            self.assertIsNotNone(row)
            self.assertEqual(row[:2], (sequence, "dispatched"))
            self.assertIs(type(row[2]), int)
            self.assertGreater(row[2], 0)
            self.assertIsNotNone(row[3], "HTTP acknowledgement requires the durable Store fact")
            return row[2]

        def assert_resume_wake(self, run, request, acknowledged_at_ms):
            job_id = "agent_run.lifecycle:" + run.pk
            event_id = "runtime_job_wake:" + job_id + ":provider_power:" + request["operationId"]
            with connection.cursor() as cursor:
                cursor.execute("SELECT event_type,session_id,task_id,at_ms,payload_json "
                    "FROM runtime.runtime_events WHERE event_id=%s", [event_id])
                row = cursor.fetchone()
            self.assertIsNotNone(row, "resume ACK must atomically persist its own exact wake event")
            self.assertEqual(row[:4], ("runtime_job_wake_requested", run.session_id, job_id, acknowledged_at_ms))
            payload = json.loads(row[4])
            self.assertEqual(payload, {"schema": "runtime.provider_environment.power.wake.v1",
                "jobId": job_id, "environmentId": request["environmentId"],
                "sourceOperationId": request["operationId"], "wokenAtMs": acknowledged_at_ms,
                "transitionReason": "provider_environment_resumed"})
            return {"eventId": event_id, "recordedAtUnixMs": row[3], "source": "databaseClock"}

        def park_successor(self, run):
            job_id = "agent_run.lifecycle:" + run.pk
            job = self.claim_due_job(self.worker, job_id, "agent_run.lifecycle", "worker:e2b-power-waiter")
            observed = []
            resolve = self.worker.resolve_agent_run_lifecycle

            def resolve_once(*args):
                result = resolve(*args)
                observed.append(result)
                return result

            self.worker.start_job(job_id, job["leaseOwner"])
            with patch.object(self.worker, "AGENT_RUN_LIFECYCLE_RECHECK_MS", 30000), patch.object(
                    self.worker, "resolve_agent_run_lifecycle", side_effect=resolve_once):
                result = self.worker.execute_agent_run_lifecycle_job(job, job["leaseOwner"], lambda: None)
            self.assertFalse(result)
            self.assertEqual(observed, [{"schema": "runtime.agent_run_lifecycle.resolved.v1",
                "disposition": "waiting", "waitReason": "environment_power_pending"}])
            queued = self.worker.runtime_request("/internal/jobs/" + job_id)["job"]
            self.assertEqual((queued["jobId"], queued["status"]), (job_id, "queued"))
            self.assertGreater(queued["runAtMs"], self.worker.now_ms(), "waiting must yield until a future deadline")
            run.refresh_from_db()
            self.assertEqual(run.status, "queued")
            self.assertFalse(run.events.filter(payload__type__in=("model_request_started", "tool_call",
                "agent_run_failed", "agent_run_completed")).exists())
            with connection.cursor() as cursor:
                cursor.execute("SELECT count(*) FROM runtime.provider_environment_run_uses WHERE agent_run_id=%s", [run.pk])
                self.assertEqual(cursor.fetchone()[0], 0, "a paused Environment must not bind a successor use")
            return queued

        def test_real_e2b_power_queue_restart_resume_and_independent_retirement(self):
            from e2b import E2B
            product = retained_fixture.product_fixture
            sdk = E2B(api_key=os.environ["E2B_API_KEY"], domain=os.environ.get("E2B_DOMAIN"),
                api_url=os.environ["E2B_API_URL"], sandbox_url=os.environ["E2B_SANDBOX_URL"],
                retries=0, debug=False, request_timeout=5)
            with patch("app_core.model_adapter.fake_model_response", side_effect=self.model_response):
                with override_settings(RUNTIME_URL=self.runtime_url):
                    first = self.admit("new", "Write A proof, preserve the environment, and publish delivery A.")
                    target = self.execute_retained(first, "A")
                    self.assert_artifact(first, product.ARTIFACT_A)
                    self.assert_provider_present(sdk, target)
                    session = product.Session.objects.get(pk=first.session_id)
                    first_generation, first_hash = session.workspaceGeneration, session.workspaceSnapshotSha256
                    first.environment.refresh_from_db()
                    pause = self.power_request(target, "pause", None, first.environment.controller_epoch)
                    self.power_observations.bind(pause["operationId"], "pause")
                    status, _ = power_http(self.runtime_url, pause, settings.INTERNAL_API_TOKEN, None)
                    self.assertEqual(status, 403, "ordinary internal authority cannot pause an Environment")
                    self.assert_provider_present(sdk, target)
                    started = time.monotonic()
                    status, paused = power_http(self.runtime_url, pause, settings.INTERNAL_API_TOKEN, self.maintenance_token)
                    self.timings["pauseHttpMs"] = round((time.monotonic() - started) * 1000, 3)
                    self.assertEqual(status, 200)
                    require_power_acknowledgement(paused, pause, 1)
                    pause_ack_ms = self.acknowledged_at(pause, 1)
                    self.assert_provider_paused(sdk, target)

                started = time.monotonic()
                retained_fixture.restart_retained_runtime(self)
                self.timings["pausedRuntimeRestartMs"] = round((time.monotonic() - started) * 1000, 3)
                self.assert_provider_paused(sdk, target)
                with override_settings(RUNTIME_URL=self.runtime_url):
                    second = self.admit(session.pk, "Verify preserved A, append B, and publish delivery B.")
                    self.assertEqual(second.environment_id, first.environment_id)
                    parked = self.park_successor(second)
                    first.environment.refresh_from_db()
                    self.assertTrue(first.environment.controller_released)
                    resume = self.power_request(target, "resume", pause["operationId"], first.environment.controller_epoch)
                    self.power_observations.bind(resume["operationId"], "resume")
                    status, _ = power_http(self.runtime_url, resume, settings.INTERNAL_API_TOKEN, None)
                    self.assertEqual(status, 403, "ordinary internal authority cannot resume an Environment")
                    self.assert_provider_paused(sdk, target)
                    self.lifecycle_calls.marker("resumeRequested")
                    resume_started_unix_ms = time.time_ns() // 1000000
                    started = time.monotonic()
                    status, resumed = power_http(self.runtime_url, resume, settings.INTERNAL_API_TOKEN, self.maintenance_token)
                    self.timings["resumeHttpMs"] = round((time.monotonic() - started) * 1000, 3)
                    self.lifecycle_calls.marker("resumeReturned")
                    self.assertEqual(status, 200)
                    require_power_acknowledgement(resumed, resume, 2)
                    resume_ack_ms = self.acknowledged_at(resume, 2)
                    wake_clock = self.assert_resume_wake(second, resume, resume_ack_ms)
                    self.assert_provider_present(sdk, target)
                    due = self.worker.runtime_request("/internal/jobs/" + parked["jobId"])["job"]
                    self.assertEqual((due["jobId"], due["status"]), (parked["jobId"], "queued"))
                    self.assertLessEqual(due["runAtMs"], self.worker.now_ms(), "resume ACK must durably wake the parked Job")
                    self.assertLess(due["runAtMs"], parked["runAtMs"], "the thirty-second fallback must be bypassed")

                started = time.monotonic()
                retained_fixture.restart_retained_runtime(self)
                self.timings["resumedRuntimeRestartMs"] = round((time.monotonic() - started) * 1000, 3)
                with override_settings(RUNTIME_URL=self.runtime_url):
                    second_target = self.execute_retained(second, "B")
                    self.assertEqual(second_target, target)
                    self.assert_provider_present(sdk, target)
                    self.assertNotEqual(self.retained_uses[first.pk]["providerUseId"], self.retained_uses[second.pk]["providerUseId"])
                    self.assertNotEqual(self.retained_uses[first.pk]["executionId"], self.retained_uses[second.pk]["executionId"])
                    self.assert_artifact(second, product.FILE_B)
                    session.refresh_from_db()
                    self.assertEqual(session.workspaceGeneration, first_generation + 1)
                    self.assertNotEqual(session.workspaceSnapshotSha256, first_hash)
                    first_tool = second.events.filter(payload__type="tool_result").order_by("agent_run_sequence").first()
                    self.assertIsNotNone(first_tool)
                    self.assertEqual(first_tool.payload["payload"]["toolName"], "read")
                    first_tool_clock = {"eventId": first_tool.eventId,
                        "createdAtUnixMs": first_tool.createdAtMs, "source": "coreEventClock"}
                    # Historical ACK replay must not issue another pause or erase
                    # the resumed environment. The original ACK time is stable.
                    self.power_observations.bind_replay(pause["operationId"])
                    status, replay = power_http(self.runtime_url, pause, settings.INTERNAL_API_TOKEN, self.maintenance_token)
                    self.assertEqual(status, 200)
                    self.assertEqual(replay, paused)
                    self.assertEqual(self.acknowledged_at(pause, 1), pause_ack_ms)
                    self.assert_provider_present(sdk, target)
                    second.environment.refresh_from_db()
                    retire = {"schema": "runtime.provider_environment.retire.v1", "operationId": "e2b-power-retire-" + uuid4().hex,
                        "environmentId": target["environmentId"], "creationAttemptId": target["creationAttemptId"],
                        "sandboxId": target["sandboxId"], "providerConfigDigest": target["providerConfigDigest"],
                        "expectedControllerEpoch": second.environment.controller_epoch, "acknowledgeDataLoss": True}
                    started = time.monotonic()
                    status, retired = retained_fixture.retirement_http(self.runtime_url, retire,
                        settings.INTERNAL_API_TOKEN, self.maintenance_token)
                    self.timings["environmentRetirementMs"] = round((time.monotonic() - started) * 1000, 3)
                    self.assertEqual(status, 200)
                    self.assertEqual(retired.get("status"), "removed")
                    self.assert_provider_absent(target["sandboxId"])
                    self.assertTrue(power_teardown_is_safe(self, connection, os.environ.get("CENTAERIS_E2B_PRODUCT_DB_OWNER")))
            self.lifecycle_passed = True
            self.stop_runtime()
            self.assertEqual(self.retained_stream_completions, [True, True, True])
            observations = self.workspace_observations.summary(lifecycle_passed=True,
                stream_drained=all(self.retained_stream_completions))
            retained_fixture.require_retained_workspace_observations(observations, first.pk, second.pk)
            power_timings = self.power_observations.summary(lifecycle_passed=True,
                stream_drained=all(self.retained_stream_completions))
            require_power_timing_coverage(power_timings)
            print("e2b-power-product-measurements:" + json.dumps({"schema": "e2b.power.acceptance.v1",
                "sandboxId": target["sandboxId"], "instanceId": target["instanceId"],
                "runAId": first.pk, "runBId": second.pk, "jobBId": parked["jobId"],
                "pauseOperationId": pause["operationId"], "resumeOperationId": resume["operationId"],
                "pauseAcknowledgedAtUnixMs": pause_ack_ms, "resumeAcknowledgedAtUnixMs": resume_ack_ms,
                "resumeRequestStartedAtUnixMs": resume_started_unix_ms,
                "firstToolResult": first_tool_clock, "toolLatencyClockStatus": "notCorrelated",
                "durableResumeWake": wake_clock, "ackAndWakeRecordedAtomically": True,
                "requestedPauseMode": "full", "memoryPreservation": "notValidated",
                "workspaceNoRestoreBVerified": True, "providerAbsent": True, "pendingDuties": 0,
                "timings": self.timings, "powerTimings": power_timings}, sort_keys=True), flush=True)
