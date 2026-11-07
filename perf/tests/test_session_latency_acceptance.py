import unittest

from perf.harness.session_latency_acceptance import (
    continuous_acceptance, drain_conditions, evaluate_drain, pool_health,
)


def pool(at=1000, size=1, **changes):
    return {"observedAtMs": at, "pid": 123, "poolIdentity": "actual-api-pool", "maxSize": 10,
            "poolSize": size, "available": size, "actualConnectionCount": size,
            "borrowed": 0, "waiting": 0, "activeWaiting": 0, "expiredWaiting": 0,
            "lostWithoutPut": 0, "activeTransactions": 0, "idleInTransaction": 0,
            "closed": False, "growing": False, "backendPids": list(range(100, 100+size)), **changes}


def settled_run(identity="run"):
    return {"runId": identity, "status": "completed", "jobStatus": "succeeded",
            "releasedAtMs": 1100, "preAdmissionCancelledAtMs": None}


def sample(at, runs=(), **changes):
    return {"atMs": at-1, "observedAtMs": at, "runs": list(runs), "apiPool": pool(at),
            "queued": 0, "running": 0, "residentCount": 0, "reservedCount": 0,
            "releasingCount": 0, "declaredResidentMemoryBytes": 0, "pendingRetirements": 0,
            "runscRunningSlotCount": 0, "runscPausedCount": 0, "capacityWaiters": 0,
            "physicalContainerCount": 0, "activeJobs": [], **changes}


