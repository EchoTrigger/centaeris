"""Measure the real execution_agent in owned, bounded Docker/runsc sandboxes.

This is direct helper capture and restore, not the Runtime trigger/upload path.
Missing API, transport and Runtime observations remain unmeasured.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time

if __package__:
    from . import workspace_protection as fixture
else:
    import workspace_protection as fixture

LABEL = "org.centaeris.snapshot-probe"
NAME = re.compile(r"[a-z0-9][a-z0-9-]{0,63}\Z")
IMAGE = re.compile(r"sha256:[0-9a-f]{64}\Z")
MAX_DISK_BYTES = 8 * 1024 ** 3


def file_hash(path):
    result = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def container_arguments(name, image_id, binary, module):
    if not NAME.fullmatch(name) or not IMAGE.fullmatch(image_id):
        raise ValueError("exact image digest and unique safe container identity required")
    binary, module = fixture.checked_root(binary), fixture.checked_root(module)
    if not binary.is_file() or not module.is_file():
        raise ValueError("explicit helper and fixture sources must exist")
    return ["docker", "run", "--detach", "--name=" + name, "--label=" + LABEL + "=" + name,
            "--runtime=runsc", "--network=none", "--read-only", "--cpus=1", "--memory=1g", "--pids-limit=128",
            "--cap-drop=ALL", "--cap-add=CHOWN", "--security-opt=no-new-privileges",
            "--tmpfs", "/mnt/data:rw,nosuid,nodev,size=536870912,mode=1777,uid=0,gid=0",
            "--tmpfs", "/run/centaeris:rw,nosuid,nodev,size=1048576,mode=0700,uid=0,gid=0",
            "--mount", f"type=bind,src={binary},dst=/probe/execution_agent,readonly",
            "--mount", f"type=bind,src={module},dst=/probe/workspace_protection.py,readonly",
            "--env", "PYTHONPATH=/probe", "--env", "PYTHONDONTWRITEBYTECODE=1",
            "--user=0:0", "--entrypoint=/bin/sleep", image_id, "600"]


def read_observations(stderr, operation_id, exit_code):
    events = []
    for line in stderr.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict) and event.get("schema") == "workspace.perf.v1":
            fixture.validate_event(event)
            if event["source"] != "sandbox" or event["operationId"] != operation_id:
                raise ValueError("foreign helper observation")
            if event.get("outcome") != ("ok" if exit_code == 0 else "error"):
                raise ValueError("observation outcome contradicts actual command exit")
            events.append(event)
    if sorted(event["metric"] for event in events) != ["scannedEntries", "sourceReadBytes"]:
        raise ValueError("helper did not provide exactly one complete measurement set")
    return events


def inspect_frame(stream):
    frame_hash = hashlib.sha256()
    total = 0
    def read(size):
        nonlocal total
        data = stream.read(size)
        frame_hash.update(data)
        total += len(data)
        if len(data) != size:
            raise ValueError("truncated snapshot frame")
        return data
    length = int.from_bytes(read(4), "big")
    if not 0 < length <= 1048576:
        raise ValueError("invalid frame manifest size")
    manifest = json.loads(read(length))
    if set(manifest) != {"schema", "files"} or manifest["schema"] != "workspace.snapshot.v1":
        raise ValueError("unsupported frame schema")
    files, names = [], set()
    for item in manifest["files"]:
        if set(item) != {"path", "sizeBytes", "sha256", "executable"}:
            raise ValueError("unknown frame file metadata")
        name = item["path"].removeprefix("fixture/")
        if item["path"] != "fixture/" + name or name not in {"cold.bin", "hot.bin", "append.log"} or name in names:
            raise ValueError("frame is outside the synthetic fixture")
        names.add(name)
        remaining = item["sizeBytes"]
        if type(remaining) is not int or not 0 <= remaining <= 512 * 1024 * 1024:
            raise ValueError("unbounded frame file")
        actual = hashlib.sha256()
        while remaining:
            chunk = read(min(remaining, 1024 * 1024))
            actual.update(chunk)
            remaining -= len(chunk)
        if item["sha256"] != "sha256:" + actual.hexdigest():
            raise ValueError("snapshot payload digest differs from manifest")
        files.append({"name": name, "sizeBytes": item["sizeBytes"], "sha256": actual.hexdigest()})
    if stream.read(1):
        raise ValueError("snapshot has trailing bytes")
    return {"files": sorted(files, key=lambda item: item["name"]), "frameSha256": frame_hash.hexdigest(),
            "frameBytes": total, "expandedSizeBytes": sum(item["sizeBytes"] for item in files)}


def publishable_capture(exit_code, inspected):
    return exit_code == 0 and isinstance(inspected, dict) and bool(inspected.get("frameSha256"))


def validate_kernel_limits(values):
    expected = {"cpu.max": "100000 100000", "memory.max": "1073741824", "pids.max": "128"}
    if values != expected:
        raise ValueError("kernel cgroup limits differ from the bounded experiment")


def kernel_limits(pid):
    if type(pid) is not int or pid <= 0:
        raise ValueError("sandbox lacks a live host process")
    entries = Path(f"/proc/{pid}/cgroup").read_text().splitlines()
    paths = [entry[3:] for entry in entries if entry.startswith("0::/")]
    if len(paths) != 1:
        raise ValueError("one unified cgroup is required")
    cgroup_root = Path("/sys/fs/cgroup").resolve(strict=True)
    selected = (cgroup_root / paths[0].lstrip("/")).resolve(strict=True)
    if not selected.is_relative_to(cgroup_root) or selected == cgroup_root:
        raise ValueError("invalid sandbox cgroup path")
    limits = {key: (selected / key).read_text().strip()
              for key in ("cpu.max", "memory.max", "pids.max")}
    validate_kernel_limits(limits)
    return {"path": str(selected), "limits": limits}


class Probe:
    def __init__(self, root, name, image_id, binary, module, cold_bytes):
        self.root, self.name, self.image_id = root, name, image_id
        self.binary, self.module, self.cold_bytes = binary, module, cold_bytes
        self.active = {}
        self.pending = set()
        self.commands, self.events, self.results = [], [], []

    def command(self, args, *, stdin=None, stdout=None, timeout=120, check=True):
        started = time.monotonic()
        result = subprocess.run(args, stdin=stdin, stdout=stdout or subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=timeout,
                                env={"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "C"})
        self.commands.append({"argv": [str(arg) for arg in args], "exitCode": result.returncode,
                              "elapsedMs": (time.monotonic() - started) * 1000})
        if check and result.returncode:
            raise RuntimeError(f"command failed: {args[:2]}: {result.stderr[-1500:].decode(errors='replace')}")
        return result

    def start(self, suffix):
        name = self.name + "-" + suffix
        arguments = container_arguments(name, self.image_id, self.binary, self.module)
        existing = self.command(["docker", "ps", "--all", "--quiet", "--no-trunc", "--filter", "name=^/" + name + "$"])
        if existing.stdout.strip():
            raise ValueError("sandbox name already exists; refusing to adopt it")
        # A CLI timeout can happen after Docker creates the container. Retain
        # the exact requested name until its returned identity is established.
        self.pending.add(name)
        result = self.command(arguments)
        container_id = result.stdout.decode().strip()
        if not re.fullmatch(r"[0-9a-f]{64}", container_id):
            raise ValueError("invalid new container identity")
        self.active[container_id] = name
        self.pending.remove(name)
        inspect = json.loads(self.command(["docker", "inspect", container_id]).stdout)[0]
        config = inspect["HostConfig"]
        if (inspect["Image"] != self.image_id or inspect["Config"]["Labels"].get(LABEL) != name
                or config["Runtime"] != "runsc" or config["NetworkMode"] != "none"
                or not config["ReadonlyRootfs"] or config["Memory"] != 1024 ** 3
                or config["PidsLimit"] != 128 or config["NanoCpus"] != 1000000000):
            raise ValueError("created sandbox does not have the requested isolation")
        cgroup = kernel_limits(inspect["State"]["Pid"])
        (self.root / (suffix + "-container.json")).write_text(json.dumps({
            "id": container_id, "name": name, "imageId": inspect["Image"],
            "runtime": config["Runtime"], "memoryBytes": config["Memory"],
            "pidsLimit": config["PidsLimit"], "nanoCpus": config["NanoCpus"],
            "network": config["NetworkMode"], "mounts": inspect["Mounts"],
            "kernelCgroup": cgroup,
        }, indent=2))
        return container_id

    def reconcile_start(self, name):
        if name not in self.pending:
            raise ValueError("container creation was not pending in this experiment")
        result = self.command(["docker", "inspect", name], check=False)
        if result.returncode:
            raise RuntimeError("sandbox creation outcome remains unknown")
        inspected = json.loads(result.stdout)
        if len(inspected) != 1:
            raise RuntimeError("sandbox creation outcome remains unknown")
        container = inspected[0]
        identity = container.get("Id")
        if (not isinstance(identity, str) or not re.fullmatch(r"[0-9a-f]{64}", identity)
                or container.get("Name") != "/" + name
                or container.get("Config", {}).get("Labels", {}).get(LABEL) != name):
            raise RuntimeError("refusing cleanup of a foreign container creation result")
        self.active[identity] = name
        self.pending.remove(name)

    def remove(self, container_id):
        name = self.active[container_id]
        inspect = json.loads(self.command(["docker", "inspect", container_id]).stdout)[0]
        if inspect["Config"]["Labels"].get(LABEL) != name or inspect["Name"] != "/" + name:
            raise ValueError("refusing cleanup of a foreign container")
        self.command(["docker", "rm", "--force", container_id])
        del self.active[container_id]

    def python(self, container_id, code, *, user="10001:10001"):
        return self.command(["docker", "exec", "--user=" + user, container_id,
                             "python", "-c", "import json; from pathlib import Path; import workspace_protection as w; " + code])

    def capture(self, container_id, operation_id, *, may_reject=False):
        path = self.root / (operation_id + ".snapshot")
        started = time.monotonic()
        with path.open("xb") as output:
            result = self.command(["docker", "exec", "--user=0:0", "--env", "WORKSPACE_PERF_OBSERVATIONS=1",
                                   "--env", "WORKSPACE_PERF_OPERATION_ID=" + operation_id,
                                   container_id, "/probe/execution_agent", "snapshot-collect"], stdout=output, check=False)
        elapsed = (time.monotonic() - started) * 1000
        stderr = result.stderr.decode("utf-8")
        (self.root / (operation_id + ".stderr")).write_text(stderr)
        events = read_observations(stderr, operation_id, result.returncode)
        self.events.extend(events)
        inspected = None
        if result.returncode == 0:
            with path.open("rb") as source:
                inspected = inspect_frame(source)
        elif not may_reject or "workspace snapshot source changed" not in stderr:
            raise RuntimeError("snapshot capture failed unexpectedly: " + stderr[-1500:])
        record = {"operationId": operation_id, "exitCode": result.returncode, "externalCaptureElapsedMs": elapsed,
                  "captureFrameBytes": path.stat().st_size, "publishable": publishable_capture(result.returncode, inspected),
                  "inspection": inspected, "safeConcurrentRejection": result.returncode != 0 and may_reject}
        return path, record

    def restore(self, snapshot, record, suffix):
        if not record["publishable"]:
            return {"status": "not-tested", "reason": "partial or rejected capture is not a restoration source"}
        container_id = self.start(suffix)
        try:
            started = time.monotonic()
            with snapshot.open("rb") as source:
                self.command(["docker", "exec", "--interactive", "--user=0:0", container_id,
                              "/probe/execution_agent", "snapshot-restore"], stdin=source)
            elapsed = (time.monotonic() - started) * 1000
            # Restoration seals directories root:root; observation is read-only.
            actual = json.loads(self.python(container_id, "print(json.dumps(w.tree_manifest(Path('/mnt/data/fixture'))))", user="0:0").stdout)
            if actual["files"] != record["inspection"]["files"]:
                raise ValueError("restored bytes do not match the selected immutable capture")
            return {"status": "passed", "externalRestoreElapsedMs": elapsed, "tree": actual}
        finally:
            self.remove(container_id)

    def run(self):
        baseline_hashes = []
        for index, workload in enumerate(fixture.WORKLOADS):
            container_id = self.start(str(index) + "-source")
            writer = None
            try:
                initial = json.loads(self.python(container_id, f"print(json.dumps(w.prepare_workload(Path('/mnt/data/fixture'),{self.cold_bytes})))").stdout)
                baseline_hashes.append(initial["treeSha256"])
                base_path, baseline = self.capture(container_id, f"{index}-baseline")
                baseline["restore"] = self.restore(base_path, baseline, str(index) + "-base-restore")
                if workload == "background-write":
                    writer_log = (self.root / (str(index) + "-writer.log")).open("xb")
                    writer = subprocess.Popen(["docker", "exec", "--user=10001:10001", container_id,
                                               "python", "-c", "from pathlib import Path; import workspace_protection as w; print(w.background_writer(Path('/mnt/data/fixture'),3))"],
                                              stdout=writer_log, stderr=subprocess.STDOUT)
                    time.sleep(0.1)
                else:
                    self.python(container_id, f"w.mutate_workload(Path('/mnt/data/fixture'),{workload!r})")
                path, changed = self.capture(container_id, f"{index}-after", may_reject=workload == "background-write")
                if writer:
                    writer.wait(timeout=15)
                    writer_log.close()
                    if writer.returncode:
                        raise ValueError("background writer failed instead of exercising capture")
                changed["restore"] = self.restore(path, changed, str(index) + "-after-restore")
                row = {"workload": workload, "initial": initial, "baseline": baseline, "after": changed,
                       "cacheState": "uncontrolled/warm from preparation and baseline verification"}
                if changed["publishable"]:
                    row["sameFrameAsBaseline"] = baseline["inspection"]["frameSha256"] == changed["inspection"]["frameSha256"]
                    if workload in {"unchanged", "rename-delete-revert"} and not row["sameFrameAsBaseline"]:
                        raise ValueError("unchanged fixture produced a different snapshot")
                self.results.append(row)
                (self.root / (str(index) + "-result.json")).write_text(json.dumps(row, indent=2))
            finally:
                if writer and writer.poll() is None:
                    # Its only target is this exact owned sandbox; container
                    # deletion below terminates its workload, then join the CLI.
                    self.remove(container_id)
                    writer.wait(timeout=15)
                if container_id in self.active:
                    self.remove(container_id)
        if len(set(baseline_hashes)) != 1:
            raise ValueError("workloads did not start from the same bytes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-parent", required=True, type=Path)
    parser.add_argument("--experiment-id", required=True)
    parser.add_argument("--image-id", required=True)
    parser.add_argument("--agent", required=True, type=Path)
    parser.add_argument("--cold-bytes", type=int, default=256 * 1024 * 1024)
    args = parser.parse_args()
    if not NAME.fullmatch(args.experiment_id) or len(args.experiment_id) > 36:
        parser.error("experiment identity must be safe and at most 36 characters")
    if not 8192 <= args.cold_bytes <= 256 * 1024 * 1024:
        parser.error("cold fixture must be 8 KiB..256 MiB")
    parent = fixture.checked_root(args.evidence_parent)
    stat = os.statvfs(parent)
    mount = json.loads(subprocess.check_output(["findmnt", "--json", "--target", str(parent), "--output", "TARGET,SOURCE,FSTYPE"]))["filesystems"][0]
    if mount["target"] != str(parent) or not mount["source"].startswith("/dev/loop") or stat.f_blocks * stat.f_frsize > MAX_DISK_BYTES:
        parser.error("evidence parent must be the root of a dedicated loop filesystem bounded to 8 GiB")
    root = parent / ("helper-" + args.experiment_id)
    root.mkdir(mode=0o700, exist_ok=False)
    module = Path(fixture.__file__).resolve()
    probe = Probe(root, "c-snapshot-" + args.experiment_id, args.image_id, args.agent.resolve(), module, args.cold_bytes)
    manifest = {"schema": "centaeris.workspaceSnapshotProbe.v1", "experimentId": args.experiment_id,
                "imageId": args.image_id, "agentSha256": file_hash(args.agent), "harnessSha256": file_hash(Path(__file__)),
                "fixtureSha256": file_hash(module), "coldBytes": args.cold_bytes, "outputMount": mount,
                "commandTimeoutSeconds": 120, "containerLifetimeSeconds": 600,
                "scope": "direct execution_agent collect/restore in isolated runsc containers",
                "notMeasured": ["automaticTriggers", "runtimeSpoolWrittenBytes", "runtimeUploads", "apiCommit", "apiGc", "actualTransportBytes", "coldCache"]}
    (root / "source-and-host-manifest.json").write_text(json.dumps(manifest, indent=2))
    failure = None
    try:
        probe.run()
    except BaseException as error:
        failure = {"type": type(error).__name__, "message": str(error)[:2000]}
        if not isinstance(error, Exception):
            raise
    finally:
        cleanup_failures = []
        for name in list(probe.pending):
            try:
                probe.reconcile_start(name)
            except Exception as error:
                cleanup_failures.append({"type": type(error).__name__,
                                         "message": "creation cleanup unknown: " + str(error)[:1000]})
        for container_id in list(probe.active):
            try:
                probe.remove(container_id)
            except Exception as error:
                cleanup_failures.append({"type": type(error).__name__,
                                         "message": "owned cleanup failed: " + str(error)[:1000]})
        if failure is None and cleanup_failures:
            failure = cleanup_failures[0]
        if failure is None and (len(probe.results) != len(fixture.WORKLOADS) or probe.active or probe.pending):
            failure = {"type": "IncompleteExperiment", "message": "all workloads and owned cleanup must complete"}
        (root / "commands.json").write_text(json.dumps(probe.commands, indent=2))
        (root / "observations.jsonl").write_text("".join(json.dumps(row) + "\n" for row in probe.events))
        if probe.events:
            fixture.write_summary(probe.events, root / "summary")
        outcome = {"status": "failed" if failure else "passed", "failure": failure,
                   "completedWorkloads": len(probe.results), "remainingOwnedContainers": list(probe.active),
                   "unresolvedContainerNames": sorted(probe.pending), "cleanupFailures": cleanup_failures}
        (root / "outcome.json").write_text(json.dumps(outcome, indent=2))
    print(json.dumps({"evidence": str(root), **outcome}))
    return 1 if failure else 0


if __name__ == "__main__":
    raise SystemExit(main())
