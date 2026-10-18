"""CI must provision the default OCI runtime before the full deployment gate."""
from pathlib import Path
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER_BYTES = b"gate fixture from the newly built image\x00\xff"
MOCK_DOCKER = r'''
import hashlib, json, os, pathlib, sys, tempfile
args = sys.argv[1:]
if args[0] == "mktemp":
    if "-d" in args:
        path = tempfile.mkdtemp(prefix="owned-gate-", dir=os.environ["GATE_FIXTURE_BASE"])
    else:
        handle, path = tempfile.mkstemp(prefix="owned-gate-", dir=os.environ["GATE_FIXTURE_BASE"])
        os.close(handle)
    print(pathlib.Path(path).as_posix())
    raise SystemExit
event = {"args": args}
def record():
    with open(os.environ["GATE_CALLS"], "a") as log:
        log.write(json.dumps(event) + "\n")
if args[0] == "compose":
    env_path = pathlib.Path(args[args.index("--env-file") + 1])
    values = dict(line.split("=", 1) for line in env_path.read_text().splitlines() if "=" in line and not line.startswith("#"))
    helper = pathlib.Path(values.get("ENVIRONMENT_EXECUTION_HELPER", ""))
    event.update(helper=str(helper), digest=values.get("ENVIRONMENT_EXECUTION_HELPER_SHA256"), socket=values.get("ENVIRONMENT_HOST_SOCKET_DIRECTORY"))
    args = args[args.index("--env-file") + 2:]
    event["action"] = args[0]
    record()
    if args[0] == "config":
        if not helper.is_absolute() or not event["digest"]:
            raise SystemExit(78)
        if "--format" in args:
            names = ["api", "document-processor", "runtime", "web", "worker", "workspace-general"]
            services = {name: {"build": {"context": os.environ["GATE_WORKSPACE_ROOT"]}} for name in names}
            services["workspace-general"]["image"] = "built-workspace-general"
            services["runtime"]["depends_on"] = {"workspace-general": {}}
            services["material-worker"] = {"depends_on": {"document-processor": {}}}
            volumes = {name: {"name": "centaeris-workspace_" + name} for name in ["agent-memory", "plugin-data", "postgres-data", "runtime-data", "storage-data", "upload-temp"]}
            print(json.dumps({"name": "centaeris-workspace", "services": services, "volumes": volumes}))
    elif args[0] == "images":
        print("built-" + args[-1])
    elif args[0] == "up":
        assert helper.is_file(), "startup must use the extracted helper"
        assert hashlib.sha256(helper.read_bytes()).hexdigest() == event["digest"]
        assert pathlib.Path(event["socket"]).is_dir()
        if os.environ["GATE_FAIL"] == "startup":
            raise SystemExit(42)
    elif args[0] == "ps" and "--status" in args:
        print("postgres\nredis\napi\nruntime\nworker\nmaterial-worker\nweb")
else:
    event["action"] = args[0]
    record()
    if args[0] == "info":
        print('{"runsc": {}}')
    elif args[0] == "create":
        assert args[-1] == "built-workspace-general"
        pathlib.Path(args[args.index("--cidfile") + 1]).write_text("owned-helper-container")
        print("owned-helper-container")
    elif args[0] == "cp":
        assert args[1] == "owned-helper-container:/opt/centaeris/bin/execution_agent"
        if os.environ["GATE_FAIL"] == "cp":
            raise SystemExit(41)
        pathlib.Path(args[2]).write_bytes(bytes.fromhex(os.environ["GATE_HELPER_HEX"]))
    elif args[:2] == ["image", "inspect"]:
        print("AGPL-3.0-only" if "licenses" in args[args.index("--format") + 1] else "fixture-revision")
'''


class CiRunscTests(unittest.TestCase):
    def _run_release_gate(self, fail=""):
        bash = str(Path(os.environ.get("ProgramFiles", "C:/Program Files")) / "Git/bin/bash.exe") if os.name == "nt" else "bash"
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            stub = base / "docker.py"
            stub.write_text(MOCK_DOCKER)
            calls = base / "calls.jsonl"
            sentinel = base / "keep.txt"
            sentinel.write_text("unrelated")
            environment = base / "bash-env"
            environment.write_text('''python3() {
    if [[ "${1:-}" == "scripts/workspace/verify-deployment-images.py" ]]; then cat >/dev/null; return; fi
    "$GATE_PYTHON" "$@"
}
docker() { python3 "$GATE_DOCKER" "$@"; }
mktemp() { python3 "$GATE_DOCKER" mktemp "$@"; }
node() { printf '%s\\n' fixture-revision; }
''')
            workspace = subprocess.run([bash, "-c", "pwd"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
            result = subprocess.run([bash, "scripts/workspace/docker-release-gate.sh"], cwd=ROOT,
                                    env={**os.environ, "BASH_ENV": environment.as_posix(),
                                         "CENTAERIS_CI_DESTRUCTIVE_DOCKER": "1", "GATE_FAIL": fail,
                                         "GATE_PYTHON": Path(sys.executable).as_posix(), "GATE_DOCKER": stub.as_posix(),
                                         "GATE_FIXTURE_BASE": base.as_posix(), "GATE_CALLS": calls.as_posix(),
                                         "GATE_WORKSPACE_ROOT": workspace, "GATE_HELPER_HEX": HELPER_BYTES.hex()},
                                    capture_output=True, text=True)
            events = [json.loads(line) for line in calls.read_text().splitlines()]
            self.assertEqual(sentinel.read_text(), "unrelated")
            for event in events:
                if event.get("helper") and Path(event["helper"]).is_relative_to(base):
                    self.assertFalse(Path(event["helper"]).parent.exists(), "owned fixture directory must be removed")
            return result, events

    def test_release_gate_extracts_and_binds_built_helper_before_startup(self):
        result, events = self._run_release_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        actions = [event["action"] for event in events]
        self.assertLess(actions.index("config"), actions.index("build"))
        self.assertLess(max(i for i, action in enumerate(actions) if action == "build"), actions.index("create"))
        self.assertLess(actions.index("cp"), actions.index("up"))
        self.assertTrue(all(event["digest"] == "0" * 64 for event in events if event["action"] in ("config", "build")))
        self.assertTrue(all(event["digest"] == hashlib.sha256(HELPER_BYTES).hexdigest() for event in events if event["action"] == "up"))
        self.assertEqual([event["args"] for event in events if event["action"] == "rm"], [["rm", "-f", "owned-helper-container"]])

    def test_release_gate_cleans_only_owned_helper_fixture_on_failure(self):
        for fail, status in (("cp", 41), ("startup", 42)):
            with self.subTest(fail=fail):
                result, events = self._run_release_gate(fail)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertEqual([event["args"] for event in events if event["action"] == "rm"], [["rm", "-f", "owned-helper-container"]])
                self.assertEqual(sum(event["action"] == "down" for event in events), 1)
                if fail == "cp":
                    self.assertNotIn("up", [event["action"] for event in events])

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
