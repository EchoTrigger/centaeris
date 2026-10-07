"""Verify first-party product metadata and lockfile parity."""
import ast
import json
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]

class ProductVersionTests(unittest.TestCase):
    def test_advertised_service_versions_match_product(self):
        for path, factory in [("packages/api/api/ninja_api.py", "NinjaAPI"),
                              ("packages/api/app_core/platform_mcp.py", "Server")]:
            tree = ast.parse((ROOT / path).read_text(encoding="utf-8"))
            versions = [keyword.value.value for call in ast.walk(tree)
                        if isinstance(call, ast.Call) and isinstance(call.func, ast.Name)
                        and call.func.id == factory for keyword in call.keywords
                        if keyword.arg == "version" and isinstance(keyword.value, ast.Constant)]
            self.assertEqual(versions, ["0.1.0"], path)

    def test_metadata_and_lockfiles_agree(self):
        version = "0.1.0"
        rust = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
        self.assertEqual(rust["workspace"]["package"]["version"], version)
        names = set()
        for member in rust["workspace"]["members"]:
            package = tomllib.loads((ROOT / member / "Cargo.toml").read_text(encoding="utf-8"))["package"]
            names.add(package["name"])
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
        for package in lock["package"]:
            if package["name"] in names or package["name"].startswith("centaeris-"):
                self.assertEqual(package["version"], version, package["name"])
        root = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
        projects = [root] + [tomllib.loads((ROOT / member / "pyproject.toml").read_text(encoding="utf-8")) for member in root["tool"]["uv"]["workspace"]["members"]]
        python_names = {project["project"]["name"] for project in projects}
        for project in projects:
            self.assertEqual(project["project"]["version"], version)
        uv = tomllib.loads((ROOT / "uv.lock").read_text(encoding="utf-8"))
        for package in uv["package"]:
            if package["name"] in python_names:
                self.assertEqual(package["version"], version, package["name"])
        npm = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))
        lock = json.loads((ROOT / "package-lock.json").read_text(encoding="utf-8"))
        self.assertEqual(lock["version"], version)
        for member in ["", *npm["workspaces"]]:
            self.assertEqual(json.loads((ROOT / member / "package.json").read_text(encoding="utf-8"))["version"], version)
            self.assertEqual(lock["packages"][member]["version"], version)

if __name__ == "__main__":
    unittest.main()
