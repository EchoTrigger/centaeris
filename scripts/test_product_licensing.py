"""Check license scope against parsed manifests, dependency graphs and crate payloads."""

import argparse
import json
from pathlib import Path
import subprocess
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


def apache_package_paths():
    licensing = (ROOT / "LICENSING.md").read_text(encoding="utf-8")
    return {
        line.rstrip("/") for line in licensing.split("```")[1].splitlines()
        if line.startswith("packages/")
    }


def workspace_packages():
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
    packages = {}
    for member in workspace["members"]:
        directory = ROOT / member
        manifest = tomllib.loads((directory / "Cargo.toml").read_text(encoding="utf-8"))
        license_value = manifest["package"]["license"]
        if license_value == {"workspace": True}:
            license_value = workspace["package"]["license"]
        dependencies = []
        for scope in [manifest, *manifest.get("target", {}).values()]:
            for kind in ("dependencies", "dev-dependencies", "build-dependencies"):
                for name, dependency in scope.get(kind, {}).items():
                    if not isinstance(dependency, dict):
                        continue
                    base = directory
                    if dependency.get("workspace"):
                        dependency = workspace["dependencies"][name]
                        base = ROOT
                    if isinstance(dependency, dict) and "path" in dependency:
                        dependencies.append((base / dependency["path"]).resolve())
        packages[directory.resolve()] = {
            "member": member, "name": manifest["package"]["name"],
            "license": license_value, "dependencies": dependencies,
        }
    return packages


class ProductLicensingTests(unittest.TestCase):
    def test_foundation_licenses_and_dependency_boundaries(self):
        apache_paths = apache_package_paths()
        self.assertEqual(len(apache_paths), 4)
        packages = workspace_packages()
        self.assertTrue(apache_paths <= {p["member"] for p in packages.values()})
        apache_text = (ROOT / "LICENSE-APACHE").read_bytes()
        for directory, package in packages.items():
            member = package["member"]
            expected = "Apache-2.0" if member in apache_paths else "AGPL-3.0-only"
            self.assertEqual(package["license"], expected, member)
            if member not in apache_paths:
                continue
            self.assertEqual((directory / "LICENSE").read_bytes(), apache_text, member)
            for dependency in package["dependencies"]:
                self.assertIn(dependency, packages, f"{member}: unknown path dependency {dependency}")
                self.assertEqual(packages[dependency]["license"], "Apache-2.0",
                                 f"{member} depends on {packages[dependency]['name']}")

    def test_product_defaults_remain_agpl(self):
        cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        self.assertEqual(cargo["workspace"]["package"]["license"], "AGPL-3.0-only")
        for manifest in [ROOT / "package.json", *(ROOT / "packages").glob("*/package.json")]:
            self.assertEqual(json.loads(manifest.read_text(encoding="utf-8"))["license"],
                             "AGPL-3.0-only", str(manifest))
        for manifest in [ROOT / "pyproject.toml", *(ROOT / "packages").glob("*/pyproject.toml")]:
            self.assertEqual(tomllib.loads(manifest.read_text(encoding="utf-8"))["project"]["license"],
                             "AGPL-3.0-only", str(manifest))

class CratePayloadTests(unittest.TestCase):
    def test_independent_crates_retain_license_and_attribution_files(self):
        for package in workspace_packages().values():
            if package["member"] not in apache_package_paths():
                continue
            with self.subTest(package=package["name"]):
                files = subprocess.check_output(
                    ["cargo", "package", "--locked", "--offline", "--allow-dirty", "--list",
                     "-p", package["name"]], cwd=ROOT, text=True, encoding="utf-8",
                ).replace("\\", "/").splitlines()
                self.assertIn("LICENSE", files)
                self.assertIn("README.md", files)
                if package["name"] == "centaeris-model-catalog":
                    self.assertIn("centaeris_model_catalog/logos/LICENSE", files)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--crate-payloads", action="store_true",
                        help="Also inspect crate payloads; requires prepared Cargo dependencies")
    args = parser.parse_args()
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ProductLicensingTests)
    if args.crate_payloads:
        suite.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(CratePayloadTests))
    raise SystemExit(not unittest.TextTestRunner().run(suite).wasSuccessful())
