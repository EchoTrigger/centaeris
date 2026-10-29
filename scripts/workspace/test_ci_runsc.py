"""CI must provision the default OCI runtime before the full deployment gate."""
from pathlib import Path
import importlib.util
import io
import json
import os
import shlex
import subprocess
import sys
import tempfile
import tarfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SUPERVISOR_SOURCE = r"""#!/usr/bin/env python3
import json, os, pathlib, signal, socket, sys, time
config = json.loads(pathlib.Path(sys.argv[1]).read_text())
root = pathlib.Path(sys.argv[1]).parent
mode = os.environ.get("GATE_FAIL", "")
if mode == "supervisor-ignore-stop":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
if mode == "supervisor-exit":
    print("controlled supervisor startup failure", flush=True)
    raise SystemExit(37)
if mode == "supervisor-timeout":
    time.sleep(30)
server = socket.socket(socket.AF_UNIX)
server.bind(config["socketPath"])
server.listen()
pathlib.Path(os.environ["GATE_SUPERVISOR_STARTED"]).write_text(json.dumps({"pid": os.getpid(), "root": str(root), "config": config}))
while True:
    connection, _ = server.accept()
    connection.close()
"""
HELPER_BYTES = b"gate fixture from the newly built image\x00\xff"
MOCK_DOCKER = r'''
import json, os, pathlib, socket, subprocess, sys, tempfile
args = sys.argv[1:]
if args[0] == "mktemp":
    if "-d" in args:
        path = tempfile.mkdtemp(prefix="owned-gate-", dir=os.environ["GATE_FIXTURE_BASE"])
    else:
        handle, path = tempfile.mkstemp(prefix="owned-gate-", dir=os.environ["GATE_FIXTURE_BASE"])
        os.close(handle)
    print(pathlib.Path(path).as_posix())
    raise SystemExit
if args[0] == "sudo":
    import importlib.util
    argv = args[args.index("--supervisor"):]
    spec = importlib.util.spec_from_file_location("host", pathlib.Path(os.environ["GATE_WORKSPACE_PATH"]) / "scripts/workspace/docker-fresh-start-host.py")
    host = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(host)
    original = subprocess.Popen
    if os.name == "nt":
        host.subprocess.Popen = lambda command, **kwargs: original([sys.executable, *command], **kwargs)
    host.run_host(pathlib.Path(argv[argv.index("--supervisor") + 1]), pathlib.Path(argv[argv.index("--helper") + 1]),
                  {"runtimeRoot": "/run/docker/runtime-runc/moby", "controllerUid": 0},
                  parent=pathlib.Path(os.environ["GATE_FIXTURE_BASE"]), startup_timeout=.4, stop_timeout=.4)
    raise SystemExit
event = {"args": args}
def record():
    with open(os.environ["GATE_CALLS"], "a") as log:
        log.write(json.dumps(event) + "\n")
if args[0] == "compose":
    env_path = pathlib.Path(args[args.index("--env-file") + 1])
    values = dict(line.split("=", 1) for line in env_path.read_text().splitlines() if "=" in line and not line.startswith("#"))
    helper = pathlib.Path(values.get("ENVIRONMENT_EXECUTION_HELPER", ""))
    event.update(helper=str(helper), socket=values.get("ENVIRONMENT_HOST_SOCKET_DIRECTORY"))
    args = args[args.index("--env-file") + 2:]
    event["action"] = args[0]
    record()
    if args[0] == "config":
        if not helper.is_absolute():
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
        assert helper.read_bytes() == bytes.fromhex(os.environ["GATE_HELPER_HEX"]), "startup must use the helper from the built image"
        assert pathlib.Path(event["socket"]).is_dir()
        assert values["RUNTIME_NATIVE_SHARED_ADMISSION"] == "1", "startup must preserve the shared admission deployment default"
        assert pathlib.Path(os.environ["GATE_SUPERVISOR_STARTED"]).is_file(), "startup requires a live host supervisor"
        socket_directory = pathlib.Path(event["socket"])
        assert socket_directory.stat().st_mode & 0o777 == 0o700
        assert helper.stat().st_mode & 0o777 == 0o500
        with socket.socket(socket.AF_UNIX) as connection:
            connection.connect(str(socket_directory / "control.sock"))
        if os.environ["GATE_FAIL"] == "startup":
            raise SystemExit(42)
    elif args[0] == "build" and args[-1] == os.environ["GATE_FAIL"]:
        raise SystemExit(23)
    elif args[0] == "ps" and "--status" in args:
        print("postgres\nredis\napi\nruntime\nworker\nmaterial-worker\nweb")
else:
    event["action"] = args[0]
    record()
    if args[0] == "info":
        if "{{.Driver}}" in args:
            print("overlay2")
        else:
            print("fixture-daemon" if "{{.ID}}" in args else '{"runsc": {}}')
    elif args[0] == "create":
        name = "owned-helper-container" if args[-1] == "built-workspace-general" else "owned-supervisor-container"
        pathlib.Path(args[args.index("--cidfile") + 1]).write_text(name)
        print(name)
    elif args[0] == "cp":
        if args[1] == "owned-helper-container:/opt/centaeris/bin/execution_agent":
            if os.environ["GATE_FAIL"] == "cp":
                raise SystemExit(41)
            pathlib.Path(args[2]).write_bytes(bytes.fromhex(os.environ["GATE_HELPER_HEX"]))
        else:
            assert args[1] == "owned-supervisor-container:/src/target/release/environment_host_supervisor"
            pathlib.Path(args[2]).write_text(os.environ["GATE_SUPERVISOR_SOURCE"])
    elif args[:2] == ["image", "inspect"]:
        print("AGPL-3.0-only" if "licenses" in args[args.index("--format") + 1] else "fixture-revision")
'''