def continuous_run():
    start = 1000; stop = start+540000
    records = [{"phase": "loadSchedule", "arrivalStartedAtMs": start, "submissionsStoppedAtMs": stop}]
    runs = []
    for index in range(162):
        identity = f"run-{index}"
        at = start+index*60000//18
        base = {"operationId": f"operation-{index}", "sampleId": f"sample-{index}", "agentRunId": identity,
                "turn": "first" if index < 16 else "followup", "submittedAtMs": at,
                "arrivalWindow": (at-start)//180000+1}
        records.extend({"phase": phase, **base} for phase in ("submitted", "accepted", "verified", "releaseObserved"))
        runs.append(settled_run(identity))
    return {"records": records, "durable_runs": runs, "samples": [sample(stop+20000, runs, apiPool=pool(stop+20000, 4))],
            "pool_baseline": pool(), "k6_summary": {"metrics": {"dropped_iterations": {"count": 0}}}, "k6_exit_code": 0}


class SessionLatencyAcceptanceTests(unittest.TestCase):
    def test_idle_pool_growth_is_healthy_without_fixing_size_to_the_warmed_baseline(self):
        self.assertEqual(pool_health(pool(size=4), pool())["state"], "ready")
        for change in ({"poolSize": 10, "available": 0, "actualConnectionCount": 0, "backendPids": []},
                       {"borrowed": 1}, {"waiting": 2263, "expiredWaiting": 2263},
                       {"lostWithoutPut": 1}, {"growing": True}, {"idleInTransaction": 1}, {"closed": True}):
            self.assertNotEqual(pool_health(pool(**change), pool())["state"], "ready", change)

    def test_missing_actual_pool_facts_and_process_or_pool_changes_do_not_pass(self):
        for value in (None, {}, {"poolSize": 1}):
            self.assertEqual(pool_health(value)["state"], "notObserved")
        for change in ({"pid": 124}, {"poolIdentity": "recreated"}, {"maxSize": 20}):
            self.assertNotEqual(pool_health(pool(**change), pool())["state"], "ready")

    def test_drain_waits_for_last_condition_and_uses_stop_not_generator_exit(self):
        runs = [settled_run()]
        observations = [sample(1100, runs, apiPool=pool(1100, borrowed=1)),
                        sample(2000, runs, physicalContainerCount=1), sample(3000, runs)]
        result = evaluate_drain(observations, 1000, pool(), {"run"})
        self.assertEqual(result["state"], "drained")
        self.assertEqual(result["elapsedMs"], 2000)
        self.assertEqual(result["conditionRecoveredAtMs"]["physicalSandboxesAbsent"], 3000)
        self.assertEqual(result["conditionRecoveredAtMs"]["apiPoolIdle"], 2000)
        self.assertEqual(evaluate_drain([sample(121001, runs)], 1000, pool(), {"run"})["state"], "deadlineExceeded")

    def test_drain_checks_original_runs_stopped_sandboxes_and_ledger_responsibility(self):
        runs = [settled_run()]
        for change in ({"runs": []}, {"physicalContainerCount": 1}, {"residentCount": 1},
                       {"pendingRetirements": 1}, {"capacityWaiters": 1}, {"queued": 1}):
            row = sample(2000, runs); row.update(change)
            self.assertTrue(any(not c["ready"] for c in drain_conditions(row, pool(), {"run"}).values()), change)
        stale = sample(2000, runs, apiPool=pool(999))
        self.assertNotEqual(evaluate_drain([stale], 1000, pool(), {"run"})["state"], "drained")

    def test_fixed_continuous_schedule_counts_only_completed_verified_followups(self):
        result = continuous_acceptance(**continuous_run())
        self.assertTrue(result["passed"], result["reasons"])
        self.assertEqual(result["counts"]["actuallySubmitted"], 162)
        self.assertEqual(result["counts"]["completedFollowups"], 146)
        self.assertEqual([row["submitted"] for row in result["windows"]], [54, 54, 54])
        self.assertEqual(result["drain"]["elapsedMs"], 20000)

    def test_second_window_failure_is_not_hidden_by_aggregate_success_or_final_cleanup(self):
        inputs = continuous_run()
        inputs["records"] = [r for r in inputs["records"] if not (r.get("agentRunId") == "run-60" and r["phase"] == "verified")]
        inputs["durable_runs"][60]["status"] = "failed"
        result = continuous_acceptance(**inputs)
        self.assertFalse(result["passed"])
        self.assertEqual(result["drain"]["state"], "drained")
        self.assertGreaterEqual(result["counts"]["completedFollowups"], 100)
        self.assertTrue(any("window" in reason and "2" in reason for reason in result["reasons"]))

    def test_rejection_and_intentional_disconnect_are_not_successful_followup_samples(self):
        inputs = continuous_run()
        for row in inputs["records"]:
            if row.get("agentRunId") == "run-60" and row["phase"] == "accepted":
                row.update(phase="rejected", httpStatus=500)
        inputs["durable_runs"] = [r for r in inputs["durable_runs"] if r["runId"] != "run-60"]
        inputs["samples"][0]["runs"] = inputs["durable_runs"]
        inputs["records"].append({"phase": "intentionalDisconnect", "agentRunId": "run-61"})
        result = continuous_acceptance(**inputs)
        self.assertFalse(result["passed"])
        self.assertEqual(result["counts"]["http500"], 1)
        self.assertEqual(result["counts"]["intentionalDisconnects"], 1)
        self.assertEqual(result["counts"]["completedFollowups"], 144)

    def test_unmatched_admission_and_durable_run_ids_stay_visible(self):
        inputs = continuous_run()
        inputs["records"] = [r for r in inputs["records"] if not (r.get("agentRunId") == "run-60" and r["phase"] == "accepted")]
        result = continuous_acceptance(**inputs)
        self.assertFalse(result["passed"])
        self.assertEqual(result["unresolvedAdmissionOperationIds"], ["operation-60"])
        self.assertEqual(result["unmatchedDurableRunIds"], ["run-60"])

    def test_missing_sampler_or_pool_evidence_and_late_or_shed_requests_fail_acceptance(self):
        for change in ({"pool_baseline": None}, {"sampler_error": "TimeoutError"}, {"k6_summary": {}}, {"k6_exit_code": 99}):
            inputs = continuous_run(); inputs.update(change)
            self.assertFalse(continuous_acceptance(**inputs)["passed"], change)
        for phase in ("failed", "notSubmitted"):
            inputs = continuous_run(); inputs["records"].append({"phase": phase, "agentRunId": "run-60"})
            self.assertFalse(continuous_acceptance(**inputs)["passed"])
        inputs = continuous_run(); inputs["samples"][0].pop("apiPool")
        self.assertFalse(continuous_acceptance(**inputs)["passed"])
        inputs = continuous_run(); inputs["records"][1]["submittedAtMs"] = 541000
        self.assertFalse(continuous_acceptance(**inputs)["passed"])

    def test_resource_waiting_is_a_separate_sampled_process_state(self):
        inputs = continuous_run()
        waiting = {"runId": "run-60", "jobStatus": "queued", "resourceWaitReason": "execution_resident_capacity_wait"}
        inputs["samples"][:0] = [sample(2000, activeJobs=[waiting]), sample(3000)]
        result = continuous_acceptance(**inputs)
        self.assertTrue(result["passed"], result["reasons"])
        self.assertEqual(result["resourceWaiting"]["sampledWaitMsByRun"], {"run-60": 1000})
        self.assertEqual(result["counts"]["verifiedAndReleasedCompletedRuns"], 162)


if __name__ == "__main__":
    unittest.main()
