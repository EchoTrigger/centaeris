import importlib.util
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "pool_workload", Path(__file__).resolve().parents[1] / "harness/pool_workload.py")


class WorkloadTests(unittest.TestCase):
    def load(self):
        module = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(module)
        return module

    def test_empty_workload_is_not_success(self):
        with self.assertRaises(ValueError):
            self.load().validate_result([], [], [], 3)

    def test_missing_observer_is_failure(self):
        with self.assertRaises(ValueError):
            self.load().validate_result([{"agentRunId": "r"}], [
                {"agentRunId": "r", "observer": 0, "terminal": "agent_run_completed"}],
                [{"id": "r", "status": "completed", "completedAt": "now"}], 3)

    def test_completed_requires_durable_end_time(self):
        module = self.load()
        accepted = [{"agentRunId": "r"}]
        observers = [{"agentRunId": "r", "observer": i,
                      "terminal": "agent_run_completed"} for i in range(3)]
        with self.assertRaises(ValueError):
            module.validate_result(accepted, observers, [{"id": "r", "status": "completed"}], 3)
        module.validate_result(accepted, observers,
                               [{"id": "r", "status": "completed", "completedAt": "now"}], 3)

    def test_failure_prevents_next_admission_and_saves_result(self):
        module = self.load()
        calls = []

        class Client:
            def __init__(self, records):
                pass

            def call(self, method, path, payload=None, expected=(200,)):
                calls.append(path)
                raise RuntimeError("HTTP 500")

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(module, "Client", Client), \
                patch.object(module, "setup", return_value=("w", "a", "m")):
            result = module.run_workload(object(), directory, seconds=.01, interval=.001)
            self.assertFalse(result["ok"])
            self.assertEqual(calls, ["/api/workspaces/w/sessions/new/messages"])
            self.assertTrue((Path(directory) / "workload.json").exists())

    def test_foreign_endpoint_rejected_before_network(self):
        module = self.load()
        client = module.Client([])
        with self.assertRaises(ValueError):
            client.call("GET", "https://example.com/api/csrf")

    def test_limits_reject_excessive_load(self):
        module = self.load()
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                module.run_workload(object(), directory, seconds=60, interval=1)

    def test_http_500_is_recorded_without_response_or_credentials(self):
        module = self.load()
        records = []
        client = module.Client(records)

        class Response:
            status = 500
            headers = type("Headers", (), {"get_all": lambda *args: []})()

            def __enter__(self):
                return self

            def __exit__(self, *args):
                pass

        with patch.object(client.opener, "open", return_value=Response()):
            with self.assertRaises(RuntimeError):
                client.call("POST", "/api/login", {"password": "private"})
        self.assertEqual(records[0]["status"], 500)
        self.assertIn("latencyMs", records[0])
        self.assertNotIn("private", str(records))

    def test_continuously_active_response_obeys_deadline(self):
        module = self.load()
        response = type("Response", (), {"read1": lambda *args: b"a"})()
        with patch.object(module.time, "monotonic", side_effect=[0, 1, 31]):
            with self.assertRaises(TimeoutError):
                module.Client.read(response, 30)

    def test_observer_failure_stops_later_arrivals(self):
        module = self.load()
        calls = []

        class Client:
            def __init__(self, records):
                pass

            def call(self, method, path, payload=None, expected=(200,)):
                calls.append(payload["operationId"])
                return {"sessionId": "s", "agentRunId": "r"}

            def observe(self, accepted, index, observations, stop):
                observations.append({"agentRunId": "r", "observer": index, "terminal": None})
                stop.set()

        with tempfile.TemporaryDirectory() as directory, \
                patch.object(module, "Client", Client), \
                patch.object(module, "setup", return_value=("w", "a", "m")), \
                patch.object(module, "durable_runs", return_value=[]):
            result = module.run_workload(object(), directory, seconds=.02, interval=.01)
        self.assertFalse(result["ok"])
        self.assertEqual(len(calls), 1)

    def test_existing_abort_admits_no_runs(self):
        module = self.load()
        stop = threading.Event()
        stop.set()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(module, "setup", return_value=("w", "a", "m")):
            result = module.run_workload(object(), directory, stop=stop)
        self.assertFalse(result["ok"])
        self.assertEqual(result["accepted"], [])


if __name__ == "__main__":
    unittest.main()
