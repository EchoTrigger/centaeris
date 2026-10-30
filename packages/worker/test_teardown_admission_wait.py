"""A confirmed pre-handler teardown rejection retains the original Job."""
from contextlib import contextmanager
import io
import json
import unittest
import urllib.error
from unittest.mock import patch

from test_worker import lifecycle_fixture
import test_environment_host_busy as host_busy
import test_retained_lifecycle
import worker


def rejection(status=503, payload=None):
    return urllib.error.HTTPError("http://runtime.invalid/agent-runs/teardown", status, "busy", None,
        io.BytesIO(json.dumps({"error": "runtime_busy"} if payload is None else payload).encode()))


class TeardownAdmissionWaitTests(unittest.TestCase):
    def exercise(self, mode, *, control=False, just_terminal=False, yield_error=None, lose_lease=False):
        job, start = lifecycle_fixture()
        owner = "worker:fixture-owner"
        job.update(retryCount=0, maxRetries=0, leaseOwner=owner, controlStage="cleanup")
        terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
        ready = {"schema": "runtime.agent_run_lifecycle.resolved.v1", "disposition": "ready", "agentRunStart": start}
        step_result = {"schema": "runtime.agent_run.step.result.v1", "agentRunId": start["agentRunId"],
            "disposition": "terminal", "terminalState": "completed", "transitionReason": "runtime_session_terminal_committed"}
        control_result = {"schema": "runtime.agent_run.control.result.v1", "agentRunId": start["agentRunId"],
            "disposition": "terminal", "terminalState": "completed"}
        blocked = False
        def deny(*_args, **_kwargs):
            nonlocal blocked
            blocked = True
            raise rejection()
        def healthy():
            if blocked and lose_lease:
                raise worker.RuntimeLeaseLost()
        @contextmanager
        def heartbeats(*_args):
            yield healthy
        resolves = 0
        def api(path, *_args):
            nonlocal resolves
            if path.endswith("/control"):
                return {"schema": "runtime.agent_run_lifecycle.control.resolved.v1", "agentRunControl": {"agentRunId": start["agentRunId"]}}
            self.assertTrue(path.endswith("/resolve"))
            resolves += 1
            return ready if just_terminal and resolves == 1 else terminal
        with patch.object(worker, "AGENT_RUN_EXECUTION_MODE", mode), patch.object(worker, "start_job"), \
                patch.object(worker, "lease_heartbeats", heartbeats), patch.object(worker, "api_request", side_effect=api), \
                patch.object(worker, "runtime_request", return_value=control_result), \
                patch.object(worker.urllib.request, "urlopen", side_effect=deny), \
                patch.object(worker, "agent_run_step_request", return_value=step_result) as step, \
                patch.object(worker, "now_ms", return_value=1000), \
                patch.object(worker, "yield_job", side_effect=yield_error) as yielded, \
                patch.object(worker, "fail_job") as failed, patch.object(worker, "complete_job") as complete, \
                patch.object(worker, "transition_agent_run") as transition, patch("sys.stderr"), patch("sys.stdout"):
            repeats = 1 if just_terminal or yield_error is not None or lose_lease else 20
            for _ in range(repeats):
                if control:
                    worker.execute_control_job(job)
                else:
                    worker.execute_claimed_job(job, owner)
        failed.assert_not_called();complete.assert_not_called()
        self.assertEqual(job["retryCount"], 0)
        if lose_lease:
            yielded.assert_not_called()
        else:
            self.assertEqual(yielded.call_count, repeats)
            for call in yielded.call_args_list:
                self.assertEqual(call.args, (job["jobId"], owner, 2000, "execution_host_admission_wait"))
        if just_terminal:
            step.assert_called_once()
            self.assertEqual([call.args[1] for call in transition.call_args_list], ["running", "completed"])
        elif control:
            step.assert_not_called()
            self.assertTrue(all(call.args[1] == "completed" for call in transition.call_args_list))
        else:
            step.assert_not_called();transition.assert_not_called()

    def test_already_terminal_teardown_admission_yields_original_job_without_retry(self):
        for mode in ("perRun", "retained"):
            with self.subTest(mode=mode):self.exercise(mode)

    def test_step_just_committed_terminal_keeps_it_during_teardown_admission_wait(self):
        for mode in ("perRun", "retained"):
            with self.subTest(mode=mode):self.exercise(mode, just_terminal=True)

    def test_control_cleanup_uses_same_original_job_wait(self):
        for mode in ("perRun", "retained"):
            with self.subTest(mode=mode):self.exercise(mode, control=True)

    def test_lost_lease_cannot_yield_or_complete_cleanup_responsibility(self):
        for control in (False, True):
            with self.subTest(control=control):self.exercise("perRun", control=control, lose_lease=True)

    def test_unknown_yield_handoff_does_not_fail_or_attempt_second_yield(self):
        for control in (False, True):
            with self.subTest(control=control):
                self.exercise("perRun", control=control, yield_error=worker.YieldHandoffUnknown(worker.DependencyUnavailable("unavailable")))

    def test_nearby_teardown_errors_keep_original_failure_path(self):
        for status, payload in [(500, {"error": "runtime_busy"}), (502, {"error": "runtime_busy"}),
                (409, {"error": "runtime_busy"}), (503.0, {"error": "runtime_busy"}),
                (503, {"error": "other"}), (503, {"error": "runtime_busy", "extra": True}),
                (503, {"error": "runtime_busy:unknown"}), (None, None)]:
            with self.subTest(status=status, payload=payload):
                job, _ = lifecycle_fixture();job.update(retryCount=0,maxRetries=10)
                terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
                error = OSError("transport unavailable") if status is None else rejection(status,payload)
                with patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", host_busy.heartbeats), \
                        patch.object(worker, "resolve_agent_run_lifecycle", return_value=terminal), \
                        patch.object(worker.urllib.request, "urlopen", side_effect=error), \
                        patch.object(worker, "fail_job", return_value=False) as failed, \
                        patch.object(worker, "yield_job") as yielded, patch.object(worker, "transition_agent_run") as transition:
                    worker.execute_claimed_job(job, "worker:fixture-owner")
                failed.assert_called_once_with(job, "worker:fixture-owner", "retained_cleanup_pending", True)
                yielded.assert_not_called();transition.assert_not_called()

    def test_step_and_api_runtime_busy_keep_dependency_failure(self):
        for route in ("/agent-runs/step", "/internal/agent-run-lifecycle/resolve"):
            with self.subTest(route=route):
                with patch.object(worker.urllib.request,"urlopen",side_effect=rejection()):
                    with self.assertRaises(worker.DependencyUnavailable) as caught:
                        worker.json_request("http://runtime.invalid"+route, {}, "X-Internal-Token", "synthetic", "unavailable")
                self.assertIsNone(worker.environment_cleanup_wait_reason(caught.exception))
        for field,value in (("http_status",None),("route",None),("http_error_code",None)):
            error=worker.DependencyUnavailable("runtime_busy",http_status=503);error.route="/agent-runs/teardown";error.http_error_code="runtime_busy";setattr(error,field,value)
            self.assertIsNone(worker.environment_cleanup_wait_reason(error))
        error=RuntimeError("runtime_busy");error.http_status=503;error.http_error_code="runtime_busy";error.route="/agent-runs/teardown"
        self.assertIsNone(worker.environment_cleanup_wait_reason(error))


if __name__ == "__main__":unittest.main()
