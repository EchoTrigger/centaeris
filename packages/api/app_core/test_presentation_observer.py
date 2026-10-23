import asyncio
from contextlib import asynccontextmanager
from types import SimpleNamespace
from unittest.mock import AsyncMock
from django.test import SimpleTestCase
from .presentation_observer import PresentationObserverHub
from .presentation_notifications import PresentationHint
from .presentation_stream import PresentationScope

class PresentationObserverTests(SimpleTestCase):
    async def test_display_observation_uses_five_versions_without_an_authorization_epoch(self):
        async def hints(ready):
            ready.set()
            await asyncio.Event().wait()
            yield PresentationHint(None, True)
        versions = dict(navigation=1, state=2, context=3, history=4, work=5)
        hub = PresentationObserverHub(AsyncMock(return_value=versions), hints, lambda _: ())
        try:
            async with hub.observe(('owner',), PresentationScope('workspace', 'agent'), SimpleNamespace()) as observed:
                self.assertEqual(await observed.revisions(), {**versions, 'live': 0})
        finally:
            await hub.aclose()

    async def test_two_identical_readers_share_initial_data_and_idle_does_not_reload_it(self):
        queue = asyncio.Queue()
        async def hints(ready):
            ready.set()
            while True: yield PresentationHint(await queue.get(), True)
        versions = dict(navigation=1,state=1,context=0,history=0,work=0)
        reader = AsyncMock(side_effect=lambda scope:dict(versions))
        loader = SimpleNamespace(navigation=AsyncMock(return_value=[]),state=AsyncMock(return_value={'agentRun':None,'sourceHighWater':'0'}),authorized=AsyncMock(return_value=True))
        hub = PresentationObserverHub(reader,hints,lambda scope:{'agent:agent:navigation','session:session:state'})
        scope = PresentationScope('workspace','agent','session')
        try:
            async with hub.observe(('owner',scope),scope,loader) as first, hub.observe(('owner',scope),scope,loader) as second:
                self.assertEqual(await first.navigation(),await second.navigation())
                await first.state(); await second.state()
                await queue.put(None)
                await asyncio.sleep(.01)
                await first.navigation(); await first.state()
                self.assertEqual(loader.navigation.await_count,1)
                self.assertEqual(loader.state.await_count,1)
                versions['state']=2
                await queue.put('session:session:state')
                await asyncio.wait_for(first.wait_for_change(await second.revisions()),.2)
                await second.state()
                self.assertEqual(loader.state.await_count,2)
                self.assertEqual(loader.navigation.await_count,1)
        finally: await hub.aclose()
        self.assertEqual(hub.observer_count,0)

    async def test_scope_authority_isolation_and_subscription_capacity(self):
        queue=asyncio.Queue()
        async def hints(ready):
            ready.set()
            while True: yield PresentationHint(await queue.get(), True)
        reader=AsyncMock(return_value=dict(navigation=0,state=0,context=0,history=0,work=0))
        loader=SimpleNamespace(navigation=AsyncMock(return_value=[]),authorized=AsyncMock(return_value=True))
        hub=PresentationObserverHub(reader,hints,lambda scope:{'agent:agent:navigation'},max_observers=1)
        scope=PresentationScope('workspace','agent')
        try:
            async with hub.observe(('owner-A',scope),scope,loader):
                with self.assertRaisesRegex(RuntimeError,'capacity'):
                    async with hub.observe(('owner-B',scope),scope,loader): self.fail('capacity exceeded')
        finally: await hub.aclose()

    async def test_repeated_hints_and_other_scope_changes_do_not_read_business_data(self):
        queue=asyncio.Queue()
        async def hints(ready):
            ready.set()
            while True: yield PresentationHint(await queue.get(), True)
        versions=dict(navigation=0,state=0,context=0,history=0,work=0)
        reader=AsyncMock(side_effect=lambda scope:dict(versions))
        loader=SimpleNamespace(navigation=AsyncMock(return_value=[]),authorized=AsyncMock(return_value=True))
        hub=PresentationObserverHub(reader,hints,lambda scope:{'agent:agent:navigation'})
        try:
            async with hub.observe(('owner',),PresentationScope('workspace','agent'),loader) as observed:
                await observed.navigation()
                for key in ['agent:other:navigation','agent:agent:navigation','agent:agent:navigation']: await queue.put(key)
                await asyncio.sleep(.01)
                await observed.navigation()
                self.assertEqual(loader.navigation.await_count,1)
        finally: await hub.aclose()

    async def test_change_committed_during_refresh_is_not_lost(self):
        queue=asyncio.Queue(); entered=asyncio.Event(); release=asyncio.Event()
        async def hints(ready):
            ready.set()
            while True: yield PresentationHint(await queue.get(), True)
        versions=dict(navigation=0,state=0,context=0,history=0,work=0)
        calls=0
        async def read(scope):
            nonlocal calls
            calls+=1
            result=dict(versions)
            if calls==2:
                entered.set(); await release.wait()
            return result
        loader=SimpleNamespace(navigation=AsyncMock(return_value=[]),authorized=AsyncMock(return_value=True))
        hub=PresentationObserverHub(read,hints,lambda scope:{'agent:agent:navigation'})
        try:
            async with hub.observe(('owner',),PresentationScope('workspace','agent'),loader) as observed:
                versions['navigation']=1; await queue.put('agent:agent:navigation'); await entered.wait()
                versions['navigation']=2; await queue.put('agent:agent:navigation'); await asyncio.sleep(0)
                release.set(); await asyncio.sleep(.02)
                self.assertEqual((await observed.revisions())['navigation'],2)
        finally: await hub.aclose()

    async def test_prepared_frames_keep_independent_fresh_authorization_checks(self):
        async def hints(ready):
            ready.set()
            await asyncio.Event().wait()
            yield PresentationHint(None, True)
        loader = SimpleNamespace(authorized=AsyncMock(side_effect=[True, False]))
        reader = AsyncMock(return_value=dict(navigation=0,state=0,context=0,history=0,work=0))
        hub = PresentationObserverHub(reader, hints, lambda _: ())
        scope = PresentationScope('workspace', 'agent')
        try:
            async with hub.observe(('owner',), scope, loader) as first, hub.observe(('owner',), scope, loader) as second:
                self.assertTrue(await first.authorized())
                self.assertFalse(await second.authorized())
                self.assertEqual(loader.authorized.await_count, 2)
        finally:
            await hub.aclose()

    async def test_relay_backlog_keeps_initial_reads_available_with_a_finite_drain_budget(self):
        async def hints(ready):
            ready.set()
            yield PresentationHint(None, True)
            await asyncio.Event().wait()
        drain = AsyncMock(return_value=256)
        reader = AsyncMock(return_value=dict(navigation=0,state=0,context=0,history=0,work=0))
        hub = PresentationObserverHub(reader, hints, lambda _: (), drain=drain)
        try:
            async with hub.observe(('owner',), PresentationScope('workspace','agent'), SimpleNamespace()) as observed:
                await asyncio.sleep(.02)
                self.assertEqual((await observed.revisions())['navigation'], 0)
                self.assertEqual(drain.await_count, 4)
        finally:
            await hub.aclose()

    async def test_concurrent_run_transition_owns_only_one_new_live_waiter(self):
        queue = asyncio.Queue()
        closing, release, started = asyncio.Event(), asyncio.Event(), asyncio.Event()
        active = set()
        tails = []
        async def hints(ready):
            ready.set()
            while True:
                yield PresentationHint(await queue.get(), True)
        async def tail(run):
            tails.append(run)
            return "0"
        async def wait(run, cursor):
            task = asyncio.current_task()
            active.add(task)
            if run == "A":
                started.set()
            try:
                await asyncio.Event().wait()
            finally:
                if run == "A":
                    closing.set()
                    while not release.is_set():
                        try:
                            await release.wait()
                        except asyncio.CancelledError:
                            pass
                active.discard(task)
        versions = dict(navigation=0, state=0, context=0, history=0, work=0)
        loader = SimpleNamespace(state=AsyncMock(return_value={"sourceHighWater": "0", "agentRun": {"agentRunId": "A", "status": "running"}}),
            live_signal_tail=tail, wait_live_signal=wait)
        hub = PresentationObserverHub(AsyncMock(side_effect=lambda scope: dict(versions)), hints, lambda scope: {"state"})
        scope = PresentationScope("workspace", "agent", "session")
        try:
            async with hub.observe(("owner",), scope, loader) as first, hub.observe(("owner",), scope, loader) as second:
                await first.state()
                await asyncio.wait_for(started.wait(), 1)
                versions["state"] = 1
                loader.state.return_value = {"sourceHighWater": "1", "agentRun": {"agentRunId": "B", "status": "running"}}
                await queue.put("state")
                async with asyncio.timeout(1):
                    while (await first.revisions())["state"] != 1:
                        await asyncio.sleep(0)
                one = asyncio.create_task(first.state())
                await asyncio.wait_for(closing.wait(), 1)
                two = asyncio.create_task(second.state())
                await asyncio.sleep(0)
                release.set()
                await asyncio.gather(one, two)
                for _ in range(3):
                    await asyncio.sleep(0)
                self.assertEqual(tails.count("B"), 1)
                await hub.aclose()
                self.assertFalse(active)
        finally:
            release.set()
            await hub.aclose()
            for task in tuple(active):
                task.cancel()
            await asyncio.gather(*active, return_exceptions=True)

    async def test_late_live_snapshot_from_previous_run_cannot_be_cached_for_next_run(self):
        queue = asyncio.Queue()
        old_read, finish_old, new_tail = asyncio.Event(), asyncio.Event(), asyncio.Event()
        async def hints(ready):
            ready.set()
            while True:
                yield PresentationHint(await queue.get(), True)
        async def tail(run):
            if run == "B":
                await new_tail.wait()
            return "0"
        async def wait(run, cursor):
            await asyncio.Event().wait()
        async def live(run):
            if run == "A":
                old_read.set()
                await finish_old.wait()
            return {"text": run}
        versions = dict(navigation=0, state=0, context=0, history=0, work=0)
        loader = SimpleNamespace(state=AsyncMock(return_value={"sourceHighWater": "0", "agentRun": {"agentRunId": "A", "status": "running"}}),
            live_signal_tail=tail, wait_live_signal=wait, live=AsyncMock(side_effect=live))
        hub = PresentationObserverHub(AsyncMock(side_effect=lambda scope: dict(versions)), hints, lambda scope: {"state"})
        try:
            async with hub.observe(("owner",), PresentationScope("workspace", "agent", "session"), loader) as observed:
                await observed.state()
                await asyncio.sleep(0)
                old = asyncio.create_task(observed.live("A"))
                await asyncio.wait_for(old_read.wait(), 1)
                versions["state"] = 1
                loader.state.return_value = {"sourceHighWater": "1", "agentRun": {"agentRunId": "B", "status": "running"}}
                await queue.put("state")
                async with asyncio.timeout(1):
                    while (await observed.revisions())["state"] != 1:
                        await asyncio.sleep(0)
                await observed.state()
                finish_old.set()
                self.assertEqual(await old, {"text": "A"})
                self.assertEqual(await observed.live("B"), {"text": "B"})
                self.assertEqual(loader.live.await_count, 2)
        finally:
            finish_old.set()
            new_tail.set()
            await hub.aclose()

    async def test_projected_notifications_wake_readers_without_recursively_draining(self):
        queue = asyncio.Queue()
        async def hints(ready):
            ready.set()
            while True:
                yield await queue.get()
        versions = dict(navigation=0,state=0,context=0,history=0,work=0)
        drain = AsyncMock(return_value=256)
        hub = PresentationObserverHub(AsyncMock(side_effect=lambda _: dict(versions)), hints, lambda _: {'agent:agent:navigation'}, drain=drain)
        try:
            async with hub.observe(('owner',), PresentationScope('workspace','agent'), SimpleNamespace()) as observed:
                previous = await observed.revisions()
                versions['navigation'] = 1
                await queue.put(PresentationHint('agent:agent:navigation', False))
                await asyncio.wait_for(observed.wait_for_change(previous), .2)
                self.assertEqual((await observed.revisions())['navigation'], 1)
                drain.assert_not_awaited()
        finally:
            await hub.aclose()

    async def test_silent_initial_version_read_times_out_and_releases_its_observation(self):
        stopped = asyncio.Event()
        async def hints(ready):
            ready.set()
            await asyncio.Event().wait()
            yield PresentationHint(None, True)
        async def read(_scope):
            try:
                await asyncio.Event().wait()
            finally:
                stopped.set()
        hub = PresentationObserverHub(read, hints, lambda _: (), read_timeout=.01)
        try:
            with self.assertRaises(TimeoutError):
                async with hub.observe(('owner',), PresentationScope('workspace','agent'), SimpleNamespace()):
                    self.fail('silent version reads cannot acquire an unbounded subscription')
            self.assertEqual(hub.observer_count, 0)
            self.assertTrue(stopped.is_set())
        finally:
            await hub.aclose()

    async def test_notification_source_eof_fails_existing_waiters_instead_of_heartbeating_forever(self):
        finish = asyncio.Event()
        async def hints(ready):
            ready.set()
            await finish.wait()
            if False:
                yield PresentationHint(None, True)
        reader = AsyncMock(return_value=dict(navigation=0,state=0,context=0,history=0,work=0))
        hub = PresentationObserverHub(reader, hints, lambda _: ())
        try:
            async with hub.observe(('owner',), PresentationScope('workspace','agent'), SimpleNamespace()) as observed:
                previous = await observed.revisions()
                finish.set()
                with self.assertRaisesRegex(RuntimeError, 'ended'):
                    await asyncio.wait_for(observed.wait_for_change(previous), .05)
        finally:
            await hub.aclose()
