import hashlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

from perf.harness import conditional_snapshot_probe as probe
from perf.harness.conditional_snapshot_probe import inspect_capture


def encoded(value):
    data = json.dumps(value, separators=(",", ":")).encode()
    return len(data).to_bytes(4, "big") + data


class ConditionalSnapshotReceiptTests(unittest.TestCase):
    def setUp(self):
        self.manifest = {"schema": "workspace.snapshot.v1", "files": []}
        self.manifest_hash = "sha256:" + hashlib.sha256(encoded(self.manifest)[4:]).hexdigest()
        self.baseline = {"snapshotSha256": "sha256:" + "a" * 64, "manifest": self.manifest}
        self.response = {"schema": "workspace.snapshot.capture.v1", "captureId": "attempt-1",
                         "disposition": "unchanged", "baselineSnapshotSha256": self.baseline["snapshotSha256"],
                         "manifestSha256": self.manifest_hash}

    def inspect(self, response=None, suffix=b"", baseline=True):
        return inspect_capture(io.BytesIO(encoded(response or self.response) + suffix), "attempt-1",
                               self.baseline if baseline else None)

    def test_unchanged_receipt_requires_the_exact_requested_baseline_and_no_payload(self):
        result = self.inspect()
        self.assertEqual(result["disposition"], "unchanged")
        self.assertEqual(result["snapshotFrameBytes"], 0)
        for field, value in (("captureId", "earlier-attempt"), ("baselineSnapshotSha256", "sha256:" + "b" * 64),
                             ("manifestSha256", "sha256:" + "c" * 64), ("schema", "unknown"),
                             ("disposition", "unknown"), ("extra", True)):
            with self.subTest(field=field), self.assertRaises(ValueError):
                self.inspect({**self.response, field: value})
        with self.assertRaises(ValueError):
            self.inspect(suffix=b"unexpected")
        with self.assertRaises(ValueError):
            self.inspect(baseline=False)

    def test_snapshot_receipt_binds_the_actual_frame_and_empty_snapshot(self):
        response = {**self.response, "disposition": "snapshot", "baselineSnapshotSha256": None}
        result = self.inspect(response)
        self.assertEqual(result["snapshotFrameBytes"], 0)
        self.assertEqual(result["inspection"]["files"], [])
        with self.assertRaises(ValueError):
            self.inspect({**response, "manifestSha256": "sha256:" + "b" * 64})
        with self.assertRaises(ValueError):
            self.inspect({**response, "baselineSnapshotSha256": self.baseline["snapshotSha256"]})

    def test_corrupt_or_truncated_frames_cannot_be_reported_as_success(self):
        payload = b"fixture bytes"
        manifest = {"schema": "workspace.snapshot.v1", "files": [{"path": "fixture/hot.bin", "sizeBytes": len(payload),
            "sha256": "sha256:" + hashlib.sha256(payload).hexdigest(), "executable": False}]}
        frame = encoded(manifest) + payload
        response = {**self.response, "disposition": "snapshot", "baselineSnapshotSha256": None,
                    "manifestSha256": "sha256:" + hashlib.sha256(encoded(manifest)[4:]).hexdigest()}
        result = self.inspect(response, frame)
        self.assertEqual(result["snapshotFrameBytes"], len(frame))
        for corrupted in (frame[:-1], frame[:-1] + b"x", frame + b"extra"):
            with self.subTest(corrupted=corrupted[-8:]), self.assertRaises(ValueError):
                self.inspect(response, corrupted)
        with self.assertRaises(ValueError):
            inspect_capture(io.BytesIO(b"\xff\xff\xff\xff"), "attempt-1", self.baseline)

    def test_duplicate_receipt_fields_are_not_silently_accepted(self):
        raw = json.dumps(self.response, separators=(",", ":"))
        raw = raw[:-1] + ',"captureId":"attempt-1"}'
        data = raw.encode()
        with self.assertRaises(ValueError):
            inspect_capture(io.BytesIO(len(data).to_bytes(4, "big") + data), "attempt-1", self.baseline)


class ConditionalSnapshotOutcomeTests(unittest.TestCase):
    def outcome(self, completed, failure=None, pending=False, cleanup_failure=False):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory).resolve()
            helper = parent / "synthetic_helper"
            helper.write_bytes(b"unit test only")
            experiment = MagicMock()
            experiment.events, experiment.commands = [], []
            experiment.results = [{} for _ in range(completed)]
            experiment.active = {"b" * 64: "owned-test"} if cleanup_failure else {}
            experiment.pending = {"unconfirmed-test"} if pending else set()
            experiment.reconcile_start.side_effect = RuntimeError("creation unknown")
            experiment.remove.side_effect = RuntimeError("removal unconfirmed")
            experiment.run.side_effect = failure
            argv = ["probe", "--evidence-parent", str(parent), "--experiment-id", "unit-test",
                "--image-id", "sha256:" + "a" * 64, "--agent", str(helper), "--cold-bytes", "8192"]
            with patch("sys.argv", argv), patch.object(probe, "ConditionalProbe", return_value=experiment), \
                    patch.object(probe.os, "statvfs", create=True, return_value=SimpleNamespace(f_blocks=100, f_frsize=4096)), \
                    patch.object(probe.subprocess, "check_output", return_value=json.dumps({"filesystems": [
                        {"target": str(parent), "source": "/dev/loop0", "fstype": "ext4"}]}).encode()), \
                    patch("sys.stdout", io.StringIO()):
                try:
                    result = probe.main()
                except BaseException as error:
                    result = error
            return result, json.loads((parent / "conditional-unit-test/outcome.json").read_text())

    def test_interruption_keeps_completed_count_and_failed_outcome(self):
        result, outcome = self.outcome(2, KeyboardInterrupt())
        self.assertIsInstance(result, KeyboardInterrupt)
        self.assertEqual(outcome["status"], "failed")
        self.assertEqual(outcome["completedWorkloads"], 2)
        self.assertEqual(outcome["failure"]["type"], "KeyboardInterrupt")

    def test_only_all_workloads_and_confirmed_cleanup_can_pass(self):
        for completed, pending, cleanup_failure in ((0, False, False), (7, True, False), (7, False, True)):
            with self.subTest(completed=completed, pending=pending, cleanup_failure=cleanup_failure):
                result, outcome = self.outcome(completed, pending=pending, cleanup_failure=cleanup_failure)
                self.assertEqual(result, 1)
                self.assertEqual(outcome["status"], "failed")
        result, outcome = self.outcome(len(probe.WORKLOADS))
        self.assertEqual(result, 0)
        self.assertEqual(outcome["status"], "passed")


if __name__ == "__main__":
    unittest.main()
