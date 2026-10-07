"""Private coordination identity and committed message acceptance; synthetic only."""
import json
import hashlib
import tempfile

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import transaction
from django.test import TestCase, override_settings
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .assets import captured_input_fields
from .models import Agent, AgentCoordinationSession, AgentRun, ModelConfig, Session, SessionAssetLink, SessionEvent, Source, SourceGrant, SourceObject, UserLibraryObject, Workspace, WorkspaceGroup, WorkspaceMembership


class AgentMessageTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="message-owner")
        self.other = get_user_model().objects.create_user(username="message-other")
        self.workspace = Workspace.objects.create(name="Messages", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.other, role="admin")
        self.agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Private coordinator")
        self.work = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        self.binding_url = f"/api/agents/{self.agent.id}/coordination-session"
        self.messages_url = f"/api/agents/{self.agent.id}/messages"
        self.client.force_login(self.user)

    def bind(self):
        response = self.client.post(self.binding_url, data="{}", content_type="application/json")
        self.assertIn(response.status_code, (200, 201), response.content)
        self.assertEqual(set(response.json()), {"schema", "agentId", "sessionId"})
        self.assertEqual(response.json()["schema"], "agent.coordination_session.v1")
        self.assertEqual(response.json()["agentId"], self.agent.id)
        return Session.objects.get(pk=response.json()["sessionId"])

    def message_run(self, source):
        model = ModelConfig.objects.create(displayName="Synthetic message model")
        run = AgentRun.objects.create(workspace=self.workspace, session=source, user=self.user,
                                      modelConfig=model, prompt="Send and continue.", status="running")
        create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        return run

    def validate(self, run, **changes):
        body = {"schema": "workspace.agent_message.validate.v1", "agentRunId": run.id,
                "authorizationDigest": run.authorization.digest, "coordinationSessionId": run.session_id,
                "toolCallId": "send-one", "body": "  Complete text\nUnicode: 中文 🔎  ",
                "sessionRefs": [self.work.id], "fileRefs": []}
        body.update(changes)
        return self.client.post("/internal/agent-messages/validate", data=json.dumps(body),
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def commit(self, run, event_type, payload, *, call_id="send-one", turn_id=None):
        sequence = SessionEvent.objects.filter(session=run.session).count() + 1
        turn = turn_id or run.turn_id
        wire = {"schemaVersion": "session.event.v1", "eventId": f"synthetic-{run.id}-{sequence}",
                "sessionId": run.session_id, "agentRunId": run.id, "turnId": turn,
                "eventVersion": 1, "type": event_type, "createdAtMs": sequence, "sequence": sequence,
                "payload": {"callId": call_id, **payload}}
        return SessionEvent.objects.create(eventId=wire["eventId"], workspace=self.workspace,
            session=run.session, agent_run=run, sequence=sequence, agent_run_sequence=sequence,
            projects_to_agent_run_stream=True, createdAtMs=sequence, payload=wire)

    def accepted_pair(self, run, *, body=None, file_refs=None, call_changes=None, result_changes=None):
        from .agent_messages import message_contract_digest
        response = self.validate(run, fileRefs=file_refs or [], **({"body": body} if body is not None else {}))
        self.assertEqual(response.status_code, 200, response.content)
        args = {"body": body or "  Complete text\nUnicode: 中文 🔎  ",
                "session_refs": [self.work.id], "file_refs": file_refs or []}
        call = {"toolName": "send_message", "providerId": "workspace.agent_messages",
                "toolContractDigest": message_contract_digest(), "normalizedInput": args,
                "displayTitle": "short summary, never the message body"}
        call.update(call_changes or {})
        self.commit(run, "tool_call", call)
        result = {"toolName": "send_message", "resultState": "successWithOutput",
                  "modelContent": "accepted", "summary": "diagnostic, never the body"}
        result.update(result_changes or {})
        return self.commit(run, "tool_result", result)

    def messages(self):
        response = self.client.get(self.messages_url)
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()["messages"]

    def test_binding_creates_fresh_persistent_private_session_and_is_idempotent(self):
        before = list(Session.objects.values_list("id", "agent_id", "origin"))
        self.assertEqual(self.client.get(self.binding_url).status_code, 404)
        source = self.bind()
        self.assertNotEqual(source.id, self.work.id)
        self.assertEqual(self.bind().id, source.id)
        self.assertEqual(self.client.get(self.binding_url).json()["sessionId"], source.id)
        self.assertEqual(Session.objects.count(), 2)
        self.assertEqual(list(Session.objects.filter(id=self.work.id).values_list("id", "agent_id", "origin")), before)
        self.assertFalse(source.events.exists())
        self.assertFalse(AgentRun.objects.exists())
        # Ordinary work lists keep the dedicated coordination history out of the work UI.
        listing = self.client.get(f"/api/workspaces/{self.workspace.id}/sessions").json()["sessions"]
        self.assertEqual([item["id"] for item in listing], [self.work.id])
        filtered = self.client.get(f"/api/workspaces/{self.workspace.id}/sessions", {"agentId": self.agent.id}).json()["sessions"]
        self.assertEqual(filtered, listing)

    def test_binding_and_history_require_current_owner_authentication(self):
        self.bind()
        self.client.force_login(self.other)
        for method in (self.client.get, self.client.post):
            response = method(self.binding_url, **({"data": "{}", "content_type": "application/json"}
                                                  if method == self.client.post else {}))
            self.assertEqual(response.status_code, 404)
        self.assertEqual(self.client.get(self.messages_url).status_code, 404)
        self.client.logout()
        self.assertEqual(self.client.get(self.binding_url).status_code, 401)
        self.assertEqual(self.client.get(self.messages_url).status_code, 401)

    def test_binding_rejects_model_selected_session_and_legacy_fields(self):
        for fields in ({"sessionId": self.work.id}, {"recipientSessionId": self.work.id}, {"agent_id": self.agent.id}):
            response = self.client.post(self.binding_url, data=json.dumps(fields), content_type="application/json")
            self.assertEqual(response.status_code, 400, response.content)
        self.assertEqual(Session.objects.count(), 1)

    def test_existing_binding_cannot_be_reassigned_by_new_model_instance(self):
        source = self.bind()
        binding = AgentCoordinationSession.objects.get(agent=self.agent)
        replacement = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        candidate = AgentCoordinationSession(agent=self.agent, session=replacement, created_at=binding.created_at)
        with self.assertRaisesMessage(ValueError, "coordination_session_binding_is_immutable"):
            candidate.save()
        self.assertEqual(AgentCoordinationSession.objects.get(agent=self.agent).session_id, source.id)

    def test_history_cursor_rejects_unknown_aliases_and_repeated_fields(self):
        self.bind()
        for query in ("after_sequence=1", "recipientSessionId=unknown", "afterSequence=0&afterSequence=1",
                      "limit=1&limit=2", "afterSequence=-1", "limit=0", "limit=101"):
            with self.subTest(query=query):
                self.assertEqual(self.client.get(self.messages_url + "?" + query).status_code, 400)

    def test_validation_is_read_only_and_ordinary_session_cannot_send(self):
        source = self.bind()
        run = self.message_run(source)
        response = self.validate(run)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertFalse(source.events.exists())
        self.assertEqual(self.messages(), [])
        ordinary = self.message_run(self.work)
        self.assertEqual(self.validate(ordinary).status_code, 403)
        self.assertEqual(self.validate(run, coordinationSessionId=self.work.id).status_code, 403)
        self.assertEqual(self.validate(run, recipientSessionId=self.work.id).status_code, 400)

    def test_only_bound_coordination_run_start_exposes_hosted_message_context(self):
        from .runtime_client import build_agent_run_start
        source = self.bind()
        run = self.message_run(source)
        self.assertEqual(build_agent_run_start(run)["coordinationSessionId"], source.id)
        ordinary = self.message_run(self.work)
        self.assertNotIn("coordinationSessionId", build_agent_run_start(ordinary))

    def test_current_authority_and_resource_refs_are_rechecked_each_call(self):
        source = self.bind()
        run = self.message_run(source)
        self.assertEqual(self.validate(run).status_code, 200)
        self.work.status, self.work.deletedAt = "deleted", timezone.now()
        self.work.save()
        self.assertEqual(self.validate(run).status_code, 403)
        self.assertEqual(self.validate(run, sessionRefs=[], fileRefs=["unknown-file"]).status_code, 403)
        self.assertEqual(self.validate(run, sessionRefs=["unknown-session"]).status_code, 403)
        self.membership.delete()
        self.assertEqual(self.validate(run, sessionRefs=[]).status_code, 403)
        self.assertEqual(self.client.get(self.messages_url).status_code, 404)

    def test_source_deletion_and_authorization_tampering_reject_send(self):
        source = self.bind()
        run = self.message_run(source)
        run.authorization.payload["agentId"] = "unknown-agent"
        type(run.authorization).objects.filter(pk=run.authorization.pk).update(payload=run.authorization.payload)
        self.assertEqual(self.validate(run).status_code, 403)

    def test_committed_refs_do_not_preserve_current_session_or_file_access(self):
        from .tests import streaming_response_bytes

        self.membership.role = "member"
        self.membership.save(update_fields=["role"])
        ref_workspace = Workspace.objects.create(name="Associated work", createdBy=self.other)
        ref_membership = WorkspaceMembership.objects.create(workspace=ref_workspace, user=self.user, role="member")
        ref_agent = Agent.objects.create(workspace=ref_workspace, owner=self.user, name="Associated private Agent")
        self.work = Session.objects.create(workspace=ref_workspace, owner=self.user, agent=ref_agent)
        source = self.bind()
        with tempfile.TemporaryDirectory(prefix="message-current-refs-") as storage, override_settings(MEDIA_ROOT=storage):
            content = b"Current source evidence."
            key = default_storage.save("current-refs/evidence.txt", ContentFile(content))
            file_source = Source.objects.create(workspace=self.workspace, sourceType="fileTree",
                name="Evidence", status="ready", createdBy=self.other)
            item = SourceObject.objects.create(workspace=self.workspace, source=file_source, objectType="file",
                displayPath="evidence.txt", displayName="evidence.txt", contentType="text/plain",
                sizeBytes=len(content), sha256="sha256:" + hashlib.sha256(content).hexdigest(),
                storageKey=key, status="ready", contentGeneration=1)
            group = WorkspaceGroup.objects.create(workspace=self.workspace, name="Evidence readers", createdBy=self.other)
            group.members.add(self.membership)
            grant = SourceGrant.objects.create(workspace=self.workspace, source=file_source,
                workspaceGroup=group, createdBy=self.other)
            link = SessionAssetLink.objects.create(workspace=self.workspace, session=source, sourceObject=item,
                attachedBy=self.user, capturedDisplayName=item.displayName, capturedContentType=item.contentType,
                **captured_input_fields(item))
            run = self.message_run(source)
            accepted = self.accepted_pair(run, file_refs=[link.id])
            history = self.messages()
            self.assertEqual(history[0]["id"], accepted.eventId)
            self.assertEqual(history[0]["sessionRefs"], [self.work.id])
            self.assertEqual(history[0]["fileRefs"], [link.id])
            self.assertEqual(self.client.get(f"/api/sessions/{self.work.id}").status_code, 200)
            path = f"/api/source-objects/{item.id}/download"
            download = self.client.get(path)
            self.assertEqual(download.status_code, 200, download.content if not download.streaming else "")
            self.assertEqual(streaming_response_bytes(download), content)
            # Close storage resources without firing request_finished inside
            # TestCase's outer transaction and closing its database connection.
            for close_resource in download._resource_closers:
                close_resource()

            grant.delete()
            self.assertEqual(self.client.get(path).status_code, 404)
            self.assertEqual(self.messages(), history)
            self.assertEqual(self.validate(run, fileRefs=[link.id]).status_code, 403)
            self.assertEqual(self.validate(run, sessionRefs=[], fileRefs=[]).status_code, 200)

            ref_membership.delete()
            self.assertEqual(self.client.get(f"/api/sessions/{self.work.id}").status_code, 404)
            self.assertEqual(self.messages(), history)
            self.assertEqual(self.validate(run).status_code, 403)
            self.assertEqual(self.validate(run, sessionRefs=[], fileRefs=[]).status_code, 200)

    def test_owned_files_outside_signed_source_inputs_do_not_gain_message_access(self):
        source = self.bind()
        with tempfile.TemporaryDirectory(prefix="message-input-scope-") as storage, override_settings(MEDIA_ROOT=storage):
            content = b"Owned synthetic attachment."
            key = default_storage.save("report.txt", ContentFile(content))
            library = UserLibraryObject.objects.create(owner=self.user, displayName="report.txt", objectKind="file",
                contentType="text/plain", sizeBytes=len(content), sha256="sha256:" + hashlib.sha256(content).hexdigest(),
                contentGeneration=1, storageKey=key, status="ready")

            def attach(session):
                return SessionAssetLink.objects.create(workspace=self.workspace, session=session,
                    userLibraryObject=library, attachedBy=self.user, capturedDisplayName=library.displayName,
                    capturedContentType=library.contentType, capturedOwnerKind="userLibraryObject",
                    capturedOwnerId=library.id, capturedContentGeneration=1,
                    capturedSizeBytes=library.sizeBytes, capturedSha256=library.sha256)

            work_link = attach(self.work)
            run = self.message_run(source)
            late_source_link = attach(source)
            for file_ref in (library.id, work_link.id, late_source_link.id):
                with self.subTest(file_ref=file_ref):
                    self.assertEqual(self.validate(run, fileRefs=[file_ref]).status_code, 403)
            newly_authorized = self.message_run(source)
            self.assertEqual(self.validate(newly_authorized, fileRefs=[late_source_link.id]).status_code, 200)
            self.assertEqual(self.messages(), [])

    def test_committed_success_projects_full_body_refs_with_stable_id_and_no_final_bubble(self):
        source = self.bind()
        run = self.message_run(source)
        body = "  中文 🔎\n" + "complete evidence\n" * 3000 + "Tail  "
        result = self.accepted_pair(run, body=body)
        messages = self.messages()
        self.assertEqual(len(messages), 1)
        self.assertEqual(messages[0]["body"], body)
        self.assertEqual(messages[0]["sessionRefs"], [self.work.id])
        self.assertEqual(messages[0]["fileRefs"], [])
        self.assertEqual(messages[0]["id"], result.eventId)
        self.assertEqual(self.messages(), messages)
        self.commit(run, "assistant_message", {"modelMarkdown": "sole Final", "status": "done"})
        self.assertEqual(self.messages(), messages)
        self.assertFalse(self.work.events.exists())

    def test_uncommitted_failed_forged_wrong_identity_results_never_project(self):
        source = self.bind()
        run = self.message_run(source)
        self.validate(run)
        self.assertEqual(self.messages(), [])
        with transaction.atomic():
            self.accepted_pair(run)
            transaction.set_rollback(True)
        self.assertEqual(self.messages(), [])
        cases = [({"providerId": "test.messages"}, {}),
                 ({"toolContractDigest": "sha256:" + "f" * 64}, {}),
                 ({"normalizedInput": {"body": "", "session_refs": [], "file_refs": []}}, {}),
                 ({}, {"resultState": "error"}),
                 ({}, {"toolName": "other_tool"}),
                 ({}, {"resultState": "failure", "summary": "accepted"})]
        for call_changes, result_changes in cases:
            with self.subTest(call=call_changes, result=result_changes), transaction.atomic():
                self.accepted_pair(run, call_changes=call_changes, result_changes=result_changes)
                self.assertEqual(self.messages(), [])
                transaction.set_rollback(True)

    def test_message_body_uses_utf8_budget_and_refs_are_strict(self):
        source = self.bind()
        run = self.message_run(source)
        for changes in ({"body": "  "}, {"body": "中" * 21846}, {"sessionRefs": [self.work.id] * 9},
                        {"body": 1}, {"session_refs": []}, {"fileRefs": ["/mnt/data/report.txt"]}):
            with self.subTest(changes=changes):
                self.assertEqual(self.validate(run, **changes).status_code, 400)

    def test_committed_projection_rejects_mismatched_source_record_identities(self):
        source = self.bind()
        run = self.message_run(source)
        for event_type in ("tool_call", "tool_result"):
            for field, value in (("sessionId", self.work.id), ("agentRunId", "other-run"),
                                 ("eventId", "other-event"), ("sequence", 999), ("turnId", "other-turn")):
                with self.subTest(event_type=event_type, field=field), transaction.atomic():
                    result = self.accepted_pair(run)
                    record = result if event_type == "tool_result" else source.events.get(payload__type="tool_call")
                    wire = record.payload.copy()
                    wire[field] = value
                    SessionEvent.objects.filter(pk=record.pk).update(payload=wire)
                    self.assertEqual(self.messages(), [])
                    transaction.set_rollback(True)
        with transaction.atomic():
            result = self.accepted_pair(run)
            SessionEvent.objects.filter(pk=result.pk).update(projects_to_agent_run_stream=False)
            self.assertEqual(self.messages(), [])
            transaction.set_rollback(True)
