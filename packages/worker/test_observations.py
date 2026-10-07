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

    def test_disabled_is_silent_and_context_does_not_leak(self):
        with patch.object(observations, "ENABLED", False), patch("sys.stderr", new_callable=io.StringIO) as output:
            with observations.context(jobId="job_1"), observations.measure("slotHeld"):
                pass
            self.assertEqual(output.getvalue(), "")
        self.assertEqual(getattr(observations._context, "fields", {}), {})
