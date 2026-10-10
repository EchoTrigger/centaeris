import importlib.util
import json
import os
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
    def test_compose_structure_chooses_isolated_capacity_inputs_without_changing_host_environment(self):
        host_environment = {key: value for key, value in os.environ.items() if not key.startswith("UPLOAD_") and key != "WORK_RETURN_AUDIT_INTERVAL_SECONDS"}
        with patch.dict(os.environ, host_environment, clear=True), \
                patch.object(ci.subprocess, "check_output", return_value="f" * 40), \
                patch.object(ci.subprocess, "run") as command:
            ci.run("Compose structure", ["docker", "compose", "--env-file", ".env.example", "config"])
            environment = command.call_args.kwargs["env"]
            self.assertEqual(environment["UPLOAD_BODY_MAX_BYTES"], "1048576")
            self.assertEqual(environment["UPLOAD_TEMP_MAX_BYTES"], "4194304")
            self.assertEqual(environment["UPLOAD_MAX_CONCURRENT"], "2")
            self.assertEqual(environment["WORK_RETURN_AUDIT_INTERVAL_SECONDS"], "3600")
            self.assertNotIn("UPLOAD_BODY_MAX_BYTES", os.environ)

    def test_web_prepares_rust_and_core_before_parallel_tests(self):
        for stage in ("Web", "All"):
            with self.subTest(stage=stage):
                calls = []
                def record(label, args, **kwargs):
                    calls.append(args)
                    return json.dumps({"services": {"document-processor": {}, "workspace-general": {},
                        "runtime": {"depends_on": {"workspace-general": {"condition": "service_completed_successfully"}}},
                        "material-worker": {"depends_on": {"document-processor": {"condition": "service_completed_successfully"}}}}})
                with patch.object(ci, "run", record):
                    ci.main(stage=stage)
                toolchain = calls.index(["rustup", "show", "active-toolchain"])
                source = calls.index(["node", "scripts/workspace/core-source.mjs"])
                tests = next(i for i, command in enumerate(calls) if "test:unit" in command)
                self.assertLess(toolchain, source)
                self.assertLess(source, tests)
        with patch.object(ci, "run", side_effect=RuntimeError("preparation failed")) as run:
            with self.assertRaises(RuntimeError):
                ci.main(stage="Web")
        self.assertFalse(any("test:unit" in call.args[1] for call in run.call_args_list))

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
            self.assertTrue(any("build" in c and "web" in c for c in calls))
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
