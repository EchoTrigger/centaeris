"""Bounded provider-power observations for the opt-in product fixture.

Invocations are declared by the fixture, never guessed from asynchronous log
arrival. Durations contain nested spans and are deliberately never added.
"""
from collections import Counter
from copy import deepcopy
import json
import math
import re
import threading

from .e2b_workspace_test_support import MAX_LINE_CHARS

_FIELDS = frozenset(("schema", "source", "phase", "operationId", "action", "metric", "value", "outcome", "atUnixMs"))
_RUNTIME = frozenset(("runtimePowerElapsedMs", "contextDropElapsedMs", "adapterSpawnElapsedMs",
    "adapterHandshakeElapsedMs", "adapterRoundTripElapsedMs", "adapterCloseElapsedMs",
    "storePrepareElapsedMs", "storeDispatchCasElapsedMs", "storeSideEffectPinsElapsedMs",
    "storeCallbackElapsedMs", "storeAckCommitElapsedMs", "storeAdvanceElapsedMs"))
_SDK = frozenset(("sdkImportElapsedMs", "sdkClientSetupElapsedMs", "sdkIdentityGetElapsedMs",
    "sdkPauseElapsedMs", "sdkConnectElapsedMs", "sdkResumeVerifyGetElapsedMs", "sdkHandleElapsedMs"))
_REPLAY = frozenset(("runtimePowerElapsedMs", "storePrepareElapsedMs", "storeAdvanceElapsedMs"))
_METRICS = _RUNTIME | _SDK
_IDENTITY = re.compile(r"[A-Za-z0-9_.:-]{1,256}\Z", re.ASCII)
_MAX_U64 = 2**64 - 1
_COUNTERS = ("rejectedRecords", "foreignRecords", "malformedRecords", "oversizedLines",
             "droppedRecords", "droppedBindings", "droppedInvocations")


def _identity(value):
    return isinstance(value, str) and _IDENTITY.fullmatch(value) is not None


def _unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate measurement field")
        result[key] = value
    return result


def _required(action, kind):
    if kind == "replay":
        return Counter({metric: 1 for metric in _REPLAY})
    metrics = _RUNTIME | (_SDK - ({"sdkConnectElapsedMs", "sdkResumeVerifyGetElapsedMs"}
        if action == "pause" else {"sdkPauseElapsedMs"}))
    required = Counter({metric: 1 for metric in metrics})
    required["sdkHandleElapsedMs"] = 2  # capability handshake and actual power call
    return required


