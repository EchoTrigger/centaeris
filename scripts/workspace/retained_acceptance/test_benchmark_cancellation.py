"""Opt-in Docker process/verification handoff; never submits a benchmark task."""
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
import uuid


@unittest.skipUnless(sys.platform == "linux" and os.environ.get("CENTAERIS_BENCHMARK_CANCELLATION_TEST") == "1",
                     "requires explicit isolated Linux Docker acceptance")
class BenchmarkCancellationDockerTests(unittest.TestCase):
    def test_quiescence_stops_escaped_commands_preserves_verifier_and_another_container(self):
        self.exercise_quiescence("0")

    def test_non_root_keeper_can_be_quiesced_without_changing_task_user(self):
        self.exercise_quiescence("65534")

    def exercise_quiescence(self, user):
        image = os.environ["CENTAERIS_BENCHMARK_CANCELLATION_IMAGE"]
        helper = Path(os.environ["CENTAERIS_BENCHMARK_CANCELLATION_HELPER"])
        self.assertRegex(image, r"^sha256:[0-9a-f]{64}$")
        self.assertTrue(helper.is_absolute() and helper.is_file() and not helper.is_symlink())
        scope = "benchmark-cancel-" + uuid.uuid4().hex
        remote = "/opt/centaeris/benchmark/execution_agent"
        containers = []
        def docker(*args):
            return subprocess.check_output(["docker", *args], text=True, timeout=30).strip()
        try:
            for suffix in ["target", "unrelated"]:
                container = docker("run", "--detach", "--name", scope + "-" + suffix,
                    "--label", "centaeris.test.benchmarkCancellation=" + scope,
                    "--network", "none", "--memory", "128m", "--pids-limit", "64", "--user", user,
                    "--mount", f"type=bind,source={helper},target={remote},readonly",
                    "--entrypoint", remote, image, "benchmark-keeper")
                self.assertRegex(container, r"^[0-9a-f]{64}$")
                containers.append(container)
            target, unrelated = containers
            prepare = '''import os,sys,subprocess,time,signal
from pathlib import Path
Path('/tmp/retained-output').write_text('original task output')
normal=subprocess.Popen([sys.executable,'-c','import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(60)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
Path('/tmp/normal-pid').write_text(str(normal.pid))
child=os.fork()
if child==0:
 os.setsid()
 if os.fork(): os._exit(0)
 signal.signal(signal.SIGTERM,signal.SIG_IGN)
 for fd in [0,1,2]:
  with open('/dev/null','r+b') as dev:os.dup2(dev.fileno(),fd)
 Path('/tmp/escaped-pid').write_text(str(os.getpid()))
 time.sleep(60)
 os._exit(0)
os.waitpid(child,0)
deadline=time.monotonic()+3
while not Path('/tmp/escaped-pid').exists():
 assert time.monotonic()<deadline
 time.sleep(.01)
'''
            docker("exec", target, "python3", "-c", prepare)
            expected = {"schema": "runtime.benchmark.quiescence.v1", "quiescent": True}
            # A lost acknowledgement can be reconciled before verifier admission.
            for _ in range(2):
                self.assertEqual(json.loads(docker("exec", "--user", user, target, remote, "benchmark-quiesce")), expected)
            self.assertTrue(json.loads(docker("inspect", target))[0]["State"]["Running"])
            verify = '''from pathlib import Path
assert Path('/tmp/retained-output').read_text()=='original task output'
for f in ['normal-pid','escaped-pid']:
 p=Path('/proc')/Path('/tmp',f).read_text()
 assert not p.exists() or '\\nState:\\tZ' in (p/'status').read_text()
Path('/tmp/tests').mkdir()
Path('/tmp/tests/verifier-result').write_text('verified original files')
print('verifier completed on original container')
'''
            self.assertEqual(docker("exec", target, "python3", "-c", verify), "verifier completed on original container")
            self.assertTrue(json.loads(docker("inspect", unrelated))[0]["State"]["Running"])
            self.assertEqual(docker("exec", unrelated, "python3", "-c", "print('unrelated alive')"), "unrelated alive")
        finally:
            for container in containers:
                facts = json.loads(docker("inspect", container))[0]
                self.assertEqual(facts["Config"]["Labels"]["centaeris.test.benchmarkCancellation"], scope)
                docker("rm", "--force", container)
                result = subprocess.run(["docker", "inspect", container], capture_output=True)
                self.assertNotEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
