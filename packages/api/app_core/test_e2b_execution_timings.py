"""Pure offline contracts for strict execution timing observations."""
import hashlib
import json
from pathlib import Path
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from . import e2b_execution_timing_test_support as support
from .e2b_execution_timing_test_support import ExecutionTimingCollector

DOMAIN = "a" * 64
HELPERS = ("prepareWorkspace", "fileSystem", "inputInventory", "materializeInput", "revokeInput", "readArtifact",
    "snapshotCollect", "snapshotCapture", "snapshotRestore", "workspaceGeneration", "workspaceGenerationRpc",
    "workspaceWatch", "agentProcessState", "quiesceAgentProcesses")
HELPER_METRICS = ("mkdirElapsedMs", "uploadElapsedMs", "executeElapsedMs", "waitElapsedMs", "downloadElapsedMs",
    "cleanupElapsedMs", "controlElapsedMs", "totalElapsedMs")
DISPATCH_METRICS = ("checkoutElapsedMs", "authorityBeforeElapsedMs", "callbackElapsedMs", "authorityAfterElapsedMs",
    "commitElapsedMs", "connectionReturnElapsedMs", "totalElapsedMs")
MILESTONES = ("runStepEntered", "firstToolReceiptReady", "firstToolReceiptCommitted")


class PrivateValue:
    def __str__(self):
        raise AssertionError("private value inspected")
    __repr__ = __str__


class ObservationFailure(BaseException):
    pass


