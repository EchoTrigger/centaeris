"""Opt-in timing and always-on safe failure facts; no bodies, URLs or exception text."""
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
    "environment_controller_owned", "environment_controller_busy",
    "teardown_workspace_preservation_failed",
}

_DIAGNOSTIC_ROUTES = {
    "/agent-runs/control", "/agent-runs/step", "/agent-runs/teardown",
    "/internal/jobs/:id", "/internal/jobs/claim", "/internal/jobs/wait",
    "/internal/jobs/start", "/internal/jobs/heartbeat", "/internal/jobs/yield",
    "/internal/jobs/complete", "/internal/jobs/fail", "/internal/jobs/reconcile",
    "/internal/jobs/control/claim", "/internal/jobs/control/start",
    "/internal/agent-run-lifecycle/control", "/internal/agent-run-lifecycle/resolve",
    "/internal/agent-run-lifecycle/transition", "/internal/agent-run-lifecycle/release",
    "/internal/agent-run-lifecycle/reconcile", "/internal/agent-run-lifecycle/cleanup-reconcile",
}


def _error_code(error):
    # HTTP codes must come from an exact singleton response, never exception text.
    if hasattr(error, "http_error_code"):
        code = error.http_error_code
        return code if isinstance(code, str) and code in _HTTP_ERROR_CODES else "other"
    reason = str(error)
    return reason if reason in _REASONS else "other"


def diagnostic(event, error, *, job_id=None, control_stage=None, lane=None,
               transition_reason=None, slot_index=None):
    """Always record confirmed waits/failures using only bounded public categories."""
    status = getattr(error, "http_status", None)
    route = getattr(error, "route", None)
    fact = {"schema": "workspace.worker.diagnostic.v1", "source": "worker", "event": event,
            "atUnixMs": time.time_ns() // 1_000_000,
            "errorType": type(error).__name__, "errorCode": _error_code(error),
            "httpStatus": status if type(status) is int and 100 <= status <= 599 else None,
            "route": route if isinstance(route, str) and route in _DIAGNOSTIC_ROUTES else None}
    fields = {"jobId": job_id, "controlStage": control_stage, "lane": lane,
              "transitionReason": transition_reason, "slotIndex": slot_index,
              "causeType": getattr(error, "cause_type", None)}
    fact.update({key: value for key, value in fields.items() if value is not None})
    try:
        print(json.dumps(fact, separators=(",", ":")), file=sys.stderr, flush=True)
    except OSError:
        pass  # Diagnostics cannot change the accepted Job handoff or failure.


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
