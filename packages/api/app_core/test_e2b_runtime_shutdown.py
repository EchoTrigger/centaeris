"""Service-free shutdown contracts for the opt-in product fixture."""
from functools import lru_cache
import importlib.util
import io
import queue
import threading
from collections import deque
import os
import subprocess
from types import SimpleNamespace
from unittest.mock import Mock, patch

from django.test import SimpleTestCase
from . import test_e2b_consumption_production as product


@lru_cache(maxsize=1)
def live_fixture_type():
    # Expose its methods without discovering/running a physical acceptance test.
    spec = importlib.util.spec_from_file_location("app_core.shutdown_contract_fixture", product.__file__)
    module = importlib.util.module_from_spec(spec)
    with patch.dict(os.environ, {"CENTAERIS_E2B_PRODUCT_ACCEPTANCE": "1"}):
        spec.loader.exec_module(module)
    return module.E2BConsumptionProductionTests


class RuntimeShutdownTests(SimpleTestCase):
    def process(self, *, timeout=False, exit_code=0):
        calls = []
        process = Mock(returncode=None)
        process.poll.side_effect = lambda: process.returncode
        process.stdin.write.side_effect = lambda text: calls.append(("write", text))
        process.stdin.flush.side_effect = lambda: calls.append(("flush",))
        process.terminate.side_effect = lambda: calls.append(("terminate",))
        process.stdin.close.side_effect = lambda: calls.append(("close_stdin",))
        process.stdout.close.side_effect = lambda: calls.append(("close_stdout",))
        def wait(timeout):
            calls.append(("wait", timeout))
            if len([call for call in calls if call[0] == "wait"]) == 1 and should_timeout:
                raise subprocess.TimeoutExpired("owned-test-runtime", timeout)
            process.returncode = exit_code
            return exit_code
        should_timeout = timeout
        process.wait.side_effect = wait
        return process, calls

    def fixture(self, process):
        return SimpleNamespace(runtime_process=process, runtime_reader=Mock(),
            workspace_observations=None, assertEqual=self.assertEqual)

    def test_stop_is_flushed_before_wait_and_clean_exit_never_terminates(self):
        process, calls = self.process()
        fixture = self.fixture(process)
        live_fixture_type().stop_runtime(fixture)
        self.assertEqual(calls[:2], [("write", "stop\n"), ("flush",)])
        self.assertEqual(calls[2][0], "wait")
        process.terminate.assert_not_called()
        self.assertEqual(calls[-2:], [("close_stdin",), ("close_stdout",)])
        fixture.runtime_reader.join.assert_called_once_with(timeout=5)

    def test_timeout_terminates_only_after_wait_and_is_not_reported_as_success(self):
        process, calls = self.process(timeout=True, exit_code=-15)
        with self.assertRaises(subprocess.TimeoutExpired):
            live_fixture_type().stop_runtime(self.fixture(process))
        self.assertEqual([call[0] for call in calls],
            ["write", "flush", "wait", "terminate", "wait", "close_stdin", "close_stdout"])
        self.assertEqual(calls[4], ("wait", 5))

    def test_stop_wait_covers_runtime_drain_budget_and_uses_frozen_startup_value(self):
        for timeout in (None, 100):
            with self.subTest(timeout=timeout):
                process, calls = self.process()
                fixture = self.fixture(process)
                if timeout is not None:
                    fixture.runtime_shutdown_wait_seconds = timeout
                with patch.dict(os.environ, {"RUNTIME_E2B_REQUEST_TIMEOUT_SECONDS": "1"}):
                    live_fixture_type().stop_runtime(fixture)
                self.assertEqual(calls[2], ("wait", 60 if timeout is None else timeout))

    def test_start_freezes_the_spawned_timeout_for_later_shutdown(self):
        process, calls = self.process()
        process.stdout = io.StringIO(product.READY + "http://127.0.0.1:19001\n")
        fixture = self.fixture(process)
        fixture.argv = ["owned-prebuilt-runtime"]
        fixture.live_server_url = "http://127.0.0.1:19002"
        fixture.runtime_lines = deque()
        fixture.runtime_markers = queue.Queue()
        fixture.runtime_output_drained = threading.Event()
        fixture.addCleanup = Mock()
        fixture.assertTrue, fixture.fail = self.assertTrue, self.fail
        database = {"NAME": "test_owned", "USER": "test", "PASSWORD": "test",
                    "HOST": "127.0.0.1", "PORT": "55432"}
        fixture_type = live_fixture_type()
        fixture.stop_runtime = lambda: fixture_type.stop_runtime(fixture)
        with patch.dict(fixture_type.start_runtime.__globals__, {
                "connection": SimpleNamespace(settings_dict=database),
                "subprocess": SimpleNamespace(Popen=Mock(return_value=process), PIPE=subprocess.PIPE, STDOUT=subprocess.STDOUT)}), \
                patch.dict(os.environ, {"RUNTIME_E2B_REQUEST_TIMEOUT_SECONDS": "40"}):
            self.assertEqual(fixture_type.start_runtime(fixture), "http://127.0.0.1:19001")
        with patch.dict(os.environ, {"RUNTIME_E2B_REQUEST_TIMEOUT_SECONDS": "1"}):
            fixture_type.stop_runtime(fixture)
        self.assertEqual([call for call in calls if call[0] == "wait"], [("wait", 100)])
        self.assertFalse(fixture.runtime_reader.is_alive())

    def test_fixture_wait_budget_adds_margin_to_the_runtime_shutdown_contract(self):
        self.assertEqual(product.runtime_shutdown_wait_seconds({}), 60)
        for request_timeout in (1, 20, 40, 60):
            with self.subTest(request_timeout=request_timeout):
                environment = {"RUNTIME_E2B_REQUEST_TIMEOUT_SECONDS": str(request_timeout)}
                self.assertEqual(product.runtime_shutdown_wait_seconds(environment),
                                 request_timeout * 2 + 20)
        for value in ("0", "61", "-1", "private-invalid-input"):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "invalid") as raised:
                product.runtime_shutdown_wait_seconds({"RUNTIME_E2B_REQUEST_TIMEOUT_SECONDS": value})
            self.assertNotIn("private-invalid-input", str(raised.exception))

    def test_absent_process_performs_no_cleanup(self):
        live_fixture_type().stop_runtime(SimpleNamespace(runtime_process=None))
