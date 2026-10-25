"""Local read-only projection of an explicit Workspace benchmark job event stream.

This service does not start jobs, query Workspace databases, or read trial files.
Run with --job-dir PATH; keep the listener on loopback and use an SSH tunnel.
"""
from __future__ import annotations

import argparse
from collections import deque
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import re
import stat
import threading
import time


JOB_SCHEMA = "centaeris.workspace_benchmark.job.v1"
EVENT_SCHEMA = "centaeris.workspace_benchmark.event.v1"
READ_BUDGET = 256 * 1024
LINE_LIMIT = 64 * 1024
MANIFEST_LIMIT = 256 * 1024
TRIAL_LIMIT = 1000
STALE_SECONDS = 30
ASSETS = Path(__file__).with_suffix("")
STATUSES = {"queued", "running", "completed", "failed", "cancelled", "unknown"}
PHASES = {"queued", "preparing", "admitting", "agent", "verifier", "artifacts", "cleanup", "finished"}
TRIAL_FIELDS = {"taskId", "attempt", "status", "phase", "slot", "sessionId", "agentRunId",
    "environmentId", "instanceId", "containerId", "agentStatus", "verifierStatus", "reward",
    "resourceHolding", "cleanupStatus", "error", "metrics"}
TEXT_FIELDS = {"sessionId", "agentRunId", "environmentId", "instanceId", "containerId",
    "agentStatus", "verifierStatus", "cleanupStatus", "error"}


class InvalidProjection(ValueError):
    """A safe diagnostic code, never raw input or a local path."""


def redact_output(value: str) -> str:
    value = re.sub(r"(?i)\bBearer\s+[^\s,;]+", "Bearer [redacted]", value)
    value = re.sub(r'''(?i)(\b(?:[\w-]*(?:key|token|secret|password)|authorization)["']?\s*[:=]\s*)(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;]+)''',
        r"\1[redacted]", value)
    value = re.sub(r"\b(?:sk-|ghp_|github_pat_)[a-zA-Z0-9_-]+", "[redacted]", value)
    value = re.sub(r"https?://[^\s<>]+", lambda match: match[0].split("?")[0].split("#")[0]
        if "@" not in match[0] else "[redacted URL]", value)
    return value


def redact(value: str) -> str:
    return redact_output(value)[:2048]


def fields(value, allowed, required=()):
    if not isinstance(value, dict) or set(value) - set(allowed) or set(required) - set(value):
        raise InvalidProjection("unsupportedFields")


def integer(value, minimum, maximum):
    if type(value) is not int or not minimum <= value <= maximum:
        raise InvalidProjection("invalidInteger")
    return value


def text(value, maximum=256):
    if not isinstance(value, str) or not value or len(value) > maximum:
        raise InvalidProjection("invalidText")
    return redact(value)


def member(value, options):
    return isinstance(value, str) and value in options


def observation_time(value, clock):
    try:
        timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if timestamp.utcoffset() != timezone.utc.utcoffset(None):
            raise ValueError()
        observed = timestamp.timestamp()
        if observed > clock() + 60:
            raise ValueError()
        return observed
    except (TypeError, ValueError, AttributeError) as error:
        raise InvalidProjection("invalidObservationTime") from error


def trial_metrics(value, clock):
    fields(value, {"observedAt", "usage", "memory"}, {"observedAt", "usage", "memory"})
    observation_time(value["observedAt"], clock)
    usage, memory = value["usage"], value["memory"]
    counts = {"requests", "inputTokens", "outputTokens", "promptCacheHitTokens", "promptCacheMissTokens"}
    usage_fields = counts | {"source", "scope", "promptCacheHitRate", "complete"}
    memory_fields = {"runtimeCurrentBytes", "runtimePeakBytes", "runtimeAnonBytes", "taskCurrentBytes",
        "taskPeakBytes", "taskLimitBytes", "runtimeOomKills", "taskOomKills"}
    fields(usage, usage_fields, usage_fields)
    fields(memory, memory_fields, memory_fields)
    if usage["source"] != "committedProviderUsage" or usage["scope"] != "mainAgent" or type(usage["complete"]) is not bool:
        raise InvalidProjection("invalidUsageSource")
    for values, keys in ((usage, counts), (memory, memory_fields)):
        for key in keys:
            if values[key] is not None:
                integer(values[key], 0, 2**53 - 1)
    hit, miss, rate = usage["promptCacheHitTokens"], usage["promptCacheMissTokens"], usage["promptCacheHitRate"]
    available = usage["complete"] and all(usage[key] is not None for key in counts) and hit + miss > 0
    if rate is not None and (not available or type(rate) not in (int, float)
            or not math.isfinite(rate) or not math.isclose(rate, hit / (hit + miss), rel_tol=1e-12, abs_tol=1e-12)):
        raise InvalidProjection("invalidCacheHitRate")
    # Only the latest fixed-size observation is retained, never a sample series.
    return {"observedAt": value["observedAt"], "usage": dict(usage), "memory": dict(memory)}


