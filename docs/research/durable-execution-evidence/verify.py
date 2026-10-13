"""Offline evidence validation and deterministic CSV projection; never runs a probe."""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path


def read(path):
    return json.loads(path.read_text(encoding="utf-8"))


def require(condition, message):
    if not condition:
        raise ValueError(message)


def counters(directory):
    rows = [json.loads(line) for line in (directory / "observations.jsonl").read_text().splitlines()]
    result = {}
    for row in rows:
        key = (row["operationId"], row["metric"])
        require(key not in result, f"duplicate observation: {key}")
        require(row["source"] == "sandbox", "unexpected observation source")
        result[key] = row
    return result


def operations(root):
    rows = []
    for dataset in ("full-snapshot", "conditional-snapshot"):
        directory = root / "data" / dataset
        measured = counters(directory)
        used = set()

        def add(workload, operation_id, capture, selected=None, restore=None):
            values = {}
            for metric in ("sourceReadBytes", "scannedEntries"):
                key = (operation_id, metric)
                require(key in measured, f"missing observation: {dataset}/{key}")
                observation = measured[key]
                require(observation["outcome"] == ("ok" if capture["exitCode"] == 0 else "error"),
                        f"outcome mismatch: {key}")
                values[metric] = observation["value"]
                used.add(key)
            if capture["exitCode"] == 0 and selected:
                require(values["sourceReadBytes"] == 2 * selected["expandedSizeBytes"],
                        f"two-pass byte mismatch: {dataset}/{operation_id}")
            rows.append({"dataset": dataset, "workload": workload, "operationId": operation_id,
                         "exitCode": capture["exitCode"],
                         "disposition": capture.get("disposition", "full" if capture["exitCode"] == 0 else "rejected"),
                         "stdoutBytes": capture.get("helperOutputBytes", capture.get("captureFrameBytes")),
                         "inputRequestBytes": (directory / (operation_id + ".request.json")).stat().st_size
                         if (directory / (operation_id + ".request.json")).exists() else None,
                         "sourceReadBytes": values["sourceReadBytes"], "scannedEntries": values["scannedEntries"],
                         "expandedBytes": selected["expandedSizeBytes"] if selected else None,
                         "restoreStatus": restore.get("status") if restore else None,
                         "externalCaptureElapsedMs": capture["externalCaptureElapsedMs"]})

        for i in range(6):
            data = read(directory / f"{i}-result.json")
            base = data["baseline"]
            add(data["workload"], f"{i}-baseline", base, base["inspection"], base.get("restore"))
            if dataset == "full-snapshot":
                after = data["after"]
                add(data["workload"], f"{i}-after", after, after["inspection"], after.get("restore"))
                for capture in (base, after):
                    if capture["exitCode"] == 0:
                        require(capture["inspection"]["files"] == capture["restore"]["tree"]["files"],
                                "full snapshot restored file identities differ")
            else:
                conditional = data["conditional"]
                selected = (conditional.get("inspection") or base["inspection"]) if conditional["exitCode"] == 0 else None
                add(data["workload"], f"{i}-conditional", conditional, selected, data["restore"])
                if "forcedFull" in data:
                    forced = data["forcedFull"]
                    add(data["workload"], f"{i}-forced", forced, forced["inspection"])
                    require(selected == forced["inspection"], "conditional selection differs from full capture")
                    require(selected["files"] == data["restore"]["tree"]["files"], "conditional restore differs")
                    if conditional["disposition"] == "unchanged":
                        require(conditional["snapshotFrameBytes"] == 0, "unchanged receipt contains a frame")
                        require(conditional["receipt"]["baselineSnapshotSha256"] == "sha256:" + base["inspection"]["frameSha256"],
                                "unchanged receipt bound to wrong baseline")
        if dataset == "conditional-snapshot":
            protected = read(directory / "protected-result.json")
            base = protected["baseline"]
            add(protected["workload"], "protected-baseline", base, base["inspection"])
            for check in protected["checks"]:
                for key, suffix in (("conditional", "conditional"), ("forcedFull", "forced")):
                    capture = check[key]
                    selected = capture.get("inspection") or base["inspection"]
                    add(protected["workload"], check["operation"] + "-" + suffix, capture, selected)
                    require(selected["files"] == protected["restore"]["tree"]["files"], "protected restore differs")
            require(len(measured) == 44, "conditional observation count differs")
            require(read(directory / "outcome.json")["completedWorkloads"] == 7, "conditional workload count differs")
        require(used == set(measured), f"unjoined observations in {dataset}")
    require(len(rows) == 34, "capture operation count differs")
    return rows


