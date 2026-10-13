"""Accounting must retain failed work and distinguish occupancy from I/O."""
import tempfile
import unittest
from pathlib import Path

from perf.harness import workspace_protection as probe


class WorkspaceProtectionTests(unittest.TestCase):
    def event(self, metric, value, **extra):
        return {"schema": "workspace.perf.v1", "source": "api",
                "phase": "workspaceProtection", "operationId": "attempt-1",
                "metric": metric, "value": value, **extra}

    def test_failed_attempt_bytes_count_and_gauges_are_not_added(self):
        rows = [self.event("apiReceivedBytes", 10, outcome="error"),
                self.event("apiReceivedBytes", 15, operationId="attempt-2", outcome="ok"),
                self.event("temporaryBytes", 10), self.event("temporaryBytes", 0)]
        result = probe.summarize(rows)
        self.assertEqual(result["counters"]["apiReceivedBytes"], 25)
        self.assertEqual(result["gauges"]["api:attempt-1:temporaryBytes"], {"last": 0, "peak": 10})
        self.assertNotIn("totalElapsedMs", result)

    def test_missing_measurements_are_unknown_and_invalid_samples_fail(self):
        result = probe.summarize([self.event("apiReceivedBytes", 7)])
        self.assertIsNone(result["counters"]["apiWrittenBytes"])
        for row in (self.event("apiReceivedBytes", -1), self.event("apiReceivedBytes", True),
                    self.event("apiReceivedBytes", float("nan")),
                    self.event("apiReceivedBytes", 2, markdown="private"),
                    self.event("imaginaryBytes", 1)):
            with self.assertRaises(ValueError):
                probe.summarize([row])
        with self.assertRaises(ValueError):
            probe.summarize([])

    def test_metric_owner_and_timestamp_cannot_carry_untrusted_content(self):
        for row in (self.event("apiWrittenBytes", 123, source="sandbox"),
                    self.event("scannedEntries", 2, source="runtime"),
                    self.event("apiWrittenBytes", 1, atUnixMs={"body": "not-an-event-field"}),
                    self.event("apiWrittenBytes", 1, atUnixMs=True),
                    self.event("apiWrittenBytes", 1, atUnixMs=-1)):
            with self.subTest(row=row), self.assertRaises(ValueError):
                probe.validate_event(row)

    def test_hardlink_publication_does_not_inflate_write_bytes(self):
        result = probe.summarize([self.event("apiWrittenBytes", 16),
                                  self.event("publishedLinks", 1)])
        self.assertEqual(result["counters"]["apiWrittenBytes"], 16)
        self.assertEqual(result["counters"]["publishedLinks"], 1)

    def test_timings_retain_failure_outcome_and_trigger(self):
        result = probe.summarize([self.event("commitElapsedMs", 7.5,
                                            outcome="error", trigger="workspaceCommit")])
        self.assertEqual(result["timings"][0]["outcome"], "error")
        self.assertEqual(result["timings"][0]["trigger"], "workspaceCommit")

    def test_other_successful_attempt_does_not_hide_a_missing_sample(self):
        result = probe.summarize([self.event("apiReceivedBytes", 16),
                                  self.event("apiWrittenBytes", 16),
                                  self.event("apiReceivedBytes", 8, operationId="attempt-2")])
        self.assertEqual(result["counters"]["apiWrittenBytes"], 16)
        self.assertEqual(result["counterCoverage"]["apiWrittenBytes"]["unmeasuredOperations"], ["api:attempt-2"])

    def test_workloads_have_identical_initial_baselines_and_observable_mutations(self):
        with tempfile.TemporaryDirectory() as parent:
            initial = []
            for name in probe.WORKLOADS:
                root = Path(parent) / name
                initial.append(probe.prepare_workload(root, cold_bytes=8192)["treeSha256"])
                before = probe.tree_manifest(root)
                probe.mutate_workload(root, name)
                after = probe.tree_manifest(root)
                if name in {"unchanged", "rename-delete-revert"}:
                    self.assertEqual(before, after)
                else:
                    self.assertNotEqual(before, after)
            self.assertEqual(len(set(initial)), 1)

    def test_existing_or_aliased_workload_root_is_rejected(self):
        with tempfile.TemporaryDirectory() as parent:
            with self.assertRaises(FileExistsError):
                probe.prepare_workload(Path(parent), cold_bytes=8192)
            root = Path(parent) / "owned"
            probe.prepare_workload(root, cold_bytes=8192)
            (root / "unexpected").mkdir()
            with self.assertRaises(ValueError):
                probe.mutate_workload(root, "hot-file")


if __name__ == "__main__":
    unittest.main()
