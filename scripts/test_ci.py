import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("ci", Path(__file__).with_name("ci.py"))
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


class PortableGateTests(unittest.TestCase):
    def test_rust_gate_keeps_focused_and_full_behavioral_gates_on_each_platform(self):
        for platform in ("win32", "darwin", "linux"):
            calls = []
            ci.run_gate("Rust", lambda args, **kw: calls.append(args) or "", platform=platform)
            self.assertIn(["cargo", "test", "--locked", "-p", "centaeris-core", "query_loop"], calls)
            self.assertIn([ci.sys.executable, "-B", "-m", "unittest", "discover", "-s",
                           "scripts/harbor", "-p", "test_runner.py"], calls)
            self.assertIn(["cargo", "test", "--locked", "-p", "centaeris-runtime-sqlite", "--test", "core_runtime"], calls)
            self.assertIn(["cargo", "run", "--locked", "-p", "centaeris-runtime", "--bin", "centaeris-runtime-protocol-docs", "--", "--check"], calls)
            self.assertEqual(calls[-1], ["cargo", "test", "--locked", *ci.package_flags()])
            self.assertFalse(any("pwsh" in c for c in calls))
            self.assertFalse(any("runtime_server" in c or "hosted_execution" in c for c in calls))

    def test_node_source_gate_preserves_build_lint_host_parity_and_licenses(self):
        calls = []
        ci.run_gate("Node", lambda args, **kw: calls.append(args) or "", platform="linux")
        for script in ("build", "lint:source", "check:syntax", "check:host-parity", "test:third-party-licenses"):
            self.assertTrue(any(script in c for c in calls), script)
        self.assertFalse(any("smoke:window" in c for c in calls))

    def test_node_gate_uses_a_frozen_pnpm_install_on_every_platform(self):
        for platform, command in (("win32", "pnpm.cmd"), ("linux", "pnpm"), ("darwin", "pnpm")):
            calls = []
            ci.run_gate("Node", lambda args, **kw: calls.append(args) or "", platform=platform)
            self.assertIn([command, "install", "--frozen-lockfile"], calls)
            self.assertFalse(any(c[0] in ("npm", "npm.cmd") for c in calls))

    def test_failure_stops_before_later_commands(self):
        calls = []
        def fail(args, **kwargs):
            calls.append(args)
            raise RuntimeError("failed child")
        with self.assertRaisesRegex(RuntimeError, "failed child"):
            ci.run_gate("Rust", fail)
        self.assertEqual(len(calls), 1)

    def test_dependency_boundary_rejects_actual_direct_and_transitive_adapters(self):
        for tree in (
            "centaeris-core v0.1.0\n└── rusqlite v0.32.1",
            "centaeris-core v0.1.0\n└── wrapper v0.1.0\n    └── centaeris-runtime-sqlite v0.1.0",
        ):
            with self.subTest(tree=tree):
                calls = []
                def run(args, **kwargs):
                    calls.append(args)
                    return tree if args[:2] == ["cargo", "tree"] else ""
                with self.assertRaisesRegex(RuntimeError, "SQLite"):
                    ci.run_gate("Rust", run)
                self.assertEqual(calls[-1], ["cargo", "tree", "--locked", "-p", "centaeris-core", "--edges", "normal,build,dev"])

    def test_unknown_stage_fails_before_running_commands(self):
        with self.assertRaises(ValueError):
            ci.run_gate("Release", lambda *args, **kw: self.fail("must not execute"))


if __name__ == "__main__":
    unittest.main()
