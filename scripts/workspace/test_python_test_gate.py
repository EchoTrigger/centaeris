import io
import tempfile
import unittest
from pathlib import Path

from python_test_gate import AuditedRunner


class PythonGateTests(unittest.TestCase):
    def run_gate(self, suite):
        return AuditedRunner(stream=io.StringIO()).run(suite)

    def test_empty_suite_fails(self):
        self.assertFalse(self.run_gate(unittest.TestSuite()).wasSuccessful())

    def test_skipped_case_is_not_green(self):
        class Cases(unittest.TestCase):
            @unittest.skip("not exercised")
            def test_skip(self):
                pass

        self.assertFalse(self.run_gate(unittest.defaultTestLoader.loadTestsFromTestCase(Cases)).wasSuccessful())

    def test_expected_failure_is_not_green(self):
        class Cases(unittest.TestCase):
            @unittest.expectedFailure
            def test_expected(self):
                self.fail("not fixed")

        self.assertFalse(self.run_gate(unittest.defaultTestLoader.loadTestsFromTestCase(Cases)).wasSuccessful())

    def test_import_error_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "test_gate_broken.py").write_text("raise ImportError('missing dependency')\n", encoding="utf-8")
            self.assertFalse(self.run_gate(unittest.TestLoader().discover(directory)).wasSuccessful())

    def test_dropped_case_fails_even_if_executed_case_passes(self):
        class Cases(unittest.TestCase):
            def test_one(self):
                pass

            def test_two(self):
                pass

        class DroppingSuite(unittest.TestSuite):
            def run(self, result, debug=False):
                self._tests[0](result)
                return result

        self.assertFalse(self.run_gate(DroppingSuite(unittest.defaultTestLoader.loadTestsFromTestCase(Cases))).wasSuccessful())

    def test_discovery_includes_new_module(self):
        with tempfile.TemporaryDirectory() as directory:
            for name in ("first", "new"):
                Path(directory, f"test_gate_{name}.py").write_text(
                    "import unittest\nclass Case(unittest.TestCase):\n    def test_pass(self): pass\n",
                    encoding="utf-8",
                )
            result = self.run_gate(unittest.TestLoader().discover(directory))
            self.assertTrue(result.wasSuccessful())
            self.assertEqual(result.testsRun, 2)


if __name__ == "__main__":
    unittest.main()
