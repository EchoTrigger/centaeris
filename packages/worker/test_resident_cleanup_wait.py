"""Exact teardown admission waits preserve the original lifecycle responsibility."""
import unittest
from unittest.mock import patch

import test_environment_host_busy as host_busy
from test_worker import lifecycle_fixture
import test_retained_lifecycle
import worker


class ResidentCleanupWaitTests(unittest.TestCase):
    def test_execute_cleanup_repeatedly_yields_same_job_without_retry_in_both_modes(self):
        for mode in ("perRun", "retained"):
            with self.subTest(mode=mode):
                host_busy.EnvironmentHostBusyTests().observe_cleanup_busy(
                    mode, code="execution_resident_capacity_wait",
                    waiting_reason="execution_resident_capacity_wait",
                )

    def test_control_cleanup_repeatedly_yields_same_job_without_retry_in_both_modes(self):
        for mode in ("perRun", "retained"):
            with self.subTest(mode=mode):
                host_busy.EnvironmentHostBusyTests().observe_cleanup_busy(
                    mode, control=True, code="execution_resident_capacity_wait",
                    waiting_reason="execution_resident_capacity_wait",
                )

    def test_nearby_http_and_transport_errors_keep_the_failure_path(self):
        for status, payload in (
            (503, {"error": "execution_resident_capacity_wait"}),
            (409, {"error": "execution_resident_capacity_wait", "extra": True}),
            (409, {"error": "execution_resident_capacity_wait:unknown"}),
            (409, {"error": "other"}), (409, {}), (None, None),
        ):
            with self.subTest(status=status, payload=payload):
                job, _ = lifecycle_fixture()
                terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
                error = OSError("transport unavailable") if status is None else host_busy.rejection(status, payload)
                with patch.object(worker, "resolve_agent_run_lifecycle", return_value=terminal), \
                        patch("urllib.request.urlopen", side_effect=error), \
                        patch.object(worker, "yield_job") as yielded:
                    with self.assertRaises(worker.RetainedCleanupPending):
                        worker.execute_agent_run_lifecycle_job(job, "worker:fixture-owner", lambda: None)
                yielded.assert_not_called()

    def test_503_exact_code_still_consumes_the_existing_failure_retry_path(self):
        job, _ = lifecycle_fixture()
        job.update(retryCount=0, maxRetries=10)
        terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
        with patch.object(worker, "start_job"), \
                patch.object(worker, "lease_heartbeats", host_busy.heartbeats), \
                patch.object(worker, "resolve_agent_run_lifecycle", return_value=terminal), \
                patch("urllib.request.urlopen", side_effect=host_busy.rejection(
                    503, {"error": "execution_resident_capacity_wait"})), \
                patch.object(worker, "fail_job") as failed, patch.object(worker, "yield_job") as yielded:
            worker.execute_claimed_job(job, "worker:fixture-owner")
        failed.assert_called_once_with(job, "worker:fixture-owner", "retained_cleanup_pending", True)
        yielded.assert_not_called()

    def test_exact_cleanup_wait_cannot_handoff_after_lease_loss(self):
        job, _ = lifecycle_fixture()
        terminal = test_retained_lifecycle.RetainedLifecycleTests().terminal()
        checks = 0
        def healthy():
            nonlocal checks
            checks += 1
            if checks >= 3:
                raise worker.RuntimeLeaseLost()
        with patch.object(worker, "resolve_agent_run_lifecycle", return_value=terminal), \
                patch("urllib.request.urlopen", side_effect=host_busy.rejection(
                    payload={"error": "execution_resident_capacity_wait"})), \
                patch.object(worker, "yield_job") as yielded:
            with self.assertRaises(worker.RuntimeLeaseLost):
                worker.execute_agent_run_lifecycle_job(job, "worker:fixture-owner", healthy)
        yielded.assert_not_called()


if __name__ == "__main__":
    unittest.main()