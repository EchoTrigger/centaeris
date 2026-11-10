"""Run authority, Session original identity, and Host delivery lifecycle contracts."""
import copy
import json
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from uuid import uuid4
from unittest.mock import patch
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier

from django.conf import settings
from django.contrib.auth.models import User
from django.core.files.storage import default_storage
from django.db import IntegrityError, transaction, close_old_connections
from django.test import TestCase, TransactionTestCase, override_settings
from django.db.migrations.executor import MigrationExecutor
from asgiref.sync import async_to_sync

from .agent_run_authorization_factory import create_agent_run_authorization
from .assets import captured_input_fields, DeferredInputResolutionError, store_bytes, tombstone_stored_object
from .deferred_input import resolved_input_storage
from .library_materials import library_descriptor
from .library_notes import create_note, save_note
from .material_contract import KnowledgeError
from .material_discovery import list_materials
from .material_files import material_context, prepare_original, confirm_original, reserved_original_bytes
from .material_identity import processing_spec_digest
from .models import (AgentRun, LibraryNoteRevision, ModelConfig, RunMaterialAdmission,
                     SessionAssetLink, SessionEvent, SessionMaterialInput, Workspace, WorkspaceMembership, UserLibraryObject)
from .test_platform_mcp import processing_specification
from .testing import create_session, leased_lifecycle_job_fixture
from .migration_testing import isolated_migration_database


class PersonalLibraryMaterialFixture:
    def setUp(self):
        self.user = User.objects.create_user(username="personal-material-owner")
        self.other = User.objects.create_user(username="personal-material-other")
        self.workspace = Workspace.objects.create(name="Materials", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.session = create_session(workspace=self.workspace, owner=self.user)
        self.model = ModelConfig.objects.create(displayName="Material model")
        self.note = self.new_note("Report", "exact original\r\n正文")

    def new_note(self, name, text, owner=None, parent=""):
        result = create_note(owner or self.user, save_id=uuid4().hex, display_name=name,
                             markdown=text, parent_folder_id=parent)
        return UserLibraryObject.objects.get(pk=result["object"]["id"])

    def new_run(self, personal=True, message_refs=None):
        run = AgentRun.objects.create(workspace=self.workspace, session=self.session,
                                      user=self.user, modelConfig=self.model, prompt="Use materials")
        with transaction.atomic():
            create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64,
                personal_library=personal, message_asset_refs=message_refs)
        return run

    def access(self, run):
        spec = processing_specification()
        return SimpleNamespace(agent_run=run, authorization_digest=run.authorization.digest,
                               processing_specification=spec, spec_digest=processing_spec_digest(spec))

    def call(self, run, ref):
        call_id = uuid4().hex
        sequence = SessionEvent.objects.filter(session=self.session).count() + 1
        SessionEvent.objects.create(eventId="file-call-" + call_id, workspace=self.workspace,
            session=self.session, agent_run=run, sequence=sequence, agent_run_sequence=sequence,
            projects_to_agent_run_stream=True, createdAtMs=sequence,
            payload={"type": "tool_call", "turnId": run.turn_id, "payload": {
                "providerId": "workspace.materials", "toolName": "read_material", "callId": call_id,
                "normalizedInput": {"input_ref": ref, "delivery": "execution_file"}}})
        return call_id

    def attach(self, owner):
        return SessionAssetLink.objects.create(workspace=self.workspace, session=self.session,
            attachedBy=self.user, userLibraryObject=owner, capturedDisplayName=owner.displayName,
            capturedContentType=owner.contentType, **captured_input_fields(owner))

    def prepare(self, run, owner=None):
        ref = library_descriptor(run, owner or self.note)["inputRef"]
        call_id = self.call(run, ref)
        result = prepare_original(run.pk, run.authorization.digest, ref, call_id)
        return result["resolvedInput"], call_id

    def internal(self, path, body):
        return self.client.post("/internal/agent-runs/materials/" + path,
            data=json.dumps(body), content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)


