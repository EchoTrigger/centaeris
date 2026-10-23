"""Observed execution boundaries remain independent of business transport."""
import json
import unittest
from unittest.mock import patch
from .e2b_lifecycle_timing_test_support import LifecycleCallCollector,feed_runtime_line

class ExecutionObservationBindingTests(unittest.TestCase):
    def event(self):
        return {"schema":"workspace.perf.v1","source":"sdk","phase":"e2bSdk","stage":"sdkCreate","operation":"create","value":12.5,"outcome":"ok","atUnixMs":10,"jobId":"agent_run.lifecycle:run-owned"}
    def test_runtime_stream_exports_execution_timings_under_the_admitted_run(self):
        collector=LifecycleCallCollector();collector.bind_run("run-owned")
        feed_runtime_line(collector,json.dumps(self.event()))
        result=collector.summary()["executionTimings"]
        self.assertEqual(result["observedRecords"],1)
        self.assertEqual(result["records"][0]["agentRunId"],"run-owned")
        self.assertEqual(result["records"][0]["stage"],"sdkCreate")
    def test_broken_other_observers_do_not_hide_execution_timings_or_inspect_exceptions(self):
        class PrivateFailure(BaseException):
            def __str__(self):raise AssertionError("private failure inspected")
        collector=LifecycleCallCollector();collector.bind_run("run-owned")
        with patch.object(collector,"feed_startup_line",side_effect=PrivateFailure()),patch.object(collector,"feed_transport_line",side_effect=PrivateFailure()):
            feed_runtime_line(collector,json.dumps(self.event()))
        self.assertEqual(collector.summary()["executionTimings"]["observedRecords"],1)
        self.assertEqual(collector.summary()["observationErrors"],2)
    def test_absent_or_foreign_run_does_not_get_reassigned_to_an_existing_run(self):
        collector=LifecycleCallCollector();collector.bind_run("run-other")
        feed_runtime_line(collector,json.dumps(self.event()))
        self.assertEqual(collector.summary()["executionTimings"]["counters"]["foreignRecords"],1)
        self.assertEqual(collector.summary()["executionTimings"]["records"],[])
    def test_custom_fixture_clock_cannot_claim_a_kernel_clock_domain(self):
        collector=LifecycleCallCollector(monotonic_ns=lambda:42)
        self.assertIsNone(collector.summary()["executionTimings"]["clockDomain"])
