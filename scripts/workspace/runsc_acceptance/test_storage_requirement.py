"""Run release storage preflight against a fake Docker endpoint only."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(sys.platform == "linux", "release gate runs on Linux")
class StorageRequirementTests(unittest.TestCase):
    def run_gate(self, driver, backing="xfs", dtype="true"):
        with tempfile.TemporaryDirectory(prefix="centaeris-storage-preflight-") as temporary:
            root = Path(temporary)
            calls = root / "calls.jsonl"
            docker = root / "docker"
            docker.write_text("""#!/usr/bin/env python3
import json,os,sys
with open(os.environ['TEST_DOCKER_CALLS'],'a') as output:output.write(json.dumps(sys.argv[1:])+chr(10))
if sys.argv[1:]==['info','--format','{{.Driver}}']:print(os.environ['TEST_STORAGE_DRIVER'])
elif sys.argv[1:]==['info','--format','{{json .DriverStatus}}']:print(json.dumps([['Backing Filesystem',os.environ['TEST_STORAGE_BACKING']],['Supports d_type',os.environ['TEST_STORAGE_DTYPE']]]))
elif sys.argv[1:]==['info','--format','{{json .Runtimes}}']:print('{"runsc":{}}')
elif sys.argv[1:3]==['ps','-aq']:print('owned-project-blocks-any-mutation')
else:sys.exit(97)
""")
            docker.chmod(0o700)
            node = root / "node"
            node.write_text("#!/bin/sh\necho 0000000000000000000000000000000000000000\n")
            node.chmod(0o700)
            environment = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                "CENTAERIS_CI_DESTRUCTIVE_DOCKER": "1", "TEST_STORAGE_DRIVER": driver,
                "TEST_STORAGE_BACKING": backing, "TEST_STORAGE_DTYPE": dtype,
                "TEST_DOCKER_CALLS": str(calls)}
            result = subprocess.run(["bash", str(Path(__file__).parents[1] / "docker-release-gate.sh")],
                env=environment, capture_output=True, text=True, timeout=15)
            recorded = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            return result, recorded

    def test_non_overlayfs_storage_fails_before_fixture_access(self):
        for driver in ["vfs", "", "btrfs", "unknown", "overlayfs"]:
            with self.subTest(driver=driver):
                result, calls = self.run_gate(driver)
                self.assertEqual(result.returncode, 69, result.stderr)
                self.assertEqual(calls, [["info", "--format", "{{.Driver}}"]])

    def test_classic_overlay2_on_xfs_reaches_existing_project_safety_check(self):
        result, calls = self.run_gate("overlay2")
        self.assertEqual(result.returncode, 65, result.stderr)
        self.assertTrue(any(call[:2] == ["ps", "-aq"] for call in calls))

    def test_wrong_backing_or_missing_dtype_fails_before_fixture_access(self):
        for backing, dtype in [("extfs", "true"), ("xfs", "false")]:
            with self.subTest(backing=backing, dtype=dtype):
                result, calls = self.run_gate("overlay2", backing, dtype)
                self.assertEqual(result.returncode, 69, result.stderr)
                self.assertFalse(any(call[:2] == ["ps", "-aq"] for call in calls))


if __name__ == "__main__":
    unittest.main()
