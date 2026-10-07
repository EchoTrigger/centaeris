"""Protect product membership, owned dependency sources and relocated script roots."""
import importlib.util
import json
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


class SourceLayoutTests(unittest.TestCase):
    def test_hosted_rust_products_use_this_workspace(self):
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
        self.assertTrue({"packages/runtime_server", "packages/hosted_execution"}.issubset(workspace["members"]))
        for name, folder in (("centaeris-core", "core"), ("centaeris-model-catalog", "model-catalog"),
                             ("centaeris-mcp", "mcp"), ("centaeris-runtime-sqlite", "runtime_sqlite")):
            dependency = workspace["dependencies"][name]
            self.assertEqual(dependency["path"], "packages/" + folder)
            self.assertFalse({"git", "rev", "branch"}.intersection(dependency))

    def test_all_node_products_share_the_root_lock(self):
        package = json.loads((ROOT / "package.json").read_text())
        self.assertEqual(set(package["workspaces"]), {"packages/ui", "packages/desktop", "packages/web"})
        lock = json.loads((ROOT / "package-lock.json").read_text())
        for path in package["workspaces"]:
            self.assertIn(path, lock["packages"])

    def test_relocated_python_tools_find_the_product_root(self):
        for name in ("ci.py", "test_product_version.py", "transcript-schema.py", "agent-run-authorization-gate.py"):
            with self.subTest(tool=name):
                spec = importlib.util.spec_from_file_location("layout_" + name, ROOT / "scripts/workspace" / name)
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                self.assertEqual(module.ROOT, ROOT)

    def test_core_packages_are_not_remote_git_dependencies(self):
        lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
        for package in lock["package"]:
            self.assertNotIn("git+https://github.com/EchoTrigger/centaeris", package.get("source", ""))

    def test_hosted_rust_test_fixtures_resolve_after_relocation(self):
        import re
        for file in (ROOT / "packages/runtime_server/src").rglob("*.rs"):
            for relative in re.findall(r'include_str!\(\s*"([^"]*tests/[^\"]+)"', file.read_text(encoding="utf-8")):
                self.assertTrue((file.parent / relative).is_file(), f"Missing fixture: {file}: {relative}")

    def test_python_workspace_and_database_migrations_are_retained(self):
        project = tomllib.loads((ROOT / "pyproject.toml").read_text())
        self.assertEqual(project["tool"]["uv"]["workspace"]["members"],
                         ["packages/api", "packages/document_processor", "packages/hosted_execution", "packages/worker"])
        manifest = json.loads((ROOT / "docs/workspace/source-snapshot/manifest.json").read_text())
        import hashlib
        for entry in manifest["files"]:
            if "/migrations/" in entry["source"]:
                self.assertEqual(hashlib.sha256((ROOT / entry["destination"]).read_bytes()).hexdigest(), entry["sha256"])


if __name__ == "__main__":
    unittest.main()
