"""Single-controller, append-only monitor projection. Never an execution ledger.

Restart must reconcile Workspace/Harbor first and create a new projection job.
This file deliberately cannot be used as authority to rerun model work.
"""
from __future__ import annotations

from datetime import datetime, timezone
import json
import os
import queue
from pathlib import Path
import threading

from workspace_monitor import JobProjection, LINE_LIMIT, MANIFEST_LIMIT, redact


class ProjectionConflict(ValueError):
    pass


def _wire(value):
    return (json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":")) + "\n").encode()


class JobEvents:
    """One writer per fresh job; strict allowlist shared with the actual viewer."""

    @classmethod
    def create(cls, root: Path, manifest: dict, *, max_bytes=32 * 1024 * 1024):
        if type(max_bytes) is not int or not 4096 <= max_bytes <= 128 * 1024 * 1024:
            raise ValueError("invalid projection quota")
        wire = _wire(manifest)
        if len(wire) > MANIFEST_LIMIT:
            raise ValueError("manifest too large")
        root = Path(root)
        root.mkdir(parents=True, exist_ok=True)
        if root.is_symlink():
            raise ValueError("unsafe projection directory")
        # Never truncate an old job or infer mutation rights from a log cursor.
        claim = root / "writer.lock"
        try:
            descriptor = os.open(claim, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            raise ProjectionConflict("job projection already claimed") from None
        os.close(descriptor)
        try:
            for name in ("manifest.json", "events.jsonl"):
                if (root / name).exists() or (root / name).is_symlink():
                    raise ProjectionConflict("job projection already exists")
            with (root / "manifest.json").open("xb") as handle:
                os.chmod(root / "manifest.json", 0o644)
                handle.write(wire)
                handle.flush()
                os.fsync(handle.fileno())
            projection = JobProjection(root)
            state = projection.snapshot()
            if state["monitorError"] or state["job"] is None:
                raise ValueError("invalid projection manifest")
            handle = (root / "events.jsonl").open("xb", buffering=0)
            os.chmod(root / "events.jsonl", 0o644)
            return cls(handle, projection, max_bytes)
        except Exception:
            # Keep the claim and invalid artifacts for diagnosis; do not quietly
            # reinterpret a partially initialized job as a fresh execution.
            raise

    def __init__(self, handle, projection, max_bytes):
        self._handle, self._projection, self._limit = handle, projection, max_bytes
        self._bytes = 0
        self._sequence = 0
        self._lock = threading.Lock()
        self._failed = False

    def _append(self, kind, payload, trial_id=None):
        with self._lock:
            if self._handle.closed or self._failed:
                raise ValueError("projection writer closed or failed")
            value = {"schema": "centaeris.workspace_benchmark.event.v1",
                     "sequence": self._sequence + 1,
                     "observedAt": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
                     "kind": kind, "payload": payload}
            if trial_id is not None:
                value["trialId"] = trial_id
            wire = _wire(value)
            if len(wire) > LINE_LIMIT or self._bytes + len(wire) > self._limit:
                raise ValueError("projection quota exceeded")
            # Reuse the strict consumer's validator before emitting any bytes.
            # It retains only compact identities and a bounded log ring.
            self._projection._event(value)
            try:
                written = self._handle.write(wire)
                if written != len(wire):
                    raise OSError("incomplete projection append")
                os.fsync(self._handle.fileno())
            except OSError:
                self._failed = True
                raise ValueError("projection append failed; reconcile job") from None
            self._sequence += 1
            self._bytes += len(wire)

    def job(self, status):
        self._append("job", {"status": status})

    def trial(self, trial_id, **fields):
        clean = {key: redact(value) if isinstance(value, str) else value for key, value in fields.items()}
        self._append("trial", clean, trial_id)

    def log(self, message, *, level="info", trial_id=None):
        if not isinstance(message, str):
            raise ValueError("log message must be text")
        self._append("log", {"level": level, "message": redact(message)}, trial_id)

    def close(self):
        with self._lock:
            self._handle.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class FailOpenProjection:
    """Bounded best-effort telemetry; disk or viewer failures never control a trial.

    Overflow stops this projection instead of silently drawing a complete-looking
    stream after missing lifecycle facts. The viewer then shows its stale age.
    A blocked filesystem write can retain only one record plus this small queue.
    """

    def __init__(self, writer, *, max_pending=128):
        if type(max_pending) is not int or not 1 <= max_pending <= 128:
            raise ValueError("invalid telemetry queue bound")
        self._writer = writer
        self._queue = queue.Queue(max_pending)
        self._degraded = threading.Event()
        self._closed = threading.Event()
        self._thread = threading.Thread(target=self._run, name="benchmark-projection", daemon=True)
        self._thread.start()

    @property
    def stats(self):
        return {"degraded": self._degraded.is_set(), "pending": self._queue.qsize()}

    def submit(self, kind, payload, trial_id=None):
        if self._closed.is_set() or self._degraded.is_set():
            return False
        try:
            # Copy only a bounded wire record. Do not retain an observer's mutable
            # dict or an entire tool/session object in a background queue.
            wire = _wire({"kind": kind, "payload": payload, "trialId": trial_id})
            if len(wire) > LINE_LIMIT:
                raise ValueError("telemetry record too large")
            self._queue.put_nowait(wire)
            return True
        except (ValueError, TypeError, OverflowError, queue.Full):
            self._degraded.set()
            return False

    def _run(self):
        try:
            while not self._degraded.is_set():
                try:
                    wire = self._queue.get(timeout=0.05)
                except queue.Empty:
                    if self._closed.is_set():
                        break
                    continue
                value = json.loads(wire)
                kind, payload, trial_id = value["kind"], value["payload"], value["trialId"]
                if kind == "trial":
                    self._writer.trial(trial_id, **payload)
                elif kind == "job":
                    self._writer.job(payload["status"])
                elif kind == "log":
                    self._writer.log(payload["message"], level=payload["level"], trial_id=trial_id)
                else:
                    raise ValueError("unsupported telemetry kind")
        except Exception:
            self._degraded.set()
        finally:
            try:
                self._writer.close()
            except Exception:
                self._degraded.set()
            while True:
                try:
                    self._queue.get_nowait()
                except queue.Empty:
                    break

    def close(self, *, timeout=0.1):
        if type(timeout) not in {int, float} or not 0 <= timeout <= 1:
            raise ValueError("telemetry close wait exceeds one second")
        self._closed.set()
        self._thread.join(timeout=timeout)
