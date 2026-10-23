"""Bounded acceptance observations; no dispatch, guest payload or clock guessing.

Durations can nest and overlap. Aggregates expose observed work and repetition,
never an additive lifecycle duration or zero-filled evidence for missing spans.
"""
from copy import deepcopy
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sys
import threading
import time

_MAX_U64 = 2**64 - 1
_MAX_LINE_BYTES = 4096
_AUTO_DOMAIN = object()
_IDENTITY = re.compile(r"[A-Za-z0-9_.:-]{1,160}\Z", re.ASCII)
_DOMAIN = re.compile(r"[a-f0-9]{64}\Z", re.ASCII)
_BOOT_ID = re.compile(r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}\Z", re.ASCII)
_JOB_PREFIX = "agent_run.lifecycle:"
_PHASES = frozenset(("e2bHelper", "providerDispatch", "e2bSdk", "runMilestone", "executionObservationLoss"))
_HELPERS = frozenset(("prepareWorkspace", "fileSystem", "inputInventory", "materializeInput", "revokeInput", "readArtifact",
    "snapshotCollect", "snapshotCapture", "snapshotRestore", "workspaceGeneration", "workspaceGenerationRpc",
    "workspaceWatch", "agentProcessState", "quiesceAgentProcesses"))
_HELPER_METRICS = frozenset(("mkdirElapsedMs", "uploadElapsedMs", "executeElapsedMs", "waitElapsedMs", "downloadElapsedMs",
    "cleanupElapsedMs", "controlElapsedMs", "totalElapsedMs"))
_DISPATCH_METRICS = frozenset(("checkoutElapsedMs", "authorityBeforeElapsedMs", "callbackElapsedMs", "authorityAfterElapsedMs",
    "commitElapsedMs", "connectionReturnElapsedMs", "totalElapsedMs"))
_DISPATCH_KINDS = frozenset(("perRunExecution", "retainedExecution", "retainedRunCleanup"))
_SDK_STAGES = {
    "sdkImport": frozenset(("startup",)), "sdkClientSetup": frozenset(("startup",)),
    "sdkCreate": frozenset(("create",)), "sdkCreateVerifyGet": frozenset(("create",)),
    "sdkCommandStart": frozenset(("runCommand",)), "sdkCommandWait": frozenset(("waitCommand", "recoverCommand")),
    "sdkFileUpload": frozenset(("uploadFile",)), "sdkFileDownload": frozenset(("downloadFile",)),
    "sdkHandle": frozenset(("create", "runCommand", "waitCommand", "recoverCommand", "uploadFile", "downloadFile"))}
_MILESTONES = ("runStepEntered", "firstToolReceiptReady", "firstToolReceiptCommitted")
_COMMON = frozenset(("schema", "source", "phase", "atUnixMs"))
_COUNTERS = ("invalidRecords", "foreignRecords", "unassignedRecords", "oversizedLines", "droppedRecords",
    "rejectedBindings", "droppedBindings", "observationErrors", "errorRecords", "interruptedRecords",
    "uncorrelatedMilestones", "sdkDroppedRecords")


def _identity(value):
    return type(value) is str and _IDENTITY.fullmatch(value) is not None


def _uint(value):
    return type(value) is int and 0 <= value <= _MAX_U64


def _number(value):
    return type(value) in (int, float) and 0 <= value <= _MAX_U64 and math.isfinite(value)


def _clock_domain():
    try:
        if (sys.platform != "linux" or type(getattr(time, "CLOCK_MONOTONIC", None)) is not int
                or time.get_clock_info("monotonic").implementation != "clock_gettime(CLOCK_MONOTONIC)"):
            return None
        boot_id = Path("/proc/sys/kernel/random/boot_id").read_text(encoding="ascii").strip()
        inode = os.stat("/proc/self/ns/time").st_ino
        if _BOOT_ID.fullmatch(boot_id) is None or type(inode) is not int or inode <= 0:
            return None
        return hashlib.sha256(f"{boot_id}:{inode}".encode("ascii")).hexdigest()
    except BaseException:
        return None


