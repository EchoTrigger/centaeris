import io
import tempfile
import unittest
from pathlib import Path

from python_test_gate import AuditedRunner, shard_suite, test_ids


class PythonGateTests(unittest.TestCase):
    def run_gate(self, suite):
        return AuditedRunner(stream=io.StringIO()).run(suite)

    def test_shards_cover_every_case_once_and_keep_classes_together(self):
        class First(unittest.TestCase):
            def test_one(self): pass
            def test_two(self): pass
            def test_three(self): pass

        class Second(unittest.TestCase):
            def test_one(self): pass
            def test_two(self): pass

        class Third(unittest.TestCase):
            def test_one(self): pass

        class Fourth(unittest.TestCase):
            def test_one(self): pass

        suite = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(case)
                                   for case in (First, Second, Third, Fourth))
        expected = list(test_ids(suite))
        parts = [list(test_ids(shard_suite(suite, index, 3))) for index in range(3)]
        self.assertCountEqual([identity for part in parts for identity in part], expected)
        for case in (First, Second, Third, Fourth):
            identities = set(test_ids(unittest.defaultTestLoader.loadTestsFromTestCase(case)))
            self.assertEqual(sum(identities.issubset(set(part)) for part in parts), 1)
        for index, part in enumerate(parts):
            self.assertEqual(part, [identity for identity in expected if identity in part])
            self.assertEqual(list(test_ids(shard_suite(suite, index, 3))), part)
        self.assertEqual(max(map(len, parts)) - min(map(len, parts)), 1)

    def test_unsharded_execution_preserves_the_original_suite(self):
        suite = unittest.TestSuite()
        self.assertIs(shard_suite(suite, 0, 1), suite)

    def test_invalid_shard_coordinates_fail(self):
        for index, count in ((0, 0), (-1, 3), (3, 3)):
            with self.subTest(index=index, count=count), self.assertRaises(ValueError):
                shard_suite(unittest.TestSuite(), index, count)

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
