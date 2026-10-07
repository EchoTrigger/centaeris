"""Committed Core/Runtime facts survive published job outboxes and worker restart."""
import json
import os
from pathlib import Path
import subprocess
import sys
from unittest.mock import patch

from django.conf import settings
from django.db import connection
from django.test import LiveServerTestCase

from . import test_agent_work as fixture
from . import test_agent_work_returns as returns_fixture
from .models import AgentRun, AgentWorkReturn, AgentWorkSession, HostedOperationReceipt
from .runtime_client import build_agent_run_start


class WorkReturnRuntimeTests(LiveServerTestCase):
    serialized_rollback = True
    setUp = fixture.AgentWorkTests.setUp
    request_fact = fixture.AgentWorkTests.request_fact
    dependencies = fixture.AgentWorkTests.dependencies
    post = fixture.AgentWorkTests.post
    child = returns_fixture.AgentWorkReturnTests.child
    publish = returns_fixture.AgentWorkReturnTests.publish
    query = returns_fixture.AgentWorkReturnTests.query

    def commit_facts(self, cases):
        tables_query = "SELECT tablename FROM pg_tables WHERE schemaname='runtime'"
        with connection.cursor() as cursor:
            cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='runtime')")
            previous_schema_exists = cursor.fetchone()[0]
            cursor.execute(tables_query)
            previous_tables = {row[0] for row in cursor.fetchall()}

        def cleanup_runtime_tables():
            with connection.cursor() as cursor:
                cursor.execute(tables_query)
                created = {row[0] for row in cursor.fetchall()} - previous_tables
                if created:
                    cursor.execute("DROP TABLE " + ", ".join("runtime." + connection.ops.quote_name(name) for name in sorted(created)))
                if not previous_schema_exists:
                    cursor.execute("DROP FUNCTION IF EXISTS runtime.notify_runtime_job_ready_v1()")
                    cursor.execute("DROP SCHEMA IF EXISTS runtime")
        self.addCleanup(cleanup_runtime_tables)
        database = settings.DATABASES["default"]
        runtime_fixture = {"database": {key: database[key] for key in ("NAME", "USER", "PASSWORD", "HOST", "PORT")},
            "cases": [{"kind": kind, "agentRunStart": build_agent_run_start(child)} for kind, child in cases]}
        result = subprocess.run(["cargo", "test", "--locked", "-p", "runtime_server",
            "agent_work::tests::returns::django_committed_work_return_facts", "--", "--ignored", "--exact", "--nocapture"],
            cwd=Path(__file__).resolve().parents[3],
            env={**os.environ, "CARGO_BUILD_JOBS": "1", "CENTAERIS_WORK_RETURN_FIXTURE": json.dumps(runtime_fixture)},
            capture_output=True, text=True, encoding="utf-8", timeout=300)
        diagnostic = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, diagnostic)
        for kind, _ in cases:
            marker = "work-return-fenced-fact-ok: " + kind
            self.assertIn(marker, diagnostic)
            print(marker)

    def test_real_fenced_terminal_cancel_and_dead_letter_survive_published_outbox(self):
        kinds = ("completed", "failed", "cancelled", "preAdmissionCancellation", "lifecycleFailure")
        children = {kind: self.child(kind) for kind in kinds}
        for child in children.values():
            self.assertEqual(self.publish(child.id).status_code, 202)
        self.commit_facts(list(children.items()))
        self.assertEqual(AgentWorkReturn.objects.count(), 0, "terminal commit does not require notice delivery")
        cancelled = children["preAdmissionCancellation"]
        cancelled.refresh_from_db()
        self.assertIsNotNone(cancelled.preAdmissionCancelledAt)
        self.assertFalse(cancelled.events.exists())
        with connection.cursor() as cursor:
            cursor.execute("SELECT COUNT(*) FROM runtime.runtime_job_outbox WHERE published_at_ms IS NULL")
            self.assertEqual(cursor.fetchone()[0], 0)
        failed = children["lifecycleFailure"]
        AgentRun.objects.filter(pk=failed.pk).update(status="failed", transitionReason="agent_run_lifecycle_dead_lettered")

        # Preserve the production public Runtime Job shape at the HTTP read seam;
        # the contents come from the real Runtime store, not a synthesized outcome.
        def persisted_runtime_job(job_id):
            with connection.cursor() as cursor:
                cursor.execute("SELECT job_id,job_kind,status,session_id,payload_ref,idempotency_key,updated_at_ms FROM runtime.runtime_jobs WHERE job_id=%s", [job_id])
                row = cursor.fetchone()
            return dict(zip(("jobId", "jobKind", "status", "sessionId", "payloadRef", "idempotencyKey", "updatedAtMs"), row)) if row else None

        def run_publisher():
            process = subprocess.run([sys.executable, "-B", "-c", "import worker; worker.WorkReturnPublisher()(); print('work-return-live-worker-ok')"],
                cwd=Path(__file__).resolve().parents[2] / "worker",
                env={**os.environ, "API_INTERNAL_URL": self.live_server_url, "RUNTIME_INTERNAL_URL": "http://127.0.0.1:1",
                    "INTERNAL_API_TOKEN": settings.INTERNAL_API_TOKEN, "NO_PROXY": "127.0.0.1,localhost"},
                capture_output=True, text=True, encoding="utf-8", timeout=20)
            self.assertEqual(process.returncode, 0, process.stdout + process.stderr)
            self.assertIn("work-return-live-worker-ok", process.stdout)

        with patch("app_core.agent_work_returns.get_runtime_job", side_effect=persisted_runtime_job), \
             patch("app_core.agent_work.schedule_agent_run_lifecycle", side_effect=AssertionError("no new scheduling")):
            run_publisher()
            notices = list(AgentWorkReturn.objects.order_by("id").values())
            self.assertEqual(len(notices), 5)
            run_publisher()  # A new process restarts from the head after outbox publication.
            self.assertEqual(list(AgentWorkReturn.objects.order_by("id").values()), notices)
        for notice in AgentWorkReturn.objects.all():
            self.assertEqual(self.query(notice.id).status_code, 200)
        self.assertEqual((AgentRun.objects.count(), AgentWorkSession.objects.count(), HostedOperationReceipt.objects.count()), (6, 5, 5))
        print("work-return-live-worker-restart-published-outbox-ok")
