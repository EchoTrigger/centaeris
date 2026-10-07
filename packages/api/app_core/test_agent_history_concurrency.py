"""PostgreSQL checks for the shared writer fence and one-query read snapshot."""
from concurrent.futures import ThreadPoolExecutor
from threading import Event
import json
import secrets
import time

from django.db import DatabaseError, connection, connections, transaction
from django.test import Client, TransactionTestCase

from . import test_agent_inputs, test_agent_messages
from .agent_history import _HISTORY_CANDIDATES
from .agent_inputs import accept_agent_input
from .agent_messages import message_contract_digest
from .models import AgentCoordinationSession, AgentInput, Session


class AgentHistoryConcurrencyTests(TransactionTestCase):
    serialized_rollback = True
    message_run = test_agent_messages.AgentMessageTests.message_run
    commit = test_agent_messages.AgentMessageTests.commit

    def setUp(self):
        test_agent_inputs.AgentInputConcurrencyTests.setUp(self)
        self.session = AgentCoordinationSession.objects.get(agent=self.agent).session
        self.run = self.message_run(self.session)
        self.input_url = f"/api/agents/{self.agent.pk}/inputs"
        self.history_url = f"/api/agents/{self.agent.pk}/history"

    def submit(self, input_id):
        return self.client.post(self.input_url, content_type="application/json",
            data=json.dumps({"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": input_id, "body": "exact text"}))

    def write_output(self):
        self.commit(self.run, "tool_call", {"toolName": "send_message",
            "providerId": "workspace.agent_messages", "toolContractDigest": message_contract_digest(),
            "normalizedInput": {"body": "committed output", "session_refs": [], "file_refs": []}},
            call_id="timeline-output")
        return self.commit(self.run, "tool_result", {"toolName": "send_message",
            "resultState": "successWithOutput"}, call_id="timeline-output")

    def test_output_transaction_fence_rejects_acceptance_then_retry_anchors_after_commit(self):
        ready, release = Event(), Event()
        def writer():
            try:
                with transaction.atomic():
                    Session.objects.select_for_update().get(pk=self.session.pk)
                    result = self.write_output()
                    ready.set()
                    if not release.wait(timeout=5):
                        raise AssertionError("output writer not released")
                return result.sequence
            finally:
                connections.close_all()
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(writer)
            try:
                self.assertTrue(ready.wait(timeout=5))
                response = self.submit("after-output")
                self.assertEqual(response.status_code, 503, response.content)
                self.assertFalse(AgentInput.objects.exists())
            finally:
                release.set()
            output_sequence = future.result(timeout=5)
        self.assertEqual(self.submit("after-output").status_code, 201)
        self.assertEqual(AgentInput.objects.get().accepted_source_sequence, output_sequence)
        self.assertEqual([row["kind"] for row in self.client.get(self.history_url).json()["items"]],
            ["message", "input"])

    def test_uncommitted_acceptance_holds_the_same_lock_needed_by_output_writer(self):
        def try_writer_lock():
            try:
                try:
                    with transaction.atomic():
                        Session.objects.select_for_update(nowait=True).get(pk=self.session.pk)
                except DatabaseError as error:
                    return getattr(error.__cause__, "sqlstate", None)
                return "unexpected lock acquired"
            finally:
                connections.close_all()
        with transaction.atomic():
            fact, created = accept_agent_input(self.user, self.agent.pk, "before-output", "exact text")
            self.assertTrue(created)
            with ThreadPoolExecutor(max_workers=1) as pool:
                self.assertEqual(pool.submit(try_writer_lock).result(timeout=5), "55P03")
        with transaction.atomic():
            Session.objects.select_for_update().get(pk=self.session.pk)
            self.write_output()
        fact.refresh_from_db()
        self.assertEqual(fact.accepted_source_sequence, 0)
        self.assertEqual([row["kind"] for row in self.client.get(self.history_url).json()["items"]],
            ["input", "message"])

    def test_history_snapshot_cannot_skip_input_when_output_commits_during_query(self):
        self.assertEqual(self.submit("initial-input").status_code, 201)
        lock_key = secrets.randbelow(2_000_000_000) + 1
        reader_pid, ready = [], Event()
        def reader():
            try:
                client = Client()
                client.force_login(self.user)
                connection.ensure_connection()
                reader_pid.append(connection.connection.info.backend_pid)
                def gate(execute, sql, params, many, context):
                    if sql.lstrip().startswith("SELECT anchor, kind_rank, tie, document FROM ("):
                        ready.set()
                        sql = ("WITH history_snapshot_gate AS MATERIALIZED "
                            "(SELECT pg_advisory_xact_lock(%s)) SELECT page.* "
                            "FROM history_snapshot_gate CROSS JOIN LATERAL (" + sql
                            + ") page ORDER BY page.anchor,page.kind_rank,page.tie")
                        params = [lock_key, *params]
                    return execute(sql, params, many, context)
                with connection.execute_wrapper(gate):
                    response = client.get(self.history_url)
                return response.status_code, response.json()
            finally:
                connections.close_all()
        with connection.cursor() as cursor:
            cursor.execute("SELECT pg_advisory_lock(%s)", [lock_key])
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(reader)
            try:
                self.assertTrue(ready.wait(timeout=5))
                deadline, waiting = time.monotonic() + 5, False
                while time.monotonic() < deadline:
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=%s "
                            "AND locktype='advisory' AND objid=%s AND NOT granted)", [reader_pid[0], lock_key])
                        waiting = cursor.fetchone()[0]
                    if waiting:
                        break
                    time.sleep(0.01)
                self.assertTrue(waiting, "reader must establish its SQL snapshot before writer commits")
                self.assertEqual(self.submit("concurrent-input").status_code, 201)
                with transaction.atomic():
                    Session.objects.select_for_update().get(pk=self.session.pk)
                    self.write_output()
            finally:
                with connection.cursor() as cursor:
                    cursor.execute("SELECT pg_advisory_unlock(%s)", [lock_key])
            status, old_page = future.result(timeout=5)
        self.assertEqual(status, 200, old_page)
        self.assertEqual([row["input"]["inputId"] for row in old_page["items"]], ["initial-input"])
        next_page = self.client.get(self.history_url, {"afterCursor": old_page["newestCursor"]}).json()
        self.assertEqual([row["kind"] for row in next_page["items"]], ["input", "message"])
        self.assertEqual(next_page["items"][0]["input"]["inputId"], "concurrent-input")
