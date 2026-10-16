"""Tampering counterexamples, including recomputed manifests; no services needed."""
import csv
import io
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from verify import LIMIT, read, sha, verify


ROOT = Path(__file__).resolve().parent


class EvidenceVerifierTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="retained-evidence-test-", dir=ROOT.parent)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "bundle"
        shutil.copytree(ROOT, self.root)

    def write(self, path, value):
        (self.root/path).write_text(json.dumps(value, indent=2)+"\n", encoding="utf-8")

    def reseal(self):
        """Simulate an attacker updating both export hashes and inventory."""
        provenance = read(self.root, "provenance.json")
        for r in provenance["files"]:
            data = (self.root/r["exportPath"]).read_bytes()
            r.update(exportBytes=len(data), exportSha256=sha(data))
            if not r["transforms"]:
                r.update(originalBytes=len(data), originalSha256=sha(data))
        self.write("provenance.json", provenance)
        inventory = read(self.root, "inventory.json")
        for r in inventory["files"]:
            data = (self.root/r["path"]).read_bytes()
            r.update(bytes=len(data), sha256=sha(data))
        self.write("inventory.json", inventory)

    def test_complete_bundle_is_consistent_with_open_conditions(self):
        result = verify(self.root)
        self.assertEqual((result["uses"], result["commands"]), (100, 1000))
        self.assertEqual(result["strictMemoryGate"], "incomplete-or-failed")
        self.assertEqual(result["pendingFaultScenarios"], 6)

    def test_content_tampering_without_new_hash_rejects(self):
        p = self.root/"data/long-run-1000-final/side-effects.txt"
        p.write_bytes(p.read_bytes()+b"1000\n")
        with self.assertRaisesRegex(ValueError, "file size/hash"):
            verify(self.root)

    def test_missing_file_rejects(self):
        (self.root/"tables/lifecycle_runs.csv").unlink()
        with self.assertRaisesRegex(ValueError, "missing or unregistered"):
            verify(self.root)

    def test_unregistered_file_rejects(self):
        (self.root/"unregistered.txt").write_text("extra")
        with self.assertRaisesRegex(ValueError, "missing or unregistered"):
            verify(self.root)

    def test_nested_inventory_file_is_not_self_excluded(self):
        folder = self.root/"unregistered"
        folder.mkdir()
        (folder/"inventory.json").write_text("{}")
        with self.assertRaisesRegex(ValueError, "missing or unregistered"):
            verify(self.root)

    def test_duplicate_inventory_entry_rejects(self):
        d = read(self.root, "inventory.json")
        d["files"].append(d["files"][0])
        self.write("inventory.json", d)
        with self.assertRaisesRegex(ValueError, "duplicate inventory"):
            verify(self.root)

    def test_historical_oom_cannot_be_relabeled_pass_after_rehash(self):
        d = read(self.root, "bundle.json")
        next(r for r in d["experiments"] if r["id"] == "run-708-c0349f0-oom")["classification"] = "passed"
        self.write("bundle.json", d); self.reseal()
        with self.assertRaisesRegex(ValueError, "historical failure relabeled"):
            verify(self.root)

    def test_stopped_run_cannot_be_relabeled_pass_after_rehash(self):
        d = read(self.root, "bundle.json")
        next(r for r in d["experiments"] if r["id"].endswith("-stopped"))["classification"] = "passed"
        self.write("bundle.json", d); self.reseal()
        with self.assertRaisesRegex(ValueError, "stopped run relabeled"):
            verify(self.root)

    def test_same_runtime_pid_and_start_time_reject_after_rehash(self):
        base = "data/ab-and-100-uses/"
        a, b = read(self.root, base+"observed-a.json"), read(self.root, base+"observed-b.json")
        b["runtimeStartup"] = a["runtimeStartup"]
        self.write(base+"observed-b.json", b); self.reseal()
        with self.assertRaisesRegex(ValueError, "actual A/B Runtime restart"):
            verify(self.root)

    def test_hot_record_65_rejects_even_with_forged_source_hash(self):
        path = "data/ab-and-100-uses/observed-use-0.json"
        d = read(self.root, path); d["store"]["hotTasks"] = 65
        self.write(path, d); self.reseal()
        with self.assertRaisesRegex(ValueError, "hot record limit"):
            verify(self.root)

    def test_csv_value_and_recomputed_hash_still_need_source_agreement(self):
        p = self.root/"tables/lifecycle_runs.csv"
        rows = list(csv.DictReader(io.StringIO(p.read_text())))
        rows[0]["hotRecords"] = "65"
        stream = io.StringIO(newline="")
        writer = csv.DictWriter(stream, fieldnames=list(rows[0])); writer.writeheader(); writer.writerows(rows)
        p.write_bytes(stream.getvalue().encode()); self.reseal()
        with self.assertRaisesRegex(ValueError, "derived table mismatch"):
            verify(self.root)

    def test_sample_above_512_mib_rejects_after_rehash(self):
        p = self.root/"data/long-run-1000-final/runtime-memory.jsonl"
        rows = p.read_text().splitlines(); d = json.loads(rows[0]); d["memory"]["current"] = LIMIT+1
        rows[0] = json.dumps(d); p.write_text("\n".join(rows)+"\n"); self.reseal()
        with self.assertRaisesRegex(ValueError, "sample above Runtime"):
            verify(self.root)

    def test_unknown_retirement_cannot_release_holding(self):
        path = "data/ab-and-100-uses/maintenance-observations.json"
        d = read(self.root, path); d[0]["observation"].update(status="unknown", residentHoldingCount=0)
        self.write(path, d); self.reseal()
        with self.assertRaisesRegex(ValueError, "destruction accounting"):
            verify(self.root)

    def test_present_container_cannot_be_reported_destroyed(self):
        base = "data/ab-and-100-uses/"
        d = read(self.root, base+"container-inventories.json")
        d[0]["containerIds"].append(read(self.root, base+"observed-a.json")["containerId"])
        self.write(base+"container-inventories.json", d); self.reseal()
        with self.assertRaisesRegex(ValueError, "destroyed container still present"):
            verify(self.root)

    def test_live_run_requires_one_resident_holding(self):
        path = "data/ab-and-100-uses/resident-observations.json"
        d = read(self.root, path); d[0]["observation"]["residentCount"] = 0
        self.write(path, d); self.reseal()
        with self.assertRaisesRegex(ValueError, "one resident holding"):
            verify(self.root)

    def test_different_summary_after_restart_rejects(self):
        path = "data/long-run-1000-final/long-runtime-restart.json"
        d = read(self.root, path); d["compactionRecovery"]["summaryMessageId"] = "replacement-summary"
        self.write(path, d); self.reseal()
        with self.assertRaisesRegex(ValueError, "original summary reuse"):
            verify(self.root)

    def test_missing_required_pg_identity_rejects(self):
        path = "validation/pg-identities.json"
        d = read(self.root, path); missing = d["required"][0]
        d["selected"].remove(missing); d["executed"].remove(missing)
        self.write(path, d); self.reseal()
        with self.assertRaisesRegex(ValueError, "required PG identities"):
            verify(self.root)

    def test_missing_required_configuration_field_rejects(self):
        d = read(self.root, "bundle.json"); del d["runtimeMemoryLimitBytes"]
        self.write("bundle.json", d); self.reseal()
        with self.assertRaises(KeyError):
            verify(self.root)

    def test_ci_success_requires_job_evidence(self):
        path = "validation/ci-status.json"
        d = read(self.root, path)
        d.update(status="completed", conclusion="success", jobs=[])
        self.write(path, d); self.reseal()
        with self.assertRaisesRegex(ValueError, "CI result completeness"):
            verify(self.root)


if __name__ == "__main__":
    unittest.main(verbosity=2)
