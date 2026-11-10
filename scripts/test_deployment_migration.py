"""Validate container build inputs and checked-out source provenance."""
import importlib.util
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


def rendered_config():
    spec = importlib.util.spec_from_file_location(
        "deployment_contract", ROOT / "scripts/workspace/deployment-contract.test.py",
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.compose_config()


class DeploymentMigrationTests(unittest.TestCase):
    def test_runtime_image_retains_compiled_application_inputs(self):
        dockerfile = (ROOT / "packages/workspace/runtime/Dockerfile").read_text(encoding="utf-8")
        build = dockerfile.split("\nFROM ", 1)[0]
        copied = []
        for line in build.splitlines():
            if line.startswith("COPY "):
                arguments = shlex.split(line)[1:]
                copied.extend((Path(source), Path(arguments[-1])) for source in arguments[:-1])
        application_inputs = ROOT / "packages/workspace/api/app_core"
        references = set()
        for file in (ROOT / "packages/workspace/runtime/src").rglob("*.rs"):
            for relative in re.findall(r'include_(?:str|bytes)!\(\s*"([^"]+)"', file.read_text(encoding="utf-8")):
                resolved = (file.parent / relative).resolve()
                if resolved.is_relative_to(application_inputs):
                    references.add(resolved.relative_to(ROOT))
        self.assertTrue(references, "Compiled application inputs must be discovered")
        for reference in references:
            with self.subTest(contract=reference):
                self.assertTrue((ROOT / reference).is_file(), str(reference))
                self.assertTrue(any(reference == source == destination or
                                    reference.is_relative_to(source) and source == destination
                                    for source, destination in copied),
                                f"Runtime image is missing compiled contract: {reference}")

    def test_rust_image_inputs_form_valid_locked_workspaces(self):
        paths = subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=ROOT, text=True, encoding="utf-8",
        ).split("\0")
        source_files = [Path(path) for path in paths if path and (ROOT / path).is_file()]
        for name in ("packages/workspace/runtime/Dockerfile", "packages/workspace/executor/Dockerfile.agent"):
            with self.subTest(dockerfile=name), tempfile.TemporaryDirectory(prefix="docker-cargo-") as temporary:
                stage = Path(temporary)
                build = (ROOT / name).read_text(encoding="utf-8").split("\nFROM ", 1)[0]
                for line in build.splitlines():
                    if not line.startswith("COPY "):
                        continue
                    arguments = shlex.split(line)[1:]
                    for source in arguments[:-1]:
                        source = Path(source)
                        destination = Path(arguments[-1])
                        for source_path in source_files:
                            if source_path == source:
                                target = destination / source.name if arguments[-1].endswith("/") else destination
                            elif source_path.is_relative_to(source):
                                target = destination / source_path.relative_to(source)
                            else:
                                continue
                            (stage / target).parent.mkdir(parents=True, exist_ok=True)
                            shutil.copyfile(ROOT / source_path, stage / target)
                result = subprocess.run(["cargo", "metadata", "--offline", "--locked", "--no-deps",
                                         "--format-version", "1", "--manifest-path", str(stage / "Cargo.toml")],
                                        cwd=stage, capture_output=True, text=True, encoding="utf-8")
                self.assertEqual(result.returncode, 0, result.stderr)
                workspace = tomllib.loads((stage / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]
                for member in workspace["members"]:
                    package = stage / member
                    manifest = tomllib.loads((package / "Cargo.toml").read_text(encoding="utf-8"))
                    for example in manifest.get("example", []):
                        paths = [package / example["path"]] if "path" in example else [
                            package / "examples" / (example["name"] + ".rs"),
                            package / "examples" / example["name"] / "main.rs",
                        ]
                        self.assertTrue(any(path.is_file() for path in paths),
                                        f"Cargo cannot locate declared example: {member}: {example['name']}")

    def test_image_provenance_uses_one_checked_out_monorepo_revision(self):
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        for name, service in rendered_config()["services"].items():
            if "build" in service:
                labels = service["build"]["labels"]
                self.assertEqual(labels["org.opencontainers.image.revision"], revision, name)
                self.assertEqual(labels["io.centaeris.source.revision"], revision, name)
                self.assertNotIn("io.centaeris.core.revision", labels)


if __name__ == "__main__":
    unittest.main()