def _decode(line):
    # Pairs retain duplicate-key evidence before a dict could erase it. A
    # duplicate phase cannot hide a known record behind an unknown final value.
    try:
        pairs = json.loads(line, object_pairs_hook=lambda entries: entries)
    except (ValueError, RecursionError):
        if any('"' + phase + '"' in line for phase in _PHASES):
            raise ValueError("invalid execution observation") from None
        return None
    if not any(key == "phase" and type(value) is str and value in _PHASES for key, value in pairs):
        return None
    keys = [key for key, _value in pairs]
    if len(keys) != len(set(keys)):
        raise ValueError("duplicate execution observation field")
    event = dict(pairs)
    phase = event.get("phase")
    if phase not in _PHASES or event.get("schema") != "workspace.perf.v1" or not _uint(event.get("atUnixMs")):
        raise ValueError("invalid execution observation envelope")
    optional = {"jobId"}
    if "jobId" in event and (type(event["jobId"]) is not str or not 1 <= len(event["jobId"]) <= 180
            or any(not (char.isascii() and (char.isalnum() or char in "_.:-")) for char in event["jobId"])):
        raise ValueError("invalid execution job identity")
    if phase == "runMilestone":
        required = _COMMON | {"milestone", "runId", "clockDomain", "atMonotonicNs"}
        milestone, domain, monotonic = event.get("milestone"), event.get("clockDomain"), event.get("atMonotonicNs")
        if type(milestone) is not str or milestone not in _MILESTONES or not _identity(event.get("runId")):
            raise ValueError("invalid run milestone")
        if milestone != "runStepEntered":
            required |= {"callId"}
            if not _identity(event.get("callId")):
                raise ValueError("invalid run milestone call")
        if not ((domain is None and monotonic is None) or
                (type(domain) is str and _DOMAIN.fullmatch(domain) is not None and _uint(monotonic))):
            raise ValueError("invalid monotonic clock pair")
    elif phase == "executionObservationLoss":
        required = _COMMON | {"metric", "value"}
        if event.get("metric") != "sdkDroppedRecords" or not _uint(event.get("value")):
            raise ValueError("invalid SDK observation loss")
    else:
        required = _COMMON | {"value", "outcome"}
        outcomes = ("ok", "error") if phase == "e2bSdk" else ("ok", "error", "interrupted")
        if type(event.get("outcome")) is not str or event["outcome"] not in outcomes or not _number(event.get("value")):
            raise ValueError("invalid execution duration")
        if phase == "e2bHelper":
            required |= {"operationId", "helperKind", "metric"}
            optional.add("dispatchId")
            if (not _identity(event.get("operationId")) or type(event.get("helperKind")) is not str or event["helperKind"] not in _HELPERS
                    or type(event.get("metric")) is not str or event["metric"] not in _HELPER_METRICS
                    or ("dispatchId" in event and not _identity(event["dispatchId"]))):
                raise ValueError("invalid helper observation")
        elif phase == "providerDispatch":
            required |= {"dispatchId", "dispatchKind", "metric"}
            if (not _identity(event.get("dispatchId")) or type(event.get("dispatchKind")) is not str
                    or event["dispatchKind"] not in _DISPATCH_KINDS or type(event.get("metric")) is not str
                    or event["metric"] not in _DISPATCH_METRICS):
                raise ValueError("invalid dispatch observation")
        else:
            required |= {"stage", "operation"}
            if (type(event.get("stage")) is not str or type(event.get("operation")) is not str
                    or event["operation"] not in _SDK_STAGES.get(event["stage"], ())):
                raise ValueError("invalid SDK observation")
    expected_source = "sdk" if phase == "e2bSdk" else "runtime"
    if event.get("source") != expected_source or set(event) - required - optional or required - event.keys():
        raise ValueError("unsupported execution observation field")
    return event


def _labels(event):
    phase = event["phase"]
    labels = {"phase": phase}
    fields = {"e2bHelper": ("helperKind", "metric"), "providerDispatch": ("dispatchKind", "metric"),
        "e2bSdk": ("stage", "operation"), "runMilestone": ("milestone",), "executionObservationLoss": ("metric",)}[phase]
    return labels | {key: event[key] for key in fields}


