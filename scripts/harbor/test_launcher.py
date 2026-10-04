"""Windows detached-launch acceptance with fake Harbor and Docker commands."""
import json
import ctypes
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest
import uuid


@unittest.skipUnless(os.name == "nt" and shutil.which("pwsh"), "requires Windows PowerShell 7")
class LauncherTests(unittest.TestCase):
    def test_parent_exits_while_worker_runs_and_resumes_existing_job(self):
        with tempfile.TemporaryDirectory(prefix="centaeris-launch-") as directory:
            import winreg
            root = Path(directory)
            scripts = root / "scripts/harbor"
            scripts.mkdir(parents=True)
            shutil.copyfile(Path(__file__).with_name("run.ps1"), scripts / "run.ps1")
            for name in ("five-tasks.json", "full-tasks.json"):
                shutil.copyfile(Path(__file__).with_name(name), scripts / name)
            key_name = "CENTAERIS_TEST_KEY_" + uuid.uuid4().hex
            settings = root / "local settings/settings.json"
            settings.parent.mkdir()
            settings.write_text(json.dumps({"providerId": "custom.test", "model": "mock",
                "credentialEnv": key_name, "reasoningEffort": "max",
                "contextTokens": 500000, "maxOutputTokens": 64000}))
            binary = root / "target/harbor-bookworm/debug/centaeris-runtime"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"\x7fELFfake")
            job = root / "jobs/centaeris-tb21-five-tasks-pass5"
            job.mkdir(parents=True)
            (job / "config.json").write_text("{}")
            commands = root / "commands"
            commands.mkdir()
            (commands / "docker.ps1").write_text("'linux'\nexit 0\n")
            (commands / "harbor.ps1").write_text("""
@{ arguments = @($args); keyInherited = ([Environment]::GetEnvironmentVariable($env:CENTAERIS_BENCH_CREDENTIAL_ENV, 'Process') -eq 'fake-test-key') } |
    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $env:LAUNCH_TEST_ROOT 'invoked.json')
(@{ arguments = @($args) } | ConvertTo-Json -Compress) |
    Add-Content -LiteralPath (Join-Path $env:LAUNCH_TEST_ROOT 'invocations.jsonl')
$deadline = (Get-Date).AddSeconds(20)
while (-not (Test-Path -LiteralPath (Join-Path $env:LAUNCH_TEST_ROOT 'release'))) {
    if ((Get-Date) -gt $deadline) { exit 8 }
    Start-Sleep -Milliseconds 50
}
exit 0
""")
            env = {**os.environ, "PATH": str(commands) + os.pathsep + os.environ["PATH"],
                   "CENTAERIS_RUNTIME_BINARY": str(binary),
                   "LAUNCH_TEST_ROOT": str(root)}
            control = root / "jobs/control/centaeris-tb21-five-tasks-pass5"
            try:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_SET_VALUE) as registry:
                    winreg.SetValueEx(registry, key_name, 0, winreg.REG_SZ, "fake-test-key")
                with (root / "parent.stdout").open("wb") as out, (root / "parent.stderr").open("wb") as err:
                    parent = subprocess.run([shutil.which("pwsh"), "-NoProfile", "-File",
                                             str(scripts / "run.ps1"), "-FollowWithFull",
                                             "-SettingsPath", str(settings)], env=env, stdout=out,
                                            stderr=err, timeout=15)
                self.assertEqual(parent.returncode, 0, (root / "parent.stderr").read_text())
                self.assertIn("This terminal can close", (root / "parent.stdout").read_text())
                deadline = time.monotonic() + 12
                while not (root / "invoked.json").exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                invoked = json.loads((root / "invoked.json").read_text(encoding="utf-8-sig"))
                self.assertEqual(invoked["arguments"][:2], ["jobs", "resume"])
                self.assertTrue(invoked["keyInherited"])
                self.assertFalse((control / "finished.json").exists())
                (root / "release").touch()
                deadline = time.monotonic() + 12
                while not (control / "phase.json").exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                phase = json.loads((control / "phase.json").read_text(encoding="utf-8-sig"))
                self.assertEqual(phase["stage"], "awaitingReview")
                self.assertFalse((control / "finished.json").exists())
                (control / "review.json").write_text('{"decision":"runFull"}')
            finally:
                with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_SET_VALUE) as registry:
                    winreg.DeleteValue(registry, key_name)
                (root / "release").touch()
                if control.exists() and not (control / "review.json").exists():
                    (control / "review.json").write_text('{"decision":"stop"}')
                deadline = time.monotonic() + 12
                while not (control / "finished.json").exists() and time.monotonic() < deadline:
                    time.sleep(0.05)
                if (control / "worker.json").exists():
                    record = json.loads((control / "worker.json").read_text(encoding="utf-8-sig"))
                    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
                    kernel.OpenProcess.restype = ctypes.c_void_p
                    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
                    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
                    handle = kernel.OpenProcess(0x100000, False, record["processId"])
                    if handle:
                        try:
                            self.assertEqual(kernel.WaitForSingleObject(handle, 10000), 0)
                        finally:
                            kernel.CloseHandle(handle)
            finished = json.loads((control / "finished.json").read_text(encoding="utf-8-sig"))
            self.assertEqual(finished["exitCode"], 0)
            calls = [json.loads(line) for line in (root / "invocations.jsonl").read_text(encoding="utf-8-sig").splitlines()]
            self.assertEqual(calls[1]["arguments"], ["run", "-c", str(settings.parent / "resolved-full-tasks.json")])
            resolved = json.loads((settings.parent / "resolved-full-tasks.json").read_text(encoding="utf-8-sig"))
            self.assertEqual(resolved["agents"][0]["kwargs"]["credential_env"], key_name)
            self.assertNotIn("fake-test-key", json.dumps(resolved))


if __name__ == "__main__":
    unittest.main()
