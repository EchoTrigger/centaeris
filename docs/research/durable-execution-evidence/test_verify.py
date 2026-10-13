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
