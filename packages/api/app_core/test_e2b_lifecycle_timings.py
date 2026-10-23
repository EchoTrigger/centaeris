"""Offline contracts for acceptance-side lifecycle call timing."""
import itertools
import json
import threading
import unittest
from unittest.mock import patch

from .e2b_lifecycle_timing_test_support import LifecycleCallCollector


class PrivateValue:
    def __repr__(self):
        raise AssertionError("private value inspected")
    def __str__(self):
        raise AssertionError("private value inspected")


class PrivateFailure(BaseException):
    def __repr__(self):
        raise AssertionError("private failure inspected")
    def __str__(self):
        raise AssertionError("private failure inspected")


class LifecycleCallTimingTests(unittest.TestCase):
    def collector(self, ticks=(1_000_000, 5_000_000), **kwargs):
        clock = iter(ticks)
        return LifecycleCallCollector(monotonic_ns=lambda: next(clock),
            wall_time_ns=lambda: 1_700_000_000_000_000_000, **kwargs)

    def test_success_preserves_exact_arguments_and_result_without_inspecting_body(self):
        collector = self.collector()
        body, response, calls = PrivateValue(), object(), []
        def request(*args, **kwargs):
            calls.append((args, kwargs))
            return response
        result = collector.wrap("runtime", request)("/internal/jobs/claim", body, timeout=3)
        self.assertIs(result, response)
        self.assertEqual(calls, [(("/internal/jobs/claim", body), {"timeout": 3})])
        report = collector.summary()
        self.assertEqual(report["callsStarted"], 1)
        self.assertEqual(report["callsCompleted"], 1)
        self.assertEqual(report["coverageStatus"], "complete")
        self.assertEqual(report["records"][0], {
            "kind": "call", "sequence": 1, "lane": "runtime", "endpoint": "claim",
            "startedMonotonicNs": 1_000_000, "finishedMonotonicNs": 5_000_000,
            "startedAtUnixMs": 1_700_000_000_000, "finishedAtUnixMs": 1_700_000_000_000,
            "elapsedMs": 4.0, "outcome": "ok"})

    def test_failures_propagate_the_original_instance_once_without_reading_error(self):
        collector = self.collector()
        failure, calls = PrivateFailure(), []
        def request(*args, **kwargs):
            calls.append(True)
            raise failure
        try:
            collector.wrap("api", request)("/internal/agent-run-lifecycle/resolve", PrivateValue())
        except BaseException as actual:
            self.assertIs(actual, failure)
        else:
            self.fail("business failure was swallowed")
        self.assertEqual(calls, [True])
        report = collector.summary()
        self.assertEqual(report["records"][0]["outcome"], "error")
        self.assertEqual(report["endpoints"][0]["outcomes"], {"ok": 0, "error": 1})
        self.assertNotIn("private", json.dumps(report))

    def test_path_classification_is_exact_closed_and_lane_bound_without_query_parsing(self):
        paths = {
            "runtime": {"/internal/jobs/claim": "claim", "/internal/jobs/control/claim": "claim",
                "/internal/jobs/start": "start", "/internal/jobs/control/start": "start",
                "/agent-runs/step": "step", "/agent-runs/teardown": "teardown",
                "/internal/jobs/yield": "yield", "/internal/jobs/heartbeat": "heartbeat",
                "/internal/jobs/wait": "wait"},
            "api": {"/internal/agent-run-lifecycle/resolve": "resolve",
                "/internal/agent-run-lifecycle/release": "release",
                "/internal/agent-runs/transition": "project"}}
        clock = itertools.count(1_000_000, 1_000_000)
        collector = LifecycleCallCollector(monotonic_ns=lambda: next(clock), wall_time_ns=lambda: 0)
        for lane, endpoints in paths.items():
            wrapped = collector.wrap(lane, lambda *args, **kwargs: None)
            for path, endpoint in endpoints.items():
                wrapped(path, PrivateValue())
                self.assertEqual(collector.summary()["records"][-1]["endpoint"], endpoint)
        unknown = ["/internal/jobs/claim?token=private-secret", "https://private-secret/agent-runs/step",
            "/internal/jobs/private-secret", "/private-secret/claim", PrivateValue(),
            "/internal/jobs/claim/", "/INTERNAL/jobs/claim"]
        wrapped = collector.wrap("runtime", lambda *args: None)
        for path in unknown:
            wrapped(path)
            self.assertEqual(collector.summary()["records"][-1]["endpoint"], "other")
        collector.wrap("api", lambda *args: None)("/agent-runs/step")
        self.assertEqual(collector.summary()["records"][-1]["endpoint"], "other")
        self.assertNotIn("private-secret", json.dumps(collector.summary()))

    def test_repeated_calls_remain_distinct_and_report_count_sum_min_max(self):
        collector = self.collector(ticks=(0, 1_000_000, 2_000_000, 5_000_000, 6_000_000, 8_000_000))
        wrapped = collector.wrap("runtime", lambda *args: None)
        for _ in range(3):
            wrapped("/internal/jobs/claim")
        report = collector.summary()
        self.assertEqual([row["sequence"] for row in report["records"]], [1, 2, 3])
        self.assertEqual(report["endpoints"], [{"lane": "runtime", "endpoint": "claim", "count": 3,
            "timedCount": 3, "sumElapsedMs": 6.0, "minElapsedMs": 1.0, "maxElapsedMs": 3.0,
            "outcomes": {"ok": 3, "error": 0}}])
        self.assertIs(report["durationsAdditive"], False)

    def test_keyword_path_and_calls_without_path_are_forwarded_unchanged(self):
        collector = self.collector(ticks=(0, 1, 2, 3))
        value, calls = object(), []
        wrapped = collector.wrap("runtime", lambda *args, **kwargs: calls.append((args, kwargs)) or value)
        self.assertIs(wrapped(path="/agent-runs/step", body=PrivateValue()), value)
        self.assertIs(wrapped(), value)
        self.assertEqual([row["endpoint"] for row in collector.summary()["records"]], ["step", "other"])
        self.assertEqual(calls[0][1]["path"], "/agent-runs/step")
        self.assertEqual(calls[1], ((), {}))

    def test_record_limit_retains_aggregate_call_counts_and_explicit_loss(self):
        collector = self.collector(ticks=range(6), max_records=1)
        wrapped = collector.wrap("runtime", lambda *args: None)
        for _ in range(3):
            wrapped("/agent-runs/step")
        report = collector.summary()
        self.assertEqual(len(report["records"]), 1)
        self.assertEqual(report["droppedRecords"], 2)
        self.assertEqual(report["endpoints"][0]["count"], 3)
        self.assertEqual(report["endpoints"][0]["timedCount"], 3)
        self.assertEqual(report["callsCompleted"], 3)
        self.assertEqual(report["coverageStatus"], "incomplete")
        self.assertIs(report["incomplete"], True)

    def test_broken_clocks_and_recording_do_not_change_success_or_business_failure(self):
        response = object()
        def broken():
            raise PrivateFailure()
        collector = LifecycleCallCollector(monotonic_ns=broken)
        self.assertIs(collector.wrap("runtime", lambda *args: response)("/agent-runs/step"), response)
        report = collector.summary()
        self.assertGreater(report["observationErrors"], 0)
        self.assertEqual(report["callsCompleted"], 1)
        self.assertEqual(report["records"], [])
        self.assertIsNone(report["endpoints"][0]["minElapsedMs"])
        failure = PrivateFailure()
        collector = self.collector()
        def failed(*args):
            raise failure
        with patch.object(collector, "_append_record", side_effect=PrivateFailure()):
            try:
                collector.wrap("runtime", failed)("/agent-runs/step")
            except BaseException as actual:
                self.assertIs(actual, failure)
            else:
                self.fail("business failure was swallowed")
        self.assertEqual(collector.summary()["coverageStatus"], "incomplete")

    def test_concurrent_calls_have_start_order_sequences_and_truthful_inflight_coverage(self):
        clock = itertools.count()
        collector = LifecycleCallCollector(monotonic_ns=lambda: next(clock), wall_time_ns=lambda: 0)
        entered, release = threading.Event(), threading.Event()
        def blocked(*args):
            entered.set()
            self.assertTrue(release.wait(2))
        thread = threading.Thread(target=collector.wrap("runtime", blocked), args=("/agent-runs/step",))
        thread.start()
        self.addCleanup(thread.join, 2)
        self.addCleanup(release.set)
        self.assertTrue(entered.wait(2))
        try:
            collector.wrap("api", lambda *args: None)("/internal/agent-run-lifecycle/resolve")
            pending = collector.summary()
            self.assertEqual(pending["inFlightCalls"], 1)
            self.assertEqual(pending["coverageStatus"], "incomplete")
        finally:
            release.set()
            thread.join(2)
        self.assertFalse(thread.is_alive())
        report = collector.summary()
        self.assertEqual([row["sequence"] for row in report["records"]], [1, 2])
        self.assertEqual([row["endpoint"] for row in report["records"]], ["step", "resolve"])
        self.assertEqual(report["coverageStatus"], "complete")

    def test_markers_share_clock_and_budget_but_never_count_as_business_calls(self):
        collector = self.collector(ticks=(0, 1, 2, 3, 4, 5))
        for name in ("sampleStart", "resumeRequested", "resumeReturned"):
            collector.marker(name)
        collector.wrap("runtime", lambda *args: None)("/agent-runs/step")
        collector.marker("sampleEnd")
        report = collector.summary()
        self.assertEqual(report["callsStarted"], 1)
        self.assertEqual([row["sequence"] for row in report["records"]], list(range(1, 6)))
        self.assertEqual(report["records"][2], {"kind": "marker", "sequence": 3,
            "name": "resumeReturned", "atMonotonicNs": 2, "atUnixMs": 1_700_000_000_000})
        self.assertEqual(report["records"][3]["finishedMonotonicNs"], 4)
        self.assertIn("firstToolAfterResumeElapsedMs", report["unavailableMetrics"])
        self.assertIn("runtimeStartupElapsedMs", report["unavailableMetrics"])
        self.assertIn("providerCreateElapsedMs", report["unavailableMetrics"])

    def test_unknown_markers_and_overflow_are_explicit_without_retaining_private_labels(self):
        collector = self.collector(ticks=range(4), max_records=1)
        collector.marker("sampleStart")
        collector.marker("private-secret")
        collector.marker(PrivateValue())
        collector.wrap("runtime", lambda *args: None)("/agent-runs/step")
        collector.marker("sampleEnd")
        report = collector.summary()
        self.assertEqual(report["rejectedMarkers"], 2)
        self.assertEqual(report["droppedRecords"], 2)
        self.assertEqual(report["coverageStatus"], "incomplete")
        self.assertNotIn("private-secret", json.dumps(report))

    def test_invalid_monotonic_observations_never_fabricate_zero_duration(self):
        for ticks in ((3, 2), (False, 1), (float("nan"), 2), (-1, 1)):
            with self.subTest(ticks=ticks):
                collector = self.collector(ticks=ticks)
                collector.wrap("runtime", lambda *args: None)("/agent-runs/step")
                report = collector.summary()
                self.assertEqual(report["records"], [])
                self.assertEqual(report["endpoints"][0]["timedCount"], 0)
                self.assertEqual(report["coverageStatus"], "incomplete")

    def test_summary_is_a_detached_snapshot_and_empty_coverage_is_not_observed(self):
        collector = self.collector()
        self.assertEqual(collector.summary()["observationStatus"], "notObserved")
        collector.wrap("runtime", lambda *args: None)("/agent-runs/step")
        first = collector.summary()
        first["records"][0]["endpoint"] = "private-secret"
        first["endpoints"][0]["outcomes"]["ok"] = 500
        self.assertEqual(collector.summary()["records"][0]["endpoint"], "step")
        self.assertEqual(collector.summary()["endpoints"][0]["outcomes"]["ok"], 1)

    def test_failed_begin_or_finish_observation_never_changes_business_dispatch(self):
        for observer in ("_begin", "_finish"):
            with self.subTest(observer=observer):
                collector = self.collector()
                response, calls = object(), []
                with patch.object(collector, observer, side_effect=PrivateFailure()):
                    result = collector.wrap("runtime", lambda *args: calls.append(True) or response)("/agent-runs/step")
                self.assertIs(result, response)
                self.assertEqual(calls, [True])
                self.assertEqual(collector.summary()["coverageStatus"], "incomplete")

    def test_lock_failure_keeps_business_result_and_reports_unavailable_snapshot(self):
        class BrokenLock:
            def __enter__(self):
                raise PrivateFailure()
            def __exit__(self, *args):
                return False
        collector = self.collector()
        response = object()
        with patch.object(collector, "_lock", BrokenLock()):
            self.assertIs(collector.wrap("runtime", lambda *args: response)("/agent-runs/step"), response)
            collector.marker("sampleStart")
            report = collector.summary()
        self.assertEqual(report["observationStatus"], "unavailable")
        self.assertIsNone(report["callsStarted"])
        self.assertIsNone(report["observationErrors"])
        self.assertEqual(report["coverageStatus"], "incomplete")

    def test_wall_clock_jump_does_not_change_monotonic_duration(self):
        ticks, walls = iter((2_000_000, 5_000_000)), iter((9_000_000, 1_000_000))
        collector = LifecycleCallCollector(monotonic_ns=lambda: next(ticks), wall_time_ns=lambda: next(walls))
        collector.wrap("runtime", lambda *args: None)("/agent-runs/step")
        record = collector.summary()["records"][0]
        self.assertEqual(record["elapsedMs"], 3.0)
        self.assertEqual((record["startedAtUnixMs"], record["finishedAtUnixMs"]), (9, 1))
        self.assertEqual(collector.summary()["coverageStatus"], "complete")
    def test_limits_lanes_and_callback_are_explicit_before_instrumentation(self):
        for limit in (False, 0, -1, 2049, 1.5):
            with self.subTest(limit=limit), self.assertRaises(ValueError):
                LifecycleCallCollector(max_records=limit)
        collector = self.collector()
        for lane in ("private-secret", None, PrivateValue()):
            with self.assertRaises(ValueError):
                collector.wrap(lane, lambda: None)
        with self.assertRaises(ValueError):
            collector.wrap("runtime", None)


if __name__ == "__main__":
    unittest.main()
