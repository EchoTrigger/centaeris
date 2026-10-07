import contextlib
import io
from pathlib import Path
import runpy
import subprocess
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("deployment-api-smoke.py")


class DeploymentSmokeTests(unittest.TestCase):
    def test_build_failure_survives_cleanup_failures_and_all_cleanup_is_attempted(self):
        build_error = subprocess.CalledProcessError(17, ["docker", "compose", "build"])
        cleanup_error = subprocess.CalledProcessError(18, ["docker", "compose", "down"])
        with patch("subprocess.run", side_effect=[build_error, cleanup_error, cleanup_error]) as run:
            with contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(subprocess.CalledProcessError) as raised:
                    runpy.run_path(str(SCRIPT), run_name="__main__")
        self.assertIs(raised.exception, build_error)
        self.assertEqual(run.call_count, 3)
        self.assertIn("down", run.call_args_list[1].args[0])
        self.assertEqual(run.call_args_list[2].args[0][:3], ["docker", "image", "rm"])

    def test_cleanup_failure_after_success_still_fails_the_smoke(self):
        cleanup_error = subprocess.CalledProcessError(18, ["docker", "compose", "down"])
        with patch("subprocess.run", side_effect=[None] * 5 + [cleanup_error, None]) as run:
            with contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(subprocess.CalledProcessError) as raised:
                    runpy.run_path(str(SCRIPT), run_name="__main__")
        self.assertIs(raised.exception, cleanup_error)
        self.assertEqual(run.call_count, 7)


if __name__ == "__main__":
    unittest.main()
