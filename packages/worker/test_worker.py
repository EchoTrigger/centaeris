import os
import http.client
import io
import json
import re
import subprocess
import sys
import threading
import unittest
import urllib.error
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import Mock, patch

os.environ.update(
    {
        "RUNTIME_INTERNAL_URL": "http://runtime.invalid",
        "API_INTERNAL_URL": "http://api.invalid",
        "INTERNAL_API_TOKEN": "test-internal-token",
    }
)

import worker


def lifecycle_fixture(agentRunId="agent_run_1"):
    digest = f"sha256:{'a' * 64}"
    job = {
        "jobId": f"agent_run.lifecycle:{agentRunId}",
        "jobKind": "agent_run.lifecycle",
        "sessionId": "sess_1",
        "payloadRef": f"record:agent_run:{agentRunId}",
        "idempotencyKey": f"agent_run.lifecycle:{agentRunId}:{digest}",
    }
    agentRunStart = {
        "agentRunId": agentRunId,
        "turnId": "turn_fixture",
        "authorizationDigest": digest,
    }
    return job, agentRunStart


class WorkerContractTests(unittest.TestCase):
    def test_observation_keeps_claim_rejection_reason_without_logging_credentials(self):
        output = io.StringIO()
        error = urllib.error.HTTPError("http://runtime.invalid/internal/jobs/claim", 503, "busy", {},
            io.BytesIO(b'{"error":"execution_claim_busy"}'))
        with patch.object(worker.observations, "ENABLED", True), patch("sys.stderr", output), \
                patch.object(worker.urllib.request, "urlopen", side_effect=error):
            with self.assertRaises(worker.DependencyUnavailable) as raised:
                worker.runtime_request("/internal/jobs/claim", {"secret": "do-not-log"})
        self.assertEqual(str(raised.exception), "execution_claim_busy")
        event = json.loads(output.getvalue())
        self.assertEqual(event["httpStatus"], 503)
        self.assertEqual(event["errorCode"], "execution_claim_busy")
        self.assertEqual(event["route"], "/internal/jobs/claim")
        self.assertNotIn("do-not-log", output.getvalue())
        self.assertNotIn("test-internal-token", output.getvalue())

    def test_terminal_dispatcher_does_not_acknowledge_partial_fanout(self):
        cursor = {"checkpointId": "checkpoint:dense", "toolCallId": "call:0255"}
        calls = []
        pages = iter([
            {"disposition": "woken", "checked": 256, "waiters": [], "next": cursor},
            {"disposition": "woken", "checked": 1, "waiters": [], "next": None},
        ])
        def runtime(path, body):
            calls.append((path, body))
            if path.endswith("/pending"):
                return {"events": [{"jobId": "job_1", "eventType": "runtime_job.terminal", "publishedAtMs": None, "generation": 0}]}
            if path.endswith("/wake-waiter"):
                return next(pages)
            return {"disposition": "published"}
        with patch.object(worker, "runtime_request", side_effect=runtime):
            dispatch = worker.TerminalDispatcher()
            dispatch()
            self.assertFalse(any(path.endswith("/published") for path, _ in calls))
            dispatch()
        wakes = [body for path, body in calls if path.endswith("/wake-waiter")]
        self.assertIsNone(wakes[0]["after"])
        self.assertEqual(wakes[1]["after"], cursor)
        self.assertEqual(sum(path.endswith("/published") for path, _ in calls), 1)
        self.assertEqual(dispatch.cursors, {})

    def test_waiter_reconcile_preserves_cursor_across_request_failure(self):
        cursor = {"checkpointId": "checkpoint:dense", "toolCallId": "call:0255"}
        replies = iter([
            {"disposition": "reconciled", "checked": 256, "waiters": [], "next": cursor},
            RuntimeError("waiter unavailable"),
            {"disposition": "reconciled", "checked": 1, "waiters": [], "next": None},
        ])
        after = []
        def runtime(path, body):
            if path.endswith("reconcile-waiters"):
                after.append(body["after"])
                reply = next(replies)
                if isinstance(reply, Exception):
                    raise reply
                return reply
            return {"reclaimed": 0}
        with patch.object(worker, "runtime_request", side_effect=runtime), patch.object(worker, "api_request", return_value={"activeNext": None, "deadLetterNext": None}):
            scan = worker.LifecycleReconciler()
            scan()
            with self.assertRaisesRegex(RuntimeError, "waiter unavailable"):
                scan()
            scan()
        self.assertEqual(after, [None, cursor, cursor])
        self.assertIsNone(scan.waiter_after)

    def test_recovery_scan_retries_same_page_when_api_call_fails(self):
        cursor = {"createdAt": "2026-09-06T00:00:00+00:00", "id": "agent_run_last"}
        with patch.object(worker, "runtime_request", return_value={"disposition": "reconciled", "checked": 0, "waiters": [], "next": None}), patch.object(
            worker, "api_request", side_effect=[
                {"activeNext": cursor, "deadLetterNext": None},
                worker.DependencyUnavailable("api unavailable"),
                {"activeNext": None, "deadLetterNext": None},
                {"activeNext": None, "deadLetterNext": None},
            ]
        ) as api:
            scan = worker.LifecycleReconciler()
            scan()
            with self.assertRaises(worker.DependencyUnavailable):
                scan()
            scan()
            scan()
        bodies = [call.args[1] for call in api.call_args_list]
        self.assertEqual(bodies[1]["activeAfter"], cursor)
        self.assertEqual(bodies[2]["activeAfter"], cursor)
        self.assertIsNone(bodies[3]["activeAfter"])

    def test_recovery_scan_progress_survives_later_waiter_failure_and_resets_on_restart(self):
        cursor = {"createdAt": "2026-09-06T00:00:00+00:00", "id": "agent_run_last"}
        responses = [
            {"activeNext": cursor, "deadLetterNext": None},
            {"activeNext": None, "deadLetterNext": cursor},
            {"activeNext": None, "deadLetterNext": None},
        ]
        def runtime(path, body):
            if path.endswith("reconcile-waiters"):
                raise RuntimeError("waiter temporarily unavailable")
            return {"reclaimed": 0}
        with patch.object(worker, "runtime_request", side_effect=runtime), patch.object(
            worker, "api_request", side_effect=responses
        ) as api:
            scan = worker.LifecycleReconciler()
            for operation in (scan, scan, worker.LifecycleReconciler()):
                with self.assertRaisesRegex(RuntimeError, "waiter temporarily unavailable"):
                    operation()
        bodies = [call.args[1] for call in api.call_args_list]
        self.assertIsNone(bodies[0]["activeAfter"])
        self.assertEqual(bodies[1]["activeAfter"], cursor)
        self.assertIsNone(bodies[2]["activeAfter"])
        self.assertIsNone(bodies[2]["deadLetterAfter"])

    def test_slot_configuration_at_startup(self):
        for value, expected in ((None, 8), ("1", 1), ("4", 4), ("16", 16)):
            with self.subTest(value=value):
                env = dict(os.environ)
                env.pop("WORKER_SLOT_COUNT", None)
                if value is not None:
                    env["WORKER_SLOT_COUNT"] = value
                result = subprocess.run(
                    [sys.executable, "-c", "import worker; print(worker.WORKER_SLOT_COUNT)"],
                    cwd=os.path.dirname(worker.__file__), env=env,
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), str(expected))
        for value in ("", "0", "-1", "17", "1.5", "abc"):
            with self.subTest(value=value):
                result = subprocess.run(
                    [sys.executable, "-c", "import worker"],
                    cwd=os.path.dirname(worker.__file__),
                    env={**os.environ, "WORKER_SLOT_COUNT": value},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("WORKER_SLOT_COUNT must be an integer between 1 and 16", result.stderr)

    def test_service_bounds_concurrency_and_reuses_slots_after_success_and_failure(self):
        for slots in (1, 4, 16):
            with self.subTest(slots=slots):
                barrier = threading.Barrier(slots)
                lock = threading.Lock()
                calls = {}
                active = peak = completed = 0
                stop = None

                def install_handler(_number, handler):
                    nonlocal stop
                    if callable(handler):
                        stop = handler

                def execute(slot):
                    nonlocal active, peak, completed
                    with lock:
                        calls[slot] = calls.get(slot, 0) + 1
                        attempt = calls[slot]
                        active += 1
                        peak = max(peak, active)
                    try:
                        barrier.wait(timeout=5)
                        if attempt == 1:
                            raise RuntimeError("observed job failure")
                        return True
                    finally:
                        with lock:
                            active -= 1
                            completed += 1
                            if completed == slots * 3:
                                stop(None, None)
                        barrier.wait(timeout=5)

                with (
                    patch.object(worker, "WORKER_SLOT_COUNT", slots),
                    patch.object(worker, "JOB_WAIT_FAILURE_BACKOFF_SECONDS", 0),
                    patch.object(worker.signal, "signal", side_effect=install_handler),
                    patch.object(worker, "run_loop"),
                    patch.object(worker, "execute_next_job", side_effect=execute),
                ):
                    # A timeout also terminates the service if its configured slots cannot progress.
                    watchdog = threading.Timer(10, lambda: stop(None, None))
                    watchdog.start()
                    try:
                        worker.run_worker_service()
                    finally:
                        watchdog.cancel()
                    for thread in threading.enumerate():
                        if thread.name.startswith("workspace-job-slot-"):
                            thread.join(timeout=6)
                            self.assertFalse(thread.is_alive())
                self.assertEqual(peak, slots)
                self.assertEqual(calls, {slot: 3 for slot in range(slots)})
                self.assertEqual(active, 0)

    def test_remote_disconnect_is_dependency_unavailable(self):
        with patch("urllib.request.urlopen", side_effect=http.client.RemoteDisconnected()):
            with self.assertRaisesRegex(
                worker.DependencyUnavailable, "agent_run_step_unavailable"
            ):
                worker.agent_run_step_request({"schema": "runtime.agent_run.step.v1"})

    def test_structured_runtime_failure_is_not_dependency_unavailable(self):
        response = {
            "schema": "runtime.agent_run.step.failure.v1",
            "agentRunId": "agent_run_1",
            "failureClass": "runtime_panic",
            "retryable": False,
            "transitionReason": "runtime_failure_terminalization_failed",
            "error": "runtime_step_failed",
        }
        failure = urllib.error.HTTPError(
            "http://runtime.invalid/agent-runs/step",
            500,
            "Internal Server Error",
            None,
            io.BytesIO(json.dumps(response).encode()),
        )
        with patch("urllib.request.urlopen", side_effect=failure):
            with self.assertRaises(worker.RuntimeStepFailed) as raised:
                worker.agent_run_step_request(
                    {
                        "schema": "runtime.agent_run.step.v1",
                        "agentRunStart": {"agentRunId": "agent_run_1"},
                    }
                )

        self.assertFalse(raised.exception.retryable)
        self.assertEqual(str(raised.exception), "runtime_step_failed")

    def test_deterministic_job_failure_is_terminal_without_retry(self):
        job, _agent_run_start = lifecycle_fixture("agent_run_deterministic_failure")
        job.update({"retryCount": 0, "maxRetries": 10})
        with (
            patch.object(worker, "runtime_request", return_value={"disposition": "failed"}) as request,
            patch.object(worker, "transition_agent_run") as transition,
        ):
            self.assertTrue(
                worker.fail_claimed_job(
                    job,
                    "worker:test-owner",
                    "runtime_step_failed",
                    False,
                )
            )

        self.assertFalse(request.call_args.args[1]["retryable"])
        transition.assert_called_once_with(
            "agent_run_deterministic_failure", "failed", "runtime_step_failed"
        )

    def test_transient_job_failure_schedules_retry(self):
        job, _agent_run_start = lifecycle_fixture("agent_run_transient_failure")
        job.update({"retryCount": 0, "maxRetries": 2})
        with patch.object(
            worker,
            "runtime_request",
            return_value={"disposition": "retry_scheduled"},
        ) as request:
            self.assertFalse(
                worker.fail_job(
                    job,
                    "worker:test-owner",
                    "dependency_unavailable",
                    True,
                )
            )

        self.assertTrue(request.call_args.args[1]["retryable"])

    def test_pending_result_delivery_yields_without_spending_the_failure_budget(self):
        job, start = lifecycle_fixture("agent_run_delivery_pending")
        job.update({"retryCount": 2, "maxRetries": 2})
        waiting = {"schema": "runtime.agent_run.step.result.v1", "agentRunId": start["agentRunId"],
                   "disposition": "waiting", "terminalState": None,
                   "transitionReason": "session_record_commit_unavailable"}

        def api_request(path, *_args):
            if path.endswith("/resolve"):
                return {"schema": "runtime.agent_run_lifecycle.resolved.v1",
                        "disposition": "ready", "agentRunStart": start}
            return {"agentRunId": start["agentRunId"], "state": "running"}

        with (patch.object(worker, "api_request", side_effect=api_request) as api,
              patch.object(worker, "agent_run_step_request", return_value=waiting),
              patch.object(worker, "yield_job") as yielded,
              patch.object(worker, "fail_claimed_job") as failed,
              patch.object(worker, "complete_job") as completed):
            for _ in range(2):
                self.assertFalse(worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", lambda: None))
        self.assertEqual(yielded.call_count, 2)
        failed.assert_not_called()
        completed.assert_not_called()
        self.assertTrue(all(call.args[1]["state"] == "running" for call in api.call_args_list if call.args[0].endswith("/transition")))
        self.assertEqual(job["retryCount"], 2)

    def test_agent_run_step_has_no_worker_socket_deadline(self):
        with patch.object(worker, "json_request", return_value={}) as request:
            worker.agent_run_step_request({"schema": "runtime.agent_run.step.v1"})

        self.assertIsNone(request.call_args.kwargs["timeout"])



    def test_terminal_dispatcher_wakes_waiter_and_publishes_generation(self):
        calls = []

        def runtime_request(path, body=None):
            calls.append((path, body))
            if path.endswith("/pending"):
                return {
                    "events": [
                        {
                            "jobId": "job_1",
                            "eventType": "runtime_job.terminal",
                            "publishedAtMs": None,
                            "generation": 0,
                        }
                    ]
                }
            if path.endswith("/wake-waiter"):
                return {"disposition": "woken", "checked": 1, "waiters": [], "next": None}
            return {"disposition": "published"}

        with patch.object(worker, "runtime_request", side_effect=runtime_request):
            self.assertEqual(worker.dispatch_terminal_once(), 1)

        self.assertEqual(calls[1][0], "/internal/job-outbox/wake-waiter")
        self.assertEqual(
            set(calls[-1][1]),
            {"schema", "jobId", "eventType", "generation", "publishedAtMs"},
        )

    def test_direct_claim_requires_one_canonical_worker_job_kind(self):
        with patch.object(
            worker,
            "runtime_request",
            return_value={"jobs": []},
        ) as request:
            self.assertIsNone(worker.claim_job("agent_run.lifecycle", "worker:test-owner"))

        self.assertEqual(
            request.call_args.args[1],
            {
                "schema": "runtime.job.claim.v1",
                "workerId": "worker:test-owner",
                "jobId": None,
                "jobKind": "agent_run.lifecycle",
                "nowMs": request.call_args.args[1]["nowMs"],
                "leaseMs": worker.LEASE_MS,
                "limit": 1,
            },
        )
        for forbidden in ("provider.poll", "subagent.run", "banana"):
            with self.assertRaisesRegex(RuntimeError, "worker_job_kind_invalid"):
                worker.claim_job(forbidden, "worker:test-owner")

    def test_claimed_job_uses_returned_lease_identity(self):
        job = {"jobId": "worker.noop:claim-owner", "jobKind": "worker.noop",
               "status": "leased", "leaseOwner": "opaque-claim-identity-123"}
        with patch.object(worker, "WORKER_JOB_KINDS", ("worker.noop",)), \
                patch.object(worker, "runtime_request", return_value={"jobs": [job]}) as request, \
                patch.object(worker, "execute_claimed_job") as execute:
            self.assertTrue(worker.execute_next_job(0))
        self.assertNotEqual(request.call_args.args[1]["workerId"], job["leaseOwner"])
        execute.assert_called_once_with(job, job["leaseOwner"])

    def test_job_wait_uses_strict_v1_contract_and_longer_http_timeout(self):
        with patch.object(
            worker,
            "json_request",
            return_value={
                "schema": "runtime.job.wait.result.v1",
                "disposition": "timeout",
                "nextRunAtMs": None,
            },
        ) as request:
            self.assertEqual(worker.wait_for_jobs(), "timeout")

        self.assertEqual(
            request.call_args.args[1],
            {
                "schema": "runtime.job.wait.v1",
                "jobKinds": list(worker.WORKER_JOB_KINDS),
                "waitMs": 20_000,
            },
        )
        self.assertEqual(request.call_args.kwargs["timeout"], 25)
        self.assertGreater(
            worker.JOB_WAIT_HTTP_TIMEOUT_SECONDS,
            worker.JOB_WAIT_MS / 1000,
        )

    def test_job_wait_rejects_noncanonical_response(self):
        with patch.object(
            worker,
            "json_request",
            return_value={
                "schema": "runtime.job.wait.result.v1",
                "disposition": "ready",
                "nextRunAtMs": None,
            },
        ):
            with self.assertRaisesRegex(RuntimeError, "runtime_job_wait_response_invalid"):
                worker.wait_for_jobs()

    def test_notification_path_rescans_without_fixed_sleep_and_is_quantified(self):
        stopped = Mock()
        stopped.is_set.side_effect = [False, False, True]
        fixture_clock_ms = [0]
        events = []

        def execute_next_job(_slot_index):
            events.append(("scan", fixture_clock_ms[0]))
            return len(events) == 3

        def wait_for_jobs():
            fixture_clock_ms[0] += 7
            events.append(("notified", fixture_clock_ms[0]))
            return "ready"

        with (
            patch.object(worker, "execute_next_job", side_effect=execute_next_job),
            patch.object(worker, "wait_for_jobs", side_effect=wait_for_jobs),
        ):
            worker.run_job_loop(0, stopped)

        self.assertEqual(events, [("scan", 0), ("notified", 7), ("scan", 7)])
        stopped.wait.assert_not_called()
        print("worker_notification_pickup_fixture_ms before<=1000 after=7")

    def test_wait_failure_uses_bounded_polling_fallback(self):
        stopped = Mock()
        stopped.is_set.side_effect = [False, True]
        with (
            patch.object(worker, "execute_next_job", return_value=False),
            patch.object(
                worker,
                "wait_for_jobs",
                side_effect=worker.DependencyUnavailable("disconnected"),
            ),
        ):
            worker.run_job_loop(0, stopped)

        stopped.wait.assert_called_once_with(worker.JOB_WAIT_FAILURE_BACKOFF_SECONDS)

    def test_idle_polling_amplification_is_quantified(self):
        baseline_slots = 2
        old_scan_cycles = baseline_slots * 60
        new_wait_requests = baseline_slots * (60_000 // worker.JOB_WAIT_MS)
        old_claim_http = old_scan_cycles * len(worker.WORKER_JOB_KINDS)
        new_claim_http = new_wait_requests * len(worker.WORKER_JOB_KINDS)
        new_total_http = new_wait_requests + new_claim_http

        self.assertEqual((old_scan_cycles, new_wait_requests), (120, 6))
        self.assertEqual(worker.WORKER_JOB_KINDS, ("agent_run.lifecycle", "worker.noop", "agent_work.return"))
        self.assertEqual((old_claim_http, new_claim_http, new_total_http), (360, 18, 24))
        print(
            "worker_idle_per_min "
            f"scan_cycles={old_scan_cycles}->{new_wait_requests} "
            f"claim_http={old_claim_http}->{new_claim_http} "
            f"wait_http=0->{new_wait_requests} total_http={old_claim_http}->{new_total_http}"
        )

    def test_agent_run_lifecycle_wait_yields_without_completing_job(self):
        job, agentRunStart = lifecycle_fixture()
        waiting = {
            "schema": "runtime.agent_run.step.result.v1",
            "agentRunId": agentRunStart["agentRunId"],
            "disposition": "waiting",
            "terminalState": None,
            "transitionReason": "runtime_job_wait",
        }

        def api_request(path, *_args):
            if path.endswith("/resolve"):
                return {
                    "schema": "runtime.agent_run_lifecycle.resolved.v1",
                    "disposition": "ready",
                    "agentRunStart": agentRunStart,
                }
            return {"agentRunId": agentRunStart["agentRunId"], "state": "running"}

        with (
            patch.object(worker, "api_request", side_effect=api_request),
            patch.object(worker, "agent_run_step_request", return_value=waiting) as agentRunStep,
            patch.object(worker, "yield_job") as yield_job,
            patch.object(worker, "complete_job") as complete_job,
            patch.object(worker, "now_ms", return_value=1_000_000),
        ):
            self.assertFalse(
                worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", lambda: None)
            )

        self.assertEqual(
            set(agentRunStep.call_args.args[0]),
            {"schema", "jobId", "leaseOwner", "agentRunStart"},
        )
        self.assertEqual(
            yield_job.call_args.args[2],
            1_000_000 + worker.RUNTIME_JOB_WAIT_RECHECK_MS,
        )
        complete_job.assert_not_called()

    def test_agent_run_lifecycle_accepts_all_canonical_waiting_reasons(self):
        for transition_reason in sorted(worker.AGENT_RUN_WAITING_TRANSITION_REASONS):
            with self.subTest(transition_reason=transition_reason):
                job, agentRunStart = lifecycle_fixture()
                waiting = {
                    "schema": "runtime.agent_run.step.result.v1",
                    "agentRunId": agentRunStart["agentRunId"],
                    "disposition": "waiting",
                    "terminalState": None,
                    "transitionReason": transition_reason,
                }
                if transition_reason == "execution_recovery_checkpoint_committed":
                    waiting["retryAtMs"] = 1_001_200

                def api_request(path, *_args):
                    if path.endswith("/resolve"):
                        return {
                            "schema": "runtime.agent_run_lifecycle.resolved.v1",
                            "disposition": "ready",
                            "agentRunStart": agentRunStart,
                        }
                    return {"agentRunId": agentRunStart["agentRunId"], "state": "running"}

                with (
                    patch.object(worker, "api_request", side_effect=api_request),
                    patch.object(worker, "agent_run_step_request", return_value=waiting),
                    patch.object(worker, "yield_job") as yield_job,
                    patch.object(worker, "complete_job") as complete_job,
                ):
                    if transition_reason == "execution_recovery_checkpoint_committed":
                        deadline = waiting.pop("retryAtMs")
                        with self.assertRaisesRegex(RuntimeError, "agent_run_step_response_invalid"):
                            worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", lambda: None)
                        yield_job.assert_not_called()
                        complete_job.assert_not_called()
                        waiting["retryAtMs"] = deadline
                    self.assertFalse(
                        worker.execute_agent_run_lifecycle_job(
                            job,
                            "worker:test-owner",
                            lambda: None,
                        )
                    )

                yield_job.assert_called_once()
                complete_job.assert_not_called()

    def test_runtime_and_worker_accept_the_same_waiting_reasons(self):
        runtime_source = (Path(__file__).resolve().parents[1] / "runtime_server/src/main.rs").read_text(encoding="utf-8")
        declaration = re.search(
            r"const AGENT_RUN_WAITING_TRANSITION_REASONS: &\[&str\] = &\[(.*?)\];",
            runtime_source,
            re.DOTALL,
        )
        self.assertIsNotNone(declaration, "Runtime waiting reason contract must be discoverable")
        reasons = re.findall(r'"([a-z_]+)"', declaration.group(1))
        self.assertTrue(reasons)
        self.assertEqual(len(reasons), len(set(reasons)))
        self.assertEqual(set(reasons), worker.AGENT_RUN_WAITING_TRANSITION_REASONS)

    def test_checkpointed_execution_recovery_yields_until_runtime_deadline_under_current_lease(self):
        job, start = lifecycle_fixture("agent_run_sandbox_recovery")
        reason = "execution_recovery_checkpoint_committed"
        for lose_lease in (False, True):
            with self.subTest(lose_lease_before_yield=lose_lease):
                calls = []
                lease_checks = 0

                def require_lease():
                    nonlocal lease_checks
                    lease_checks += 1
                    calls.append("lease")
                    if lose_lease and lease_checks == 3:
                        raise RuntimeError("test_lease_lost")

                def transition(_run_id, state, transition_reason):
                    calls.append((state, transition_reason))

                with (
                    patch.object(worker, "api_request", return_value={
                        "schema": "runtime.agent_run_lifecycle.resolved.v1",
                        "disposition": "ready",
                        "agentRunStart": start,
                    }),
                    patch.object(worker, "agent_run_step_request", return_value={
                        "schema": "runtime.agent_run.step.result.v1",
                        "agentRunId": start["agentRunId"],
                        "disposition": "waiting",
                        "terminalState": None,
                        "transitionReason": reason,
                        "retryAtMs": 1_001_200,
                    }),
                    patch.object(worker, "transition_agent_run", side_effect=transition),
                    patch.object(worker, "yield_job", side_effect=lambda *args: calls.append(("yield", args))) as yielded,
                    patch.object(worker, "finish_agent_run_lifecycle") as finish,
                    patch.object(worker, "now_ms", return_value=1_000_000),
                ):
                    if lose_lease:
                        with self.assertRaisesRegex(RuntimeError, "test_lease_lost"):
                            worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", require_lease)
                        yielded.assert_not_called()
                    else:
                        self.assertFalse(worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", require_lease))
                        yielded.assert_called_once_with(job["jobId"], "worker:test-owner", 1_001_200, reason)
                        self.assertEqual(calls[-3:], [("running", reason), "lease", ("yield", yielded.call_args.args)])
                    finish.assert_not_called()
                    self.assertEqual(lease_checks, 3)

    def test_terminal_projects_and_tears_down_before_completing_job(self):
        job, agentRunStart = lifecycle_fixture("agent_run_terminal")
        terminal = {
            "schema": "runtime.agent_run.step.result.v1",
            "agentRunId": agentRunStart["agentRunId"],
            "disposition": "terminal",
            "terminalState": "completed",
            "transitionReason": "runtime_session_terminal_committed",
        }
        calls = []

        def api_request(path, *_args):
            if path.endswith("/resolve"):
                return {
                    "schema": "runtime.agent_run_lifecycle.resolved.v1",
                    "disposition": "ready",
                    "agentRunStart": agentRunStart,
                }
            raise AssertionError(path)

        with (
            patch.object(worker, "api_request", side_effect=api_request),
            patch.object(worker, "agent_run_step_request", return_value=terminal),
            patch.object(
                worker,
                "transition_agent_run",
                side_effect=lambda *_args: calls.append("transition"),
            ),
            patch.object(
                worker,
                "runtime_teardown_request",
                side_effect=lambda *_args: calls.append("teardown")
                or {
                    "schema": "runtime.agent_run.teardown.result.v1",
                    "agentRunId": agentRunStart["agentRunId"],
                    "status": "removed",
                },
            ),
            patch.object(
                worker,
                "complete_job",
                side_effect=lambda *_args: calls.append("complete"),
            ),
        ):
            self.assertFalse(
                worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", lambda: None)
            )

        self.assertEqual(calls, ["transition", "transition", "teardown", "complete"])

    def test_committed_terminal_still_requires_teardown_before_job_completion(self):
        job, agentRunStart = lifecycle_fixture("agent_run_recovered_terminal")
        calls = []

        with (
            patch.object(
                worker,
                "api_request",
                return_value={
                    "schema": "runtime.agent_run_lifecycle.resolved.v1",
                    "disposition": "terminal",
                    "terminalState": "failed",
                    "agentRunStart": agentRunStart,
                },
            ),
            patch.object(
                worker,
                "transition_agent_run",
                side_effect=lambda *_args: calls.append("transition"),
            ),
            patch.object(
                worker,
                "runtime_teardown_request",
                side_effect=lambda *_args: calls.append("teardown")
                or {
                    "schema": "runtime.agent_run.teardown.result.v1",
                    "agentRunId": agentRunStart["agentRunId"],
                    "status": "removed",
                },
            ),
            patch.object(
                worker,
                "complete_job",
                side_effect=lambda *_args: calls.append("complete"),
            ),
        ):
            worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", lambda: None)

        self.assertEqual(calls, ["transition", "teardown", "complete"])

    def test_teardown_failure_leaves_lifecycle_job_retryable(self):
        job, agentRunStart = lifecycle_fixture("agent_run_teardown_retry")
        with (
            patch.object(
                worker,
                "api_request",
                return_value={
                    "schema": "runtime.agent_run_lifecycle.resolved.v1",
                    "disposition": "terminal",
                    "terminalState": "completed",
                    "agentRunStart": agentRunStart,
                },
            ),
            patch.object(worker, "transition_agent_run"),
            patch.object(
                worker,
                "runtime_teardown_request",
                side_effect=worker.DependencyUnavailable("sandbox_teardown_failed"),
            ),
            patch.object(worker, "complete_job") as complete_job,
        ):
            with self.assertRaises(worker.DependencyUnavailable):
                worker.execute_agent_run_lifecycle_job(job, "worker:test-owner", lambda: None)

        complete_job.assert_not_called()

    def test_worker_allows_two_sessions_to_run(self):
        slots = 2
        active = 0
        maxActive = 0
        lock = threading.Lock()
        barrier = threading.Barrier(slots)

        @contextmanager
        def lease_heartbeats(_jobId, _leaseOwner):
            nonlocal active, maxActive
            with lock:
                active += 1
                maxActive = max(maxActive, active)
            barrier.wait(timeout=2)
            try:
                yield lambda: None
            finally:
                with lock:
                    active -= 1

        jobs = [
            {
                "jobId": f"worker.noop:{slot}",
                "jobKind": "worker.noop",
            }
            for slot in range(slots)
        ]
        with (
            patch.object(worker, "start_job"),
            patch.object(worker, "complete_job"),
            patch.object(worker, "lease_heartbeats", side_effect=lease_heartbeats),
        ):
            threads = [
                threading.Thread(
                    target=worker.execute_claimed_job,
                    args=(job, f"worker:test-owner-{slot}"),
                )
                for slot, job in enumerate(jobs)
            ]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(timeout=3)

        self.assertEqual(maxActive, 2)


if __name__ == "__main__":
    unittest.main()
