import importlib
import json
from unittest.mock import patch

from asgiref.sync import async_to_sync
from django.db import connection
from django.test import RequestFactory, TransactionTestCase

from .models import AgentRun, HostedOperationReceipt, WorkspaceMembership
from .test_hosted_operations import OperationFixture


class QuestionResponseTests(OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        accepted = self.post().json()
        self.run = AgentRun.objects.get(pk=accepted["agentRunId"])
        AgentRun.objects.filter(pk=self.run.pk).update(status="running")
        self.url = f"/api/sessions/{self.run.session_id}/agent-runs/{self.run.pk}/questions/question-1/responses"
        self.body = {"operationId": "question-response-1", "continuationId": "continuation-1", "action": "answer", "answer": "1\nUse Release"}

    def respond(self, **changes):
        return self.client.post(self.url, json.dumps({**self.body, **changes}), content_type="application/json")

    def test_answer_receipt_replays_after_consumption_without_creating_a_run(self):
        from .http.presentation import PresentationLoader
        from .presentation_stream import PresentationScope
        from .test_presentation_stream import create_question_authority_tables
        self.run.refresh_from_db()
        question_turn = "followup-question-turn"
        self.assertNotEqual(question_turn, self.run.turn_id)
        checkpoint = {"schema": "runtime.await_question.v2", "continuationId": "continuation-1",
            "agentRunId": self.run.pk, "authorizationDigest": self.run.authorization.digest,
            "turnId": question_turn, "questionId": "question-1",
            "question": {"question": "Which branch?", "options": ["Release", "Main"]},
            "toolCall": {"id": "question-1", "name": "request_user_input", "argsJson": '{"question":"Which branch?","options":["Release","Main"]}'}}
        create_question_authority_tables()
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.checkpoints(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) "
                "VALUES('answer-question-test','wait',%s,%s,'paused_question','question',1,%s)",
                [self.run.session_id, question_turn, json.dumps(checkpoint)])
        def clean():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.checkpoints WHERE checkpoint_id='answer-question-test'")
        self.addCleanup(clean)
        request = RequestFactory().get("/")
        request.user = self.user
        state = async_to_sync(PresentationLoader(request, PresentationScope(self.workspace.pk, self.run.session_id)).state)()
        self.assertEqual(state["question"]["turnId"], question_turn)
        with patch("app_core.http.workspaces.request_agent_run_question_response") as runtime:
            first = self.respond()
            self.assertEqual(first.status_code, 202, first.content)
            self.assertEqual(first.json(), {"operationId": "question-response-1", "command": "respondQuestion", "status": "accepted",
                "sessionId": self.run.session_id, "agentRunId": self.run.pk, "turnId": self.run.turn_id})
            AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
            replay = self.respond()
            self.assertEqual(replay.json(), first.json())
            self.assertEqual(runtime.call_count, 1)
            self.assertEqual(runtime.call_args.kwargs, {"operation_id": "question-response-1", "continuation_id": "continuation-1",
                "question_id": "question-1", "action": "answer", "answer": "1\nUse Release"})
            self.assertEqual(self.respond(answer="2").status_code, 409)
            self.assertEqual(AgentRun.objects.count(), 1)
        receipt = self.client.get(f"/api/workspaces/{self.workspace.pk}/operations/respondQuestion/question-response-1")
        self.assertEqual(receipt.json(), first.json())

    def test_skip_is_explicit_and_unknown_or_incomplete_responses_fail(self):
        body = {"operationId": "skip-question", "continuationId": "continuation-1", "action": "skip"}
        with patch("app_core.http.workspaces.request_agent_run_question_response") as runtime:
            result = self.client.post(self.url, json.dumps(body), content_type="application/json")
            self.assertEqual(result.status_code, 202, result.content)
            self.assertEqual(runtime.call_args.kwargs["action"], "skip")
            self.assertIsNone(runtime.call_args.kwargs["answer"])
            for invalid in ({**body, "answer": None}, {**body, "action": "answer"},
                    {**body, "unknown": True}, {**body, "operationId": ""}):
                with self.subTest(invalid=invalid):
                    self.assertEqual(self.client.post(self.url, json.dumps(invalid), content_type="application/json").status_code, 400)
            self.assertEqual(runtime.call_count, 1)

    def test_runtime_receipt_recovers_api_rollback_after_acceptance(self):
        with patch("app_core.http.workspaces.request_agent_run_question_response") as runtime:
            with patch("app_core.http.workspaces.accept_operation", side_effect=RuntimeError("receipt_write_failed")):
                self.assertEqual(self.respond().status_code, 503)
            self.assertFalse(HostedOperationReceipt.objects.filter(command="respondQuestion").exists())
            # Runtime returns its durable existing receipt on the retry, even if
            # Core has consumed the wait and completed the original Run already.
            runtime.return_value = {"disposition": "existing"}
            AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
            result = self.respond()
            self.assertEqual(result.status_code, 202, result.content)
            self.assertEqual(runtime.call_count, 2)
            self.assertEqual(AgentRun.objects.count(), 1)

    def test_authority_and_question_target_are_rechecked_before_runtime(self):
        with patch("app_core.http.workspaces.request_agent_run_question_response") as runtime:
            first = self.respond()
            self.assertEqual(first.status_code, 202, first.content)
            alternate = self.url.replace("question-1/responses", "question-2/responses")
            self.assertEqual(self.client.post(alternate, json.dumps(self.body), content_type="application/json").status_code, 409)
            WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).delete()
            self.assertEqual(self.respond().status_code, 404)
            self.assertEqual(runtime.call_count, 1)

    def test_forward_migration_refuses_legacy_wait_without_inventing_question_body(self):
        migration = importlib.import_module("app_core.migrations.0040_question_responses")
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA IF NOT EXISTS runtime")
            cursor.execute("CREATE TABLE IF NOT EXISTS runtime.checkpoints(checkpoint_id text PRIMARY KEY,kind text NOT NULL,"
                "session_id text NOT NULL,turn_id text NOT NULL,status text NOT NULL,done_reason text,updated_at_ms bigint NOT NULL,payload_json text NOT NULL)")
            cursor.execute("INSERT INTO runtime.checkpoints(checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json) "
                "VALUES('legacy-question-test','wait',%s,'turn','paused_question','question',1,%s)",
                [self.run.session_id, json.dumps({"schema": "runtime.await_question.v1"})])
        try:
            with connection.schema_editor() as editor:
                with self.assertRaisesRegex(RuntimeError, "legacy_question_wait_requires_authoritative_reconstruction"):
                    migration.reject_unreconstructable_question_waits(None, editor)
        finally:
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.checkpoints WHERE checkpoint_id='legacy-question-test'")
        with connection.schema_editor() as editor:
            migration.reject_unreconstructable_question_waits(None, editor)
