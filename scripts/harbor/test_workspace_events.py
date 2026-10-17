"""The monitor stream is a bounded, sanitized projection of controller facts."""
import json
from pathlib import Path
import tempfile
import threading
import unittest

from workspace_events import JobEvents, ProjectionConflict, FailOpenProjection
from workspace_monitor import JobProjection


def manifest():
    return {"schema": "centaeris.workspace_benchmark.job.v1", "jobId": "pilot",
            "title": "Workspace pilot", "taskCount": 1, "attemptsPerTask": 1,
            "trialCount": 1, "maxConcurrent": 2,
            "tasks": [{"taskId": "regex-log", "memoryMiB": 2048}]}


class EventsTests(unittest.TestCase):
    def setUp(self):
        temporary_root = Path(__file__).resolve().parents[2] / ".tmp"
        temporary_root.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=temporary_root)
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def test_real_projection_receives_manifest_lifecycle_and_redacted_log(self):
        with JobEvents.create(self.root, manifest()) as writer:
            writer.job("running")
            writer.trial("trial-1", taskId="regex-log", attempt=1, status="running",
                         phase="agent", slot=1, agentRunId="agent-run-1", resourceHolding=1)
            writer.log("Authorization: Bearer sk-never-emit-this", level="warning")
        state = JobProjection(self.root).snapshot()
        self.assertIsNone(state["monitorError"])
        self.assertEqual(state["trials"][0]["agentRunId"], "agent-run-1")
        self.assertEqual(state["slots"][0]["trialIds"], ["trial-1"])
        self.assertNotIn("sk-never-emit-this", (self.root / "events.jsonl").read_text())

    def test_job_identity_cannot_be_overwritten_and_no_implicit_resume(self):
        with JobEvents.create(self.root, manifest()):
            with self.assertRaises(ProjectionConflict):
                JobEvents.create(self.root, manifest())
        with self.assertRaises(ProjectionConflict):
            JobEvents.create(self.root, manifest())
        self.assertEqual(json.loads((self.root / "manifest.json").read_text()), manifest())

    def test_unknown_fields_and_conflicting_identity_rejected_before_append(self):
        with JobEvents.create(self.root, manifest()) as writer:
            with self.assertRaises(ValueError):
                writer.trial("t1", taskId="regex-log", attempt=1, status="running",
                             phase="agent", password="secret")
            writer.trial("t1", taskId="regex-log", attempt=1, status="running", phase="agent")
            with self.assertRaises(ValueError):
                writer.trial("t2", taskId="regex-log", attempt=1, status="running", phase="agent")
        records = (self.root / "events.jsonl").read_text().splitlines()
        self.assertEqual(len(records), 1)
        self.assertEqual(json.loads(records[0])["sequence"], 1)

    def test_concurrent_observers_produce_contiguous_complete_records(self):
        with JobEvents.create(self.root, manifest()) as writer:
            threads = [threading.Thread(target=lambda: [writer.log("checked") for _ in range(25)])
                       for _ in range(4)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()
        records = [json.loads(line) for line in (self.root / "events.jsonl").read_text().splitlines()]
        self.assertEqual([item["sequence"] for item in records], list(range(1, 101)))
        self.assertIsNone(JobProjection(self.root).snapshot()["monitorError"])

    def test_output_quota_fails_explicitly_without_partial_record(self):
        with JobEvents.create(self.root, manifest(), max_bytes=4096) as writer:
            with self.assertRaisesRegex(ValueError, "projection quota"):
                for _ in range(50):
                    writer.log("z" * 2000)
        wire = (self.root / "events.jsonl").read_bytes()
        self.assertLessEqual(len(wire), 4096)
        for line in wire.splitlines():
            json.loads(line)

    def test_closed_writer_rejects_new_facts(self):
        writer = JobEvents.create(self.root, manifest())
        writer.close()
        with self.assertRaisesRegex(ValueError, "closed"):
            writer.job("completed")


class IsolationTests(unittest.TestCase):
    def test_projection_failure_cannot_fail_or_cancel_the_trial(self):
        class FailedWriter:
            def log(self, *_args, **_kwargs):
                raise OSError("full projection disk")

            def close(self):
                pass

        sink = FailOpenProjection(FailedWriter())
        self.assertTrue(sink.submit("log", {"message": "started", "level": "info"}))
        sink.close(timeout=1)
        self.assertTrue(sink.stats["degraded"])
        self.assertFalse(sink.submit("log", {"message": "later", "level": "info"}))

    def test_slow_projection_is_bounded_and_never_blocks_a_producer(self):
        entered, release = threading.Event(), threading.Event()

        class SlowWriter:
            def log(self, *_args, **_kwargs):
                entered.set()
                release.wait(2)

            def close(self):
                pass

        sink = FailOpenProjection(SlowWriter(), max_pending=1)
        try:
            sink.submit("log", {"message": "one", "level": "info"})
            self.assertTrue(entered.wait(1))
            self.assertTrue(sink.submit("log", {"message": "two", "level": "info"}))
            self.assertFalse(sink.submit("log", {"message": "three", "level": "info"}))
            self.assertTrue(sink.stats["degraded"])
            self.assertLessEqual(sink.stats["pending"], 1)
        finally:
            release.set()
            sink.close(timeout=1)


if __name__ == "__main__":
    unittest.main()
