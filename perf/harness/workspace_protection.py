"""Prepare bounded synthetic workloads and account for observed protection work.

This module does not model unobserved I/O as zero or initiate snapshot capture.
Run the current product capture path against the prepared fixtures and summarize
its observation records. Every workload starts with the same synthetic bytes.
"""
import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import time

WORKLOADS = ("unchanged", "hot-file", "large-overwrite", "append-log",
             "rename-delete-revert", "background-write")
COUNTERS = {
    "scannedEntries": "count", "sourceReadBytes": "bytes", "snapshotLogicalBytes": "bytes",
    "runtimeSpoolWrittenBytes": "bytes", "apiReceivedBytes": "bytes",
    "apiWrittenBytes": "bytes", "apiVerificationReadBytes": "bytes",
    "uploadAttempts": "count", "uploadedBytes": "bytes", "publishedLinks": "count",
    "committedSnapshots": "count", "gcReclaimedBytes": "bytes",
}
GAUGES = {"temporaryBytes": "bytes", "retainedBytes": "bytes"}
COUNTER_SOURCES = {metric: ("api" if metric.startswith("api") or metric in
                          {"publishedLinks", "committedSnapshots", "gcReclaimedBytes"}
                          else "sandbox" if metric in {"scannedEntries", "sourceReadBytes"}
                          else "runtime") for metric in COUNTERS}
TIMINGS = {"captureElapsedMs", "uploadElapsedMs", "commitElapsedMs",
           "restoreDownloadElapsedMs", "restoreApplyElapsedMs", "gcElapsedMs"}
TIMING_FIELDS = ("source", "operationId", "metric", "value", "outcome", "trigger",
                 "sessionId", "runId", "jobId", "environmentId")
FIELDS = {"schema", "source", "phase", "operationId", "metric", "value", "outcome",
          "atUnixMs", "environmentId", "sessionId", "runId", "jobId", "trigger"}
IDENTITY = re.compile(r"[A-Za-z0-9_.:-]{1,160}\Z")


def validate_event(event):
    if not isinstance(event, dict) or set(event) - FIELDS:
        raise ValueError("unknown observation fields")
    if event.get("schema") != "workspace.perf.v1" or event.get("phase") != "workspaceProtection":
        raise ValueError("unsupported protection observation")
    if event.get("source") not in {"api", "runtime", "sandbox"}:
        raise ValueError("unsupported observation source")
    for key in ("operationId", "environmentId", "sessionId", "runId", "jobId", "trigger"):
        value = event.get(key)
        if key == "operationId" and value is None:
            raise ValueError("operation identity is required")
        if value is not None and (not isinstance(value, str) or not IDENTITY.fullmatch(value)):
            raise ValueError("invalid observation identity")
    metric, value = event.get("metric"), event.get("value")
    if metric not in COUNTERS and metric not in GAUGES and metric not in TIMINGS:
        raise ValueError("unsupported protection metric")
    owner = (COUNTER_SOURCES[metric] if metric in COUNTERS else
             "api" if metric in GAUGES or metric in {"commitElapsedMs", "gcElapsedMs"} else "runtime")
    if event["source"] != owner:
        raise ValueError("metric does not belong to the stated source")
    if "atUnixMs" in event and (type(event["atUnixMs"]) is not int or
                               not 0 <= event["atUnixMs"] <= 2 ** 63 - 1):
        raise ValueError("invalid observation timestamp")
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise ValueError("invalid protection measurement")
    if metric not in TIMINGS and type(value) is not int:
        raise ValueError("byte/count measurements must be integers")
    if event.get("outcome") not in {None, "ok", "error", "interrupted"}:
        raise ValueError("invalid measurement outcome")
    return event


def summarize(events):
    counters = dict.fromkeys(COUNTERS)
    gauges, timings, operations = {}, [], {}
    count = 0
    for event in events:
        validate_event(event)
        count += 1
        metric, value = event["metric"], event["value"]
        operation = f'{event["source"]}:{event["operationId"]}'
        operations.setdefault(operation, set()).add(metric)
        if metric in COUNTERS:
            counters[metric] = (counters[metric] or 0) + value
        elif metric in GAUGES:
            # Separate scopes: repeated occupancy samples are not cumulative I/O.
            scope = f'{event["source"]}:{event["operationId"]}:{metric}'
            old = gauges.get(scope, {"peak": 0})
            gauges[scope] = {"last": value, "peak": max(old["peak"], value)}
        else:
            # Keep nested stages independent. Summing them invents elapsed time.
            timings.append({key: event[key] for key in TIMING_FIELDS if key in event})
    if not count:
        raise ValueError("no protection observations; missing sampling is not zero work")
    coverage = {metric: {
        "observedOperations": sorted(key for key, seen in operations.items() if metric in seen),
        "unmeasuredOperations": sorted(key for key, seen in operations.items()
                                       if key.startswith(COUNTER_SOURCES[metric] + ":") and metric not in seen),
    } for metric in COUNTERS}
    return {"recordCount": count, "counters": counters, "counterCoverage": coverage, "gauges": gauges,
            "timings": timings, "missingCounters": [key for key, value in counters.items() if value is None]}