def csv_text(rows):
    output = io.StringIO(newline="")
    writer = csv.DictWriter(output, fieldnames=list(rows[0]), lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
    return output.getvalue()


def process_evidence(directory):
    lifecycle = read(directory / "source-exit-evidence.json")
    running, stopped = lifecycle["sourceRunning"], lifecycle["sourceStopped"]
    require(running["id"] == stopped["id"] and running["status"] == "running" and running["pid"] > 0
            and running["hostStartTicks"].isdigit() and stopped["status"] == "stopped" and stopped["pid"] == -1,
            "source exit identity or state differs")
    steps = lifecycle["steps"]
    require(all(type(item["exitCode"]) is int and item["exitCode"] == 0 for item in steps), "source exit command failed")
    checkpoint = next(item for item in steps if item["operation"] == "checkpoint")
    deletion = next(item for item in steps if item["operation"] == "delete" and item["instanceId"] == running["id"])
    restoration = next(item for item in steps if item["operation"] == "restore")
    require(checkpoint["commandIndex"] < deletion["commandIndex"] < restoration["commandIndex"]
            and restoration["instanceId"] != running["id"], "source exit must precede distinct-instance restore")
    integer_fields = {"counter", "fileCounter", "fdOffset", "unlinkedOffset", "pipeBytes", "childPid", "threadTicks"}
    fields = integer_fields | {"nonce", "mmapNonce", "paused"}
    oracles = [read(directory / f"oracle-{stage}.json") for stage in ("before", "after", "resumed")]
    for oracle in oracles:
        require(set(oracle) == fields, "oracle fields differ")
        require(oracle["paused"] is True, "oracle was not paused at observation")
        require(all(type(oracle[key]) is int and oracle[key] >= 0 for key in integer_fields), "oracle integer type differs")
        require(all(type(oracle[key]) is str and len(oracle[key]) == 16 and
                    all(char in "0123456789abcdef" for char in oracle[key]) for key in ("nonce", "mmapNonce")),
                "oracle nonce type differs")
    before, after, resumed = oracles
    require(before == after, "restored process oracle differs")
    require(all(resumed[key] == before[key] for key in ("nonce", "mmapNonce", "childPid")), "resumed oracle identity differs")
    require(all(resumed[key] > before[key] for key in ("counter", "fileCounter", "threadTicks", "fdOffset", "unlinkedOffset"))
            and resumed["pipeBytes"] == 0, "restored process made no verified progress")
    descriptors = read(directory / "restored-fd-verification.json")
    require(set(descriptors) == {"pipeVerified", "fileVerified", "unlinkedVerified", "linksVerified"}
            and all(value is True for value in descriptors.values()), "restored descriptor checks incomplete or invalid")
    comparison = read(directory / "oracle-comparison.json")
    require(comparison == {"passed": True, "sourceStopped": True, "missingFields": [], "mismatchedFields": []}
            and comparison["passed"] is True and comparison["sourceStopped"] is True, "oracle comparison differs")
    outcome = read(directory / "outcome.json")
    require(outcome["status"] == "supported" and outcome["failure"] is None, "process outcome differs")
    cleanup = outcome["cleanup"]
    require(len(cleanup) == 4, "cleanup records missing")
    for record in cleanup:
        codes = [value for key, value in record.items() if key.endswith("ExitCode") or key == "exitCode"]
        require(len(codes) == 1 and type(codes[0]) is int and codes[0] == 0, "cleanup failed or malformed")
    matrix = read(directory / "capability-matrix.json")
    supported = {"memoryState", "processTreeAndPipe", "openFileCursor", "unlinkedOpenFile", "threadAndMmap",
                 "renameLinksPermissions", "quiescedBackgroundWrites", "controllerReconnect",
                 "truncatedCheckpoint", "wrongDigest", "runtimeMismatch"}
    unmeasured = {"unquiescedCapture", "crossHostRecovery", "externalConnections"}
    require(set(matrix) == supported | unmeasured, "capability fields differ")
    require(all(matrix[key]["status"] == "supported" for key in supported)
            and all(matrix[key]["status"] == "not-tested" for key in unmeasured), "capability exceeds recorded evidence")
    faults = [json.loads(line) for line in (directory / "fault-results.jsonl").read_text().splitlines()]
    require({item["case"] for item in faults} == {"truncatedCheckpoint", "wrongDigest", "runtimeMismatch"}
            and len(faults) == 3 and all(item["layer"] == "probe admission" and item["result"] == "rejected before restore"
                                       for item in faults), "capability fault layer differs")


def verify(root, write_table=False):
    provenance = read(root / "provenance.json")
    for entry in provenance["files"]:
        path = root / entry["path"]
        require(hashlib.sha256(path.read_bytes()).hexdigest() == entry["exportSha256"],
                f"export digest mismatch: {entry['path']}")
    table = csv_text(operations(root))
    target = root / "tables/operations.csv"
    if write_table:
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(table, encoding="utf-8", newline="")
    else:
        require(target.read_text(encoding="utf-8") == table, "CSV differs from raw observations")
    for suffix in ("", "guarded"):
        directory = root / "data/runsc-process" / suffix
        process_evidence(directory)
        retained = sum(item["sizeBytes"] for item in read(directory / "checkpoint-identity.json")["files"].values())
        with (directory / "io-and-transfer.csv").open(newline="") as file:
            actual = next(row for row in csv.DictReader(file) if row["measurement"] == "retainedLogicalBytes")
        require(int(actual["value"]) == retained, "checkpoint file sizes differ from retained bytes")
    if not write_table:
        inventory = read(root / "inventory.json")
        require(len({item["path"] for item in inventory["files"]}) == len(inventory["files"]), "duplicate inventory entry")
        for entry in inventory["files"]:
            require(type(entry["sizeBytes"]) is int and (root / entry["path"]).stat().st_size == entry["sizeBytes"],
                    f"inventory size mismatch: {entry['path']}")
            require(hashlib.sha256((root / entry["path"]).read_bytes()).hexdigest() == entry["sha256"],
                    f"bundle digest mismatch: {entry['path']}")
        files = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file() and "__pycache__" not in p.parts}
        require(files == {item["path"] for item in inventory["files"]} | {"inventory.json"}, "bundle inventory differs")
    print("Verified: 34 capture operations, 68 observations, matching restores and two process checkpoint oracles.")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write-table", action="store_true")
    args = parser.parse_args()
    verify(Path(__file__).resolve().parent, args.write_table)
