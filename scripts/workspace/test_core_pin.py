import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("core_pin", Path(__file__).with_name("core-pin.py"))
pin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pin)


class CorePinTests(unittest.TestCase):
    def test_checked_in_sources_share_one_pin(self):
        pin.validate()

    def test_drift_is_rejected_and_sync_updates_only_derived_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("core-revision.txt", "Cargo.toml", "Cargo.lock", ".env.example"):
                (root / name).write_bytes((pin.ROOT / name).read_bytes())
            previous = pin.validate(root)
            new = "b" * 40
            (root / "core-revision.txt").write_text(new + "\n")
            with self.assertRaises(ValueError):
                pin.validate(root)
            original_lock = (root / "Cargo.lock").read_bytes()
            pin.sync(root)
            self.assertEqual((root / "Cargo.lock").read_bytes(), original_lock)
            with self.assertRaises(ValueError):
                pin.validate(root)
            (root / "Cargo.lock").write_text(original_lock.decode().replace(previous, new), encoding="utf-8")
            self.assertEqual(pin.validate(root), new)
            (root / "Cargo.toml").write_text((root / "Cargo.toml").read_text().replace('rev =', 'branch ='), encoding="utf-8")
            with self.assertRaises(ValueError):
                pin.validate(root)


if __name__ == "__main__":
    unittest.main()
