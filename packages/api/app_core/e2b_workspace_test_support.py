"""Bounded, sanitized observations for the opt-in real E2B test fixture.

Records retain producer order, including repeated attempts. They deliberately do
not sum bytes or infer durable storage from a consumed HTTP request body.
"""
from copy import deepcopy
import json
import math
import re
import threading


MAX_LINE_CHARS = 4096
METRICS = frozenset({
    'scannedEntries', 'sourceReadBytes', 'runtimeSpoolWrittenBytes', 'snapshotLogicalBytes',
    'uploadAttempts', 'uploadedBytes', 'captureElapsedMs', 'uploadElapsedMs',
    'restoreDownloadElapsedMs', 'restoreApplyElapsedMs', 'rawCaptureSpoolWrittenBytes',
    'sdkDownloadFrameBytes', 'workspaceCommitElapsedMs', 'workspaceCommitDisposition',
})
ELAPSED_METRICS = frozenset({
    'captureElapsedMs', 'uploadElapsedMs', 'restoreDownloadElapsedMs',
    'restoreApplyElapsedMs', 'workspaceCommitElapsedMs',
})
DISPOSITIONS = frozenset({'full', 'unchanged', 'alreadyAdvanced', 'pending', 'rejected'})
TRIGGERS = frozenset({'terminalCommit', 'recoveryCheckpoint', 'sessionRestore', 'recoveryRestore'})
FIELDS = frozenset({'schema', 'source', 'phase', 'operationId', 'metric', 'value', 'outcome',
                    'atUnixMs', 'trigger', 'runId', 'jobId'})
IDENTITY = re.compile(r'[A-Za-z0-9_.:-]{1,160}\Z', re.ASCII)
MAX_U64 = 2**64 - 1


def iter_runtime_output(stream):
    """Yield bounded text lines; None denotes an entirely discarded long line."""
    while True:
        chunk = stream.readline(MAX_LINE_CHARS + 1)
        if not chunk:
            return
        if len(chunk) > MAX_LINE_CHARS:
            while chunk and not chunk.endswith('\n'):
                chunk = stream.readline(MAX_LINE_CHARS + 1)
            yield None
        else:
            yield chunk


def _unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError('duplicate field')
        value[key] = item
    return value


def _identity(value):
    return isinstance(value, str) and IDENTITY.fullmatch(value) is not None


def _integer(value):
    return type(value) is int and 0 <= value <= MAX_U64


