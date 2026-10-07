"""Compare rendered deployment identity with the frozen Workspace baseline."""
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]


def identity(config):
    keys = ("container_name", "volumes", "environment", "entrypoint", "command", "ports", "depends_on", "restart")
    return {"name": config["name"], "volumes": config["volumes"],
            "services": {name: {key: service[key] for key in keys if key in service}
                         for name, service in config["services"].items()}}


def rendered_config():
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    environment = {**os.environ, "CENTAERIS_SOURCE_REVISION": revision}
    example_keys = [line.split("=", 1)[0] for line in (ROOT / ".env.example").read_text().splitlines()
                    if "=" in line and not line.startswith("#")]
    for key in example_keys:
        environment.pop(key, None)
    return json.loads(subprocess.check_output(["docker", "compose", "--env-file", ".env.example", "-f",
                                              "docker-compose.yml", "config", "--format", "json"],
                                             cwd=ROOT, env=environment, text=True, encoding="utf-8"))


class DeploymentMigrationTests(unittest.TestCase):
    def test_service_volume_mount_and_runtime_settings_match_the_source_stack(self):
        baseline = json.loads((ROOT / "tests/workspace/fixtures/compose_identity.json").read_text(encoding="utf-8"))
        self.assertEqual(identity(rendered_config()), baseline)

    def test_each_product_dockerfile_selects_its_inputs(self):
        for name in ("api", "worker", "runtime_server", "web", "hosted_execution"):
            filename = "Dockerfile.agent" if name == "hosted_execution" else "Dockerfile"
            self.assertNotIn("COPY . .", (ROOT / "packages" / name / filename).read_text(encoding="utf-8"), name)

    def test_runtime_image_retains_compiled_hosted_command_contracts(self):
        dockerfile = (ROOT / "packages/runtime_server/Dockerfile").read_text(encoding="utf-8")
        build = dockerfile.split("\nFROM ", 1)[0]
        copied = []
        for line in build.splitlines():
            if line.startswith("COPY "):
                arguments = shlex.split(line)[1:]
                copied.extend((Path(source), Path(arguments[-1])) for source in arguments[:-1])
        contracts = ROOT / "packages/api/app_core/contracts"
        references = set()
        for file in (ROOT / "packages/runtime_server/src").rglob("*.rs"):
            for relative in re.findall(r'include_str!\(\s*"([^"]+)"', file.read_text(encoding="utf-8")):
                resolved = (file.parent / relative).resolve()
                if resolved.is_relative_to(contracts):
                    references.add(resolved.relative_to(ROOT))
        self.assertTrue(references, "Hosted command contracts must be discovered")
        for reference in references:
            with self.subTest(contract=reference):
                self.assertTrue(any(reference == source == destination or
                                    reference.is_relative_to(source) and source == destination
                                    for source, destination in copied),
                                f"Runtime image is missing compiled contract: {reference}")

    def test_rust_image_inputs_form_valid_locked_workspaces(self):
        tracked = subprocess.check_output(["git", "ls-files"], cwd=ROOT, text=True).splitlines()
        for name in ("packages/runtime_server/Dockerfile", "packages/hosted_execution/Dockerfile.agent"):
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
                        for tracked_path in map(Path, tracked):
                            if tracked_path == source:
                                target = destination / source.name if arguments[-1].endswith("/") else destination
                            elif tracked_path.is_relative_to(source):
                                target = destination / tracked_path.relative_to(source)
                            else:
                                continue
                            (stage / target).parent.mkdir(parents=True, exist_ok=True)
                            shutil.copyfile(ROOT / tracked_path, stage / target)
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
