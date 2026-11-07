"""Pure acceptance checks for one bounded, continuous Session arrival stream."""
from collections import Counter


TERMINAL_JOBS = {"succeeded", "failed", "dead_lettered", "cancelled"}
RESOURCE_WAITS = {
    "execution_host_preparation_wait", "execution_host_admission_wait",
    "execution_resident_capacity_wait", "runsc_environment_capacity_wait",
}
POOL_COUNTS = (
    "maxSize", "poolSize", "available", "borrowed", "waiting", "actualConnectionCount",
    "activeTransactions", "idleInTransaction", "lostWithoutPut", "activeWaiting", "expiredWaiting",
)


def _integer(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def pool_health(pool, baseline=None):
    """Require actual pool/lease facts; legitimate idle pool growth is allowed."""
    if not isinstance(pool, dict):
        return {"state": "notObserved", "reasons": ["actual API pool observation missing"]}
    required = (*POOL_COUNTS, "observedAtMs", "pid", "poolIdentity", "closed", "growing", "backendPids")
    missing = [key for key in required if key not in pool]
    invalid = [key for key in (*POOL_COUNTS, "observedAtMs", "pid") if key in pool and not _integer(pool[key])]
    if "poolIdentity" in pool and not isinstance(pool["poolIdentity"], str):
        invalid.append("poolIdentity")
    for key in ("closed", "growing"):
        if key in pool and not isinstance(pool[key], bool):
            invalid.append(key)
    if "backendPids" in pool and (not isinstance(pool["backendPids"], list)
            or any(not _integer(pid) or pid == 0 for pid in pool["backendPids"])):
        invalid.append("backendPids")
    if missing or invalid:
        return {"state": "notObserved", "reasons": [
            *["API pool field missing: " + key for key in missing],
            *["API pool field invalid: " + key for key in invalid],
        ]}
    reasons = []
    if not pool["pid"] or not pool["poolIdentity"] or not pool["maxSize"]:
        reasons.append("API pool identity or bound invalid")
    if baseline is not None:
        for key in ("pid", "poolIdentity", "maxSize"):
            if pool[key] != baseline.get(key):
                reasons.append("API pool changed during continuous load: " + key)
    for key in ("borrowed", "waiting", "activeWaiting", "expiredWaiting", "activeTransactions", "idleInTransaction", "lostWithoutPut"):
        if pool[key] != 0:
            reasons.append("API pool has outstanding " + key)
    if pool["closed"] or pool["growing"]:
        reasons.append("API pool is closed or still growing")
    if not (pool["available"] == pool["poolSize"] == pool["actualConnectionCount"] <= pool["maxSize"]):
        reasons.append("API pool occupancy differs from its actual idle connections")
    pids = pool["backendPids"]
    if len(set(pids)) != len(pids) or len(pids) != pool["actualConnectionCount"]:
        reasons.append("API pool backend identities do not match actual connections")
    return {"state": "ready" if not reasons else "pending", "reasons": reasons}


def load_schedule(records):
    values = {(r.get("arrivalStartedAtMs"), r.get("submissionsStoppedAtMs")) for r in records if r.get("phase") == "loadSchedule"}
    if len(values) != 1:
        return {"state": "notObserved", "reasons": ["one consistent k6 scenario start/stop schedule required"]}
    start, stop = values.pop()
    if not _integer(start) or not _integer(stop) or stop - start != 540000:
        return {"state": "notObserved", "reasons": ["continuous scenario must span exactly 540 seconds"]}
    return {"state": "observed", "startedAtMs": start, "stoppedAtMs": stop, "windowDurationMs": 180000}


def drain_conditions(sample, pool_baseline, accepted_run_ids):
    """Check one composite observation without using k6 process-exit time."""
    checks = {}
    def check(name, ready, reasons=()):
        checks[name] = {"ready": bool(ready), "reasons": list(reasons) if not ready else []}
    runs = {r["runId"]: r for r in sample.get("runs", [])}
    missing = sorted(set(accepted_run_ids) - runs.keys())
    unsettled = [rid for rid in accepted_run_ids if rid in runs and (
        runs[rid].get("status") not in {"completed", "failed", "cancelled"}
        or (runs[rid].get("jobStatus") not in TERMINAL_JOBS
            and not (runs[rid].get("jobStatus") is None and runs[rid].get("preAdmissionCancelledAtMs")))
        or not (runs[rid].get("releasedAtMs") or runs[rid].get("preAdmissionCancelledAtMs")))]
    check("originalRunsSettled", not missing and not unsettled,
          [*["accepted Run not observed: " + rid for rid in missing], *["Run/Job/release unsettled: " + rid for rid in unsettled]])
    for name, fields in {
        "jobsAndCapacityWaitersIdle": ("queued", "running", "capacityWaiters"),
        "resourceLedgerReleased": ("residentCount", "reservedCount", "releasingCount", "declaredResidentMemoryBytes", "pendingRetirements", "runscRunningSlotCount", "runscPausedCount"),
        "physicalSandboxesAbsent": ("physicalContainerCount",),
    }.items():
        reasons = [key + (" not observed" if key not in sample else " has outstanding responsibility")
                   for key in fields if not _integer(sample.get(key)) or sample[key] != 0]
        check(name, not reasons, reasons)
    health = pool_health(sample.get("apiPool"), pool_baseline)
    check("apiPoolIdle", health["state"] == "ready", health["reasons"])
    return checks


def evaluate_drain(samples, stopped_at_ms, pool_baseline, accepted_run_ids):
    """Return the observed upper-bound drain time, including every required condition."""
    baseline = pool_health(pool_baseline)
    if baseline["state"] != "ready":
        return {"state": "notObserved", "reasons": ["healthy warmed API pool baseline required", *baseline["reasons"]]}
    after_stop = [s for s in samples if _integer(s.get("observedAtMs")) and s["observedAtMs"] >= stopped_at_ms]
    after_stop.sort(key=lambda s: s["observedAtMs"])
    recovered = {}; latest = {}; observed_times = []
    for sample in after_stop:
        observed = sample["observedAtMs"]; observed_times.append(observed)
        latest = drain_conditions(sample, pool_baseline, accepted_run_ids)
        pool_time = (sample.get("apiPool") or {}).get("observedAtMs")
        if not _integer(pool_time) or pool_time < stopped_at_ms:
            latest["apiPoolIdle"] = {"ready": False, "reasons": ["API pool observation predates stop or is missing"]}
        for name, result in latest.items():
            if result["ready"]:
                recovered.setdefault(name, observed)
            else:
                recovered.pop(name, None)
        if all(result["ready"] for result in latest.values()):
            done = max(recovered.values())
            return {"state": "drained" if done - stopped_at_ms <= 120000 else "deadlineExceeded",
                    "stoppedAtMs": stopped_at_ms, "completedAtMs": done, "elapsedMs": done - stopped_at_ms,
                    "conditionRecoveredAtMs": recovered, "checks": latest,
                    "maxObservedSamplingGapMs": max((b-a for a, b in zip(observed_times, observed_times[1:])), default=None),
                    "timingKind": "composite-observation upper bound"}
    return {"state": "deadlineExceeded" if after_stop else "notObserved", "stoppedAtMs": stopped_at_ms,
            "completedAtMs": None, "conditionRecoveredAtMs": recovered, "checks": latest,
            "reasons": ["no observation satisfies every natural-drain condition"]}


def _count(summary, name):
    metric = summary.get("metrics", {}).get(name, {})
    value = metric.get("count", metric.get("values", {}).get("count"))
    return value if _integer(value) else None


def continuous_acceptance(records, durable_runs, samples, pool_baseline, k6_summary, *, k6_exit_code, sampler_error=None, baseline_run_ids=()):
    """Keep attempted, rejected, verified, released, and durable outcomes separate."""
    schedule = load_schedule(records); reasons = []
    baseline_health = pool_health(pool_baseline)
    if baseline_health["state"] != "ready":
        reasons.extend(["healthy warmed API pool baseline required", *baseline_health["reasons"]])
    by_phase = {phase: [r for r in records if r.get("phase") == phase]
                for phase in ("submitted", "accepted", "rejected", "verified", "releaseObserved", "failed", "notSubmitted", "intentionalDisconnect", "arrivalCutoff")}
    accepted = {r["agentRunId"]: r for r in by_phase["accepted"]}
    durable = {r["runId"]: r for r in durable_runs if r["runId"] not in set(baseline_run_ids)}
    submitted_operations = [r.get("operationId") for r in by_phase["submitted"]]
    observed_operations = [r.get("operationId") for r in by_phase["accepted"] + by_phase["rejected"]]
    unresolved = sorted(set(submitted_operations) - set(observed_operations))
    if len(accepted) != len(by_phase["accepted"]) or len(set(submitted_operations)) != len(submitted_operations):
        reasons.append("duplicate original submission or accepted Run identity")
    if set(submitted_operations) != set(observed_operations) or len(observed_operations) != len(submitted_operations):
        reasons.append("original POST admission outcomes are not fully reconciled")
    if set(accepted) != set(durable):
        reasons.append("original accepted Run IDs differ from durable new Runs")
    verified = {r.get("agentRunId") for r in by_phase["verified"]}
    released = {r.get("agentRunId") for r in by_phase["releaseObserved"]}
    failed = {r.get("agentRunId") for r in by_phase["failed"]}
    disconnected = {r.get("agentRunId") for r in by_phase["intentionalDisconnect"]}
    successful = {rid for rid in accepted.keys() & durable.keys() & verified & released
                  if rid not in failed | disconnected and durable[rid].get("status") == "completed"
                  and durable[rid].get("jobStatus") == "succeeded" and durable[rid].get("releasedAtMs")}
    followups = {rid for rid in successful if accepted[rid].get("turn") == "followup"}
    rejected_statuses = Counter(str(r.get("httpStatus")) for r in by_phase["rejected"])
    dropped = _count(k6_summary, "dropped_iterations")
    counts = {"plannedBusinessInputs": 162, "actuallySubmitted": len(by_phase["submitted"]), "accepted": len(accepted),
              "rejected": len(by_phase["rejected"]), "rejectedByHttpStatus": dict(rejected_statuses),
              "http500": rejected_statuses["500"], "notSubmitted": len(by_phase["notSubmitted"]),
              "droppedIterations": dropped, "verifiedContracts": len(verified), "verifiedAndReleasedCompletedRuns": len(successful),
              "completedFollowups": len(followups), "failedObservations": len(by_phase["failed"]),
              "failedRuns": sum(r.get("status") == "failed" for r in durable.values()),
              "cancelledRuns": sum(r.get("status") == "cancelled" for r in durable.values()),
              "intentionalDisconnects": len(by_phase["intentionalDisconnect"]), "schedulingBoundaryIterations": len(by_phase["arrivalCutoff"])}
    for name in ("rejected", "notSubmitted", "failedObservations", "failedRuns", "cancelledRuns"):
        if counts[name]: reasons.append("continuous load has " + name)
    if dropped is None or dropped != 0: reasons.append("k6 dropped iterations missing or nonzero")
    if len(by_phase["submitted"]) != 162: reasons.append("actual submissions do not match the fixed 162-input schedule")
    if len(followups) < 100: reasons.append("fewer than 100 actually completed verified followups")
    if successful != accepted.keys(): reasons.append("not every original accepted Run completed verification and release")
    if k6_exit_code != 0: reasons.append("k6 did not exit successfully")
    if sampler_error: reasons.append("resource sampler failed: " + str(sampler_error))
    rounds = []; drain = {"state": "notObserved", "reasons": schedule.get("reasons", [])}
    if schedule["state"] != "observed": reasons.extend(schedule["reasons"])
    else:
        start = schedule["startedAtMs"]; stop = schedule["stoppedAtMs"]
        if any(not _integer(r.get("submittedAtMs")) or not start <= r["submittedAtMs"] < stop for r in by_phase["submitted"]):
            reasons.append("business POST was issued outside its scheduled arrival interval")
        for window in range(1, 4):
            ids = {rid for rid, r in accepted.items() if r.get("arrivalWindow") == window}
            submissions = [r for r in by_phase["submitted"] if r.get("arrivalWindow") == window]
            rejections = [r for r in by_phase["rejected"] if r.get("arrivalWindow") == window]
            rounds.append({"window": window, "plannedSubmissions": 54, "startedAtMs": start+(window-1)*180000, "stoppedAtMs": start+window*180000,
                           "submitted": len(submissions), "accepted": len(ids), "rejected": len(rejections),
                           "completedVerifiedRuns": len(ids & successful), "completedVerifiedFollowups": len(ids & followups)})
            if not submissions or rejections or ids - successful:
                reasons.append("continuous window did not satisfy fixed load and success: " + str(window))
        drain = evaluate_drain(samples, stop, pool_baseline, set(accepted))
        if drain["state"] != "drained": reasons.append("complete natural drain was not observed within 120 seconds")
    pool_observation_failures = []
    for sample in samples:
        pool = sample.get("apiPool"); health = pool_health(pool, pool_baseline)
        if health["state"] == "notObserved": pool_observation_failures.append(sample.get("observedAtMs"))
        elif isinstance(pool_baseline, dict) and any(pool[key] != pool_baseline.get(key) for key in ("pid", "poolIdentity", "maxSize")):
            reasons.append("API process or pool changed during the continuous load")
        if isinstance(pool, dict) and _integer(pool.get("lostWithoutPut")) and pool["lostWithoutPut"]:
            reasons.append("API connection was lost without a matching putconn")
    if pool_observation_failures: reasons.append("actual API pool facts were not observed throughout the load")
    waits = {}
    for before, after in zip(samples, samples[1:]):
        for job in before.get("activeJobs", []):
            if job.get("jobStatus") == "queued" and job.get("resourceWaitReason") in RESOURCE_WAITS:
                waits[job["runId"]] = waits.get(job["runId"], 0) + max(0, after["observedAtMs"]-before["observedAtMs"])
    return {"passed": not reasons, "reasons": list(dict.fromkeys(reasons)), "schedule": schedule, "counts": counts,
            "windows": rounds, "drain": drain, "unresolvedAdmissionOperationIds": unresolved,
            "unmatchedDurableRunIds": sorted(set(durable)-set(accepted)), "poolObservationFailureAtMs": pool_observation_failures,
            "resourceWaiting": {"affectedRunCount": len(waits), "sampledWaitMsByRun": waits,
                                "timingKind": "one-second sampled queued resource-wait state; not exact wait duration"},
            "intentionalDisconnectsCountedAsSuccessfulFollowups": False}
