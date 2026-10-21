"""Control processing must progress while all ordinary worker slots remain held."""
from contextlib import contextmanager
import threading
import unittest
from unittest.mock import patch
from test_worker import lifecycle_fixture
import worker


class ControlLifecycleTests(unittest.TestCase):
    def test_control_claim_rejects_execution_or_wrong_stage_authority(self):
        job, _ = lifecycle_fixture()
        for purpose, stage in (("execute", "advance"), ("control", "cleanup")):
            with self.subTest(purpose=purpose, stage=stage):
                candidate = {**job, "status": "leased", "leaseOwner": "worker:control-owner",
                             "leasePurpose": purpose, "controlStage": stage}
                with patch.object(worker, "runtime_request", return_value={"jobs": [candidate]}):
                    with self.assertRaisesRegex(RuntimeError, "control_claim_response_invalid"):
                        worker.claim_control_job("advance", "worker:control-observer")

    def test_terminal_control_hands_off_or_cleans_up_without_execution(self):
        for stage, cleanup_fails in (("advance", False), ("cleanup", False), ("cleanup", True)):
            with self.subTest(stage=stage, cleanup_fails=cleanup_fails):
                job, start = lifecycle_fixture()
                job.update(status="leased", leaseOwner="worker:control-owner", leasePurpose="control", controlStage=stage)
                requests = []
                cleanup = {"schema": "workspace.agent_run.cleanup.v1", "agentRunId": "agent_run_1",
                    "authorizationDigest": start["authorizationDigest"], "authorizationSignature": "fixture",
                    "authorization": {}, "environmentAuthorization": {"payload": {
                        "controllerPurpose": "cleanup", "controllerEpoch": 2}}}

                def runtime(path, body=None, **_kwargs):
                    requests.append((path, body))
                    if path == "/agent-runs/control":
                        return {"schema": "runtime.agent_run.control.result.v1", "agentRunId": "agent_run_1",
                            "disposition": "terminal", "terminalState": "cancelled"}
                    if path == "/agent-runs/teardown":
                        if cleanup_fails:
                            raise RuntimeError("cleanup_unavailable")
                        return {"schema": "runtime.agent_run.teardown.result.v1", "agentRunId": "agent_run_1",
                            "status": "cleanupConfirmed", "cleanupReceiptId": "cleanup:fixture"}
                    if path in {"/internal/jobs/control/start", "/internal/jobs/yield", "/internal/jobs/complete"}:
                        return {"ok": True}
                    self.fail("unexpected Runtime call: " + path)

                def api(path, body, _reason):
                    requests.append((path, body))
                    if path.endswith("/control"):
                        return {"schema": "runtime.agent_run_lifecycle.control.resolved.v1",
                            "agentRunControl": {"agentRunId": "agent_run_1"}}
                    if path.endswith("/transition"):
                        return {"agentRunId": "agent_run_1", "state": "cancelled"}
                    if path.endswith("/resolve"):
                        return {"schema": "runtime.agent_run_lifecycle.resolved.v1", "disposition": "terminal",
                            "terminalState": "cancelled", "agentRunCleanup": cleanup}
                    if path.endswith("/release"):
                        self.assertEqual(body["cleanupReceiptId"], "cleanup:fixture")
                        return {"schema": "runtime.agent_run_lifecycle.released.v1", "agentRunId": "agent_run_1",
                            "releaseReceiptId": "release:fixture"}
                    self.fail("unexpected API call: " + path)

                @contextmanager
                def heartbeats(*_args):
                    yield lambda: None

                with patch.object(worker, "runtime_request", side_effect=runtime), \
                        patch.object(worker, "runtime_teardown_request", side_effect=lambda body: runtime("/agent-runs/teardown", body)), \
                        patch.object(worker, "api_request", side_effect=api), \
                        patch.object(worker, "lease_heartbeats", heartbeats), \
                        patch.object(worker, "agent_run_step_request") as execute:
                    worker.execute_control_job(job)
                paths = [path for path, _ in requests]
                execute.assert_not_called()
                self.assertNotIn("/internal/jobs/fail", paths)
                if stage == "advance":
                    self.assertNotIn("/agent-runs/teardown", paths)
                    self.assertEqual(requests[-1][1]["transitionReason"], "control_cleanup_pending")
                elif cleanup_fails:
                    self.assertNotIn("/internal/jobs/complete", paths)
                    self.assertEqual(requests[-1][1]["transitionReason"], "control_retry_pending")
                else:
                    self.assertLess(paths.index("/agent-runs/teardown"), paths.index("/internal/agent-run-lifecycle/release"))
                    self.assertEqual(paths[-1], "/internal/jobs/complete")
                self.assertEqual(requests[-1][1]["jobId"], job["jobId"])
                self.assertEqual(requests[-1][1]["leaseOwner"], job["leaseOwner"])

    def test_queued_cancel_completes_while_eight_ordinary_slots_stay_held(self):
        job, _ = lifecycle_fixture()
        job.update(status="leased", leaseOwner="worker:control-owner", leasePurpose="control", controlStage="advance")
        held = threading.Event()
        release = threading.Event()
        lock = threading.Lock()
        active = 0
        stop = None
        claimed = False
        completed_with = []
        requests = []

        def signal(_number, handler):
            nonlocal stop
            if callable(handler):
                stop = handler

        def ordinary(_slot):
            nonlocal active
            with lock:
                active += 1
                if active == 8:
                    held.set()
            release.wait(4)
            return True

        def runtime(path, body=None, **_kwargs):
            nonlocal claimed
            requests.append(path)
            if path == "/internal/jobs/control/claim":
                if held.is_set() and not claimed and body["stage"] == "advance":
                    claimed = True
                    return {"jobs": [job]}
                return {"jobs": []}
            if path in {"/internal/jobs/control/start", "/internal/jobs/heartbeat"}:
                return {"ok": True}
            if path == "/agent-runs/control":
                return {"schema": "runtime.agent_run.control.result.v1", "agentRunId": "agent_run_1",
                        "disposition": "released", "terminalState": "cancelled",
                        "cleanupReceiptId": "cleanup-no-effects", "releaseReceiptId": "release-no-effects"}
            if path == "/internal/jobs/complete":
                with lock:
                    completed_with.append(active)
                stop(None, None)
                return {"ok": True}
            self.fail("unexpected control request: " + path)

        def api(path, body, reason):
            if path == "/internal/agent-run-lifecycle/control":
                return {"schema": "runtime.agent_run_lifecycle.control.resolved.v1",
                        "agentRunControl": {"agentRunId": "agent_run_1"}}
            if path == "/internal/agent-runs/transition":
                return {"agentRunId": "agent_run_1", "state": "cancelled"}
            self.fail("unexpected API request: " + path)

        @contextmanager
        def heartbeats(*_args):
            yield lambda: None

        with patch.object(worker, "WORKER_SLOT_COUNT", 8), \
                patch.object(worker.signal, "signal", side_effect=signal), \
                patch.object(worker, "run_loop"), \
                patch.object(worker, "execute_next_job", side_effect=ordinary), \
                patch.object(worker, "runtime_request", side_effect=runtime), \
                patch.object(worker, "api_request", side_effect=api), \
                patch.object(worker, "lease_heartbeats", heartbeats), \
                patch.object(worker, "agent_run_step_request") as execute:
            timer = threading.Timer(2, lambda: stop(None, None))
            timer.start()
            try:
                worker.run_worker_service()
            finally:
                timer.cancel()
                release.set()
                for thread in threading.enumerate():
                    if thread.name.startswith("workspace-job-slot-"):
                        thread.join(timeout=2)
            self.assertEqual(completed_with, [8])
            execute.assert_not_called()
            self.assertNotIn("/internal/jobs/start", requests)


if __name__ == "__main__":
    unittest.main()
