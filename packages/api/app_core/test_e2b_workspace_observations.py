"""No-network tests for bounded, fixture-owned workspace measurements."""
import io
import json
import math
import unittest

from .e2b_workspace_test_support import (WorkspaceObservationCollector, collect_runtime_output,
    iter_runtime_output, workspace_observation_collector)


class E2BWorkspaceObservationTests(unittest.TestCase):
    def setUp(self):
        self.collector = WorkspaceObservationCollector()
        self.collector.bind('owned-run', 'agent_run.lifecycle:owned-run')

    def event(self, **changes):
        return {'schema': 'workspace.perf.v1', 'source': 'runtime', 'phase': 'workspaceProtection',
                'operationId': 'protection-owned', 'runId': 'owned-run',
                'jobId': 'agent_run.lifecycle:owned-run', 'trigger': 'terminalCommit',
                'metric': 'uploadedBytes', 'value': 0, 'outcome': 'ok', 'atUnixMs': 100} | changes

    def feed(self, **changes):
        self.collector.feed(json.dumps(self.event(**changes)))

    def report(self, **changes):
        return self.collector.summary(lifecycle_passed=True, stream_drained=True, **changes)

    def test_measured_zero_missing_and_unavailable_are_distinct(self):
        self.feed(value=0)
        self.feed(metric='sourceReadBytes', value=None, outcome='unavailable', source='sandbox')
        operation = self.report()['operations'][0]
        self.assertEqual(operation['records'][0]['value'], 0)
        self.assertEqual(operation['records'][1]['value'], None)
        self.assertEqual(operation['metricCoverage']['uploadedBytes'], 'observed')
        self.assertEqual(operation['metricCoverage']['sourceReadBytes'], 'unavailable')
        self.assertEqual(operation['metricCoverage']['snapshotLogicalBytes'], 'notObserved')
        self.assertEqual(self.report()['physicalIoCoverage'], 'notMeasured')

    def test_private_fields_unknown_metrics_and_values_are_not_forwarded(self):
        private = 'private-fixture-credential'
        for changes in ({'authorization': private}, {'metric': private}, {'value': private},
                        {'outcome': private}, {'source': private}, {'trigger': private},
                        {'atUnixMs': private}, {'value': True}, {'value': -1}, {'value': math.nan},
                        {'metric': []}, {'trigger': {}}, {'source': []}, {'outcome': {}},
                        {'value': 10**300}):
            self.feed(**changes)
        self.assertEqual(self.report()['operations'], [])
        self.assertNotIn(private, json.dumps(self.report()))
        self.assertGreater(self.report()['rejectedRecords'], 0)

    def test_measured_guest_counters_keep_sandbox_source_but_missing_can_be_runtime(self):
        self.feed(metric='sourceReadBytes', value=10, source='runtime')
        self.feed(metric='sourceReadBytes', value=10, source='sandbox')
        self.feed(metric='scannedEntries', value=None, outcome='unavailable', source='runtime')
        records = self.report()['operations'][0]['records']
        self.assertEqual([record['source'] for record in records], ['sandbox', 'runtime'])
        self.assertEqual(self.report()['rejectedRecords'], 1)

    def test_foreign_run_and_job_cannot_bind_an_operation(self):
        self.feed(runId='foreign-run', jobId='agent_run.lifecycle:foreign-run')
        self.feed(jobId='agent_run.lifecycle:foreign-run')
        self.assertEqual(self.report()['operations'], [])
        self.feed()
        self.assertEqual(len(self.report()['operations']), 1)
        self.assertEqual(self.report()['foreignRecords'], 2)

    def test_operation_cannot_move_to_another_owned_run(self):
        self.collector.bind('other-owned-run', 'agent_run.lifecycle:other-owned-run')
        self.feed()
        self.feed(runId='other-owned-run', jobId='agent_run.lifecycle:other-owned-run')
        self.feed()
        report = self.report()
        self.assertEqual(report['operations'], [])
        self.assertEqual(report['conflictingOperations'], 1)
        self.assertTrue(report['lossDetected'])

    def test_repeated_upload_records_are_preserved_without_summing(self):
        self.feed(value=42)
        self.feed(value=42)
        self.feed(value=42, outcome='interrupted')
        records = self.report()['operations'][0]['records']
        self.assertEqual([record['value'] for record in records], [42, 42, 42])
        self.assertEqual(self.report()['aggregation'], 'recordsOnly')

    def test_dispositions_and_numeric_new_metrics_are_explicit(self):
        for metric in ('rawCaptureSpoolWrittenBytes', 'sdkDownloadFrameBytes', 'workspaceCommitElapsedMs'):
            self.feed(metric=metric, value=0)
        for disposition in ('full', 'unchanged', 'alreadyAdvanced', 'pending', 'rejected'):
            self.feed(metric='workspaceCommitDisposition', value=disposition)
        self.assertEqual(len(self.report()['operations'][0]['records']), 8)
        self.feed(metric='workspaceCommitDisposition', value='private-value')
        self.feed(metric='workspaceCommitElapsedMs', value=None, outcome='ok')
        self.assertEqual(len(self.report()['operations'][0]['records']), 8)

    def test_line_reader_discards_oversized_lines_without_losing_next_record(self):
        line = json.dumps(self.event())
        stream = io.StringIO('x' * 10000 + '\n' + line + '\n' + '{broken\n')
        pieces = list(iter_runtime_output(stream))
        self.assertEqual(pieces[0], None)
        for piece in pieces:
            if piece is None:
                self.collector.oversized_line()
            else:
                self.collector.feed(piece)
        report = self.report()
        self.assertEqual(report['oversizedLines'], 1)
        self.assertEqual(report['malformedRecords'], 1)
        self.assertEqual(len(report['operations']), 1)
        self.assertTrue(report['lossDetected'])

    def test_non_measurement_logs_are_ignored_and_duplicate_keys_are_rejected(self):
        self.collector.feed('a private log that is not reported')
        self.collector.feed(json.dumps(self.event(phase='leaseHeartbeat')))
        self.collector.feed(json.dumps(self.event()).replace('"value": 0', '"value": 0, "value": 999'))
        self.assertEqual(self.report()['operations'], [])
        self.assertEqual(self.report()['malformedRecords'], 1)
        self.assertNotIn('private log', json.dumps(self.report()))

    def test_record_operation_and_binding_limits_are_bounded(self):
        collector = WorkspaceObservationCollector(max_records=2, max_operations=1, max_bindings=1)
        collector.bind('owned-run', 'agent_run.lifecycle:owned-run')
        collector.bind('another-run', 'agent_run.lifecycle:another-run')
        for event in (self.event(value=1), self.event(value=2), self.event(value=3),
                      self.event(operationId='another-operation')):
            collector.feed(json.dumps(event))
        report = collector.summary(lifecycle_passed=True, stream_drained=True)
        self.assertEqual(len(report['operations']), 1)
        self.assertEqual(len(report['operations'][0]['records']), 2)
        self.assertEqual(report['droppedRecords'], 2)
        self.assertEqual(report['droppedBindings'], 1)
        self.assertTrue(report['lossDetected'])

    def test_incomplete_drain_and_failed_lifecycle_are_not_successful_coverage(self):
        report = self.collector.summary(lifecycle_passed=False, stream_drained=False)
        self.assertEqual(report['lifecycleStatus'], 'failed')
        self.assertFalse(report['streamComplete'])
        self.assertTrue(report['lossDetected'])
        self.assertEqual(report['operations'], [])
        self.assertEqual(report['observationStatus'], 'notObserved')
        self.assertEqual(report['runCoverage'], [{'runId': 'owned-run',
            'jobId': 'agent_run.lifecycle:owned-run', 'operationCount': 0}])

    def test_null_and_observed_repeats_have_partial_metric_coverage(self):
        self.feed(value=None, outcome='unavailable')
        self.feed(value=0)
        report = self.report()
        self.assertEqual(report['operations'][0]['metricCoverage']['uploadedBytes'], 'partial')
        self.assertEqual(report['observationStatus'], 'observed')

    def test_collection_is_explicitly_opt_in_and_normal_output_is_unchanged(self):
        for environment in ({}, {'WORKSPACE_PERF_OBSERVATIONS': '0'}, {'WORKSPACE_PERF_OBSERVATIONS': 'true'}):
            self.assertIsNone(workspace_observation_collector(environment))
        self.assertIsInstance(workspace_observation_collector({'WORKSPACE_PERF_OBSERVATIONS': '1'}),
                              WorkspaceObservationCollector)
        line = 'x' * 10000 + '\n'
        seen = []
        collect_runtime_output(io.StringIO(line), seen.append, None)
        self.assertEqual(seen, [line])

    def test_reader_feeds_only_bounded_complete_lines_to_existing_diagnostics(self):
        line = json.dumps(self.event()) + '\n'
        seen = []
        collect_runtime_output(io.StringIO('x' * 10000 + '\n' + line), seen.append, self.collector)
        self.assertEqual(seen, [line])
        self.assertEqual(self.report()['oversizedLines'], 1)
        self.assertEqual(len(self.report()['operations']), 1)

    def test_unterminated_line_is_still_a_bounded_record(self):
        self.assertEqual(list(iter_runtime_output(io.StringIO('ready'))), ['ready'])