class PersonalLibraryMaterialTests(PersonalLibraryMaterialFixture, TestCase):
    def test_discovery_is_read_only_metadata_paginated_and_owner_scoped(self):
        self.new_note("Report", "body needle")
        self.new_note("Other", "Report")
        self.new_note("Report foreign", "secret", owner=self.other)
        run = self.new_run()
        first = list_materials(self.access(run), scope="library", query="Report", limit=1)
        self.assertEqual(len(first["items"]), 1)
        self.assertTrue(first["items"][0]["canFetchOriginal"])
        self.assertEqual(first["items"][0]["contentState"], "unknown")
        second = list_materials(self.access(run), scope="library", query="Report", limit=1, cursor=first["nextCursor"])
        self.assertNotEqual(first["items"][0]["inputRef"], second["items"][0]["inputRef"])
        self.assertIsNone(second["nextCursor"])
        self.assertEqual(list_materials(self.access(run), scope="library", query="needle")["items"], [])
        self.assertFalse(SessionMaterialInput.objects.exists())
        self.assertFalse(RunMaterialAdmission.objects.exists())
        with self.assertRaises(KnowledgeError):
            list_materials(self.access(run), scope="library", query="Other", cursor=first["nextCursor"])

    def test_old_scope_all_contains_only_attachments_and_signed_history_does_not_change(self):
        link = self.attach(self.note)
        old = self.new_run(personal=False)
        frozen = copy.deepcopy(old.authorization.payload), old.authorization.digest, old.authorization.signature
        listed = list_materials(self.access(old), scope="all")
        self.assertEqual([item["inputRef"] for item in listed["items"]], [link.pk])
        self.assertEqual(listed["items"][0]["sources"], ["attached"])
        with self.assertRaises(KnowledgeError):
            list_materials(self.access(old), scope="library")
        self.new_run()
        old.authorization.refresh_from_db()
        self.assertEqual((old.authorization.payload, old.authorization.digest, old.authorization.signature), frozen)

    def test_attached_and_library_discovery_never_select_or_open_original_bodies(self):
        self.attach(self.note)
        run = self.new_run()
        with patch("app_core.library_materials.content_source", side_effect=AssertionError("metadata selected original content")), \
                patch("app_core.note_content.NoteRevision.open", side_effect=AssertionError("metadata opened note body")), \
                patch("app_core.note_content.StoredObject.open", side_effect=AssertionError("metadata opened stored original")):
            for scope in ("attached", "library", "all"):
                with self.subTest(scope=scope):
                    self.assertEqual(len(list_materials(self.access(run), scope=scope)["items"]), 1)
        self.assertFalse(SessionMaterialInput.objects.exists())

    def test_library_first_then_attachment_uses_one_ref_and_path_across_runs(self):
        run = self.new_run()
        original, call_id = self.prepare(run)
        pending = material_context(run.pk, run.authorization.digest)
        self.assertEqual(pending["inputs"], [])
        self.assertEqual(pending["pendingInputs"][0]["resolvedInput"], original)
        self.assertNotIn("citationIds", pending)
        confirmed = confirm_original(run.pk, run.authorization.digest, original["inputRef"],
                                     original["sourceVersion"], original["sha256"], call_id)
        self.assertEqual(confirmed["resolvedInput"], original)
        link = self.attach(self.note)
        next_run = self.new_run(message_refs=[link.pk])
        self.assertEqual(next_run.authorization.payload["assetRefs"][0]["inputRef"], original["inputRef"])
        self.assertEqual(next_run.authorization.payload["messageAssetRefs"], [original["inputRef"]])
        self.assertEqual(material_context(next_run.pk, next_run.authorization.digest)["inputs"], [original])
        resolved, source = resolved_input_storage(next_run, original["inputRef"], next_run.authorization.digest)
        self.assertEqual(resolved, original)
        with source.open() as handle:
            self.assertEqual(handle.read(), "exact original\r\n正文".encode())
        self.assertEqual(reserved_original_bytes(next_run), 0)  # already budgeted as an explicit attachment

    def test_attachment_first_then_library_fetch_reuses_declared_identity(self):
        link = self.attach(self.note)
        run = self.new_run(message_refs=[link.pk])
        self.assertFalse(SessionMaterialInput.objects.exists())
        merged = list_materials(self.access(run), scope="all")
        self.assertEqual(len(merged["items"]), 1)
        self.assertEqual(merged["items"][0]["inputRef"], link.pk)
        self.assertEqual(merged["items"][0]["sources"], ["attached", "library"])
        original, _call = self.prepare(run)
        self.assertEqual(original["inputRef"], link.pk)
        self.assertEqual(SessionMaterialInput.objects.count(), 1)
        link.delete()
        later = self.new_run()
        self.assertEqual(library_descriptor(later, self.note)["inputRef"], original["inputRef"])
        resolved, _source = resolved_input_storage(later, original["inputRef"], later.authorization.digest)
        self.assertEqual(resolved, original)

    def test_prepare_is_not_delivery_and_retries_preserve_reservation_after_call_history_deletion(self):
        run = self.new_run()
        original, call_id = self.prepare(run)
        again = prepare_original(run.pk, run.authorization.digest, original["inputRef"], call_id)
        self.assertEqual(again["resolvedInput"], original)
        self.assertFalse(SessionMaterialInput.objects.get().confirmed)
        self.assertEqual(RunMaterialAdmission.objects.count(), 1)
        before = reserved_original_bytes(run)
        SessionEvent.objects.get(payload__payload__callId=call_id).delete()
        admission = RunMaterialAdmission.objects.get()
        self.assertIsNone(admission.origin_call_id)
        self.assertEqual(reserved_original_bytes(run), before)
        context = material_context(run.pk, run.authorization.digest)
        self.assertIsNone(context["pendingInputs"][0]["reservations"][0]["callId"])

    def test_download_requires_preparation_and_fresh_version_and_membership(self):
        run = self.new_run()
        ref = library_descriptor(run, self.note)["inputRef"]
        request = {"schema": "runtime.deferred_input.read.v1", "agentRunId": run.pk,
            "authorizationDigest": run.authorization.digest, "inputRef": ref,
            "sourceVersion": "1", "sha256": self.note.sha256}
        response = self.client.post("/internal/agent-runs/read-input", data=json.dumps(request),
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(response.status_code, 409)
        original, call_id = self.prepare(run)
        save_note(self.user, self.note.pk, save_id=uuid4().hex, base_generation=1, display_name="Report", markdown="updated")
        with self.assertRaises(DeferredInputResolutionError) as stale:
            confirm_original(run.pk, run.authorization.digest, ref, "1", original["sha256"], call_id)
        self.assertEqual(str(stale.exception), "stale_generation")
        state = material_context(run.pk, run.authorization.digest)["pendingInputs"][0]["state"]
        self.assertEqual(state, "stale_generation")
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).delete()
        with self.assertRaises(KnowledgeError):
            material_context(run.pk, run.authorization.digest)

    def test_download_streams_original_bytes_and_does_not_create_citations(self):
        run = self.new_run()
        original, _call_id = self.prepare(run)
        response = self.client.post("/internal/agent-runs/read-input", data=json.dumps({
            "schema": "runtime.deferred_input.read.v1", "agentRunId": run.pk,
            "authorizationDigest": run.authorization.digest, "inputRef": original["inputRef"],
            "sourceVersion": original["sourceVersion"], "sha256": original["sha256"]}),
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response["X-Content-Sha256"], original["sha256"])
        self.assertEqual(response["X-Source-Version"], "1")

        async def consume():
            return b"".join([chunk async for chunk in response.streaming_content])

        self.assertEqual(async_to_sync(consume)(), "exact original\r\n正文".encode())
        from .models import MaterialEvidenceReceipt, SessionCitationProjection
        self.assertFalse(MaterialEvidenceReceipt.objects.exists())
        self.assertFalse(SessionCitationProjection.objects.exists())

    def test_confirmation_checks_backing_file_after_prepare(self):
        storage = self.enterContext(TemporaryDirectory(prefix="material-confirmation-"))
        self.enterContext(override_settings(MEDIA_ROOT=storage))
        metadata = store_bytes(b"fixture body", "library", "original.txt", "text/plain")
        metadata.pop("safeFilename")
        owner = UserLibraryObject.objects.create(
            owner=self.user, objectKind="file", status="ready", contentGeneration=1, **metadata,
        )
        link = self.attach(owner)
        run = self.new_run(personal=False, message_refs=[link.pk])
        original, call_id = self.prepare(run, owner)
        self.assertEqual(original["inputRef"], link.pk)
        default_storage.delete(owner.storageKey)

        context = material_context(run.pk, run.authorization.digest)
        self.assertEqual(context["pendingInputs"][0]["state"], "active")
        with self.assertRaisesMessage(DeferredInputResolutionError, "asset_unavailable"):
            confirm_original(run.pk, run.authorization.digest, original["inputRef"],
                             original["sourceVersion"], original["sha256"], call_id)
        self.assertFalse(RunMaterialAdmission.objects.get().confirmed)
        self.assertFalse(SessionMaterialInput.objects.get().confirmed)

    def test_corrupt_replacement_cannot_change_note_before_confirmation(self):
        run = self.new_run()
        original, call_id = self.prepare(run)
        head = self.note.noteHead
        with self.assertRaisesMessage(IntegrityError, "note_revision_body_identity"), transaction.atomic():
            UserLibraryObject.objects.filter(pk=self.note.pk).update(noteHead=None)
            head.delete()
            LibraryNoteRevision.objects.create(
                note=self.note, generation=head.generation, displayName=head.displayName,
                markdown=head.markdown.replace("exact", "wrong"), sha256=head.sha256,
                sizeBytes=head.sizeBytes, createdBy=self.user,
            )

        context = material_context(run.pk, run.authorization.digest)
        self.assertEqual(context["pendingInputs"][0]["state"], "active")
        confirmed = confirm_original(run.pk, run.authorization.digest, original["inputRef"],
                                     original["sourceVersion"], original["sha256"], call_id)
        self.assertEqual(confirmed["resolvedInput"], original)

    def test_deleted_and_restored_library_object_does_not_revive_an_old_input_version(self):
        run = self.new_run()
        original, _call = self.prepare(run)
        tombstone_stored_object(self.note, deleted_by=self.user)
        self.assertEqual(material_context(run.pk, run.authorization.digest)["pendingInputs"][0]["state"], "asset_removed")
        self.client.force_login(self.user)
        restored = self.client.post(f"/api/library/{self.note.pk}/restore")
        self.assertEqual(restored.status_code, 200, restored.content)
        self.assertEqual(material_context(run.pk, run.authorization.digest)["pendingInputs"][0]["state"], "stale_generation")
        self.note.refresh_from_db()
        self.assertNotEqual(library_descriptor(run, self.note)["inputRef"], original["inputRef"])

    def test_reconcile_requires_live_execute_lease_and_settles_only_observed_admission(self):
        run = self.new_run()
        original, _call = self.prepare(run)
        pending = material_context(run.pk, run.authorization.digest)["pendingInputs"][0]
        owner = leased_lifecycle_job_fixture(self, run)
        body = {"schema": "workspace.material.reconcile.v1", "agentRunId": run.pk,
                "authorizationDigest": run.authorization.digest, "jobId": "agent_run.lifecycle:" + run.pk,
                "leaseOwner": owner, "inputs": [{"inputRef": original["inputRef"],
                    "sourceVersion": original["sourceVersion"], "sha256": original["sha256"], "state": "absent",
                    "admissionIds": [item["admissionId"] for item in pending["reservations"]]}]}
        other_call = self.call(run, original["inputRef"])
        prepare_original(run.pk, run.authorization.digest, original["inputRef"], other_call)
        rejected = self.internal("reconcile", {**body, "leaseOwner": "worker:wrong-owner"})
        self.assertEqual(rejected.status_code, 409)
        self.assertFalse(RunMaterialAdmission.objects.filter(settled_absent=True).exists())
        settled = self.internal("reconcile", body)
        self.assertEqual(settled.status_code, 200, settled.content)
        self.assertEqual(RunMaterialAdmission.objects.filter(settled_absent=True).count(), 1)
        self.assertEqual(RunMaterialAdmission.objects.filter(settled_absent=False).count(), 1)
        self.assertEqual(reserved_original_bytes(run), original["sizeBytes"])

    def test_invalid_confirmed_input_is_settled_without_pending_reservations_or_new_evidence(self):
        run = self.new_run()
        original, call_id = self.prepare(run)
        confirmed = self.internal("confirm", {
            "schema": "workspace.material.confirm.v1", "agentRunId": run.pk,
            "authorizationDigest": run.authorization.digest, "inputRef": original["inputRef"],
            "sourceVersion": original["sourceVersion"], "sha256": original["sha256"], "callId": call_id})
        self.assertEqual(confirmed.status_code, 200, confirmed.content)
        self.assertEqual(confirmed.json()["resolvedInput"], original)
        save_note(self.user, self.note.pk, save_id=uuid4().hex, base_generation=1,
                  display_name="Report", markdown="new version")
        run.status = "completed"
        run.save(update_fields=["status"])
        current = self.new_run()
        context = self.internal("context", {"schema": "workspace.material.context.read.v1",
            "agentRunId": current.pk, "authorizationDigest": current.authorization.digest})
        self.assertEqual(context.status_code, 200, context.content)
        self.assertEqual(context.json()["inputs"], [])
        self.assertEqual(context.json()["pendingInputs"], [{
            "resolvedInput": original, "state": "stale_generation", "reservations": []}])
        self.assertEqual(reserved_original_bytes(current), original["sizeBytes"])
        event_count = SessionEvent.objects.count()
        owner = leased_lifecycle_job_fixture(self, current)
        settled = self.internal("reconcile", {
            "schema": "workspace.material.reconcile.v1", "agentRunId": current.pk,
            "authorizationDigest": current.authorization.digest,
            "jobId": "agent_run.lifecycle:" + current.pk, "leaseOwner": owner,
            "inputs": [{"inputRef": original["inputRef"], "sourceVersion": original["sourceVersion"],
                        "sha256": original["sha256"], "state": "absent", "admissionIds": []}]})
        self.assertEqual(settled.status_code, 200, settled.content)
        self.assertFalse(SessionMaterialInput.objects.get().confirmed)
        self.assertEqual(reserved_original_bytes(current), 0)
        self.assertEqual(SessionEvent.objects.count(), event_count)
        self.assertFalse(SessionEvent.objects.filter(payload__type="tool_result").exists())
        from .models import MaterialEvidenceReceipt, SessionCitationProjection
        self.assertFalse(MaterialEvidenceReceipt.objects.exists())
        self.assertFalse(SessionCitationProjection.objects.exists())

    def test_prepare_caps_retained_identities_at_128_including_deduplicated_attachments(self):
        self.attach(self.note)
        run = self.new_run()
        first, _call = self.prepare(run)
        for index in range(127):
            empty = self.new_note(f"Empty original {index}", "")
            self.prepare(run, empty)
        # Fetching an already attached/prepared version spends no second identity slot.
        again, _call = self.prepare(run)
        self.assertEqual(again, first)
        self.assertEqual(SessionMaterialInput.objects.count(), 128)
        overflow = self.new_note("Overflow empty original", "")
        with self.assertRaises(KnowledgeError) as denied:
            self.prepare(run, overflow)
        self.assertEqual((denied.exception.code, denied.exception.status),
                         ("material_execution_input_count_exceeded", 409))
        self.assertEqual(SessionMaterialInput.objects.count(), 128)
        self.assertEqual(RunMaterialAdmission.objects.count(), 129)
        self.assertEqual(reserved_original_bytes(run), 0)
        # Metadata discovery still includes the unprepared 129th object.
        page = list_materials(self.access(run), scope="library", limit=100)
        remainder = list_materials(self.access(run), scope="library", limit=100, cursor=page["nextCursor"])
        self.assertEqual(len(page["items"]) + len(remainder["items"]), 129)
        self.assertIsNone(remainder["nextCursor"])
        self.assertEqual(SessionMaterialInput.objects.count(), 128)