class ExecutionTimingTests(unittest.TestCase):
    def collector(self, **kwargs):
        collector = ExecutionTimingCollector(clock_domain=DOMAIN, **kwargs)
        collector.bind_run("run-a")
        return collector

    def event(self, phase="e2bHelper", **changes):
        event = {"schema": "workspace.perf.v1", "source": "sdk" if phase == "e2bSdk" else "runtime", "phase": phase,
            "atUnixMs": 1000, "jobId": "agent_run.lifecycle:run-a"}
        if phase == "e2bHelper":
            event.update(operationId="helper-local-1", helperKind="fileSystem", metric="executeElapsedMs", value=2.5, outcome="ok")
        elif phase == "providerDispatch":
            event.update(dispatchId="dispatch-local-1", dispatchKind="perRunExecution", metric="callbackElapsedMs", value=2.5, outcome="ok")
        elif phase == "e2bSdk":
            event.update(stage="sdkCreate", operation="create", value=2.5, outcome="ok")
        elif phase == "runMilestone":
            event.update(milestone="runStepEntered", runId="run-a", clockDomain=DOMAIN, atMonotonicNs=123)
        return event | changes

    def feed(self, collector, phase="e2bHelper", **changes):
        collector.feed_line(json.dumps(self.event(phase, **changes)))

    def test_all_helper_labels_and_metrics_remain_closed_and_aggregate_without_dynamic_identity_buckets(self):
        collector = self.collector()
        for index, helper in enumerate(HELPERS):
            for metric in HELPER_METRICS:
                for suffix in ("first", "second"):
                    self.feed(collector, helperKind=helper, metric=metric, operationId=f"helper-{index}-{suffix}", dispatchId="dispatch-local")
        summary = collector.summary()
        self.assertEqual(len(summary["aggregates"]), 112)
        self.assertEqual(len(summary["records"]), 224)
        aggregate = summary["aggregates"][0]
        self.assertEqual((aggregate["count"], aggregate["sumElapsedMs"], aggregate["minElapsedMs"], aggregate["maxElapsedMs"]), (2, 5, 2.5, 2.5))
        self.assertEqual(aggregate["outcomes"], {"ok": 2, "error": 0, "interrupted": 0})
        self.assertEqual(summary["records"][0]["agentRunId"], "run-a")
        self.assertEqual(summary["records"][0]["dispatchId"], "dispatch-local")
        self.assertNotIn("jobId", summary["records"][0])
        self.assertIs(summary["durationsAdditive"], False)

    def test_dispatch_kinds_preserve_separate_execution_and_cleanup_buckets(self):
        collector = self.collector()
        kinds = ("perRunExecution", "retainedExecution", "retainedRunCleanup")
        for kind in kinds:
            for index in range(2):
                self.feed(collector, "providerDispatch", dispatchKind=kind, dispatchId=f"dispatch-{kind}-{index}")
        summary = collector.summary()
        self.assertEqual(summary["counters"]["invalidRecords"], 0)
        self.assertEqual(len(summary["records"]), 6)
        self.assertEqual(len(summary["aggregates"]), 3)
        self.assertEqual({row["dispatchKind"] for row in summary["aggregates"]}, set(kinds))
        for row in summary["aggregates"]:
            self.assertEqual((row["count"], row["sumElapsedMs"]), (2, 5.0))
            self.assertNotIn("dispatchId", row)

    def test_dispatch_kind_is_required_closed_and_never_inferred(self):
        collector = self.collector()
        missing = self.event("providerDispatch")
        del missing["dispatchKind"]
        collector.feed_line(json.dumps(missing))
        invalid = (None, True, 1, [], {}, "", "retained", "private-secret")
        for value in invalid:
            self.feed(collector, "providerDispatch", dispatchKind=value)
        summary = collector.summary()
        self.assertEqual(summary["counters"]["invalidRecords"], 1 + len(invalid))
        self.assertEqual(summary["records"], [])
        self.assertEqual(summary["aggregates"], [])

    def test_dispatch_errors_and_interruptions_are_preserved_without_inventing_missing_commit_zero(self):
        collector = self.collector()
        for metric in DISPATCH_METRICS:
            for outcome in ("ok", "error", "interrupted"):
                self.feed(collector, "providerDispatch", metric=metric, outcome=outcome)
        summary = collector.summary()
        self.assertEqual(summary["counters"]["errorRecords"], 7)
        self.assertEqual(summary["counters"]["interruptedRecords"], 7)
        self.assertEqual(len(summary["aggregates"]), 7)
        self.assertEqual(summary["aggregates"][0]["outcomes"], {"ok": 1, "error": 1, "interrupted": 1})
        partial = self.collector()
        self.feed(partial, "providerDispatch", outcome="error")
        self.assertEqual([row["metric"] for row in partial.summary()["aggregates"]], ["callbackElapsedMs"])
        self.assertEqual(partial.summary()["coverageStatus"], "incomplete")

    def test_sdk_stage_operation_pairs_and_startup_have_safe_explicit_attribution(self):
        collector = self.collector()
        pairs = [("sdkCreate", "create"), ("sdkCreateVerifyGet", "create"), ("sdkCommandStart", "runCommand"),
            ("sdkCommandWait", "waitCommand"), ("sdkCommandWait", "recoverCommand"), ("sdkFileUpload", "uploadFile"),
            ("sdkFileDownload", "downloadFile")]
        pairs += [("sdkHandle", operation) for operation in ("create", "runCommand", "waitCommand", "recoverCommand", "uploadFile", "downloadFile")]
        for stage, operation in pairs:
            self.feed(collector, "e2bSdk", stage=stage, operation=operation)
        for stage in ("sdkImport", "sdkClientSetup"):
            event = self.event("e2bSdk", stage=stage, operation="startup")
            del event["jobId"]
            collector.feed_line(json.dumps(event))
        summary = collector.summary()
        self.assertEqual(len(summary["records"]), 15)
        self.assertEqual(len(summary["aggregates"]), 15)
        self.assertEqual(summary["counters"]["unassignedRecords"], 2)
        self.assertEqual(summary["records"][-1]["attribution"], "unassigned")
        self.assertIsNone(summary["records"][-1]["agentRunId"])

    def test_milestones_preserve_clock_domain_and_absence_instead_of_cross_clock_arithmetic(self):
        collector = self.collector()
        for index, milestone in enumerate(MILESTONES):
            event = self.event("runMilestone", milestone=milestone, atMonotonicNs=100 + index)
            if index:
                event["callId"] = "e2b-1-0"
            del event["jobId"]
            collector.feed_line(json.dumps(event))
        summary = collector.summary()
        self.assertEqual(summary["clockDomain"], DOMAIN)
        self.assertEqual(summary["coverageStatus"], "complete")
        self.assertEqual(summary["runCoverage"][0]["missingMilestones"], [])
        self.assertEqual([row["atMonotonicNs"] for row in summary["records"]], [100, 101, 102])
        self.assertNotIn("sumElapsedMs", summary["aggregates"][0])
        self.feed(collector, "runMilestone", clockDomain=None, atMonotonicNs=None)
        self.feed(collector, "runMilestone", clockDomain="b" * 64)
        summary = collector.summary()
        self.assertEqual(summary["counters"]["uncorrelatedMilestones"], 2)
        self.assertIsNone(summary["records"][-2]["atMonotonicNs"])
        self.assertEqual(summary["records"][-1]["clockDomain"], "b" * 64)
        self.assertEqual(summary["coverageStatus"], "incomplete")

    def test_foreign_jobs_or_conflicting_milestone_run_are_not_relabelled(self):
        collector = self.collector()
        for phase in ("e2bHelper", "providerDispatch", "e2bSdk"):
            self.feed(collector, phase, jobId="agent_run.lifecycle:private-foreign")
            event = self.event(phase)
            del event["jobId"]
            collector.feed_line(json.dumps(event))
        self.feed(collector, "runMilestone", runId="private-foreign", jobId="agent_run.lifecycle:private-foreign")
        self.feed(collector, "runMilestone", jobId="agent_run.lifecycle:run-b")
        summary = collector.summary()
        self.assertEqual(summary["counters"]["foreignRecords"], 5)
        self.assertEqual(summary["counters"]["unassignedRecords"], 3)
        self.assertEqual(len(summary["records"]), 3)
        self.assertTrue(all(row["agentRunId"] is None for row in summary["records"]))
        self.assertNotIn("private-foreign", json.dumps(summary))

    def test_sdk_reader_loss_is_counted_without_turning_missing_records_into_elapsed_time(self):
        collector = self.collector()
        event = {"schema": "workspace.perf.v1", "source": "runtime", "phase": "executionObservationLoss",
            "metric": "sdkDroppedRecords", "value": 7, "atUnixMs": 1000, "jobId": "agent_run.lifecycle:run-a"}
        collector.feed_line(json.dumps(event))
        summary = collector.summary()
        self.assertEqual(summary["counters"]["sdkDroppedRecords"], 7)
        self.assertEqual(summary["coverageStatus"], "incomplete")
        self.assertNotIn("sumElapsedMs", summary["aggregates"][0])
        self.assertEqual(summary["aggregates"][0]["droppedRecords"], 7)
        for value in (True, -1, 2**64, 1.5, "private-secret"):
            invalid = self.collector()
            invalid.feed_line(json.dumps(event | {"value": value}))
            self.assertEqual(invalid.summary()["counters"]["invalidRecords"], 1)
            self.assertEqual(invalid.summary()["counters"]["sdkDroppedRecords"], 0)
        collector.feed_line(json.dumps(event | {"private": "private-secret"}))
        self.assertEqual(collector.summary()["counters"]["invalidRecords"], 1)
        self.assertNotIn("private-secret", json.dumps(collector.summary()))

    def test_limits_binding_identity_and_dropped_bindings_are_explicit(self):
        for kwargs in ({"max_records": False}, {"max_records": 0}, {"max_records": 2049}, {"max_runs": 0},
                {"max_runs": 33}, {"max_runs": True}, {"clock_domain": "private/path"}):
            with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                ExecutionTimingCollector(**kwargs)
        collector = ExecutionTimingCollector(max_runs=1, clock_domain=None)
        collector.bind_run("run-a")
        collector.bind_run("run-a")
        collector.bind_run("run-b")
        for value in (None, PrivateValue(), "private/path", "x" * 161):
            collector.bind_run(value)
        summary = collector.summary()
        self.assertEqual(summary["counters"]["droppedBindings"], 1)
        self.assertEqual(summary["counters"]["rejectedBindings"], 4)
        self.assertIsNone(summary["clockDomain"])
        self.assertEqual(len(summary["runCoverage"]), 1)
        self.assertEqual(summary["runCoverage"][0]["missingMilestones"], list(MILESTONES))

    def test_closed_known_shapes_reject_extra_private_fields_nonfinite_numbers_and_wrong_labels(self):
        cases = []
        for phase in ("e2bHelper", "providerDispatch", "e2bSdk", "runMilestone"):
            cases += [self.event(phase, private="private-secret"), self.event(phase, schema="old"), self.event(phase, source="private-secret"),
                self.event(phase, atUnixMs=True), self.event(phase, atUnixMs=-1), self.event(phase, atUnixMs=2**64),
                self.event(phase, atUnixMs=1.5), self.event(phase, jobId=None), self.event(phase, jobId="private/path")]
        for phase in ("e2bHelper", "providerDispatch", "e2bSdk"):
            cases += [self.event(phase, value=value) for value in (True, -1, float("nan"), float("inf"), 2**64, "private-secret")]
            cases += [self.event(phase, outcome="private-secret")]
        cases += [self.event(helperKind="private-secret"), self.event(metric="private-secret"), self.event(operationId="private/path"),
            self.event(dispatchId=None), self.event("providerDispatch", dispatchId="private/path"),
            self.event("providerDispatch", metric="private-secret"), self.event("e2bSdk", stage="private-secret"),
            self.event("e2bSdk", stage="sdkCreate", operation="runCommand"), self.event("e2bSdk", outcome="interrupted"),
            self.event("e2bSdk", requestId="rpc-secret"), self.event("runMilestone", callId="unexpected"),
            self.event("runMilestone", milestone="firstToolReceiptReady"), self.event("runMilestone", milestone="private-secret"),
            self.event("runMilestone", clockDomain=None), self.event("runMilestone", atMonotonicNs=None),
            self.event("runMilestone", clockDomain="A" * 64), self.event("runMilestone", atMonotonicNs=True)]
        for index, event in enumerate(cases):
            with self.subTest(index=index):
                collector = self.collector()
                collector.feed_line(json.dumps(event))
                summary = collector.summary()
                self.assertEqual(summary["counters"]["invalidRecords"], 1)
                self.assertEqual(summary["records"], [])
                self.assertNotIn("private-secret", json.dumps(summary))

    def test_duplicate_keys_unknown_fields_and_malformed_known_lines_never_copy_raw_content(self):
        collector = self.collector()
        line = json.dumps(self.event())
        for suffix in ('"value":2.5', '"phase":"private-phase"', '"jobId":"agent_run.lifecycle:run-a"'):
            collector.feed_line(line[:-1] + "," + suffix + "}")
        collector.feed_line(line[:-1])
        summary = collector.summary()
        self.assertEqual(summary["counters"]["invalidRecords"], 4)
        self.assertEqual(summary["records"], [])
        self.assertNotIn("private-phase", json.dumps(summary))

    def test_oversize_unknown_phases_and_non_string_input_have_no_private_record(self):
        collector = self.collector()
        collector.feed_line(json.dumps(self.event(private="private-secret" * 400)))
        collector.feed_line(json.dumps(self.event(phase="unrelatedPhase", private="private-secret")))
        for line in ("private-secret", "ready\n", '["private-secret"]', None, PrivateValue()):
            collector.feed_line(line)
        summary = collector.summary()
        self.assertEqual(summary["counters"]["oversizedLines"], 1)
        self.assertEqual(summary["counters"]["invalidRecords"], 0)
        self.assertEqual(summary["records"], [])
        self.assertNotIn("private-secret", json.dumps(summary))

    def test_record_budget_keeps_repeated_aggregate_counts_and_only_bounded_safe_raw_ids(self):
        collector = self.collector(max_records=2)
        for index in range(10):
            self.feed(collector, operationId=f"helper-{index}", value=index)
        summary = collector.summary()
        self.assertEqual(len(summary["records"]), 2)
        self.assertEqual(summary["counters"]["droppedRecords"], 8)
        self.assertEqual(len(summary["aggregates"]), 1)
        self.assertEqual(summary["aggregates"][0]["count"], 10)
        self.assertEqual(summary["aggregates"][0]["sumElapsedMs"], 45)
        self.assertEqual(summary["aggregates"][0]["minElapsedMs"], 0)
        self.assertEqual(summary["aggregates"][0]["maxElapsedMs"], 9)
        self.assertEqual(summary["coverageStatus"], "incomplete")
        summary["records"][0]["metric"] = "private-secret"
        summary["aggregates"][0]["outcomes"]["ok"] = 100
        self.assertEqual(collector.summary()["aggregates"][0]["outcomes"]["ok"], 10)
        self.assertNotIn("private-secret", json.dumps(collector.summary()))

    def test_concurrent_observation_is_bounded_and_does_not_lose_aggregate_counts(self):
        collector = self.collector(max_records=2)
        threads = [threading.Thread(target=self.feed, args=(collector,)) for _ in range(16)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(2)
            self.assertFalse(thread.is_alive())
        summary = collector.summary()
        self.assertEqual(summary["aggregates"][0]["count"], 16)
        self.assertEqual(summary["counters"]["droppedRecords"], 14)
        self.assertEqual([row["sequence"] for row in summary["records"]], [1, 2])

    def test_parser_and_lock_failures_cannot_escape_optional_observation(self):
        collector = self.collector()
        with patch.object(support, "_decode", side_effect=ObservationFailure(), create=True):
            self.feed(collector)
        self.assertEqual(collector.summary()["counters"]["observationErrors"], 1)
        class BrokenLock:
            def __enter__(self):
                raise ObservationFailure()
            def __exit__(self, *_args):
                return False
        with patch.object(collector, "_lock", BrokenLock()):
            collector.bind_run("run-a")
            self.feed(collector)
            summary = collector.summary()
        self.assertEqual(summary["coverageStatus"], "incomplete")
        self.assertEqual(summary["observationStatus"], "unavailable")
        self.assertIsNone(summary["counters"])
        self.assertEqual(summary["records"], [])


class ClockDomainTests(unittest.TestCase):
    def test_linux_clock_monotonic_domain_hashes_stripped_boot_identity_and_time_namespace(self):
        boot_id = "123e4567-e89b-12d3-a456-426614174000"
        with patch.object(support.sys, "platform", "linux"), patch.object(support.time, "get_clock_info",
                return_value=SimpleNamespace(implementation="clock_gettime(CLOCK_MONOTONIC)")), \
                patch.object(Path, "read_text", return_value=" " + boot_id + "\n"), \
                patch.object(support.os, "stat", return_value=SimpleNamespace(st_ino=12345)), \
                patch.object(support.time, "CLOCK_MONOTONIC", 1, create=True):
            summary = ExecutionTimingCollector().summary()
        self.assertEqual(summary["clockDomain"], hashlib.sha256((boot_id + ":12345").encode()).hexdigest())

    def test_unsupported_clock_or_failed_boot_namespace_detection_is_explicitly_unavailable(self):
        for platform, implementation, read, inode in (("win32", "clock_gettime(CLOCK_MONOTONIC)", "boot", 1),
                ("linux", "clock_gettime(CLOCK_MONOTONIC_RAW)", "boot", 1), ("linux", "clock_gettime(CLOCK_MONOTONIC)", "", 1),
                ("linux", "clock_gettime(CLOCK_MONOTONIC)", "boot", None)):
            with self.subTest(platform=platform, implementation=implementation), patch.object(support.sys, "platform", platform), \
                    patch.object(support.time, "get_clock_info", return_value=SimpleNamespace(implementation=implementation)), \
                    patch.object(Path, "read_text", return_value=read), patch.object(support.os, "stat", return_value=SimpleNamespace(st_ino=inode)):
                self.assertIsNone(ExecutionTimingCollector().summary()["clockDomain"])
        with patch.object(support.sys, "platform", "linux"), patch.object(support.time, "get_clock_info", side_effect=ObservationFailure()):
            self.assertIsNone(ExecutionTimingCollector().summary()["clockDomain"])
        for failing in ("boot", "namespace"):
            with self.subTest(failing=failing), patch.object(support.sys, "platform", "linux"), \
                    patch.object(support.time, "CLOCK_MONOTONIC", 1, create=True), \
                    patch.object(support.time, "get_clock_info", return_value=SimpleNamespace(implementation="clock_gettime(CLOCK_MONOTONIC)")), \
                    patch.object(Path, "read_text", return_value="123e4567-e89b-12d3-a456-426614174000", side_effect=ObservationFailure() if failing == "boot" else None), \
                    patch.object(support.os, "stat", return_value=SimpleNamespace(st_ino=1), side_effect=ObservationFailure() if failing == "namespace" else None):
                self.assertIsNone(ExecutionTimingCollector().summary()["clockDomain"])
