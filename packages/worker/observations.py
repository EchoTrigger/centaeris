"""Opt-in timing facts. Never log request bodies, URLs, or exception messages."""
from contextlib import contextmanager
import json
import os
import sys
import threading
import time

ENABLED = os.environ.get("WORKSPACE_PERF_OBSERVATIONS") == "1"
_context = threading.local()
_REASONS = {"runtime_busy", "execution_claim_busy"}


def emit(phase, started, **fields):
    if not ENABLED:
        return
    event = {"schema": "workspace.perf.v1", "source": "worker", "phase": phase,
             "atUnixMs": time.time_ns() // 1_000_000,
             "elapsedMs": round((time.monotonic() - started) * 1000, 3),
             **getattr(_context, "fields", {}), **fields}
    try:
        print(json.dumps(event, separators=(",", ":")), file=sys.stderr, flush=True)
    except OSError:
        pass  # Observations must not fail an otherwise successful operation.


@contextmanager
def context(**fields):
    previous = getattr(_context, "fields", {})
    _context.fields = {**previous, **fields}
    try:
        yield
    finally:
        _context.fields = previous


@contextmanager
def measure(phase, **fields):
    if not ENABLED:
        yield
        return
    started = time.monotonic()
    result = {"outcome": "ok"}
    try:
        yield
    except BaseException as error:
        result = {"outcome": "error", "errorType": type(error).__name__,
                  "errorCode": str(error) if str(error) in _REASONS else "other",
                  "httpStatus": getattr(error, "http_status", None)}
        raise
    finally:
        emit(phase, started, **{**fields, **result})
