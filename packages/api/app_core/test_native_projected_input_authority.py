from importlib import import_module
import json
from pathlib import Path
import re
from unittest.mock import patch
from uuid import uuid4

from django.conf import settings
from django.db import connection, transaction
from django.test import TransactionTestCase

from .assets import tombstone_stored_object
from .deferred_input import resolve_deferred_input
from .environment_lifecycle import acquire_environment_controller
from .library_notes import save_note
from .material_files import confirm_original
from .models import UserLibraryObject, WorkspaceMembership
from .note_content import NoteRevision
from .test_personal_library_materials import PersonalLibraryMaterialFixture
from .testing import leased_lifecycle_job_fixture


def native_authority_sql(name):
    path = Path(__file__).with_name(name)
    fragment = path.with_name("native_run_execution_authority.sql").read_text(encoding="utf-8")
    statement = path.read_text(encoding="utf-8").replace("/* native_run_execution_authority */", fragment)
    return re.sub(r"\$[0-9]+", "%s", statement)


def native_run_authority_fixture(test_case, run):
    lease_owner = leased_lifecycle_job_fixture(test_case, run)
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
    return lease_owner, acquire_environment_controller(run.id, lease_owner)


def native_run_authority_params(run, lease_owner, controller):
    grant = run.authorization
    return {
        "run_id": run.id, "digest": grant.digest, "signature": grant.signature,
        "payload": json.dumps(grant.payload), "lease_owner": lease_owner,
        "environment_id": controller["environmentId"], "controller_epoch": controller["controllerEpoch"],
    }


AUTHORITY_SQL = native_authority_sql("native_projected_input_authority.sql")
VALIDATE_SCHEMA = "runtime.projected_input.validate.v1"
DESCRIPTOR_FIELDS = ("inputRef", "virtualPath", "sizeBytes", "sha256", "sourceVersion")


class NativeProjectedInputAuthorityTests(PersonalLibraryMaterialFixture, TransactionTestCase):
    def activate(self, run):
        self.run = run
        self.grant = run.authorization
        self.lease_owner, self.controller = native_run_authority_fixture(self, run)

    def prepared(self, run, owner=None):
        original, call_id = self.prepare(run, owner)
        return {field: original[field] for field in DESCRIPTOR_FIELDS}, call_id

    def confirm(self, run, descriptor, call_id):
        confirm_original(run.id, run.authorization.digest, descriptor["inputRef"],
                         descriptor["sourceVersion"], descriptor["sha256"], call_id)

    def native(self, inputs, **changes):
        values = native_run_authority_params(self.run, self.lease_owner, self.controller) | {
            "inputs": json.dumps(inputs),
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

    def test_mismatched_note_head_fails_closed_and_external_storage_is_legacy(self):
        run = self.new_run()
        self.activate(run)
        descriptor, call_id = self.prepared(run)
        self.confirm(run, descriptor, call_id)
        foreign = self.new_note(self.note.displayName, self.note.noteHead.markdown)
        UserLibraryObject.objects.filter(pk=self.note.pk).update(noteHead=None, storageKey="library/missing-original.md")
        self.assertEqual(self.native([descriptor]), [(descriptor["inputRef"], "legacy")])
        fallback = self.legacy([descriptor])
        self.assertEqual(fallback.status_code, 200, fallback.content)
        self.assertEqual(fallback.json()["inputs"], [{"inputRef": descriptor["inputRef"], "state": "access_revoked"}])
        UserLibraryObject.objects.filter(pk=self.note.pk).update(noteHead=foreign.noteHead)
        self.assertEqual(self.legacy([descriptor]).status_code, 400)
        self.assertEqual(self.native([descriptor]), [(descriptor["inputRef"], "invalid")])

    def test_mixed_batch_authority_does_not_materialize_validated_note_bodies(self):
        attached = self.new_note("Attachment", "original attachment")
        link = self.attach(attached)
        run = self.new_run(message_refs=[link.pk])
        self.activate(run)
        note, call_id = self.prepared(run)
        self.confirm(run, note, call_id)
        attachment = resolve_deferred_input(run, link.pk, run.authorization.digest)
        descriptors = sorted(
            [note, {field: attachment[field] for field in DESCRIPTOR_FIELDS}],
            key=lambda descriptor: descriptor["inputRef"],
        )
        states = {note["inputRef"]: "active", link.pk: "legacy"}
        self.assertEqual(self.native(descriptors), [
            (item["inputRef"], states[item["inputRef"]]) for item in descriptors
        ])
        with patch("app_core.note_content.NoteRevision", side_effect=AssertionError("loaded a validated body")):
            fallback = self.legacy(descriptors)
        self.assertEqual(fallback.status_code, 200, fallback.content)
        self.assertEqual(fallback.json()["inputs"], [
            {"inputRef": item["inputRef"], "state": "active"} for item in descriptors
        ])

    def test_unvalidated_constraint_keeps_body_checks_in_the_legacy_resolver(self):
        run = self.new_run()
        self.activate(run)
        descriptor, call_id = self.prepared(run)
        self.confirm(run, descriptor, call_id)
        addition = import_module("app_core.migrations.0052_library_note_body_constraint")
        with transaction.atomic():
            with connection.cursor() as cursor:
                cursor.execute(
                    "ALTER TABLE public.app_core_librarynoterevision DROP CONSTRAINT note_revision_body_identity"
                )
                cursor.execute(addition.ADD_CONSTRAINT)
            self.assertEqual(self.native([descriptor]), [(descriptor["inputRef"], "legacy")])
            with patch("app_core.note_content.NoteRevision", wraps=NoteRevision) as captured:
                response = self.legacy([descriptor])
            self.assertEqual(response.status_code, 200, response.content)
            captured.assert_called_once()
            transaction.set_rollback(True)

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
