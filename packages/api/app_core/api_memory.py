"""Bounded, content-free process and ASGI lifetime observations.

These are passive allocator/process statistics, not allocation-stack profiling.
"""
import asyncio
from collections import deque
from contextvars import ContextVar
import ctypes
from dataclasses import dataclass
from importlib.metadata import PackageNotFoundError, version
import json
import logging
import os
from pathlib import Path
import sys
import time
from uuid import uuid4


logger = logging.getLogger("uvicorn.error.memory")
_current = ContextVar("api_memory_request", default=None)
_SAMPLE_SECONDS = 30
_REQUEST_SAMPLE_LIMIT = 16


class _Mallinfo2(ctypes.Structure):
    _fields_ = [(name, ctypes.c_size_t) for name in (
        "arena", "ordblks", "smblks", "hblks", "hblkhd", "usmblks",
        "fsmblks", "uordblks", "fordblks", "keepcost",
    )]


def _read_integer(path):
    try:
        return int(path.read_text().strip())
    except (OSError, ValueError):
        return None


class ProcessMemory:
    def __init__(self):
        self._mallinfo = None
        self._libc = None
        self._cgroup = None
        if sys.platform != "linux":
            return
        self._libc = ctypes.CDLL(None)
        # mallinfo2 is a glibc API. A missing symbol means unsupported, not zero.
        if hasattr(self._libc, "gnu_get_libc_version") and hasattr(self._libc, "mallinfo2"):
            self._mallinfo = self._libc.mallinfo2
            self._mallinfo.argtypes = []
            self._mallinfo.restype = _Mallinfo2
        self._cgroup = self._cgroup_directory()

    @staticmethod
    def _cgroup_directory():
        try:
            membership = next(line[3:] for line in Path("/proc/self/cgroup").read_text().splitlines()
                              if line.startswith("0::"))
            # Resolve this process's unified hierarchy against its actual mount.
            for line in Path("/proc/self/mountinfo").read_text().splitlines():
                before, after = line.split(" - ", 1)
                if after.split()[0] != "cgroup2":
                    continue
                fields = before.split()
                root, mount = fields[3:5]
                relative = Path(membership).relative_to(root)
                if ".." not in relative.parts:
                    return Path(mount) / relative
        except (OSError, ValueError, StopIteration):
            pass
        return None

    def sample(self):
        rss = peak = None
        if sys.platform == "linux":
            try:
                values = dict(line.split(":", 1) for line in Path("/proc/self/status").read_text().splitlines())
                rss = int(values["VmRSS"].split()[0]) * 1024
                peak = int(values["VmHWM"].split()[0]) * 1024
            except (OSError, KeyError, ValueError):
                pass
        current = _read_integer(self._cgroup / "memory.current") if self._cgroup is not None else None
        cgroup_peak = _read_integer(self._cgroup / "memory.peak") if self._cgroup is not None else None
        allocator = {"status": "unsupported", "arenaBytes": None, "usedArenaBytes": None,
                     "freeArenaBytes": None, "mmapBytes": None, "releasableTopBytes": None}
        if self._mallinfo is not None:
            info = self._mallinfo()
            allocator = {"status": "available", "arenaBytes": info.arena,
                         "usedArenaBytes": info.uordblks, "freeArenaBytes": info.fordblks,
                         "mmapBytes": info.hblkhd, "releasableTopBytes": info.keepcost}
        process_status = "available" if rss is not None else ("unavailable" if sys.platform == "linux" else "unsupported")
        cgroup_status = "available" if current is not None and cgroup_peak is not None else (
            "partial" if current is not None or cgroup_peak is not None else (
                "unavailable" if sys.platform == "linux" else "unsupported"))
        return {"processMemoryStatus": process_status, "cgroupMemoryStatus": cgroup_status,
                "rssBytes": rss, "rssPeakBytes": peak,
                "cgroupCurrentBytes": current, "cgroupPeakBytes": cgroup_peak,
                "allocator": allocator}


@dataclass
class RequestObservation:
    owner: object
    request_id: str
    started: float
    authenticated: bool = False
    operation: str | None = None
    agent_run_id: str | None = None
    streaming: bool = False
    received_bytes: int = 0
    sent_bytes: int = 0
    status: int | None = None
    disconnected: bool = False

    def mark_stream(self):
        if not self.streaming:
            self.streaming = True
            self.owner.active_streams += 1

    def fields(self, now):
        return {"requestId": self.request_id, "authenticated": self.authenticated,
                "operation": self.operation, "agentRunId": self.agent_run_id,
                "streaming": self.streaming, "receivedBytes": self.received_bytes,
                "sentBytes": self.sent_bytes, "status": self.status,
                "durationMs": round((now - self.started) * 1000),
                "disconnected": self.disconnected}


