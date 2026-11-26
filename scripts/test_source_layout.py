"""Protect product membership, owned dependency sources and relocated script roots."""
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

from markdown_links import markdown_sources, missing_local_links

ROOT = Path(__file__).resolve().parents[1]


class SourceLayoutTests(unittest.TestCase):
    def test_documentation_local_links_exist(self):
        missing = missing_local_links(markdown_sources(ROOT), ROOT)
        self.assertFalse(missing, "Missing local Markdown destinations:\n" + "\n".join(missing))

    def test_markdown_source_scope(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "--quiet", str(root)], check=True)
            (root / ".gitignore").write_text("AGENTS.md\nlocal.md\n", encoding="utf-8")
            for name in ("tracked.md", "docs/new.md", ".github/template.md", "packages/README.md",
                         "docs/workspace/source-snapshot/README.md", "AGENTS.md", "local.md"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("# Fixture\n", encoding="utf-8")
            subprocess.run(["git", "add", "tracked.md"], cwd=root, check=True)
            self.assertEqual([path.relative_to(root).as_posix() for path in markdown_sources(root)], [
                ".github/template.md", "docs/new.md", "tracked.md",
            ])

    def test_markdown_local_link_check(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "nested").mkdir()
            (root / "target file.md").write_text("# Target\n", encoding="utf-8")
            (root / "target(1).md").write_text("# Target\n", encoding="utf-8")
            source = root / "nested" / "source.md"
            source.write_text('''[relative](../target%20file.md?raw=1#heading)
![image](<../target file.md> "Title")
[root](/target%20file.md)
[parentheses](../target(1).md)
[full reference][target]
[target]: ../target%20file.md "Title"
[external](https://example.com/missing.md)
[protocol relative](//example.com/missing.md)
[mail](mailto:example@example.com)
[fragment](#heading)
`[inline code](missing-inline.md)`
``[inline backticks ` code](missing-backticks.md)``
`literal `` [example](missing-reverse-backticks.md) `` literal`
```markdown
[fenced example](missing-fenced.md)
[fenced-reference]: missing-reference.md
```
~~~markdown
[tilde fence](missing-tilde.md)
~~~
''', encoding="utf-8")
            self.assertEqual(missing_local_links([source], root), [])
            with source.open("a", encoding="utf-8") as output:
                output.write("[missing](missing.md)\n[missing-reference]: missing-reference.md\n")
            self.assertEqual(missing_local_links([source], root), [
                "nested/source.md -> missing.md", "nested/source.md -> missing-reference.md",
            ])

    def test_hosted_rust_products_use_this_workspace(self):
        workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
        self.assertTrue({"packages/workspace/runtime", "packages/workspace/executor"}.issubset(workspace["members"]))
        for name, folder in (("centaeris-core", "core"), ("centaeris-model-catalog", "model-catalog"),
                             ("centaeris-mcp", "mcp"), ("centaeris-runtime-sqlite", "sqlite")):
            dependency = workspace["dependencies"][name]
            self.assertEqual(dependency["path"], "packages/" + folder)
            self.assertFalse({"git", "rev", "branch"}.intersection(dependency))

    def test_all_node_products_share_the_root_lock(self):
        self.assertTrue((ROOT / "pnpm-lock.yaml").is_file())
        shell = ROOT / "packages/electron-shell"
        for manifest in [*(ROOT / "packages").glob("*/package.json"), *(ROOT / "packages/workspace").glob("*/package.json")]:
            for name in ("package-lock.json", "pnpm-lock.yaml", "yarn.lock"):
                if manifest.parent == shell and name == "pnpm-lock.yaml":
                    continue
                self.assertFalse((manifest.parent / name).exists(), str(manifest.parent / name))
        self.assertFalse((ROOT / "package-lock.json").exists())
        self.assertTrue((shell / "pnpm-lock.yaml").is_file())
        self.assertTrue((shell / "pnpm-workspace.yaml").is_file())
        self.assertNotIn("packages/electron-shell", (ROOT / "pnpm-workspace.yaml").read_text())
        self.assertNotIn("packages/electron-shell", (ROOT / "pnpm-lock.yaml").read_text())

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
                         ["packages/workspace/api", "packages/workspace/documents", "packages/workspace/executor"])
        manifest = json.loads((ROOT / "docs/workspace/source-snapshot/manifest.json").read_text())
        import hashlib
        for entry in manifest["files"]:
            if "/migrations/" in entry["source"]:
                # The import manifest records the original destination before product regrouping.
                destination = entry["destination"].replace("packages/api/", "packages/workspace/api/", 1)
                self.assertEqual(hashlib.sha256((ROOT / destination).read_bytes()).hexdigest(), entry["sha256"])


if __name__ == "__main__":
    unittest.main()
