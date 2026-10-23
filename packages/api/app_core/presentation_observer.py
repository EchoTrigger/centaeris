"""Application-owned, bounded observations shared only within exact read grants."""
import asyncio
from contextlib import asynccontextmanager


class PresentationObserverHub:
    def __init__(self, read_versions, hints, change_keys, *, max_observers=128, max_subscriptions=512, max_owner_subscriptions=16, drain=None, read_timeout=5.0):
        self.read_versions = read_versions
        self.hints = hints
        self.change_keys = change_keys
        self.drain = drain
        self.read_timeout = read_timeout
        self.max_owner_subscriptions = max_owner_subscriptions
        self._owners = {}
        self.max_observers = max_observers
        self.max_subscriptions = max_subscriptions
        self._groups = {}
        self._lock = asyncio.Lock()
        self._listener = None
        self._ready = asyncio.Event()
        self._closed = False
        self._subscriptions = 0

    @property
    def observer_count(self):
        return len(self._groups)

    async def _monitor(self):
        try:
            async for hint in self.hints(self._ready):
                if self.drain is not None and hint.needs_drain:
                    for _ in range(4):
                        if await asyncio.wait_for(self.drain(), self.read_timeout) < 256:
                            break
                    # Remaining durable dirtiness waits for the next source
                    # hint, bounded reconciliation or maintenance invocation.
                    # Projected hints do not recursively trigger more draining.
                groups = tuple(self._groups.values())
                # A timeout or reconnect performs one bounded version check per
                # active shared scope. It never reloads unchanged business data.
                for group in groups:
                    if hint.key is None or hint.key in group.keys:
                        group.schedule_refresh()
            raise RuntimeError("presentation change listener ended")
        except asyncio.CancelledError:
            raise
        except Exception as error:
            self._ready.set()
            for group in tuple(self._groups.values()):
                await group.fail(error)

    @asynccontextmanager
    async def observe(self, key, scope, loader):
        async with self._lock:
            if self._closed:
                raise RuntimeError("presentation observer is closed")
            owner = key[0]
            if self._owners.get(owner, 0) >= self.max_owner_subscriptions or self._subscriptions >= self.max_subscriptions or (key not in self._groups and len(self._groups) >= self.max_observers):
                raise RuntimeError("presentation observer capacity exceeded")
            group = self._groups.get(key)
            if group is None:
                group = _Observation(self, scope, loader)
                self._groups[key] = group
            group.readers += 1
            self._subscriptions += 1
            self._owners[owner] = self._owners.get(owner, 0) + 1
            if self._listener is None:
                self._ready.clear()
                self._listener = asyncio.create_task(self._monitor())
        try:
            await asyncio.wait_for(self._ready.wait(), 5)
            if self._listener.done():
                raise RuntimeError("presentation change listener unavailable")
            await group.initialize()
            yield _ObservedLoader(group, loader)
        finally:
            async with self._lock:
                group.readers -= 1
                self._subscriptions -= 1
                self._owners[owner] -= 1
                if self._owners[owner] == 0:
                    self._owners.pop(owner)
                if group.readers == 0:
                    self._groups.pop(key, None)
                    await group.aclose()
                if not self._groups and self._listener is not None:
                    self._listener.cancel()
                    await asyncio.gather(self._listener, return_exceptions=True)
                    self._listener = None

    async def aclose(self):
        self._closed = True
        if self._listener is not None:
            self._listener.cancel()
            await asyncio.gather(self._listener, return_exceptions=True)
            self._listener = None
        for group in tuple(self._groups.values()):
            await group.fail(RuntimeError("presentation observer is closed"))
            await group.aclose()
        self._groups.clear()