def authenticate_request(request):
    """Called after existing authentication succeeds; never inspect credentials."""
    observation = _current.get()
    if observation is not None:
        observation.authenticated = True
        match = getattr(request, "resolver_match", None)
        # The registered route template contains no query string or path values.
        observation.operation = getattr(match, "route", None)


def bind_agent_run(agent_run_id):
    """Bind only an identity already accepted by the route's authorization checks."""
    observation = _current.get()
    if observation is not None:
        observation.agent_run_id = str(agent_run_id)


def mark_response_stream():
    observation = _current.get()
    if observation is not None:
        observation.mark_stream()


class ApiMemoryObservations:
    def __init__(self):
        self.started = time.monotonic()
        self.deployment_revision = os.environ.get("CENTAERIS_SOURCE_REVISION") or None
        self.active_http = 0
        self.active_streams = 0
        self.received_bytes = 0
        self.sent_bytes = 0
        self.finished_requests = 0
        self.cancelled_requests = 0
        self.failed_requests = 0
        self._active = {}
        self._completed = deque(maxlen=_REQUEST_SAMPLE_LIMIT)
        self._task = None
        self._memory = ProcessMemory()
        try:
            self.version = version("api")
        except PackageNotFoundError:
            self.version = "0.1.0"

    def start(self):
        if self._task is None:
            self._task = asyncio.create_task(self._sample_loop(), name="api-memory-observations")

    async def aclose(self):
        if self._task is not None:
            task, self._task = self._task, None
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass

    async def _sample_loop(self):
        while True:
            self.emit_sample()
            await asyncio.sleep(_SAMPLE_SECONDS)

    def emit_sample(self):
        now = time.monotonic()
        # At most 16 active and 16 recently completed identities per 30s sample.
        event = {"event": "api_memory_sample", "schema": "api.memory.v1",
                 "atUnixMs": int(time.time() * 1000), "version": self.version,
                 "deploymentRevision": self.deployment_revision,
                 "pid": os.getpid(), "uptimeSeconds": round(now - self.started, 3),
                 **self._memory.sample(), "activeHttp": self.active_http,
                 "activeStreams": self.active_streams,
                 "receivedBytes": self.received_bytes, "sentBytes": self.sent_bytes,
                 "finishedRequests": self.finished_requests,
                 "cancelledRequests": self.cancelled_requests, "failedRequests": self.failed_requests,
                 "activeRequests": [item.fields(now) for item in self._active.values()],
                 "recentRequests": list(self._completed)}
        logger.info(json.dumps(event, separators=(",", ":")))
        self._completed.clear()

    async def observe_http(self, scope, receive, send, application):
        observation = RequestObservation(self, uuid4().hex, time.monotonic())
        token = _current.set(observation)
        self.active_http += 1
        if len(self._active) < _REQUEST_SAMPLE_LIMIT:
            self._active[observation.request_id] = observation

        async def counted_receive():
            message = await receive()
            if message["type"] == "http.request":
                size = len(message.get("body", b""))
                observation.received_bytes += size
                self.received_bytes += size
            elif message["type"] == "http.disconnect":
                observation.disconnected = True
            return message

        async def counted_send(message):
            if message["type"] == "http.response.start":
                observation.status = message["status"]
                if any(name.lower() == b"content-type" and value.split(b";", 1)[0] == b"text/event-stream"
                       for name, value in message.get("headers", [])):
                    observation.mark_stream()
            await send(message)
            if message["type"] == "http.response.body":
                size = len(message.get("body", b""))
                observation.sent_bytes += size
                self.sent_bytes += size

        outcome = "completed"
        try:
            return await application(scope, counted_receive, counted_send)
        except asyncio.CancelledError:
            outcome = "cancelled"
            self.cancelled_requests += 1
            raise
        except BaseException:
            outcome = "failed"
            self.failed_requests += 1
            raise
        finally:
            self.active_http -= 1
            if observation.streaming:
                self.active_streams -= 1
            self.finished_requests += 1
            self._active.pop(observation.request_id, None)
            self._completed.append({**observation.fields(time.monotonic()), "outcome": outcome})
            _current.reset(token)
