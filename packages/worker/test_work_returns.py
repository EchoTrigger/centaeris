import os
import threading
import unittest
from unittest.mock import patch
os.environ.update(RUNTIME_INTERNAL_URL="http://runtime.invalid", API_INTERNAL_URL="http://api.invalid", INTERNAL_API_TOKEN="synthetic-token")
import worker


class WorkReturnAuditControlTests(unittest.TestCase):
    def page(self):
        return {"schema": "workspace.agent_work.return_audit.claimed.v1", "disposition": "claimed",
                "leaseOwner": "synthetic-audit-owner", "after": None, "through": "003", "next": None,
                "entries": [{"cursor": f"{index:03}", "workAgentRunId": f"run_{index}", "materialized": False}
                            for index in range(1, 4)]}

    def test_partial_progress_deadline_keeps_unattempted_tail(self):
        clock, attempted, finished = [0], [], []
        def request(path, body, *args, **kwargs):
            self.assertGreater(kwargs["timeout"], 0)
            self.assertLessEqual(kwargs["timeout"], worker.CONTROL_HTTP_TIMEOUT_SECONDS-clock[0])
            if path.endswith("claim"): return self.page()
            if path.endswith("schedule"):
                attempted.append(body["workAgentRunId"])
                clock[0] += worker.CONTROL_HTTP_TIMEOUT_SECONDS*0.8
                return {"schema": "workspace.agent_work.return_scheduled.v1", "disposition": "inserted"}
            finished.append(body)
            return {"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"}
        with patch.object(worker, "api_request", side_effect=request), patch.object(worker.time, "monotonic", side_effect=lambda: clock[0]):
            worker.WorkReturnAudit()()
        self.assertEqual(attempted, ["run_1"])
        self.assertEqual(finished[0]["after"], "001")
        self.assertFalse(finished[0]["complete"])

    def test_unknown_source_and_lost_result_retry_without_cursor_loss(self):
        for status in (None, 404):
            after, calls, finished = [None], [], []
            def request(path, body, *args, **kwargs):
                if path.endswith("claim"): return {**self.page(), "after": after[0]}
                if path.endswith("schedule"):
                    calls.append(body["workAgentRunId"])
                    if len(calls)==1:
                        error = worker.DependencyUnavailable("result unknown") if status is None else RuntimeError("source unknown")
                        error.http_status = status
                        raise error
                    return {"schema": "workspace.agent_work.return_scheduled.v1", "disposition": "existing"}
                finished.append(body); after[0]=body["after"]
                return {"schema": "workspace.agent_work.return_audit.finished.v1", "disposition": "recorded"}
            with self.subTest(status=status), patch.object(worker, "api_request", side_effect=request), patch.object(worker.time, "monotonic", return_value=0):
                worker.WorkReturnAudit()()
                self.assertIsNone(after[0])
                worker.WorkReturnAudit()()
            self.assertEqual(calls, ["run_1", "run_1", "run_2", "run_3"])
            self.assertTrue(finished[-1]["complete"])

    def test_stop_and_malformed_page_never_advance_cursor(self):
        stopped=threading.Event(); stopped.set()
        with patch.object(worker, "api_request") as api: worker.WorkReturnAudit(stopped)()
        api.assert_not_called(); stopped.clear()
        def claim(*args, **kwargs): stopped.set(); return self.page()
        with patch.object(worker, "api_request", side_effect=claim) as api: worker.WorkReturnAudit(stopped)()
        self.assertEqual(api.call_count, 1)
        invalid=self.page(); invalid["entries"][0]["cursor"]="999"
        with patch.object(worker, "api_request", return_value=invalid) as api:
            with self.assertRaises(RuntimeError): worker.WorkReturnAudit()()
        self.assertEqual(api.call_count, 1)
