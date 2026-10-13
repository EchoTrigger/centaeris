"""Real helper runner contracts; no fake backend result counts as a measurement."""
import hashlib
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

from perf.harness import workspace_snapshot_probe as probe


class SnapshotProbeTests(unittest.TestCase):
    def main_outcome(self, completed, failure=None, unresolved=()):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory).resolve()
            helper = parent / "execution_agent"
            helper.write_bytes(b"synthetic test executable")
            experiment = MagicMock()
            experiment.active, experiment.events, experiment.commands = {}, [], []
            experiment.pending = set(unresolved)
            if unresolved:
                experiment.reconcile_start.side_effect = RuntimeError("creation outcome unknown")
            experiment.results = [{} for _ in range(completed)]
            experiment.run.side_effect = failure
            argv = ["probe", "--evidence-parent", str(parent), "--experiment-id", "interruption-test",
                    "--image-id", "sha256:" + "a" * 64, "--agent", str(helper), "--cold-bytes", "8192"]
            with patch("sys.argv", argv), patch.object(probe, "Probe", return_value=experiment), \
                    patch.object(probe.os, "statvfs", create=True,
                                 return_value=SimpleNamespace(f_blocks=100, f_frsize=4096)), \
                    patch.object(probe.subprocess, "check_output", return_value=json.dumps({
                        "filesystems": [{"target": str(parent), "source": "/dev/loop0", "fstype": "ext4"}]
                    }).encode()), patch("sys.stdout", io.StringIO()):
                try:
                    result = probe.main()
                except BaseException as error:
                    result = error
            outcome = json.loads((parent / "helper-interruption-test/outcome.json").read_text())
            return result, outcome

    def test_interrupted_probe_preserves_completed_count_and_never_claims_success(self):
        result, outcome = self.main_outcome(2, KeyboardInterrupt())
        self.assertIsInstance(result, KeyboardInterrupt)
        self.assertIn(outcome["status"], {"failed", "interrupted"})
        self.assertEqual(outcome["completedWorkloads"], 2)
        self.assertEqual(outcome["failure"]["type"], "KeyboardInterrupt")

    def test_only_a_complete_workload_set_can_pass(self):
        result, outcome = self.main_outcome(0)
        self.assertEqual(result, 1)
        self.assertEqual(outcome["status"], "failed")
        self.assertEqual(outcome["completedWorkloads"], 0)
        result, outcome = self.main_outcome(len(probe.fixture.WORKLOADS))
        self.assertEqual(result, 0)
        self.assertEqual(outcome["status"], "passed")

    def test_unknown_creation_outcome_is_retained_as_a_cleanup_failure(self):
        result, outcome = self.main_outcome(len(probe.fixture.WORKLOADS), unresolved=("owned-source",))
        self.assertEqual(result, 1)
        self.assertEqual(outcome["status"], "failed")
        self.assertEqual(outcome["unresolvedContainerNames"], ["owned-source"])

    def test_failed_launch_reconciles_only_the_owned_identity_for_cleanup(self):
        identity = "b" * 64
        owned = {"Id": identity, "Name": "/review-source", "Config": {"Labels": {probe.LABEL: "review-source"}}}
        empty = SimpleNamespace(stdout=b"", stderr=b"", returncode=0)
        inspected = SimpleNamespace(stdout=json.dumps([owned]).encode(), stderr=b"", returncode=0)
        for failure in (RuntimeError("docker created but failed to start"),
                        subprocess.TimeoutExpired(["docker", "run"], 120)):
            experiment = probe.Probe(Path("unused"), "review", "sha256:" + "a" * 64,
                                     Path("unused"), Path("unused"), 8192)
            with patch.object(probe, "container_arguments", return_value=["docker", "run"]), \
                    patch.object(experiment, "command", side_effect=[empty, failure, inspected, inspected, empty]) as command:
                with self.assertRaises(type(failure)):
                    experiment.start("source")
                self.assertEqual(experiment.pending, {"review-source"})
                experiment.reconcile_start("review-source")
                experiment.remove(identity)
                self.assertFalse(experiment.active)
                self.assertFalse(experiment.pending)
                self.assertEqual(command.call_args.args[0], ["docker", "rm", "--force", identity])

    def test_foreign_or_unknown_launch_results_are_never_removed(self):
        identity = "b" * 64
        foreign = {"Id": identity, "Name": "/review-source", "Config": {"Labels": {probe.LABEL: "foreign"}}}
        for inspected in (SimpleNamespace(stdout=json.dumps([foreign]).encode(), returncode=0),
                          SimpleNamespace(stdout=b"", returncode=1)):
            experiment = probe.Probe(Path("unused"), "review", "sha256:" + "a" * 64,
                                     Path("unused"), Path("unused"), 8192)
            experiment.pending = {"review-source"}
            with patch.object(experiment, "command", return_value=inspected) as command:
                with self.assertRaises(RuntimeError):
                    experiment.reconcile_start("review-source")
                self.assertEqual(experiment.pending, {"review-source"})
                self.assertFalse(experiment.active)
                self.assertEqual(command.call_count, 1)
                self.assertEqual(command.call_args.args[0], ["docker", "inspect", "review-source"])

    def test_preexisting_name_is_rejected_before_launch_even_with_an_owned_label(self):
        experiment = probe.Probe(Path("unused"), "review", "sha256:" + "a" * 64,
                                 Path("unused"), Path("unused"), 8192)
        with patch.object(probe, "container_arguments", return_value=["docker", "run"]), \
                patch.object(experiment, "command", return_value=SimpleNamespace(stdout=b"b" * 64)) as command:
            with self.assertRaisesRegex(ValueError, "already exists"):
                experiment.start("source")
            self.assertEqual(command.call_count, 1)
            self.assertFalse(experiment.active)
            self.assertFalse(experiment.pending)

    def test_snapshot_frame_is_verified_against_payload_not_just_manifest(self):
        content = b"actual bytes"
        manifest = {"schema": "workspace.snapshot.v1", "files": [{"path": "fixture/hot.bin", "sizeBytes": len(content),
                    "sha256": "sha256:" + hashlib.sha256(content).hexdigest(), "executable": False}]}
        encoded = json.dumps(manifest, separators=(",", ":")).encode()
        frame = len(encoded).to_bytes(4, "big") + encoded + content
        result = probe.inspect_frame(io.BytesIO(frame))
        self.assertEqual(result["expandedSizeBytes"], len(content))
        self.assertEqual(result["files"][0]["name"], "hot.bin")
        for bad in (frame[:-1], frame + b"trailing", frame[:-1] + b"!"):
            with self.assertRaises(ValueError):
                probe.inspect_frame(io.BytesIO(bad))

    def test_runtime_config_cannot_reuse_mutable_tags_or_production_mounts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            binary = root / "execution_agent"
            binary.write_bytes(b"test binary")
            module = root / "workspace_protection.py"
            module.write_text("# fixture")
            args = probe.container_arguments("smoke-abc", "sha256:" + "a" * 64, binary, module)
            self.assertIn("--runtime=runsc", args)
            self.assertIn("--network=none", args)
            self.assertIn("--read-only", args)
            self.assertIn("--memory=1g", args)
            self.assertIn("--pids-limit=128", args)
            self.assertEqual(args[-1], "600", "owned sandbox must expire if the harness disappears")
            self.assertEqual(sum(arg.startswith("type=bind") for arg in args), 2)
            self.assertTrue(all("readonly" in arg for arg in args if arg.startswith("type=bind")))
            with self.assertRaises(ValueError):
                probe.container_arguments("smoke-abc", "general:latest", binary, module)
            with self.assertRaises(ValueError):
                probe.container_arguments("../foreign", "sha256:" + "a" * 64, binary, module)

    def test_missing_or_failed_observations_are_never_invented(self):
        rows = [dict(schema="workspace.perf.v1", source="sandbox", phase="workspaceProtection",
                     operationId="attempt-1", metric=metric, value=value, outcome="error")
                for metric, value in (("scannedEntries", 4), ("sourceReadBytes", 120))]
        result = probe.read_observations("\n".join(map(json.dumps, rows)), "attempt-1", 1)
        self.assertEqual(result, rows)
        with self.assertRaises(ValueError):
            probe.read_observations("", "attempt-1", 0)
        with self.assertRaises(ValueError):
            probe.read_observations(json.dumps(rows[0]), "attempt-1", 1)
        with self.assertRaises(ValueError):
            probe.read_observations("\n".join(map(json.dumps, rows)), "wrong-attempt", 1)

    def test_failed_capture_must_not_be_offered_to_restore(self):
        self.assertFalse(probe.publishable_capture(1, None))
        self.assertFalse(probe.publishable_capture(0, None))
        self.assertTrue(probe.publishable_capture(0, {"frameSha256": "a" * 64}))

    def test_kernel_limits_must_match_the_spec_not_only_docker_metadata(self):
        values = {"cpu.max": "100000 100000", "memory.max": "1073741824", "pids.max": "128"}
        probe.validate_kernel_limits(values)
        for field in values:
            with self.assertRaises(ValueError):
                probe.validate_kernel_limits({**values, field: "max"})


if __name__ == "__main__":
    unittest.main()
