"""Bounded full-service PostgreSQL/k6 working-set acceptance adapter.

Fixture Docker, builds and service startup belong to the caller. This adapter
uses public admission/supplements/cancel, actual Core compaction and read-only
fixture observations. It never inserts Session events or restores snapshots.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
HISTORIES = (1000, 10000)
PREPARATION = frozenset({"modelInputEstimate", "promptCompaction", "modelInputIntake",
                         "modelInputProjection", "agentsInstructionsRead", "agentsInstructionsReuse",
                         "model_request_safe_point"})


def plan():
    return {"schema": "runtime.working_set.pg_plan.v1", "versions": 2,
            "histories": list(HISTORIES), "warmupPerArm": 20, "measuredPerArm": 100,
            "measurementRuns": 480, "setupRuns": 4,
            "setupMainCallsUpper": sum(2 * (math.ceil(history / 8) + 3) for history in HISTORIES),
            "setupSummaryCallsUpper": sum(2 * (math.ceil(history / 8) + 2) for history in HISTORIES),
            "saturationVus": [1, 2, 4, 8], "saturationIterations": 6, "saturationRunsUpper": 90,
            "setupContextTokens": 131072, "measurementContextTokens": 200000,
            "outputTokens": 32768, "barrierSeconds": 20, "setupDeadlineSeconds": 3600,
            "usageMode": "syntheticNoLlmTokens",
            "tokenEstimationAccuracy": "notMeasuredByThisFixture",
            "historyMeaning": "actual admitted historical supplement inputs; canonical event count reported separately",
            "workingSetMeaning": "W(t) observed; equality required at paired iteration, no globally constant W claim"}


def model_control(base, path, value=None):
    if not re.fullmatch(r"http://127\.0\.0\.1:[0-9]+", base):
        raise ValueError("loopback model control required")
    request = urllib.request.Request(base + path, data=None if value is None else json.dumps(value).encode(),
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=5) as response:
        data = response.read(4 * 1024 * 1024 + 1)
    if len(data) > 4 * 1024 * 1024:
        raise ValueError("model observation exceeded bound")
    return json.loads(data)


def fixture_driver(fixture, output):
    manifest = json.loads((fixture / "manifest.json").read_text(encoding="utf-8"))
    source = Path(manifest["source"]).resolve(strict=True)
    # Load the existing ownership validator from the fixture's exact checkout.
    sys.path.insert(0, str(source / "scripts/workspace"))
    module = importlib.import_module("runsc_rotation_acceptance")
    driver = module.FixtureDriver(fixture, output, expected_running_limit=8)
    if driver.private.get("WORKSPACE_PERF_OBSERVATIONS") != "1":
        raise ValueError("existing Runtime preparation observations must be enabled")
    return driver


def configure_business_model(driver, context_tokens, *, initial=False):
    driver.sql("""import json
