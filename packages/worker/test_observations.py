import io
import json
import unittest
from unittest.mock import patch

import observations


class ObservationTests(unittest.TestCase):
    def test_error_is_preserved_and_private_message_is_not_logged(self):
        output = io.StringIO()
        error = RuntimeError("private body and token")
        with patch.object(observations, "ENABLED", True), patch("sys.stderr", output):
            with self.assertRaises(RuntimeError) as raised:
                with observations.context(slotIndex=3, jobId="job_1"):
                    with observations.measure("rpc", route="/agent-runs/step"):
                        raise error
        self.assertIs(raised.exception, error)
        event = json.loads(output.getvalue())
        self.assertEqual(event["jobId"], "job_1")
        self.assertEqual(event["errorCode"], "other")
        self.assertNotIn("private", output.getvalue())
        self.assertGreaterEqual(event["elapsedMs"], 0)

    def test_failure_diagnostic_is_always_on_and_drops_private_context_and_routes(self):
        error = RuntimeError("private exception with test-internal-token")
        error.http_status, error.http_error_code = 503, "retained_cleanup_unconfirmed"
        error.route = "/agent-runs/teardown"
        with patch.object(observations, "ENABLED", False), patch("sys.stderr", new_callable=io.StringIO) as output:
            with observations.context(leaseOwner="private-lease", url="https://private.invalid/token"):
                observations.diagnostic("jobWaiting", error, job_id="job_1", control_stage="cleanup",
                    lane="control", transition_reason="control_retry_pending")
            event = json.loads(output.getvalue())
        self.assertEqual(event["event"], "jobWaiting")
        self.assertEqual(event["jobId"], "job_1")
        self.assertEqual(event["httpStatus"], 503)
        self.assertEqual(event["errorCode"], "retained_cleanup_unconfirmed")
        self.assertEqual(event["route"], "/agent-runs/teardown")
        self.assertNotIn("private", output.getvalue())
        self.assertNotIn("test-internal-token", output.getvalue())
        error.route, error.http_error_code = "/internal/jobs/private-lease?token=private", "private-response"
        with patch("sys.stderr", new_callable=io.StringIO) as output:
            observations.diagnostic("slotFailed", error, slot_index=2, lane="execution")
        event = json.loads(output.getvalue())
        self.assertIsNone(event["route"])
        self.assertEqual(event["errorCode"], "other")
        self.assertNotIn("private", output.getvalue())

    def test_slot_failure_logs_safe_http_route_without_performance_opt_in(self):
        import test_worker
        import worker
        error = worker.DependencyUnavailable("private transport body", http_status=503)
        error.http_error_code, error.route = "execution_claim_busy", "/internal/jobs/claim"
        from unittest.mock import Mock
        stopped = Mock()
        stopped.is_set.side_effect = [False, True]
        with patch.object(observations, "ENABLED", False), patch("sys.stderr", new_callable=io.StringIO) as output, \
                patch.object(worker, "execute_next_job", side_effect=error):
            worker._run_job_loop(2, stopped)
        event = json.loads(output.getvalue())
        self.assertEqual((event["event"], event["slotIndex"]), ("slotFailed", 2))
        self.assertEqual((event["route"], event["httpStatus"], event["errorCode"]),
            ("/internal/jobs/claim", 503, "execution_claim_busy"))
        self.assertNotIn("private", output.getvalue())


    def test_disabled_is_silent_and_context_does_not_leak(self):
        with patch.object(observations, "ENABLED", False), patch("sys.stderr", new_callable=io.StringIO) as output:
            with observations.context(jobId="job_1"), observations.measure("slotHeld"):
                pass
            self.assertEqual(output.getvalue(), "")
        self.assertEqual(getattr(observations._context, "fields", {}), {})
