"""Isolated ASGI pool observation; never installed in production images.

The wrapper reads the serving process's existing pool registry. It neither
creates a pool nor borrows a connection. stdout records omit exception messages,
request bodies, query strings and headers.
"""
import asyncio
import contextvars
import datetime
import json
import os
import sys
import time
import uuid


MARKER = "CONNECTION_POOL_PROBE "
_request_started = contextvars.ContextVar("pool_probe_request_started", default=None)
_request_id = contextvars.ContextVar("pool_probe_request_id", default=None)


def pool_snapshot(enabled, pools):
    if not enabled:
        return {"state": "disabled"}
    pool = pools.get("default")
    if pool is None:
        return {"state": "uncreated"}
    return {"state": "active", "stats": {
        key: value for key, value in pool.get_stats().items() if type(value) is int}}


def stdout_record(record):
    print(MARKER + json.dumps(record, separators=(",", ":")), flush=True)


class PoolProbe:
    def __init__(self, app, snapshot, sink=stdout_record, interval=1):
        self.app = app
        self.snapshot = snapshot
        self.sink = sink
        self.interval = interval

    def record(self, kind, **fields):
        self.sink({"kind": kind, "pid": os.getpid(),
                   "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                   "pool": self.snapshot(), **fields})

    def record_exception(self, path, method, error):
        started = _request_started.get()
        chain, seen = [], set()
        current = error
        while current is not None and id(current) not in seen and len(chain) < 8:
            seen.add(id(current))
            chain.append(f"{type(current).__module__}.{type(current).__qualname__}")
            current = current.__cause__ or (None if current.__suppress_context__ else current.__context__)
        self.record("exception", path=path, method=method, requestId=_request_id.get(),
                    exceptionType=f"{type(error).__module__}.{type(error).__qualname__}",
                    exceptionChain=chain,
                    elapsedMs=(time.monotonic() - started) * 1000 if started is not None else None)

    async def sample(self):
        while True:
            self.record("sample")
            await asyncio.sleep(self.interval)

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            sampler = asyncio.create_task(self.sample())
            try:
                return await self.app(scope, receive, send)
            finally:
                sampler.cancel()
                try:
                    await sampler
                except asyncio.CancelledError:
                    pass
        if scope["type"] != "http":
            return await self.app(scope, receive, send)
        started = time.monotonic()
        token = _request_started.set(started)
        request_token = _request_id.set(uuid.uuid4().hex)

        async def observed_send(message):
            if message["type"] == "http.response.start" and message["status"] >= 500:
                self.record("httpError", path=scope.get("path", ""), method=scope.get("method", ""),
                            requestId=_request_id.get(), status=message["status"],
                            elapsedMs=(time.monotonic() - started) * 1000)
            await send(message)

        try:
            return await self.app(scope, receive, observed_send)
        except Exception as error:
            self.record_exception(scope.get("path", ""), scope.get("method", ""), error)
            raise
        finally:
            _request_started.reset(token)
            _request_id.reset(request_token)


class ProductionApplication:
    """Lazy imports keep instrumentation tests independent of Django settings."""
    def __init__(self):
        self.probe = None

    async def __call__(self, scope, receive, send):
        if self.probe is None:
            from api.asgi import application as wrapped
            from django.conf import settings
            from django.core.signals import got_request_exception
            from django.db.backends.postgresql.base import DatabaseWrapper

            enabled = bool(settings.DATABASES["default"]["OPTIONS"].get("pool"))
            self.probe = PoolProbe(wrapped, lambda: pool_snapshot(enabled, DatabaseWrapper._connection_pools))

            def request_exception(sender, request, **kwargs):
                error = sys.exc_info()[1]
                if error is not None:
                    self.probe.record_exception(request.path, request.method, error)

            # Django translates most view exceptions into 500 responses before
            # returning through ASGI; this signal preserves their actual type.
            got_request_exception.connect(request_exception, weak=False,
                                          dispatch_uid="perf-connection-pool-probe")
        return await self.probe(scope, receive, send)


application = ProductionApplication()
