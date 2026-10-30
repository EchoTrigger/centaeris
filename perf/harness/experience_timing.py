"""Read-only latency reports from bounded, allowlisted experiment observations.

Durations use the producer's monotonic clock. Wall-clock end markers place
approximate intervals on a timeline; they do not establish causal critical paths.
This module never subtracts model latency from client or tool measurements.
"""
import argparse
import json
import math
from pathlib import Path
import re

SCHEMA = "workspace.perf.v1"
MAX_RECORDS = 20000
MAX_BYTES = 32 * 1024 * 1024
MAX_LINE_BYTES = 128 * 1024
MAX_DURATION_MS = 4 * 60 * 60 * 1000
RUNTIME_PHASES = frozenset({
    "preparationAttempt", "environmentAcquire", "globalAcquire", "globalHold",
    "preparationInventory", "preparationReconcileReserve", "physicalConfirm",
    "controllerTakeover", "workspacePreparation", "dockerCreatePermitWait",
    "dockerCreate", "dockerEnsureCreated", "dockerDaemonStart", "dockerStart",
    "sandboxEnsure", "model_request_safe_point", "tool_call_safe_point",
    "tool_receipt_safe_point", "other_safe_point", "workspaceSnapshotInspect",
    "workspaceRecoverySnapshotHold", "workspaceFinalSnapshotHold",
    "recoveryUploadFence", "modelHttp", "modelFirstEvent", "modelFirstText",
    "toolService", "hostCommand",
})
HOST_PHASES = frozenset({"taskReady", "taskLeaderExited"})
STARTUP_DURATIONS = ("runtimePreludeMs", "sandboxEnsureMs", "mcpCredentialResolveMs",
                     "mcpDiscoveryMs", "workspaceRestoreMs", "sessionStartCommitMs", "startupTotalMs")
STARTUP_COUNTS = ("serverCount", "credentialCount", "mcpConnectionCount")
IDENTITY_FIELDS = ("runId", "requestId", "callId", "operationId", "taskId")


def _identity(value):
    return isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,160}", value) is not None


def _duration(value):
    return type(value) in (int, float) and math.isfinite(value) and 0 <= value <= MAX_DURATION_MS


def _time(value):
    return type(value) is int and 0 <= value < 2**53


def read_records(path):
    """Read bare JSONL or Docker JSON logs; arbitrary prose is never retained."""
    result, scanned = [], 0
    with Path(path).open("rb") as source:
        for line in source:
            scanned += len(line)
            if scanned > MAX_BYTES or len(line) > MAX_LINE_BYTES:
                raise ValueError("timing input exceeded byte bound")
            try:
                value = json.loads(line)
                if isinstance(value, dict) and isinstance(value.get("log"), str):
                    value = json.loads(value["log"])
            except (ValueError, UnicodeDecodeError):
                if b'"workspace.perf.v1"' in line and line.lstrip().startswith(b'{'):
                    raise ValueError("malformed timing observation") from None
                continue
            if (isinstance(value, dict) and value.get("phase") in
                    RUNTIME_PHASES | HOST_PHASES | {"startupProfile", "accepted", "verified", "toolTiming", "failed", "notSubmitted"}):
                result.append(value)
            if len(result) > MAX_RECORDS:
                raise ValueError("timing record bound exceeded")
    return result


def _bindings(records):
    jobs = {}
    for record in records:
        if record.get("phase") != "accepted":
            continue
        run, session = record.get("agentRunId"), record.get("sessionId")
        if not _identity(run) or not _identity(session) or "agent_run.lifecycle:" + run in jobs:
            raise ValueError("distinct original client Run binding required")
        submitted = record.get("submittedAtMs")
        if submitted is not None and not _time(submitted):
            raise ValueError("client submission time invalid")
        jobs["agent_run.lifecycle:" + run] = {"runId": run, "sessionId": session,
                                             "submittedAtMs": submitted}
    if not jobs or len(jobs) > 64 * 20:
        raise ValueError("bounded original client Run binding required")
    return jobs


