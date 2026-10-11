"""Check license scope against parsed manifests, dependency graphs and crate payloads."""

import json
from pathlib import Path
import subprocess
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ProductLicensingTests(unittest.TestCase):
    def test_foundation_licenses_and_independent_package_payloads(self):
        licensing = (ROOT / "LICENSING.md").read_text(encoding="utf-8")
        apache_paths = {
            line.rstrip("/") for line in licensing.split("```")[1].splitlines()
            if line.startswith("packages/")
        }
        self.assertEqual(len(apache_paths), 4)
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"],
            cwd=ROOT, text=True, encoding="utf-8",
        ))
        packages = {p["name"]: p for p in metadata["packages"]}
        apache_text = (ROOT / "LICENSE-APACHE").read_bytes()
        for package in packages.values():
            directory = Path(package["manifest_path"]).parent
            member = directory.relative_to(ROOT).as_posix()
            expected = "Apache-2.0" if member in apache_paths else "AGPL-3.0-only"
            self.assertEqual(package["license"], expected, member)
            if member not in apache_paths:
                continue
            self.assertEqual((directory / "LICENSE").read_bytes(), apache_text, member)
            for dependency in package["dependencies"]:
                if dependency.get("path"):
                    self.assertEqual(packages[dependency["name"]]["license"], "Apache-2.0",
                                     f"{member} depends on {dependency['name']}")
            files = subprocess.check_output(
                ["cargo", "package", "--locked", "--offline", "--allow-dirty", "--list",
                 "-p", package["name"]], cwd=ROOT, text=True, encoding="utf-8",
            ).replace("\\", "/").splitlines()
            self.assertIn("LICENSE", files, member)
            self.assertIn("README.md", files, member)
            if package["name"] == "centaeris-model-catalog":
                self.assertIn("centaeris_model_catalog/logos/LICENSE", files)

    def test_product_defaults_remain_agpl(self):
        cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        self.assertEqual(cargo["workspace"]["package"]["license"], "AGPL-3.0-only")
        for manifest in [ROOT / "package.json", *(ROOT / "packages").glob("*/package.json")]:
            self.assertEqual(json.loads(manifest.read_text(encoding="utf-8"))["license"],
                             "AGPL-3.0-only", str(manifest))
        for manifest in [ROOT / "pyproject.toml", *(ROOT / "packages").glob("*/pyproject.toml")]:
            self.assertEqual(tomllib.loads(manifest.read_text(encoding="utf-8"))["project"]["license"],
                             "AGPL-3.0-only", str(manifest))


if __name__ == "__main__":
    unittest.main()
