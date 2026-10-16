"""Validate this research bundle's integrity and evidence relationships, offline."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import subprocess

from export_tables import read, render


HEAD = "25d3a95087a7662eb67610b235440a0b6c6a8264"
PRODUCT = "735b1c2ec651c095f8efb3dccfafb948f2e881c6"
LIMIT = 512 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def check_task_limits(d):
    store = d.get("store") or {}
    if store:
        require(0 <= store["activeTasks"] <= 32, "active task limit")
        require(0 <= store["hotTasks"] <= 64, "hot record limit")
        require(0 <= store["controlDirectoryBytes"] <= 8 * 1024 * 1024, "control storage limit")
        require(0 <= store["outputDirectoryBytes"] <= 72 * 1024 * 1024, "output storage limit")


def verify(root, source_root=None):
    root = Path(root)
    inventory = read(root, "inventory.json")
    records = inventory["files"]
    names = [r["path"] for r in records]
    require(len(names) == len(set(names)), "duplicate inventory entry")
    require(inventory["selfExcluded"] == "inventory.json", "inventory self exclusion")
    actual = set()
    for path in root.rglob("*"):
        require(not path.is_symlink(), "symlink in evidence")
        if path.is_file() and path.relative_to(root).as_posix() != "inventory.json":
            actual.add(path.relative_to(root).as_posix())
    require(actual == set(names), "missing or unregistered file")
    for record in records:
        path = PurePosixPath(record["path"])
        require(not path.is_absolute() and ".." not in path.parts and path.as_posix() == record["path"], "unsafe inventory path")
        data = (root / record["path"]).read_bytes()
        require(len(data) == record["bytes"] and sha(data) == record["sha256"], "file size/hash: " + record["path"])
    provenance = read(root, "provenance.json")
    for record in provenance["files"]:
        data = (root / record["exportPath"]).read_bytes()
        require(sha(data) == record["exportSha256"] and len(data) == record["exportBytes"], "provenance/export mismatch")
        if not record["transforms"]:
            require(record["originalSha256"] == record["exportSha256"], "unrecorded export transformation")

    bundle = read(root, "bundle.json")
    require(bundle["implementationHead"] == HEAD and bundle["productEvidenceSource"] == PRODUCT, "source identity")
    require(bundle["runtimeMemoryLimitBytes"] == LIMIT and bundle["modelContextTokens"] == 131072, "experiment configuration")
    fingerprints = read(root, "validation/source-fingerprints.json")
    require(fingerprints["sourceRevision"] == HEAD, "source fingerprint scope")
    paths = [r["path"] for r in fingerprints["files"]]
    require(len(paths) == len(set(paths)), "duplicate source fingerprint")
    for r in fingerprints["files"]:
        require(re.fullmatch(r"[0-9a-f]{64}", r["sha256"]) and re.fullmatch(r"[0-9a-f]{40}", r["gitBlob"]), "source digest syntax")
    if source_root is not None:
        source_root = Path(source_root)
        commit = subprocess.check_output(["git", "-C", str(source_root), "rev-parse", "HEAD"], text=True).strip()
        require(commit == HEAD, "source checkout HEAD")
        for r in fingerprints["files"]:
            # Git applies the checkout's declared filters, including CRLF on Windows.
            blob = subprocess.check_output(["git", "-C", str(source_root), "hash-object", "--path", r["path"], r["path"]], text=True).strip()
            require(blob == r["gitBlob"], "source checkout content: " + r["path"])
            data = subprocess.check_output(["git", "-C", str(source_root), "cat-file", "blob", blob])
            require(len(data) == r["bytes"] and sha(data) == r["sha256"], "source canonical digest: " + r["path"])

    cross = "data/ab-and-100-uses/"
    cross_summary = read(root, cross + "summary.json")
    require(cross_summary["uses"] == cross_summary["completedRuns"] == 100 and cross_summary["failedRuns"] == 0, "100 uses")
    files = [root/cross/"observed-a.json", root/cross/"observed-b.json"]
    files += sorted((root/cross).glob("observed-use-*.json"), key=lambda p: int(p.stem.split("-")[-1]))
    observations = [json.loads(p.read_text()) for p in files]
    require(len(observations) == 100, "100 per-Run observations")
    first, second = observations[:2]
    require(first["runtimeStartup"]["hostPid"] != second["runtimeStartup"]["hostPid"] and
            first["runtimeStartup"]["startedAt"] != second["runtimeStartup"]["startedAt"], "actual A/B Runtime restart")
    ids, releases, previous_epoch = set(), set(), -1
    residency = read(root, cross + "resident-observations.json")
    resident_by_id = {r["observation"]["agentRunId"]: r["observation"] for r in residency}
    require(len(residency) == len(resident_by_id) == 100, "exact resident observation identities")
    for d in observations:
        for k in ["containerId", "instanceId", "sessionId", "environmentId", "fileSha256", "backgroundTaskIds"]:
            require(d[k] == first[k], "same live instance/task/file: " + k)
        require(d["runStatus"] == "completed" and d["releaseReceiptCount"] == 1 and d["cleanupComplete"] is True, "Run cleanup/release")
        require(d["fence"]["controllerEpoch"] > previous_epoch, "monotonic controller epoch")
        previous_epoch = d["fence"]["controllerEpoch"]
        require(d["agentRunId"] not in ids and d["releaseReceiptId"] not in releases, "duplicate Run/release")
        ids.add(d["agentRunId"]); releases.add(d["releaseReceiptId"])
        require(resident_by_id[d["agentRunId"]]["residentCount"] == 1, "one resident holding")
        check_task_limits(d)
    require(second["oldControllerRejected"] and second["staleTerminationRejected"], "stale controller rejection")

    long = "data/long-run-1000-final/"
    summary = read(root, long + "summary.json")
    work = summary["longRunObservation"]
    require(work["attemptedCommands"] == work["completedCommands"] == work["distinctCallIds"] == 1000, "1000 exact successful operations")
    effects = (root/long/"side-effects.txt").read_bytes()
    require(effects == "".join(f"{i:04d}\n" for i in range(1000)).encode(), "exact side-effect sequence")
    proof = read(root, long + "long-side-effect-proof.json")
    require(proof["sha256"] == sha(effects) and proof["bytes"] == len(effects) and proof["lines"] == 1000 and proof["exactSequence"], "side-effect proof")
    instructions = [json.loads(s) for s in (root/long/"automatic-instructions.jsonl").read_text().splitlines()]
    require(len(instructions) == summary["automaticInstructionsObservation"]["observations"] == 1000 and
            {r["commandIndex"] for r in instructions} == set(range(1000)), "AGENTS observation identities")
    require({r["instructionSha256"] for r in instructions} == {summary["automaticInstructionsObservation"]["instructionSha256"]}, "AGENTS content")
    restart = read(root, long + "long-runtime-restart.json")
    require(restart["before"]["Pid"] != restart["after"]["Pid"] and restart["before"]["StartedAt"] != restart["after"]["StartedAt"], "actual long Runtime restart")
    reclaim = restart["controllerReclaim"]
    require(reclaim["afterFence"]["controllerEpoch"] > reclaim["beforeFence"]["controllerEpoch"] and reclaim["backgroundStillRunning"], "takeover continuity")
    require(set(reclaim["rejections"]) == {"list", "start", "input", "terminate", "endRun"}, "stale operation identities")
    require(all(r["error"] == "task_controller_fenced" for r in reclaim["rejections"].values()), "stale operation result")
    before = read(root, long + "long-compaction-before-restart.json")["durableCompaction"]
    recovered = restart["compactionRecovery"]
    for key in ["compactionId", "summaryMessageId", "summarySha256"]:
        require(before[key] == recovered[key], "original summary reuse: " + key)
    facts = read(root, long + "compaction-observation.json")["facts"]
    require(facts["compactionRequests"] == facts["compactionCommits"] == 1 and facts["failureCount"] == 0, "compaction commit facts")
    for d in read(root, long + "long-live-samples.json")["retainedSamples"]:
        check_task_limits(d)
    analysis = read(root, long + "memory-analysis.json")
    require(bundle["strictMemoryGate"] == analysis["status"] == "incomplete-or-failed", "preserve strict memory gate result")
    require(not analysis["oomEvents"] and analysis["unattributedSignalExits"], "preserve stop attribution evidence")
    for line in (root/long/"runtime-memory.jsonl").read_text().splitlines():
        d = json.loads(line)
        require(d.get("memoryLimitBytes", LIMIT) == LIMIT, "Runtime memory limit")
        for value in [(d.get("memory") or {}).get("current"), (d.get("process") or {}).get("RssAnonBytes")]:
            require(value is None or 0 <= value <= LIMIT, "sample above Runtime memory limit")

    for directory, s in [(cross, cross_summary), (long, summary)]:
        require(s["containerAbsent"] and s["maintenanceResponseDiscarded"], "physical absence and lost response evidence")
        request, response = read(root, directory+"retirement-request.json"), read(root, directory+"retirement-retry.json")
        require(request["operationId"] == response["operationId"] and response["status"] == "destroyed", "retirement operation identity")
        maintenance = read(root, directory+"maintenance-observations.json")
        require(len(maintenance) == 1, "one maintenance ledger result")
        ledger = maintenance[0]["observation"]
        require(ledger["status"] == "destroyed" and ledger["residentHoldingCount"] == 0 and
                ledger["maintenanceReceiptCount"] == 1 and ledger["historyPreserved"], "destruction accounting")
        inventories = read(root, directory+"container-inventories.json")
        require(inventories, "physical container inventory")
        observed = read(root, directory+"observed-a.json")
        require(all(observed["containerId"] not in i["containerIds"] for i in inventories), "destroyed container still present")

    for experiment in bundle["experiments"]:
        if experiment["id"].endswith("-oom"):
            require(experiment["classification"] == "historicalFailureBaseline", "historical failure relabeled")
        elif experiment["id"].endswith("-stopped"):
            require(experiment["classification"] == "stoppedNonPass", "stopped run relabeled")
    old = read(root, "data/historical-oom-baselines/run-708-c0349f0-oom/status.json")
    require(old["status"] == "failed" and old["completedCommands"] == 708 and
            old["postLongRestart"]["processAnonPeakBytes"] > old["preLongRestart"]["processAnonPeakBytes"], "708 failure baseline")
    oom = read(root, "data/historical-oom-baselines/run-901-oom/pre-oom-evidence.json")
    require(oom["state"]["OOMKilled"] and oom["state"]["ExitCode"] == 137, "901 OOM evidence")
    pg = read(root, "validation/pg-identities.json")
    required = read(root, "validation/required-pg-identities.json")
    require(pg["exitCode"] == 0 and Counter(pg["selected"]) == Counter(pg["executed"]) and
            all(v == 1 for v in Counter(pg["executed"]).values()), "PG exact selected/executed identities")
    require(set(required) == set(pg["required"]) and set(required) <= set(pg["executed"]), "required PG identities")
    native = read(root, "validation/warm-binary-proof.json")
    require(native["pythonCandidate"] == HEAD and native["rustProductSource"] == PRODUCT and len(native["nativeRuns"]) == 2,
            "follow-up Native evidence identity")
    require(all(any("1 passed; 0 failed" in result for result in run["results"]) for run in native["nativeRuns"]), "Native server execution")
    by_path = {r["path"]: r for r in fingerprints["files"]}
    require(all(by_path[path]["sha256"] == value for path, value in native["testFiles"].items()), "Native Python source binding")
    original = read(root, "validation/final-ignored-inventory.json")
    reconciled = read(root, "validation/ignored-identities-reconciled.json")
    require(set(native["ignoredIdentities"]) == set(original["allIgnoredIdentities"]), "ignored identity discovery parity")
    covered = set(original["pgExecutedIdentities"]) | set(original["apiNativeExecutedIdentities"]) | set(native["passedNativeIdentities"])
    require(set(reconciled["coveredIdentities"]) == covered and
            set(reconciled["remainingUnexecutedIdentities"]) == set(native["ignoredIdentities"]) - covered,
            "ignored evidence reconciliation")
    require(set(bundle["pendingIntegratedScenarios"]) == set(summary["unrun"]) == set(cross_summary["unrun"]), "pending fault scenarios")
    for r in read(root, "data/core-turn-scaling/observations.json"):
        if "retainedFullSnapshots" in r:
            require(r["retainedFullSnapshots"] == 1, "Core snapshot lifetime")
    for name, data in render(root).items():
        require((root/"tables"/name).read_bytes() == data, "derived table mismatch: " + name)
    ci = read(root, "validation/ci-status.json")
    require(ci["sourceRevision"] == HEAD and ci["runId"] == 38942094515, "CI source binding")
    if ci["conclusion"] == "success":
        jobs = ci.get("jobs", [])
        require(ci["status"] == "completed" and jobs and
                any(j["name"] == "Required gates" and j["conclusion"] == "success" for j in jobs) and
                all(j["status"] == "completed" and j["conclusion"] in {"success", "skipped"} for j in jobs),
                "CI result completeness")
    return {"files": len(records), "uses": 100, "commands": 1000, "requiredPgIdentities": len(required),
            "strictMemoryGate": bundle["strictMemoryGate"], "pendingFaultScenarios": len(summary["unrun"]),
            "ciConclusion": ci["conclusion"], "sourceCheckoutCompared": source_root is not None}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, help="Also compare recorded source digests to the exact implementation checkout.")
    args = parser.parse_args()
    print(json.dumps(verify(Path(__file__).resolve().parent, args.source_root), indent=2))
