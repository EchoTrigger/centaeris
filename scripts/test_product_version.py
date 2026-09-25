"""Keep first-party release metadata and locked package versions in sync."""

import json
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ProductVersionTests(unittest.TestCase):
    def test_product_metadata_matches_rust_workspace(self):
        cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        version = cargo["workspace"]["package"]["version"]
        self.assertEqual(version, "0.1.0")
        self.assertEqual(cargo["workspace"]["dependencies"]["centaeris-model-catalog"]["version"], f"={version}")
        first_party = set()
        for member in cargo["workspace"]["members"]:
            package = tomllib.loads((ROOT / member / "Cargo.toml").read_text(encoding="utf-8"))["package"]
            self.assertEqual(package["version"], {"workspace": True})
            first_party.add(package["name"])
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
        locked = {p["name"]: p["version"] for p in lock["package"] if p["name"] in first_party}
        self.assertEqual(locked, dict.fromkeys(first_party, version))
        npm = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
        npm_lock = json.loads((ROOT / "package-lock.json").read_text(encoding="utf-8"))
        self.assertEqual(npm_lock["version"], version)
        for directory in ["", *npm["workspaces"], "packages/tui"]:
            package = json.loads((ROOT / directory / "package.json").read_text(encoding="utf-8"))
            self.assertEqual(package["version"], version, directory)
            if directory in npm_lock["packages"]:
                self.assertEqual(npm_lock["packages"][directory]["version"], version, directory)


if __name__ == "__main__":
    unittest.main()
