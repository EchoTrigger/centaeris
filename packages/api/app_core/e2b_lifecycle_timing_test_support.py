"""Bounded acceptance-side call observations; never dispatches or interprets results.

Only exact, closed path labels are retained. Durations use a process monotonic
clock and may overlap: endpoint sums show call work, not an additive lifecycle
latency. A step return after resume bounds a completed step, not its first tool.
"""
from copy import deepcopy
import threading
import time
import re
import json
import math


_PATHS = {
    "runtime": {
        "/internal/jobs/claim": "claim",
        "/internal/jobs/control/claim": "claim",
        "/internal/jobs/start": "start",
        "/internal/jobs/control/start": "start",
        "/agent-runs/step": "step",
        "/agent-runs/teardown": "teardown",
        "/internal/jobs/yield": "yield",
        "/internal/jobs/heartbeat": "heartbeat",
        "/internal/jobs/wait": "wait",
    },
    "api": {
        "/internal/agent-run-lifecycle/resolve": "resolve",
        "/internal/agent-run-lifecycle/release": "release",
        "/internal/agent-runs/transition": "project",
    },
}
_MARKERS = frozenset(("sampleStart", "resumeRequested", "resumeReturned", "sampleEnd"))
_UNAVAILABLE = ("runtimeProcessReadyElapsedMs", "providerCreateElapsedMs", "firstToolAfterResumeElapsedMs")
_MAX_U64 = 2**64 - 1

_STARTUP_PREFIX = "agent_run_startup_profile:"
_STARTUP_MAX_LINE_CHARS = 2048
_STARTUP_FIELDS = ("serverCount", "credentialCount", "mcpConnectionCount", "runtimePreludeMs",
    "sandboxEnsureMs", "mcpCredentialResolveMs", "mcpDiscoveryMs", "workspaceRestoreMs",
    "sessionStartCommitMs", "startupTotalMs")
_STARTUP_IDENTITY = re.compile(r"[A-Za-z0-9_.:-]{1,160}\Z", re.ASCII)
_STARTUP_PATTERN = re.compile(r"agent_run_startup_profile: agentRunId=(?P<agentRunId>[A-Za-z0-9_.:-]{1,160}); "
    + "; ".join(name + r"=(?P<" + name + r">0|[1-9][0-9]{0,19})" for name in _STARTUP_FIELDS)
    + r"\Z", re.ASCII)
_STARTUP_COUNTERS = ("invalidRecords", "foreignRecords", "duplicateRecords", "conflictingRecords",
    "oversizedLines", "rejectedBindings", "droppedBindings", "observationErrors")


_TRANSPORT_PHASES = frozenset("e2bAdapter" + suffix for suffix in (
    "SpawnOrdinary", "SpawnControl", "SpawnCleanup", "HandshakeOrdinary", "HandshakeControl", "HandshakeCleanup",
    "CallCapabilities", "CallCreate", "CallAttach", "CallInspect", "CallKill", "CallPause", "CallConnect",
    "CallReadFile", "CallWriteFile", "CallUploadFile", "CallDownloadFile", "CallRunCommand", "CallWaitCommand",
    "CallRecoverCommand", "CallCancelCommand", "CallInvokeControlHelper"))
_TRANSPORT_REQUIRED = frozenset(("schema", "source", "phase", "elapsedMs", "outcome", "atUnixMs"))
_TRANSPORT_COUNTERS = ("invalidRecords", "foreignRecords", "unassignedRecords", "oversizedLines",
    "droppedRecords", "observationErrors")
_TRANSPORT_MAX_LINE_CHARS = 2048
_RUN_JOB_PREFIX = "agent_run.lifecycle:"