def decode(wire):
    def unique_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise InvalidProjection("duplicateField")
            result[key] = value
        return result
    try:
        return json.loads(wire, object_pairs_hook=unique_pairs,
            parse_constant=lambda _: (_ for _ in ()).throw(InvalidProjection("nonFiniteNumber")))
    except (ValueError, UnicodeError, RecursionError) as error:
        raise InvalidProjection("malformedJson") from error


class JobProjection:
    def __init__(self, job_dir: Path, clock=time.time):
        self.root = Path(job_dir).resolve()
        self.clock = clock
        self.lock = threading.Lock()
        self.resets = 0
        self.manifest_wire = None
        self.job = None
        self._reset()

    def _reset(self):
        self.offset = 0
        self.identity = None
        self.file_stamp = None
        self.event_anchor = b""
        self.sequence = 0
        self.last_observed = None
        self.first_observed = None
        self.last_heartbeat = None
        self.trials = {}
        self.logs = deque(maxlen=100)
        self.job_status = "unknown"
        self.error = None

    def _open(self, name):
        path = self.root / name
        if path.is_symlink() or path.resolve().parent != self.root:
            raise InvalidProjection("unsafeFile")
        # Reject devices and directories before opening; never follow a symlink.
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode):
            raise InvalidProjection("unsafeFile")
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
        handle = os.fdopen(descriptor, "rb")
        after = os.fstat(handle.fileno())
        if path.is_symlink() or (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
            handle.close()
            raise InvalidProjection("fileChangedDuringOpen")
        return handle, after

    def _manifest(self):
        with self._open("manifest.json")[0] as handle:
            wire = handle.read(MANIFEST_LIMIT + 1)
        if len(wire) > MANIFEST_LIMIT:
            raise InvalidProjection("manifestTooLarge")
        if wire == self.manifest_wire:
            return
        value = decode(wire)
        required = {"schema", "jobId", "title", "taskCount", "trialCount", "attemptsPerTask", "maxConcurrent", "tasks"}
        fields(value, required | {"model", "resourcePolicy", "baseSha", "candidateSha"}, required)
        if value["schema"] != JOB_SCHEMA:
            raise InvalidProjection("unsupportedJobSchema")
        task_count = integer(value["taskCount"], 1, TRIAL_LIMIT)
        attempts = integer(value["attemptsPerTask"], 1, 100)
        if integer(value["trialCount"], 1, TRIAL_LIMIT) != task_count * attempts:
            raise InvalidProjection("trialCountMismatch")
        integer(value["maxConcurrent"], 1, 2)
        if not isinstance(value["tasks"], list) or len(value["tasks"]) != task_count:
            raise InvalidProjection("taskCountMismatch")
        task_ids = set()
        for task in value["tasks"]:
            fields(task, {"taskId", "memoryMiB"}, {"taskId", "memoryMiB"})
            text(task["taskId"])
            integer(task["memoryMiB"], 1, 1048576)
            if task["taskId"] in task_ids:
                raise InvalidProjection("duplicateTask")
            task_ids.add(task["taskId"])
        for key in ("jobId", "title", "baseSha", "candidateSha"):
            if key in value:
                value[key] = text(value[key])
        if "model" in value:
            fields(value["model"], {"provider", "modelName", "thinkingMode", "contextTokens", "maxOutputTokens"})
            for key, item in value["model"].items():
                if key.endswith("Tokens"):
                    integer(item, 1, 10000000)
                else:
                    value["model"][key] = text(item)
        if "resourcePolicy" in value:
            fields(value["resourcePolicy"], {"exclusiveMemoryMiB"}, {"exclusiveMemoryMiB"})
            if value["resourcePolicy"]["exclusiveMemoryMiB"] != 8192:
                raise InvalidProjection("unsupportedResourcePolicy")
        if self.manifest_wire is not None:
            self.resets += 1
            self._reset()
        self.manifest_wire, self.job = wire, value

    def _event(self, value):
        fields(value, {"schema", "sequence", "observedAt", "kind", "trialId", "payload"},
            {"schema", "sequence", "observedAt", "kind", "payload"})
        if value["schema"] != EVENT_SCHEMA:
            raise InvalidProjection("unsupportedEventSchema")
        if integer(value["sequence"], 1, 2**63 - 1) != self.sequence + 1:
            raise InvalidProjection("eventSequenceGap")
        observed = observation_time(value["observedAt"], self.clock)
        kind, payload = value["kind"], value["payload"]
        if kind == "job":
            fields(payload, {"status"}, {"status"})
            if not member(payload["status"], STATUSES):
                raise InvalidProjection("invalidStatus")
            self.job_status = payload["status"]
        elif kind == "heartbeat":
            fields(payload, {})
            if "trialId" in value:
                raise InvalidProjection("invalidHeartbeat")
            self.last_heartbeat = observed
        elif kind == "log":
            fields(payload, {"message", "level"}, {"message", "level"})
            if not member(payload["level"], {"info", "warning", "error"}):
                raise InvalidProjection("invalidLogLevel")
            message = text(payload["message"], 4096)
            self.logs.append({"sequence": value["sequence"], "observedAt": value["observedAt"],
                "trialId": text(value["trialId"]) if "trialId" in value else None,
                "level": payload["level"], "message": message})
        elif kind == "trial":
            trial_id = text(value.get("trialId"))
            fields(payload, TRIAL_FIELDS, {"taskId", "attempt", "status", "phase"})
            task = next((item for item in self.job["tasks"] if item["taskId"] == payload["taskId"]), None)
            if task is None:
                raise InvalidProjection("unknownTask")
            integer(payload["attempt"], 1, self.job["attemptsPerTask"])
            if not member(payload["status"], STATUSES) or not member(payload["phase"], PHASES):
                raise InvalidProjection("invalidTrialState")
            clean = dict(payload)
            if "metrics" in clean:
                clean["metrics"] = trial_metrics(clean["metrics"], self.clock)
            for key in TEXT_FIELDS & clean.keys():
                if clean[key] is not None:
                    clean[key] = text(clean[key], 4096 if key == "error" else 256)
            if clean.get("slot") is not None:
                integer(clean["slot"], 1, self.job["maxConcurrent"])
            if clean.get("resourceHolding") is not None:
                integer(clean["resourceHolding"], 0, 1)
            if clean.get("reward") is not None and (type(clean["reward"]) not in (int, float) or not math.isfinite(clean["reward"])):
                raise InvalidProjection("invalidReward")
            old = self.trials.get(trial_id)
            if old and (old["taskId"], old["attempt"]) != (clean["taskId"], clean["attempt"]):
                raise InvalidProjection("trialIdentityConflict")
            if old is None and len(self.trials) >= self.job["trialCount"]:
                raise InvalidProjection("trialCountExceeded")
            for existing_id, existing in self.trials.items():
                if existing_id != trial_id and (existing["taskId"], existing["attempt"]) == (clean["taskId"], clean["attempt"]):
                    raise InvalidProjection("duplicateTrialAttempt")
            self.trials[trial_id] = {**(old or {}), **clean, "trialId": trial_id,
                "memoryMiB": task["memoryMiB"], "observedAt": value["observedAt"]}
            row = self.trials[trial_id]
            if row.get("startedAt") is None and row["status"] == "running":
                row["startedAt"] = value["observedAt"]
            if row.get("finishedAt") is None and row["status"] in {"completed", "failed", "cancelled"}:
                row["finishedAt"] = value["observedAt"]
        else:
            raise InvalidProjection("unsupportedEventKind")
        self.sequence = value["sequence"]
        if self.first_observed is None:
            self.first_observed = observed
        if kind != "heartbeat":
            self.last_observed = observed

    def snapshot(self):
        with self.lock:
            return self._snapshot()

    def _outputs(self):
        if not self.job:
            return [], None, None
        try:
            with self._open("output.json")[0] as handle:
                wire = handle.read(1024 * 1024 + 1)
            if len(wire) > 1024 * 1024:
                raise InvalidProjection("outputTooLarge")
            value = decode(wire)
            keys = {"schema", "jobId", "observedAt", "error", "trials"}
            fields(value, keys, keys)
            if value["schema"] != "centaeris.workspaceBenchmarkOutput.v1" or value["jobId"] != self.job["jobId"]:
                raise InvalidProjection("outputJobMismatch")
            observed = observation_time(value["observedAt"], self.clock)
            if value["error"] not in (None, "unavailable") or not isinstance(value["trials"], list) or len(value["trials"]) > 2:
                raise InvalidProjection("invalidOutput")
            results, seen = [], set()
            for item in value["trials"]:
                required = {"trialId", "sessionId", "agentRunId", "sourceSequence", "entries", "truncated", "error"}
                fields(item, required, required)
                row = self.trials.get(text(item["trialId"]))
                if not row or any(item[k] != row.get(k) for k in ("sessionId", "agentRunId")) or item["trialId"] in seen:
                    raise InvalidProjection("outputRunMismatch")
                seen.add(item["trialId"])
                integer(item["sourceSequence"], 0, 2**53 - 1)
                if (type(item["truncated"]) is not bool or item["error"] not in (None, "outputUnavailable")
                        or not isinstance(item["entries"], list) or len(item["entries"]) > 40):
                    raise InvalidProjection("invalidOutput")
                entries, entry_ids, size = [], set(), 0
                for entry in item["entries"]:
                    fields(entry, {"id", "kind", "text"}, {"id", "kind", "text"})
                    key, kind = text(entry["id"]), text(entry["kind"], 300)
                    if key in entry_ids or not isinstance(entry["text"], str):
                        raise InvalidProjection("invalidOutputEntry")
                    entry_ids.add(key)
                    count = len(entry["text"].encode("utf-8"))
                    size += count
                    if count > 8192 or size > 65536:
                        raise InvalidProjection("outputTooLarge")
                    entries.append({"id": key, "kind": kind, "text": redact_output(entry["text"])})
                results.append({**item, "entries": entries})
            age = max(0, self.clock() - observed)
            return results, value["error"] or ("outputStale" if age > STALE_SECONDS else None), round(age, 1)
        except FileNotFoundError:
            return [], None, None
        except (InvalidProjection, OSError, UnicodeError) as error:
            return [], str(error) if isinstance(error, InvalidProjection) else "outputReadError", None

    def _snapshot(self):
        read_bytes, catching_up = 0, False
        # A transient filesystem error is retried from the last validated cursor.
        # Invalid event data stays blocked until the stream is replaced/rebuilt.
        if self.error == "fileReadError":
            self.error = None
        try:
            self._manifest()
            try:
                handle, info = self._open("events.jsonl")
            except FileNotFoundError:
                handle = None
            if handle:
                with handle:
                    identity = (info.st_dev, info.st_ino)
                    changed = self.identity not in (None, identity) or info.st_size < self.offset
                    stamp = (info.st_size, info.st_mtime_ns)
                    if self.file_stamp and stamp[0] == self.file_stamp[0] and stamp[1] != self.file_stamp[1]:
                        changed = True
                    if not changed and self.event_anchor:
                        handle.seek(self.offset - len(self.event_anchor))
                        changed = handle.read(len(self.event_anchor)) != self.event_anchor
                    if changed:
                        self.resets += 1
                        self._reset()
                    self.identity = identity
                    self.file_stamp = stamp
                    if self.error is None:
                        handle.seek(self.offset)
                        chunk = handle.read(READ_BUDGET)
                        read_bytes = len(chunk)
                        end = chunk.rfind(b"\n") + 1
                        if not end and len(chunk) > LINE_LIMIT:
                            raise InvalidProjection("eventTooLarge")
                        for wire in chunk[:end].splitlines(keepends=True):
                            line = wire.rstrip(b"\r\n")
                            if len(line) > LINE_LIMIT:
                                raise InvalidProjection("eventTooLarge")
                            self._event(decode(line))
                            self.offset += len(wire)
                            # Retain only the last validated event from this bounded
                            # read. A short suffix misses identity
                            # changes when same-size rewrites share timestamps.
                            self.event_anchor = wire
                        catching_up = self.offset < info.st_size and end > 0
        except FileNotFoundError:
            # A missing manifest is an explicit empty state, never a fabricated run.
            self._reset()
            self.manifest_wire = None
            self.job = None
        except (InvalidProjection, OSError) as error:
            self.error = str(error) if isinstance(error, InvalidProjection) else "fileReadError"
        age = max(0, self.clock() - self.last_observed) if self.last_observed is not None else None
        health_observed = self.last_heartbeat if self.last_heartbeat is not None else self.first_observed
        heartbeat_age = max(0, self.clock() - health_observed) if health_observed is not None else None
        trials = list(self.trials.values()) if self.job else []
        terminal = {"completed", "failed", "cancelled"}
        complete = (self.job is not None and self.job_status in terminal and not catching_up
            and len(trials) == self.job["trialCount"] and all(
                trial["status"] in terminal and trial.get("resourceHolding") == 0
                and trial.get("cleanupStatus") == "absent" for trial in trials))
        status = ("invalid" if self.error else "noLiveRun" if self.job is None else
            "complete" if complete else "stale" if heartbeat_age is not None and heartbeat_age > STALE_SECONDS else
            "live" if self.sequence else "waiting")
        slots = []
        for slot in (1, 2):
            held = [trial for trial in trials if trial.get("slot") == slot and trial.get("resourceHolding") == 1]
            slots.append({"slot": slot, "trialIds": [trial["trialId"] for trial in held]})
        exclusive = next((trial["trialId"] for trial in trials if trial.get("resourceHolding") == 1 and trial["memoryMiB"] >= 8192), None)
        outputs, output_error, output_age = self._outputs() if self.error is None else ([], "lifecycleUnavailable", None)
        if complete and output_error == "outputStale":
            output_error = None
        return {"schema": "centaeris.workspace_benchmark.monitor.v1", "job": self.job,
            "jobStatus": self.job_status, "monitorStatus": status, "monitorError": self.error,
            "sequence": self.sequence, "heartbeatAgeSeconds": round(heartbeat_age, 1) if heartbeat_age is not None else None,
            "lastObservationAgeSeconds": round(age, 1) if age is not None else None,
            "trials": trials, "slots": slots, "exclusiveTrial": exclusive, "logs": list(self.logs),
            "cursorResetCount": self.resets, "readBytes": read_bytes, "catchingUp": catching_up,
            "outputs": outputs, "outputError": output_error, "outputAgeSeconds": output_age}


def create_server(projection: JobProjection, port=9000):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass  # Do not log attacker-controlled request paths or headers.

        def respond(self, code, body, content_type):
            self.send_response(code)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Content-Security-Policy", "default-src 'self'; style-src 'self'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path == "/api/state":
                self.respond(200, json.dumps(projection.snapshot(), ensure_ascii=True, allow_nan=False).encode(), "application/json; charset=utf-8")
                return
            assets = {"/": ("index.html", "text/html"), "/app.js": ("app.js", "text/javascript"), "/style.css": ("style.css", "text/css")}
            if self.path not in assets:
                self.respond(404, b"Not found", "text/plain")
                return
            name, kind = assets[self.path]
            self.respond(200, (ASSETS / name).read_bytes(), kind + "; charset=utf-8")

        def reject_write(self):
            self.respond(405, b"Read-only monitor", "text/plain")

        do_POST = do_PUT = do_PATCH = do_DELETE = do_OPTIONS = do_CONNECT = do_TRACE = reject_write

    return ThreadingHTTPServer(("127.0.0.1", port), Handler)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--job-dir", required=True, type=Path)
    parser.add_argument("--port", default=9000, type=int)
    arguments = parser.parse_args()
    server = create_server(JobProjection(arguments.job_dir), arguments.port)
    print(f"Read-only benchmark monitor: http://127.0.0.1:{server.server_port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