from django.conf import settings
from app_core.models import AgentRun,ModelConfig
assert settings.DATABASES['default']['NAME']=='retained_acceptance'
model=ModelConfig.objects.select_related('provider').get(pk=MODEL)
assert model.provider.workspace_id in (None,WORKSPACE)
if INITIAL:assert not AgentRun.objects.exists(),'fresh owned fixture required'
model.provider.api='openai-completions'
model.provider.apiBase='https://model:8080/v1'
model.provider.save(update_fields=['api','apiBase'])
model.modelName='working-set-benchmark'
model.contextTokens=CAPACITY
model.maxOutputTokens=32768
model.save(update_fields=['modelName','contextTokens','maxOutputTokens'])
print(json.dumps({'configured':True}))
""".replace("MODEL", repr(driver.seed["modelConfigRef"]))
        .replace("WORKSPACE", repr(driver.seed["workspaceId"]))
        .replace("INITIAL", repr(initial)).replace("CAPACITY", str(context_tokens)))


def wait_gate(base, previous, deadline, driver, run_id, receipt):
    next_status = time.monotonic() + 1
    while time.monotonic() < deadline:
        state = model_control(base, "/state")
        if state["cancelled"]:
            raise RuntimeError("setup model stopped")
        pending = state["pending"]
        if pending is not None and pending["id"] != previous:
            return pending
        if time.monotonic() >= next_status:
            status = driver.status({"agentRunId": run_id})
            if status["runStatus"] in {"completed", "failed", "cancelled"}:
                receipt["terminalDuringSetup"] = status
                raise RuntimeError("actual Run terminated before the next setup gate")
            next_status = time.monotonic() + 1
        time.sleep(0.05)
    raise RuntimeError("setup deadline")


def observe_release(driver, run_id):
    value = driver.status({"agentRunId": run_id})
    return value, (value["runStatus"] == "completed" and value["coreTerminalType"] == "agent_run_completed"
                   and value["releaseReceiptCount"] == 1 and value["cleanupComplete"] is True)


def wait_release(driver, run_id, seconds=120):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        state, released = observe_release(driver, run_id)
        if released:
            return state
        if state["runStatus"] in {"failed", "cancelled"}:
            raise RuntimeError("Run did not complete")
        time.sleep(0.25)
    raise RuntimeError("Run release deadline")


def cancel_observed(driver, run_id):
    """Issue the real public cancel; keep unknown/unsettled outcomes explicit."""
    try:
        state = driver.status({"agentRunId": run_id})
        if state["runStatus"] in {"queued", "running"}:
            driver.http(f"/api/sessions/{state['sessionId']}/agent-runs/{run_id}/cancel", {})
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            state = driver.status({"agentRunId": run_id})
            if state["runStatus"] in {"completed", "failed", "cancelled"} and state["cleanupComplete"]:
                return {"state": "observed", "receipt": state}
            time.sleep(0.25)
        return {"state": "unsettled", "receipt": state}
    except Exception:
        return {"state": "unknown"}


def seed_history(driver, history, control, receipt):
    model = model_control(control, "/configure", {"history": history})
    expected_model = hashlib.sha256(Path(__file__).with_name("working_set_pg_model.py").read_bytes()).hexdigest()
    if model.get("implementationSha256") != expected_model:
        raise ValueError("running model bytes differ from frozen adapter")
    if model.get("usageMode") != "syntheticNoLlmTokens":
        raise ValueError("explicit synthetic LLM usage mode required")
    receipt["modelImplementationSha256"] = expected_model
    receipt["usageMode"] = model["usageMode"]
    configure_business_model(driver, 131072, initial=True)
    admitted = driver.http(f"/api/workspaces/{driver.seed['workspaceId']}/sessions/new/messages", {
        "operationId": "working-set-setup", "text": "WORKING_SET_SETUP\nFollow the next synthetic performance task exactly.",
        "modelConfigRef": driver.seed["modelConfigRef"], "attachmentRefs": []})
    run, session = admitted["agentRunId"], admitted["sessionId"]
    receipt.update(agentRunId=run, sessionId=session, history=history,
                   mainCallsUpper=math.ceil(history / 8) + 3, summaryCallsUpper=math.ceil(history / 8) + 2)
    deadline = time.monotonic() + 3600
    gate = wait_gate(control, None, deadline, driver, run, receipt)
    if gate["kind"] != "initial":
        raise ValueError("initial setup request not observed")
    first = driver.sql("""import json
