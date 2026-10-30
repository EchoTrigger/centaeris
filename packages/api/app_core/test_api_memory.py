import asyncio
import json
from pathlib import PurePosixPath
from types import SimpleNamespace
from unittest import IsolatedAsyncioTestCase, TestCase
from unittest.mock import AsyncMock, Mock, patch

from . import api_memory as memory


class ProcessMemoryTests(TestCase):
    def test_unsupported_values_are_null_and_available_allocator_zero_is_real_zero(self):
        with patch.object(memory.sys, "platform", "win32"):
            reader = memory.ProcessMemory()
            sample = reader.sample()
        self.assertEqual(sample["processMemoryStatus"], "unsupported")
        self.assertEqual(sample["cgroupMemoryStatus"], "unsupported")
        self.assertIsNone(sample["rssBytes"])
        self.assertIsNone(sample["cgroupCurrentBytes"])
        self.assertEqual(sample["allocator"]["status"], "unsupported")
        self.assertIsNone(sample["allocator"]["freeArenaBytes"])
        reader._mallinfo = Mock(return_value=memory._Mallinfo2())
        self.assertEqual(reader.sample()["allocator"]["freeArenaBytes"], 0)
        self.assertEqual(reader.sample()["allocator"]["status"], "available")

    def test_linux_process_cgroup_and_allocator_have_distinct_units_and_missing_peak(self):
        files = {
            "/proc/self/cgroup": "0::/tenant/api\n",
            "/proc/self/mountinfo": "1 2 0:1 /tenant /sys/fs/cgroup rw - cgroup2 cgroup rw\n",
            "/proc/self/status": "VmRSS:\t128 kB\nVmHWM:\t256 kB\n",
            "/sys/fs/cgroup/api/memory.current": "1024\n",
        }
        def read(path):
            try:
                return files[str(path).replace('\\', '/')]
            except KeyError:
                raise FileNotFoundError()
        # Use native PurePosix paths in this platform-independent proc fixture.
        with patch.object(memory.sys, "platform", "linux"), \
                patch.object(memory, "Path", PurePosixPath), \
                patch.object(PurePosixPath, "read_text", read, create=True), \
                patch.object(memory.ctypes, "CDLL", return_value=SimpleNamespace()):
            reader = memory.ProcessMemory()
            reader._mallinfo = Mock(return_value=SimpleNamespace(arena=900, uordblks=500, fordblks=400,
                                                                hblkhd=600, keepcost=100))
            sample = reader.sample()
        self.assertEqual(sample["rssBytes"], 128 * 1024)
        self.assertEqual(sample["rssPeakBytes"], 256 * 1024)
        self.assertEqual(sample["cgroupCurrentBytes"], 1024)
        self.assertIsNone(sample["cgroupPeakBytes"])
        self.assertEqual(sample["cgroupMemoryStatus"], "partial")
        self.assertEqual(sample["allocator"], {"status": "available", "arenaBytes": 900,
            "usedArenaBytes": 500, "freeArenaBytes": 400, "mmapBytes": 600, "releasableTopBytes": 100})


