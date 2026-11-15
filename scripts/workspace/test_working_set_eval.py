import importlib.util
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("working_set_eval", Path(__file__).with_name("working_set_eval.py"))
EVAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVAL)
PG_SPEC = importlib.util.spec_from_file_location("working_set_pg", Path(__file__).with_name("working_set_pg.py"))
PG = importlib.util.module_from_spec(PG_SPEC)
PG_SPEC.loader.exec_module(PG)


class WorkingSetEvidenceTests(unittest.TestCase):
    def sample(self):
        expected = {"project": "p", "owner": "mira", "path": "a.rs", "error": "unknown", "action": "query"}
        return {"mode": "quality", "case": "normal-history", "expected": expected,
                "reply": json.dumps(expected), "stop": "finalized", "error": None,
                "proofReads": 0, "compactions": [], "mainCalls": 1, "summaryCalls": 0,
                "elapsedMs": 2.0, "firstRequestPreparedMs": 1.0, "costUpperUsd": .01}

    def test_evidence_rejects_a_self_declared_pass_with_wrong_facts(self):
        sample = self.sample()
        self.assertEqual(EVAL.validate_sample(sample), [])
        sample["passed"] = True
        sample["reply"] = json.dumps({**sample["expected"], "owner": "wrong"})
        self.assertIn("task_result_mismatch", EVAL.validate_sample(sample))

    def test_tool_proof_and_compaction_count_are_not_inferred_from_final_text(self):
        sample = self.sample()
        sample["case"] = "normal-tool"
        self.assertIn("tool_read_count_mismatch", EVAL.validate_sample(sample))
        sample["proofReads"] = 1
        self.assertEqual(EVAL.validate_sample(sample), [])
        sample["case"] = "repeat-original"
        sample["proofReads"] = 0
        sample["compactions"] = [True]
        self.assertIn("compaction_count_or_outcome_mismatch", EVAL.validate_sample(sample))

    def test_percentile_uses_samples_and_compile_wrapper_preserves_source_roots(self):
        self.assertEqual(EVAL.percentile(list(range(1, 101)), .95), 95)
        root = Path("baseline").resolve()
        harness = Path("candidate/example.rs").resolve()
        manifest = EVAL.manifest(root, harness)
        self.assertIn(str(root / "packages/core").replace("\\", "/"), manifest)
        self.assertIn(str(harness).replace("\\", "/"), manifest)


class WorkingSetSetupTests(unittest.TestCase):
    def test_ready_model_gate_does_not_wait_for_slow_status_subprocess(self):
        clock = SimpleNamespace(now=0.0)
        pending = {"id": "next"}
        driver = SimpleNamespace(status=mock.Mock())

        def state(*_args):
            return {"cancelled": False, "pending": pending if clock.now >= 1.5 else None}

        def advance(seconds):
            clock.now += seconds

        with mock.patch.object(PG, "model_control", side_effect=state), \
                mock.patch.object(PG.time, "monotonic", side_effect=lambda: clock.now), \
                mock.patch.object(PG.time, "sleep", side_effect=advance):
            self.assertIs(PG.wait_gate("unused", None, 20, driver, "run", {}), pending)
        driver.status.assert_not_called()

    def test_terminal_run_is_still_detected_during_gate_wait(self):
        clock, receipt = SimpleNamespace(now=0.0), {}
        terminal = {"runStatus": "failed"}
        driver = SimpleNamespace(status=mock.Mock(return_value=terminal))

        def advance(seconds):
            clock.now += seconds

        with mock.patch.object(PG, "model_control", return_value={"cancelled": False, "pending": None}), \
                mock.patch.object(PG.time, "monotonic", side_effect=lambda: clock.now), \
                mock.patch.object(PG.time, "sleep", side_effect=advance):
            with self.assertRaisesRegex(RuntimeError, "Run terminated"):
                PG.wait_gate("unused", None, 20, driver, "run", receipt)
        driver.status.assert_called_once_with({"agentRunId": "run"})
        self.assertIs(receipt["terminalDuringSetup"], terminal)
        self.assertLess(clock.now, 20)


