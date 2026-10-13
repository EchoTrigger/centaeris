"""Behavioral checks for the offline evidence verifier, using temporary copies."""
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest

from verify import verify


class EvidenceVerifierTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="centaeris-evidence-check-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "bundle"
        shutil.copytree(Path(__file__).resolve().parent, self.root,
                        ignore=shutil.ignore_patterns("__pycache__"))
        self.reseal()

    def reseal(self):
        provenance = self.root / "provenance.json"
        data = json.loads(provenance.read_text(encoding="utf-8"))
        for entry in data["files"]:
            entry["exportSha256"] = hashlib.sha256((self.root / entry["path"]).read_bytes()).hexdigest()
        provenance.write_text(json.dumps(data), encoding="utf-8")
        inventory = self.root / "inventory.json"
        data = {"files": []}
        for path in sorted(self.root.rglob("*")):
            if path.is_file() and path != inventory:
                payload = path.read_bytes()
                data["files"].append({"path": path.relative_to(self.root).as_posix(),
                                      "sha256": hashlib.sha256(payload).hexdigest(), "sizeBytes": len(payload)})
        inventory.write_text(json.dumps(data), encoding="utf-8")

    def replace_json(self, name, data):
        (self.root / name).write_text(json.dumps(data), encoding="utf-8")
        self.reseal()

    def test_fd_checks_require_exact_fields_and_boolean_true(self):
        name = "data/runsc-process/guarded/restored-fd-verification.json"
        correct = json.loads((self.root / name).read_text())
        for value in ({}, {key: "true" for key in correct}, {**correct, "pipeVerified": 1}):
            with self.subTest(value=value):
                self.replace_json(name, value)
                with self.assertRaisesRegex(ValueError, "descriptor"):
                    verify(self.root)

    def test_restored_process_must_make_progress(self):
        name = "data/runsc-process/guarded/oracle-resumed.json"
        before = json.loads((self.root / "data/runsc-process/guarded/oracle-before.json").read_text())
        self.replace_json(name, before)
        with self.assertRaisesRegex(ValueError, "progress"):
            verify(self.root)

    def test_oracles_require_paused_state_and_exact_types(self):
        prefix = "data/runsc-process/guarded/"
        before = json.loads((self.root / (prefix + "oracle-before.json")).read_text())
        for changed in ({**before, "paused": False}, {**before, "counter": True}):
            with self.subTest(changed=changed):
                self.replace_json(prefix + "oracle-before.json", changed)
                self.replace_json(prefix + "oracle-after.json", changed)
                with self.assertRaisesRegex(ValueError, "oracle"):
                    verify(self.root)

    def test_supported_outcome_cannot_hide_cleanup_failure(self):
        name = "data/runsc-process/guarded/outcome.json"
        data = json.loads((self.root / name).read_text())
        data["cleanup"][0]["exitCode"] = 1
        self.replace_json(name, data)
        with self.assertRaisesRegex(ValueError, "cleanup"):
            verify(self.root)

    def test_restore_cannot_precede_source_deletion(self):
        name = "data/runsc-process/guarded/source-exit-evidence.json"
        data = json.loads((self.root / name).read_text())
        next(step for step in data["steps"] if step["operation"] == "restore")["commandIndex"] = 0
        self.replace_json(name, data)
        with self.assertRaisesRegex(ValueError, "source exit"):
            verify(self.root)

    def test_unmeasured_capability_cannot_be_promoted(self):
        name = "data/runsc-process/guarded/capability-matrix.json"
        data = json.loads((self.root / name).read_text())
        data["crossHostRecovery"]["status"] = "supported"
        self.replace_json(name, data)
        with self.assertRaisesRegex(ValueError, "capability"):
            verify(self.root)

    def test_inventory_rejects_duplicate_entries_and_wrong_size(self):
        name = self.root / "inventory.json"
        original = json.loads(name.read_text())
        for data in ({**original, "files": original["files"] + original["files"][:1]},
                     {**original, "files": [{**original["files"][0], "sizeBytes": 0}] + original["files"][1:]}):
            with self.subTest(first=data["files"][0]):
                name.write_text(json.dumps(data))
                with self.assertRaisesRegex(ValueError, "inventory"):
                    verify(self.root)

    def change_observation(self, *, remove=False, update_export_hash=False):
        relative = "data/conditional-snapshot/observations.jsonl"
        path = self.root / relative
        lines = path.read_text(encoding="utf-8").splitlines()
        if remove:
            lines.pop(0)
        else:
            row = json.loads(lines[1])
            row["value"] += 1
            lines[1] = json.dumps(row)
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        if update_export_hash:
            provenance = self.root / "provenance.json"
            data = json.loads(provenance.read_text(encoding="utf-8"))
            entry = next(item for item in data["files"] if item["path"] == relative)
            entry["exportSha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
            provenance.write_text(json.dumps(data), encoding="utf-8")

    def test_complete_bundle_passes(self):
        verify(self.root)

    def test_modified_observation_fails_integrity(self):
        self.change_observation()
        with self.assertRaisesRegex(ValueError, "export digest mismatch"):
            verify(self.root)

    def test_forged_export_hash_does_not_hide_wrong_byte_count(self):
        self.change_observation(update_export_hash=True)
        with self.assertRaisesRegex(ValueError, "two-pass byte mismatch"):
            verify(self.root)

    def test_missing_counter_is_not_silently_treated_as_zero(self):
        self.change_observation(remove=True, update_export_hash=True)
        with self.assertRaisesRegex(ValueError, "missing observation"):
            verify(self.root)


if __name__ == "__main__":
    unittest.main()