class _Observation:
    def __init__(self, hub, scope, loader):
        self.hub, self.scope, self.loader = hub, scope, loader
        self.keys = frozenset(hub.change_keys(scope))
        self.readers = 0
        self.versions = None
        self.error = None
        self.condition = asyncio.Condition()
        self.refresh_lock = asyncio.Lock()
        self.initial_lock = asyncio.Lock()
        self.topic_locks = {}
        self.cache = {}
        self.refresh_task = None
        self.refresh_dirty = False
        self.live_lock = asyncio.Lock()
        self.closed = False
        self.live_task = None
        self.live_run = None
        self.live_revision = 0

    async def initialize(self):
        async with self.initial_lock:
            if self.versions is None:
                await self.refresh()

    async def refresh(self):
        async with self.refresh_lock:
            result = await asyncio.wait_for(self.hub.read_versions(self.scope), self.hub.read_timeout)
            expected = {"navigation", "state", "context", "history", "work", "authority"}
            if set(result) != expected or any(type(value) is not int or value < 0 for value in result.values()):
                raise RuntimeError("presentation change versions invalid")
            async with self.condition:
                if self.versions != result:
                    self.versions = dict(result)
                    self.condition.notify_all()

    def schedule_refresh(self):
        self.refresh_dirty = True
        if self.refresh_task is None or self.refresh_task.done():
            self.refresh_task = asyncio.create_task(self._refresh_owned())

    async def _refresh_owned(self):
        try:
            while self.refresh_dirty:
                self.refresh_dirty = False
                await self.refresh()
        except Exception as error:
            await self.fail(error)

    async def fail(self, error):
        async with self.condition:
            self.error = error
            self.condition.notify_all()

    def snapshot(self):
        if self.error is not None:
            raise self.error
        return {**self.versions, "live": self.live_revision}

    async def wait_for_change(self, previous):
        async with self.condition:
            while self.snapshot() == previous:
                await self.condition.wait()

    async def memo(self, topic, operation, *, identity=None):
        lock = self.topic_locks.setdefault(topic, asyncio.Lock())
        async with lock:
            version = (identity, self.live_revision) if topic == "live" else self.versions[topic]
            stored = self.cache.get(topic)
            if stored is None or stored[0] != version:
                value = await operation()
                self.cache[topic] = (version, value)
            return self.cache[topic][1]

    async def set_live_run(self, run):
        async with self.live_lock:
            if self.closed or run == self.live_run:
                return
            if self.live_task is not None:
                self.live_task.cancel()
                await asyncio.gather(self.live_task, return_exceptions=True)
                self.live_task = None
            if self.closed:
                return
            self.live_run = run
            self.cache.pop("live", None)
            if run is not None:
                self.live_task = asyncio.create_task(self._watch_live(run))
    async def _watch_live(self, run):
        # Redis is ephemeral. Its loss cannot hide committed progress: the
        # database change listener remains active independently of this waiter.
        for attempt in range(5):
            try:
                cursor = await self.loader.live_signal_tail(run)
                async with self.condition:
                    self.live_revision += 1
                    self.condition.notify_all()
                while True:
                    next_cursor = await self.loader.wait_live_signal(run, cursor)
                    if next_cursor != cursor:
                        cursor = next_cursor
                        async with self.condition:
                            self.live_revision += 1
                            self.condition.notify_all()
            except asyncio.CancelledError:
                raise
            except Exception:
                if attempt < 4:
                    await asyncio.sleep(min(2 ** attempt, 16))

    async def aclose(self):
        self.closed = True
        async with self.live_lock:
            tasks = [task for task in (self.refresh_task, self.live_task) if task is not None]
            for task in tasks:
                task.cancel()
            if tasks:
                await asyncio.gather(*tasks, return_exceptions=True)
            self.live_task = None
            self.cache.clear()

class _ObservedLoader:
    def __init__(self, group, original):
        self.group, self.original = group, original

    def __getattr__(self, name):
        return getattr(self.original, name)

    async def authorized(self):
        return await self.original.authorized()

    async def revisions(self):
        return self.group.snapshot()

    async def wait_for_change(self, previous):
        await self.group.wait_for_change(previous)

    async def navigation(self):
        return await self.group.memo("navigation", self.original.navigation)

    async def state(self):
        state = await self.group.memo("state", self.original.state)
        run = state["agentRun"]
        await self.group.set_live_run(run["agentRunId"] if run is not None and run["status"] in {"queued", "running"} else None)
        return state

    async def live(self, run):
        return await self.group.memo("live", lambda: self.original.live(run), identity=run)