class ExecutionTimingCollector:
    """One fixture owns bounded records, bounded Run bindings and closed buckets.

    Explicit clock_domain=None disables local domain detection. Only matching
    non-null domains permit a caller to correlate monotonic timestamps; this
    collector never subtracts clocks or turns a missing metric into zero.
    """
    def __init__(self, *, max_records=2048, max_runs=32, clock_domain=_AUTO_DOMAIN):
        if type(max_records) is not int or not 1 <= max_records <= 2048:
            raise ValueError("bounded positive execution record limit required")
        if type(max_runs) is not int or not 1 <= max_runs <= 32:
            raise ValueError("bounded positive execution Run limit required")
        if clock_domain is _AUTO_DOMAIN:
            clock_domain = _clock_domain()
        elif clock_domain is not None and (type(clock_domain) is not str or _DOMAIN.fullmatch(clock_domain) is None):
            raise ValueError("explicit clock domain must be a lowercase SHA-256 identity or null")
        self._clock_domain = clock_domain
        self._max_records, self._max_runs = max_records, max_runs
        self._lock = threading.Lock()
        self._runs, self._records, self._aggregates = {}, [], {}
        self._counters = dict.fromkeys(_COUNTERS, 0)
        self._sequence = 0

    def _observation_failed(self):
        try:
            with self._lock:
                self._counters["observationErrors"] += 1
        except BaseException:
            pass

    def bind_run(self, run_id):
        try:
            with self._lock:
                if not _identity(run_id):
                    self._counters["rejectedBindings"] += 1
                elif run_id in self._runs:
                    return
                elif len(self._runs) >= self._max_runs:
                    self._counters["droppedBindings"] += 1
                else:
                    self._runs[run_id] = {"observedRecords": 0, "milestones": set()}
        except BaseException:
            self._observation_failed()

    def feed_line(self, line):
        try:
            if type(line) is not str:
                return
            if len(line) > _MAX_LINE_BYTES or len(line.encode("utf-8")) > _MAX_LINE_BYTES:
                with self._lock:
                    self._counters["oversizedLines"] += 1
                return
            if not line.lstrip().startswith("{"):
                return
            try:
                event = _decode(line)
            except (ValueError, TypeError, OverflowError, RecursionError):
                with self._lock:
                    self._counters["invalidRecords"] += 1
                return
            if event is None:
                return
            with self._lock:
                if event["phase"] == "runMilestone":
                    run = event["runId"]
                    if run not in self._runs or ("jobId" in event and event["jobId"] != _JOB_PREFIX + run):
                        self._counters["foreignRecords"] += 1
                        return
                elif "jobId" in event:
                    job = event["jobId"]
                    run = job[len(_JOB_PREFIX):] if job.startswith(_JOB_PREFIX) else None
                    if run not in self._runs:
                        self._counters["foreignRecords"] += 1
                        return
                else:
                    run = None
                    self._counters["unassignedRecords"] += 1
                self._record(event, run)
        except BaseException:
            self._observation_failed()

    def _record(self, event, run):
        # Caller holds the lock. Dynamic operation/dispatch/call IDs are raw
        # bounded evidence, never aggregate keys or growing identity registries.
        labels = _labels(event)
        key = (run, *labels.values())
        attribution = "unassigned" if run is None else "boundRun"
        aggregate = self._aggregates.setdefault(key, {"agentRunId": run, "attribution": attribution, **labels, "count": 0})
        aggregate["count"] += 1
        phase = event["phase"]
        if phase == "runMilestone":
            self._runs[run]["milestones"].add(event["milestone"])
            if event["clockDomain"] is None or event["clockDomain"] != self._clock_domain:
                self._counters["uncorrelatedMilestones"] += 1
        elif phase == "executionObservationLoss":
            self._counters["sdkDroppedRecords"] += event["value"]
            aggregate["droppedRecords"] = aggregate.get("droppedRecords", 0) + event["value"]
        else:
            duration, outcome = event["value"], event["outcome"]
            aggregate["sumElapsedMs"] = aggregate.get("sumElapsedMs", 0.0) + duration
            aggregate["minElapsedMs"] = min(aggregate.get("minElapsedMs", duration), duration)
            aggregate["maxElapsedMs"] = max(aggregate.get("maxElapsedMs", duration), duration)
            aggregate.setdefault("outcomes", {"ok": 0, "error": 0, "interrupted": 0})[outcome] += 1
            if outcome != "ok":
                self._counters["errorRecords" if outcome == "error" else "interruptedRecords"] += 1
        if run is not None:
            self._runs[run]["observedRecords"] += 1
        self._sequence += 1
        if len(self._records) >= self._max_records:
            self._counters["droppedRecords"] += 1
        else:
            self._records.append({key: value for key, value in event.items() if key != "jobId"}
                | {"sequence": self._sequence, "agentRunId": run, "attribution": attribution})

    def summary(self):
        try:
            with self._lock:
                coverage = [{"runId": run, "observedRecords": value["observedRecords"],
                    "missingMilestones": [name for name in _MILESTONES if name not in value["milestones"]]}
                    for run, value in sorted(self._runs.items())]
                incomplete = (not self._sequence or not coverage or any(self._counters.values())
                    or any(row["missingMilestones"] for row in coverage))
                return {"schema": "e2b.executionTimings.v1", "clockDomain": self._clock_domain,
                    "coverageStatus": "incomplete" if incomplete else "complete",
                    "observationStatus": "observed" if self._sequence else "notObserved",
                    "durationsAdditive": False, "observedRecords": self._sequence,
                    "records": deepcopy(self._records), "aggregates": deepcopy(list(self._aggregates.values())),
                    "counters": self._counters.copy(), "runCoverage": coverage}
        except BaseException:
            return {"schema": "e2b.executionTimings.v1", "clockDomain": self._clock_domain,
                "coverageStatus": "incomplete", "observationStatus": "unavailable", "durationsAdditive": False,
                "observedRecords": None, "records": [], "aggregates": [], "counters": None, "runCoverage": []}