def _runtime_record(record, jobs):
    if record.get("schema") != SCHEMA or record.get("source") != "runtime":
        return None
    phase, job = record.get("phase"), record.get("jobId")
    if phase not in RUNTIME_PHASES | {"startupProfile"} or job not in jobs:
        return None
    if not _time(record.get("atUnixMs")):
        raise ValueError("timing observation has invalid end marker")
    if "runId" in record and record["runId"] != jobs[job]["runId"]:
        raise ValueError("timing observation Run binding mismatch")
    safe = {"jobId": job, "phase": phase, "atUnixMs": record["atUnixMs"]}
    if phase == "startupProfile":
        for key in STARTUP_DURATIONS:
            if not _duration(record.get(key)):
                raise ValueError("startup timing observation incomplete")
            safe[key] = record[key]
        for key in STARTUP_COUNTS:
            if type(record.get(key)) is not int or not 0 <= record[key] <= 100000:
                raise ValueError("startup timing observation count invalid")
            safe[key] = record[key]
        return safe
    unpaired = phase == "toolService" and record.get("outcome") == "unpaired"
    if ((not unpaired and not _duration(record.get("elapsedMs")))
            or (unpaired and "elapsedMs" in record and not _duration(record["elapsedMs"]))
            or record.get("outcome") not in {
                "ok", "error", "interrupted", "unpaired", "commitError", "released", "ready"}):
        raise ValueError("timing observation duration or outcome invalid")
    safe["outcome"] = record["outcome"]
    if "elapsedMs" in record:
        safe["elapsedMs"] = record["elapsedMs"]
    for key in IDENTITY_FIELDS:
        if key in record:
            if not _identity(record[key]):
                raise ValueError("timing observation identity invalid")
            safe[key] = record[key]
    if phase in {"modelHttp", "modelFirstEvent", "modelFirstText"}:
        if (not _identity(record.get("requestId")) or type(record.get("attempt")) is not int
                or not 1 <= record["attempt"] <= 1000 or type(record.get("streaming")) is not bool):
            raise ValueError("model timing observation request identity required")
        safe.update(attempt=record["attempt"], streaming=record["streaming"])
    if phase == "toolService" and not _identity(record.get("callId")):
        raise ValueError("tool timing observation call identity required")
    if phase == "hostCommand" and any(not _identity(record.get(key)) for key in ("operationId", "taskId")):
        raise ValueError("host command timing observation identity required")
    return safe


def _host_record(record, tasks):
    if (record.get("schema") != SCHEMA or record.get("source") != "host"
            or record.get("phase") not in HOST_PHASES or record.get("taskId") not in tasks):
        return None
    if (not _time(record.get("atUnixMs")) or not _duration(record.get("elapsedMs"))
            or record.get("outcome") != "ok"):
        raise ValueError("host timing observation invalid")
    return {key: record[key] for key in ("phase", "taskId", "atUnixMs", "elapsedMs", "outcome")}


def _covered(intervals):
    points = sorted((row["atUnixMs"] - row["elapsedMs"], row["atUnixMs"]) for row in intervals)
    total, high = 0, None
    for low, end in points:
        total += max(0, end - max(low, high if high is not None else low))
        high = end if high is None else max(high, end)
    return round(total, 6)