@override_settings(SANDBOX_WORKSPACE_BYTES=131072)
class MaterialAdmissionConcurrencyTests(PersonalLibraryMaterialFixture, TransactionTestCase):
    serialized_rollback = True

    def test_concurrent_prepares_cannot_both_spend_the_same_session_budget(self):
        first = self.new_note("Large one", "a" * 40000)
        second = self.new_note("Large two", "b" * 40000)
        run = self.new_run()
        refs = [library_descriptor(run, owner)["inputRef"] for owner in (first, second)]
        calls = [self.call(run, ref) for ref in refs]
        barrier = Barrier(2)

        def prepare(index):
            close_old_connections()
            try:
                barrier.wait(timeout=10)
                prepare_original(run.pk, run.authorization.digest, refs[index], calls[index])
                return "prepared"
            except KnowledgeError as error:
                return error.code
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as workers:
            results = list(workers.map(prepare, range(2)))
        self.assertCountEqual(results, ["prepared", "material_execution_input_budget_exceeded"])
        self.assertEqual(RunMaterialAdmission.objects.count(), 1)
        self.assertEqual(reserved_original_bytes(run), 40000)


class PersonalLibraryScopeMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_forward_marks_existing_run_none_without_rewriting_authorization(self):
        old = [("app_core", "0041_workspace_avatar")]
        current = [("app_core", "0042_personal_library_materials")]
        with isolated_migration_database(old) as database:
            before = MigrationExecutor(database).loader.project_state(old).apps
            manager = lambda name: before.get_model("app_core", name).objects.using(database.alias)
            user = before.get_model("auth", "User").objects.using(database.alias).create(username="material-migration")
            workspace = manager("Workspace").create(name="Existing", createdBy_id=user.pk)
            membership = manager("WorkspaceMembership").create(workspace=workspace, user_id=user.pk, role="owner")
            model = manager("ModelConfig").create(displayName="Existing model")
            with transaction.atomic(using=database.alias):
                session = manager("Session").create(workspace=workspace, owner_id=user.pk)
                environment = manager("SessionEnvironment").create(session=session)
                session.current_environment = environment
                session.save(update_fields=["current_environment"])
                run = manager("AgentRun").create(workspace=workspace, session=session, environment=environment,
                    user_id=user.pk, modelConfig=model, membership_ref=membership.pk, prompt="accepted")
            from .test_agent_run_authorization import fixture
            from .runtime_contract import authorization_digest, authorization_signature
            payload = fixture() | {"id": "material-migration-authorization", "workspaceId": workspace.pk,
                "userId": str(user.pk), "sessionId": session.pk, "agentRunId": run.pk, "modelConfigRef": model.pk}
            frozen = payload, authorization_digest(payload), authorization_signature(payload, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY)
            manager("AgentRunAuthorization").create(id=payload["id"], agent_run=run,
                payload=frozen[0], digest=frozen[1], signature=frozen[2])
            MigrationExecutor(database).migrate(current)
            after = MigrationExecutor(database).loader.project_state(current).apps
            scope = after.get_model("app_core", "RunMaterialScope").objects.using(database.alias).get(agent_run_id=run.pk)
            self.assertFalse(scope.personal_library)
            accepted = after.get_model("app_core", "AgentRunAuthorization").objects.using(database.alias).get(agent_run_id=run.pk)
            self.assertEqual((accepted.payload, accepted.digest, accepted.signature), frozen)


