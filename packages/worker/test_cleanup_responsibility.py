"""Unconfirmed terminal cleanup retains its canonical Job and retry budget."""
from contextlib import contextmanager
import unittest
from unittest.mock import patch

from test_worker import lifecycle_fixture
import test_retained_lifecycle
import worker


class CleanupResponsibilityTests(unittest.TestCase):
    def test_unknown_cleanup_yields_inside_current_lease_even_with_exhausted_execution_budget(self):
        for retry_count in (10, 11):
            for mode in ("perRun", "retained"):
                with self.subTest(retry_count=retry_count, mode=mode):
                    self.exercise(retry_count, mode)

    def test_cleanup_handoff_cannot_yield_after_lease_loss(self):
        self.exercise(11, "retained", lose_lease=True)

    def test_unknown_cleanup_handoff_does_not_attempt_failure_or_second_yield(self):
        self.exercise(11, "retained", unknown_handoff=True)

    def test_release_or_completion_acknowledgement_loss_yields_without_an_execution_failure(self):
        for stage in ("release", "complete"):
            with self.subTest(stage=stage):
                job, _ = lifecycle_fixture()
                job.update(retryCount=11, maxRetries=10)
                terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
                teardown = {"schema": "runtime.agent_run.teardown.result.v1", "agentRunId": "agent_run_1",
                    "status": "cleanupConfirmed", "cleanupReceiptId": "cleanup-one"}
                release = {"schema": "runtime.agent_run_lifecycle.released.v1", "agentRunId": "agent_run_1",
                    "releaseReceiptId": "release-one"}
                @contextmanager
                def heartbeats(*_args):
                    yield lambda: None
                with patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", heartbeats), \
                        patch.object(worker, "resolve_agent_run_lifecycle", return_value=terminal), \
                        patch.object(worker, "runtime_teardown_request", return_value=teardown), \
                        patch.object(worker, "api_request", return_value=release,
                            side_effect=OSError("lost release acknowledgement") if stage == "release" else None), \
                        patch.object(worker, "complete_job",
                            side_effect=OSError("lost completion acknowledgement") if stage == "complete" else None) as complete, \
                        patch.object(worker, "yield_job") as yielded, patch.object(worker, "fail_job") as failed, \
                        patch.object(worker, "agent_run_step_request") as step:
                    worker.execute_claimed_job(job, "worker:fixture-owner")
                failed.assert_not_called()
                step.assert_not_called()
                self.assertEqual(yielded.call_args.args[-1], "retained_cleanup_pending")
                self.assertEqual(complete.call_count, 1 if stage == "complete" else 0)

    def exercise(self, retry_count, mode, *, lose_lease=False, unknown_handoff=False):
        job, _ = lifecycle_fixture()
        job.update(retryCount=retry_count, maxRetries=10)
        terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
        terminal["terminalState"] = "failed"
        owner = "worker:fixture-owner"
        active = False
        cleanup_attempted = False

        def healthy():
            self.assertTrue(active, "handoff must retain heartbeat ownership")
            if cleanup_attempted and lose_lease:
                raise worker.RuntimeLeaseLost()

        @contextmanager
        def heartbeats(*_args):
            nonlocal active
            active = True
            try:
                yield healthy
            finally:
                active = False

        def teardown(*_args):
            nonlocal cleanup_attempted
            cleanup_attempted = True
            raise OSError("lost teardown acknowledgement")

        def handoff(*_args):
            healthy()
            if unknown_handoff:
                raise worker.YieldHandoffUnknown(OSError("lost yield acknowledgement"))

        with patch.object(worker, "AGENT_RUN_EXECUTION_MODE", mode), \
                patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", heartbeats), \
                patch.object(worker, "resolve_agent_run_lifecycle", return_value=terminal), \
                patch.object(worker, "runtime_teardown_request", side_effect=teardown), \
                patch.object(worker, "now_ms", return_value=1000), \
                patch.object(worker, "yield_job", side_effect=handoff) as yielded, \
                patch.object(worker, "fail_job") as failed, patch.object(worker, "complete_job") as complete, \
                patch.object(worker, "agent_run_step_request") as step, \
                patch.object(worker, "api_request") as api, patch.object(worker, "transition_agent_run") as transition:
            worker.execute_claimed_job(job, owner)
        failed.assert_not_called()
        complete.assert_not_called()
        step.assert_not_called()
        api.assert_not_called()
        transition.assert_not_called()
        self.assertEqual(job["retryCount"], retry_count)
        if lose_lease:
            yielded.assert_not_called()
        else:
            yielded.assert_called_once_with(job["jobId"], owner, 2000, "retained_cleanup_pending")


if __name__ == "__main__":
    unittest.main()
