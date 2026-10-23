"""Offline contracts for independently bound, non-additive power measurements."""
import json
import math
from django.test import SimpleTestCase

from .e2b_power_timing_test_support import (PowerTimingCollector, PowerTimingMultiplexer,
    require_power_timing_coverage)
from .e2b_workspace_test_support import WorkspaceObservationCollector

RUNTIME = ("storePrepareElapsedMs", "storeDispatchCasElapsedMs", "storeSideEffectPinsElapsedMs",
    "storeCallbackElapsedMs", "storeAckCommitElapsedMs", "storeAdvanceElapsedMs", "contextDropElapsedMs",
    "adapterSpawnElapsedMs", "adapterHandshakeElapsedMs", "adapterRoundTripElapsedMs", "adapterCloseElapsedMs")
SDK = ("sdkImportElapsedMs", "sdkClientSetupElapsedMs", "sdkIdentityGetElapsedMs", "sdkHandleElapsedMs", "sdkHandleElapsedMs")


class PowerTimingCollectorTests(SimpleTestCase):
    def event(self, operation="pause-operation", action="pause", metric="storePrepareElapsedMs", **changes):
        return {"schema": "workspace.perf.v1", "phase": "providerPower", "source": "runtime",
            "operationId": operation, "action": action, "metric": metric, "value": 1.0,
            "outcome": "ok", "atUnixMs": 100} | changes

    def feed(self, collector, **changes):
        collector.feed(json.dumps(self.event(**changes)))

    def report(self, collector, **changes):
        return collector.summary(**({"lifecycle_passed": True, "stream_drained": True} | changes))

    def fresh(self, collector, operation="pause-operation", action="pause", *, close=True, values=None):
        values = values or {}
        for metric in RUNTIME:
            self.feed(collector, operation=operation, action=action, metric=metric, value=values.get(metric, 1.0))
        for metric in SDK + (("sdkPauseElapsedMs",) if action == "pause" else ("sdkConnectElapsedMs", "sdkResumeVerifyGetElapsedMs")):
            self.feed(collector, operation=operation, action=action, metric=metric, source="sdk", value=values.get(metric, 1.0))
        if close:
            self.feed(collector, operation=operation, action=action, metric="runtimePowerElapsedMs", value=values.get("runtimePowerElapsedMs", 1.0))

    def test_observed_zero_and_missing_sdk_are_distinct_without_invented_values(self):
        collector = PowerTimingCollector()
        collector.bind("pause-operation", "pause")
        self.feed(collector, metric="runtimePowerElapsedMs", value=0)
        report = self.report(collector)
        invocation = report["operations"][0]["invocations"][0]
        self.assertEqual(invocation["records"][0]["value"], 0)
        self.assertEqual(invocation["metricCoverage"]["sdkImportElapsedMs"], "notObserved")
        self.assertEqual(report["coverageStatus"], "incomplete")
        with self.assertRaises(AssertionError):
            require_power_timing_coverage(report)

    def test_complete_fresh_and_replay_observations_remain_separate_ordered_invocations(self):
        collector = PowerTimingCollector()
        collector.bind("pause-operation", "pause")
        self.fresh(collector)
        collector.bind("resume-operation", "resume")
        self.fresh(collector, "resume-operation", "resume")
        collector.bind_replay("pause-operation")
        for metric in ("storePrepareElapsedMs", "storeAdvanceElapsedMs", "runtimePowerElapsedMs"):
            self.feed(collector, metric=metric, outcome="replayed")
        report = self.report(collector)
        self.assertIsNone(require_power_timing_coverage(report))
        pause, resume = report["operations"]
        self.assertEqual([row["kind"] for row in pause["invocations"]], ["fresh", "replay"])
        self.assertEqual([row["kind"] for row in resume["invocations"]], ["fresh"])
        self.assertEqual(len(pause["invocations"][1]["records"]), 3)
        first = pause["invocations"][0]
        self.assertEqual(len([row for row in first["records"] if row["metric"] == "sdkHandleElapsedMs"]), 2)
        self.assertLess(first["records"][-1]["sequence"], resume["invocations"][0]["records"][0]["sequence"])
        self.assertLess(resume["invocations"][0]["records"][-1]["sequence"], pause["invocations"][1]["records"][0]["sequence"])

    def test_nested_parent_and_child_durations_are_raw_records_without_additive_summary(self):
        collector = PowerTimingCollector()
        collector.bind("pause-operation", "pause")
        values = {"runtimePowerElapsedMs": 100, "storeAdvanceElapsedMs": 90,
            "storeCallbackElapsedMs": 70, "sdkPauseElapsedMs": 30}
        self.fresh(collector, values=values)
        report = self.report(collector)
        self.assertEqual(report["aggregation"], "recordsOnly")
        self.assertFalse(report["durationsAdditive"])
        self.assertEqual(report["spanSemantics"], "nestedAndOverlapping")
        rows = report["operations"][0]["invocations"][0]["records"]
        self.assertEqual({row["metric"]: row["value"] for row in rows if row["metric"] in values}, values)

    def test_foreign_unknown_fields_invalid_sources_and_nonfinite_values_are_not_retained(self):
        collector = PowerTimingCollector()
        collector.bind("pause-operation", "pause")
        private = "private-fixture-credential"
        for changes in ({"authorization": private}, {"metric": private}, {"source": private},
                        {"action": private}, {"value": private}, {"outcome": private},
                        {"source": []}, {"metric": {}}, {"value": True}, {"value": -1},
                        {"value": math.nan}, {"value": math.inf}, {"value": 10**300}, {"value": 10**1000},
                        {"source": "sdk"}, {"atUnixMs": True}, {"schema": "old"},
                        {"operationId": "foreign-owned-looking"}, {"action": "resume"}):
            self.feed(collector, **changes)
        collector.feed('{"schema":"workspace.perf.v1","phase":"providerPower",broken')
        duplicate = json.dumps(self.event()).replace('"value": 1.0', '"value": 1.0, "value": 2')
        collector.feed(duplicate)
        collector.feed("private unstructured diagnostics")
        report = self.report(collector)
        self.assertEqual(report["operations"][0]["invocations"][0]["records"], [])
        self.assertNotIn(private, json.dumps(report))
        self.assertNotIn("foreign-owned-looking", json.dumps(report))
        self.assertGreater(report["rejectedRecords"], 0)
        self.assertGreater(report["foreignRecords"], 0)
        self.assertEqual(report["malformedRecords"], 2)
        self.assertTrue(report["lossDetected"])

    def test_record_operation_and_invocation_limits_are_bounded_and_report_loss(self):
        collector = PowerTimingCollector(max_records=2, max_operations=1, max_invocations=1)
        collector.bind("pause-operation", "pause")
        self.feed(collector)
        self.feed(collector, metric="storeAdvanceElapsedMs")
        self.feed(collector, metric="runtimePowerElapsedMs")
        collector.bind("other-operation", "resume")
        collector.bind_replay("pause-operation")
        report = self.report(collector)
        self.assertEqual(report["droppedRecords"], 1)
        self.assertEqual(report["droppedBindings"], 1)
        self.assertEqual(report["droppedInvocations"], 1)
        self.assertEqual(len(report["operations"][0]["invocations"][0]["records"]), 2)
        self.assertTrue(report["lossDetected"])

    def test_unclosed_failed_or_undrained_streams_never_establish_complete_coverage(self):
        collector = PowerTimingCollector()
        collector.bind("pause-operation", "pause")
        self.fresh(collector, close=False)
        self.assertEqual(self.report(collector)["coverageStatus"], "incomplete")
        self.feed(collector, metric="runtimePowerElapsedMs")
        for changes in ({"stream_drained": False}, {"lifecycle_passed": False}):
            report = self.report(collector, **changes)
            self.assertEqual(report["coverageStatus"], "incomplete")
            with self.assertRaises(AssertionError):
                require_power_timing_coverage(report)

    def test_undeclared_replays_and_replay_sdk_work_are_not_silently_combined(self):
        collector = PowerTimingCollector()
        collector.bind("pause-operation", "pause")
        self.fresh(collector)
        self.feed(collector)
        self.feed(collector, metric="runtimePowerElapsedMs")
        self.assertEqual(self.report(collector)["coverageStatus"], "incomplete")
        collector.bind_replay("pause-operation")
        for metric in ("storePrepareElapsedMs", "storeAdvanceElapsedMs", "runtimePowerElapsedMs"):
            self.feed(collector, metric=metric)
        self.assertEqual(len(self.report(collector)["operations"][0]["invocations"]), 2)
        self.assertEqual(self.report(collector)["operations"][0]["invocations"][0]["coverageStatus"], "incomplete")
        late = PowerTimingCollector()
        late.bind("pause-operation", "pause")
        self.fresh(late)
        late.bind_replay("pause-operation")
        self.feed(late, source="sdk", metric="sdkPauseElapsedMs")
        for metric in ("storePrepareElapsedMs", "storeAdvanceElapsedMs", "runtimePowerElapsedMs"):
            self.feed(late, metric=metric)
        report = self.report(late)
        self.assertEqual(report["operations"][0]["invocations"][1]["excessMetrics"], ["sdkPauseElapsedMs"])
        with self.assertRaises(AssertionError):
            require_power_timing_coverage(report)

    def test_late_sdk_records_after_parent_completion_remain_in_the_declared_fresh_invocation(self):
        collector = PowerTimingCollector()
        collector.bind("pause-operation", "pause")
        for metric in RUNTIME + ("runtimePowerElapsedMs",):
            self.feed(collector, metric=metric)
        for metric in SDK + ("sdkPauseElapsedMs",):
            self.feed(collector, metric=metric, source="sdk")
        self.assertIsNone(require_power_timing_coverage(self.report(collector)))
        self.assertEqual(self.report(collector)["foreignRecords"], 0)

    def test_multiplexer_preserves_workspace_report_and_delegates_overflow(self):
        workspace, reference, power = WorkspaceObservationCollector(), WorkspaceObservationCollector(), PowerTimingCollector()
        combined = PowerTimingMultiplexer(workspace, power)
        combined.bind("run-a", "agent_run.lifecycle:run-a")
        reference.bind("run-a", "agent_run.lifecycle:run-a")
        event = json.dumps({"schema": "workspace.perf.v1", "source": "runtime", "phase": "workspaceProtection",
            "operationId": "capture", "metric": "uploadedBytes", "value": 1, "outcome": "ok", "atUnixMs": 1,
            "trigger": "terminalCommit", "runId": "run-a", "jobId": "agent_run.lifecycle:run-a"})
        combined.feed(event)
        reference.feed(event)
        power.bind("pause-operation", "pause")
        combined.feed(json.dumps(self.event()))
        self.assertEqual(self.report(combined), self.report(reference))
        combined.oversized_line()
        reference.oversized_line()
        self.assertEqual(self.report(combined), self.report(reference))
        self.assertEqual(self.report(power)["oversizedLines"], 1)
        self.assertEqual(len(self.report(power)["operations"][0]["invocations"][0]["records"]), 1)
