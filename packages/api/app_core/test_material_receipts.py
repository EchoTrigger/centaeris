import json
from types import SimpleNamespace

from django.contrib.auth.models import User
from django.test import TestCase

from . import material_receipts as receipts
from .models import AgentRun, ModelConfig, SessionEvent, Workspace, WorkspaceMembership
from .testing import create_session


class MaterialReceiptTests(TestCase):
    def setUp(self):
        user = User.objects.create_user(username="receipt-owner")
        workspace = Workspace.objects.create(name="Receipts", createdBy=user)
        WorkspaceMembership.objects.create(workspace=workspace, user=user, role="owner")
        session = create_session(workspace=workspace, owner=user)
        model = ModelConfig.objects.create(id="receipt-model", displayName="Receipt")
        self.run = AgentRun.objects.create(workspace=workspace, session=session, user=user, modelConfig=model, prompt="read")
        self.access = SimpleNamespace(agent_run=self.run, authorization_digest="sha256:" + "a" * 64)
        self.arguments = {"input_ref": "input-1"}
        self.evidence = {"inputRef": "input-1", "ownerRef": "source-1", "ownerKind": "sourceObject",
                         "displayName": "Source", "evidenceKind": "workspaceSource", "ownerSha256": "sha256:" + "b" * 64,
                         "ownerGeneration": 1, "representationId": "representation:sha256:" + "c" * 64,
                         "specDigest": "sha256:" + "d" * 64, "evidenceSha256": "sha256:" + "e" * 64,
                         "locator": {"kind": "textSpan", "startLine": 1, "endLine": 1},
                         "citationAllowed": True, "content": "trusted evidence"}
        self.result = {"disposition": "ready", "items": [self.evidence]}

    def event(self, kind, payload, run=None):
        run = run or self.run
        sequence = SessionEvent.objects.count() + 1
        return SessionEvent.objects.create(eventId=f"event-{sequence}", workspace=run.workspace, session=run.session,
            agent_run=run, sequence=sequence, agent_run_sequence=sequence, projects_to_agent_run_stream=True,
            payload={"type": kind, "turnId": run.turn_id, "payload": payload}, createdAtMs=1)

    def call(self, provider=receipts.PROVIDER_ID, arguments=None, call_id="call-1"):
        return self.event("tool_call", {"callId": call_id, "toolName": "read_material", "providerId": provider,
            "normalizedInput": self.arguments if arguments is None else arguments})

    def persist(self, call_id="call-1", result=None):
        call = receipts.authorize_call(self.access, call_id, "read_material", self.arguments)
        return receipts.persist_receipt(self.access, call, "read_material", self.result if result is None else result)

    def finish(self, output, state="successWithOutput", call_id="call-1"):
        return self.event("tool_result", {"callId": call_id, "toolName": "read_material", "resultState": state,
            "modelContent": json.dumps(output), "outputComplete": True})

    def closure(self, call_id="call-1"):
        sequence = SessionEvent.objects.count() + 1
        return SessionEvent.objects.create(
            eventId=f"closure-{sequence}", workspace=self.run.workspace, session=self.run.session,
            agent_run=self.run, sequence=sequence, agent_run_sequence=None, session_level=True,
            projects_to_agent_run_stream=False,
            payload={"type": "tool_call_closure", "turnId": self.run.turn_id, "payload": {
                "callId": call_id, "toolName": "read_material", "resultState": "blocked",
                "modelContent": "closed before execution", "outputComplete": True,
                "recovery": "not_executed", "callEventId": None,
                "triggerAgentRunId": "agent_run_trigger", "triggerTurnId": "turn_trigger"}},
            createdAtMs=1,
        )

    def test_recovered_closure_does_not_grant_a_new_citation_projection(self):
        self.call()
        self.persist()
        # The old run ends with a session-level closure, not a durable tool_result.
        self.closure()
        self.assertEqual(receipts.citation_projections(self.run), [])

    def test_saved_result_survives_reload_and_only_delivered_ranges_are_cited(self):
        from unittest.mock import patch
        from .material_delivery import save_result, read_result
        from .models import MaterialResultSnapshot
        self.call()
        call = receipts.authorize_call(self.access, "call-1", "read_material", self.arguments)
        content = "界😀" * 40000
        item = {**self.evidence, "content": content,
                "locator": {"kind": "textSpan", "startByte": 0, "endByte": len(content.encode()), "startLine": 1, "endLine": 1}}
        page = save_result(self.access, call, {"disposition": "ready", "hits": [item]})
        output = receipts.persist_receipt(self.access, call, "read_material", page)
        self.assertLessEqual(len(json.dumps(output, ensure_ascii=False).encode()), 50 * 1024)
        self.assertEqual(MaterialResultSnapshot.objects.get().payload["hits"][0]["content"], content)
        self.assertEqual(receipts.citation_projections(self.run), [])
        self.finish(output)
        evidence = receipts.citation_projections(self.run)[0]
        self.assertEqual(evidence.evidenceSha256, output["hits"][0]["evidenceSha256"])
        self.assertLess(evidence.locator["endByte"], len(content.encode()))
        continuation = page["continuation"]["arguments"]
        self.access.spec_digest = "spec"
        with patch("app_core.material_access.bind_inputs") as bind:
            next_page = read_result(self.access, continuation["result_ref"], continuation["cursor"])
            bind.assert_called_once()
        self.assertEqual(next_page["hits"][0]["locator"]["startByte"], evidence.locator["endByte"])
        with patch("app_core.material_access.bind_inputs", side_effect=ValueError("revoked")):
            with self.assertRaisesRegex(ValueError, "revoked"):
                read_result(self.access, continuation["result_ref"], continuation["cursor"])

    def test_requires_committed_first_party_call_and_exact_arguments(self):
        with self.assertRaises(ValueError):
            self.persist()
        self.call(provider="external.mcp")
        with self.assertRaises(ValueError):
            self.persist()
        self.call(arguments={"input_ref": "other"}, call_id="call-2")
        with self.assertRaises(ValueError):
            self.persist("call-2")

    def test_snapshot_waits_for_success_and_publishes_without_terminal(self):
        self.client.force_login(self.run.user)
        url = f"/api/sessions/{self.run.session_id}/agent-runs/{self.run.id}/citations"
        self.call()
        output = self.persist()
        response = self.client.get(url)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        self.assertEqual(response.json()["citations"], [])
        result = self.finish(output)
        self.assertEqual(receipts.citation_projections(self.run, result.sequence - 1), [])
        snapshot = self.client.get(url).json()
        self.assertEqual(snapshot["throughSequence"], result.sequence)
        self.assertEqual(snapshot["citations"], [{"citationId": output["citationIds"][0], "inputRef": "input-1",
            "displayName": "Source", "sourceToolCallId": "call-1",
            "sourceUrl": f"/api/citations/{output['citationIds'][0]}"}])
        self.assertEqual(self.client.get(url).json(), snapshot)
        self.assertEqual(receipts.SessionCitationProjection.objects.filter(agent_run=self.run).count(), 1)
        self.assertEqual(SessionEvent.objects.filter(agent_run=self.run, payload__type="citation_recorded").count(), 0)

    def test_snapshot_rejects_foreign_session_and_revoked_membership(self):
        self.client.force_login(self.run.user)
        base = f"/api/sessions/{self.run.session_id}/agent-runs/{self.run.id}/citations"
        self.assertEqual(self.client.get(base.replace(self.run.session_id, "other-session")).status_code, 404)
        WorkspaceMembership.objects.filter(workspace=self.run.workspace, user=self.run.user).delete()
        self.assertEqual(self.client.get(base).status_code, 404)

    def test_receipt_is_durable_idempotent_and_waits_for_success(self):
        self.call()
        output = self.persist()
        self.assertEqual(self.persist(), output)
        self.assertEqual(receipts.MaterialEvidenceReceipt.objects.count(), 1)
        self.assertEqual(receipts.citation_projections(self.run), [])
        result_event = self.finish(output)
        projected = receipts.citation_projections(self.run)
        self.assertEqual(len(projected), 1)
        self.assertEqual(projected[0].sourceToolCallId, "call-1")
        self.assertEqual(projected[0].sequence, result_event.sequence)
        self.assertEqual(projected[0].locator, self.evidence["locator"])
        self.assertEqual(receipts.citation_projections(self.run)[0].citationId, projected[0].citationId)

    def test_failed_or_changed_result_never_publishes(self):
        for index, state in enumerate(["failed", "denied", "aborted", "successWithOutput"]):
            call_id = f"call-{index}"
            self.call(call_id=call_id)
            output = self.persist(call_id)
            if state == "successWithOutput":
                output = {**output, "items": []}
            self.finish(output, state, call_id)
        self.assertEqual(receipts.citation_projections(self.run), [])

    def test_pending_and_empty_evidence_do_not_create_receipts(self):
        self.call()
        for result in [{"disposition": "pending", "missing": []}, {"disposition": "ready", "items": []}]:
            self.assertEqual(self.persist(result=result), result)
        self.assertEqual(receipts.MaterialEvidenceReceipt.objects.count(), 0)

    def test_conflicting_replay_fails_and_external_copy_cannot_bind(self):
        self.call()
        output = self.persist()
        with self.assertRaises(ValueError):
            self.persist(result={**self.result, "extra": "changed"})
        self.call(provider="external.mcp", call_id="external-call")
        self.finish(output, call_id="external-call")
        self.assertEqual(receipts.citation_projections(self.run), [])

    def test_cross_run_and_wrong_turn_cannot_bind(self):
        self.call()
        output = self.persist()
        other = AgentRun.objects.create(workspace=self.run.workspace, session=self.run.session,
            user=self.run.user, modelConfig=self.run.modelConfig, prompt="other")
        with self.assertRaises(ValueError):
            receipts.authorize_call(SimpleNamespace(agent_run=other), "call-1", "read_material", self.arguments)
        event = self.finish(output)
        # Bypass append-only save only to simulate an invalid imported contract fixture.
        changed = {**event.payload, "turnId": "other-turn"}
        SessionEvent.objects.filter(pk=event.pk).update(payload=changed)
        self.assertEqual(receipts.citation_projections(self.run), [])

    def test_receipts_and_legacy_events_survive_projection_rebuild_together(self):
        from .session_event import rebuild_agent_run_citation_projection
        self.call()
        output = self.persist()
        self.finish(output)
        legacy = {field: self.evidence[field] for field in receipts.EVIDENCE_FIELDS}
        self.event("citation_recorded", {**legacy, "citationId": "citation:legacy",
            "sourceToolCallId": "legacy-call", "sourceToolName": "read"})
        for _ in range(2):
            projected = rebuild_agent_run_citation_projection(self.run)
            self.assertEqual({item.citationId for item in projected}, {"citation:legacy", output["citationIds"][0]})

    def test_explicit_history_deletion_removes_its_receipt(self):
        call = self.call()
        self.persist()
        SessionEvent.objects.filter(pk=call.pk).delete()
        self.assertFalse(receipts.MaterialEvidenceReceipt.objects.exists())

    def test_receipt_is_immutable_and_truncated_output_is_not_evidence(self):
        self.call()
        output = self.persist()
        receipt = receipts.MaterialEvidenceReceipt.objects.get()
        with self.assertRaises(ValueError):
            receipt.save()
        event = self.finish(output)
        changed = {**event.payload, "payload": {**event.payload["payload"], "outputComplete": False}}
        SessionEvent.objects.filter(pk=event.pk).update(payload=changed)
        self.assertEqual(receipts.citation_projections(self.run), [])
