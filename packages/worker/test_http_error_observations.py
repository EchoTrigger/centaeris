"""HTTP observations retain only exact public error codes, without changing failures."""
import io
import json
import unittest
import urllib.error
from unittest.mock import patch

import test_worker  # Establish the same isolated Worker configuration.
import worker


class HttpErrorObservationTests(unittest.TestCase):
    def observe(self, status, payload):
        output = io.StringIO()
        rejection = urllib.error.HTTPError(
            "http://runtime.invalid/agent-runs/teardown", status, "private status text",
            None, io.BytesIO(json.dumps(payload).encode()),
        )
        with patch.object(worker.observations, "ENABLED", True), patch("sys.stderr", output), \
                patch.object(worker.urllib.request, "urlopen", side_effect=rejection):
            with self.assertRaises(RuntimeError) as raised:
                worker.runtime_teardown_request({"privateInput": "secret-input"})
        event = json.loads(output.getvalue())
        self.assertNotIn("secret-input", output.getvalue())
        self.assertNotIn("private status text", output.getvalue())
        self.assertNotIn("test-internal-token", output.getvalue())
        return raised.exception, event

    def test_exact_public_cleanup_codes_are_visible_without_changing_503_failure_type(self):
        for code in ("retained_cleanup_unconfirmed", "retained_cleanup_unavailable",
                     "agent_run_supplement_cleanup_failed", "terminal_subagent_cleanup_unavailable"):
            with self.subTest(code=code):
                error, event = self.observe(503, {"error": code})
                self.assertIsInstance(error, worker.DependencyUnavailable)
                self.assertEqual(str(error), code)
                self.assertEqual(error.http_status, 503)
                self.assertEqual(event["httpStatus"], 503)
                self.assertEqual(event["errorCode"], code)
                self.assertEqual(event["route"], "/agent-runs/teardown")

    def test_exact_conflict_codes_keep_their_status_and_original_exception_type(self):
        for code in ("environment_host_busy", "execution_resident_capacity_wait"):
            with self.subTest(code=code):
                error, event = self.observe(409, {"error": code})
                self.assertNotIsInstance(error, worker.DependencyUnavailable)
                self.assertEqual(error.http_status, 409)
                self.assertEqual(event["errorCode"], code)

    def test_unknown_or_extra_fields_cannot_enter_the_observation(self):
        for payload in (
            {"error": "secret-response"},
            {"error": "retained_cleanup_unconfirmed:secret-response"},
            {"error": "retained_cleanup_unconfirmed", "details": "secret-response"},
            {"error": "execution_claim_busy", "details": "secret-response"},
            {"error": ["secret-response"]},
        ):
            with self.subTest(payload=payload):
                error, event = self.observe(503, payload)
                self.assertIsInstance(error, worker.DependencyUnavailable)
                self.assertEqual(event["errorCode"], "other")
                self.assertNotIn("secret-response", json.dumps(event))

    def test_transport_failure_does_not_become_a_response_error_code(self):
        output = io.StringIO()
        with patch.object(worker.observations, "ENABLED", True), patch("sys.stderr", output), \
                patch.object(worker.urllib.request, "urlopen",
                             side_effect=urllib.error.URLError("secret-transport")):
            with self.assertRaises(worker.DependencyUnavailable) as raised:
                worker.runtime_teardown_request({"privateInput": "secret-input"})
        self.assertIsNone(raised.exception.http_status)
        self.assertEqual(json.loads(output.getvalue())["errorCode"], "other")
        self.assertNotIn("secret", output.getvalue())

    def test_local_known_rejection_still_has_its_existing_safe_category(self):
        output = io.StringIO()
        with patch.object(worker.observations, "ENABLED", True), patch("sys.stderr", output):
            with self.assertRaises(RuntimeError), worker.observations.measure("claimJob"):
                raise RuntimeError("runtime_busy")
        self.assertEqual(json.loads(output.getvalue())["errorCode"], "runtime_busy")


if __name__ == "__main__":
    unittest.main()