class ApiMemoryTests(IsolatedAsyncioTestCase):
    async def test_sampler_has_one_task_and_shutdown_joins_it(self):
        owner = memory.ApiMemoryObservations()
        with patch.object(owner, "emit_sample") as emit:
            owner.start()
            task = owner._task
            owner.start()
            self.assertIs(owner._task, task)
            await asyncio.sleep(0)
            emit.assert_called_once()
            self.assertFalse(task.done())
            await owner.aclose()
            self.assertTrue(task.done())
            self.assertIsNone(owner._task)
            await owner.aclose()

    async def test_request_counts_include_stream_close_after_send_cancellation_and_exclude_content(self):
        with patch.dict(memory.os.environ, {"CENTAERIS_SOURCE_REVISION": "81f9327"}):
            owner = memory.ApiMemoryObservations()
        sending, closing, release = asyncio.Event(), asyncio.Event(), asyncio.Event()
        secret = "private-prompt-and-api-key"
        request = SimpleNamespace(resolver_match=SimpleNamespace(route="api/sessions/<session_id>/events"))

        async def application(scope, receive, send):
            memory.authenticate_request(request)
            memory.bind_agent_run("run-authorized")
            await receive()
            await send({"type": "http.response.start", "status": 200,
                        "headers": [(b"content-type", b"text/event-stream")]})
            try:
                await send({"type": "http.response.body", "body": secret.encode(), "more_body": True})
            finally:
                closing.set()
                await release.wait()

        async def send(message):
            if message["type"] == "http.response.body":
                sending.set()
                await asyncio.Future()

        task = asyncio.create_task(owner.observe_http(
            {"type": "http", "path": f"/{secret}", "query_string": secret.encode(),
             "headers": [(b"authorization", secret.encode())]},
            AsyncMock(return_value={"type": "http.request", "body": secret.encode()}), send, application))
        await sending.wait()
        self.assertEqual((owner.active_http, owner.active_streams), (1, 1))
        task.cancel()
        await closing.wait()
        self.assertEqual((owner.active_http, owner.active_streams), (1, 1))
        release.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual((owner.active_http, owner.active_streams), (0, 0))
        self.assertEqual(owner.cancelled_requests, 1)
        self.assertEqual(owner.received_bytes, len(secret))
        self.assertEqual(owner.sent_bytes, 0)  # Only completed sends count.
        with patch.object(owner._memory, "sample", return_value={}), self.assertLogs(memory.logger, "INFO") as logs:
            owner.emit_sample()
        event = json.loads(logs.records[0].getMessage())
        self.assertNotIn(secret, logs.records[0].getMessage())
        self.assertEqual(event["deploymentRevision"], "81f9327")
        self.assertTrue(event["version"])
        self.assertEqual(event["recentRequests"][0]["agentRunId"], "run-authorized")
        self.assertEqual(event["recentRequests"][0]["operation"], request.resolver_match.route)
        self.assertEqual(event["recentRequests"][0]["outcome"], "cancelled")
        self.assertTrue(event["recentRequests"][0]["authenticated"])
        self.assertEqual(len(event["recentRequests"][0]["requestId"]), 32)
        self.assertIsNone(memory._current.get())

    async def test_active_and_completed_details_are_bounded_and_disconnect_is_observed(self):
        owner = memory.ApiMemoryObservations()
        release = asyncio.Event()
        entered = 0
        all_entered = asyncio.Event()

        async def application(scope, receive, send):
            nonlocal entered
            entered += 1
            if entered == 24:
                all_entered.set()
            await receive()
            memory.mark_response_stream()  # Non-SSE Django streaming response.
            memory.mark_response_stream()
            await release.wait()
            await send({"type": "http.response.start", "status": 200})
            await send({"type": "http.response.body", "body": b"result"})

        tasks = [asyncio.create_task(owner.observe_http({"type": "http"},
            AsyncMock(return_value={"type": "http.disconnect"}), AsyncMock(), application)) for _ in range(24)]
        await all_entered.wait()
        self.assertEqual((owner.active_http, owner.active_streams), (24, 24))
        self.assertEqual(len(owner._active), 16)
        release.set()
        await asyncio.gather(*tasks)
        self.assertEqual((owner.active_http, owner.active_streams), (0, 0))
        self.assertEqual(owner.finished_requests, 24)
        self.assertEqual(owner.sent_bytes, 24 * 6)
        self.assertEqual(len(owner._completed), 16)
        self.assertTrue(all(item["disconnected"] for item in owner._completed))
        self.assertTrue(all(item["operation"] is None for item in owner._completed))
        with patch.object(owner._memory, "sample", return_value={}), patch.object(memory.logger, "info"):
            owner.emit_sample()
        self.assertFalse(owner._completed)