from django.db import connection
with connection.cursor() as c:
 c.execute(\"SELECT (payload->'payload'->>'contextTokenEstimate')::bigint FROM app_core_sessionevent WHERE agent_run_id=%s AND payload->>'type'='model_request_started' AND payload->'payload'->>'purpose'='main' ORDER BY sequence DESC LIMIT 1\",[RUN])
 print(json.dumps({'estimate':c.fetchone()[0]}))
""".replace("RUN", repr(run)))

    def advance(items, expected_kind, expected_indices):
        nonlocal gate
        for identifier, message in items:
            result = driver.http(f"/api/sessions/{session}/agent-runs/{run}/supplements",
                                 {"supplementId": identifier, "message": message})
            if result.get("disposition") != "accepted" or not 1 <= result.get("queuedCount", 0) <= 8:
                raise ValueError("supplement admission not acknowledged")
        previous = gate["id"]
        model_control(control, "/release", {"gateId": previous})
        gate = wait_gate(control, previous, deadline, driver, run, receipt)
        if gate["kind"] != expected_kind or gate["historyIndices"] != expected_indices:
            raise ValueError("accepted batch differs from actual model input")
        receipt["lastObservedGate"] = {"kind": expected_kind, "id": gate["id"],
                                      "lastHistoricalIndex": expected_indices[-1] if expected_indices else None}

    for start in range(0, history, 8):
        indices = list(range(start, min(start + 8, history)))
        advance([(f"history-{index:06}", f"WORKING_SET_ARCHIVE {index:06}\n" +
                  "Synthetic historical input; no execution or completion claim. " * 4)
                 for index in indices], "history", indices)
    # Choose a normal input below the main hard cap but above the existing
    # quarter-window trigger after compaction. No summary/replay policy is reduced.
    input_limit = 131072 - 32768
    headroom = min(32768, input_limit // 4)
    padding_tokens = input_limit - headroom // 2 - first["estimate"]
    if not 1024 <= padding_tokens <= 90000:
        raise ValueError("observed prompt overhead leaves no bounded padding window")
    padding_chars = padding_tokens * 4
    parts = math.ceil(padding_chars / 60000)
    if not 1 <= parts <= 8:
        raise ValueError("padding exceeds actual pending-input limit")
    advance([(f"padding-{index}", "WORKING_SET_PADDING\n" + "p" * min(60000, padding_chars - index * 60000))
             for index in range(parts)], "padding", [])
    advance([("seal", "WORKING_SET_SEAL\nFollow the next synthetic performance task exactly.")], "seal", [])
    if gate["boundary"]["historicalMarkers"] or gate["boundary"]["paddingMarkers"]:
        raise ValueError("actual compaction did not remove setup working data")
    receipt["sealedBoundary"] = gate["boundary"]
    model_control(control, "/release", {"gateId": gate["id"]})
    receipt["release"] = wait_release(driver, run)
    facts = driver.sql("""import json
from django.db import connection
with connection.cursor() as c:
 c.execute(\"SELECT count(*),count(*) FILTER(WHERE payload->>'type'='turn_supplement' AND payload->'payload'->>'supplementId' LIKE 'history-%%'),count(*) FILTER(WHERE payload->>'type'='model_request_started' AND payload->'payload'->>'purpose'='compaction') FROM app_core_sessionevent WHERE session_id=%s\",[SESSION])
 events,inputs,summaries=c.fetchone()
 print(json.dumps({'canonicalEvents':events,'historicalInputs':inputs,'compactionRequests':summaries}))
""".replace("SESSION", repr(session)))
    if facts["historicalInputs"] != history or facts["compactionRequests"] < 1:
        raise ValueError("actual historical source inputs or compaction facts missing")
    receipt["sourceFacts"] = facts
    configure_business_model(driver, 200000)
    return session


class ReleaseControl:
    def __init__(self, driver):
        self.driver, self.cache, self.barriers = driver, {}, {}
        self.lock = threading.Lock()
        self.allowed, self.vus, self.submissions = True, 1, 0

    def handler(self):
        owner = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                try:
                    if self.path == "/admission":
                        with owner.lock:
                            owner.submissions += 1
                            value = {"allowed": owner.allowed and owner.submissions <= 210}
                    elif re.fullmatch(r"/release/[A-Za-z0-9_.:-]{1,160}", self.path):
                        run = self.path.removeprefix("/release/")
                        with owner.lock:
                            cached = owner.cache.get(run)
                        if cached is None:
                            state, released = observe_release(owner.driver, run)
                            if released:
                                with owner.lock:
                                    owner.cache[run] = state
                            value = {"released": released}
                        else:
                            value = {"released": True}
                    else:
                        match = re.fullmatch(r"/barrier/([1-5])/([A-Za-z0-9_.:-]{1,160})", self.path)
                        if match is None:
                            raise ValueError("unknown fixture observation")
                        with owner.lock:
                            members = owner.barriers.setdefault((owner.vus, int(match[1])), set())
                            if match[2] not in owner.cache:
                                raise ValueError("barrier requires actual release")
                            members.add(match[2])
                            value = {"ready": len(members) == owner.vus}
                    body = json.dumps(value).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                except Exception:
                    self.send_error(503, "fixture observation unavailable")
        return Handler


def client_records(path):
    records = []
    for line in path.read_text(encoding="utf-8").splitlines():
        try:
            outer = json.loads(line)
            message = outer.get("msg", "")
            if not message.startswith("PERF_WORKING_SET "):
                continue
            value = json.loads(message.removeprefix("PERF_WORKING_SET "))
            if value.get("schema") != "runtime.working_set.client.v1":
                raise ValueError("unknown client observation")
            records.append(value)
        except (json.JSONDecodeError, AttributeError):
            continue
    return records


def k6_run(driver, binary, output, session, control, mode, vus):
    directory = output / ("comparison" if mode == "comparison" else f"saturation-{vus}")
    directory.mkdir(mode=0o700)
    iterations = 120 if mode == "comparison" else 6
    environment = {**os.environ, "API_BASE": driver.base,
        "PERF_ADMIN_EMAIL": driver.seed["email"], "PERF_ADMIN_PASSWORD": driver.seed["password"],
        "PERF_WORKSPACE_ID": driver.seed["workspaceId"],
        "PERF_MODEL_REFS": json.dumps({"tools": driver.seed["modelConfigRef"]}),
        "PERF_WORKING_SET_SESSION": session, "PERF_WORKING_SET_CONTROL": control,
        "PERF_WORKING_SET_PLAN": json.dumps({"mode": mode, "vus": vus, "iterations": iterations}),
        "K6_NO_USAGE_REPORT": "true"}
    with (directory / "client.jsonl").open("x", encoding="utf-8") as log:
        result = subprocess.run([str(binary), "--log-format", "json", "run", "--summary-export",
            str(directory / "summary.json"), str(ROOT / "perf/k6/scenarios/working-set-latency.js")],
            env=environment, stdin=subprocess.DEVNULL, stdout=log, stderr=log, timeout=3600)
    records = client_records(directory / "client.jsonl")
    released = [row for row in records if row["phase"] == "released"]
    if result.returncode or len(released) != vus * iterations or any(row["phase"] in
            {"failed", "notSubmitted"} for row in records):
        raise RuntimeError("bounded k6 cohort failed; preserve original Run observations")
    return records


def preparation_records(driver, since, accepted):
    allowed = {"agent_run.lifecycle:" + row["agentRunId"] for row in accepted}
    command = [*driver.docker, "compose", "-f", str(driver.compose), "logs", "--no-color",
               "--no-log-prefix", "--since", since, "runtime"]
    process = subprocess.Popen(command, env=driver.environment, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    rows, scanned = [], 0
    timer = threading.Timer(20, process.terminate)
    timer.start()
    try:
        for line in process.stdout:
            scanned += len(line)
            if scanned > 32 * 1024 * 1024 or len(line) > 128 * 1024:
                raise ValueError("runtime timing observation exceeds existing bound")
            try:
                value = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                continue
            if (isinstance(value, dict) and value.get("schema") == "workspace.perf.v1" and value.get("source") == "runtime"
                    and value.get("jobId") in allowed and value.get("phase") in PREPARATION):
                elapsed = value.get("elapsedMs")
                if (value.get("outcome") != "ok" or type(elapsed) not in (int, float)
                        or not math.isfinite(elapsed) or not 0 <= elapsed <= 14400000):
                    raise ValueError("preparation observation incomplete")
                rows.append({key: value[key] for key in ("jobId", "turnId", "phase", "elapsedMs", "outcome") if key in value})
        if process.wait(timeout=5):
            raise RuntimeError("runtime timing reader failed")
        return rows
    finally:
        timer.cancel()
        if process.poll() is None:
            process.terminate()
        process.stdout.close()
        process.wait(timeout=5)


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)] if ordered else None


def measurement_start(output, ready, wait_for_start):
    pending = output / "setup-ready.pending"
    with pending.open("x", encoding="utf-8") as stream:
        json.dump(ready, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(pending, output / "setup-ready.json")
    if not wait_for_start:
        return "notRequested"
    deadline = time.monotonic() + 1200
    while time.monotonic() < deadline:
        if (output / "start.request").exists():
            return "signalled"
        time.sleep(0.25)
    raise TimeoutError("measurement start signal deadline")


def compare(paths):
    arms = [json.loads(path.read_text(encoding="utf-8")) for path in paths]
    if len(arms) != 4 or {(arm["version"], arm["history"]) for arm in arms} != {
            (version, history) for version in ("baseline", "candidate") for history in HISTORIES}:
        raise ValueError("exact four frozen arms required")
    report = {"schema": "runtime.working_set.pg_comparison.v1", "pairs": []}
    for version in ("baseline", "candidate"):
        left, right = [next(arm for arm in arms if arm["version"] == version and arm["history"] == history)
                       for history in HISTORIES]
        indexed = [{row["sampleId"]: row for row in arm["boundaries"] if row["phase"] == "measurement" and row["round"] == 0}
                   for arm in (left, right)]
        clients = [{row["sampleId"]: row for row in arm["client"] if row["phase"] == "verified" and row["cohort"] == "measured"}
                   for arm in (left, right)]
        matched, unmatched = [], []
        for sample in sorted(clients[0].keys() & clients[1].keys()):
            a, b = indexed[0].get(sample), indexed[1].get(sample)
            if a is None or b is None or any(a[key] != b[key] for key in
                    ("messageCount", "contentBytes", "semanticSha256", "toolsSha256")):
                unmatched.append(sample)
            else:
                matched.append(sample)
        stage_costs = []
        for arm, client in zip((left, right), clients):
            per_run = {}
            for row in arm["preparation"]:
                key = (row["jobId"], row["phase"])
                cost, count = per_run.get(key, (0, 0))
                per_run[key] = (cost + row["elapsedMs"], count + 1)
            stages = {}
            for stage in sorted(PREPARATION):
                values = [per_run[("agent_run.lifecycle:" + client[sample]["agentRunId"], stage)]
                    for sample in matched if ("agent_run.lifecycle:" + client[sample]["agentRunId"], stage) in per_run]
                stages[stage] = {"observedRuns": len(values),
                    "sumPerRunMsP50": percentile([cost for cost, _ in values], .5),
                    "sumPerRunMsP95": percentile([cost for cost, _ in values], .95),
                    "observationsPerRunP50": percentile([count for _, count in values], .5),
                    "observationsPerRunP95": percentile([count for _, count in values], .95)}
            stage_costs.append({"history": arm["history"], "stages": stages})
        report["pairs"].append({"version": version, "matchedWorkingSets": len(matched),
            "unmatchedSamples": unmatched, "latency": [{"history": history,
                "terminalMsP50": percentile([clients[index][sample]["terminalMs"] for sample in matched], .5),
                "terminalMsP95": percentile([clients[index][sample]["terminalMs"] for sample in matched], .95),
                "admissionMsP50": percentile([clients[index][sample]["admissionMs"] for sample in matched], .5),
                "admissionMsP95": percentile([clients[index][sample]["admissionMs"] for sample in matched], .95),
                "firstTextMsP50": percentile([clients[index][sample]["firstTextMs"] for sample in matched], .5),
                "firstTextMsP95": percentile([clients[index][sample]["firstTextMs"] for sample in matched], .95)}
                for index, history in enumerate(HISTORIES)], "preparation": stage_costs})
    report["matchedAllMeasuredPairs"] = all(row["matchedWorkingSets"] == 100 for row in report["pairs"])
    return report


def run(args):
    output = args.output.resolve()
    if ROOT == output or ROOT in output.parents:
        raise ValueError("external disposable output required")
    driver = fixture_driver(args.fixture.resolve(), output)
    setup, records = {}, []
    report = {"schema": "runtime.working_set.pg_arm.v1", "version": args.version,
              "history": args.history, "sourceRevision": driver.manifest["sourceRevision"],
              "plan": plan(), "status": "failedPreserved", "setup": setup}
    report["harnessSha256"] = {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in (Path(__file__), Path(__file__).with_name("working_set_pg_model.py"),
                     ROOT / "perf/k6/scenarios/working-set-latency.js")}
    server = None
    try:
        report["stage"] = "historySetup"
        session = seed_history(driver, args.history, args.model_control, setup)
        report["stage"] = "measurementStart"
        report["measurementStart"] = measurement_start(output, {
            "schema": "runtime.working_set.setup_ready.v1", "sessionId": session,
            "sourceRevision": driver.manifest["sourceRevision"], "setup": setup,
            "waitForStart": args.wait_for_start,
        }, args.wait_for_start)
        release = ReleaseControl(driver)
        server = ThreadingHTTPServer(("127.0.0.1", 0), release.handler())
        threading.Thread(target=server.serve_forever, daemon=True).start()
        control = f"http://127.0.0.1:{server.server_port}"
        since = datetime.now(timezone.utc).isoformat()
        report["stage"] = "comparisonRuns"
        records.extend(k6_run(driver, args.k6, output, session, control, "comparison", 1))
        if args.saturation:
            for vus in (1, 2, 4, 8):
                report["stage"] = f"saturationRuns:{vus}"
                release.vus = vus
                records.extend(k6_run(driver, args.k6, output, session, control, "saturation", vus))
        accepted = [row for row in records if row["phase"] == "accepted"]
        report["stage"] = "timingObservation"
        report.update(client=records, boundaries=model_control(args.model_control, "/boundaries")["records"],
                      preparation=preparation_records(driver, since, accepted),
                      releaseReceipts=list(release.cache.values()), status="observed")
    except Exception as error:
        report["failureType"] = type(error).__name__
        if setup.get("agentRunId"):
            try:
                model_control(args.model_control, "/cancel", {"cancel": True})
            except Exception:
                pass
            report["setupCancellation"] = cancel_observed(driver, setup["agentRunId"])
        for path in output.glob("*/client.jsonl"):
            for row in client_records(path):
                if row["phase"] == "accepted":
                    report.setdefault("cohortCancellation", {})[row["agentRunId"]] = cancel_observed(driver, row["agentRunId"])
    finally:
        if server:
            server.shutdown()
            server.server_close()
        (output / "arm.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("plan")
    comparison = commands.add_parser("compare")
    comparison.add_argument("arms", nargs=4, type=Path)
    execute = commands.add_parser("run")
    for name in ("fixture", "output", "k6"):
        execute.add_argument("--" + name, type=Path, required=True)
    execute.add_argument("--version", choices=("baseline", "candidate"), required=True)
    execute.add_argument("--history", choices=HISTORIES, type=int, required=True)
    execute.add_argument("--model-control", required=True)
    execute.add_argument("--saturation", action="store_true")
    execute.add_argument("--wait-for-start", action="store_true")
    args = parser.parse_args()
    if args.command == "plan":
        value = plan()
    elif args.command == "compare":
        value = compare(args.arms)
    else:
        if args.saturation and (args.version != "candidate" or args.history != 10000):
            parser.error("the one 90-Run saturation cohort belongs to candidate/H10000")
        value = run(args)
    print(json.dumps(value if args.command != "run" else {"status": value["status"],
        "version": value["version"], "history": value["history"]}, indent=2))
    if value.get("status") == "failedPreserved":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