class WorkspaceObservationCollector:
    def __init__(self, *, max_records=256, max_operations=16, max_bindings=8):
        if any(type(value) is not int or not 1 <= value <= 1024
               for value in (max_records, max_operations, max_bindings)):
            raise ValueError('bounded positive measurement limits required')
        self._max_records = max_records
        self._max_operations = max_operations
        self._max_bindings = max_bindings
        self._lock = threading.Lock()
        self._bindings = {}
        self._operations = {}
        self._record_count = 0
        self._counts = {key: 0 for key in ('rejectedRecords', 'foreignRecords', 'malformedRecords',
            'oversizedLines', 'droppedRecords', 'droppedBindings', 'conflictingOperations')}

    def bind(self, run_id, job_id):
        if not _identity(run_id) or not _identity(job_id):
            raise ValueError('bounded fixture identities required')
        with self._lock:
            if run_id in self._bindings:
                if self._bindings[run_id] != job_id:
                    raise ValueError('fixture identity cannot be rebound')
                return
            if len(self._bindings) == self._max_bindings:
                self._counts['droppedBindings'] += 1
                return
            self._bindings[run_id] = job_id

    def oversized_line(self):
        with self._lock:
            self._counts['oversizedLines'] += 1

    def feed(self, line):
        if len(line) > MAX_LINE_CHARS or len(line.encode('utf-8')) > MAX_LINE_CHARS:
            self.oversized_line()
            return
        if not line.lstrip().startswith('{'):
            return
        try:
            event = json.loads(line, object_pairs_hook=_unique_object)
        except (ValueError, TypeError, RecursionError):
            with self._lock:
                self._counts['malformedRecords'] += 1
            return
        if (not isinstance(event, dict) or event.get('schema') != 'workspace.perf.v1'
                or event.get('phase') != 'workspaceProtection'):
            return
        with self._lock:
            self._accept(event)

    def _accept(self, event):
        valid = (event.keys() == FIELDS
            and event['source'] in ('runtime', 'sandbox')
            and isinstance(event['trigger'], str) and event['trigger'] in TRIGGERS
            and isinstance(event['metric'], str) and event['metric'] in METRICS
            and event['outcome'] in ('ok', 'error', 'interrupted', 'unavailable')
            and all(_identity(event[key]) for key in ('operationId', 'runId', 'jobId'))
            and _integer(event['atUnixMs']))
        if valid:
            metric, value, outcome = event['metric'], event['value'], event['outcome']
            if outcome == 'unavailable':
                valid = value is None
            elif metric == 'workspaceCommitDisposition':
                valid = isinstance(value, str) and value in DISPOSITIONS
            elif metric in ELAPSED_METRICS:
                valid = (_integer(value) or (type(value) is float and math.isfinite(value)
                         and 0 <= value <= MAX_U64))
            else:
                valid = _integer(value)
            if metric in ('sourceReadBytes', 'scannedEntries') and value is not None:
                valid = valid and event['source'] == 'sandbox'
        if not valid:
            self._counts['rejectedRecords'] += 1
            return
        if self._bindings.get(event['runId']) != event['jobId']:
            self._counts['foreignRecords'] += 1
            return
        identity = (event['runId'], event['jobId'], event['trigger'])
        operation_id = event['operationId']
        operation = self._operations.get(operation_id)
        if operation is not None and operation['identity'] != identity:
            if not operation['conflicted']:
                operation['records'].clear()
                operation['conflicted'] = True
                self._counts['conflictingOperations'] += 1
            return
        if operation is not None and operation['conflicted']:
            return
        if (self._record_count == self._max_records
                or (operation is None and len(self._operations) == self._max_operations)):
            self._counts['droppedRecords'] += 1
            return
        if operation is None:
            operation = {'identity': identity, 'conflicted': False, 'records': []}
            self._operations[operation_id] = operation
        operation['records'].append({key: event[key] for key in
            ('source', 'metric', 'value', 'outcome', 'atUnixMs')})
        self._record_count += 1

    def summary(self, *, lifecycle_passed, stream_drained):
        with self._lock:
            operations = []
            for operation_id, operation in self._operations.items():
                if operation['conflicted']:
                    continue
                run_id, job_id, trigger = operation['identity']
                records = deepcopy(operation['records'])
                coverage = {}
                for metric in sorted(METRICS):
                    matching = [record for record in records if record['metric'] == metric]
                    coverage[metric] = ('notObserved' if not matching else
                        'unavailable' if all(record['value'] is None for record in matching) else
                        'partial' if any(record['value'] is None for record in matching) else 'observed')
                operations.append({'operationId': operation_id, 'runId': run_id, 'jobId': job_id,
                    'trigger': trigger, 'records': records, 'metricCoverage': coverage})
            counts = dict(self._counts)
            run_coverage = [{'runId': run_id, 'jobId': job_id,
                'operationCount': sum(operation['runId'] == run_id for operation in operations)}
                for run_id, job_id in self._bindings.items()]
        return {'schema': 'e2b.workspaceMeasurements.v1',
            'lifecycleStatus': 'passed' if lifecycle_passed else 'failed',
            'streamComplete': bool(stream_drained), 'lossDetected': not stream_drained or any(counts.values()),
            'aggregation': 'recordsOnly', 'physicalIoCoverage': 'notMeasured',
            'observationStatus': 'observed' if operations else 'notObserved', 'runCoverage': run_coverage,
            'uploadedBytesBoundary': 'httpRequestBodyConsumed',
            'providerDurableBytes': 'notMeasured', 'sdkPartialStagingBytes': 'notMeasured',
            **counts, 'operations': operations}


def workspace_observation_collector(environment):
    return WorkspaceObservationCollector() if environment.get('WORKSPACE_PERF_OBSERVATIONS') == '1' else None


def collect_runtime_output(stream, on_line, collector):
    lines = iter_runtime_output(stream) if collector is not None else stream
    for line in lines:
        if line is None:
            collector.oversized_line()
            continue
        if collector is not None:
            collector.feed(line)
        on_line(line)
