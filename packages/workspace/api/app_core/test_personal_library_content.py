"""Non-attached personal content through real authorization, processing and evidence."""

import json
from io import BytesIO
from tempfile import TemporaryDirectory
from uuid import uuid4

from asgiref.sync import async_to_sync
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import transaction
from django.db.models import Max
from django.test import TestCase, override_settings

from . import material_leases, platform_mcp
from .agent_run_authorization_factory import create_agent_run_authorization
from .assets import tombstone_stored_object
from .library_materials import library_descriptor
from .library_notes import save_note
from .material_access import MaterialAccessContext
from .material_contract import KnowledgeError, sha256_bytes
from .models import (
    AgentRun, DerivedRepresentation, MaterialEvidenceReceipt, MaterialOperation,
    MaterialProcessingTask, MaterialResultSnapshot, SessionAssetLink, SessionEvent,
    UserLibraryObject, WorkspaceMembership,
)
from .platform_material_commit import ProcessingOutput, commit_processing_output
from .test_personal_library_materials import PersonalLibraryMaterialFixture
from .testing import create_session


class PersonalLibraryContentTests(PersonalLibraryMaterialFixture, TestCase):
    def setUp(self):
        temporary = TemporaryDirectory(prefix="personal-library-content-")
        self.addCleanup(temporary.cleanup)
        storage = override_settings(MEDIA_ROOT=temporary.name)
        storage.enable()
        self.addCleanup(storage.disable)
        super().setUp()

    def event(self, run, kind, payload):
        sequence = (SessionEvent.objects.filter(session_id=run.session_id).aggregate(value=Max("sequence"))["value"] or 0) + 1
        run_sequence = (SessionEvent.objects.filter(agent_run=run).aggregate(value=Max("agent_run_sequence"))["value"] or 0) + 1
        return SessionEvent.objects.create(
            eventId="content-event-" + uuid4().hex, workspace=run.workspace, session=run.session,
            agent_run=run, sequence=sequence, agent_run_sequence=run_sequence,
            projects_to_agent_run_stream=True, createdAtMs=sequence,
            payload={"type": kind, "turnId": run.turn_id, "payload": payload},
        )

    def invoke(self, run, name, arguments):
        platform_mcp.VALIDATORS[name].validate(arguments)
        call = self.event(run, "tool_call", {
            "providerId": "workspace.materials", "toolName": name,
            "callId": uuid4().hex, "normalizedInput": arguments,
        })
        return self.execute(run, call), call

    def execute(self, run, call):
        access = self.access(run)
        context = MaterialAccessContext(run.pk, access.authorization_digest,
                                       access.processing_specification, access.spec_digest)
        payload = call.payload["payload"]
        marker = platform_mcp.request_call_id.set(payload["callId"])
        try:
            return platform_mcp.execute_tool(context, payload["toolName"], payload["normalizedInput"])
        finally:
            platform_mcp.request_call_id.reset(marker)

    def finish(self, run, call, output):
        return self.event(run, "tool_result", {
            "callId": call.payload["payload"]["callId"],
            "toolName": call.payload["payload"]["toolName"],
            "resultState": "successWithOutput", "modelContent": json.dumps(output), "outputComplete": True,
        })

    def publish(self, operation_id, content):
        task = MaterialOperation.objects.select_related("task").get(pk=operation_id).task
        lease = material_leases.claim(task.pk, "personal-content-fixture", 60)
        self.assertIsNotNone(lease)
        raw = content.encode("utf-8")
        digest = sha256_bytes(raw)
        # Same single-native-page manifest shape used by platform commit tests.
        manifest = {"schema": "knowledge.derived_manifest.v1", "pageCount": 1, "pages": [{
            "pageText": {"schema": "knowledge.page_text.v1", "page": 1, "route": "nativeText",
                         "widthMillipoints": 1000, "heightMillipoints": 1000,
                         "text": content, "textSha256": digest,
                         "spans": [{"text": content, "bbox": [0, 0, 10000, 10000]}]},
            "canonicalStartByte": 0, "canonicalEndByte": len(raw),
            "canonicalStartLine": 1, "canonicalEndLine": 1 + content.count("\n"),
        }]}
        output = ProcessingOutput(task.pk, len(raw), digest, 0, None, manifest)
        commit_processing_output(lease, output, BytesIO(raw))
        task.refresh_from_db()
        self.assertEqual(task.status, "completed")
        return DerivedRepresentation.objects.get(pk=task.pk)

    def ready(self, owner, content):
        run = self.new_run()
        ref = library_descriptor(run, owner)["inputRef"]
        arguments = {"input_ref": ref, "delivery": "content"}
        pending, pending_call = self.invoke(run, "read_material", arguments)
        self.assertEqual(pending["disposition"], "pending")
        operation_id = pending["operations"][0]["operationId"]
        self.assertEqual(MaterialProcessingTask.objects.count(), 1)
        self.assertEqual(MaterialOperation.objects.get(pk=operation_id).agent_run_id, run.pk)
        self.assertFalse(MaterialResultSnapshot.objects.exists())
        self.assertFalse(MaterialEvidenceReceipt.objects.exists())
        self.finish(run, pending_call, pending)
        status, status_call = self.invoke(run, "get_operation", {"operation_id": operation_id})
        self.assertEqual(status["status"], "pending")
        self.finish(run, status_call, status)
        representation = self.publish(operation_id, content)
        status, status_call = self.invoke(run, "get_operation", {"operation_id": operation_id})
        self.assertEqual(status["status"], "completed")
        self.finish(run, status_call, status)
        page, call = self.invoke(run, "read_material", arguments)
        self.assertEqual(page["disposition"], "ready")
        self.assertEqual(page["items"][0]["representationId"], representation.pk)
        self.assertFalse(SessionAssetLink.objects.exists())
        return run, page, call, operation_id

    def snapshot(self, run):
        response = self.client.get(f"/api/sessions/{run.session_id}/agent-runs/{run.pk}/citations")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.headers["Cache-Control"], "no-store")
        return response.json()

    def test_note_processing_pages_only_publish_committed_delivered_evidence(self):
        content = "界😀" * 20000 + "\nfinal line"
        note = self.new_note("No suffix", content)
        run, page, call, _operation = self.ready(note, content)
        self.assertEqual(call.payload["payload"]["normalizedInput"]["delivery"], "content")
        self.client.force_login(self.user)
        self.assertEqual(self.snapshot(run)["citations"], [])
        self.assertEqual(MaterialResultSnapshot.objects.get(call=call).payload["items"][0]["content"], content)
        receipt_count = MaterialEvidenceReceipt.objects.count()
        self.assertEqual(self.execute(run, call), page)
        self.assertEqual(MaterialEvidenceReceipt.objects.count(), receipt_count)
        chunks, citation_ids = [], []
        while True:
            item = page["items"][0]
            self.assertEqual(item["locator"]["startByte"], len("".join(chunks).encode("utf-8")))
            self.assertEqual(item["evidenceSha256"], sha256_bytes(item["content"].encode("utf-8")))
            chunks.append(item["content"])
            citation_ids.extend(page["citationIds"])
            self.finish(run, call, page)
            snapshot = self.snapshot(run)
            self.assertEqual({c["citationId"] for c in snapshot["citations"]}, set(citation_ids))
            self.assertEqual(len(snapshot["citations"]), len(citation_ids))
            self.assertEqual(self.snapshot(run), snapshot)
            continuation = page["continuation"]
            if continuation is None:
                break
            self.assertEqual(continuation["tool"], "read_material_result")
            previous_citations = snapshot["citations"]
            page, call = self.invoke(run, continuation["tool"], continuation["arguments"])
            self.assertEqual(self.snapshot(run)["citations"], previous_citations)
        self.assertGreater(len(chunks), 1)
        self.assertEqual("".join(chunks), content)
        citation = self.snapshot(run)["citations"][0]
        detail = self.client.get(citation["sourceUrl"])
        self.assertEqual(detail.status_code, 200)
        preview = self.client.get(detail.json()["citation"]["previewUrl"])
        self.assertEqual(preview.status_code, 200)

        async def read_preview():
            return b"".join([chunk async for chunk in preview.streaming_content])

        self.assertEqual(async_to_sync(read_preview)().decode("utf-8"), content)

    def test_unattached_file_uses_the_same_processing_and_receipt_path(self):
        content = "file evidence 第一行\nsecond line"
        raw = content.encode("utf-8")
        key = default_storage.save("synthetic-original.txt", ContentFile(raw))
        owner = UserLibraryObject.objects.create(owner=self.user, objectKind="file", displayName="Original.txt",
            contentType="text/plain", sizeBytes=len(raw), sha256=sha256_bytes(raw), storageKey=key,
            contentGeneration=1, status="ready")
        run, page, call, _operation = self.ready(owner, content)
        self.assertEqual(page["items"][0]["content"], content)
        self.finish(run, call, page)
        self.client.force_login(self.user)
        self.assertEqual(len(self.snapshot(run)["citations"]), 1)

    def long_note_page(self):
        note = self.new_note("Versioned", "正文😀" * 20000)
        run, page, call, operation = self.ready(note, note.noteHead.markdown)
        self.finish(run, call, page)
        self.assertIsNotNone(page["continuation"])
        return note, run, page, operation

    def assert_old_content_rejected(self, run, page, operation, code):
        for name, arguments in (("read_material_result", page["continuation"]["arguments"]),
                                ("get_operation", {"operation_id": operation})):
            with self.subTest(tool=name), self.assertRaises(KnowledgeError) as caught:
                self.invoke(run, name, arguments)
            self.assertEqual(caught.exception.code, code)

    def test_note_update_revokes_old_result_operation_and_citation_preview(self):
        note, run, page, operation = self.long_note_page()
        self.client.force_login(self.user)
        citation = self.snapshot(run)["citations"][0]
        save_note(self.user, note.pk, save_id=uuid4().hex, base_generation=note.contentGeneration,
                  display_name=note.displayName, markdown="new version")
        self.assert_old_content_rejected(run, page, operation, "stale_generation")
        response = self.client.get(citation["sourceUrl"] + "/preview")
        self.assertEqual(response.status_code, 409)
        self.assertEqual(response.json()["error"], "citation_source_stale")

    def test_deleted_and_restored_note_does_not_reauthorize_the_old_result(self):
        note, run, page, operation = self.long_note_page()
        self.client.force_login(self.user)
        citation = self.snapshot(run)["citations"][0]
        old_ref = page["items"][0]["inputRef"]
        tombstone_stored_object(note, deleted_by=self.user)
        self.assert_old_content_rejected(run, page, operation, "asset_removed")
        self.assertEqual(self.client.get(citation["sourceUrl"] + "/preview").status_code, 404)
        response = self.client.post(f"/api/library/{note.pk}/restore")
        self.assertEqual(response.status_code, 200, response.content)
        self.assert_old_content_rejected(run, page, operation, "stale_generation")
        note.refresh_from_db()
        self.assertNotEqual(library_descriptor(run, note)["inputRef"], old_ref)
        self.assertEqual(self.client.get(citation["sourceUrl"] + "/preview").status_code, 409)

    def separate_run(self, user):
        session = create_session(workspace=self.workspace, owner=user)
        run = AgentRun.objects.create(workspace=self.workspace, session=session, user=user,
                                      modelConfig=self.model, prompt="Other content reader")
        with transaction.atomic():
            create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64, personal_library=True)
        return run

    def test_result_scope_is_current_run_authority_not_a_session_or_cross_user_grant(self):
        _note, run, page, operation = self.long_note_page()
        continuation = page["continuation"]["arguments"]
        no_scope = self.new_run(personal=False)
        with self.assertRaises(KnowledgeError) as denied:
            self.invoke(no_scope, "read_material_result", continuation)
        self.assertEqual(denied.exception.code, "knowledge_input_not_authorized")
        other_session = self.separate_run(self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.other, role="member")
        other_user = self.separate_run(self.other)
        for reader in (other_session, other_user):
            with self.subTest(user=reader.user_id), self.assertRaises(KnowledgeError) as denied:
                self.invoke(reader, "read_material_result", continuation)
            self.assertEqual(denied.exception.code, "material_result_not_authorized")
            with self.assertRaises(KnowledgeError) as denied:
                self.invoke(reader, "get_operation", {"operation_id": operation})
            self.assertEqual(denied.exception.code, "material_operation_not_found")
        with self.assertRaises(KnowledgeError) as denied:
            self.invoke(other_user, "read_material", {"input_ref": page["items"][0]["inputRef"], "delivery": "content"})
        self.assertEqual(denied.exception.code, "asset_unavailable")
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(f"/api/sessions/{run.session_id}/agent-runs/{run.pk}/citations").status_code, 404)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).delete()
        with self.assertRaises(platform_mcp.CredentialRejected):
            self.invoke(run, "read_material_result", continuation)