def _parse_transport_profile(line):
    # Decode bounded JSON into pairs so duplicate keys cannot silently replace a
    # phase, duration, or attribution. Unknown phases have no observer contract.
    try:
        pairs = json.loads(line, object_pairs_hook=lambda entries: entries)
    except (ValueError, RecursionError):
        known = any('"' + phase + '"' in line for phase in _TRANSPORT_PHASES)
        return ("invalid" if known else "ignored"), None
    if not isinstance(pairs, list) or not any(
            key == "phase" and type(value) is str and value in _TRANSPORT_PHASES for key, value in pairs):
        return "ignored", None
    keys = [key for key, _ in pairs]
    if len(keys) != len(set(keys)):
        return "invalid", None
    fields = set(keys)
    if fields not in (_TRANSPORT_REQUIRED, _TRANSPORT_REQUIRED | {"jobId"}):
        return "invalid", None
    event = dict(pairs)
    elapsed, timestamp = event["elapsedMs"], event["atUnixMs"]
    if (event["schema"] != "workspace.perf.v1" or event["source"] != "runtime"
            or type(event["outcome"]) is not str or event["outcome"] not in ("ok", "error", "interrupted")
            or type(elapsed) not in (int, float) or not math.isfinite(elapsed) or not 0 <= elapsed <= _MAX_U64
            or type(timestamp) is not int or not 0 <= timestamp <= _MAX_U64
            or ("jobId" in event and (type(event["jobId"]) is not str or not 1 <= len(event["jobId"]) <= 180))):
        return "invalid", None
    return "accepted", event


def _parse_startup_profile(line):
    # Only the one Rust-owned fixed format is accepted, with one optional line
    # ending. Never retain a raw line, free-form error, or an unbound identifier.
    if line.endswith("\r\n"):
        line = line[:-2]
    elif line.endswith("\n"):
        line = line[:-1]
    matched = _STARTUP_PATTERN.fullmatch(line)
    if matched is None:
        return None
    record = {"agentRunId": matched["agentRunId"]}
    for field in _STARTUP_FIELDS:
        value = int(matched[field])
        if value > _MAX_U64:
            return None
        record[field] = value
    return record


def feed_lifecycle_startup_line(collector, line):
    """The fixture's optional observer cannot interrupt READY or stream drain."""
    if collector is None:
        return
    try:
        collector.feed_startup_line(line)
    except BaseException:
        try:
            collector._startup_observation_failed()
        except BaseException:
            pass


def feed_runtime_line(collector, line):
    """Keep each optional observer independent of the other and of stream IO."""
    feed_lifecycle_startup_line(collector, line)
    if collector is None:
        return
    try:
        collector.feed_transport_line(line)
    except BaseException:
        try:
            collector._transport_observation_failed()
        except BaseException:
            pass


def _endpoint(lane, path):
    # No URL parsing, string coercion, prefix matching, or query inspection.
    return _PATHS[lane].get(path, "other") if type(path) is str else "other"


def _statistics(lane, endpoint):
    return {"lane": lane, "endpoint": endpoint, "count": 0, "timedCount": 0,
        "sumElapsedMs": 0.0, "minElapsedMs": None, "maxElapsedMs": None,
        "outcomes": {"ok": 0, "error": 0}}


