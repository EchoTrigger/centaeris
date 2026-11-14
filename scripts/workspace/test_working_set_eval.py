import importlib.util
import json
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("working_set_eval", Path(__file__).with_name("working_set_eval.py"))
EVAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVAL)


class WorkingSetEvidenceTests(unittest.TestCase):
    def sample(self):
        expected = {"project": "p", "owner": "mira", "path": "a.rs", "error": "unknown", "action": "query"}
        return {"mode": "quality", "case": "normal-history", "expected": expected,
                "reply": json.dumps(expected), "stop": "finalized", "error": None,
                "proofReads": 0, "compactions": [], "mainCalls": 1, "summaryCalls": 0,
                "elapsedMs": 2.0, "firstRequestPreparedMs": 1.0, "costUpperUsd": .01}

    def test_evidence_rejects_a_self_declared_pass_with_wrong_facts(self):
        sample = self.sample()
        self.assertEqual(EVAL.validate_sample(sample), [])
        sample["passed"] = True
        sample["reply"] = json.dumps({**sample["expected"], "owner": "wrong"})
        self.assertIn("task_result_mismatch", EVAL.validate_sample(sample))

    def test_tool_proof_and_compaction_count_are_not_inferred_from_final_text(self):
        sample = self.sample()
        sample["case"] = "normal-tool"
        self.assertIn("tool_read_count_mismatch", EVAL.validate_sample(sample))
        sample["proofReads"] = 1
        self.assertEqual(EVAL.validate_sample(sample), [])
        sample["case"] = "repeat-original"
        sample["proofReads"] = 0
        sample["compactions"] = [True]
        self.assertIn("compaction_count_or_outcome_mismatch", EVAL.validate_sample(sample))

    def test_percentile_uses_samples_and_compile_wrapper_preserves_source_roots(self):
        self.assertEqual(EVAL.percentile(list(range(1, 101)), .95), 95)
        root = Path("baseline").resolve()
        harness = Path("candidate/example.rs").resolve()
        manifest = EVAL.manifest(root, harness)
        self.assertIn(str(root / "packages/core").replace("\\", "/"), manifest)
        self.assertIn(str(harness).replace("\\", "/"), manifest)


if __name__ == "__main__":
    unittest.main()
