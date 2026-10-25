import threading
import unittest
from unittest.mock import Mock, patch

import test_worker  # Establish the same isolated worker environment.
from test_worker import worker


def busy(reason="execution_claim_busy", route="/internal/jobs/claim", status=503):
    error = worker.DependencyUnavailable(reason, http_status=status)
    error.route = route
    return error


class RetryTests(unittest.TestCase):
    def run_sequence(self, sequence, wait_error=None):
        stopped = Mock()
        stopped.is_set.side_effect = [False] * len(sequence) + [True]
        with patch.object(worker, "execute_next_job", side_effect=sequence), \
                patch.object(worker, "wait_for_jobs", side_effect=wait_error), \
                patch("sys.stderr"):
            worker.run_job_loop(0, stopped)
        return [call.args[0] for call in stopped.wait.call_args_list]

    def test_claim_contention_retries_promptly_then_bounds_sustained_pressure(self):
        self.assertEqual(self.run_sequence([busy()] * 7), [.1, .2, .4, .8, 1, 1, 1])

    def test_successful_claim_and_empty_scan_reset_contention_backoff(self):
        for recovered in (True, False):
            with self.subTest(recovered=recovered):
                self.assertEqual(self.run_sequence([busy(), busy(), recovered, busy()]), [.1, .2, .1])

    def test_runtime_admission_busy_uses_the_same_bounded_claim_retry(self):
        self.assertEqual(self.run_sequence([busy("runtime_busy")]), [.1])

    def test_wait_endpoint_busy_keeps_its_existing_fallback(self):
        self.assertEqual(self.run_sequence([False] * 2,
            busy("runtime_busy", "/internal/jobs/wait")), [5, 5])

    def test_backoff_records_error_origin(self):
        with patch.object(worker.observations, "measure", wraps=worker.observations.measure) as measure:
            self.run_sequence([busy()])
        fields = [call.kwargs for call in measure.call_args_list if call.args == ("backoff",)]
        self.assertEqual(fields[0]["route"], "/internal/jobs/claim")
        self.assertEqual(fields[0]["requestedMs"], 100)

    def test_unrelated_errors_do_not_get_fast_claim_retry(self):
        self.assertEqual(self.run_sequence([
            busy(route="/agent-runs/step"), busy(status=502), busy(route=None),
            RuntimeError("execution_claim_busy"),
            worker.DependencyUnavailable("disconnected"),
            busy("runtime_busy", "/agent-runs/step"),
        ]), [5, 5, 5, 5, 1, 5])

    def test_stop_interrupts_retry_without_another_claim(self):
        stopped = threading.Event()
        def fail(_slot):
            stopped.set()
            raise busy()
        with patch.object(worker, "execute_next_job", side_effect=fail) as claim, patch("sys.stderr"):
            worker.run_job_loop(0, stopped)
        claim.assert_called_once()

    def test_stop_wakes_a_claim_retry_already_waiting(self):
        waiting = threading.Event()

        class ObservedStop(threading.Event):
            def wait(self, timeout=None):
                waiting.set()
                return super().wait(timeout)

        stopped = ObservedStop()
        with patch.object(worker, "JOB_CLAIM_BUSY_INITIAL_BACKOFF_SECONDS", 10), \
                patch.object(worker, "execute_next_job", side_effect=busy()) as claim, \
                patch("sys.stderr"):
            thread = threading.Thread(target=worker.run_job_loop, args=(0, stopped), daemon=True)
            thread.start()
            try:
                self.assertTrue(waiting.wait(1), "claim retry did not enter its wait")
                stopped.set()
                thread.join(1)
                self.assertFalse(thread.is_alive(), "shutdown must interrupt the pending retry wait")
                claim.assert_called_once()
            finally:
                stopped.set()
                thread.join(1)

    def test_rpc_attaches_route_to_dependency_failure(self):
        error = worker.DependencyUnavailable("execution_claim_busy", http_status=503)
        with patch.object(worker, "_json_request", side_effect=error):
            with self.assertRaises(worker.DependencyUnavailable) as caught:
                worker.runtime_request("/internal/jobs/claim", {})
        self.assertEqual(caught.exception.route, "/internal/jobs/claim")


if __name__ == "__main__":
    unittest.main()
