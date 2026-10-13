"""Guard and oracle contracts; these tests do not claim real process recovery."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from perf.harness import durable_execution_probe as probe


class DurableExecutionProbeTests(unittest.TestCase):
    def test_readiness_waits_for_application_and_progress_without_replaying_mutations(self):
        experiment = probe.Experiment(Path("unused"), "case-ready", "runsc")
        observations = [RuntimeError("socket not ready"), {"counter": 0, "threadTicks": 0},
                        {"counter": 1, "threadTicks": 1}]
        with patch.object(experiment, "observe", side_effect=observations) as observe, \
                patch.object(probe.time, "sleep"):
            result = experiment.wait_observation("source", lambda value: value["counter"] > 0)
        self.assertEqual(result["counter"], 1)
        self.assertEqual(observe.call_count, 3)
        self.assertTrue(all(call.args == ("source",) for call in observe.call_args_list))

    def test_readiness_has_a_deadline_and_does_not_hide_invalid_json(self):
        experiment = probe.Experiment(Path("unused"), "case-ready", "runsc")
        with patch.object(experiment, "observe", side_effect=RuntimeError("unavailable")), \
                patch.object(probe.time, "monotonic", side_effect=[0, 0, 11]), \
                patch.object(probe.time, "sleep"):
            with self.assertRaisesRegex(TimeoutError, "observation"):
                experiment.wait_observation("source", lambda value: True, timeout=10)
        with patch.object(experiment, "observe", side_effect=ValueError("invalid response")):
            with self.assertRaisesRegex(ValueError, "invalid response"):
                experiment.wait_observation("source", lambda value: True)

    def test_experiment_directory_is_unique_owned_and_never_a_link(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            path = probe.create_experiment(base, "case-abc123")
            self.assertEqual(path.parent, base.resolve())
            self.assertEqual(json.loads((path / "ownership.json").read_text())["experimentId"], "case-abc123")
            with self.assertRaises(FileExistsError):
                probe.create_experiment(base, "case-abc123")
            for invalid in ("../other", "", "a/b", "a b", ".", "x" * 97):
                with self.assertRaises(ValueError):
                    probe.create_experiment(base, invalid)

    def test_oci_spec_has_no_host_mounts_and_enforces_finite_limits(self):
        spec = probe.build_spec("case-abc123")
        self.assertEqual(spec["process"]["user"], {"uid": 1000, "gid": 1000})
        self.assertTrue(spec["process"]["noNewPrivileges"])
        self.assertEqual(spec["root"], {"path": "rootfs", "readonly": True})
        self.assertEqual(spec["linux"]["resources"]["pids"]["limit"], 96)
        self.assertEqual(spec["linux"]["resources"]["memory"]["limit"], 536870912)
        self.assertEqual(spec["linux"]["resources"]["cpu"]["quota"], 100000)
        self.assertEqual(spec["linux"]["cgroupsPath"], "/centaeris-durable-probe-case-abc123")
        self.assertFalse(any(m["type"] == "bind" for m in spec["mounts"]))
        self.assertIn("size=16777216", next(m for m in spec["mounts"] if m["destination"] == "/work")["options"])

    def test_recovery_requires_exact_live_memory_and_stopped_source(self):
        before = {"nonce": "memory-only", "counter": 8, "mmapNonce": "mapped-only",
                  "fileCounter": 8, "fdOffset": 6, "unlinkedOffset": 7,
                  "pipeBytes": 10, "childPid": 2, "paused": True, "threadTicks": 8}
        self.assertTrue(probe.compare_recovery(before, dict(before), source_stopped=True)["passed"])
        for key in before:
            after = dict(before)
            after[key] = "changed"
            self.assertFalse(probe.compare_recovery(before, after, source_stopped=True)["passed"], key)
        self.assertFalse(probe.compare_recovery(before, before, source_stopped=False)["passed"])
        self.assertFalse(probe.compare_recovery({}, {}, source_stopped=True)["passed"])

    def test_checkpoint_integrity_includes_all_files_and_rejects_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "pages.img").write_bytes(b"checkpoint-pages")
            (root / "metadata.img").write_bytes(b"metadata")
            identity = probe.seal_checkpoint(root, "runtime-digest")
            probe.verify_checkpoint(root, identity, "runtime-digest")
            with self.assertRaises(ValueError):
                probe.verify_checkpoint(root, identity, "different-runtime")
            (root / "pages.img").write_bytes(b"truncated")
            with self.assertRaises(ValueError):
                probe.verify_checkpoint(root, identity, "runtime-digest")

    def test_cleanup_failures_cannot_be_reported_as_supported(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            experiment = probe.Experiment(root, "case-abc123", "/usr/bin/runsc")
            experiment.mounted = True
            failed = probe.subprocess.CompletedProcess([], 1, "", "owned cleanup failed")
            with patch.object(experiment, "runtime", return_value=failed), \
                    patch.object(experiment, "command", return_value=failed):
                outcome = experiment.finish(None)
            self.assertEqual(outcome["status"], "failed")
            self.assertEqual(json.loads((root / "outcome.json").read_text())["status"], "failed")

    def test_cleanup_refuses_a_mount_replaced_with_another_backing_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            experiment = probe.Experiment(root, "case-abc123", "/usr/bin/runsc")
            experiment.mounted = True
            def command(arguments, **_kwargs):
                if arguments[0] == "losetup":
                    output = json.dumps({"loopdevices": [{"back-file": str(root / "unrelated.img")}]})
                elif "--json" in arguments:
                    output = json.dumps({"filesystems": [{"target": str(experiment.disk), "source": "/dev/loop9", "fstype": "ext4"}]})
                else:
                    output = str(experiment.disk)
                return probe.subprocess.CompletedProcess(arguments, 0, output, "")
            with patch.object(experiment, "runtime", return_value=probe.subprocess.CompletedProcess([], 0, "", "")) as runtime, patch.object(experiment, "command", side_effect=command) as commands:
                result = experiment.finish(None)
            runtime.assert_not_called()
            self.assertFalse(any(call.args[0][0] == "umount" for call in commands.call_args_list))
            self.assertEqual(result["status"], "failed")

    def test_checkpoints_reject_unexpected_extra_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "state").write_bytes(b"valid-state")
            identity = probe.seal_checkpoint(root, "runtime")
            (root / "unexpected").write_bytes(b"unlisted")
            with self.assertRaises(ValueError):
                probe.verify_checkpoint(root, identity, "runtime")

    def test_capabilities_never_convert_missing_measurement_into_support(self):
        matrix = probe.capability_matrix()
        self.assertTrue(matrix)
        self.assertTrue(all(value["status"] == "not-tested" for value in matrix.values()))
        self.assertIn("crossHostRecovery", matrix)
        self.assertIn("externalConnections", matrix)


if __name__ == "__main__":
    unittest.main()