def checked_root(root):
    root = Path(os.path.abspath(root))
    for entry in (root, *root.parents):
        if not entry.exists():
            continue
        metadata = entry.lstat()
        if stat.S_ISLNK(metadata.st_mode) or getattr(metadata, "st_file_attributes", 0) & 0x400:
            raise ValueError("workload paths must not contain aliases")
    return root


def tree_manifest(root):
    root = checked_root(root)
    records = []
    for path in sorted(root.iterdir()):
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise ValueError("fixture must contain only owned regular files")
        digest = hashlib.sha256()
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
        records.append({"name": path.name, "sizeBytes": metadata.st_size, "sha256": digest.hexdigest()})
    encoded = json.dumps(records, sort_keys=True, separators=(",", ":")).encode()
    return {"files": records, "treeSha256": hashlib.sha256(encoded).hexdigest()}


def prepare_workload(root, cold_bytes=256 * 1024 * 1024):
    if type(cold_bytes) is not int or not 8192 <= cold_bytes <= 512 * 1024 * 1024:
        raise ValueError("cold data must be 8 KiB..512 MiB")
    root = checked_root(root)
    root.mkdir(parents=False, exist_ok=False)
    block = bytes(range(256)) * 4096
    with (root / "cold.bin").open("xb") as target:
        remaining = cold_bytes
        while remaining:
            size = min(remaining, len(block))
            target.write(block[:size])
            remaining -= size
    (root / "hot.bin").write_bytes(b"h" * 4096)
    (root / "append.log").write_bytes(b"baseline\n")
    return tree_manifest(root)


def mutate_workload(root, name):
    root = checked_root(root)
    manifest = tree_manifest(root)
    if {row["name"] for row in manifest["files"]} != {"cold.bin", "hot.bin", "append.log"}:
        raise ValueError("unexpected fixture contents")
    if name not in WORKLOADS:
        raise ValueError("unsupported workload")
    if name == "hot-file":
        (root / "hot.bin").write_bytes(b"H" * 4096)
    elif name == "large-overwrite":
        with (root / "cold.bin").open("r+b") as target:
            target.seek(4096)
            target.write(b"X" * 4096)
    elif name in {"append-log", "background-write"}:
        with (root / "append.log").open("ab") as target:
            target.write(b"append\n" * 512)
    elif name == "rename-delete-revert":
        original = (root / "hot.bin").read_bytes()
        (root / "hot.bin").rename(root / "renamed.bin")
        (root / "renamed.bin").unlink()
        (root / "hot.bin").write_bytes(original)


def background_writer(root, seconds):
    """Run in a separate process while the caller captures the synthetic fixture."""
    root = checked_root(root)
    if not 0 < seconds <= 60:
        raise ValueError("writer duration must be 0..60 seconds")
    # Validate before opening, never follow a workload-created alias.
    mutate_workload(root, "background-write")
    deadline, count = time.monotonic() + seconds, 0
    with (root / "append.log").open("ab") as output:
        while time.monotonic() < deadline:
            output.write(b"background\n" * 128)
            output.flush()
            count += 1
            time.sleep(0.01)
    return count


def write_summary(records, output):
    result = summarize(records)
    output = checked_root(output)
    output.mkdir(parents=False, exist_ok=False)
    (output / "summary.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    with (output / "io-and-transfer.csv").open("x", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(["metric", "kind", "unit", "value", "status"])
        for metric, value in result["counters"].items():
            status = ("not-measured" if value is None else "partial" if
                      result["counterCoverage"][metric]["unmeasuredOperations"] else "observed")
            writer.writerow([metric, "counter", COUNTERS[metric], value, status])
    with (output / "restore-phases.csv").open("x", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=TIMING_FIELDS)
        writer.writeheader()
        writer.writerows(result["timings"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    seed = commands.add_parser("prepare")
    seed.add_argument("--root", type=Path, required=True)
    seed.add_argument("--cold-bytes", type=int, default=256 * 1024 * 1024)
    mutate = commands.add_parser("mutate")
    mutate.add_argument("--root", type=Path, required=True)
    mutate.add_argument("--workload", choices=WORKLOADS, required=True)
    writer = commands.add_parser("background-write")
    writer.add_argument("--root", type=Path, required=True)
    writer.add_argument("--seconds", type=float, default=10)
    summary = commands.add_parser("summarize")
    summary.add_argument("--events", type=Path, required=True)
    summary.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.command == "prepare":
        print(json.dumps(prepare_workload(args.root, args.cold_bytes)))
    elif args.command == "mutate":
        mutate_workload(args.root, args.workload)
    elif args.command == "background-write":
        print(json.dumps({"writes": background_writer(args.root, args.seconds)}))
    else:
        with args.events.open(encoding="utf-8") as handle:
            rows = [json.loads(line) for line in handle if line.strip()]
        print(json.dumps(write_summary(rows, args.output)))


if __name__ == "__main__":
    main()