class WorkingSetComparisonTests(unittest.TestCase):
    def test_failed_history_setup_keeps_missing_pair_unknown(self):
        arms = []
        for version in ("baseline", "candidate"):
            for history in (1000, 10000):
                arm = {"version": version, "history": history, "sourceRevision": version,
                       "status": "observed", "stage": "timingObservation",
                       "boundaries": [{"phase": "measurement", "round": 0, "sampleId": "sample",
                           "messageCount": 1, "contentBytes": 1, "semanticSha256": "content",
                           "toolsSha256": "tools"}],
                       "client": [{"phase": "verified", "cohort": "measured", "sampleId": "sample",
                           "agentRunId": "run", "terminalMs": 3, "admissionMs": 1, "firstTextMs": 2}],
                       "preparation": []}
                if version == "baseline" and history == 10000:
                    arm = {key: value for key, value in arm.items()
                           if key not in {"client", "boundaries", "preparation"}}
                    arm.update(status="failedPreserved", stage="historySetup", failureType="RuntimeError")
                arms.append(arm)
        with TemporaryDirectory(prefix="working-set-comparison-") as folder:
            paths = [Path(folder) / f"{index}.json" for index in range(4)]
            for path, arm in zip(paths, arms):
                path.write_text(json.dumps(arm), encoding="utf-8")
            report = PG.compare(paths)
        self.assertFalse(report["matchedAllMeasuredPairs"])
        self.assertEqual(report["arms"][1]["status"], "failedPreserved")
        self.assertEqual(report["pairs"][0]["matchedWorkingSets"], 0)
        self.assertIsNone(report["pairs"][0]["latency"][0]["terminalMsP95"])
        self.assertEqual(report["pairs"][1]["matchedWorkingSets"], 1)
        self.assertEqual(report["pairs"][1]["latency"][1]["terminalMsP95"], 3)


class WorkingSetSaturationTests(unittest.TestCase):
    def driver(self, events, bad_run=None, invalid=None):
        def paused(label, admitted):
            events.append(("paused", admitted["sessionId"], admitted["agentRunId"]))
            return {**admitted, "environmentId": "env-" + admitted["sessionId"],
                    "instanceId": "instance", "containerId": "container", "hostId": "host",
                    "controllerEpoch": 1}

        def retire(observed):
            events.append(("retired", observed["sessionId"], observed["agentRunId"]))
            return {"environmentId": observed["environmentId"], "status": "destroyed",
                    "containerAbsent": True, "residentHoldingCount": 0, "historyPreserved": True,
                    **(invalid if observed["agentRunId"] == bad_run else {})}

        return SimpleNamespace(paused=paused, retire=retire)

    def released(self, session, run):
        return {"phase": "released", "sessionId": session, "agentRunId": run}

    def test_retirement_uses_only_each_sessions_last_released_run(self):
        events, report = [], {}
        rows = [self.released("first", "old"), self.released("second", "other"),
                self.released("first", "final"),
                {"phase": "accepted", "sessionId": "first", "agentRunId": "notReleased"},
                {"phase": "accepted", "sessionId": "unreleased", "agentRunId": "pending"}]
        PG.retire_released_sessions(self.driver(events), "comparison", rows, report)
        self.assertEqual(events, [("paused", "first", "final"), ("retired", "first", "final"),
                                  ("paused", "second", "other"), ("retired", "second", "other")])
        self.assertEqual([(row["sessionId"], row["agentRunId"]) for row in report["retirement"]],
                         [("first", "final"), ("second", "other")])
        self.assertEqual(report["retirement"][0]["observation"]["environmentId"],
                         report["retirement"][0]["receipt"]["environmentId"])

    def exercise_saturation(self, driver, events, report):
        records = [self.released("comparison", f"comparison-{index}") for index in range(120)]

        def cohort(_driver, _binary, _output, _session, _control, mode, vus):
            self.assertEqual(mode, "saturation")
            events.append(("k6", vus))
            return [self.released(f"sat{vus}-vu{vu}", f"sat{vus}-vu{vu}-run{iteration}")
                    for iteration in range(1, 7) for vu in range(1, vus + 1)]

        with mock.patch.object(PG, "k6_run", side_effect=cohort):
            PG.saturation_runs(driver, SimpleNamespace(k6="unused"), None, "comparison", "unused",
                               SimpleNamespace(vus=1), report, records)
        return records

    def test_every_cohort_retires_before_next_cohort_including_final_eight(self):
        events, report = [], {}
        records = self.exercise_saturation(self.driver(events), events, report)
        expected = [("paused", "comparison", "comparison-119"),
                    ("retired", "comparison", "comparison-119")]
        for vus in (1, 2, 4, 8):
            expected.append(("k6", vus))
            for vu in range(1, vus + 1):
                expected.extend([(action, f"sat{vus}-vu{vu}", f"sat{vus}-vu{vu}-run6")
                                 for action in ("paused", "retired")])
        self.assertEqual(events, expected)
        self.assertEqual(len(records), 210)
        self.assertEqual(len(report["retirement"]), 16)

    def test_invalid_retirement_receipt_is_retained_and_stops_next_cohort(self):
        for invalid in ({"status": "pending"}, {"containerAbsent": False},
                        {"residentHoldingCount": 1}, {"historyPreserved": False}):
            with self.subTest(invalid=invalid):
                events, report = [], {}
                driver = self.driver(events, "sat1-vu1-run6", invalid)
                with self.assertRaisesRegex(RuntimeError, "positive product retirement"):
                    self.exercise_saturation(driver, events, report)
                self.assertEqual([event for event in events if event[0] == "k6"], [("k6", 1)])
                receipt = report["retirement"][-1]
                self.assertEqual(receipt["agentRunId"], "sat1-vu1-run6")
                for key, value in invalid.items():
                    self.assertEqual(receipt["receipt"][key], value)


if __name__ == "__main__":
    unittest.main()
