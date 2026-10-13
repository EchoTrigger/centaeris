"""Bounded native runsc restore experiment on an explicitly isolated Linux host.

This uses neither Docker nor any Centaeris database, account, volume or model.
The outer directory is evidence; only a fixed-size loop filesystem is writable
by runsc. Probe challenge values are returned to the host oracle, never saved by
the workload itself. Unit tests validate guards, not backend recovery support.
"""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys
import time

PREFIX = "centaeris-durable-probe-"
DISK_BYTES = 256 * 1024 * 1024
MEMORY_BYTES = 512 * 1024 * 1024
IDENTITY = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,95}\Z")
ORACLE_KEYS = ("nonce", "counter", "mmapNonce", "fileCounter", "fdOffset",
               "unlinkedOffset", "pipeBytes", "childPid", "paused", "threadTicks")


def write_json(path, value):
    with path.open("x", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2)
        handle.write("\n")


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def validate_id(experiment_id):
    if not IDENTITY.fullmatch(experiment_id):
        raise ValueError("invalid experiment identity")


def create_experiment(base, experiment_id):
    validate_id(experiment_id)
    base = Path(base)
    if base.is_symlink() or base.resolve() != base.absolute():
        raise ValueError("evidence parent must not traverse links")
    if not base.is_dir():
        raise ValueError("evidence parent must already exist")
    root = base / (PREFIX + experiment_id)
    root.mkdir(mode=0o700, exist_ok=False)
    write_json(root / "ownership.json", {"schema": "centaeris.durableProbeOwnership.v1",
                                        "experimentId": experiment_id, "root": str(root)})
    return root


def build_spec(experiment_id):
    validate_id(experiment_id)
    return {
        "ociVersion": "1.2.0",
        "process": {"terminal": False, "user": {"uid": 1000, "gid": 1000},
                    "args": ["/probe", "serve"], "cwd": "/work", "env": ["PATH=/"],
                    "noNewPrivileges": True,
                    "capabilities": {name: [] for name in ("bounding", "effective", "inheritable", "permitted", "ambient")},
                    "rlimits": [{"type": "RLIMIT_NOFILE", "hard": 128, "soft": 128},
                                {"type": "RLIMIT_FSIZE", "hard": 16777216, "soft": 16777216}]},
        "root": {"path": "rootfs", "readonly": True},
        "hostname": "durable-probe",
        "mounts": [{"destination": "/proc", "type": "proc", "source": "proc", "options": ["nosuid", "noexec", "nodev"]},
                   {"destination": "/work", "type": "tmpfs", "source": "tmpfs",
                    "options": ["nosuid", "nodev", "size=16777216", "mode=0700", "uid=1000", "gid=1000"]}],
        "linux": {"namespaces": [{"type": name} for name in ("pid", "mount", "ipc", "uts", "network")],
                  "cgroupsPath": "/" + PREFIX + experiment_id,
                  "resources": {"memory": {"limit": MEMORY_BYTES}, "pids": {"limit": 96},
                                "cpu": {"quota": 100000, "period": 100000}}},
    }


def compare_recovery(before, after, *, source_stopped):
    missing = [key for key in ORACLE_KEYS if key not in before or key not in after]
    mismatch = [key for key in ORACLE_KEYS if before.get(key) != after.get(key)]
    return {"passed": bool(source_stopped and not missing and not mismatch and before.get("paused") is True
                           and before.get("counter") == before.get("fileCounter")
                           and isinstance(before.get("counter"), int) and before["counter"] > 0),
            "sourceStopped": source_stopped, "missingFields": missing, "mismatchedFields": mismatch}


