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
        self.assertTrue((ROOT / "pnpm-lock.yaml").is_file())
        for manifest in (ROOT / "packages").glob("*/package.json"):
            for name in ("package-lock.json", "pnpm-lock.yaml", "yarn.lock"):
                self.assertFalse((manifest.parent / name).exists(), str(manifest.parent / name))
        self.assertFalse((ROOT / "package-lock.json").exists())

    def test_package_manager_selection_matches_the_root_manifest(self):
        manifest = json.loads((ROOT / "package.json").read_text())
        version = manifest["engines"]["pnpm"]
        self.assertEqual(manifest["packageManager"], f"pnpm@{version}")

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
