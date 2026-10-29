"""Own one genuine native supervisor for the disposable Ubuntu Docker gate."""
import argparse
import json
import os
from pathlib import Path
import queue
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import time


def daemon_flags(arguments):
    values = {}
    wanted = {"config-file", "data-root", "exec-root", "containerd-namespace"}
    for index, argument in enumerate(arguments):
        name, separator, value = argument.removeprefix("--").partition("=")
        if not argument.startswith("--") or name not in wanted:
            continue
        if not separator:
            if index + 1 == len(arguments) or arguments[index + 1].startswith("--"):
                raise RuntimeError(f"Docker --{name} value unavailable")
            value = arguments[index + 1]
        values[name] = value
    return values


def daemon_configuration(expected_id):
    docker = str(Path(shutil.which("docker")).resolve(strict=True))
    info = json.loads(subprocess.check_output(
        [docker, "--host", "unix:///var/run/docker.sock", "info", "--format", "{{json .}}"], text=True))
    if info["ID"] != expected_id:
        raise RuntimeError("gate and supervisor must use the same local Docker daemon")
    pid = int(Path("/run/docker.pid").read_text())
    arguments = Path(f"/proc/{pid}/cmdline").read_bytes().decode().rstrip("\0").split("\0")
    if Path(arguments[0]).name != "dockerd":
        raise RuntimeError("local Docker daemon process identity unavailable")
    flags = daemon_flags(arguments[1:])
    config_path = Path(flags.get("config-file", "/etc/docker/daemon.json"))
    config = json.loads(config_path.read_text()) if config_path.exists() else {}
    data_root = Path(flags.get("data-root", config.get("data-root", "/var/lib/docker")))
    if not data_root.is_absolute():
        raise RuntimeError("absolute Docker data-root required")
    if data_root.resolve() != Path(info["DockerRootDir"]).resolve():
        raise RuntimeError("Docker data-root does not match the local daemon configuration")
    exec_root = Path(flags.get("exec-root", config.get("exec-root", "/var/run/docker")))
    namespace = flags.get("containerd-namespace", config.get("containerd-namespace", "moby"))
    if not exec_root.is_absolute() or not namespace or "/" in namespace or namespace in (".", ".."):
        raise RuntimeError("Docker runtime root configuration invalid")
    runtime = info["Runtimes"]["runsc"]
    runtime_path = Path(runtime.get("path", runtime.get("Path", "")))
    if not runtime_path.is_absolute():
        located = shutil.which(str(runtime_path))
        if located is None:
            raise RuntimeError("installed runsc binary unavailable")
        runtime_path = Path(located)
    return {"ociRuntime": "runsc", "runtimePath": str(runtime_path.resolve(strict=True)),
            "runtimeRoot": str(exec_root.resolve() / "runtime-runc" / namespace),
            "dockerPath": docker, "dockerSocket": "/var/run/docker.sock", "controllerUid": 0}


def run_host(supervisor, helper, configuration, *, parent=Path("/run"),
             control=sys.stdin, ready=sys.stdout, startup_timeout=15, stop_timeout=10):
    # The production entry point requires root. Only this freshly created tree
    # and this Popen child belong to the launcher; it never searches for processes.
    with tempfile.TemporaryDirectory(prefix="centaeris-fresh-start-", dir=parent) as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        for name, source in (("environment_host_supervisor", supervisor), ("execution_agent", helper)):
            shutil.copyfile(source, root / name)
            (root / name).chmod(0o500)
        socket_directory = root / "host"
        socket_directory.mkdir(mode=0o700)
        config = {**configuration, "schema": "environment.host.v1",
                  "socketPath": str(socket_directory / "control.sock"),
                  "controlRoot": str(root / "control"), "outputRoot": str(root / "output"),
                  "helperPath": str(root / "execution_agent")}
        config_path = root / "supervisor.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        config_path.chmod(0o600)
        with (root / "supervisor.log").open("wb") as log:
            process = subprocess.Popen([str(root / "environment_host_supervisor"), str(config_path)],
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
            try:
                deadline = time.monotonic() + startup_timeout
                while True:
                    if process.poll() is not None:
                        raise RuntimeError(f"owned host supervisor exited before readiness ({process.returncode})")
                    path = Path(config["socketPath"])
                    if path.exists() and stat.S_ISSOCK(path.stat().st_mode):
                        with socket.socket(socket.AF_UNIX) as connection:
                            connection.settimeout(1)
                            connection.connect(str(path))
                        break
                    if time.monotonic() >= deadline:
                        raise RuntimeError("owned host supervisor readiness timed out")
                    time.sleep(.05)
                print(root, file=ready, flush=True)
                commands = queue.Queue()
                threading.Thread(target=lambda: commands.put(control.readline()), daemon=True).start()
                while True:
                    if process.poll() is not None:
                        raise RuntimeError(f"owned host supervisor exited during gate ({process.returncode})")
                    try:
                        command = commands.get(timeout=.1)
                    except queue.Empty:
                        continue
                    if command not in ("", "stop\n"):
                        raise RuntimeError("invalid host lifecycle command")
                    break
            except BaseException:
                log.flush()
                with (root / "supervisor.log").open("rb") as diagnostic:
                    diagnostic.seek(max(0, diagnostic.seek(0, os.SEEK_END) - 16384))
                    print(diagnostic.read(16384).decode(errors="replace"), file=sys.stderr)
                raise
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=stop_timeout)
                    except subprocess.TimeoutExpired:
                        process.kill()
                process.wait(timeout=stop_timeout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--supervisor", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--daemon-id", required=True)
    args = parser.parse_args()
    if sys.platform != "linux" or os.geteuid() != 0:
        parser.error("the disposable host supervisor requires Linux root")
    for number in (signal.SIGTERM, signal.SIGINT):
        signal.signal(number, lambda *_, number=number: sys.exit(128 + number))
    run_host(args.supervisor, args.helper, daemon_configuration(args.daemon_id))


if __name__ == "__main__":
    main()
