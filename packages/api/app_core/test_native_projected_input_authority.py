import json
from pathlib import Path
import re
from uuid import uuid4

from django.conf import settings
from django.db import connection
from django.test import TransactionTestCase

from .assets import tombstone_stored_object
from .environment_lifecycle import acquire_environment_controller
from .library_notes import save_note
from .material_files import confirm_original
from .models import LibraryNoteRevision, UserLibraryObject, WorkspaceMembership
from .test_personal_library_materials import PersonalLibraryMaterialFixture
from .testing import leased_lifecycle_job_fixture


AUTHORITY_SQL = re.sub(
    r"\$[1-8]", "%s",
    Path(__file__).with_name("native_projected_input_authority.sql").read_text(encoding="utf-8"),
)
VALIDATE_SCHEMA = "runtime.projected_input.validate.v1"
DESCRIPTOR_FIELDS = ("inputRef", "virtualPath", "sizeBytes", "sha256", "sourceVersion")


class NativeProjectedInputAuthorityTests(PersonalLibraryMaterialFixture, TransactionTestCase):
    def activate(self, run):
        self.run = run
        self.grant = run.authorization
        self.lease_owner = leased_lifecycle_job_fixture(self, run)
        with connection.cursor() as cursor:
            cursor.execute(
                "CREATE TABLE runtime.runtime_events(event_id text PRIMARY KEY, session_id text, task_id text, "
                "event_type text NOT NULL, visibility text, at_ms bigint, payload_json text)"
            )
            cursor.execute("CREATE TABLE runtime.execution_job_tenants(job_id text PRIMARY KEY, workspace_id text NOT NULL)")
            cursor.execute(
                "INSERT INTO runtime.execution_job_tenants VALUES (%s, %s)",
                ["agent_run.lifecycle:" + run.id, run.workspace_id],
            )
        self.controller = acquire_environment_controller(run.id, self.lease_owner)

    def prepared(self, run, owner=None):
        original, call_id = self.prepare(run, owner)
        return {field: original[field] for field in DESCRIPTOR_FIELDS}, call_id

    def confirm(self, run, descriptor, call_id):
        confirm_original(run.id, run.authorization.digest, descriptor["inputRef"],
                         descriptor["sourceVersion"], descriptor["sha256"], call_id)

    def native(self, inputs, **changes):
        values = {
            "run_id": self.run.id, "digest": self.grant.digest, "signature": self.grant.signature,
            "payload": json.dumps(self.grant.payload), "lease_owner": self.lease_owner,
            "inputs": json.dumps(inputs), "environment_id": self.controller["environmentId"],
            "controller_epoch": self.controller["controllerEpoch"],
        } | changes
        with connection.cursor() as cursor:
            cursor.execute(AUTHORITY_SQL, list(values.values()))
            return cursor.fetchall()

    def legacy(self, inputs):
        return self.client.post(
            "/internal/agent-runs/validate-inputs",
            {"schema": VALIDATE_SCHEMA, "agentRunId": self.run.id,
             "authorizationDigest": self.grant.digest, "inputs": inputs},
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )

    def assert_state(self, descriptor, state):
        legacy = self.legacy([descriptor])
        self.assertEqual(legacy.status_code, 200, legacy.content)
        self.assertEqual(legacy.json()["inputs"], [{"inputRef": descriptor["inputRef"], "state": state}])
        self.assertEqual(self.native([descriptor]), [(descriptor["inputRef"], state)])

    def test_installed_note_matches_legacy_and_keeps_revocation_precedence(self):
        run = self.new_run()
        self.activate(run)
        descriptor, call_id = self.prepared(run)
        self.assertEqual(self.native([descriptor]), [(descriptor["inputRef"], "legacy")])
        self.confirm(run, descriptor, call_id)
        self.assert_state(descriptor, "active")
        for field, value in {
            "virtualPath": "inputs/another.md", "sizeBytes": descriptor["sizeBytes"] + 1,
            "sha256": "sha256:" + "0" * 64, "sourceVersion": "999",
        }.items():
            with self.subTest(field=field):
                self.assert_state(descriptor | {field: value}, "stale_generation")
        save_note(self.user, self.note.pk, save_id=uuid4().hex, base_generation=1,
                  display_name="Report", markdown="new version")
        self.assert_state(descriptor, "stale_generation")
        self.note.refresh_from_db()
        tombstone_stored_object(self.note, deleted_by=self.user)
        self.assert_state(descriptor, "asset_removed")
        self.user.is_active = False
        self.user.save(update_fields=["is_active"])
        self.assert_state(descriptor, "access_revoked")

    def test_confirmed_note_can_restore_in_new_run_and_mix_with_legacy(self):
        origin = self.new_run()
        original, call_id = self.prepared(origin)
        self.confirm(origin, original, call_id)
        origin.status = "completed"
        origin.save(update_fields=["status"])
        attached_note = self.new_note("Attachment", "direct attachment")
        link = self.attach(attached_note)
        run = self.new_run(personal=False, message_refs=[link.pk])
        self.activate(run)
        self.assert_state(original, "access_revoked")
        declared, call_id = self.prepared(run, attached_note)
        self.confirm(run, declared, call_id)
        self.assertEqual(declared["inputRef"], link.pk)
        legacy = self.legacy([declared])
        self.assertEqual(legacy.status_code, 200, legacy.content)
        self.assertEqual(legacy.json()["inputs"], [{"inputRef": link.pk, "state": "active"}])
        self.assertEqual(self.native([declared, original]),
                         [(link.pk, "legacy"), (original["inputRef"], "access_revoked")])

    def test_new_run_can_use_confirmed_note_without_a_new_admission(self):
        origin = self.new_run()
        descriptor, call_id = self.prepared(origin)
        self.confirm(origin, descriptor, call_id)
        origin.status = "completed"
        origin.save(update_fields=["status"])
        self.activate(self.new_run())
        self.assert_state(descriptor, "active")

    def test_signed_attachment_reuses_confirmed_canonical_note(self):
        origin = self.new_run()
        descriptor, call_id = self.prepared(origin)
        self.confirm(origin, descriptor, call_id)
        origin.status = "completed"
        origin.save(update_fields=["status"])
        link = self.attach(self.note)
        self.activate(self.new_run(message_refs=[link.pk]))
        self.assertEqual(self.grant.payload["messageAssetRefs"], [descriptor["inputRef"]])
        self.assert_state(descriptor, "active")

    def test_corrupt_note_body_fails_closed_and_external_storage_is_legacy(self):
        run = self.new_run()
        self.activate(run)
        descriptor, call_id = self.prepared(run)
        self.confirm(run, descriptor, call_id)
        head = self.note.noteHead
        UserLibraryObject.objects.filter(pk=self.note.pk).update(noteHead=None)
        self.assertEqual(self.native([descriptor]), [(descriptor["inputRef"], "legacy")])
        # A persisted revision cannot be rewritten. Create a malformed row to
        # exercise integrity handling without disabling the immutability trigger.
        LibraryNoteRevision.objects.filter(pk=head.pk).delete()
        corrupt = LibraryNoteRevision.objects.create(
            note=self.note, generation=head.generation, displayName=head.displayName,
            markdown=head.markdown.replace("exact", "wrong"), sha256=head.sha256,
            sizeBytes=head.sizeBytes, createdBy=self.user,
        )
        UserLibraryObject.objects.filter(pk=self.note.pk).update(noteHead=corrupt)
        self.assertEqual(self.legacy([descriptor]).status_code, 400)
        self.assertEqual(self.native([descriptor]), [(descriptor["inputRef"], "invalid")])

    def test_live_run_proof_and_membership_are_required(self):
        run = self.new_run()
        self.activate(run)
        descriptor, call_id = self.prepared(run)
        self.confirm(run, descriptor, call_id)
        self.assert_state(descriptor, "active")
        for field, value in {
            "digest": "sha256:" + "0" * 64, "signature": "hmac-sha256:" + "0" * 64,
            "payload": "{}", "lease_owner": "worker:fixture:other-lease",
            "environment_id": "other-environment", "controller_epoch": self.controller["controllerEpoch"] + 1,
        }.items():
            with self.subTest(field=field):
                self.assertEqual(self.native([descriptor], **{field: value}), [])
        with connection.cursor() as cursor:
            cursor.execute(
                "INSERT INTO runtime.runtime_events(event_id, event_type) VALUES (%s, %s)",
                ["agent_run_cancel_requested:" + run.id, "runtime.agent_run.cancel_requested.v1"],
            )
        self.assertEqual(self.native([descriptor]), [])
        with connection.cursor() as cursor:
            cursor.execute("DELETE FROM runtime.runtime_events WHERE event_id = %s", ["agent_run_cancel_requested:" + run.id])
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms = 0 WHERE job_id = %s", ["agent_run.lifecycle:" + run.id])
        self.assertEqual(self.native([descriptor]), [])
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms = "
                           "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint + 60000 WHERE job_id = %s",
                           ["agent_run.lifecycle:" + run.id])
        self.assert_state(descriptor, "active")
        WorkspaceMembership.objects.filter(pk=run.membership_ref).delete()
        self.assertEqual(self.legacy([descriptor]).status_code, 400)
        self.assertEqual(self.native([descriptor]), [])
