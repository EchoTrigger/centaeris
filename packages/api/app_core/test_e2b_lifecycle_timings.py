"""Offline contracts for acceptance-side lifecycle call timing."""
import itertools
import json
import threading
import unittest
from unittest.mock import patch

from .e2b_lifecycle_timing_test_support import LifecycleCallCollector, feed_lifecycle_startup_line


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
        self.assertIn("runtimeProcessReadyElapsedMs", report["unavailableMetrics"])
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



class LifecycleStartupTimingTests(unittest.TestCase):
    fields = ("serverCount", "credentialCount", "mcpConnectionCount", "runtimePreludeMs",
        "sandboxEnsureMs", "mcpCredentialResolveMs", "mcpDiscoveryMs", "workspaceRestoreMs",
        "sessionStartCommitMs", "startupTotalMs")

    def line(self, run="run-a", **changes):
        values = {"serverCount": 0, "credentialCount": 0, "mcpConnectionCount": 0,
            "runtimePreludeMs": 10, "sandboxEnsureMs": 100, "mcpCredentialResolveMs": 0,
            "mcpDiscoveryMs": 0, "workspaceRestoreMs": 20, "sessionStartCommitMs": 5,
            "startupTotalMs": 140} | changes
        return "agent_run_startup_profile: agentRunId=" + run + "; " + "; ".join(
            key + "=" + str(values[key]) for key in self.fields)

    def test_bound_startup_records_are_preserved_with_explicit_missing_run_coverage(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        collector.bind_run("run-b")
        collector.feed_startup_line(self.line() + "\n")
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["coverageStatus"], "incomplete")
        self.assertEqual(summary["missingRunIds"], ["run-b"])
        collector.feed_startup_line(self.line("run-b", sandboxEnsureMs=90) + "\r\n")
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["coverageStatus"], "complete")
        self.assertEqual((summary["boundRunCount"], summary["observedRunCount"]), (2, 2))
        self.assertEqual(summary["missingRunIds"], [])
        self.assertEqual(summary["records"][0], {"agentRunId": "run-a", "serverCount": 0,
            "credentialCount": 0, "mcpConnectionCount": 0, "runtimePreludeMs": 10,
            "sandboxEnsureMs": 100, "mcpCredentialResolveMs": 0, "mcpDiscoveryMs": 0,
            "workspaceRestoreMs": 20, "sessionStartCommitMs": 5, "startupTotalMs": 140})
        self.assertIs(summary["durationsAdditive"], False)
        summary["records"][0]["sandboxEnsureMs"] = 999
        self.assertEqual(collector.summary()["startupProfiles"]["records"][0]["sandboxEnsureMs"], 100)

    def test_identical_and_conflicting_duplicates_never_overwrite_first_observation(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        collector.bind_run("run-a")
        collector.feed_startup_line(self.line())
        collector.feed_startup_line(self.line())
        collector.feed_startup_line(self.line(sandboxEnsureMs=101))
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["boundRunCount"], 1)
        self.assertEqual(summary["duplicateRecords"], 2)
        self.assertEqual(summary["conflictingRecords"], 1)
        self.assertEqual(summary["records"][0]["sandboxEnsureMs"], 100)
        self.assertEqual(summary["coverageStatus"], "incomplete")

    def test_unbound_or_forged_run_cannot_publish_startup_profile(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        collector.feed_startup_line(self.line("private-foreign-run"))
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["foreignRecords"], 1)
        self.assertEqual(summary["missingRunIds"], ["run-a"])
        self.assertEqual(summary["records"], [])
        self.assertNotIn("private-foreign-run", json.dumps(collector.summary()))

    def test_strict_startup_format_rejects_missing_extra_private_and_out_of_range_fields(self):
        malformed = [
            self.line() + "; private=private-secret", self.line() + " private-secret",
            self.line().replace("; serverCount=0", ""),
            self.line().replace("; credentialCount=0; mcpConnectionCount=0", "; mcpConnectionCount=0; credentialCount=0"),
            self.line().replace("runtimePreludeMs=10", "runtimePreludeMs=-1"),
            self.line().replace("runtimePreludeMs=10", "runtimePreludeMs=1.5"),
            self.line().replace("runtimePreludeMs=10", "runtimePreludeMs=True"),
            self.line().replace("runtimePreludeMs=10", "runtimePreludeMs=01"),
            self.line().replace("runtimePreludeMs=10", "runtimePreludeMs=" + str(2**64)),
            self.line().replace("serverCount=0", "serverCount=" + str(2**64)),
            self.line().replace("; serverCount=0", "; serverCount=0; serverCount=0"),
            self.line() + "\n\n", self.line() + "\r", self.line() + "\x00private-secret",
            self.line().replace("run-a", "run/a"), self.line().replace("serverCount", "server_count"),
            self.line().replace("; credentialCount=0", ";\tcredentialCount=0"),
        ]
        for line in malformed:
            with self.subTest(index=malformed.index(line)):
                collector = LifecycleCallCollector()
                collector.bind_run("run-a")
                collector.feed_startup_line(line)
                report = collector.summary()
                summary = report["startupProfiles"]
                self.assertEqual(summary["records"], [])
                self.assertEqual(summary["invalidRecords"], 1)
                self.assertEqual(summary["coverageStatus"], "incomplete")
                self.assertNotIn("private-secret", json.dumps(report))

    def test_unrelated_private_logs_are_ignored_and_oversized_profile_is_counted(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        for line in ("private-secret", "{\"token\":\"private-secret\"}", "e2b-product-ready:http://127.0.0.1:1", PrivateValue()):
            collector.feed_startup_line(line)
        collector.feed_startup_line(self.line() + "x" * 2048)
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["oversizedLines"], 1)
        self.assertEqual(summary["invalidRecords"], 0)
        self.assertEqual(summary["records"], [])
        self.assertNotIn("private-secret", json.dumps(collector.summary()))

    def test_binding_limit_is_explicit_and_foreign_profiles_cannot_expand_memory(self):
        collector = LifecycleCallCollector(max_startup_runs=1)
        collector.bind_run("run-a")
        collector.bind_run("run-b")
        collector.feed_startup_line(self.line())
        collector.feed_startup_line(self.line("run-b"))
        summary = collector.summary()["startupProfiles"]
        self.assertEqual((summary["boundRunCount"], summary["observedRunCount"]), (1, 1))
        self.assertEqual(summary["droppedBindings"], 1)
        self.assertEqual(summary["foreignRecords"], 1)
        self.assertEqual(summary["coverageStatus"], "incomplete")
        for maximum in (False, 0, -1, 33, 1.5):
            with self.subTest(maximum=maximum), self.assertRaises(ValueError):
                LifecycleCallCollector(max_startup_runs=maximum)

    def test_invalid_bindings_do_not_inspect_private_objects_or_retain_invalid_identities(self):
        collector = LifecycleCallCollector()
        for run in (None, PrivateValue(), "private/secret", "x" * 161):
            collector.bind_run(run)
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["rejectedBindings"], 4)
        self.assertEqual(summary["boundRunCount"], 0)
        self.assertNotIn("private", json.dumps(collector.summary()))

    def test_concurrent_duplicate_profiles_are_counted_once_without_losing_counters(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        threads = [threading.Thread(target=collector.feed_startup_line, args=(self.line(),)) for _ in range(16)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(2)
            self.assertFalse(thread.is_alive())
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["observedRunCount"], 1)
        self.assertEqual(summary["duplicateRecords"], 15)
        self.assertEqual(summary["conflictingRecords"], 0)

    def test_parser_and_lock_failure_cannot_interrupt_runtime_stream_or_real_call(self):
        import io
        from .e2b_workspace_test_support import collect_runtime_output
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        seen = []
        def on_line(line):
            feed_lifecycle_startup_line(collector, line)
            seen.append(line)
        with patch("app_core.e2b_lifecycle_timing_test_support._parse_startup_profile", side_effect=PrivateFailure(), create=True):
            collect_runtime_output(io.StringIO(self.line() + "\nready\n"), on_line, None)
        self.assertEqual(len(seen), 2)
        self.assertEqual(seen[-1], "ready\n")
        report = collector.summary()
        self.assertEqual(report["observationErrors"], 1)
        self.assertEqual(report["startupProfiles"]["missingRunIds"], ["run-a"])
        response = object()
        self.assertIs(collector.wrap("runtime", lambda *args: response)("/agent-runs/step"), response)

    def test_fixture_stream_feed_stays_open_even_when_observer_entrypoint_fails(self):
        import io
        from types import SimpleNamespace
        from .e2b_workspace_test_support import collect_runtime_output
        failed = []
        def broken_feed(_line):
            raise PrivateFailure()
        observer = SimpleNamespace(feed_startup_line=broken_feed,
            _startup_observation_failed=lambda: failed.append(True))
        seen = []
        def on_line(line):
            feed_lifecycle_startup_line(observer, line)
            seen.append(line)
        collect_runtime_output(io.StringIO(self.line() + "\nready\n"), on_line, None)
        self.assertEqual(len(failed), 2)
        self.assertEqual(seen[-1], "ready\n")
        observer._startup_observation_failed = lambda: (_ for _ in ()).throw(PrivateFailure())
        feed_lifecycle_startup_line(observer, "ready")
        feed_lifecycle_startup_line(None, PrivateValue())

    def test_startup_values_use_existing_rust_durations_without_reading_local_clocks(self):
        def forbidden_clock():
            raise PrivateFailure()
        collector = LifecycleCallCollector(monotonic_ns=forbidden_clock, wall_time_ns=forbidden_clock)
        collector.bind_run("run-a")
        collector.feed_startup_line(self.line())
        summary = collector.summary()["startupProfiles"]
        self.assertEqual(summary["coverageStatus"], "complete")
        self.assertEqual(summary["records"][0]["startupTotalMs"], 140)
        self.assertEqual(collector.summary()["observationErrors"], 0)


class LifecycleTransportTimingTests(unittest.TestCase):
    phases = tuple("e2bAdapter" + suffix for suffix in (
        "SpawnOrdinary", "SpawnControl", "SpawnCleanup", "HandshakeOrdinary", "HandshakeControl", "HandshakeCleanup",
        "CallCapabilities", "CallCreate", "CallAttach", "CallInspect", "CallKill", "CallPause", "CallConnect",
        "CallReadFile", "CallWriteFile", "CallUploadFile", "CallDownloadFile", "CallRunCommand", "CallWaitCommand",
        "CallRecoverCommand", "CallCancelCommand", "CallInvokeControlHelper"))

    def event(self, **changes):
        return {"schema": "workspace.perf.v1", "source": "runtime", "phase": "e2bAdapterCallCreate",
            "elapsedMs": 12.5, "outcome": "ok", "atUnixMs": 1700000000000,
            "jobId": "agent_run.lifecycle:run-a"} | changes

    def feed(self, collector, **changes):
        collector.feed_transport_line(json.dumps(self.event(**changes)))

    def test_all_closed_phases_preserve_safe_values_and_separate_bound_run_aggregates(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        collector.bind_run("run-b")
        for phase in self.phases:
            self.feed(collector, phase=phase)
        self.feed(collector, elapsedMs=7.5, outcome="error")
        self.feed(collector, jobId="agent_run.lifecycle:run-b", elapsedMs=30)
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["observedRecords"], 24)
        self.assertEqual(summary["coverageStatus"], "complete")
        self.assertIs(summary["durationsAdditive"], False)
        self.assertEqual(len(summary["aggregates"]), 23)
        aggregate = next(row for row in summary["aggregates"]
            if row["agentRunId"] == "run-a" and row["phase"] == "e2bAdapterCallCreate")
        self.assertEqual((aggregate["count"], aggregate["sumElapsedMs"], aggregate["minElapsedMs"], aggregate["maxElapsedMs"]),
            (2, 20.0, 7.5, 12.5))
        self.assertEqual(aggregate["outcomes"], {"ok": 1, "error": 1, "interrupted": 0})
        record = summary["records"][0]
        self.assertEqual(record, {"sequence": 1, "agentRunId": "run-a", "attribution": "boundRun",
            "phase": self.phases[0], "atUnixMs": 1700000000000, "elapsedMs": 12.5, "outcome": "ok"})
        record["phase"] = "private-secret"
        aggregate["outcomes"]["ok"] = 999
        self.assertEqual(collector.summary()["transportSpans"]["records"][0]["phase"], self.phases[0])
        self.assertNotIn("jobId", json.dumps(summary))

    def test_missing_job_is_unassigned_before_and_after_run_binding_without_guessing(self):
        collector = LifecycleCallCollector()
        event = self.event()
        del event["jobId"]
        collector.feed_transport_line(json.dumps(event))
        collector.bind_run("run-a")
        collector.feed_transport_line(json.dumps(event))
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["unassignedRecords"], 2)
        self.assertEqual(summary["observedRecords"], 2)
        self.assertEqual(summary["coverageStatus"], "incomplete")
        self.assertEqual({row["attribution"] for row in summary["records"]}, {"unassigned"})
        self.assertEqual({row["agentRunId"] for row in summary["records"]}, {None})
        self.assertEqual(summary["aggregates"][0]["count"], 2)

    def test_foreign_or_unrelated_job_never_publishes_identity_or_relabels_to_bound_run(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        for job in ("agent_run.lifecycle:private-foreign", "unrelated.private-job"):
            self.feed(collector, jobId=job)
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["foreignRecords"], 2)
        self.assertEqual(summary["observedRecords"], 0)
        self.assertEqual(summary["records"], [])
        self.assertEqual(summary["aggregates"], [])
        self.assertNotIn("private", json.dumps(collector.summary()))

    def test_known_phase_rejects_extra_missing_private_or_noncanonical_values(self):
        invalid = [self.event(private="private-secret"), self.event(source="sandbox"),
            self.event(schema="workspace.perf.v2"), self.event(elapsedMs=True), self.event(elapsedMs=-1),
            self.event(elapsedMs=float("nan")), self.event(elapsedMs=float("inf")), self.event(elapsedMs=2**64),
            self.event(elapsedMs="private-secret"), self.event(outcome="unsupported"), self.event(outcome="private-secret"),
            self.event(atUnixMs=False), self.event(atUnixMs=-1), self.event(atUnixMs=1.5), self.event(atUnixMs=2**64),
            self.event(jobId=None), self.event(jobId="x" * 181), self.event(jobId={"private": "private-secret"})]
        for key in ("schema", "source", "elapsedMs", "outcome", "atUnixMs"):
            event = self.event()
            del event[key]
            invalid.append(event)
        for index, event in enumerate(invalid):
            with self.subTest(index=index):
                collector = LifecycleCallCollector()
                collector.bind_run("run-a")
                collector.feed_transport_line(json.dumps(event))
                summary = collector.summary()["transportSpans"]
                self.assertEqual(summary["invalidRecords"], 1)
                self.assertEqual(summary["records"], [])
                self.assertNotIn("private-secret", json.dumps(collector.summary()))

    def test_duplicate_json_keys_are_invalid_even_when_values_match_or_hide_known_phase(self):
        for addition in ('"elapsedMs":12.5', '"phase":"unknownPrivate"', '"jobId":"agent_run.lifecycle:run-a"'):
            with self.subTest(addition=addition):
                collector = LifecycleCallCollector()
                collector.bind_run("run-a")
                line = json.dumps(self.event())[:-1] + "," + addition + "}"
                collector.feed_transport_line(line)
                summary = collector.summary()["transportSpans"]
                self.assertEqual(summary["invalidRecords"], 1)
                self.assertEqual(summary["records"], [])

    def test_unknown_phases_and_unrelated_lines_are_ignored_without_private_value_retention(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        self.feed(collector, phase="private-phase", private="private-secret")
        self.feed(collector, phase="workspaceProtection", metric="private-secret")
        for line in ("ready\n", "private-secret", '["private-secret"]', PrivateValue()):
            collector.feed_transport_line(line)
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["observedRecords"], 0)
        self.assertEqual(summary["invalidRecords"], 0)
        self.assertEqual(summary["records"], [])
        self.assertNotIn("private", json.dumps(collector.summary()))

    def test_oversize_and_malformed_known_json_are_explicit_without_copying_raw_text(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        collector.feed_transport_line(json.dumps(self.event(private="private-secret" * 300)))
        collector.feed_transport_line(json.dumps(self.event())[:-1])
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["oversizedLines"], 1)
        self.assertEqual(summary["invalidRecords"], 1)
        self.assertEqual(summary["records"], [])
        self.assertNotIn("private-secret", json.dumps(collector.summary()))

    def test_raw_record_limit_keeps_all_repeated_call_aggregates_and_explicit_loss(self):
        collector = LifecycleCallCollector(max_records=1)
        collector.bind_run("run-a")
        for duration in (2, 5, 8):
            self.feed(collector, elapsedMs=duration)
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["observedRecords"], 3)
        self.assertEqual(summary["droppedRecords"], 2)
        self.assertEqual(len(summary["records"]), 1)
        self.assertEqual(summary["coverageStatus"], "incomplete")
        aggregate = summary["aggregates"][0]
        self.assertEqual((aggregate["count"], aggregate["sumElapsedMs"], aggregate["minElapsedMs"], aggregate["maxElapsedMs"]), (3, 15, 2, 8))
        self.assertEqual(collector.summary()["callsStarted"], 0)

    def test_concurrent_transport_records_are_bounded_and_aggregate_counts_are_lossless(self):
        collector = LifecycleCallCollector(max_records=2)
        collector.bind_run("run-a")
        threads = [threading.Thread(target=self.feed, args=(collector,)) for _ in range(16)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(2)
            self.assertFalse(thread.is_alive())
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["aggregates"][0]["count"], 16)
        self.assertEqual(summary["observedRecords"], 16)
        self.assertEqual(summary["droppedRecords"], 14)
        self.assertEqual([row["sequence"] for row in summary["records"]], [1, 2])

    def test_runtime_feed_isolates_observer_failures_and_preserves_stream_and_real_call(self):
        import io
        from types import SimpleNamespace
        from .e2b_workspace_test_support import collect_runtime_output
        from .e2b_lifecycle_timing_test_support import feed_runtime_line
        seen, transport, errors = [], [], []
        def broken(_line):
            raise PrivateFailure()
        observer = SimpleNamespace(feed_startup_line=broken, feed_transport_line=transport.append,
            _startup_observation_failed=lambda: errors.append("startup"),
            _transport_observation_failed=lambda: errors.append("transport"))
        def on_line(line):
            feed_runtime_line(observer, line)
            seen.append(line)
        source = json.dumps(self.event()) + "\nready\n"
        collect_runtime_output(io.StringIO(source), on_line, None)
        self.assertEqual(seen, source.splitlines(keepends=True))
        self.assertEqual(transport, seen)
        self.assertEqual(errors, ["startup", "startup"])
        observer.feed_startup_line = seen.append
        observer.feed_transport_line = broken
        observer._transport_observation_failed = broken
        feed_runtime_line(observer, "ready\n")
        feed_runtime_line(None, PrivateValue())
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        with patch("app_core.e2b_lifecycle_timing_test_support._parse_transport_profile", side_effect=PrivateFailure(), create=True):
            feed_runtime_line(collector, json.dumps(self.event()))
        self.assertEqual(collector.summary()["transportSpans"]["observationErrors"], 1)
        response, calls = object(), []
        self.assertIs(collector.wrap("runtime", lambda *args: calls.append(True) or response)("/agent-runs/step"), response)
        self.assertEqual(calls, [True])

    def test_interrupted_span_is_observed_and_counted_separately_from_success_or_error(self):
        collector = LifecycleCallCollector()
        collector.bind_run("run-a")
        self.feed(collector, outcome="interrupted")
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["invalidRecords"], 0)
        self.assertEqual(summary["observedRecords"], 1)
        self.assertEqual(summary["records"][0]["outcome"], "interrupted")
        self.assertEqual(summary["aggregates"][0]["outcomes"], {"ok": 0, "error": 0, "interrupted": 1})
        self.assertEqual(summary["aggregates"][0]["count"], 1)

    def test_transport_values_use_existing_rust_duration_without_local_clock_arithmetic(self):
        def forbidden_clock():
            raise PrivateFailure()
        collector = LifecycleCallCollector(monotonic_ns=forbidden_clock, wall_time_ns=forbidden_clock)
        collector.bind_run("run-a")
        self.feed(collector)
        summary = collector.summary()["transportSpans"]
        self.assertEqual(summary["coverageStatus"], "complete")
        self.assertEqual(summary["records"][0]["elapsedMs"], 12.5)
        self.assertEqual(collector.summary()["observationErrors"], 0)
        self.assertIn("providerCreateElapsedMs", collector.summary()["unavailableMetrics"])
        self.assertIn("runtimeProcessReadyElapsedMs", collector.summary()["unavailableMetrics"])

if __name__ == "__main__":
    unittest.main()
