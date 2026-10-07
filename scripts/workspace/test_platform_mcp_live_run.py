"""Offline checks protecting the live gate against false positive citations."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("material_live_gate", Path(__file__).with_name("platform-mcp-live-run.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class CitationExposureTests(unittest.TestCase):
    def test_answer_or_tool_text_is_not_a_browser_citation(self):
        for kind in ("assistant_message", "tool_result"):
            history = {"agentRuns": [{"id": "run", "citations": [], "events": [
                {"sequence": 1, "event": {"type": kind, "payload": {"citationId": "citation:one", "modelMarkdown": "citation:one"}}}
            ]}]}
            self.assertFalse(gate.history_exposes_citation(history, "run", "citation:one"))

    def test_citation_requires_matching_run_and_id(self):
        history = {"agentRuns": [{"id": "run", "events": [], "citations": [
            {"citationId": "citation:one", "sourceUrl": "/api/citations/citation:one"}
        ]}]}
        self.assertTrue(gate.history_exposes_citation(history, "run", "citation:one"))
        self.assertFalse(gate.history_exposes_citation(history, "other", "citation:one"))
        self.assertFalse(gate.history_exposes_citation(history, "run", "citation:other"))


if __name__ == "__main__":
    unittest.main()
