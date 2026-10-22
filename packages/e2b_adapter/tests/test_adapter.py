import base64
import io
import json
import os
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig, serve


class FileReader:
    def __init__(self, chunks):
        self.chunks = chunks
        self.closed = False

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.closed = True

    def __iter__(self):
        return iter(self.chunks)


class Handle:
    pid = 71

    def __init__(self, output=None, exit_code=0):
        self.output = output or [("ok", None, None)]
        self.exit_code = exit_code
        self.disconnected = False
        self.iterations = 0

    def __iter__(self):
        self.iterations += 1
        def events():
            for out, err, _ in self.output:
                yield (out.encode("utf-8") if isinstance(out, str) else out,
                       err.encode("utf-8") if isinstance(err, str) else err, None)
            yield None, None, SimpleNamespace(exit_code=self.exit_code, exited=True, error=None)
        return events()

    def disconnect(self):
        self.disconnected = True


class FakeSandbox:
    sandbox_id = "sandbox-a"

    def __init__(self):
        self.calls = []
        self.metadata = {"centaerisAttemptId": "attempt-a"}
        self.handle = Handle()
        self.reader = FileReader([b"\x00\xff", b"abc"])
        self.commands = SimpleNamespace(run=self.run, connect=self.command_connect, kill=self.command_kill)
        self.files = SimpleNamespace(read=self.read, write=self.write)

    def get_info(self):
        self.calls.append(("info", {}))
        return SimpleNamespace(sandbox_id=self.sandbox_id, template_id="template-a", metadata=self.metadata, state="running", envd_version="0.3.0")

    def run(self, command, **kwargs):
        self.calls.append(("run", {"command": command, **kwargs}))
        return self.handle

    def command_connect(self, pid, **kwargs):
        self.calls.append(("commandConnect", {"pid": pid, **kwargs}))
        return self.handle

    def command_kill(self, pid, **kwargs):
        self.calls.append(("commandKill", {"pid": pid, **kwargs}))
        return True

    def read(self, path, **kwargs):
        self.calls.append(("read", {"path": path, **kwargs}))
        return self.reader

    def write(self, path, data, **kwargs):
        self.calls.append(("write", {"path": path, "data": data, **kwargs}))


class FakeClient:
    def __init__(self):
        self.sandbox = FakeSandbox()
        self.calls = []
        self.failure = None
        self.Sandbox = self

    def create(self, **kwargs):
        self.calls.append(("create", kwargs))
        if self.failure:
            raise self.failure
        self.sandbox.metadata = kwargs["metadata"]
        return self.sandbox

    def connect(self, sandbox_id, **kwargs):
        self.calls.append(("connect", {"sandboxId": sandbox_id, **kwargs}))
        return self.sandbox

    def get_info(self, sandbox_id):
        self.calls.append(("inspect", {"sandboxId": sandbox_id}))
        if self.failure:
            raise self.failure
        return self.sandbox.get_info()

    def kill(self, sandbox_id):
        self.calls.append(("kill", {"sandboxId": sandbox_id}))
        if self.failure:
            raise self.failure
        return False

    def pause(self, sandbox_id, **kwargs):
        self.calls.append(("pause", {"sandboxId": sandbox_id, **kwargs}))
        return False


def request(operation, payload=None, **extra):
    return {"schema": "runtime.e2b.adapter.request.v1", "requestId": "request-a", "operation": operation, "payload": payload or {}, **extra}


