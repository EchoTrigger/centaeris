from contextlib import redirect_stdout
import importlib.util
import io
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
    def test_sdk_transport_rust_fixtures_get_the_direct_gate_interpreter(self):
        with patch.dict(os.environ, {key: value for key, value in os.environ.items()
                                   if key != "E2B_ADAPTER_TEST_PYTHON"}, clear=True), \
                patch.object(ci.subprocess, "run") as command:
            ci.run("Rust tests runtime_server", ["cargo", "test", "--locked", "-p", "runtime_server"])
            self.assertEqual(command.call_args.kwargs["env"]["E2B_ADAPTER_TEST_PYTHON"], sys.executable)
            self.assertNotIn("E2B_ADAPTER_TEST_PYTHON", os.environ)

    def test_cargo_tests_keep_proxy_exceptions_and_bypass_loopback_in_both_variable_cases(self):
        cases = [
            ({}, ["127.0.0.1", "localhost", "::1"]),
            ({"NO_PROXY": ".company.example"}, [".company.example", "127.0.0.1", "localhost", "::1"]),
            ({"no_proxy": "registry.internal"}, ["registry.internal", "127.0.0.1", "localhost", "::1"]),
            ({"NO_PROXY": ".company.example,127.0.0.1", "no_proxy": "registry.internal,localhost"},
             [".company.example", "registry.internal", "127.0.0.1", "localhost", "::1"]),
            ({"NO_PROXY": "localhost,127.0.0.1,::1", "no_proxy": "localhost"},
             ["localhost", "127.0.0.1", "::1"]),
            ({"NO_PROXY": " LOCALHOST , .EXAMPLE.COM ,127.0.0.1,,", "no_proxy": ".example.com,[::1]"},
             ["LOCALHOST", ".EXAMPLE.COM", "127.0.0.1", ".example.com", "[::1]", "localhost", "::1"]),
        ]
        proxies = {"HTTP_PROXY": "http://proxy.invalid:3128", "HTTPS_PROXY": "http://tls-proxy.invalid:3128",
                   "http_proxy": "http://lower-proxy.invalid:3128", "ALL_PROXY": "socks5://all-proxy.invalid:1080"}
        for configured, expected in cases:
            with self.subTest(configured=configured):
                environment = {key: value for key, value in os.environ.items() if key.lower() != "no_proxy"}
                environment["SystemRoot"] = os.environ.get("SystemRoot", "C:/Windows")
                environment.update(proxies)
                environment.update(configured)
                before = environment.copy()
                with patch.object(ci.os, "environ", environment), \
                        patch.object(ci.subprocess, "run") as command, redirect_stdout(io.StringIO()):
                    ci.run("Rust tests", ["cargo", "test", "--locked", "-p", "runtime_server"])
                    self.assertTrue(environment == before, "Cargo launch changed the host environment")
                child = command.call_args.kwargs["env"]
                for name in ("NO_PROXY", "no_proxy"):
                    self.assertCountEqual(child.get(name, "").split(","), expected)
                self.assertEqual(child["NO_PROXY"], child["no_proxy"])
                for name, value in proxies.items():
                    self.assertEqual(child[name], value)

    def test_loopback_proxy_overrides_do_not_change_other_gate_commands(self):
        environment = {**os.environ, "NO_PROXY": ".company.example", "no_proxy": "registry.internal",
                       "HTTP_PROXY": "http://proxy.invalid:3128"}
        environment["SystemRoot"] = os.environ.get("SystemRoot", "C:/Windows")
        before = environment.copy()
        for args in (["cargo", "check", "--locked", "-p", "runtime_server"],
                     ["node", "--test", "scripts/workspace/core-source.test.mjs"],
                     ["uv", "run", "--frozen", "--package", "centaeris-e2b-adapter", "python", "-m", "unittest"]):
            with self.subTest(args=args), patch.object(ci.os, "environ", environment), \
                    patch.object(ci.subprocess, "run") as command, redirect_stdout(io.StringIO()):
                ci.run("Unrelated gate", args)
                self.assertTrue(environment == before, "Gate launch changed the host environment")
                child = command.call_args.kwargs["env"]
                self.assertEqual(child["NO_PROXY"], ".company.example")
                self.assertEqual(child["no_proxy"], "registry.internal")
                self.assertEqual(child["HTTP_PROXY"], "http://proxy.invalid:3128")

    def test_e2b_adapter_behavior_runs_in_python_stage_with_its_pinned_package(self):
        expected = ["uv", "run", "--frozen", "--package", "centaeris-e2b-adapter", "python", "-B",
                    "-m", "unittest", "discover", "-s", "packages/e2b_adapter/tests", "-v"]
        config = {"services": {"document-processor": {}, "workspace-general": {},
                  "runtime": {"depends_on": {"workspace-general": {"condition": "service_completed_successfully"}}},
                  "material-worker": {"depends_on": {"document-processor": {"condition": "service_completed_successfully"}}}}}
        for stage in ("Python", "All", "Rust", "Web"):
            with self.subTest(stage=stage):
                calls = []
                def record(label, args, **kwargs):
                    calls.append(args)
                    return json.dumps(config)
                with patch.object(ci, "run", record):
                    ci.main(stage=stage)
                self.assertEqual(calls.count(expected), 1 if stage in ("Python", "All") else 0)

    def test_adapter_gate_disables_sdk_retries_only_for_its_child_process(self):
        config = {"services": {"document-processor": {}, "workspace-general": {},
                  "runtime": {"depends_on": {"workspace-general": {"condition": "service_completed_successfully"}}},
                  "material-worker": {"depends_on": {"document-processor": {"condition": "service_completed_successfully"}}}}}
        for inherited in (None, "7", "0"):
            with self.subTest(inherited=inherited):
                environment = {key: value for key, value in os.environ.items()
                               if key != "E2B_CONNECTION_RETRIES"}
                if inherited is not None:
                    environment["E2B_CONNECTION_RETRIES"] = inherited
                children = []
                def record(args, **kwargs):
                    children.append((args, kwargs["env"]))
                    return subprocess.CompletedProcess(args, 0, stdout=json.dumps(config))
                with patch.dict(os.environ, environment, clear=True), \
                        patch.object(ci.subprocess, "run", record), \
                        patch.object(ci.subprocess, "check_output", return_value="f" * 40), \
                        redirect_stdout(io.StringIO()):
                    ci.main(stage="Python")
                    self.assertTrue(dict(os.environ) == environment, "CI launch changed the host environment")
                adapter = [env for command, env in children
                           if "centaeris-e2b-adapter" in command]
                self.assertEqual(len(adapter), 1)
                self.assertEqual(adapter[0].get("E2B_CONNECTION_RETRIES"), "0")
                others = [env for command, env in children
                          if "centaeris-e2b-adapter" not in command]
                self.assertTrue(others)
                self.assertTrue(all(env.get("E2B_CONNECTION_RETRIES") == inherited for env in others))

    def test_workspace_harbor_runs_with_pinned_dependency_early_in_python_stage(self):
        harbor = ["uv", "run", "--no-project", "--with", "harbor==0.21.0", "python", "-B",
                  "-m", "unittest", "discover", "-s", "scripts/harbor", "-p", "test_workspace*.py"]
        config = {"services": {"document-processor": {}, "workspace-general": {},
                  "runtime": {"depends_on": {"workspace-general": {"condition": "service_completed_successfully"}}},
                  "material-worker": {"depends_on": {"document-processor": {"condition": "service_completed_successfully"}}}}}
        for stage in ("Python", "All", "Rust", "Web"):
            with self.subTest(stage=stage):
                calls = []
                def record(label, args, **kwargs):
                    calls.append(args)
                    return json.dumps(config)
                with patch.object(ci, "run", record):
                    ci.main(stage=stage)
                self.assertEqual(calls.count(harbor), 1 if stage in ("Python", "All") else 0)
                if stage in ("Python", "All"):
                    adapter = calls.index(harbor)
                    for script in ("scripts/workspace/runtime_outbox_gate.py", "scripts/workspace/python_test_gate.py"):
                        self.assertLess(adapter, next(i for i, command in enumerate(calls) if script in command))

    def test_workspace_harbor_failure_stops_before_database_and_api_gates(self):
        calls = []
        def fail_adapter(label, args, **kwargs):
            calls.append(args)
            if "harbor==0.21.0" in args:
                raise RuntimeError("adapter regression")
            return json.dumps({"services": {"document-processor": {}, "workspace-general": {},
                "runtime": {"depends_on": {"workspace-general": {"condition": "service_completed_successfully"}}},
                "material-worker": {"depends_on": {"document-processor": {"condition": "service_completed_successfully"}}}}})
        with patch.object(ci, "run", fail_adapter):
            with self.assertRaisesRegex(RuntimeError, "adapter regression"):
                ci.main(stage="Python")
        self.assertFalse(any("scripts/workspace/runtime_outbox_gate.py" in command
                             or "scripts/workspace/python_test_gate.py" in command for command in calls))

    def test_compose_structure_chooses_isolated_capacity_inputs_without_changing_host_environment(self):
        host_environment = {key: value for key, value in os.environ.items()
                            if not key.startswith("UPLOAD_") and key not in (
                                "WORK_RETURN_AUDIT_INTERVAL_SECONDS", "ENVIRONMENT_EXECUTION_HELPER")}
        with patch.dict(os.environ, host_environment, clear=True), \
                patch.object(ci.subprocess, "check_output", return_value="f" * 40), \
                patch.object(ci.subprocess, "run") as command:
            ci.run("Compose structure", ["docker", "compose", "--env-file", ".env.example", "config"])
            environment = command.call_args.kwargs["env"]
            self.assertEqual(environment["UPLOAD_BODY_MAX_BYTES"], "1048576")
            self.assertEqual(environment["UPLOAD_TEMP_MAX_BYTES"], "4194304")
            self.assertEqual(environment["UPLOAD_MAX_CONCURRENT"], "2")
            self.assertEqual(environment["WORK_RETURN_AUDIT_INTERVAL_SECONDS"], "3600")
            self.assertEqual(environment["ENVIRONMENT_EXECUTION_HELPER"], "/opt/centaeris/host/execution_agent")
            self.assertEqual(dict(os.environ), host_environment)
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

    def test_web_checks_shared_types_and_component_interactions(self):
        config = {"services": {"document-processor": {}, "workspace-general": {},
            "runtime": {"depends_on": {"workspace-general": {"condition": "service_completed_successfully"}}},
            "material-worker": {"depends_on": {"document-processor": {"condition": "service_completed_successfully"}}}}}
        for skip in (False, True):
            calls = []
            with patch.object(ci, "run", side_effect=lambda label, args, **kw: calls.append(args) or json.dumps(config)):
                ci.main(skip, stage="Web")
            self.assertTrue(any("@centaeris/frontend-foundations" in command and "typecheck" in command for command in calls))
            self.assertEqual(any("test:components" in command for command in calls), not skip)

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
                           "scripts/workspace/platform-mcp-client-gate.py", "scripts/workspace/retained_acceptance"):
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
