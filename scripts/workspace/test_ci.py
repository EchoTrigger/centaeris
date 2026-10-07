import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

original_search_path = list(sys.path)
spec = importlib.util.spec_from_file_location("ci", Path(__file__).with_name("ci.py"))
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


class PortableCITests(unittest.TestCase):
    def test_loading_the_gate_preserves_the_hosted_test_discovery_path(self):
        self.assertEqual(sys.path, original_search_path)

    def test_gate_preserves_backend_checks_and_frontend_skip_scope(self):
        for skip in (False, True):
            calls = []
            config = {"services": {"document-processor": {}, "workspace-general": {},
                      "runtime": {"depends_on": {"workspace-general": {"condition": "service_completed_successfully"}}},
                      "material-worker": {"depends_on": {"document-processor": {"condition": "service_completed_successfully"}}}}}
            def record(label, args, **kwargs):
                calls.append(args)
                return json.dumps(config)
            with patch.object(ci, "run", record):
                ci.main(skip)
            self.assertTrue(any("build" in c and "packages/web" in c for c in calls))
            self.assertEqual(any("test:unit" in c for c in calls), not skip)
            for script in ("scripts/workspace/runtime_outbox_gate.py", "scripts/workspace/agent-run-authorization-gate.py",
                           "scripts/workspace/transcript-schema.py", "scripts/workspace/test_transcript_schema.py",
                           "scripts/workspace/platform-mcp-client-gate.py"):
                self.assertTrue(any(script in c for c in calls), script)
            self.assertTrue(any("scripts/workspace/python_test_gate.py" in c and "api" == c[-1] for c in calls))

    def test_failure_stops_the_gate(self):
        with patch.object(ci, "run", side_effect=RuntimeError("failed")) as run:
            with self.assertRaises(RuntimeError):
                ci.main()
            self.assertEqual(run.call_count, 1)
        with self.assertRaises(subprocess.CalledProcessError):
            ci.run("synthetic failed command", [sys.executable, "-c", "raise SystemExit(7)"])


if __name__ == "__main__":
    unittest.main()