def _client_rows(records, binding):
    result, tools, verified = {}, [], []
    for record in records:
        if record.get("agentRunId") != binding["runId"]:
            continue
        if record.get("sessionId") != binding["sessionId"]:
            raise ValueError("client Run binding mismatch")
        if record.get("phase") == "verified":
            verified.append(record)
        if record.get("phase") != "toolTiming":
            continue
        if (not _identity(record.get("callId")) or record.get("toolName") != "bash"
                or record.get("contractState") != "verified"
                or type(record.get("toolIndex")) is not int or record["toolIndex"] != len(tools) + 1
                or any(item["callId"] == record["callId"] for item in tools)
                or record.get("resultState") not in {"successWithOutput", "successNoOutput"}):
            raise ValueError("client tool timing identity or result invalid")
        call, receipt = record.get("toolCallObservedAtMs"), record.get("toolResultObservedAtMs")
        client_observed = _time(call) and _time(receipt) and receipt >= call
        if (not _time(call) or not _time(receipt)
                or record.get("clientTimingState") != ("observed" if client_observed else "notObserved")
                or (client_observed and (not _duration(record.get("clientCallToResultMs"))
                    or record["clientCallToResultMs"] != receipt - call))
                or (not client_observed and record.get("clientCallToResultMs") is not None)):
            raise ValueError("client tool timing clock invalid")
        created = [record.get(key) for key in ("toolCallCreatedAtMs", "toolResultCreatedAtMs")]
        observed = all(_time(value) for value in created) and created[0] <= created[1]
        if (record.get("serverTimingState") != ("observed" if observed else "notObserved")
                or any(value is not None and not _time(value) for value in created)):
            raise ValueError("client tool event timestamp provenance invalid")
        tools.append({key: record[key] for key in ("callId", "toolIndex", "toolName",
            "toolCallCreatedAtMs", "toolResultCreatedAtMs", "toolCallObservedAtMs", "toolResultObservedAtMs",
            "clientCallToResultMs", "resultState", "serverTimingState", "clientTimingState")})
    if len(verified) > 1:
        raise ValueError("duplicate client verification timing")
    mapping = {"admissionMs": "admissionMs", "firstSuccessfulToolResultMs": "newRunToFirstToolSuccessMs",
               "terminalMs": "newRunToTerminalMs", "firstModelTextMs": "runStartToFirstObservedAssistantTextMs",
               "secondToolClientCallToResultMs": "secondToolClientCallToResultMs"}
    for key, target in mapping.items():
        value = verified[0].get(key) if verified else None
        if value is not None and not _duration(value):
            raise ValueError("client timing duration invalid")
        result[target] = value
    source = verified[0].get("firstModelTextSource") if verified else None
    kind = verified[0].get("firstModelTextTimingKind") if verified else None
    if source not in {None, "live", "committedAssistantMessage"} or kind not in {None, "runStartToFirstObservedAssistantText"}:
        raise ValueError("client assistant text timing provenance invalid")
    result["firstModelTextSource"] = source
    result["firstModelTextTimingKind"] = kind
    expected_second = tools[1]["clientCallToResultMs"] if len(tools) >= 2 else None
    if result["secondToolClientCallToResultMs"] != expected_second:
        raise ValueError("client second tool timing summary mismatch")
    return result, tools


def _model_rows(intervals):
    groups = {}
    for row in intervals:
        if row["phase"] not in {"modelHttp", "modelFirstEvent", "modelFirstText"}:
            continue
        key = row["requestId"], row["attempt"], row["streaming"]
        group = groups.setdefault(key, {"requestId": key[0], "attempt": key[1], "streaming": key[2],
                                      "httpMs": None, "firstEventMs": None, "firstTextMs": None})
        field = {"modelHttp": "httpMs", "modelFirstEvent": "firstEventMs", "modelFirstText": "firstTextMs"}[row["phase"]]
        duplicate = field in group.get("observedFields", [])
        if duplicate:
            ambiguous = group.setdefault("ambiguousFields", [])
            if field not in ambiguous:
                ambiguous.append(field)
        else:
            group.setdefault("observedFields", []).append(field)
        group[field] = row["elapsedMs"] if not duplicate and row["outcome"] == "ok" else None
        if row["phase"] == "modelHttp":
            group["outcome"] = "ambiguous" if duplicate else row["outcome"]
    return list(groups.values())


