"""The Docker sandbox exports the same static helper contract as native runsc gates."""
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
DOCKERFILE = ROOT / "packages/hosted_execution/Dockerfile.agent"
STATIC_BYTES = b"\x7fELF controlled static musl fixture"
TOOL = r'''
import json, os, pathlib, sys
name, *args = sys.argv[1:]
environment = {key: value for key, value in os.environ.items() if key in ("RUSTFLAGS", "CC") or key.startswith("CARGO_TARGET_") and key.endswith("_LINKER")}
with open(os.environ["FIXTURE_CALLS"], "a") as output:
    output.write(json.dumps({"tool": name, "args": args, "env": environment}) + "\n")
target = pathlib.Path(os.environ["CARGO_TARGET_DIR"])
if name == "rustc":
    print("rustc fixture\nhost: " + os.environ["FIXTURE_HOST"])
elif name == "cargo":
    if args[0] == "rustc":
        binary = target / args[args.index("--target") + 1] / "release/execution_agent"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(bytes.fromhex(os.environ["FIXTURE_STATIC_HEX"]))
    else:
        for name in ("execution_agent", "environment_host_supervisor"):
            binary = target / "release" / name
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_bytes(("gnu-" + name).encode())
elif name == "readelf":
    mode = os.environ["FIXTURE_ELF"]
    if mode == "malformed":
        raise SystemExit(1)
    print({"-h": "  Type: " + ("DYN" if mode == "pie" else "EXEC"),
           "-l": "  INTERP /lib/ld-musl.so.1" if mode == "interpreter" else "  LOAD",
           "-d": "  (NEEDED) libc.so" if mode == "needed" else "There is no dynamic section."}[args[0]])
'''


def instructions():
    return DOCKERFILE.read_text().replace("\\\n", " ").splitlines()


class StaticDockerHelperTests(unittest.TestCase):
    def run_builder(self, root, host="x86_64-unknown-linux-gnu", elf="static"):
        target = root / "target"
        tool = root / "tool.py"
        tool.write_text(TOOL)
        calls = root / "calls.jsonl"
        environment = root / "bash-env"
        environment.write_text('''python3() { "$FIXTURE_PYTHON" "$@"; }
cargo() { python3 "$FIXTURE_TOOL" cargo "$@"; }
rustc() { python3 "$FIXTURE_TOOL" rustc "$@"; }
rustup() { python3 "$FIXTURE_TOOL" rustup "$@"; }
readelf() { python3 "$FIXTURE_TOOL" readelf "$@"; }
''')
        build = next(line for line in instructions() if line.startswith("RUN ") and "cargo build" in line)
        command = build.partition(" ")[2]
        while command.startswith("--"):
            command = command.partition(" ")[2]
        bash = shutil.which("bash") or "C:/Program Files/Git/bin/bash.exe"
        result = subprocess.run([bash, "-c", command], cwd=ROOT, capture_output=True, text=True,
                                env={**os.environ, "BASH_ENV": environment.as_posix(),
                                     "CARGO_TARGET_DIR": target.as_posix(), "FIXTURE_HOST": host,
                                     "FIXTURE_ELF": elf, "FIXTURE_PYTHON": Path(sys.executable).as_posix(),
                                     "FIXTURE_TOOL": tool.as_posix(), "FIXTURE_CALLS": calls.as_posix(),
                                     "FIXTURE_STATIC_HEX": STATIC_BYTES.hex()})
        events = [json.loads(line) for line in calls.read_text().splitlines()]
        copy = next(shlex.split(line) for line in instructions()
                    if line.startswith("COPY --from=build ") and line.endswith(" /opt/centaeris/bin/execution_agent"))
        artifact = target / PurePosixPath(copy[-2]).relative_to("/src/target")
        return result, events, artifact

    def test_docker_exports_static_helper_without_changing_gnu_supervisor(self):
        for architecture in ("x86_64", "aarch64"):
            with self.subTest(architecture=architecture), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                result, events, artifact = self.run_builder(root, architecture + "-unknown-linux-gnu")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(artifact.read_bytes(), STATIC_BYTES)
                self.assertEqual((root / "target/release/environment_host_supervisor").read_bytes(), b"gnu-environment_host_supervisor")
                helper = next(event for event in events if event["tool"] == "cargo" and event["args"][0] == "rustc")
                target = architecture + "-unknown-linux-musl"
                self.assertEqual(helper["args"][helper["args"].index("--target") + 1], target)
                self.assertEqual(helper["env"]["CC"], "musl-gcc")
                self.assertEqual(helper["env"]["CARGO_TARGET_" + target.upper().replace("-", "_") + "_LINKER"], "musl-gcc")
                gnu = next(event for event in events if event["tool"] == "cargo" and event["args"][0] == "build")
                self.assertEqual(gnu["env"].get("RUSTFLAGS"), os.environ.get("RUSTFLAGS"))
                self.assertTrue(any(event["tool"] == "readelf" for event in events))

    def test_dynamic_or_invalid_helper_is_rejected_before_export(self):
        for elf in ("interpreter", "needed", "pie", "malformed"):
            with self.subTest(elf=elf), tempfile.TemporaryDirectory() as temporary:
                result, _, artifact = self.run_builder(Path(temporary), elf=elf)
                self.assertNotEqual(result.returncode, 0, elf)
                self.assertFalse(artifact.exists(), "unverified helper must not reach the final image COPY")

    def test_docker_helper_cargo_contract_matches_existing_runsc_acceptance_builder(self):
        spec = importlib.util.spec_from_file_location("native_build", ROOT / "scripts/workspace/runsc_acceptance/build.py")
        build = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(build)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            result, events, _ = self.run_builder(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            helper = next(event for event in events if event["tool"] == "cargo" and event["args"][0] == "rustc")
            target = root / "target"
            for name in ("execution_agent", "environment_host_supervisor", "runtime_server"):
                path = target / "debug" / name
                path.parent.mkdir(exist_ok=True)
                path.write_bytes(name.encode())
            argv = ["build", "--source", str(ROOT), "--target-dir", str(target), "--output", str(root / "manifest.json"),
                    "--runsc-rotation", "--artifact-directory", str(root / "bins")]
            with patch.object(sys, "argv", argv), patch.object(sys, "platform", "linux"), \
                 patch.object(build, "command", side_effect=["a" * 40, "b" * 40, "", "ELF without interpreter", "a" * 40, "", "rustc fixture", "cargo fixture"]), \
                 patch.object(build.subprocess, "run") as commands, patch("builtins.print"):
                build.main()
            acceptance = commands.call_args_list[1]
            expected = list(acceptance.args[0][1:])
            expected[expected.index("--target-dir") + 1] = target.as_posix()
            self.assertEqual(helper["args"], expected)
            self.assertEqual(helper["env"]["RUSTFLAGS"], acceptance.kwargs["env"]["RUSTFLAGS"])


if __name__ == "__main__":
    unittest.main()
