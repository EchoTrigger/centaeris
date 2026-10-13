"""Opt-in, body-free observations of the current workspace protection path."""

from contextlib import contextmanager
from contextvars import ContextVar
from functools import wraps
import json
import logging
import os
import re
import sys
import time
from uuid import uuid4


_current = ContextVar("workspace_protection_observation", default=None)
_identity = re.compile(r"[A-Za-z0-9_.:-]{1,160}\Z")
logger = logging.getLogger(__name__)


def _missing(metric):
    try:
        logger.warning("Workspace protection observation unavailable: %s", metric)
    except Exception:
        pass


class _Observation:
    def __init__(self, trigger):
        self.fields = {"schema": "workspace.perf.v1", "source": "api", "phase": "workspaceProtection",
                       "operationId": uuid4().hex, "trigger": trigger}
        self.values = {}
        self.samples = []
        self.unavailable = set()

    def finish(self, outcome):
        for metric, value in [*self.values.items(), *self.samples]:
            if metric in self.unavailable:
                continue
            try:
                event = {**self.fields, "atUnixMs": int(time.time() * 1000),
                         "metric": metric, "value": value, "outcome": outcome}
                print(json.dumps(event, separators=(",", ":")), file=sys.stderr, flush=True)
            except Exception:
                _missing("eventSink")
                break


@contextmanager
def operation(trigger):
    if os.environ.get("WORKSPACE_PERF_OBSERVATIONS") != "1":
        yield
        return
    try:
        observation = _Observation(trigger)
    except Exception:
        _missing("initialization")
        yield
        return
    token = _current.set(observation)
    outcome = "error"
    try:
        yield
        outcome = "ok"
    finally:
        _current.reset(token)
        observation.finish(outcome)


def observe_request(trigger):
    def decorate(function):
        @wraps(function)
        def observed(*args, **kwargs):
            if os.environ.get("WORKSPACE_PERF_OBSERVATIONS") != "1":
                return function(*args, **kwargs)
            try:
                observation = _Observation(trigger)
            except Exception:
                _missing("initialization")
                return function(*args, **kwargs)
            token = _current.set(observation)
            outcome = "error"
            try:
                for metric in ("apiReceivedBytes", "apiWrittenBytes", "apiVerificationReadBytes",
                               "publishedLinks", "committedSnapshots"):
                    increment(metric, 0)
                response = function(*args, **kwargs)
                outcome = "ok" if response.status_code < 400 else "error"
                return response
            finally:
                _current.reset(token)
                observation.finish(outcome)
        return observed
    return decorate


def active():
    return _current.get() is not None


def bind(**fields):
    observation = _current.get()
    if observation is not None:
        for key in ("sessionId", "runId", "jobId"):
            value = fields.get(key)
            if value is not None and isinstance(value, str) and _identity.fullmatch(value):
                observation.fields[key] = value


def increment(metric, value):
    observation = _current.get()
    if observation is not None:
        observation.values[metric] = observation.values.get(metric, 0) + value


def gauge(metric, value):
    observation = _current.get()
    if observation is not None:
        observation.samples.append((metric, value))


def unavailable(metric):
    observation = _current.get()
    if observation is not None:
        observation.unavailable.add(metric)
        _missing(metric)


@contextmanager
def elapsed(metric):
    if not active():
        yield
        return
    try:
        started = time.monotonic()
    except Exception:
        unavailable(metric)
        yield
        return
    try:
        yield
    finally:
        try:
            gauge(metric, (time.monotonic() - started) * 1000)
        except Exception:
            unavailable(metric)
