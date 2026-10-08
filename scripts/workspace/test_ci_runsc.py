"""CI must provision the default OCI runtime before the full deployment gate."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class CiRunscTests(unittest.TestCase):
    def test_rust_builds_exclusively_share_the_registry_cache(self):
        for file in ("packages/runtime_server/Dockerfile", "packages/hosted_execution/Dockerfile.agent"):
            with self.subTest(file=file):
                command = next(line for line in (ROOT / file).read_text().splitlines()
                               if line.startswith("RUN ") and "cargo build" in line)
                mount = next(word for word in command.split() if word.startswith("--mount="))
                options = dict(part.split("=", 1) for part in mount.removeprefix("--mount=").split(","))
                self.assertEqual(options["target"], "/usr/local/cargo/registry")
                self.assertEqual(options.get("sharing"), "locked")

    def test_deployment_builds_one_image_at_a_time_and_stops_on_failure(self):
        source = (ROOT / "scripts/workspace/docker-release-gate.sh").read_text()
        build = source.split('"${compose[@]}" config --format json | python3 scripts/workspace/verify-deployment-images.py', 1)[0].rsplit("\n'\n\n", 1)[1]
        bash = str(Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/bin/bash.exe") if os.name == "nt" else "bash"
        services = ["document-processor", "workspace-general", "runtime", "api", "worker", "web"]
        for fail in ("", "worker"):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as temporary:
                script = '''set -euo pipefail
record() {
    printf '%s\\n' "$*" >> calls.txt
    if [[ "$*" == "build --pull $FAIL_SERVICE" ]]; then return 23; fi
}
compose=(record)
''' + build
                result = subprocess.run([bash, "-c", script], cwd=temporary,
                                        env={**os.environ, "FAIL_SERVICE": fail}, capture_output=True, text=True)
                expected = services[:5] if fail else services
                self.assertEqual((Path(temporary) / "calls.txt").read_text().splitlines(),
                                 ["build --pull " + service for service in expected])
                self.assertEqual(result.returncode, 23 if fail else 0, result.stderr)

    def test_fresh_start_provisions_and_executes_runsc_before_deployment(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        job = workflow.split("  docker-fresh-start:\n", 1)[1]
        commands = [
            "sha256sum --check",
            "sudo tar --zstd",
            "sudo /usr/local/bin/runsc install -- --platform=systrap",
            "sudo systemctl reload docker",
            "docker run --rm --runtime=runsc",
            "run: ./scripts/workspace/docker-release-gate.sh",
        ]
        for command in commands:
            self.assertIn(command, job)
        offsets = [job.index(command) for command in commands]
        self.assertEqual(offsets, sorted(offsets))
        self.assertIn("https://github.com/google/gvisor/releases/download/release-20260921.0/", job)
        self.assertIn("e3e7776afd36b08431c668a60f4271f9e9787ce0def960be680acb1681af3861", job)


if __name__ == "__main__":
    unittest.main()
