"""Behavioral tests for the headless Runtime client; no external model calls."""
import asyncio
import io
import json
import tempfile
import unittest
import os
import shutil
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from runner import Rpc, run_trial, trial_profile, collect_provider_usage


class FakeRpc:
    def __init__(self, statuses):
        self.statuses = iter(statuses)
        self.calls = []

    async def call(self, method, request):
        self.calls.append((method, request))
        if method == "agent_runtime_config_set":
            return {"modelProviderId": request["modelProviderId"], "model": request.get("model"),
                    "modelThinkingMode": request.get("modelThinkingMode")}
        if method == "session/new":
            return {"id": "conversation-test"}
        if method == "session/prompt":
            return {"agentRunId": "run-test", "sessionId": "conversation-test"}
        if method == "_centaeris/session/agent-runs":
            return {"agentRuns": [{"agentRunId": "run-test", "status": next(self.statuses)}]}
        if method == "session/load":
            return {"id": "conversation-test", "messages": []}
        return {}


class TrialTests(unittest.IsolatedAsyncioTestCase):
    @unittest.skipUnless(shutil.which('curl'), 'requires curl')
    def test_verifier_download_recovers_from_partial_transfer(self):
        requests = []
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append(self.path)
                self.send_response(200)
                self.send_header('Content-Length', '10' if len(requests) == 1 else '2')
                self.end_headers()
                self.wfile.write(b'ok')
                self.close_connection = True
            def log_message(self, *args):
                pass
        with tempfile.TemporaryDirectory() as directory:
            shutil.copyfile(Path(__file__).with_name('verifier.curlrc'), Path(directory) / '.curlrc')
            server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                output = Path(directory) / 'download'
                result = subprocess.run([shutil.which('curl'), '--noproxy', '*', '-sSf',
                                         '-o', str(output), f'http://127.0.0.1:{server.server_port}/asset'],
                                        env={**os.environ, 'CURL_HOME': directory},
                                        capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(output.read_bytes(), b'ok')
                self.assertEqual(len(requests), 2)
            finally:
                server.shutdown()
                thread.join()
                server.server_close()

    def test_usage_export_sums_authoritative_turns_and_preserves_missing_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sessions = root / 'sessions/day'
            sessions.mkdir(parents=True)
            records = [{'schemaVersion': 'session.event.v1', 'type': 'provider_usage',
                        'sessionId': 'session-test', 'agentRunId': 'run-test', 'turnId': turn,
                        'payload': {'inputTokens': 100, 'outputTokens': 20,
                                    'promptCacheHitTokens': hit, 'promptCacheMissTokens': miss}}
                       for turn, hit, miss in [('first', 80, 20), ('second', 80, 20)]]
            records.append({**records[0], 'agentRunId': 'other-run', 'turnId': 'other'})
            (sessions / 'session-test.jsonl').write_text('\n'.join(map(json.dumps, records)), encoding='utf-8')
            result = collect_provider_usage(root, 'session-test', 'run-test')
            self.assertEqual(result['totals']['inputTokens'], 200)
            self.assertEqual(result['totals']['outputTokens'], 40)
            self.assertEqual(result['totals']['promptCacheHitRate'], 0.8)
            self.assertIsNone(result['totals']['totalTokens'])
            self.assertEqual(result['nRequests'], 2)
            child = {**records[0], 'sessionId': 'session-agent-test', 'agentRunId': 'subagent-test'}
            (sessions / 'session-agent-test.jsonl').write_text(json.dumps(child), encoding='utf-8')
            combined = collect_provider_usage(root, 'session-test', 'run-test')
            self.assertEqual(combined['nRequests'], 3)
            self.assertEqual(combined['totals']['inputTokens'], 300)
            self.assertEqual(combined['mainAgent']['totals']['inputTokens'], 200)
            self.assertEqual(combined['mainAgent']['nRequests'], 2)
            self.assertEqual(combined['subagents'][0]['sessionId'], 'session-agent-test')
            self.assertEqual(combined['subagents'][0]['totals']['inputTokens'], 100)
            self.assertEqual(combined['subagents'][0]['nRequests'], 1)
            records[1]['payload']['promptCacheHitTokens'] = None
            (sessions / 'session-test.jsonl').write_text('\n'.join(map(json.dumps, records)), encoding='utf-8')
            incomplete = collect_provider_usage(root, 'session-test', 'run-test')
            self.assertIsNone(incomplete['totals']['promptCacheHitRate'])
            self.assertIsNone(incomplete['mainAgent']['totals']['promptCacheHitRate'])
            self.assertEqual(incomplete['subagents'][0]['totals']['promptCacheHitRate'], 0.8)

    async def test_credential_mutation_is_separate_and_not_written_to_result(self):
        rpc = FakeRpc(["succeeded"])
        result = await run_trial(rpc, "task", "/app", "mock", "custom.test", None,
                                 30, api_key="fake-test-key")
        self.assertEqual(rpc.calls[0][1], {"modelProviderId": "custom.test", "modelApiKey": "fake-test-key"})
        self.assertEqual(rpc.calls[1][1], {"modelProviderId": "custom.test", "model": "mock"})
        self.assertNotIn("fake-test-key", json.dumps(result))

    def test_pilot_and_full_share_execution_configuration(self):
        directory = Path(__file__).parent
        pilot = json.loads((directory / "five-tasks.json").read_text())
        full = json.loads((directory / "full-tasks.json").read_text())
        pilot_tasks, full_tasks = pilot.pop("tasks"), full.pop("tasks")
        pilot.pop("job_name")
        full.pop("job_name")
        self.assertEqual(pilot, full)
        self.assertEqual((len(pilot_tasks), len(full_tasks)), (5, 89))
        self.assertTrue(all(not task.get("source") for task in full_tasks))
        self.assertTrue({task["path"] for task in pilot_tasks} <= {task["path"] for task in full_tasks})
        self.assertEqual(pilot["n_concurrent_trials"], 2)
        self.assertEqual(pilot["n_attempts"], 5)
        self.assertEqual(pilot["agents"], [{"import_path": "centaeris_agent:CentaerisAgent"}])
        self.assertTrue(pilot["environment"]["delete"])

    async def test_success_uses_runtime_loop_and_does_not_stop_verifier_services(self):
        rpc = FakeRpc(["running", "succeeded"])
        result = await run_trial(rpc, "Solve the task", "/app", "mock",
                                 "custom.test", "max", 30, poll_interval=0)
        self.assertEqual(result["status"], "succeeded")
        self.assertEqual(rpc.calls[0][1]["modelThinkingMode"], "max")
        prompt = next(r for m, r in rpc.calls if m == "session/prompt")
        self.assertEqual(prompt["message"], "Solve the task")
        self.assertNotIn("runtime/shutdown", [m for m, _ in rpc.calls])

    async def test_failed_run_is_not_reported_as_success(self):
        result = await run_trial(FakeRpc(["failed"]), "task", "/app", "mock",
                                 "custom.mock", "max", 30, poll_interval=0)
        self.assertEqual(result["status"], "failed")

    async def test_deadline_cancels_exact_run(self):
        rpc = FakeRpc(["running"])
        result = await run_trial(rpc, "task", "/app", "mock", "custom.mock", "max", 0,
                                 poll_interval=0)
        self.assertEqual(result["status"], "timedOut")
        cancel = next(r for m, r in rpc.calls if m.endswith("/cancel"))
        self.assertEqual(cancel["agentRunId"], "run-test")
        self.assertEqual(cancel["sessionId"], "conversation-test")

    async def test_unknown_status_fails_loudly(self):
        with self.assertRaisesRegex(ValueError, "unknown.*status"):
            await run_trial(FakeRpc(["success"]), "task", "/app", "mock", "custom.mock",
                            "max", 30, poll_interval=0)

    async def test_cleanup_preserves_primary_failure(self):
        class BrokenCancel(FakeRpc):
            async def call(self, method, request):
                if method.endswith("/cancel"):
                    raise ConnectionError("cancel connection failed")
                return await super().call(method, request)
        with self.assertRaises(BaseExceptionGroup) as caught:
            await run_trial(BrokenCancel(["invalid"]), "task", "/app", "mock",
                            "custom.mock", "max", 30, poll_interval=0)
        self.assertIsInstance(caught.exception.exceptions[0], ValueError)
        self.assertIsInstance(caught.exception.exceptions[1], ConnectionError)

    def test_attempts_have_separate_profiles(self):
        with tempfile.TemporaryDirectory() as root:
            first = trial_profile(Path(root), "first")
            second = trial_profile(Path(root), "second")
            self.assertNotEqual(first, second)
            with self.assertRaises(FileExistsError):
                trial_profile(Path(root), "first")


class RpcTests(unittest.IsolatedAsyncioTestCase):
    async def test_notifications_and_out_of_order_responses(self):
        async def peer(reader, writer):
            frames = [json.loads(await reader.readline()) for _ in range(2)]
            self.assertEqual(frames[0]["params"], {"request": {"value": 1}})
            writer.write(b'{"jsonrpc":"2.0","method":"event","params":{}}\n')
            for frame in reversed(frames):
                writer.write((json.dumps({"id": frame["id"], "result": frame["method"]}) + "\n").encode())
            await writer.drain()
            await reader.read()
            writer.close()
            await writer.wait_closed()
        async with await asyncio.start_server(peer, "127.0.0.1", 0) as server:
            reader, writer = await asyncio.open_connection(*server.sockets[0].getsockname())
            events = io.StringIO()
            rpc = Rpc(reader, writer, events)
            try:
                values = await asyncio.gather(rpc.call("first", {"value": 1}), rpc.call("second", {}))
                self.assertEqual(values, ["first", "second"])
                self.assertEqual(json.loads(events.getvalue())["method"], "event")
            finally:
                await rpc.close()

    async def test_closed_connection_rejects_pending_and_future_calls(self):
        async def peer(reader, writer):
            await reader.readline()
            writer.close()
            await writer.wait_closed()
        async with await asyncio.start_server(peer, "127.0.0.1", 0) as server:
            reader, writer = await asyncio.open_connection(*server.sockets[0].getsockname())
            rpc = Rpc(reader, writer, io.StringIO())
            try:
                with self.assertRaises(ConnectionError):
                    await rpc.call("first", {})
                with self.assertRaises(ConnectionError):
                    await rpc.call("second", {}, timeout=0.01)
            finally:
                await rpc.close()


if __name__ == "__main__":
    unittest.main()