def checkpoint_files(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise ValueError("checkpoint contains a link")
        if path.is_file():
            result[path.relative_to(root).as_posix()] = {"sizeBytes": path.stat().st_size, "sha256": digest(path)}
        elif not path.is_dir():
            raise ValueError("checkpoint contains an unsupported node")
    if not result:
        raise ValueError("checkpoint is empty")
    return result


def seal_checkpoint(root, runtime_digest):
    return {"runtimeSha256": runtime_digest, "files": checkpoint_files(root)}


def verify_checkpoint(root, identity, runtime_digest):
    if identity["runtimeSha256"] != runtime_digest or identity["files"] != checkpoint_files(root):
        raise ValueError("checkpoint compatibility or content integrity mismatch")


def capability_matrix():
    return {key: {"status": "not-tested", "reason": "experiment has not established this capability"}
            for key in ("memoryState", "processTreeAndPipe", "openFileCursor", "unlinkedOpenFile",
                        "threadAndMmap", "renameLinksPermissions", "quiescedBackgroundWrites",
                        "controllerReconnect", "unquiescedCapture", "crossHostRecovery", "externalConnections",
                        "truncatedCheckpoint", "wrongDigest", "runtimeMismatch")}


class Experiment:
    def __init__(self, root, experiment_id, runsc):
        self.root, self.experiment_id, self.runsc = root, experiment_id, runsc
        self.disk = root / "disk"
        self.mounted = False
        self.ids = [PREFIX + experiment_id + suffix for suffix in ("-source", "-restored")]
        self.phases, self.commands = [], []
        self.io = []
        self.matrix = capability_matrix()

    def command(self, args, timeout=30, check=True, detached_output=None):
        started = time.monotonic()
        record = {"argv": [str(arg) for arg in args]}
        try:
            env = {"PATH": os.environ.get("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"), "LANG": "C"}
            if detached_output:
                # A detached sandbox inherits its stdio. Pipes would keep
                # communicate() waiting after the runsc parent has exited.
                with (self.root / (detached_output + ".log")).open("x") as output:
                    result = subprocess.run(args, stdin=subprocess.DEVNULL, stdout=output, stderr=output,
                                            text=True, timeout=timeout, env=env)
                result.stdout = ""
                result.stderr = (self.root / (detached_output + ".log")).read_text()[-8192:]
            else:
                result = subprocess.run(args, capture_output=True, text=True, timeout=timeout, env=env)
            record.update(exitCode=result.returncode, stdout=result.stdout[-8192:], stderr=result.stderr[-8192:])
            if check and result.returncode:
                raise RuntimeError(f"{Path(args[0]).name} exited {result.returncode}: {result.stderr[-2000:]}")
            return result
        finally:
            record["elapsedMs"] = (time.monotonic() - started) * 1000
            self.commands.append(record)

    def runtime(self, *args, **kwargs):
        return self.command([self.runsc, "--root=" + str(self.disk / "state"),
                             "--platform=systrap", "--network=none", "--overlay2=none", *args], **kwargs)

    def observe(self, container_id, action="observe", *, timeout=30):
        result = self.runtime("exec", "--user=1000:1000", container_id, "/probe", action, timeout=timeout)
        return json.loads(result.stdout)

    def wait_observation(self, container_id, predicate, *, timeout=10):
        # Only read-only observations may be retried. Freeze/resume/verify can
        # have effects even when their response is lost.
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("application observation deadline exceeded")
            try:
                value = self.observe(container_id, timeout=remaining)
            except (RuntimeError, subprocess.TimeoutExpired):
                pass
            else:
                if predicate(value):
                    return value
            time.sleep(min(0.05, remaining))

    def phase(self, name, operation):
        started = time.monotonic()
        outcome = "failed"
        try:
            result = operation()
            outcome = "supported"
            return result
        finally:
            self.phases.append({"phase": name, "elapsedMs": (time.monotonic() - started) * 1000, "outcome": outcome})

    def require_owned_mount(self):
        mount = json.loads(self.command(["findmnt", "--json", "--target", str(self.disk), "--output", "TARGET,SOURCE,FSTYPE,OPTIONS"]).stdout)["filesystems"][0]
        if mount["target"] != str(self.disk) or mount["fstype"] != "ext4" or not mount["source"].startswith("/dev/loop"):
            raise ValueError("fixed-size filesystem did not mount as expected")
        loop = json.loads(self.command(["losetup", "--json", "--list", "--output", "NAME,BACK-FILE", mount["source"]]).stdout)["loopdevices"]
        if len(loop) != 1 or Path(loop[0]["back-file"]).resolve() != (self.root / "disk.img").resolve():
            raise ValueError("loop device does not belong to this experiment")
        return mount

    def prepare(self, source, revision):
        if sys.platform != "linux" or os.geteuid() != 0:
            raise ValueError("native probe requires root on an explicitly disposable Linux experiment path")
        controllers = Path("/sys/fs/cgroup/cgroup.controllers").read_text().split()
        if not {"cpu", "memory", "pids"}.issubset(controllers):
            raise ValueError("required cgroup v2 controllers are unavailable")
        if (Path("/sys/fs/cgroup") / (PREFIX + self.experiment_id)).exists():
            raise ValueError("experiment cgroup already exists; refusing to reuse it")
        if shutil.disk_usage(self.root).free < DISK_BYTES * 2:
            raise ValueError("insufficient space for the fixed disk and retained evidence")
        self.disk.mkdir()
        disk_image = self.root / "disk.img"
        self.command(["fallocate", "-l", str(DISK_BYTES), str(disk_image)])
        self.command(["mkfs.ext4", "-q", "-F", str(disk_image)])
        self.command(["mount", "-o", "loop,nosuid,nodev", str(disk_image), str(self.disk)])
        self.mounted = True
        mount = self.require_owned_mount()
        bundle = self.disk / "bundle"
        rootfs = bundle / "rootfs"
        for subdir in ("proc", "work"):
            (rootfs / subdir).mkdir(parents=True)
        binary = rootfs / "probe"
        self.command(["gcc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-static", "-pthread", str(source), "-o", str(binary)])
        spec = build_spec(self.experiment_id)
        write_json(bundle / "config.json", spec)
        runsc_digest = digest(Path(self.runsc))
        manifest = {"schema": "centaeris.durableProbeManifest.v1", "experimentId": self.experiment_id,
                    "sourceRevision": revision, "sourceSha256": digest(source), "harnessSha256": digest(Path(__file__)),
                    "probeSha256": digest(binary), "rootfsIdentity": {"kind": "singleStaticBinary", "sha256": digest(binary)},
                    "runscSha256": runsc_digest, "runscVersion": self.command([self.runsc, "--version"]).stdout,
                    "kernel": platform.release(), "architecture": platform.machine(),
                    "cpu": self.command(["lscpu", "--json"]).stdout, "ociSpec": spec,
                    "network": "none", "diskLimitBytes": DISK_BYTES, "mount": mount,
                    "captureProtocol": "application freeze acknowledgement then one runsc memory checkpoint; memory-backed tmpfs; no resume",
                    "failureDomain": "single host", "cacheState": "uncontrolled correctness smoke"}
        write_json(self.root / "source-and-host-manifest.json", manifest)
        return bundle, runsc_digest

    def check_limits(self, filename="enforced-limits.json"):
        cgroup = Path("/sys/fs/cgroup") / (PREFIX + self.experiment_id)
        values = {key: (cgroup / key).read_text().strip() for key in ("cpu.max", "memory.max", "pids.max")}
        if values != {"cpu.max": "100000 100000", "memory.max": str(MEMORY_BYTES), "pids.max": "96"}:
            raise ValueError("kernel cgroup limits do not match the probe contract")
        write_json(self.root / filename, values)

    def run(self, bundle, runtime_digest):
        source_id, restored_id = self.ids
        self.phase("start", lambda: self.runtime("run", "--detach", "--bundle=" + str(bundle), source_id,
                                               detached_output="source-process"))
        self.check_limits()
        running_state = json.loads(self.runtime("state", source_id).stdout)
        source_pid = running_state["pid"]
        if running_state["status"] != "running" or not isinstance(source_pid, int) or source_pid <= 1:
            raise ValueError("source physical process is not running")
        source_process = Path(f"/proc/{source_pid}/stat")
        source_start = source_process.read_text().rsplit(")", 1)[1].split()[19]
        running_state["hostStartTicks"] = source_start
        write_json(self.root / "source-running.json", running_state)
        first = self.wait_observation(source_id, lambda value: value["counter"] > 0)
        self.wait_observation(source_id, lambda value: value["counter"] > first["counter"])
        before = self.observe(source_id, "freeze")
        if before["counter"] <= first["counter"]:
            raise ValueError("background writer did not run before freezing")
        write_json(self.root / "oracle-before.json", before)
        reconnected = self.observe(source_id)
        if before != reconnected:
            raise ValueError("new controller connection did not observe the same live instance")
        self.matrix["controllerReconnect"] = {"status": "supported", "reason": "independent runsc exec reconnected to the live application socket"}
        checkpoint = self.disk / "checkpoint"
        checkpoint.mkdir()
        self.phase("checkpoint", lambda: self.runtime("checkpoint", "--image-path=" + str(checkpoint),
                                                      source_id, timeout=60))
        # No --leave-running: demand physical source termination before restoration.
        source_state = json.loads(self.runtime("state", source_id).stdout)
        if source_state["status"] != "stopped":
            raise ValueError("source is not stopped after checkpoint")
        self.runtime("delete", source_id)
        deadline = time.monotonic() + 2
        while source_process.exists() and source_process.read_text().rsplit(")", 1)[1].split()[19] == source_start:
            if time.monotonic() >= deadline:
                raise ValueError("source physical process remains after deletion")
            time.sleep(0.02)
        write_json(self.root / "source-stopped.json", source_state)
        identity = seal_checkpoint(checkpoint, runtime_digest)
        write_json(self.root / "checkpoint-identity.json", identity)
        self.io.append({"operation": "checkpoint", "measurement": "retainedLogicalBytes", "kind": "gauge",
                        "value": sum(item["sizeBytes"] for item in identity["files"].values()), "status": "measured"})
        self.io.append({"operation": "checkpoint", "measurement": "retainedAllocatedBytes", "kind": "gauge",
                        "value": sum(path.stat().st_blocks * 512 for path in checkpoint.rglob("*") if path.is_file()), "status": "measured"})
        self.io.append({"operation": "restore", "measurement": "crossHostTransferBytes", "kind": "counter",
                        "value": "", "status": "not-tested"})
        verify_checkpoint(checkpoint, identity, runtime_digest)
        # Integrity validation happens before allowing a backend restore to consume it.
        faults = []
        for name, wrong in (("runtimeMismatch", {**identity, "runtimeSha256": "0" * 64}),
                            ("wrongDigest", {**identity, "files": {}})):
            try:
                verify_checkpoint(checkpoint, wrong, runtime_digest)
            except ValueError:
                faults.append({"case": name, "status": "supported", "layer": "probe admission", "result": "rejected before restore"})
                self.matrix[name] = {"status": "supported", "reason": "probe admission rejected incompatible identity; backend behavior not tested"}
            else:
                raise ValueError("invalid checkpoint identity was accepted")
        corrupted = self.disk / "checkpoint-truncated"
        shutil.copytree(checkpoint, corrupted)
        truncate_name = next(name for name, item in identity["files"].items() if item["sizeBytes"] > 0)
        with (corrupted / truncate_name).open("r+b") as handle:
            handle.truncate(0)
        try:
            verify_checkpoint(corrupted, identity, runtime_digest)
        except ValueError:
            faults.append({"case": "truncatedCheckpoint", "status": "supported", "layer": "probe admission", "result": "rejected before restore"})
            self.matrix["truncatedCheckpoint"] = {"status": "supported", "reason": "actual truncated image rejected by admission; backend behavior not tested"}
        else:
            raise ValueError("truncated image was accepted")
        with (self.root / "fault-results.jsonl").open("x", encoding="utf-8") as handle:
            for result in faults:
                handle.write(json.dumps(result) + "\n")
        self.phase("restore", lambda: self.runtime("restore", "--detach", "--bundle=" + str(bundle),
                                                   "--image-path=" + str(checkpoint), restored_id, timeout=60,
                                                   detached_output="restored-process"))
        self.check_limits("restored-enforced-limits.json")
        after = self.wait_observation(restored_id, lambda value: value["paused"] is True)
        write_json(self.root / "oracle-after.json", after)
        comparison = compare_recovery(before, after, source_stopped=True)
        write_json(self.root / "oracle-comparison.json", comparison)
        if not comparison["passed"]:
            raise ValueError("restored process state does not match the external oracle")
        verified = self.observe(restored_id, "verify")
        write_json(self.root / "restored-fd-verification.json", verified)
        if not all(verified.get(key) is True for key in ("pipeVerified", "fileVerified", "unlinkedVerified", "linksVerified")):
            raise ValueError("restored descriptors or child process failed actual read verification")
        self.observe(restored_id, "resume")
        self.wait_observation(restored_id, lambda value: value["threadTicks"] > before["threadTicks"]
                              and value["counter"] > before["counter"])
        resumed = self.observe(restored_id, "freeze")
        if resumed["threadTicks"] <= before["threadTicks"] or resumed["counter"] != resumed["fileCounter"]:
            raise ValueError("restored background thread did not continue coherently")
        write_json(self.root / "oracle-resumed.json", resumed)
        for key in ("memoryState", "processTreeAndPipe", "openFileCursor", "unlinkedOpenFile", "threadAndMmap", "renameLinksPermissions", "quiescedBackgroundWrites"):
            self.matrix[key] = {"status": "supported", "reason": "source deleted; new sandbox matched external oracle and actual restored descriptor reads"}

    def finish(self, failure):
        cleanup = []
        cleanup_failed = False
        owned_mount = False
        if self.mounted:
            try:
                self.require_owned_mount()
                owned_mount = True
            except Exception as error:
                cleanup.append({"ownershipCheckErrorType": type(error).__name__})
                cleanup_failed = True
        if owned_mount:
            for container_id in self.ids:
                try:
                    result = self.runtime("delete", "--force", container_id, check=False)
                    cleanup.append({"containerId": container_id, "exitCode": result.returncode})
                    cleanup_failed |= result.returncode != 0
                except Exception as error:
                    cleanup.append({"containerId": container_id, "errorType": type(error).__name__})
                    cleanup_failed = True
            # runsc's independent root owns a persistent null network namespace.
            # It is a mount even after the sandbox is deleted; unmount only this
            # exact known child, never a recursive host mount tree.
            network = self.disk / "state/null-netns"
            try:
                current = self.command(["findmnt", "-n", "-o", "TARGET,FSTYPE", "--target", str(network)], check=False)
                if current.stdout.split() == [str(network), "nsfs"]:
                    result = self.command(["umount", str(network)], check=False)
                    cleanup.append({"networkUnmountExitCode": result.returncode})
                    cleanup_failed |= result.returncode != 0
                # Refuse to unmount a path that no longer refers to our loop filesystem.
                mount = self.command(["findmnt", "-n", "-o", "TARGET", "--target", str(self.disk)], check=False)
                if mount.stdout.strip() == str(self.disk):
                    result = self.command(["umount", str(self.disk)], check=False)
                    cleanup.append({"unmountExitCode": result.returncode})
                    cleanup_failed |= result.returncode != 0
                else:
                    cleanup_failed = True
            except Exception as error:
                cleanup.append({"errorType": type(error).__name__})
                cleanup_failed = True
        write_json(self.root / "commands.json", self.commands)
        write_json(self.root / "capability-matrix.json", self.matrix)
        outcome = {"status": "failed" if failure or cleanup_failed else "supported", "failure": failure,
                   "scope": "single-host correctness smoke", "cleanup": cleanup}
        write_json(self.root / "outcome.json", outcome)
        with (self.root / "restore-phases.csv").open("x", newline="", encoding="utf-8") as handle:
            writer = csv.DictWriter(handle, fieldnames=["phase", "elapsedMs", "outcome"])
            writer.writeheader()
            writer.writerows(self.phases)
        with (self.root / "io-and-transfer.csv").open("x", newline="", encoding="utf-8") as handle:
            writer = csv.DictWriter(handle, fieldnames=["operation", "measurement", "kind", "value", "status"])
            writer.writeheader()
            writer.writerows(self.io)
        return outcome


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-parent", required=True, type=Path)
    parser.add_argument("--experiment-id", required=True)
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1] / "workloads/durable_execution/probe_main.c")
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--runsc", default="/usr/bin/runsc")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.source_revision):
        parser.error("source revision must be an exact Git commit")
    root = create_experiment(args.evidence_parent, args.experiment_id)
    experiment = Experiment(root, args.experiment_id, args.runsc)
    failure = None
    def deadline(_signal, _frame):
        raise TimeoutError("probe exceeded 240 second experiment deadline")
    if hasattr(signal, "SIGALRM"):
        signal.signal(signal.SIGALRM, deadline)
        signal.alarm(240)
    try:
        bundle, runtime_digest = experiment.prepare(args.source.resolve(), args.source_revision)
        experiment.run(bundle, runtime_digest)
    except Exception as error:
        failure = {"errorType": type(error).__name__, "message": str(error)[:2000]}
    finally:
        if hasattr(signal, "SIGALRM"):
            signal.alarm(0)
        outcome = experiment.finish(failure)
    print(json.dumps({"evidence": str(root), "status": outcome["status"]}))
    return 0 if outcome["status"] == "supported" else 1


if __name__ == "__main__":
    raise SystemExit(main())