def create_payload(**extra):
    return {"attemptId": "attempt-a", "idempotencyKey": "key-a", "templateRef": {"templateId": "template-a", "buildId": "123e4567-e89b-12d3-a456-426614174000"}, "metadata": {"workspaceId": "workspace-a"}, "timeoutSeconds": 300, "allowInternetAccess": False, "onTimeout": "kill", **extra}


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.client = FakeClient()
        self.adapter = Adapter(AdapterConfig(api_key="do-not-expose", domain="self.example", idempotency_supported=True), client=self.client,
                               command_stream_factory=lambda sandbox, version, payload: sandbox.run(payload["command"], background=True,
                                   timeout=payload["timeoutSeconds"], **{key: payload[key] for key in ("cwd", "envs", "user") if key in payload}))

    def connect(self):
        response = self.adapter.handle(request("connect", {"sandboxId": "sandbox-a", "timeoutSeconds": 300, "onResume": "restore"}))
        self.assertEqual(response["outcome"], "succeeded")

    def start(self, pid=71):
        self.client.sandbox.handle.pid = pid
        response = self.adapter.handle(request("runCommand", {"sandboxId": "sandbox-a", "command": "echo ok", "timeoutSeconds": 10}))
        self.assertEqual(response["outcome"], "succeeded", response)
        return self.client.sandbox.handle

    def wait(self, pid=71, **extra):
        return self.adapter.handle(request("waitCommand", {"sandboxId": "sandbox-a", "pid": pid, "timeoutSeconds": 10, **extra}))

    def test_explicit_self_hosted_urls_are_bound_to_the_sdk_without_a_domain(self):
        with patch("e2b.E2B", return_value=self.client) as factory:
            adapter = Adapter(AdapterConfig(api_key="fixture-key", api_url="http://localhost:3000",
                                            sandbox_url="http://localhost:3002"))
        self.assertIs(adapter.client, self.client)
        self.assertEqual(factory.call_args.kwargs["api_url"], "http://localhost:3000")
        self.assertEqual(factory.call_args.kwargs["sandbox_url"], "http://localhost:3002")
        self.assertIsNone(factory.call_args.kwargs["domain"])

    def test_partial_self_hosted_url_configuration_fails_before_sdk_construction(self):
        with patch("e2b.E2B") as factory:
            for urls in ({"api_url": "http://localhost:3000"}, {"sandbox_url": "http://localhost:3002"}):
                with self.subTest(urls=urls), self.assertRaises(ValueError):
                    Adapter(AdapterConfig(api_key="fixture-key", domain="domain.example", **urls))
            factory.assert_not_called()

    def test_sandbox_endpoint_rejects_credentials_and_unencrypted_non_loopback(self):
        for endpoint in ("http://remote.example:3002", "https://user:private-probe@host.example",
                         "https://host.example?secret=private-probe", "https://host.example#private-probe"):
            with self.subTest(endpoint=endpoint), self.assertRaises(ValueError) as error:
                AdapterConfig(api_key="fixture-key", domain="domain.example", api_url="http://localhost:3000", sandbox_url=endpoint)
            self.assertNotIn("private-probe", str(error.exception))

    def test_public_domain_cannot_inherit_unselected_ambient_sdk_endpoints(self):
        with patch.dict(os.environ, {"E2B_API_URL": "http://localhost:3000", "E2B_SANDBOX_URL": "http://localhost:3002"}), \
                patch("e2b.E2B") as factory:
            with self.assertRaises(ValueError):
                Adapter(AdapterConfig(api_key="fixture-key", domain="public.example"))
            factory.assert_not_called()

    def test_public_domain_configuration_still_uses_the_explicit_domain(self):
        with patch.dict(os.environ, {"E2B_API_URL": "", "E2B_SANDBOX_URL": ""}), \
                patch("e2b.E2B", return_value=self.client) as factory:
            Adapter(AdapterConfig(api_key="fixture-key", domain="public.example"))
        self.assertEqual(factory.call_args.kwargs["domain"], "public.example")
        self.assertIsNone(factory.call_args.kwargs["api_url"])
        self.assertIsNone(factory.call_args.kwargs["sandbox_url"])

    def test_cli_selects_the_exact_embed_endpoint_pair_without_a_domain(self):
        from centaeris_e2b_adapter.__main__ import main
        with patch.dict(os.environ, {"E2B_API_KEY": "fixture-key", "E2B_API_URL": "http://localhost:3000",
                                     "E2B_SANDBOX_URL": "http://localhost:3002"}, clear=True), \
                patch("sys.argv", ["adapter", "--lane", "control"]), \
                patch("sys.stdin", SimpleNamespace(buffer=io.BytesIO())), \
                patch("sys.stdout", SimpleNamespace(buffer=io.BytesIO())), \
                patch("centaeris_e2b_adapter.__main__.Adapter", return_value=self.adapter) as factory, \
                patch("centaeris_e2b_adapter.__main__.serve"):
            status = main()
        self.assertEqual(status, 0)
        selected = factory.call_args.args[0]
        self.assertEqual(selected.api_url, "http://localhost:3000")
        self.assertEqual(selected.sandbox_url, "http://localhost:3002")
        self.assertIsNone(selected.domain)
        self.assertEqual(selected.lane, "control")

    def test_create_uses_persisted_identity_and_immutable_template(self):
        response = self.adapter.handle(request("create", create_payload()))
        self.assertEqual(response["outcome"], "succeeded")
        call = self.client.calls[0]
        self.assertEqual(call[0], "create")
        self.assertEqual(call[1]["api_headers"], {"Idempotency-Key": "key-a"})
        self.assertEqual(call[1]["retries"], 0)
        self.assertEqual(call[1]["template"], "template-a:123e4567-e89b-12d3-a456-426614174000")
        self.assertEqual(call[1]["metadata"]["centaerisAttemptId"], "attempt-a")
        self.assertEqual(call[1]["lifecycle"], {"on_timeout": "kill", "auto_resume": False})
        self.assertFalse(call[1]["allow_internet_access"])
        self.assertEqual(response["result"]["sandboxId"], "sandbox-a")

    def test_create_without_configured_idempotency_preserves_one_explicit_dispatch(self):
        adapter = Adapter(AdapterConfig(api_key="key", domain="self.example", idempotency_supported=False), client=self.client)
        response = adapter.handle(request("create", create_payload()))
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual([call[0] for call in self.client.calls], ["create"])
        self.assertEqual(self.client.calls[0][1]["api_headers"], {"Idempotency-Key": "key-a"})
        self.assertEqual(self.client.calls[0][1]["retries"], 0)
        self.assertFalse(adapter.handle(request("capabilities"))["result"]["createIdempotencyConfigured"])

    def test_provider_key_grammar_does_not_narrow_attempt_or_sandbox_identity(self):
        self.client.sandbox.sandbox_id = "Sandbox_A:42"
        response = self.adapter.handle(request("create", create_payload(attemptId="Attempt_A:42", idempotencyKey="key-42")))
        self.assertEqual(response["outcome"], "succeeded", response)
        self.assertEqual(response["result"]["attemptId"], "Attempt_A:42")
        inspected = self.adapter.handle(request("inspect", {"sandboxId": "Sandbox_A:42"}))
        self.assertEqual(inspected["outcome"], "succeeded", inspected)
        self.assertEqual(inspected["result"]["sandboxId"], "Sandbox_A:42")

    def test_create_rejects_mutable_builds_and_noncanonical_template_identity(self):
        refs = [{"templateId": "template-a", "buildId": "latest"},
                {"templateId": "template-a", "buildId": "123E4567-E89B-12D3-A456-426614174000"},
                {"templateId": "template-a", "buildId": "123e4567e89b12d3a456426614174000"},
                {"templateId": "_template", "buildId": "123e4567-e89b-12d3-a456-426614174000"},
                {"templateId": "template/a", "buildId": "123e4567-e89b-12d3-a456-426614174000"}]
        for ref in refs:
            with self.subTest(ref=ref):
                self.assertEqual(self.adapter.handle(request("create", create_payload(templateRef=ref)))["outcome"], "notDispatched")
        self.assertEqual(self.client.calls, [])

    def test_create_timeout_unknown_never_retried_and_error_redacted(self):
        self.client.failure = TimeoutError("do-not-expose")
        response = self.adapter.handle(request("create", create_payload()))
        self.assertEqual(response["outcome"], "unknown")
        self.assertEqual(len(self.client.calls), 1)
        self.assertNotIn("do-not-expose", json.dumps(response))

    def test_create_inspection_failure_retains_known_id(self):
        self.client.sandbox.get_info = lambda: (_ for _ in ()).throw(TimeoutError())
        response = self.adapter.handle(request("create", create_payload()))
        self.assertEqual(response["outcome"], "unknown")
        self.assertEqual(response["result"]["sandboxId"], "sandbox-a")
        self.assertEqual(response["result"]["attemptId"], "attempt-a")

    def test_provider_info_identity_mismatch_never_rebinds_the_requested_sandbox(self):
        self.client.sandbox.get_info = lambda: SimpleNamespace(
            sandbox_id="another-sandbox", template_id="template-a", metadata={}, state="running")
        values = [request("create", create_payload()),
                  request("inspect", {"sandboxId": "sandbox-a"}),
                  request("connect", {"sandboxId": "sandbox-a", "timeoutSeconds": 300, "onResume": "restore"})]
        for value in values:
            with self.subTest(operation=value["operation"]):
                response = self.adapter.handle(value)
                self.assertEqual(response["outcome"], "unknown")
                self.assertEqual(response["error"]["code"], "identityMismatch")
                self.assertEqual(response["result"]["sandboxId"], "sandbox-a")

    def test_unknown_keys_and_schema_fail_before_sdk(self):
        old = request("inspect", {"sandboxId": "sandbox-a"})
        old["schema"] = "old"
        for value in [request("inspect", {"sandboxId": "sandbox-a", "unknown": True}), request("inspect", {"sandboxId": "sandbox-a"}, extra=True), old]:
            with self.subTest(value=value):
                self.assertEqual(self.adapter.handle(value)["outcome"], "notDispatched")
        self.assertEqual(self.client.calls, [])

    def test_inspect_failed_lookup_not_refund_proof(self):
        self.client.failure = LookupError("missing")
        response = self.adapter.handle(request("inspect", {"sandboxId": "sandbox-a"}))
        self.assertEqual(response["outcome"], "unknown")
        self.assertNotIn("removed", response.get("result", {}))

    def test_kill_exact_id_without_connect_or_resume(self):
        adapter = Adapter(AdapterConfig(api_key="key", domain="self.example", lane="cleanup"), client=self.client)
        response = adapter.handle(request("kill", {"sandboxId": "sandbox-a"}))
        self.assertEqual(response["outcome"], "succeeded")
        self.assertEqual(self.client.calls, [("kill", {"sandboxId": "sandbox-a"})])
        self.assertEqual(response["result"], {"sandboxId": "sandbox-a", "deleteAcknowledged": True, "providerReportedKilled": False})

    def test_pause_reports_request_without_claiming_continuity(self):
        adapter = Adapter(AdapterConfig(api_key="key", domain="self.example", lane="cleanup"), client=self.client)
        response = adapter.handle(request("pause", {"sandboxId": "sandbox-a", "mode": "filesystem"}))
        self.assertEqual(response["result"], {"sandboxId": "sandbox-a", "requestedMode": "filesystem", "changed": False})
        self.assertEqual(self.client.calls[0][0], "pause")

    def test_commands_require_explicit_connect(self):
        response = self.adapter.handle(request("runCommand", {"sandboxId": "sandbox-a", "command": "echo ok", "timeoutSeconds": 10}))
        self.assertEqual(response["outcome"], "notDispatched")
        self.assertEqual(self.client.calls, [])

    def test_run_returns_pid_without_wait(self):
        self.connect()
        response = self.adapter.handle(request("runCommand", {"sandboxId": "sandbox-a", "command": "echo ok", "timeoutSeconds": 10, "cwd": "/workspace", "envs": {"LANG": "C"}}))
        self.assertEqual(response["result"], {"sandboxId": "sandbox-a", "pid": 71})
        self.assertTrue(self.client.sandbox.calls[-1][1]["background"])
        self.assertFalse(self.client.sandbox.handle.disconnected)

    def test_recover_known_pid_never_reexecutes(self):
        self.connect()
        self.start()
        response = self.adapter.handle(request("recoverCommand", {"sandboxId": "sandbox-a", "pid": 71, "timeoutSeconds": 10}))
        self.assertEqual(response["result"]["stdout"], "ok")
        self.assertEqual(response["result"]["exitCode"], 0)
        self.assertEqual(sum(call[0] == "run" for call in self.client.sandbox.calls), 1)
        self.assertFalse(any(call[0] == "commandConnect" for call in self.client.sandbox.calls))

    def test_cold_recovery_retains_pid_without_claiming_output_replay(self):
        self.connect()
        response = self.adapter.handle(request("recoverCommand", {"sandboxId": "sandbox-a", "pid": 71, "timeoutSeconds": 10}))
        self.assertEqual(response["outcome"], "unknown")
        self.assertEqual(response["result"]["pid"], 71)
        self.assertEqual(response["error"]["code"], "commandOutputUnavailable")
        self.assertFalse(any(call[0] in ("commandConnect", "run") for call in self.client.sandbox.calls))

    def test_output_limit_disconnects_unsettled_process(self):
        self.connect()
        self.client.sandbox.handle = Handle([("12345", None, None)])
        self.start()
        response = self.wait(maxOutputBytes=4)
        self.assertEqual(response["outcome"], "unknown")
        self.assertEqual(response["error"]["code"], "outputLimitExceeded")
        self.assertEqual(response["result"]["pid"], 71)
        self.assertTrue(self.client.sandbox.handle.disconnected)

    def test_completed_output_replays_without_consuming_or_restarting_the_command(self):
        self.connect()
        handle = self.start()
        completed = self.wait()
        replay = self.adapter.handle(request("recoverCommand", {"sandboxId": "sandbox-a", "pid": 71, "timeoutSeconds": 10}))
        self.assertEqual(completed["outcome"], "succeeded", completed)
        self.assertEqual(replay["result"], completed["result"])
        self.assertEqual(handle.iterations, 1)
        self.assertTrue(handle.disconnected)
        self.assertEqual(sum(call[0] == "run" for call in self.client.sandbox.calls), 1)

    def test_insufficient_wait_budget_keeps_the_original_stream_waitable(self):
        self.connect()
        with patch("centaeris_e2b_adapter.bridge.monotonic", return_value=1000):
            handle = self.start()
            rejected = self.adapter.handle(request("waitCommand", {"sandboxId": "sandbox-a", "pid": 71, "timeoutSeconds": 1}))
            self.assertEqual(rejected["outcome"], "notDispatched")
            self.assertEqual(rejected["error"]["code"], "waitBudgetInsufficient")
            self.assertEqual(rejected["result"]["pid"], 71)
            self.assertEqual(handle.iterations, 0)
            self.assertFalse(handle.disconnected)
            completed = self.wait()
        self.assertEqual(completed["result"]["stdout"], "ok")

    def test_cached_completion_respects_a_smaller_requested_output_budget(self):
        self.connect()
        self.start()
        self.assertEqual(self.wait()["outcome"], "succeeded")
        rejected = self.wait(maxOutputBytes=1)
        self.assertEqual(rejected["outcome"], "unknown")
        self.assertEqual(rejected["error"]["code"], "outputLimitExceeded")
        self.assertEqual(self.wait(maxOutputBytes=2)["result"]["stdout"], "ok")

    def test_live_command_capacity_is_checked_before_another_start(self):
        self.connect()
        with patch("centaeris_e2b_adapter.bridge.MAX_COMMANDS", 2):
            first = self.start(71)
            self.client.sandbox.handle = Handle()
            second = self.start(72)
            before = len(self.client.sandbox.calls)
            response = self.adapter.handle(request("runCommand", {"sandboxId": "sandbox-a", "command": "third", "timeoutSeconds": 10}))
        self.assertEqual(response["outcome"], "notDispatched")
        self.assertEqual(response["error"]["code"], "commandCapacityUnavailable")
        self.assertEqual(len(self.client.sandbox.calls), before)
        self.assertFalse(first.disconnected)
        self.assertFalse(second.disconnected)

    def test_conflicting_process_identity_preserves_the_existing_pending_stream(self):
        self.connect()
        original = self.start()
        replacement = Handle()
        self.client.sandbox.handle = replacement
        response = self.adapter.handle(request("runCommand", {"sandboxId": "sandbox-a", "command": "conflict", "timeoutSeconds": 10}))
        self.assertEqual(response["outcome"], "unknown")
        self.assertEqual(response["error"]["code"], "commandIdentityConflict")
        self.assertEqual(response["result"]["pid"], 71)
        self.assertFalse(original.disconnected)
        self.assertTrue(replacement.disconnected)
        self.assertEqual(self.wait()["result"]["stdout"], "ok")

    def test_only_completed_evidence_can_be_evicted_for_new_command_capacity(self):
        self.connect()
        with patch("centaeris_e2b_adapter.bridge.MAX_COMMANDS", 2):
            self.start(71)
            self.wait(71)
            self.client.sandbox.handle = Handle([("second", None, None)])
            live = self.start(72)
            self.client.sandbox.handle = Handle([("third", None, None)])
            self.start(73)
            old = self.wait(71)
            current = self.wait(72)
        self.assertEqual(old["outcome"], "unknown")
        self.assertEqual(old["error"]["code"], "commandOutputUnavailable")
        self.assertEqual(current["result"]["stdout"], "second")
        self.assertTrue(live.disconnected)

    def test_reconnect_does_not_drop_the_original_pending_output_stream(self):
        self.connect()
        handle = self.start()
        self.connect()
        completed = self.wait()
        self.assertEqual(completed["result"]["stdout"], "ok")
        self.assertEqual(handle.iterations, 1)
        self.assertTrue(handle.disconnected)

    def test_connection_cache_eviction_requires_explicit_reconnect_without_losing_command_output(self):
        with patch("centaeris_e2b_adapter.bridge.MAX_CONNECTED_SANDBOXES", 1):
            self.connect()
            original = self.client.sandbox
            self.start()
            self.client.sandbox = FakeSandbox()
            self.client.sandbox.sandbox_id = "sandbox-b"
            connected = self.adapter.handle(request("connect", {"sandboxId": "sandbox-b", "timeoutSeconds": 300, "onResume": "restore"}))
            self.assertEqual(connected["outcome"], "succeeded")
            rejected = self.wait()
            self.assertEqual(rejected["outcome"], "notDispatched")
            self.assertEqual(rejected["error"]["code"], "connectionRequired")
            self.client.sandbox = original
            self.connect()
            completed = self.wait()
        self.assertEqual(completed["result"]["stdout"], "ok")

    def test_stdout_and_stderr_share_one_output_budget(self):
        self.connect()
        self.client.sandbox.handle = Handle([("ab", None, None), (None, "cd", None)])
        handle = self.start()
        response = self.wait(maxOutputBytes=3)
        self.assertEqual(response["outcome"], "unknown")
        self.assertEqual(response["error"]["code"], "outputLimitExceeded")
        self.assertTrue(handle.disconnected)
        self.assertEqual(self.wait()["error"]["code"], "commandOutputUnavailable")

    def test_broken_original_stream_is_closed_and_never_replaced_by_a_fresh_stream(self):
        def broken_stream():
            yield "partial", None, None
            raise ConnectionError("fixture-secret")
        self.connect()
        self.client.sandbox.handle = Handle(broken_stream())
        handle = self.start()
        response = self.wait()
        self.assertEqual(response["outcome"], "unknown")
        self.assertEqual(response["result"]["pid"], 71)
        self.assertTrue(handle.disconnected)
        self.assertFalse(any(call[0] == "commandConnect" for call in self.client.sandbox.calls))
        self.assertEqual(self.wait()["error"]["code"], "commandOutputUnavailable")

    def test_cancel_reports_signal_not_tree_cleanup(self):
        self.connect()
        response = self.adapter.handle(request("cancelCommand", {"sandboxId": "sandbox-a", "pid": 71}))
        self.assertEqual(response["result"], {"sandboxId": "sandbox-a", "pid": 71, "signalSent": True})

    def test_binary_file_read_and_write_exact_bytes(self):
        self.connect()
        response = self.adapter.handle(request("readFile", {"sandboxId": "sandbox-a", "path": "/workspace/blob"}))
        self.assertEqual(base64.b64decode(response["result"]["dataBase64"]), b"\x00\xffabc")
        write = self.adapter.handle(request("writeFile", {"sandboxId": "sandbox-a", "path": "/workspace/blob", "dataBase64": response["result"]["dataBase64"]}))
        self.assertEqual(write["outcome"], "succeeded")
        self.assertEqual(self.client.sandbox.calls[-1][1]["data"], b"\x00\xffabc")
        self.assertTrue(self.client.sandbox.reader.closed)

    def test_file_limit_closes_without_partial_success(self):
        self.connect()
        response = self.adapter.handle(request("readFile", {"sandboxId": "sandbox-a", "path": "/workspace/blob", "maxBytes": 4}))
        self.assertEqual(response["outcome"], "unknown")
        self.assertTrue(self.client.sandbox.reader.closed)

    def test_malformed_base64_rejected_before_write(self):
        self.connect()
        before = list(self.client.sandbox.calls)
        response = self.adapter.handle(request("writeFile", {"sandboxId": "sandbox-a", "path": "/workspace/blob", "dataBase64": "!bad!"}))
        self.assertEqual(response["outcome"], "notDispatched")
        self.assertEqual(before, self.client.sandbox.calls)

    def test_capabilities_expose_unsupported_guarantees(self):
        result = self.adapter.handle(request("capabilities"))["result"]
        self.assertTrue(result["createIdempotencyConfigured"])
        self.assertTrue(result["commandOutputBytes"])
        self.assertFalse(result["commandStartIdempotency"])
        self.assertFalse(result["runProcessTreeCancellation"])
        self.assertFalse(result["storageQuota"])

    def test_control_cannot_create_execute_write_or_resume(self):
        adapter = Adapter(AdapterConfig(api_key="key", domain="self.example", lane="control", idempotency_supported=True), client=self.client)
        values = [request("create", create_payload()),
                  request("connect", {"sandboxId": "sandbox-a", "timeoutSeconds": 300, "onResume": "restore"}),
                  request("runCommand", {"sandboxId": "sandbox-a", "command": "echo ok", "timeoutSeconds": 10}),
                  request("writeFile", {"sandboxId": "sandbox-a", "path": "/data/a", "dataBase64": "YQ=="})]
        for value in values:
            with self.subTest(operation=value["operation"]):
                self.assertEqual(adapter.handle(value)["error"]["code"], "laneForbidden")
        self.assertEqual(self.client.calls, [])

    def test_fresh_control_lane_explicitly_rejects_unavailable_pid_cancellation(self):
        adapter = Adapter(AdapterConfig(api_key="key", domain="self.example", lane="control"), client=self.client)
        self.assertFalse(adapter.handle(request("capabilities"))["result"]["controlCommandCancellation"])
        response = adapter.handle(request("cancelCommand", {"sandboxId": "sandbox-a", "pid": 71}))
        self.assertEqual(response["outcome"], "notDispatched")
        self.assertEqual(response["error"]["code"], "laneForbidden")
        self.assertEqual(self.client.calls, [])

    def test_cleanup_cannot_create_or_execute(self):
        adapter = Adapter(AdapterConfig(api_key="key", domain="self.example", lane="cleanup", idempotency_supported=True), client=self.client)
        self.assertEqual(adapter.handle(request("create", create_payload()))["error"]["code"], "laneForbidden")
        self.assertEqual(self.client.calls, [])

    def test_empty_file_can_be_written(self):
        self.connect()
        response = self.adapter.handle(request("writeFile", {"sandboxId": "sandbox-a", "path": "/workspace/empty", "dataBase64": ""}))
        self.assertEqual(response["outcome"], "succeeded")
        self.assertEqual(self.client.sandbox.calls[-1][1]["data"], b"")

    def test_worker_survives_invalid_json(self):
        source = io.BytesIO(b"bad json\n" + json.dumps(request("capabilities")).encode() + b"\n")
        output = io.BytesIO()
        serve(self.adapter, source, output)
        responses = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(responses[0]["outcome"], "notDispatched")
        self.assertEqual(responses[1]["outcome"], "succeeded")
        self.assertEqual(responses[1]["requestId"], "request-a")

    def test_raw_duplicate_fields_are_rejected_before_dispatch(self):
        requests = [b'{"schema":"runtime.e2b.adapter.request.v1","requestId":"one","requestId":"two","operation":"inspect","payload":{"sandboxId":"sandbox-a"}}\n',
                    b'{"schema":"runtime.e2b.adapter.request.v1","requestId":"one","operation":"kill","operation":"inspect","payload":{"sandboxId":"sandbox-a"}}\n',
                    b'{"schema":"runtime.e2b.adapter.request.v1","requestId":"one","operation":"inspect","payload":{"sandboxId":"sandbox-a","sandboxId":"sandbox-b"}}\n']
        for value in requests:
            with self.subTest(value=value):
                output = io.BytesIO()
                serve(self.adapter, io.BytesIO(value), output)
                self.assertEqual(json.loads(output.getvalue())["outcome"], "notDispatched")
        self.assertEqual(self.client.calls, [])

    def test_oversized_frame_does_not_split_into_new_requests(self):
        source = io.BytesIO(b"x" * 1025 + b"\n" + json.dumps(request("capabilities")).encode() + b"\n")
        output = io.BytesIO()
        serve(self.adapter, source, output, max_frame_bytes=1024)
        responses = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(len(responses), 2)
        self.assertEqual(responses[0]["error"]["code"], "frameTooLarge")
        self.assertEqual(responses[1]["outcome"], "succeeded")


if __name__ == "__main__":
    unittest.main()
