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
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from runner import Rpc, run_trial, trial_profile, collect_provider_usage, cancel, supervise, write_json
from unittest.mock import AsyncMock, Mock, patch
from types import SimpleNamespace


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
    def test_concurrent_evidence_writers_publish_one_complete_json_without_temporary_conflicts(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "provider-usage.json"
            values = [{"writer": "supervisor", "inputTokens": 100},
                      {"writer": "cancel", "inputTokens": 200}]
            barrier = threading.Barrier(2)
            first_replaced = threading.Event()
            arrival_lock = threading.Lock()
            arrived = []
            original_replace = os.replace

            def replace_after_both_writers_finished(source, destination):
                self.assertEqual(Path(source).parent, target.parent)
                with arrival_lock:
                    arrived.append(Path(source))
                    first = len(arrived) == 1
                barrier.wait(timeout=3)
                if first:
                    try:
                        return original_replace(source, destination)
                    finally:
                        first_replaced.set()
                self.assertTrue(first_replaced.wait(timeout=3))
                return original_replace(source, destination)

            with patch("runner.os.replace", replace_after_both_writers_finished), ThreadPoolExecutor(max_workers=2) as workers:
                pending = [workers.submit(write_json, target, value) for value in values]
                for future in pending:
                    future.result(timeout=5)
            self.assertIn(json.loads(target.read_text(encoding="utf-8")), values)
            self.assertEqual(list(target.parent.glob("*.tmp")), [])

    def write_committed_usage(self, profile):
        sessions = profile / "sessions/day"
        sessions.mkdir(parents=True)
        record = {"schemaVersion": "session.event.v1", "type": "provider_usage",
                  "sessionId": "session-test", "agentRunId": "run-test", "turnId": "turn-test",
                  "payload": {"inputTokens": 100, "outputTokens": 20,
                              "promptCacheHitTokens": 80, "promptCacheMissTokens": 20,
                              "authorization": "fake-do-not-export"}}
        (sessions / "session-test.jsonl").write_text(json.dumps(record), encoding="utf-8")
        (profile / "runtime-config.json").write_text('{"modelApiKey":"fake-do-not-read"}', encoding="utf-8")

    async def test_cancel_exports_committed_usage_before_shutdown_without_reading_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            self.write_committed_usage(profile)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime", "profilePath": str(profile)}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session-test", "agentRunId": "run-test"}))
            statuses = iter(["running", "cancelled"])
            methods = []

            async def call(method, request, timeout=30):
                methods.append(method)
                if method == "_centaeris/session/agent-runs":
                    return {"agentRuns": [{"agentRunId": "run-test", "status": next(statuses)}]}
                if method == "runtime/shutdown":
                    self.assertTrue((root / "provider-usage.json").exists())
                return {}

            original_read = Path.read_text
            def read_only_known_evidence(path, *args, **kwargs):
                self.assertIn(path, [root / "endpoint.json", root / "state.json",
                                    profile / "sessions/day/session-test.jsonl"])
                return original_read(path, *args, **kwargs)

            rpc = SimpleNamespace(call=AsyncMock(side_effect=call), close=AsyncMock())
            with patch("runner.connect", AsyncMock(return_value=rpc)), patch.object(Path, "read_text", read_only_known_evidence):
                await cancel(SimpleNamespace(logs=directory))
            artifact = json.loads((root / "provider-usage.json").read_text(encoding="utf-8"))
            self.assertEqual(artifact["providerUsage"]["nRequests"], 1)
            self.assertEqual(artifact["providerUsage"]["totals"]["inputTokens"], 100)
            self.assertIsNone(artifact["providerUsage"]["totals"]["totalTokens"])
            self.assertTrue(artifact["usageCoverage"]["terminalObserved"])
            self.assertTrue(artifact["usageCoverage"]["inFlightRequestMayBeMissing"])
            self.assertEqual(methods, ["_centaeris/session/agent-runs/cancel",
                                      "_centaeris/session/agent-runs", "_centaeris/session/agent-runs",
                                      "runtime/shutdown"])
            self.assertNotIn("fake-do-not", json.dumps(artifact))
            rpc.close.assert_awaited_once()

    async def test_cancel_poll_failure_still_exports_committed_usage_and_preserves_rpc_error(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            self.write_committed_usage(profile)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime", "profilePath": str(profile)}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session-test", "agentRunId": "run-test"}))
            failure = ConnectionError("terminal confirmation lost")

            async def call(method, request, timeout=30):
                if method == "_centaeris/session/agent-runs":
                    raise failure
                return {}

            rpc = SimpleNamespace(call=AsyncMock(side_effect=call), close=AsyncMock())
            with patch("runner.connect", AsyncMock(return_value=rpc)):
                with self.assertRaises(ConnectionError) as caught:
                    await cancel(SimpleNamespace(logs=directory))
            self.assertIs(caught.exception, failure)
            artifact = json.loads((root / "provider-usage.json").read_text(encoding="utf-8"))
            self.assertEqual(artifact["providerUsage"]["nRequests"], 1)
            self.assertFalse(artifact["usageCoverage"]["terminalObserved"])
            self.assertTrue(artifact["usageCoverage"]["inFlightRequestMayBeMissing"])
            rpc.close.assert_awaited_once()

    async def test_cancel_usage_export_failure_does_not_fail_an_accepted_cancellation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime", "profilePath": str(root / "profile")}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session-test", "agentRunId": "run-test"}))
            async def call(method, request, timeout=30):
                if method == "_centaeris/session/agent-runs":
                    return {"agentRuns": [{"agentRunId": "run-test", "status": "cancelled"}]}
                return {}
            rpc = SimpleNamespace(call=AsyncMock(side_effect=call), close=AsyncMock())
            with patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.collect_provider_usage", side_effect=ValueError("usage log truncated")):
                await cancel(SimpleNamespace(logs=directory))
            artifact = json.loads((root / "provider-usage.json").read_text(encoding="utf-8"))
            self.assertEqual(artifact["usageExportError"]["type"], "ValueError")
            self.assertNotIn("providerUsage", artifact)
            self.assertTrue(artifact["usageCoverage"]["terminalObserved"])
            self.assertEqual(rpc.call.await_args_list[-1].args[0], "runtime/shutdown")

    async def test_cancel_rpc_failure_is_not_replaced_by_usage_export_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime", "profilePath": str(root / "profile")}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session-test", "agentRunId": "run-test"}))
            failure = ConnectionError("cancel acknowledgement lost")
            rpc = SimpleNamespace(call=AsyncMock(side_effect=failure), close=AsyncMock())
            with patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.collect_provider_usage", side_effect=ValueError("usage log truncated")):
                with self.assertRaises(ConnectionError) as caught:
                    await cancel(SimpleNamespace(logs=directory))
            self.assertIs(caught.exception, failure)
            artifact = json.loads((root / "provider-usage.json").read_text(encoding="utf-8"))
            self.assertEqual(artifact["usageExportError"]["type"], "ValueError")
            self.assertFalse(artifact["usageCoverage"]["terminalObserved"])
            self.assertTrue(artifact["usageCoverage"]["inFlightRequestMayBeMissing"])
            rpc.close.assert_awaited_once()

    async def test_cancel_close_failure_keeps_the_primary_rpc_error(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime", "profilePath": str(root / "profile")}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session-test", "agentRunId": "run-test"}))
            failure = ConnectionError("cancel acknowledgement lost")
            rpc = SimpleNamespace(call=AsyncMock(side_effect=failure),
                                  close=AsyncMock(side_effect=TimeoutError("close timed out")))
            with patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.collect_provider_usage", return_value={}):
                with self.assertRaises(ConnectionError) as caught:
                    await cancel(SimpleNamespace(logs=directory))
            self.assertIs(caught.exception, failure)
            self.assertTrue(any("close timed out" in note for note in failure.__notes__))
            rpc.close.assert_awaited_once()

    async def test_cancel_close_failure_is_reported_after_successful_cancellation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime", "profilePath": str(root / "profile")}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session-test", "agentRunId": "run-test"}))
            async def call(method, request, timeout=30):
                if method == "_centaeris/session/agent-runs":
                    return {"agentRuns": [{"agentRunId": "run-test", "status": "cancelled"}]}
                return {}
            rpc = SimpleNamespace(call=AsyncMock(side_effect=call),
                                  close=AsyncMock(side_effect=TimeoutError("close timed out")))
            with patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.collect_provider_usage", return_value={}):
                with self.assertRaisesRegex(TimeoutError, "close timed out"):
                    await cancel(SimpleNamespace(logs=directory))
            rpc.close.assert_awaited_once()

    async def test_endpoint_without_profile_reports_unknown_usage_without_configuration_lookup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime"}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session-test", "agentRunId": "run-test"}))
            async def call(method, request, timeout=30):
                if method == "_centaeris/session/agent-runs":
                    return {"agentRuns": [{"agentRunId": "run-test", "status": "cancelled"}]}
                return {}
            rpc = SimpleNamespace(call=AsyncMock(side_effect=call), close=AsyncMock())
            with patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.collect_provider_usage") as collect:
                await cancel(SimpleNamespace(logs=directory))
            collect.assert_not_called()
            artifact = json.loads((root / "provider-usage.json").read_text(encoding="utf-8"))
            self.assertEqual(artifact["usageExportError"]["type"], "ValueError")
            self.assertNotIn("providerUsage", artifact)
            self.assertTrue(artifact["usageCoverage"]["inFlightRequestMayBeMissing"])

    async def test_supervisor_usage_export_failure_preserves_terminal_result_and_profile_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            profile.mkdir()
            instruction = root / "instruction.txt"
            instruction.write_text("task", encoding="utf-8")
            endpoint_process = SimpleNamespace(returncode=0, communicate=AsyncMock(return_value=(b'{"endpoint":"/runtime"}', b"")))
            runtime = SimpleNamespace(returncode=0)
            rpc = SimpleNamespace(call=AsyncMock(return_value={}), close=AsyncMock(),
                                  restore_event_log=lambda: {"mirrorErrorErrno": None, "mirrorRestored": False})
            args = SimpleNamespace(logs=str(root), attempt="attempt", context_tokens=500000,
                                   output_tokens=64000, runtime="runtime", instruction=str(instruction),
                                   cwd="/workspace", model="mock", provider="custom.test", effort="max",
                                   timeout=30, credential_env="TEST_MODEL_KEY", deadline_at_ms=None,
                                   keep_alive=False, linger=0)
            result = {"sessionId": "session-test", "agentRunId": "run-test", "status": "succeeded"}
            with patch("runner.trial_profile", return_value=profile), patch("runner.asyncio.create_subprocess_exec", AsyncMock(side_effect=[endpoint_process, runtime])), patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.run_trial", AsyncMock(return_value=result)), patch("runner.collect_provider_usage", side_effect=ValueError("usage log truncated")), patch.dict("runner.os.environ", {"TEST_MODEL_KEY": "fake-test-key"}):
                await supervise(args)
            endpoint = json.loads((root / "endpoint.json").read_text(encoding="utf-8"))
            saved = json.loads((root / "result.json").read_text(encoding="utf-8"))
            artifact = json.loads((root / "provider-usage.json").read_text(encoding="utf-8"))
            self.assertEqual(endpoint["profilePath"], str(profile))
            self.assertEqual(saved["status"], "succeeded")
            self.assertEqual(saved["usageExportError"]["type"], "ValueError")
            self.assertEqual(saved["usageCoverage"], artifact["usageCoverage"])
            self.assertFalse(saved["usageCoverage"]["inFlightRequestMayBeMissing"])
            self.assertNotIn("fake-test-key", json.dumps(saved))

    async def test_supervisor_cleanup_preserves_primary_failure_and_still_kills_the_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            profile.mkdir()
            instruction = root / "instruction.txt"
            instruction.write_text("task", encoding="utf-8")
            endpoint_process = SimpleNamespace(returncode=0, communicate=AsyncMock(return_value=(b'{"endpoint":"/runtime"}', b"")))
            runtime = SimpleNamespace(returncode=None, wait=AsyncMock(side_effect=[TimeoutError("still running"), 0]), kill=Mock())
            rpc = SimpleNamespace(call=AsyncMock(side_effect=ConnectionError("shutdown connection closed")),
                                  close=AsyncMock(side_effect=OSError("connection close failed")))
            args = SimpleNamespace(logs=str(root), attempt="attempt", context_tokens=500000,
                                   output_tokens=64000, runtime="runtime", instruction=str(instruction),
                                   cwd="/workspace", model="mock", provider="custom.test", effort="max",
                                   timeout=30, credential_env="TEST_MODEL_KEY", deadline_at_ms=None,
                                   keep_alive=False, linger=0)
            primary = ValueError("trial contract failure")
            with patch("runner.trial_profile", return_value=profile), patch("runner.asyncio.create_subprocess_exec", AsyncMock(side_effect=[endpoint_process, runtime])), patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.run_trial", AsyncMock(side_effect=primary)), patch.dict("runner.os.environ", {"TEST_MODEL_KEY": "fake-test-key"}):
                with self.assertRaises(ValueError) as caught:
                    await supervise(args)
            self.assertIs(caught.exception, primary)
            self.assertTrue(any("shutdown connection closed" in note for note in primary.__notes__))
            self.assertTrue(any("connection close failed" in note for note in primary.__notes__))
            rpc.close.assert_awaited_once()
            runtime.kill.assert_called_once()
            self.assertEqual(runtime.wait.await_count, 2)

    async def test_supervisor_reports_cleanup_failure_after_a_successful_run_and_reaps_runtime(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profile = root / "profile"
            profile.mkdir()
            instruction = root / "instruction.txt"
            instruction.write_text("task", encoding="utf-8")
            endpoint_process = SimpleNamespace(returncode=0, communicate=AsyncMock(return_value=(b'{"endpoint":"/runtime"}', b"")))
            runtime = SimpleNamespace(returncode=None, wait=AsyncMock(return_value=0), kill=Mock())
            cleanup = ConnectionError("shutdown connection closed")
            rpc = SimpleNamespace(call=AsyncMock(side_effect=cleanup), close=AsyncMock(),
                                  restore_event_log=lambda: {"mirrorErrorErrno": None, "mirrorRestored": False})
            args = SimpleNamespace(logs=str(root), attempt="attempt", context_tokens=500000,
                                   output_tokens=64000, runtime="runtime", instruction=str(instruction),
                                   cwd="/workspace", model="mock", provider="custom.test", effort="max",
                                   timeout=30, credential_env="TEST_MODEL_KEY", deadline_at_ms=None,
                                   keep_alive=False, linger=0)
            result = {"sessionId": "session-test", "agentRunId": "run-test", "status": "succeeded"}
            with patch("runner.trial_profile", return_value=profile), patch("runner.asyncio.create_subprocess_exec", AsyncMock(side_effect=[endpoint_process, runtime])), patch("runner.connect", AsyncMock(return_value=rpc)), patch("runner.run_trial", AsyncMock(return_value=result)), patch("runner.collect_provider_usage", return_value={}), patch.dict("runner.os.environ", {"TEST_MODEL_KEY": "fake-test-key"}):
                with self.assertRaises(ConnectionError) as caught:
                    await supervise(args)
            self.assertIs(caught.exception, cleanup)
            self.assertEqual(json.loads((root / "result.json").read_text(encoding="utf-8"))["status"], "succeeded")
            rpc.close.assert_awaited_once()
            runtime.wait.assert_awaited_once()
            runtime.kill.assert_not_called()

    async def test_expired_evaluation_deadline_does_not_admit_a_model_request(self):
        rpc = FakeRpc(["running"])
        with patch("runner.time.time", return_value=1000):
            with self.assertRaisesRegex(TimeoutError, "deadline expired before admission"):
                await run_trial(rpc, "task", "/app", "mock", "custom.test", None,
                                None, deadline_at_ms=999999)
        self.assertEqual(rpc.calls, [])

    async def test_explicit_task_deadline_overrides_standalone_relative_timeout(self):
        rpc = FakeRpc(["running", "succeeded"])
        with patch("runner.time.time", return_value=1000):
            result = await run_trial(rpc, "task", "/workspace", "mock", "custom.test", None,
                                     0, poll_interval=0, deadline_at_ms=1000000 + 7200 * 1000)
        self.assertEqual(result["status"], "succeeded")
        self.assertFalse(any(method.endswith("/cancel") for method, _ in rpc.calls))

    async def test_cancel_rpc_deadlines_fit_the_adapter_cleanup_window(self):
        async def call(method, request, timeout=30):
            if method == "_centaeris/session/agent-runs":
                return {"agentRuns": [{"agentRunId": "run", "status": "cancelled"}]}
            return {}
        rpc = SimpleNamespace(call=AsyncMock(side_effect=call), close=AsyncMock())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/runtime"}))
            (root / "state.json").write_text(json.dumps({"sessionId": "session", "agentRunId": "run"}))
            with patch("runner.connect", AsyncMock(return_value=rpc)):
                await cancel(SimpleNamespace(logs=directory))
        calls = rpc.call.await_args_list
        self.assertEqual(calls[0].args[1]["agentRunId"], "run")
        self.assertLessEqual(calls[0].kwargs.get("timeout", 30), 6)
        self.assertLessEqual(calls[1].kwargs.get("timeout", 30), 6)
        rpc.close.assert_awaited_once()

    async def test_external_deadline_has_no_internal_900_second_limit(self):
        rpc = FakeRpc(["running", "succeeded"])
        result = await run_trial(rpc, "task", "/app", "mock", "custom.test", None,
                                 None, poll_interval=0)
        self.assertEqual(result["status"], "succeeded")
        self.assertFalse(any(method.endswith("/cancel") for method, _ in rpc.calls))

    async def test_cancel_of_terminal_result_with_closed_runtime_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/gone"}))
            (root / "result.json").write_text(json.dumps({"status": "failed"}))
            with patch("runner.connect", AsyncMock(side_effect=TimeoutError("gone"))):
                await cancel(SimpleNamespace(logs=directory))

    async def test_cancel_does_not_hide_closed_runtime_without_terminal_result(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "endpoint.json").write_text(json.dumps({"endpoint": "/gone"}))
            with patch("runner.connect", AsyncMock(side_effect=TimeoutError("gone"))):
                with self.assertRaisesRegex(TimeoutError, "gone"):
                    await cancel(SimpleNamespace(logs=directory))

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

    async def test_event_disk_error_does_not_break_rpc_and_preserves_notification(self):
        class BrokenDisk(io.StringIO):
            def write(self, value):
                raise OSError(5, "Input/output error")
        async def peer(reader, writer):
            while line := await reader.readline():
                request = json.loads(line)
                writer.write((json.dumps({"method": "event", "params": {"sequence": request["id"]}}) + "\n").encode())
                writer.write((json.dumps({"id": request["id"], "result": request["method"]}) + "\n").encode())
                await writer.drain()
            writer.close()
            await writer.wait_closed()
        async with await asyncio.start_server(peer, "127.0.0.1", 0) as server:
            reader, writer = await asyncio.open_connection(*server.sockets[0].getsockname())
            rpc = Rpc(reader, writer, BrokenDisk())
            try:
                with patch("runner.sys.stderr", BrokenDisk()):
                    self.assertEqual(await rpc.call("first", {}), "first")
                    self.assertEqual(await rpc.call("second", {}), "second")
                self.assertIsNone(rpc.failure)
                self.assertEqual(rpc.event_log_error.errno, 5)
                rpc.event_spool.seek(0)
                self.assertEqual([json.loads(line)["params"]["sequence"] for line in rpc.event_spool], [1, 2])
                with tempfile.TemporaryDirectory() as directory:
                    artifact = Path(directory) / 'recovered.jsonl'
                    self.assertTrue(rpc.snapshot_event_spool(artifact))
                    self.assertEqual([json.loads(line)['params']['sequence']
                                      for line in artifact.read_text().splitlines()], [1, 2])
            finally:
                await rpc.close()

    async def test_partial_mirror_flush_is_rebuilt_without_duplicates(self):
        class FailedFlush(io.StringIO):
            fail = True
            def flush(self):
                if self.fail:
                    raise OSError(5, "Input/output error")
        reader = asyncio.StreamReader()
        writer = SimpleNamespace()
        events = FailedFlush()
        rpc = Rpc(reader, writer, events)
        try:
            rpc.record_event({"method": "one"})
            rpc.record_event({"method": "two"})
            events.fail = False
            health = rpc.restore_event_log()
            self.assertTrue(health["mirrorRestored"])
            self.assertEqual(health["mirrorErrorErrno"], 5)
            rpc.record_event({"method": "three"})
            self.assertEqual([json.loads(line)["method"] for line in events.getvalue().splitlines()], ["one", "two", "three"])
        finally:
            rpc.task.cancel()
            await asyncio.gather(rpc.task, return_exceptions=True)
            rpc.event_spool.close()

    async def test_both_event_sinks_failing_reports_loss_without_poisoning_rpc(self):
        class BrokenDisk(io.StringIO):
            def write(self, value):
                raise OSError(28, "No space left on device")
        rpc = Rpc(asyncio.StreamReader(), SimpleNamespace(), BrokenDisk(), event_spool=BrokenDisk())
        try:
            rpc.record_event({"method": "one"})
            self.assertIsNone(rpc.failure)
            health = rpc.restore_event_log()
            self.assertEqual(health["unspooledEvents"], 1)
            self.assertEqual(health["spoolErrorErrno"], 28)
            self.assertFalse(health["mirrorRestored"])
        finally:
            rpc.task.cancel()
            await asyncio.gather(rpc.task, return_exceptions=True)

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