class PersonalLibraryAdmissionTests(PersonalLibraryMaterialFixture, TransactionTestCase):
    serialized_rollback = True

    def test_new_run_acceptance_rejects_new_129th_attachment_but_allows_retained_identity(self):
        run = self.new_run()
        first = None
        for index in range(128):
            original, call_id = self.prepare(run, self.note if index == 0 else self.new_note(f"Retained {index}", ""))
            confirm_original(run.pk, run.authorization.digest, original["inputRef"],
                             original["sourceVersion"], original["sha256"], call_id)
            if first is None:
                first = original
        from .testing import release_simulated_run
        release_simulated_run(self, run)
        overflow = self.new_note("Extra attachment", "")
        link = self.attach(overflow)
        self.client.force_login(self.user)
        url = f"/api/workspaces/{self.workspace.pk}/sessions/{self.session.pk}/messages"
        body = {"operationId": "reject-input-overflow", "text": "Use the new file", "modelConfigRef": self.model.pk,
                "attachmentRefs": [link.pk]}
        from .models import HostedOperationReceipt, AgentRunAuthorization, RunMaterialScope
        before = (AgentRun.objects.count(), AgentRunAuthorization.objects.count(), RunMaterialScope.objects.count())
        with patch("app_core.http.workspaces.request_execution_profile", return_value={
                "schema": "runtime.execution_profile.v1", "imageDigest": "sha256:" + "a" * 64}), \
                patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted") as schedule:
            rejected = self.client.post(url, json.dumps(body), content_type="application/json")
            self.assertEqual(rejected.status_code, 409, rejected.content)
            self.assertEqual(rejected.json(), {"error": "material_execution_input_count_exceeded"})
            self.assertEqual((AgentRun.objects.count(), AgentRunAuthorization.objects.count(), RunMaterialScope.objects.count()), before)
            self.assertFalse(HostedOperationReceipt.objects.filter(operationId=body["operationId"]).exists())
            schedule.assert_not_called()
            link.delete()
            same = self.attach(self.note)
            accepted = self.client.post(url, json.dumps({**body, "operationId": "accept-retained-input",
                "attachmentRefs": [same.pk]}), content_type="application/json")
            self.assertEqual(accepted.status_code, 202, accepted.content)
            accepted_run = AgentRun.objects.get(pk=accepted.json()["agentRunId"])
            self.assertEqual(accepted_run.authorization.payload["assetRefs"][0]["inputRef"], first["inputRef"])
            self.assertEqual(accepted_run.authorization.payload["messageAssetRefs"], [first["inputRef"]])
            self.assertEqual(SessionMaterialInput.objects.count(), 128)
            schedule.assert_called_once()