class PowerTimingCollector:
    def __init__(self, *, max_records=256, max_operations=8, max_invocations=4):
        if any(type(value) is not int or not 1 <= value <= 1024
               for value in (max_records, max_operations, max_invocations)):
            raise ValueError("bounded positive power measurement limits required")
        self._max_records, self._max_operations, self._max_invocations = max_records, max_operations, max_invocations
        self._lock = threading.Lock()
        self._operations = {}
        self._record_count = 0
        self._counts = dict.fromkeys(_COUNTERS, 0)

    def bind(self, operation_id, action):
        if not _identity(operation_id) or action not in ("pause", "resume"):
            raise ValueError("bounded fixture power identity required")
        with self._lock:
            existing = self._operations.get(operation_id)
            if existing is not None:
                if existing["action"] != action:
                    raise ValueError("fixture power identity cannot be rebound")
                return
            if len(self._operations) >= self._max_operations:
                self._counts["droppedBindings"] += 1
                return
            self._operations[operation_id] = {"action": action, "invocations": [{"kind": "fresh", "records": []}]}

    def bind_replay(self, operation_id):
        with self._lock:
            operation = self._operations.get(operation_id) if _identity(operation_id) else None
            if operation is None:
                raise ValueError("replay requires an owned fixture power identity")
            if len(operation["invocations"]) >= self._max_invocations:
                self._counts["droppedInvocations"] += 1
                return
            operation["invocations"].append({"kind": "replay", "records": []})

    def oversized_line(self):
        with self._lock:
            self._counts["oversizedLines"] += 1

    def feed(self, line):
        if len(line) > MAX_LINE_CHARS or len(line.encode("utf-8")) > MAX_LINE_CHARS:
            self.oversized_line()
            return
        if not line.lstrip().startswith("{"):
            return
        try:
            event = json.loads(line, object_pairs_hook=_unique_object)
        except (ValueError, TypeError, RecursionError):
            with self._lock:
                self._counts["malformedRecords"] += 1
            return
        if not isinstance(event, dict) or event.get("phase") != "providerPower":
            return
        with self._lock:
            self._accept(event)

    def _accept(self, event):
        valid = (event.keys() == _FIELDS and event.get("schema") == "workspace.perf.v1"
            and isinstance(event.get("action"), str) and event["action"] in ("pause", "resume")
            and isinstance(event.get("metric"), str) and event["metric"] in _METRICS
            and isinstance(event.get("source"), str) and event["source"] in ("runtime", "sdk")
            and isinstance(event.get("outcome"), str) and event["outcome"] in ("ok", "error", "pending", "replayed")
            and _identity(event.get("operationId")) and type(event.get("atUnixMs")) is int
            and 0 <= event["atUnixMs"] <= _MAX_U64
            and type(event.get("value")) in (int, float) and 0 <= event["value"] <= _MAX_U64
            and math.isfinite(event["value"]))
        if valid:
            valid = (event["source"] == ("runtime" if event["metric"] in _RUNTIME else "sdk")
                and (event["source"] != "sdk" or event["outcome"] in ("ok", "error"))
                and not (event["action"] == "pause" and event["metric"] in ("sdkConnectElapsedMs", "sdkResumeVerifyGetElapsedMs"))
                and not (event["action"] == "resume" and event["metric"] == "sdkPauseElapsedMs"))
        if not valid:
            self._counts["rejectedRecords"] += 1
            return
        operation = self._operations.get(event["operationId"])
        if operation is None or operation["action"] != event["action"]:
            self._counts["foreignRecords"] += 1
            return
        if self._record_count >= self._max_records:
            self._counts["droppedRecords"] += 1
            return
        operation["invocations"][-1]["records"].append({"sequence": self._record_count, **event})
        self._record_count += 1

    def summary(self, *, lifecycle_passed, stream_drained):
        with self._lock:
            operations, counts = deepcopy(self._operations), dict(self._counts)
        reported = []
        all_complete = bool(operations)
        for operation_id, operation in operations.items():
            invocations = []
            for ordinal, invocation in enumerate(operation["invocations"], 1):
                rows, kind = invocation["records"], invocation["kind"]
                required = _required(operation["action"], kind)
                observed = Counter(row["metric"] for row in rows)
                missing = sorted(metric for metric, count in required.items() if observed[metric] < count)
                excess = sorted(metric for metric, count in observed.items() if count > required[metric])
                acceptable = ("ok", "replayed") if kind == "replay" else ("ok",)
                complete = not missing and not excess and all(row["outcome"] in acceptable for row in rows)
                all_complete = all_complete and complete
                invocations.append({"ordinal": ordinal, "kind": kind,
                    "coverageStatus": "complete" if complete else "incomplete",
                    "metricCoverage": {metric: "observed" if observed[metric] else "notObserved" for metric in sorted(_METRICS)},
                    "missingMetrics": missing, "excessMetrics": excess, "records": rows})
            reported.append({"operationId": operation_id, "action": operation["action"], "invocations": invocations})
        loss = not stream_drained or any(counts.values())
        return {"schema": "e2b.powerTimings.v1", "lifecycleStatus": "passed" if lifecycle_passed else "failed",
            "streamComplete": bool(stream_drained), "lossDetected": loss,
            "aggregation": "recordsOnly", "durationsAdditive": False, "spanSemantics": "nestedAndOverlapping",
            "observationStatus": "observed" if any(row["records"] for operation in reported for row in operation["invocations"]) else "notObserved",
            "coverageStatus": "complete" if all_complete and lifecycle_passed and not loss else "incomplete",
            **counts, "operations": reported}


class PowerTimingMultiplexer:
    def __init__(self, workspace, power):
        self._workspace, self._power = workspace, power

    def bind(self, run_id, job_id):
        self._workspace.bind(run_id, job_id)

    def feed(self, line):
        self._workspace.feed(line)
        self._power.feed(line)

    def oversized_line(self):
        self._workspace.oversized_line()
        self._power.oversized_line()

    def summary(self, *, lifecycle_passed, stream_drained):
        return self._workspace.summary(lifecycle_passed=lifecycle_passed, stream_drained=stream_drained)


def require_power_timing_coverage(report):
    if (not isinstance(report, dict) or report.get("schema") != "e2b.powerTimings.v1"
            or report.get("lifecycleStatus") != "passed" or report.get("streamComplete") is not True
            or report.get("lossDetected") is not False or report.get("coverageStatus") != "complete"
            or report.get("aggregation") != "recordsOnly" or report.get("durationsAdditive") is not False
            or report.get("spanSemantics") != "nestedAndOverlapping"
            or report.get("observationStatus") != "observed"
            or any(type(report.get(key)) is not int or report[key] != 0 for key in _COUNTERS)):
        raise AssertionError("e2b_power_timing_coverage_incomplete")
