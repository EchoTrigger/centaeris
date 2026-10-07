"""CI must provision the default OCI runtime before the full deployment gate."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]


class CiRunscTests(unittest.TestCase):
    def test_fresh_start_provisions_and_executes_runsc_before_deployment(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        job = workflow.split("  docker-fresh-start:\n", 1)[1]
        commands = [
            "sha256sum --check",
            "sudo tar --zstd",
            "sudo /usr/local/bin/runsc install -- --platform=systrap",
            "sudo systemctl reload docker",
            "docker run --rm --runtime=runsc",
            "run: ./scripts/docker-release-gate.sh",
        ]
        for command in commands:
            self.assertIn(command, job)
        offsets = [job.index(command) for command in commands]
        self.assertEqual(offsets, sorted(offsets))
        self.assertIn("https://github.com/google/gvisor/releases/download/release-20260921.0/", job)
        self.assertIn("e3e7776afd36b08431c668a60f4271f9e9787ce0def960be680acb1681af3861", job)


if __name__ == "__main__":
    unittest.main()