def build_report(client_records, observations, *, execution_mode="perRun", model_kind="immediateMock"):
    """Project an experiment's timing facts without deciding product acceptance."""
    if execution_mode not in {"perRun", "retained"} or model_kind not in {"immediateMock", "real"}:
        raise ValueError("explicit supported timing plan required")
    records, observations = list(client_records), list(observations)
    if len(records) > MAX_RECORDS or len(observations) > MAX_RECORDS:
        raise ValueError("timing record bound exceeded")
    if any(not isinstance(record, dict) for record in records + observations):
        raise ValueError("timing records must be objects")
    jobs = _bindings(records)
    runtime = [safe for record in observations if (safe := _runtime_record(record, jobs)) is not None]
    tasks = {}
    for row in runtime:
        if row["phase"] == "hostCommand":
            if tasks.setdefault(row["taskId"], row["jobId"]) != row["jobId"]:
                raise ValueError("host task binding is ambiguous")
    host = [safe for record in observations if (safe := _host_record(record, tasks)) is not None]
    rows = []
    for job, binding in jobs.items():
        own = [row for row in runtime if row["jobId"] == job]
        intervals = [row for row in own if row["phase"] != "startupProfile" and "elapsedMs" in row]
        startup = [row for row in own if row["phase"] == "startupProfile"]
        client, tools = _client_rows(records, binding)
        for tool in tools:
            services = [row for row in own if row["phase"] == "toolService" and row.get("callId") == tool["callId"]]
            tool["serviceOutcome"] = services[0]["outcome"] if len(services) == 1 else "ambiguous" if services else "notObserved"
            tool["serviceMs"] = services[0]["elapsedMs"] if len(services) == 1 and services[0]["outcome"] == "ok" else None
        commands = []
        for row in intervals:
            if row["phase"] != "hostCommand":
                continue
            command = dict(row)
            for phase, field in [("taskReady", "readyReceivedMs"), ("taskLeaderExited", "leaderExitedReceivedMs")]:
                matches = [event for event in host if event["taskId"] == row["taskId"] and event["phase"] == phase]
                command[field] = matches[0]["elapsedMs"] if len(matches) == 1 else None
                command[field + "State"] = "observed" if len(matches) == 1 else "ambiguous" if matches else "notObserved"
            ready, exited = command["readyReceivedMs"], command["leaderExitedReceivedMs"]
            command["readyToLeaderExitReceivedMs"] = exited - ready if ready is not None and exited is not None and exited >= ready else None
            commands.append(command)
        model = _model_rows(intervals)
        missing = [key for key, present in [("startupProfile", bool(startup)), ("modelRequestTiming", bool(model)),
                   ("toolServiceTiming", any(row["phase"] == "toolService" for row in intervals)),
                   ("hostCommandTiming", bool(commands)), ("clientToolTiming", bool(tools))] if not present]
        rows.append({**binding, "jobId": job, "client": client, "startupProfiles": startup,
                     "intervals": intervals, "runtimeCoveredMs": _covered(intervals) if intervals else None,
                     "criticalPath": "notObserved", "modelRequests": model, "tools": tools,
                     "hostCommands": commands,
                     "unpairedTools": [row for row in own if row["phase"] == "toolService" and row.get("outcome") == "unpaired"],
                     "notObserved": missing})
    return {"schema": "workspace.perf.experience_timing.v1", "scope": "latencyOnly",
            "executionMode": execution_mode, "executionModeEvidence": "callerDeclared", "modelKind": model_kind,
            "toolPerformanceVerdict": "notEvaluated", "physicalAcceptance": "notEvaluated",
            "intervalClock": "monotonicDurationWithWallClockEnd", "durationsAdditive": False,
            "modelRequestIdentity": "requestAtHttpStart", "serverEventTime": "createdAtMsNotCommit",
            "runs": rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--client-records", type=Path, required=True)
    parser.add_argument("--runtime-observations", type=Path, required=True)
    parser.add_argument("--host-observations", type=Path)
    parser.add_argument("--execution-mode", choices=("perRun", "retained"), required=True)
    parser.add_argument("--model-kind", choices=("immediateMock", "real"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    observations = read_records(args.runtime_observations)
    if args.host_observations:
        observations += read_records(args.host_observations)
    report = build_report(read_records(args.client_records), observations,
                          execution_mode=args.execution_mode, model_kind=args.model_kind)
    with args.output.open("x", encoding="utf-8") as target:
        json.dump(report, target, indent=2, allow_nan=False)
        target.write("\n")
    print(json.dumps({"schema": report["schema"], "runs": len(report["runs"]), "scope": report["scope"]}))


if __name__ == "__main__":
    main()
