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
_HTTP_ERROR_CODES = _REASONS | {
    "environment_host_busy", "execution_resident_capacity_wait",
    "retained_cleanup_unconfirmed", "retained_cleanup_unavailable",
    "agent_run_supplement_cleanup_failed", "terminal_subagent_cleanup_unavailable",
}


def _error_code(error):
    # HTTP codes must come from an exact singleton response, never exception text.
    if hasattr(error, "http_error_code"):
        code = error.http_error_code
        return code if isinstance(code, str) and code in _HTTP_ERROR_CODES else "other"
    reason = str(error)
    return reason if reason in _REASONS else "other"


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
                  "errorCode": _error_code(error),
                  "httpStatus": getattr(error, "http_status", None)}
        raise
    finally:
        emit(phase, started, **{**fields, **result})
