"""A shared return audit preserves restart progress and rejects stale finishers."""
import json
import threading
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from django.conf import settings
from django.test import override_settings
from django.test import Client
from django.db import connection, connections, transaction

from .test_agent_work_returns import AgentWorkReturnTests


class WorkReturnAuditBoundTests(AgentWorkReturnTests):
    def audit(self, operation, **fields):
        return self.client.post(f"/internal/agent-work/returns/audit/{operation}",
            content_type="application/json", data=json.dumps({
                "schema": f"workspace.agent_work.return_audit.{operation}.v1", **fields}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    @override_settings(WORK_RETURN_AUDIT_INTERVAL_SECONDS=1)
    def test_two_workers_restart_cursor_and_expired_owner_fence(self):
        first_child, second_child = self.child("audit-first"), self.child("audit-second")
        first = self.audit("claim", limit=1)
        self.assertEqual(first.status_code, 200, first.content)
        page = first.json()
        self.assertEqual(page["disposition"], "claimed")
        self.assertEqual(self.audit("claim", limit=1).json()["disposition"], "idle")
        first_cursor = page["entries"][0]["cursor"]
        recorded = self.audit("finish", leaseOwner=page["leaseOwner"], after=first_cursor, complete=False)
        self.assertEqual(recorded.status_code, 200, recorded.content)
        # The next process reads durable progress, rather than its own memory.
        with patch("app_core.agent_work_return_jobs._now_ms", return_value=10**14):
            resumed = self.audit("claim", limit=1)
            self.assertEqual(resumed.status_code, 200, resumed.content)
            resumed_page = resumed.json()
            self.assertEqual(resumed_page["after"], first_cursor)
            old = self.audit("finish", leaseOwner=page["leaseOwner"], after=first_cursor, complete=False)
            self.assertEqual(old.status_code, 409, old.content)
        with patch("app_core.agent_work_return_jobs._now_ms", return_value=10**14 + 10**9):
            replacement = self.audit("claim", limit=1)
            self.assertEqual(replacement.json()["after"], first_cursor)
            stale = self.audit("finish", leaseOwner=resumed_page["leaseOwner"],
                               after=resumed_page["entries"][-1]["cursor"], complete=True)
            self.assertEqual(stale.status_code, 409, stale.content)
        self.assertEqual({entry["workAgentRunId"] for entry in page["entries"] + resumed_page["entries"]},
                         {first_child.pk, second_child.pk})

    def test_simultaneous_claims_grant_one_shared_audit_lease(self):
        from .models import WorkReturnAuditCursor
        self.child("simultaneous-audit")
        WorkReturnAuditCursor.objects.get_or_create(pk="work_returns")
        entered = threading.Barrier(2)

        def now():
            entered.wait(timeout=4)
            return 1000

        def claim():
            try:
                with connection.cursor() as cursor:
                    cursor.execute("SET statement_timeout = '4s'")
                return Client().post("/internal/agent-work/returns/audit/claim",
                    content_type="application/json", data=json.dumps({
                        "schema": "workspace.agent_work.return_audit.claim.v1", "limit": 100}),
                    HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
            finally:
                connections.close_all()

        with patch("app_core.agent_work_return_jobs._now_ms", side_effect=now):
            with ThreadPoolExecutor(max_workers=2) as executor:
                futures = [executor.submit(claim), executor.submit(claim)]
                responses = [future.result(timeout=5) for future in futures]
        self.assertEqual([response.status_code for response in responses], [200, 200])
        self.assertEqual(sorted(response.json()["disposition"] for response in responses), ["claimed", "idle"])

    def test_lost_admission_schedule_is_repaired_with_one_stable_runtime_job(self):
        with patch("app_core.agent_work_return_jobs.schedule_runtime_job", side_effect=RuntimeError("response lost")):
            child = self.child("lost-return-schedule")
        page = self.audit("claim", limit=100).json()
        self.assertEqual(page["disposition"], "claimed")
        self.assertEqual([entry["workAgentRunId"] for entry in page["entries"] if not entry["materialized"]], [child.pk])
        jobs = {}
        def runtime(payload, **kwargs):
            self.assertGreater(kwargs["timeout"], 0)
            self.assertLessEqual(kwargs["timeout"], settings.RUNTIME_CONTROL_TIMEOUT_SECONDS)
            created = payload["jobId"] not in jobs
            jobs.setdefault(payload["jobId"], payload)
            return {"disposition": "inserted" if created else "existing", "job": jobs[payload["jobId"]]}
        def schedule():
            return self.client.post("/internal/agent-work/returns/schedule", content_type="application/json",
                data=json.dumps({"schema": "workspace.agent_work.return_schedule.v1", "workAgentRunId": child.pk}),
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        with patch("app_core.agent_work_return_jobs.schedule_runtime_job", side_effect=runtime) as producer:
            self.assertEqual(schedule().json()["disposition"], "inserted")
            self.assertEqual(schedule().json()["disposition"], "existing")
            self.terminal(child)
            self.assertEqual(self.publish(child.pk).status_code, 201)
            self.assertEqual(schedule().json()["disposition"], "delivered")
        self.assertEqual(producer.call_count, 2)
        self.assertEqual(list(jobs), ["agent_work.return:" + child.pk])
        self.assertEqual(next(iter(jobs.values()))["payloadRef"], "record:agent_work:" + child.pk)

    @override_settings(WORK_RETURN_AUDIT_INTERVAL_SECONDS=3600, RUNTIME_CONTROL_TIMEOUT_SECONDS=5)
    def test_lost_claim_response_still_observes_configured_audit_interval(self):
        self.child("claim-response-lost")
        with patch("app_core.agent_work_return_jobs._now_ms", return_value=1000):
            lost = self.audit("claim", limit=100)
        self.assertEqual(lost.json()["disposition"], "claimed")
        # No finish arrives. An expired short lease does not authorize another
        # history page before the explicitly configured one-hour interval.
        with patch("app_core.agent_work_return_jobs._now_ms", return_value=11001):
            retry = self.audit("claim", limit=100)
        self.assertEqual(retry.json()["disposition"], "idle", retry.content)

    def test_finisher_waiting_on_row_lock_checks_expiry_after_acquisition(self):
        from .models import WorkReturnAuditCursor
        clock, attempted = [1000], threading.Event()
        with patch("app_core.agent_work_return_jobs._now_ms", side_effect=lambda: clock[0]):
            page = self.audit("claim", limit=100).json()
            def finish():
                try:
                    with connection.cursor() as cursor:
                        cursor.execute("SET statement_timeout = '4s'")
                    def observe(execute, sql, params, many, context):
                        if "FOR UPDATE" in sql and "workreturnauditcursor" in sql:
                            attempted.set()
                        return execute(sql, params, many, context)
                    with connection.execute_wrapper(observe):
                        return Client().post("/internal/agent-work/returns/audit/finish",
                            content_type="application/json", data=json.dumps({
                                "schema": "workspace.agent_work.return_audit.finish.v1",
                                "leaseOwner": page["leaseOwner"], "after": None, "complete": True}),
                            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
                finally:
                    connections.close_all()
            with ThreadPoolExecutor(max_workers=1) as executor:
                with transaction.atomic():
                    held = WorkReturnAuditCursor.objects.select_for_update().get(pk="work_returns")
                    result = executor.submit(finish)
                    self.assertTrue(attempted.wait(2), "finisher did not attempt its row lock")
                    clock[0] = held.leaseExpiresAtMs + 1
                response = result.result(timeout=5)
        self.assertEqual(response.status_code, 409, response.content)

    def test_dead_lettered_stable_job_is_not_revived_and_manual_publication_is_idempotent(self):
        from .models import AgentRun, AgentWorkReturn
        jobs = {}
        def runtime(payload, **kwargs):
            jobs.setdefault(payload["jobId"], {**payload, "status": "dead_lettered"})
            return {"disposition": "existing", "job": jobs[payload["jobId"]]}
        with patch("app_core.agent_work_return_jobs.schedule_runtime_job", side_effect=runtime):
            child = self.child("manual-dead-letter-return")
            original_runs = AgentRun.objects.count()
            response = self.client.post("/internal/agent-work/returns/schedule", content_type="application/json",
                data=json.dumps({"schema": "workspace.agent_work.return_schedule.v1", "workAgentRunId": child.pk}),
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
            self.assertEqual(response.json()["disposition"], "existing")
            self.assertEqual(next(iter(jobs.values()))["status"], "dead_lettered")
            self.terminal(child)
            delivered, duplicate = self.publish(child.pk), self.publish(child.pk)
        self.assertEqual((delivered.status_code, duplicate.status_code), (201, 200))
        self.assertEqual(delivered.json()["notice"], duplicate.json()["notice"])
        self.assertEqual(AgentWorkReturn.objects.filter(child_run=child).count(), 1)
        self.assertEqual(AgentRun.objects.count(), original_runs)
        self.assertEqual(list(jobs), ["agent_work.return:" + child.pk])
