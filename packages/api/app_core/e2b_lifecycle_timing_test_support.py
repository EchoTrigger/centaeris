"""Bounded acceptance-side call observations; never dispatches or interprets results.

Only exact, closed path labels are retained. Durations use a process monotonic
clock and may overlap: endpoint sums show call work, not an additive lifecycle
latency. A step return after resume bounds a completed step, not its first tool.
"""
from copy import deepcopy
import threading
import time


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
_UNAVAILABLE = ("runtimeStartupElapsedMs", "providerCreateElapsedMs", "firstToolAfterResumeElapsedMs")
_MAX_U64 = 2**64 - 1


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
    def __init__(self, *, max_records=2048, monotonic_ns=None, wall_time_ns=None):
        if type(max_records) is not int or not 1 <= max_records <= 2048:
            raise ValueError("bounded positive lifecycle measurement limit required")
        self._monotonic_ns = time.monotonic_ns if monotonic_ns is None else monotonic_ns
        self._wall_time_ns = time.time_ns if wall_time_ns is None else wall_time_ns
        if not callable(self._monotonic_ns) or not callable(self._wall_time_ns):
            raise ValueError("lifecycle measurement clocks must be callable")
        self._max_records = max_records
        self._lock = threading.Lock()
        self._records, self._endpoints = [], {}
        self._sequence = self._in_flight = 0
        self._dropped_records = self._observation_errors = self._rejected_markers = 0

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
                in_flight = self._in_flight
                dropped, errors, rejected = self._dropped_records, self._observation_errors, self._rejected_markers
        except BaseException:
            # Reporting is optional too. Preserve only an explicit unavailable
            # observation when a snapshot cannot be read, never a guessed zero.
            return {"schema": "e2b.lifecycleCallTimings.v1", "coverageStatus": "incomplete",
                "incomplete": True, "observationStatus": "unavailable", "observationErrors": None,
                "callsStarted": None, "callsCompleted": None, "inFlightCalls": None,
                "droppedRecords": None, "rejectedMarkers": None, "durationsAdditive": False,
                "unavailableMetrics": list(_UNAVAILABLE), "endpoints": [], "records": []}
        started = sum(row["count"] for row in endpoints)
        completed = sum(sum(row["outcomes"].values()) for row in endpoints)
        incomplete = not records or bool(in_flight or dropped or errors or rejected)
        return {"schema": "e2b.lifecycleCallTimings.v1", "coverageStatus": "incomplete" if incomplete else "complete",
            "incomplete": incomplete, "observationStatus": "observed" if records else "notObserved",
            "callsStarted": started, "callsCompleted": completed, "inFlightCalls": in_flight,
            "droppedRecords": dropped, "observationErrors": errors, "rejectedMarkers": rejected,
            "durationsAdditive": False, "unavailableMetrics": list(_UNAVAILABLE),
            "endpoints": endpoints, "records": records}
