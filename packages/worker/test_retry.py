import io
import random
import threading
import unittest
import urllib.error
from unittest.mock import Mock, patch

import test_worker  # Establish the same isolated worker environment.
from test_worker import worker


def busy(reason="execution_claim_busy", route="/internal/jobs/claim", status=503):
    error = worker.DependencyUnavailable(reason, http_status=status)
    error.route = route
    error.http_error_code = reason
    return error


class RetryTests(unittest.TestCase):
    def run_sequence(self, sequence, wait_error=None, jitter=None):
        stopped = Mock()
        stopped.is_set.side_effect = [False] * len(sequence) + [True]
        jitter = jitter or (lambda lower, upper: upper)
        with patch.object(worker, "execute_next_job", side_effect=sequence), \
                patch.object(worker, "wait_for_jobs", side_effect=wait_error) as wait, \
                patch.object(worker, "fail_claimed_job") as fail, \
                patch.object(random, "uniform", side_effect=jitter), patch("sys.stderr"):
            worker.run_job_loop(0, stopped)
        self.assertEqual(wait.call_count, sum(item is False for item in sequence))
        fail.assert_not_called()
        return [call.args[0] for call in stopped.wait.call_args_list]

    def test_claim_contention_retries_in_tens_of_milliseconds_and_resets_on_success_or_idle(self):
        delays = self.run_sequence([busy(), busy(), True, busy(), False, busy()])
        for delay in delays:
            self.assertGreaterEqual(delay, .025)
            self.assertLessEqual(delay, .8)
        self.assertGreater(delays[1], delays[0])
        self.assertEqual(delays[0], delays[2])
        self.assertEqual(delays[0], delays[3])

    def test_sustained_claim_contention_is_jittered_bounded_and_never_hotloops(self):
        calls = []
        def jitter(lower, upper):
            calls.append((lower, upper))
            return lower if len(calls) % 2 else upper
        delays = self.run_sequence([busy() for _ in range(40)], jitter=jitter)
        self.assertEqual(len(calls), 40)
        self.assertTrue(all(.025 <= delay <= .8 for delay in delays))
        self.assertTrue(all(lower < upper for lower, upper in calls))
        self.assertNotEqual(delays[-1], delays[-2])

    def test_recovery_executes_the_next_claim_without_an_extra_wait_or_job_wait(self):
        trace = [];stopped = Mock();stopped.is_set.side_effect = [False, False, True]
        def execute(_slot):
            trace.append("claim")
            if trace.count("claim") == 1:
                raise busy()
            trace.append("execute")
            return True
        stopped.wait.side_effect = lambda delay: trace.append(("backoff", delay))
        with patch.object(worker, "execute_next_job", side_effect=execute), \
                patch.object(worker, "wait_for_jobs") as wait, \
                patch.object(random, "uniform", side_effect=lambda low, high: high), patch("sys.stderr"):
            worker.run_job_loop(0, stopped)
        wait.assert_not_called()
        self.assertEqual(trace[0], "claim")
        self.assertEqual(trace[2:], ["claim", "execute"])
        self.assertLessEqual(trace[1][1], .05)

    def test_backoff_records_error_origin(self):
        with patch.object(worker.observations, "measure", wraps=worker.observations.measure) as measure:
            self.run_sequence([busy()])
        fields = [call.kwargs for call in measure.call_args_list if call.args == ("backoff",)]
        self.assertEqual(fields[0]["route"], "/internal/jobs/claim")
        self.assertGreaterEqual(fields[0]["requestedMs"], 25)
        self.assertLessEqual(fields[0]["requestedMs"], 50)

    def test_unrelated_errors_keep_the_original_fault_policy(self):
        missing_code = busy();missing_code.http_error_code = None
        missing_status = busy(status=None)
        wrong_code = busy();wrong_code.http_error_code = "other"
        missing_route = busy();del missing_route.route
        errors = [busy(route="/agent-runs/step"), busy(status=502), busy(status=409),
            busy(status=503.0), missing_status, missing_code, wrong_code, missing_route,
            RuntimeError("execution_claim_busy"), worker.DependencyUnavailable("disconnected"),
            busy("runtime_busy", "/internal/jobs/claim"), busy("runtime_busy", "/internal/jobs/wait")]
        self.assertEqual(self.run_sequence(errors), [5, 5, 5, 5, 5, 5, 5, 5, 5, 1, 5, 5])

    def test_extra_response_fields_do_not_enable_claim_contention_retry(self):
        error = urllib.error.HTTPError("http://runtime.invalid/internal/jobs/claim", 503, "busy", {},
            io.BytesIO(b'{"error":"execution_claim_busy","details":"synthetic-extra-field"}'))
        with patch.object(worker.urllib.request, "urlopen", side_effect=error):
            with self.assertRaises(worker.DependencyUnavailable) as raised:
                worker.runtime_request("/internal/jobs/claim", {})
        self.assertIsNone(raised.exception.http_error_code)
        self.assertEqual(self.run_sequence([raised.exception]), [5])

    def test_transport_failure_keeps_the_original_fault_policy(self):
        with patch.object(worker.urllib.request, "urlopen", side_effect=urllib.error.URLError("synthetic-disconnect")):
            with self.assertRaises(worker.DependencyUnavailable) as raised:
                worker.runtime_request("/internal/jobs/claim", {})
        self.assertIsNone(raised.exception.http_status)
        self.assertEqual(self.run_sequence([raised.exception]), [1])

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