class LifecycleCallCollector:
    """One fixture owns this bounded observer; no process-global state or IO.

    Clock injection is only for deterministic offline tests. Call wrappers return
    the original object or re-raise the original exception after exactly one
    dispatch, including when observation itself raises a BaseException.
    """
    def __init__(self, *, max_records=2048, max_startup_runs=16, monotonic_ns=None, wall_time_ns=None):
        if type(max_records) is not int or not 1 <= max_records <= 2048:
            raise ValueError("bounded positive lifecycle measurement limit required")
        if type(max_startup_runs) is not int or not 1 <= max_startup_runs <= 32:
            raise ValueError("bounded positive startup binding limit required")
        self._max_startup_runs = max_startup_runs
        self._monotonic_ns = time.monotonic_ns if monotonic_ns is None else monotonic_ns
        self._wall_time_ns = time.time_ns if wall_time_ns is None else wall_time_ns
        if not callable(self._monotonic_ns) or not callable(self._wall_time_ns):
            raise ValueError("lifecycle measurement clocks must be callable")
        self._max_records = max_records
        self._lock = threading.Lock()
        self._records, self._endpoints = [], {}
        self._startup_bindings, self._startup_profiles = set(), {}
        self._startup_counts = dict.fromkeys(_STARTUP_COUNTERS, 0)
        self._transport_records, self._transport_aggregates = [], {}
        self._transport_counts = dict.fromkeys(_TRANSPORT_COUNTERS, 0)
        self._transport_sequence = 0
        self._sequence = self._in_flight = 0
        self._dropped_records = self._observation_errors = self._rejected_markers = 0

    def _startup_observation_failed(self):
        try:
            with self._lock:
                self._startup_counts["observationErrors"] += 1
                self._observation_errors += 1
        except BaseException:
            pass

    def bind_run(self, run_id):
        """Bind only the fixture's admitted A/B identities, before its Run step."""
        try:
            with self._lock:
                if type(run_id) is not str or _STARTUP_IDENTITY.fullmatch(run_id) is None:
                    self._startup_counts["rejectedBindings"] += 1
                elif run_id in self._startup_bindings:
                    return
                elif len(self._startup_bindings) >= self._max_startup_runs:
                    self._startup_counts["droppedBindings"] += 1
                else:
                    self._startup_bindings.add(run_id)
        except BaseException:
            self._startup_observation_failed()

    def feed_startup_line(self, line):
        try:
            if type(line) is not str or not line.startswith(_STARTUP_PREFIX):
                return
            if len(line) > _STARTUP_MAX_LINE_CHARS:
                with self._lock:
                    self._startup_counts["oversizedLines"] += 1
                return
            record = _parse_startup_profile(line)
            with self._lock:
                if record is None:
                    self._startup_counts["invalidRecords"] += 1
                    return
                run = record["agentRunId"]
                if run not in self._startup_bindings:
                    self._startup_counts["foreignRecords"] += 1
                    return
                previous = self._startup_profiles.get(run)
                if previous is not None:
                    self._startup_counts["duplicateRecords"] += 1
                    if previous != record:
                        self._startup_counts["conflictingRecords"] += 1
                    return
                self._startup_profiles[run] = record
        except BaseException:
            self._startup_observation_failed()

    def _startup_snapshot(self):
        # The caller holds the collector lock. One profile per bounded binding
        # is retained; duplicate/conflicting records never replace the first.
        missing = sorted(self._startup_bindings - self._startup_profiles.keys())
        complete = bool(self._startup_bindings) and not missing and not any(self._startup_counts.values())
        return {"schema": "e2b.runStartupProfiles.v1", "scope": "agentRunStartup",
            "coverageStatus": "complete" if complete else "incomplete",
            "observationStatus": "observed" if self._startup_profiles else "notObserved",
            "boundRunCount": len(self._startup_bindings), "observedRunCount": len(self._startup_profiles),
            "missingRunIds": missing, "durationsAdditive": False,
            "clockBoundary": "Existing Rust Instant elapsed durations for each Run startup; not process readiness or cross-clock differences",
            **self._startup_counts,
            "records": deepcopy([self._startup_profiles[run] for run in sorted(self._startup_profiles)])}


    def _transport_observation_failed(self):
        try:
            with self._lock:
                self._transport_counts["observationErrors"] += 1
                self._observation_errors += 1
        except BaseException:
            pass

    def feed_transport_line(self, line):
        try:
            if type(line) is not str or not line.lstrip().startswith("{"):
                return
            if len(line) > _TRANSPORT_MAX_LINE_CHARS:
                with self._lock:
                    self._transport_counts["oversizedLines"] += 1
                return
            status, event = _parse_transport_profile(line)
            if status == "ignored":
                return
            with self._lock:
                if status == "invalid":
                    self._transport_counts["invalidRecords"] += 1
                    return
                run = None
                if "jobId" in event:
                    job = event["jobId"]
                    if job.startswith(_RUN_JOB_PREFIX):
                        run = job[len(_RUN_JOB_PREFIX):]
                    if run not in self._startup_bindings:
                        self._transport_counts["foreignRecords"] += 1
                        return
                else:
                    self._transport_counts["unassignedRecords"] += 1
                self._transport_sequence += 1
                phase, duration, outcome = event["phase"], event["elapsedMs"], event["outcome"]
                attribution = "unassigned" if run is None else "boundRun"
                aggregate = self._transport_aggregates.setdefault((run, phase), {
                    "agentRunId": run, "attribution": attribution, "phase": phase, "count": 0,
                    "sumElapsedMs": 0.0, "minElapsedMs": duration, "maxElapsedMs": duration,
                    "outcomes": {"ok": 0, "error": 0, "interrupted": 0}})
                aggregate["count"] += 1
                aggregate["sumElapsedMs"] += duration
                aggregate["minElapsedMs"] = min(aggregate["minElapsedMs"], duration)
                aggregate["maxElapsedMs"] = max(aggregate["maxElapsedMs"], duration)
                aggregate["outcomes"][outcome] += 1
                if len(self._transport_records) >= self._max_records:
                    self._transport_counts["droppedRecords"] += 1
                else:
                    self._transport_records.append({"sequence": self._transport_sequence,
                        "agentRunId": run, "attribution": attribution, "phase": phase,
                        "atUnixMs": event["atUnixMs"], "elapsedMs": duration, "outcome": outcome})
        except BaseException:
            self._transport_observation_failed()

    def _transport_snapshot(self):
        # The caller holds the lock. The Run binding limit and closed phases
        # bound aggregate cardinality; raw rows have an independent record cap.
        observed = self._transport_sequence
        return {"schema": "e2b.adapterSpans.v1", "scope": "adapterSpans",
            "coverageStatus": "complete" if observed and not any(self._transport_counts.values()) else "incomplete",
            "observationStatus": "observed" if observed else "notObserved", "observedRecords": observed,
            "durationsAdditive": False,
            "timingBoundary": "Existing Rust monotonic adapter spawn, handshake and method round-trip durations; nested spans overlap and are not pure provider-internal create time",
            **self._transport_counts, "records": deepcopy(self._transport_records),
            "aggregates": deepcopy([self._transport_aggregates[key]
                for key in sorted(self._transport_aggregates, key=lambda key: (key[0] or "", key[1]))])}

    def wrap(self, lane, callback):
        if type(lane) is not str or lane not in _PATHS or not callable(callback):
            raise ValueError("closed lifecycle lane and callable required")
        def measured(*args, **kwargs):
            token = None
            try:
                token = self._begin(lane, args[0] if args else kwargs.get("path"))
            except BaseException:
                self._note_error()
            try:
                result = callback(*args, **kwargs)
            except BaseException:
                self._safe_finish(token, "error")
                raise
            self._safe_finish(token, "ok")
            return result
        return measured

    def _read_clocks(self):
        monotonic, wall = self._monotonic_ns(), self._wall_time_ns()
        if any(type(value) is not int or not 0 <= value <= _MAX_U64 for value in (monotonic, wall)):
            raise ValueError("lifecycle measurement clock invalid")
        return monotonic, wall // 1_000_000

    def _note_error(self):
        try:
            with self._lock:
                self._observation_errors += 1
        except BaseException:
            # Observation failure must never replace a business result, even
            # when the observation lock itself is unavailable.
            pass

    def _begin(self, lane, path):
        endpoint = _endpoint(lane, path)
        with self._lock:
            self._sequence += 1
            token = {"kind": "call", "sequence": self._sequence,
                "lane": lane, "endpoint": endpoint}
            stats = self._endpoints.setdefault((lane, endpoint), _statistics(lane, endpoint))
            stats["count"] += 1
            self._in_flight += 1
        try:
            token["startedMonotonicNs"], token["startedAtUnixMs"] = self._read_clocks()
        except BaseException:
            self._note_error()
        return token

    def _safe_finish(self, token, outcome):
        if token is None:
            return
        try:
            self._finish(token, outcome)
        except BaseException:
            self._note_error()

    def _finish(self, token, outcome):
        timing = None
        try:
            monotonic, wall = self._read_clocks()
            start = token.get("startedMonotonicNs")
            if start is not None:
                if monotonic < start:
                    raise ValueError("lifecycle monotonic clock moved backwards")
                timing = {"finishedMonotonicNs": monotonic, "finishedAtUnixMs": wall,
                    "elapsedMs": (monotonic - start) / 1_000_000}
        except BaseException:
            self._note_error()
        with self._lock:
            stats = self._endpoints[(token["lane"], token["endpoint"])]
            stats["outcomes"][outcome] += 1
            self._in_flight -= 1
            if timing is None:
                return
            duration = timing["elapsedMs"]
            stats["timedCount"] += 1
            stats["sumElapsedMs"] += duration
            stats["minElapsedMs"] = duration if stats["minElapsedMs"] is None else min(stats["minElapsedMs"], duration)
            stats["maxElapsedMs"] = duration if stats["maxElapsedMs"] is None else max(stats["maxElapsedMs"], duration)
            self._append_record({**token, **timing, "outcome": outcome})

    def _append_record(self, record):
        # Caller holds the collector lock. Aggregate statistics are constant-size
        # and continue counting repeated real calls after this record budget fills.
        if len(self._records) >= self._max_records:
            self._dropped_records += 1
        else:
            self._records.append(record)

    def marker(self, name):
        """A closed, local clock marker; never a network or provider operation."""
        try:
            if type(name) is not str or name not in _MARKERS:
                with self._lock:
                    self._rejected_markers += 1
                return
            monotonic, wall = self._read_clocks()
            with self._lock:
                self._sequence += 1
                self._append_record({"kind": "marker", "sequence": self._sequence,
                    "name": name, "atMonotonicNs": monotonic, "atUnixMs": wall})
        except BaseException:
            self._note_error()

    def summary(self):
        try:
            with self._lock:
                records = deepcopy(sorted(self._records, key=lambda row: row["sequence"]))
                endpoints = deepcopy([self._endpoints[key] for key in sorted(self._endpoints)])
                startup = self._startup_snapshot()
                transport = self._transport_snapshot()
                in_flight = self._in_flight
                dropped, errors, rejected = self._dropped_records, self._observation_errors, self._rejected_markers
        except BaseException:
            # Reporting is optional too. Preserve only an explicit unavailable
            # observation when a snapshot cannot be read, never a guessed zero.
            return {"schema": "e2b.lifecycleCallTimings.v1", "coverageStatus": "incomplete",
                "incomplete": True, "observationStatus": "unavailable", "observationErrors": None,
                "callsStarted": None, "callsCompleted": None, "inFlightCalls": None,
                "droppedRecords": None, "rejectedMarkers": None, "durationsAdditive": False,
                "unavailableMetrics": list(_UNAVAILABLE), "endpoints": [], "records": [],
                "startupProfiles": {"schema": "e2b.runStartupProfiles.v1", "scope": "agentRunStartup",
                    "coverageStatus": "incomplete", "observationStatus": "unavailable",
                    "missingRunIds": None, "records": []},
                "transportSpans": {"schema": "e2b.adapterSpans.v1", "scope": "adapterSpans",
                    "coverageStatus": "incomplete", "observationStatus": "unavailable",
                    "observedRecords": None, "records": [], "aggregates": []}}
        started = sum(row["count"] for row in endpoints)
        completed = sum(sum(row["outcomes"].values()) for row in endpoints)
        incomplete = not records or bool(in_flight or dropped or errors or rejected)
        return {"schema": "e2b.lifecycleCallTimings.v1", "coverageStatus": "incomplete" if incomplete else "complete",
            "incomplete": incomplete, "observationStatus": "observed" if records else "notObserved",
            "callsStarted": started, "callsCompleted": completed, "inFlightCalls": in_flight,
            "droppedRecords": dropped, "observationErrors": errors, "rejectedMarkers": rejected,
            "durationsAdditive": False, "unavailableMetrics": list(_UNAVAILABLE),
            "endpoints": endpoints, "records": records, "startupProfiles": startup, "transportSpans": transport}