@unittest.skipIf(os.name == "nt", "native host supervisor lifecycle requires Linux Unix sockets")
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
sudo() { python3 "$GATE_DOCKER" sudo "$@"; }
mktemp() { python3 "$GATE_DOCKER" mktemp "$@"; }
node() { printf '%s\\n' fixture-revision; }
''')
            workspace = subprocess.run([bash, "-c", "pwd"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
            result = subprocess.run([bash, "scripts/workspace/docker-release-gate.sh"], cwd=ROOT,
                                    env={**os.environ, "BASH_ENV": environment.as_posix(),
                                         "CENTAERIS_CI_DESTRUCTIVE_DOCKER": "1", "GATE_FAIL": fail,
                                         "GATE_PYTHON": Path(sys.executable).as_posix(), "GATE_DOCKER": stub.as_posix(),
                                         "GATE_FIXTURE_BASE": base.as_posix(), "GATE_CALLS": calls.as_posix(),
                                         "GATE_WORKSPACE_ROOT": workspace, "GATE_WORKSPACE_PATH": ROOT.as_posix(),
                                         "GATE_SUPERVISOR_SOURCE": SUPERVISOR_SOURCE, "GATE_HELPER_HEX": HELPER_BYTES.hex(),
                                         "GATE_SUPERVISOR_STARTED": (base / "supervisor.started").as_posix()},
                                    capture_output=True, text=True)
            events = [json.loads(line) for line in calls.read_text().splitlines()]
            self.assertEqual(sentinel.read_text(), "unrelated")
            started = base / "supervisor.started"
            if started.exists():
                host = json.loads(started.read_text())
                self.assertFalse(Path(host["root"]).exists(), "root host fixture must be removed after its child stops")
                if sys.platform == "linux":
                    self.assertFalse(Path(f"/proc/{host['pid']}").exists(), "owned supervisor must be reaped")
            self.assertFalse(any(path.is_dir() for path in base.iterdir()), "all owned fixture directories must be removed")
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
        started = next(event for event in events if event["action"] == "up")
        self.assertIn("material-worker", started["args"])
        self.assertTrue(any(event["action"] == "ps" and "--status" in event["args"] for event in events))
        self.assertEqual([event["args"] for event in events if event["action"] == "rm"], [["rm", "-f", "owned-helper-container"], ["rm", "-f", "owned-supervisor-container"]])

    def test_release_gate_cleans_only_owned_helper_fixture_on_failure(self):
        for fail, status in (("cp", 41), ("startup", 42)):
            with self.subTest(fail=fail):
                result, events = self._run_release_gate(fail)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertEqual([event["args"] for event in events if event["action"] == "rm"], [["rm", "-f", "owned-helper-container"], ["rm", "-f", "owned-supervisor-container"]])
                self.assertEqual(sum(event["action"] == "down" for event in events), 1)
                if fail == "cp":
                    self.assertNotIn("up", [event["action"] for event in events])

    def test_deployment_builds_one_image_at_a_time_and_stops_on_failure(self):
        for fail in ("", "worker"):
            with self.subTest(fail=fail):
                result, events = self._run_release_gate(fail)
                builds = [event for event in events if event["action"] == "build" and event["args"][0] == "compose"]
                built = [event["args"][-1] for event in builds]
                self.assertTrue(built)
                self.assertEqual(len(built), len(set(built)))
                for event in builds:
                    args = event["args"]
                    self.assertEqual(args[args.index("--parallel") + 1], "1")
                if fail:
                    self.assertEqual(result.returncode, 23, result.stderr)
                    self.assertEqual(built[-1], fail)
                    self.assertFalse(any(event["action"] == "up" for event in events))
                    self.assertEqual(sum(event["action"] == "down" for event in events), 1)
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertTrue(any(event["action"] == "up" for event in events))

    def test_release_gate_kills_and_reaps_unresponsive_supervisor(self):
        result, events = self._run_release_gate("supervisor-ignore-stop")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sum(event["action"] == "down" for event in events), 1)

    def test_release_gate_reaps_supervisor_on_failed_readiness(self):
        for fail in ("supervisor-exit", "supervisor-timeout"):
            with self.subTest(fail=fail):
                result, events = self._run_release_gate(fail)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("owned host supervisor", result.stderr)
                if fail == "supervisor-exit":
                    self.assertIn("controlled supervisor startup failure", result.stderr)
                self.assertFalse(any(event["action"] == "up" for event in events))
                self.assertEqual(sum(event["action"] == "down" for event in events), 1)

class HostConfigurationTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("host", ROOT / "scripts/workspace/docker-fresh-start-host.py")
        self.host = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.host)

    @unittest.skipIf(os.name == "nt", "native runner installation requires Unix file permissions")
    def test_ci_runsc_install_has_trusted_ancestry_even_with_writable_node_bin(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        extraction = next(shlex.split(line.strip())[1:] for line in workflow.splitlines()
                          if line.strip().startswith("sudo tar "))
        variables = {"runsc_directory": next((line.strip().partition("=")[2].strip('"')
                                             for line in workflow.splitlines()
                                             if line.strip().startswith("runsc_directory=")), "")}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            node_bin = root / "usr/local/bin"
            node_bin.mkdir(parents=True)
            node_bin.chmod(0o777)
            archive = root / "runtime.tar"
            with tarfile.open(archive, "w") as bundle:
                for name in ("runsc", "gvisor-bin/runsc-sandbox"):
                    member = tarfile.TarInfo(name)
                    member.mode = 0o755
                    member.size = 6
                    bundle.addfile(member, io.BytesIO(b"binary"))
            # Compression is orthogonal to filesystem ownership and ancestry.
            # Execute the workflow's real tar extraction against an uncompressed fixture.
            arguments = [str(archive) if item == "$archive" else
                         str(root / variables[item[1:]].lstrip("/")) if item.startswith("$") else
                         str(root / item.lstrip("/")) if item.startswith("/usr/") else item
                         for item in extraction if item != "--zstd"]
            destination = Path(arguments[arguments.index("--directory") + 1])
            destination.mkdir(parents=True, exist_ok=True)
            subprocess.run(arguments, check=True, capture_output=True)
            for name in ("runsc", "gvisor-bin/runsc-sandbox"):
                executable = destination / name
                self.assertEqual(executable.read_bytes(), b"binary")
                for path in (executable, *executable.parents):
                    if path == root.parent:
                        break
                    metadata = path.lstat()
                    # The workflow runs extraction as root; the fixture uses its test actor.
                    self.assertEqual(metadata.st_uid, os.geteuid(), str(path))
                    self.assertEqual(metadata.st_mode & 0o022, 0, f"untrusted installed ancestry: {path}")
            self.assertEqual(node_bin.stat().st_mode & 0o777, 0o777,
                             "install must preserve the runner Node directory")

    def test_daemon_flags_preserve_paths_after_boolean_options(self):
        self.assertEqual(self.host.daemon_flags([
            "--debug", "--exec-root", "/run/owned-docker", "--experimental=true",
            "--data-root=/srv/owned-docker", "--containerd-namespace", "owned"]),
            {"exec-root": "/run/owned-docker", "data-root": "/srv/owned-docker", "containerd-namespace": "owned"})
        with self.assertRaisesRegex(RuntimeError, "value unavailable"):
            self.host.daemon_flags(["--exec-root", "--debug"])

    @unittest.skipIf(os.name == "nt", "Linux daemon path configuration")
    def test_supervisor_matches_local_daemon_and_its_explicit_runtime_root(self):
        info = {"ID": "owned", "DockerRootDir": "/srv/docker", "Runtimes": {"runsc": {"path": "/usr/bin/runsc"}}}
        def read_text(path, *args, **kwargs):
            return "123" if str(path) == "/run/docker.pid" else "{}"
        with patch.object(self.host.shutil, "which", return_value="/usr/bin/docker"), \
             patch.object(self.host.subprocess, "check_output", side_effect=lambda *args, **kwargs: json.dumps(info)), \
             patch.object(Path, "read_text", read_text), \
             patch.object(Path, "read_bytes", return_value=b"/usr/bin/dockerd\0--debug\0--data-root=/srv/docker\0--exec-root\0/run/owned\0--containerd-namespace=gate\0"), \
             patch.object(Path, "resolve", lambda path, **kwargs: path):
            config = self.host.daemon_configuration("owned")
            self.assertEqual(config["runtimeRoot"], "/run/owned/runtime-runc/gate")
            self.assertEqual(config["runtimePath"], "/usr/bin/runsc")
            self.assertEqual(config["controllerUid"], 0)
            with self.assertRaisesRegex(RuntimeError, "same local Docker"):
                self.host.daemon_configuration("unrelated")
            info["DockerRootDir"] = "/srv/unrelated"
            with self.assertRaisesRegex(RuntimeError, "data-root does not match"):
                self.host.daemon_configuration("owned")

if __name__ == "__main__":
    unittest.main()
