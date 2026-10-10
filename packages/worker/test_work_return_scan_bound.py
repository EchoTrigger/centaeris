"""Bound return control traffic independently of already delivered history."""
import os
import unittest
from contextlib import contextmanager
from unittest.mock import patch

os.environ.update(RUNTIME_INTERNAL_URL="http://runtime.invalid", API_INTERNAL_URL="http://api.invalid",
                  INTERNAL_API_TOKEN="synthetic-token", WORK_RETURN_AUDIT_INTERVAL_SECONDS="3600")
import worker


@contextmanager
def healthy_lease(*args):
    yield lambda: None


class WorkReturnScanBoundTests(unittest.TestCase):
    def test_delivered_history_does_not_increase_normal_materialization_traffic(self):
        for historical_count in (20, 2000):
            with self.subTest(historical_count=historical_count):
                rows = [f"{index:05}" for index in range(historical_count + 3)]
                materialized = []
                clock = [0]

                def api(path, body, *args, **kwargs):
                    if path.endswith("consume/discover"):
                        return {"schema": "workspace.agent_work.consume_discovered.v1", "entries": [],
                                "through": None, "next": None}
                    if path.endswith("returns/discover"):
                        after = body["after"]
                        tail = [row for row in rows if after is None or row > after]
                        page = tail[:100]
                        return {"schema": "workspace.agent_work.returns.discovered.v1", "through": rows[-1],
                                "next": page[-1] if len(tail) > 100 else None,
                                "entries": [{"cursor": row, "workAgentRunId": "run" + row} for row in page]}
                    if path.endswith("audit/claim"):
                        return {"schema": "workspace.agent_work.return_audit.claimed.v1", "disposition": "idle"}
                    if path.endswith("returns/materialize"):
                        materialized.append(body["workAgentRunId"])
                        return {"schema": "workspace.agent_work.return_materialized.v1", "disposition": "duplicate",
                                "notice": {"noticeId": "retained"}}
                    raise AssertionError(path)

                with patch.object(worker, "api_request", side_effect=api), \
                        patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]):
                    control = worker.WorkReturnReconciler()
                    for _ in range((historical_count + 3) // 100 + 3):
                        control()
                        clock[0] += worker.RECONCILE_INTERVAL_SECONDS
                historical_calls = [identity for identity in materialized if int(identity[3:]) < historical_count]
                print(f"work-return-history-counter history={historical_count} pending=3 historicalCalls={len(historical_calls)}")
                self.assertEqual(len(historical_calls), 0, "normal control repeatedly materializes delivered historical children")

    def test_legitimate_pending_return_yields_without_consuming_failure_budget(self):
        job = {"jobId": "agent_work.return:run_pending", "jobKind": "agent_work.return",
               "sessionId": "session_pending", "payloadRef": "record:agent_work:run_pending",
               "idempotencyKey": "agent_work.return:run_pending", "retryCount": 0, "maxRetries": 1}
        with patch.object(worker, "start_job"), patch.object(worker, "lease_heartbeats", healthy_lease), \
                patch.object(worker, "api_request", return_value={
                    "schema": "workspace.agent_work.return_materialized.v1", "disposition": "pending", "notice": None}), \
                patch.object(worker, "yield_job") as yielded, patch.object(worker, "complete_job") as complete, \
                patch.object(worker, "fail_claimed_job") as failed:
            worker.execute_claimed_job(job, "synthetic-return-owner")
        failed.assert_not_called()
        complete.assert_not_called()
        self.assertEqual(yielded.call_count, 1)
