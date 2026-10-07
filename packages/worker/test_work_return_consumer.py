"""Control scans preserve admission identity and progress without a second job queue."""
import os
import unittest
from unittest.mock import patch

os.environ.setdefault("RUNTIME_INTERNAL_URL", "http://runtime.invalid")
os.environ.setdefault("API_INTERNAL_URL", "http://api.invalid")
os.environ.setdefault("INTERNAL_API_TOKEN", "test-internal-token")
import worker


def notice(index):
    return "work-return:" + f"{index:064x}"


class NoticeLedger:
    def __init__(self, count=3):
        self.ids = [notice(index) for index in range(1, count + 1)]
        self.attempts = {}
        self.pending = set()
        self.bound = set()
        self.lost = set()
        self.now = 0
        self.slow = set()
        self.discovery_bounds = []

    def request(self, path, body, reason, **kwargs):
        if path.endswith("/consume/discover"):
            ids = [value for value in self.ids if value not in self.bound]
            through = body["through"] or (max(ids) if ids else None)
            self.discovery_bounds.append(through)
            rows = [value for value in ids if (body["after"] is None or value > body["after"])
                    and through is not None and value <= through]
            page = rows[:body["limit"]]
            return {"schema": "workspace.agent_work.consume_discovered.v1", "through": through,
                "next": page[-1] if len(rows) > len(page) else None,
                "entries": [{"cursor": value, "noticeId": value,
                             "operationId": "agent-work-consume:" + value.removeprefix("work-return:")} for value in page]}
        if path.endswith("/consume"):
            value = body["noticeId"]
            if value in self.slow:
                self.now += worker.RECONCILE_INTERVAL_SECONDS
            self.attempts.setdefault(value, {"schema": "workspace.agent_work.consumed.v1", "disposition": "accepted",
                "noticeId": value, "attemptId": "attempt:" + value,
                "operation": {"agentRunId": None if value in self.pending else "run:" + value,
                              "turnId": None if value in self.pending else "turn:" + value}})
            if value not in self.pending:
                self.bound.add(value)
            if value in self.lost:
                raise worker.DependencyUnavailable("response lost after durable admission")
            return self.attempts[value]
        raise AssertionError(path)


class WorkReturnConsumerTests(unittest.TestCase):
    def test_pending_acceptance_survives_restart_and_binds_without_another_root(self):
        ledger = NoticeLedger(2)
        ledger.pending.add(ledger.ids[0])
        with patch.object(worker, "api_request", side_effect=ledger.request):
            worker.WorkReturnConsumer()()
            self.assertEqual(set(ledger.attempts), set(ledger.ids))
            self.assertEqual(ledger.bound, {ledger.ids[1]})
            committed = dict(ledger.attempts)
            ledger.pending.clear()
            worker.WorkReturnConsumer()()
            worker.WorkReturnConsumer()()
        self.assertEqual(set(ledger.attempts), set(ledger.ids))
        self.assertEqual(ledger.attempts, committed)
        self.assertEqual(ledger.bound, set(ledger.ids))

    def test_lost_response_and_budget_leave_unattempted_tail_under_original_bound(self):
        ledger = NoticeLedger()
        ledger.lost.add(ledger.ids[0])
        ledger.slow.add(ledger.ids[0])
        with patch.object(worker, "api_request", side_effect=ledger.request), \
             patch.object(worker.time, "monotonic", side_effect=lambda: ledger.now):
            scan = worker.WorkReturnConsumer()
            scan()
            self.assertEqual((scan.after, scan.through), (ledger.ids[0], ledger.ids[-1]))
            self.assertEqual(set(ledger.attempts), {ledger.ids[0]})
            upper = scan.through
            ledger.ids.append(notice(100))
            scan()
            self.assertEqual(set(ledger.attempts), set(ledger.ids[:3]))
            self.assertEqual(ledger.discovery_bounds[:2], [upper, upper])
            self.assertIsNone(scan.through)
            worker.WorkReturnConsumer()()
        self.assertEqual(set(ledger.attempts), set(ledger.ids))

    def test_existing_return_control_round_gives_consumer_its_own_budget_after_delivery_failure(self):
        ledger = NoticeLedger(1)
        def request(path, body, reason, **kwargs):
            if path == "/internal/agent-work/returns/discover":
                ledger.now += worker.RECONCILE_INTERVAL_SECONDS
                raise worker.DependencyUnavailable("delivery discovery timed out")
            return ledger.request(path, body, reason, **kwargs)
        with patch.object(worker, "api_request", side_effect=request), \
             patch.object(worker.time, "monotonic", side_effect=lambda: ledger.now):
            worker.WorkReturnReconciler()()
        self.assertEqual(set(ledger.attempts), set(ledger.ids))

    def test_malformed_or_reordered_page_cannot_create_an_admission(self):
        for changes in ({"schema": "unknown"}, {"through": None}, {"notice_id": "alias"},
                        {"entries": [{"cursor": notice(1), "noticeId": notice(2), "operationId": "agent-work-consume:" + "a" * 64}]},
                        {"entries": [{"cursor": notice(1), "noticeId": notice(1), "operationId": "bad op"}]}):
            with self.subTest(changes=changes):
                ledger = NoticeLedger(1)
                def request(path, body, reason, **kwargs):
                    result = ledger.request(path, body, reason, **kwargs)
                    return {**result, **changes} if path.endswith("/consume/discover") else result
                with patch.object(worker, "api_request", side_effect=request):
                    with self.assertRaises(RuntimeError):
                        worker.WorkReturnConsumer()()
                self.assertFalse(ledger.attempts)

    def test_stop_keeps_unattempted_notice_eligible_for_the_next_worker(self):
        ledger = NoticeLedger(1)
        stopped = worker.threading.Event()
        stopped.set()
        with patch.object(worker, "api_request", side_effect=ledger.request):
            worker.WorkReturnConsumer(stopped)()
            self.assertFalse(ledger.attempts)
            worker.WorkReturnConsumer()()
        self.assertEqual(set(ledger.attempts), set(ledger.ids))
