import os
import threading
import unittest
from unittest.mock import patch

os.environ.update(RUNTIME_INTERNAL_URL="http://runtime.invalid", API_INTERNAL_URL="http://api.invalid",
                  INTERNAL_API_TOKEN="test-internal-token")
import worker


class WorkReturnPublisherTests(unittest.TestCase):
    def page(self, start=0, count=100, more=True, **changes):
        entries = [{"cursor": f"{i:03}", "workAgentRunId": f"run{i:03}"} for i in range(start, start+count)]
        return {"schema": "workspace.agent_work.returns.discovered.v1", "entries": entries,
            "through": "999", "next": entries[-1]["cursor"] if entries and more else None, **changes}

    def test_bounded_discovery_retains_upper_then_restarts_from_head(self):
        pages = [self.page(), self.page(start=100, count=2, more=False), self.page()]
        def request(path, *args, **kwargs):
            return pages.pop(0) if path.endswith("discover") else {}
        with patch.object(worker, "api_request", side_effect=request) as api:
            scan = worker.WorkReturnPublisher()
            scan(); scan(); scan()
        discovery = [call for call in api.call_args_list if call.args[0].endswith("discover")]
        self.assertEqual(len(discovery), 3)
        self.assertEqual([call.args[1]["after"] for call in discovery], [None, "099", None])
        self.assertEqual([call.args[1]["through"] for call in discovery], [None, "999", None])
        for call in discovery:
            self.assertEqual(call.args[0], "/internal/agent-work/returns/discover")
            self.assertEqual(call.args[1]["limit"], 100)
            self.assertEqual(call.kwargs["timeout"], worker.RECONCILE_INTERVAL_SECONDS)
        self.assertEqual(api.call_count-len(discovery), 202)

    def test_lost_response_keeps_page_and_process_restart_replays_from_head(self):
        scan = worker.WorkReturnPublisher()
        scan.after, scan.through = "010", "999"
        with patch.object(worker, "api_request", side_effect=worker.DependencyUnavailable("response lost")):
            with self.assertRaises(worker.DependencyUnavailable): scan()
        self.assertEqual((scan.after, scan.through), ("010", "999"))
        with patch.object(worker, "api_request", return_value=self.page(count=0, more=False)) as api:
            worker.WorkReturnPublisher()()
        self.assertIsNone(api.call_args.args[1]["after"])

    def test_malformed_entries_or_cursor_never_advance_progress(self):
        invalid = [self.page(entries=True), self.page(count=101), self.page(next="999"), self.page(through="changed"),
            self.page(extra="unknown"), self.page(entries=[]), self.page(start=0, count=1),
            self.page(entries=[{"cursor":"011", "workAgentRunId":None}]),
            self.page(entries=[{"cursor":"011", "workAgentRunId":"run", "extra":"unknown"}])]
        for page in invalid:
            with self.subTest(page=page), patch.object(worker, "api_request", return_value=page):
                scan = worker.WorkReturnPublisher()
                scan.after, scan.through = "010", "999"
                with self.assertRaises(RuntimeError): scan()
                self.assertEqual((scan.after, scan.through), ("010", "999"))

    def test_slow_or_lost_materialize_response_advances_only_attempted_source_and_retries_next_pass(self):
        clock, attempted = [0], []
        def request(path, body, reason, **kwargs):
            if path.endswith("discover"):
                return self.page(start=1, count=2, more=False) if body["after"] == "000" else self.page(count=3, more=False)
            attempted.append(body["workAgentRunId"])
            if body["workAgentRunId"] == "run000":
                clock[0] += worker.RECONCILE_INTERVAL_SECONDS
                raise worker.DependencyUnavailable("response lost")
            return {}
        with patch.object(worker, "api_request", side_effect=request), \
             patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]):
            scan = worker.WorkReturnPublisher()
            scan()
            self.assertEqual((scan.after, scan.through), ("000", "999"))
            self.assertEqual(attempted, ["run000"])
            scan()
            self.assertEqual(attempted, ["run000", "run001", "run002"])
            self.assertIsNone(scan.after)
            scan()
            self.assertEqual(attempted[-1], "run000")

    def test_stop_after_discovery_never_advances_an_unattempted_tail(self):
        stopped = threading.Event()
        def discover(*args, **kwargs):
            stopped.set()
            return self.page(count=2, more=False)
        with patch.object(worker, "api_request", side_effect=discover) as api:
            scan = worker.WorkReturnPublisher(stopped)
            scan()
        self.assertEqual(api.call_count, 1)
        self.assertIsNone(scan.after)
        self.assertEqual(scan.through, "999")

    def test_stop_prevents_rpc_and_control_service_joins_single_return_thread(self):
        stopped = threading.Event(); stopped.set()
        with patch.object(worker, "api_request") as api:
            worker.WorkReturnPublisher(stopped)()
        api.assert_not_called()
        threads = []
        class Thread:
            def __init__(self, **kwargs): self.kwargs=kwargs; self.joined=False; threads.append(self)
            def start(self):
                if self.kwargs["name"] == "workspace-work-return-publisher": self.kwargs["args"][2].set()
            def join(self): self.joined=True
        with patch.object(worker.threading, "Thread", Thread), patch.object(worker.signal, "signal"):
            worker.run_worker_service()
        publishers = [t for t in threads if t.kwargs["name"] == "workspace-work-return-publisher"]
        self.assertEqual(len(publishers), 1)
        control = publishers[0].kwargs["args"][0]
        self.assertIsInstance(control, worker.WorkReturnReconciler)
        self.assertIsInstance(control.publisher, worker.WorkReturnPublisher)
        self.assertIsInstance(control.consumer, worker.WorkReturnConsumer)
        self.assertIs(control.publisher.stopped, publishers[0].kwargs["args"][2])
        self.assertIs(control.consumer.stopped, publishers[0].kwargs["args"][2])
        self.assertTrue(publishers[0].joined)

    def test_slow_delivery_thread_does_not_hold_existing_control_threads(self):
        entered, release, stopped = threading.Event(), threading.Event(), threading.Event()
        def api(path, body, reason, **kwargs):
            if path.endswith("returns/discover"):
                entered.set()
                self.assertTrue(release.wait(5))
                return self.page(count=0, more=False)
            return {"activeNext": None, "deadLetterNext": None}
        with patch.object(worker, "api_request", side_effect=api), patch.object(worker, "runtime_request",
                return_value={"disposition":"reconciled", "checked":0, "waiters":[], "next":None}):
            thread = threading.Thread(target=worker.run_loop, args=(worker.WorkReturnPublisher(stopped), 5, stopped))
            thread.start()
            try:
                self.assertTrue(entered.wait(5))
                self.assertIn("waiters", worker.LifecycleReconciler()())
                stopped.set()
            finally:
                release.set(); thread.join(5)
            self.assertFalse(thread.is_alive())
