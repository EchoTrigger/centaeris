"""Regenerate public tables from the included observations; Python standard library."""
import argparse
import csv
import io
import json
from pathlib import Path


def read(root, relative):
    return json.loads((root / relative).read_text(encoding="utf-8-sig"))


def csv_bytes(fields, rows):
    stream = io.StringIO(newline="")
    writer = csv.DictWriter(stream, fieldnames=fields)
    writer.writeheader()
    writer.writerows(rows)
    return stream.getvalue().encode("utf-8")


def render(root):
    result = {}
    cross = "data/ab-and-100-uses/"
    residency = {r["observation"]["agentRunId"]: r["observation"]["residentCount"]
                 for r in read(root, cross + "resident-observations.json")}
    paths = [root / cross / "observed-a.json", root / cross / "observed-b.json"]
    paths += sorted((root / cross).glob("observed-use-*.json"), key=lambda p: int(p.stem.split("-")[-1]))
    fields = ["ordinal", "agentRunId", "containerId", "instanceId", "environmentTaskId", "fileSha256",
              "controllerEpoch", "runStatus", "releaseReceiptId", "releaseReceiptCount", "cleanupComplete",
              "activeTasks", "hotRecords", "controlDirectoryBytes", "outputDirectoryBytes", "residentCount"]
    rows = []
    for i, path in enumerate(paths):
        d = json.loads(path.read_text())
        rows.append({"ordinal": i, **{k: d[k] for k in ["agentRunId", "containerId", "instanceId", "fileSha256",
                     "runStatus", "releaseReceiptId", "releaseReceiptCount", "cleanupComplete"]},
                     "environmentTaskId": d["backgroundTaskIds"][0], "controllerEpoch": d["fence"]["controllerEpoch"],
                     "activeTasks": d["store"]["activeTasks"], "hotRecords": d["store"]["hotTasks"],
                     "controlDirectoryBytes": d["store"]["controlDirectoryBytes"],
                     "outputDirectoryBytes": d["store"]["outputDirectoryBytes"], "residentCount": residency[d["agentRunId"]]})
    result["lifecycle_runs.csv"] = csv_bytes(fields, rows)

    long = "data/long-run-1000-final/"
    fields = ["completedCommands", "attemptedCommands", "agentRunId", "instanceId", "containerId", "controllerEpoch",
              "activeTasks", "hotRecords", "controlDirectoryBytes", "outputDirectoryBytes"]
    rows = []
    for d in read(root, long + "long-live-samples.json")["retainedSamples"]:
        store = d.get("store") or {}
        rows.append({**{k: d.get(k, "") for k in fields[:5]}, "controllerEpoch": d.get("fence", {}).get("controllerEpoch", ""),
                     "activeTasks": store.get("activeTasks", ""), "hotRecords": store.get("hotTasks", ""),
                     "controlDirectoryBytes": store.get("controlDirectoryBytes", ""),
                     "outputDirectoryBytes": store.get("outputDirectoryBytes", "")})
    result["long_run_1000_steps.csv"] = csv_bytes(fields, rows)

    analysis = read(root, long + "memory-analysis.json")
    segments = {(s["containerId"], s["pid"], s["startedAt"]): i for i, s in enumerate(analysis["processSegments"])}

    def memory_rows(case, classification, path):
        for line in path.read_text(encoding="utf-8-sig").splitlines():
            d = json.loads(line)
            state, memory, process = d.get("state") or {}, d.get("memory") or {}, d.get("process") or {}
            key = (d.get("containerId"), state.get("Pid"), state.get("StartedAt"))
            if case == "final":
                segment = ["startup", "pre-restart", "post-restart"][segments[key]] if key in segments else "unobserved"
            else:
                segment = "lifetime:" + str(state.get("Pid", "unknown")) + ":" + str(d.get("startTicks", "unknown"))
            yield {"runId": case, "classification": classification, "timeUnixNs": d.get("timeUnixNs", ""),
                   "segment": segment, "containerId": d.get("containerId", ""), "pid": state.get("Pid", ""),
                   "anonymousBytes": process.get("RssAnonBytes", ""), "rssBytes": process.get("VmRSSBytes", ""),
                   "cgroupCurrentBytes": memory.get("current", ""), "cgroupPeakBytes": memory.get("peak", ""),
                   "limitBytes": d.get("memoryLimitBytes", ""), "OOMKilled": state.get("OOMKilled", ""),
                   "exitCode": state.get("ExitCode", "")}

    rows = list(memory_rows("final", "physicalChainPassedStrictMemoryOpen", root / long / "runtime-memory.jsonl"))
    for case in ["run-708-c0349f0-oom", "run-708-cb238d0-oom", "run-05555f5-stopped"]:
        rows += list(memory_rows(case, "stoppedNonPass" if "stopped" in case else "historicalFailureBaseline",
                                 root / "data/historical-oom-baselines" / case / "runtime-memory.jsonl"))
    result["memory_ablation_series.csv"] = csv_bytes(list(rows[0]), rows)
    result["fault_and_fence_matrix.csv"] = csv_bytes(["scenario", "finalIntegratedStatus", "source"],
        [{"scenario": name, "finalIntegratedStatus": "unrun", "source": long + "summary.json#unrun"}
         for name in read(root, long + "summary.json")["unrun"]])
    scaling = {}
    for row in read(root, "data/core-turn-scaling/observations.json"):
        scaling.setdefault(row["rounds"], {}).update(row)
    result["core_turn_scaling.csv"] = csv_bytes(list(scaling[100]), [scaling[n] for n in [100, 1000, 10000]])
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="Replace derived tables; inventory must then be regenerated.")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    for name, data in render(root).items():
        path = root / "tables" / name
        if args.write:
            path.write_bytes(data)
        elif path.read_bytes() != data:
            raise SystemExit("Table differs from included sources: " + name)
    print("Six tables reproduce exactly.")
