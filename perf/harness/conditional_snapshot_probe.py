"""Measure conditional capture against a forced full capture of the same bytes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

if __package__:
    from . import workspace_snapshot_probe as full
else:
    import workspace_snapshot_probe as full

SCHEMA = "workspace.snapshot.capture.v1"
WORKLOADS = (*full.fixture.WORKLOADS, "protected-inputs")


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate metadata field")
        value[key] = item
    return value


def read_json_frame(stream, limit):
    prefix = stream.read(4)
    if len(prefix) != 4:
        raise ValueError("truncated metadata length")
    length = int.from_bytes(prefix, "big")
    if not 0 < length <= limit:
        raise ValueError("unbounded metadata")
    data = stream.read(length)
    if len(data) != length:
        raise ValueError("truncated metadata")
    return json.loads(data, object_pairs_hook=unique_object), data


def manifest_digest(manifest):
    return "sha256:" + hashlib.sha256(json.dumps(manifest, separators=(",", ":"), ensure_ascii=False).encode()).hexdigest()


def inspect_capture(stream, capture_id, baseline):
    response, _ = read_json_frame(stream, 1024)
    if (not isinstance(response, dict) or set(response) != {
            "schema", "captureId", "disposition", "baselineSnapshotSha256", "manifestSha256"}
            or response["schema"] != SCHEMA or response["captureId"] != capture_id):
        raise ValueError("capture response identity mismatch")
    offset = stream.tell()
    if response["disposition"] == "unchanged":
        if (baseline is None or response["baselineSnapshotSha256"] != baseline["snapshotSha256"]
                or response["manifestSha256"] != manifest_digest(baseline["manifest"]) or stream.read(1)):
            raise ValueError("unchanged response does not bind the requested immutable baseline")
        inspection = None
        frame_bytes = 0
    elif response["disposition"] == "snapshot":
        if response["baselineSnapshotSha256"] is not None:
            raise ValueError("full capture cannot claim unchanged baseline")
        if stream.read(1):
            stream.seek(offset)
            manifest, _ = read_json_frame(stream, 1048576)
            stream.seek(offset)
            inspection = full.inspect_frame(stream)
        else:
            manifest = {"schema": "workspace.snapshot.v1", "files": []}
            inspection = {"files": [], "frameSha256": hashlib.sha256(b"").hexdigest(),
                          "frameBytes": 0, "expandedSizeBytes": 0}
        if response["manifestSha256"] != manifest_digest(manifest):
            raise ValueError("response manifest differs from actual snapshot")
        frame_bytes = inspection["frameBytes"]
    else:
        raise ValueError("unknown capture disposition")
    return {"disposition": response["disposition"], "bodyOffset": offset,
            "snapshotFrameBytes": frame_bytes, "inspection": inspection, "receipt": response}


class ConditionalProbe(full.Probe):
    def protected_inputs(self):
        container = self.start("protected-source")
        try:
            self.python(container, "w.prepare_workload(Path('/mnt/data/fixture'), 8192)")
            base_path, base = self.capture(container, "protected-baseline")
            with base_path.open("rb") as source:
                manifest, _ = read_json_frame(source, 1048576)
            baseline = {"snapshotSha256": "sha256:" + base["inspection"]["frameSha256"], "manifest": manifest}
            body = b"synthetic protected input; must not enter a workspace snapshot\n"
            materialized = {"inputRef": "synthetic-input", "virtualPath": "protected-note.txt", "sizeBytes": len(body),
                "sha256": "sha256:" + hashlib.sha256(body).hexdigest(), "sourceVersion": "synthetic-v1"}
            encoded = json.dumps(materialized, separators=(",", ":")).encode()
            request_path = self.root / "protected-input.frame"
            request_path.write_bytes(len(encoded).to_bytes(4, "big") + encoded + body)
            checks = []
            for operation in ("materialize-input", "revoke-input"):
                if operation == "revoke-input":
                    request_path = self.root / "protected-revoke.json"
                    request_path.write_text(json.dumps({"inputRef": "synthetic-input",
                        "virtualPath": "protected-note.txt", "state": "access_revoked"}))
                with request_path.open("rb") as source:
                    self.command(["docker", "exec", "--interactive", "--user=0:0", container,
                                  "/probe/execution_agent", operation], stdin=source)
                _, conditional = self.capture_conditional(container, operation + "-conditional", baseline)
                _, forced = self.capture(container, operation + "-forced")
                if (conditional.get("disposition") != "unchanged" or conditional["exitCode"]
                        or forced["inspection"] != base["inspection"]):
                    raise ValueError("protected input leaked into workspace capture")
                checks.append({"operation": operation, "conditional": conditional, "forcedFull": forced})
            restored = self.restore_at_offset(base_path, 0, base["inspection"], "protected-restore")
            row = {"workload": "protected-inputs", "baseline": base, "checks": checks, "restore": restored}
            self.results.append(row)
            (self.root / "protected-result.json").write_text(json.dumps(row, indent=2))
        finally:
            self.remove(container)

    def capture_conditional(self, container, operation, baseline):
        request = {"schema": SCHEMA, "captureId": operation, "baseline": baseline}
        request_path = self.root / (operation + ".request.json")
        request_path.write_text(json.dumps(request, separators=(",", ":")))
        output_path = self.root / (operation + ".capture")
        started = time.monotonic()
        with request_path.open("rb") as source, output_path.open("xb") as output:
            result = self.command(["docker", "exec", "--interactive", "--user=0:0",
                "--env", "WORKSPACE_PERF_OBSERVATIONS=1", "--env", "WORKSPACE_PERF_OPERATION_ID=" + operation,
                container, "/probe/execution_agent", "snapshot-capture"], stdin=source, stdout=output, check=False)
        elapsed = (time.monotonic() - started) * 1000
        stderr = result.stderr.decode("utf-8")
        (self.root / (operation + ".stderr")).write_text(stderr)
        self.events.extend(full.read_observations(stderr, operation, result.returncode))
        row = {"exitCode": result.returncode, "helperOutputBytes": output_path.stat().st_size,
               "externalCaptureElapsedMs": elapsed}
        if result.returncode == 0:
            with output_path.open("rb") as source:
                row.update(inspect_capture(source, operation, baseline))
        elif "snapshot capture requires no agent processes" not in stderr:
            raise ValueError("conditional capture failed unexpectedly: " + stderr[-1000:])
        return output_path, row

    def restore_at_offset(self, path, offset, expected, suffix):
        container = self.start(suffix)
        try:
            with path.open("rb") as source:
                source.seek(offset)
                self.command(["docker", "exec", "--interactive", "--user=0:0", container,
                              "/probe/execution_agent", "snapshot-restore"], stdin=source)
            actual = json.loads(self.python(container,
                "print(json.dumps(w.tree_manifest(Path('/mnt/data/fixture'))))", user="0:0").stdout)
            if actual["files"] != expected["files"]:
                raise ValueError("conditional selection restored different bytes")
            return {"status": "passed", "tree": actual}
        finally:
            self.remove(container)

    def run(self):
        for index, workload in enumerate(full.fixture.WORKLOADS):
            container = self.start(str(index) + "-source")
            paths, writer = [], None
            try:
                self.python(container, f"w.prepare_workload(Path('/mnt/data/fixture'), {self.cold_bytes})")
                base_path, base = self.capture(container, f"{index}-baseline")
                paths.append(base_path)
                with base_path.open("rb") as source:
                    manifest, _ = read_json_frame(source, 1048576)
                baseline = {"snapshotSha256": "sha256:" + base["inspection"]["frameSha256"], "manifest": manifest}
                if workload == "background-write":
                    writer = subprocess.Popen(["docker", "exec", "--user=10001:10001", container, "python", "-c",
                        "import json,time\nfrom pathlib import Path\n"
                        "count=0\ndeadline=time.monotonic()+10\n"
                        "with Path('/mnt/data/fixture/append.log').open('ab') as output:\n"
                        " while time.monotonic()<deadline:\n"
                        "  output.write(b'background\\n'*128); output.flush(); count+=1\n"
                        "  if count==1: Path('/mnt/data/.conditional-writer-ready').write_text('ready')\n"
                        "  time.sleep(0.01)\nprint(json.dumps({'writes':count}))"],
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    deadline = time.monotonic() + 5
                    while True:
                        ready = self.python(container, "print(json.dumps(Path('/mnt/data/.conditional-writer-ready').is_file()))")
                        if json.loads(ready.stdout):
                            break
                        if writer.poll() is not None or time.monotonic() >= deadline:
                            raise ValueError("background writer did not acknowledge its first completed write")
                        time.sleep(0.02)
                else:
                    self.python(container, f"w.mutate_workload(Path('/mnt/data/fixture'), {workload!r})")
                capture_path, conditional = self.capture_conditional(container, f"{index}-conditional", baseline)
                paths.append(capture_path)
                row = {"workload": workload, "baseline": base, "conditional": conditional,
                       "cacheState": "warm/uncontrolled; forced full verification follows conditional capture"}
                if writer:
                    output, error = writer.communicate(timeout=15)
                    if writer.returncode or conditional["exitCode"] == 0 or json.loads(output).get("writes", 0) < 1:
                        raise ValueError("active writer must run successfully and prevent capture: " + error.decode(errors="replace"))
                    row["writer"] = json.loads(output)
                    row["restore"] = {"status": "not-tested", "reason": "active writer safely rejected"}
                else:
                    if conditional["exitCode"]:
                        raise ValueError("quiescent conditional capture failed")
                    forced_path, forced = self.capture(container, f"{index}-forced")
                    paths.append(forced_path)
                    expected = "unchanged" if workload in {"unchanged", "rename-delete-revert"} else "snapshot"
                    if conditional["disposition"] != expected:
                        raise ValueError("unexpected disposition for controlled mutation")
                    selected = base["inspection"] if expected == "unchanged" else conditional["inspection"]
                    if selected != forced["inspection"]:
                        raise ValueError("conditional result differs from forced full capture")
                    row["forcedFull"] = forced
                    row["restore"] = self.restore_at_offset(base_path if expected == "unchanged" else capture_path,
                        0 if expected == "unchanged" else conditional["bodyOffset"], selected, str(index) + "-restore")
                self.results.append(row)
                (self.root / (str(index) + "-result.json")).write_text(json.dumps(row, indent=2))
                # Only exact synthetic payloads created by this completed case are
                # removed; their hashes/results and deterministic fixture remain.
                for path in paths:
                    if path.parent != self.root or not path.is_file() or path.is_symlink():
                        raise ValueError("unexpected evidence payload identity")
                    path.unlink()
            finally:
                if writer and writer.poll() is None:
                    self.remove(container)
                    writer.communicate(timeout=15)
                if container in self.active:
                    self.remove(container)
        self.protected_inputs()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-parent", required=True, type=Path)
    parser.add_argument("--experiment-id", required=True)
    parser.add_argument("--image-id", required=True)
    parser.add_argument("--agent", required=True, type=Path)
    parser.add_argument("--cold-bytes", type=int, default=256 * 1024 * 1024)
    args = parser.parse_args()
    if not full.NAME.fullmatch(args.experiment_id) or len(args.experiment_id) > 36:
        parser.error("bounded safe experiment identity required")
    if not 8192 <= args.cold_bytes <= 256 * 1024 * 1024:
        parser.error("fixture must be 8 KiB..256 MiB")
    parent = full.fixture.checked_root(args.evidence_parent)
    stat = os.statvfs(parent)
    mount = json.loads(subprocess.check_output(["findmnt", "--json", "--target", str(parent),
        "--output", "TARGET,SOURCE,FSTYPE"]))["filesystems"][0]
    if (mount["target"] != str(parent) or not mount["source"].startswith("/dev/loop")
            or stat.f_blocks * stat.f_frsize > full.MAX_DISK_BYTES):
        parser.error("dedicated loop filesystem bounded to 8 GiB required")
    root = parent / ("conditional-" + args.experiment_id)
    root.mkdir(mode=0o700, exist_ok=False)
    probe = ConditionalProbe(root, "c-conditional-" + args.experiment_id, args.image_id,
        args.agent.resolve(), Path(full.fixture.__file__).resolve(), args.cold_bytes)
    (root / "manifest.json").write_text(json.dumps({"schema": "centaeris.conditionalSnapshotProbe.v1",
        "imageId": args.image_id, "agentSha256": full.file_hash(args.agent), "coldBytes": args.cold_bytes,
        "harnessSha256": full.file_hash(Path(__file__)), "baseHarnessSha256": full.file_hash(Path(full.__file__)),
        "fixtureSha256": full.file_hash(Path(full.fixture.__file__)), "mount": mount,
        "scope": "actual helper stdout and isolated restores; Runtime spool covered separately by Runtime tests",
        "notMeasured": ["physicalDeviceIo", "runtimeUploads", "apiCommit", "apiGc", "coldCache", "transportProtocolOverhead"]}, indent=2))
    failure = None
    try:
        probe.run()
    except BaseException as error:
        failure = {"type": type(error).__name__, "message": str(error)[:1500]}
        if not isinstance(error, Exception):
            raise
    finally:
        cleanup = []
        for name in list(probe.pending):
            try:
                probe.reconcile_start(name)
            except Exception as error:
                cleanup.append(str(error)[:500])
        for container in list(probe.active):
            try:
                probe.remove(container)
            except Exception as error:
                cleanup.append(str(error)[:500])
        if failure is None and (cleanup or probe.pending or probe.active or len(probe.results) != len(WORKLOADS)):
            failure = {"type": "IncompleteExperiment", "message": "workloads or owned cleanup incomplete"}
        (root / "commands.json").write_text(json.dumps(probe.commands, indent=2))
        (root / "observations.jsonl").write_text("".join(json.dumps(row) + "\n" for row in probe.events))
        outcome = {"status": "failed" if failure else "passed", "failure": failure,
            "completedWorkloads": len(probe.results), "remainingOwnedContainers": list(probe.active),
            "unresolvedContainerNames": sorted(probe.pending), "cleanupFailures": cleanup}
        (root / "outcome.json").write_text(json.dumps(outcome, indent=2))
    print(json.dumps({"evidence": str(root), **outcome}))
    return int(failure is not None)


if __name__ == "__main__":
    raise SystemExit(main())
