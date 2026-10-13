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
        require(read(directory / "oracle-before.json") == read(directory / "oracle-after.json"),
                "restored process oracle differs")
        require(all(read(directory / "restored-fd-verification.json").values()), "restored descriptor check failed")
        require(read(directory / "oracle-comparison.json")["sourceStopped"], "source was not stopped")
        retained = sum(item["sizeBytes"] for item in read(directory / "checkpoint-identity.json")["files"].values())
        with (directory / "io-and-transfer.csv").open(newline="") as file:
            actual = next(row for row in csv.DictReader(file) if row["measurement"] == "retainedLogicalBytes")
        require(int(actual["value"]) == retained, "checkpoint file sizes differ from retained bytes")
    if not write_table:
        inventory = read(root / "inventory.json")
        for entry in inventory["files"]:
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
