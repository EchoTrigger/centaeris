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

    def test_contention_keeps_fixed_delay(self):
        self.assertEqual(self.run_sequence([busy(), busy(), True, busy()]), [5, 5, 5])
        self.assertEqual(self.run_sequence([False] * 2,
            busy("runtime_busy", "/internal/jobs/wait")), [5, 5])

    def test_backoff_records_error_origin(self):
        with patch.object(worker.observations, "measure", wraps=worker.observations.measure) as measure:
            self.run_sequence([busy()])
        fields = [call.kwargs for call in measure.call_args_list if call.args == ("backoff",)]
        self.assertEqual(fields[0]["route"], "/internal/jobs/claim")
        self.assertEqual(fields[0]["requestedMs"], 5000)

    def test_unrelated_errors_do_not_get_fast_claim_retry(self):
        self.assertEqual(self.run_sequence([
            busy(route="/agent-runs/step"), busy(status=502),
            RuntimeError("execution_claim_busy"),
            worker.DependencyUnavailable("disconnected"),
            busy("runtime_busy", "/agent-runs/step"),
        ]), [5, 5, 5, 1, 5])

    def test_stop_interrupts_retry_without_another_claim(self):
        stopped = threading.Event()
        def fail(_slot):
            stopped.set()
            raise busy()
        with patch.object(worker, "execute_next_job", side_effect=fail) as claim, patch("sys.stderr"):
            worker.run_job_loop(0, stopped)
        claim.assert_called_once()

    def test_rpc_attaches_route_to_dependency_failure(self):
        error = worker.DependencyUnavailable("execution_claim_busy", http_status=503)
        with patch.object(worker, "_json_request", side_effect=error):
            with self.assertRaises(worker.DependencyUnavailable) as caught:
                worker.runtime_request("/internal/jobs/claim", {})
        self.assertEqual(caught.exception.route, "/internal/jobs/claim")


if __name__ == "__main__":
    unittest.main()
