import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("authorization_gate", Path(__file__).with_name("agent-run-authorization-gate.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class AuthorizationGateTests(unittest.TestCase):
    def test_requires_python_artifact_and_positive_rust_receipt(self):
        for missing in ("artifact", "receipt", "empty", None):
            calls = []
            paths = []
            def run(args, env):
                calls.append(args)
                artifact = Path(env["AGENT_RUN_AUTHORIZATION_VECTORS"])
                receipt = Path(env["AGENT_RUN_AUTHORIZATION_RECEIPT"])
                paths.extend([artifact, receipt])
                if args[0] == "uv" and missing != "artifact":
                    artifact.write_text("[]")
                if args[0] == "cargo" and missing != "receipt":
                    receipt.write_text("0" if missing == "empty" else "14")
            if missing:
                with self.assertRaises(RuntimeError):
                    gate.run_gate(run)
            else:
                self.assertEqual(gate.run_gate(run), 14)
            self.assertTrue(all(not p.exists() for p in paths))
            self.assertEqual(len(calls), 1 if missing == "artifact" else 2)

    def test_failed_python_does_not_run_rust(self):
        calls = []
        def fail(args, env):
            calls.append(args)
            raise RuntimeError("child failed")
        with self.assertRaisesRegex(RuntimeError, "child failed"):
            gate.run_gate(fail)
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
