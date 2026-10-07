"""Only the committed canonical confirmation pair handles a retained notice."""
import importlib
import importlib.util
import json
from pathlib import Path

from django.conf import settings
from django.db import connection, transaction
from django.test import TransactionTestCase
from django.test.utils import CaptureQueriesContext

from . import test_agent_work_consumption as fixture
from .models import AgentRun, SessionEvent, WorkspaceMembership


class WorkReturnConfirmationTests(TransactionTestCase):
    serialized_rollback = True
    setUp = fixture.AgentWorkConsumptionTests.setUp
    request_fact = fixture.AgentWorkConsumptionTests.request_fact
    dependencies = fixture.AgentWorkConsumptionTests.dependencies
    post = fixture.AgentWorkConsumptionTests.post
    child = fixture.AgentWorkConsumptionTests.child
    terminal = fixture.AgentWorkConsumptionTests.terminal
    publish = fixture.AgentWorkConsumptionTests.publish
    notice = fixture.AgentWorkConsumptionTests.notice
    consume = fixture.AgentWorkConsumptionTests.consume
    attempts = fixture.AgentWorkConsumptionTests.attempts
    runtime = fixture.AgentWorkConsumptionTests.runtime

    def accepted(self):
        notice = self.notice()
        with self.runtime():
            response = self.consume(notice)
        self.assertEqual(response.status_code, 201, response.content)
        attempt = self.attempts().get(pk=response.json()["attemptId"])
        return notice, attempt, attempt.coordinator_run

    def validate(self, notice, attempt, run, **changes):
        body = {"schema": "workspace.agent_work.return_confirm.validate.v1",
                "agentRunId": run.pk, "authorizationDigest": run.authorization.digest,
                "coordinationSessionId": run.session_id, "toolCallId": "confirm-one",
                "noticeId": notice["noticeId"], "attemptId": attempt.pk, **changes}
        with CaptureQueriesContext(connection) as queries:
            response = self.client.post("/internal/agent-work/returns/confirm/validate",
                data=json.dumps(body), content_type="application/json",
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertFalse(any(q["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for q in queries))
        if response.status_code == 200:
            self.assertTrue(any("REPEATABLE READ, READ ONLY" in q["sql"] for q in queries))
        return response

    def module(self):
        name = "app_core.agent_work_confirmation"
        self.assertIsNotNone(importlib.util.find_spec(name), "confirmation projection is missing")
        return importlib.import_module(name)

    def commit(self, run, kind, payload, *, turn="actual-confirm-turn"):
        sequence = (run.session.events.order_by("-sequence").values_list("sequence", flat=True).first() or 0) + 1
        wire = {"schemaVersion": "session.event.v1", "eventVersion": 1,
            "eventId": f"confirm-fixture:{run.pk}:{sequence}", "sequence": sequence,
            "sessionId": run.session_id, "agentRunId": run.pk, "turnId": turn,
            "createdAtMs": sequence, "type": kind, "payload": {"callId": "confirm-one", **payload}}
        return SessionEvent.objects.create(eventId=wire["eventId"], workspace_id=run.workspace_id,
            session=run.session, agent_run=run, sequence=sequence, agent_run_sequence=sequence,
            payload=wire, createdAtMs=sequence, projects_to_agent_run_stream=True)

    def pair(self, notice, attempt, run, *, call_changes=None, result_changes=None, validated=None):
        if validated is None:
            response = self.validate(notice, attempt, run)
            self.assertEqual(response.status_code, 200, response.content)
            validated = response.json()
        call = {"toolName": "confirm_work_return", "providerId": "workspace.agent_work",
            "toolContractDigest": self.module().confirmation_contract_digest(),
            "normalizedInput": {"notice_id": notice["noticeId"], "attempt_id": attempt.pk},
            **(call_changes or {})}
        self.commit(run, "tool_call", call)
        return self.commit(run, "tool_result", {"toolName": "confirm_work_return",
            "resultState": "successWithOutput", "modelContent": json.dumps(validated),
            **(result_changes or {})})

    def test_contract_has_two_exact_model_arguments_and_continues(self):
        path = Path(__file__).parent / "contracts/confirm_work_return.json"
        self.assertTrue(path.exists(), "confirmation contract is missing")
        contract = json.loads(path.read_text())
        self.assertEqual(contract["name"], "confirm_work_return")
        self.assertEqual(contract["providerId"], "workspace.agent_work")
        self.assertEqual(contract["turnBehavior"], "continueTurn")
        self.assertFalse(contract["concurrencySafe"])
        self.assertEqual(contract["inputSchema"]["required"], ["notice_id", "attempt_id"])
        self.assertEqual(set(contract["inputSchema"]["properties"]), {"notice_id", "attempt_id"})
        self.assertFalse(contract["inputSchema"]["additionalProperties"])

    def test_validation_is_read_only_and_server_supplies_original_identity(self):
        notice, attempt, run = self.accepted()
        before = SessionEvent.objects.count(), self.attempts().count()
        response = self.validate(notice, attempt, run)
        self.assertEqual(response.status_code, 200, response.content)
        record = response.json()
        self.assertEqual(record["noticeIdentity"], notice["identity"])
        self.assertEqual((record["agentRunId"], record["noticeId"], record["attemptId"]),
                         (run.pk, notice["noticeId"], attempt.pk))
        self.assertIsNone(self.module().committed_confirmation(attempt))
        self.assertEqual((SessionEvent.objects.count(), self.attempts().count()), before)

    def test_only_exact_successful_pair_projects_once_after_split_commit(self):
        notice, attempt, run = self.accepted()
        result = self.pair(notice, attempt, run)
        projection = self.module().committed_confirmation(attempt)
        self.assertEqual(projection["eventId"], result.pk)
        self.assertEqual(projection["sequence"], result.sequence)
        self.assertEqual(projection["turnId"], "actual-confirm-turn")
        self.assertEqual(self.module().committed_confirmation(attempt), projection)
        AgentRun.objects.filter(pk=run.pk).update(status="completed")
        self.assertEqual(self.module().committed_confirmation(attempt), projection)

    def test_uncommitted_failed_final_message_ack_and_read_do_not_handle(self):
        notice, attempt, run = self.accepted()
        module = self.module()
        response = self.validate(notice, attempt, run)
        self.assertEqual(response.status_code, 200, response.content)
        validated = response.json()
        for state in ["failed", "denied", "aborted", "successNoOutput", "successNoMatches"]:
            with self.subTest(state=state), transaction.atomic():
                self.pair(notice, attempt, run, result_changes={"resultState": state}, validated=validated)
                self.assertIsNone(module.committed_confirmation(attempt))
                transaction.set_rollback(True)
        with transaction.atomic():
            self.pair(notice, attempt, run, validated=validated)
            transaction.set_rollback(True)
        self.commit(run, "assistant_message", {"modelMarkdown": "Handled in Final"})
        self.commit(run, "tool_result", {"toolName": "send_message", "resultState": "successWithOutput", "modelContent": "handled"})
        self.commit(run, "tool_result", {"toolName": "read", "resultState": "successWithOutput", "modelContent": "read the work output"})
        self.attempts().filter(pk=attempt.pk).update(acknowledged_at_ms=7)
        self.assertIsNone(module.committed_confirmation(attempt))

    def test_forged_pair_identities_and_result_body_never_project(self):
        notice, attempt, run = self.accepted()
        response = self.validate(notice, attempt, run)
        self.assertEqual(response.status_code, 200, response.content)
        validated = response.json()
        for call, result in [({"providerId": "other"}, {}),
            ({"toolContractDigest": "sha256:" + "f" * 64}, {}),
            ({"normalizedInput": {"notice_id": notice["noticeId"], "attempt_id": "other"}}, {}),
            ({}, {"modelContent": "handled"}), ({}, {"callId": "other"}),
            ({}, {"toolName": "other"})]:
            with self.subTest(call=call, result=result), transaction.atomic():
                self.pair(notice, attempt, run, call_changes=call, result_changes=result, validated=validated)
                self.assertIsNone(self.module().committed_confirmation(attempt))
                transaction.set_rollback(True)

    def test_wrong_pending_and_other_run_attempts_are_rejected(self):
        notice, attempt, run = self.accepted()
        self.assertEqual(self.validate(notice, attempt, run, attemptId="unknown").status_code, 403)
        self.assertEqual(self.validate(notice, attempt, self.run).status_code, 403)
        self.attempts().filter(pk=attempt.pk).update(coordinator_run=None)
        self.assertEqual(self.validate(notice, attempt, run).status_code, 403)

    def test_original_membership_revocation_and_rejoin_do_not_revive_confirmation(self):
        notice, attempt, run = self.accepted()
        self.membership.delete()
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.assertEqual(self.validate(notice, attempt, run).status_code, 403)

    def test_transport_rejects_model_aliases_and_extra_source_fields(self):
        notice, attempt, run = self.accepted()
        for changes in [{"notice_id": notice["noticeId"]}, {"attemptId": " /path "},
                        {"sourceAgentRunId": self.run.pk}, {"toolCallId": ""}]:
            with self.subTest(changes=changes):
                self.assertEqual(self.validate(notice, attempt, run, **changes).status_code, 400)

    def test_existing_query_reads_target_notice_attempt_and_handled_without_writes(self):
        notice, attempt, run = self.accepted()
        body = {"schema": "workspace.agent_work.query.v1", "agentRunId": run.pk,
            "authorizationDigest": run.authorization.digest, "coordinationSessionId": run.session_id,
            "toolCallId": "query-one", "sourceAgentRunId": self.run.pk,
            "sourceToolCallId": "dispatch-one"}
        def query():
            with CaptureQueriesContext(connection) as queries:
                response = self.client.post("/internal/agent-work/query", data=json.dumps(body),
                    content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
            self.assertFalse(any(q["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for q in queries))
            self.assertEqual(response.status_code, 200, response.content)
            self.assertIn("returns", response.json()["work"], "the existing query does not expose notice/attempt identities")
            views = response.json()["work"]["returns"]
            self.assertEqual(len(views), 1)
            return views[0]
        view = query()
        self.assertEqual(view["notice"], notice)
        self.assertEqual(view["attempt"]["attemptId"], attempt.pk)
        self.assertFalse(view["handled"])
        self.pair(notice, attempt, run)
        self.assertEqual(query()["confirmation"], self.module().committed_confirmation(attempt))
        self.assertTrue(query()["handled"])
