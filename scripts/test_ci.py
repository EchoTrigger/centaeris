import importlib.util
from pathlib import Path
import tempfile
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
            self.assertIn(["cargo", "test", "--locked", "-p", "centaeris-runtime-sqlite", "--test", "core_runtime"], calls)
            self.assertIn(["cargo", "run", "--locked", "-p", "centaeris-runtime", "--bin", "centaeris-runtime-protocol-docs", "--", "--check"], calls)
            self.assertEqual(calls[-1], ["cargo", "test", "--workspace", "--locked"])
            self.assertFalse(any("pwsh" in c for c in calls))

    def test_node_source_gate_preserves_build_lint_host_parity_and_licenses(self):
        calls = []
        ci.run_gate("Node", lambda args, **kw: calls.append(args) or "", platform="linux")
        for script in ("build", "lint:source", "check:syntax", "check:host-parity", "test:third-party-licenses"):
            self.assertTrue(any(script in c for c in calls), script)
        self.assertFalse(any("smoke:window" in c for c in calls))

    def test_failure_stops_before_later_commands(self):
        calls = []
        def fail(args, **kwargs):
            calls.append(args)
            raise RuntimeError("failed child")
        with self.assertRaisesRegex(RuntimeError, "failed child"):
            ci.run_gate("Rust", fail)
        self.assertEqual(len(calls), 1)

    def test_dependency_and_source_boundaries_remain_enforced(self):
        with self.assertRaisesRegex(RuntimeError, "SQLite"):
            ci.run_gate("Rust", lambda args, **kw: "rusqlite" if "tree" in args else "")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "packages/core/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("use rusqlite::Connection;")
            with self.assertRaisesRegex(RuntimeError, "SQLite"):
                ci.check_core_boundary(root)

    def test_unknown_stage_fails_before_running_commands(self):
        with self.assertRaises(ValueError):
            ci.run_gate("Release", lambda *args, **kw: self.fail("must not execute"))


if __name__ == "__main__":
    unittest.main()
