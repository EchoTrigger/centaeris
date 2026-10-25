"""Released database history is archival input, never an executable provider adapter."""

from django.db.migrations.loader import MigrationLoader
from django.test import SimpleTestCase


class NativeArchiveGraphTests(SimpleTestCase):
    def test_forward_admin_upgrade_keeps_applied_historical_parents(self):
        plan = MigrationLoader(None).graph.forwards_plan(
            ("app_core", "0032_workspace_resource_scope")
        )
        for node in (
            "0027_workspace_snapshot_publication",
            "0028_provider_maintenance_target",
            "0031_merge_lifecycle_and_presentation",
        ):
            self.assertIn(("app_core", node), plan)

    def test_publication_archive_cannot_create_or_settle_new_responsibilities(self):
        from app_core.models import WorkspaceSnapshotPublication

        with self.assertRaisesMessage(ValueError, "archived_publication_read_only"):
            WorkspaceSnapshotPublication(pk="new").save()
        with self.assertRaisesMessage(ValueError, "archived_publication_read_only"):
            WorkspaceSnapshotPublication.objects.filter(pk="old").update(
                state="committed"
            )

    def test_native_profile_rejects_archived_provider_without_relabeling(self):
        from app_core.native_archive import (
            require_native_environment_profile,
            NativeArchiveBlocked,
        )

        profile = {
            "schema": "runtime.e2b_environment_profile.v1",
            "provider": "e2b",
            "templateRef": {
                "templateId": "retained",
                "buildId": "11111111-1111-4111-8111-111111111111",
            },
        }
        original = dict(profile)
        with self.assertRaisesMessage(
            NativeArchiveBlocked,
            "environment_profile_unsupported_create_new_environment",
        ):
            require_native_environment_profile(profile)
        self.assertEqual(profile, original)


import hashlib
from contextlib import nullcontext
from unittest.mock import patch
from django.core.files.storage import default_storage
from django.db import connection
from django.test import TestCase
from django.utils import timezone
from . import test_workspace_snapshot_gc as snapshot_fixture


class NativeArchiveReferenceTests(TestCase):
    setUp = snapshot_fixture.WorkspaceSnapshotGcTests.setUp
    create_owner = snapshot_fixture.WorkspaceSnapshotGcTests.create_owner
    store = snapshot_fixture.WorkspaceSnapshotGcTests.store
    snapshot = snapshot_fixture.WorkspaceSnapshotGcTests.snapshot
    checkpoint = snapshot_fixture.WorkspaceSnapshotGcTests.checkpoint

    def publication(self, state):
        from .agent_run_authorization_factory import create_agent_run_authorization

        session, run = self.create_owner(purged=True)
        with (
            patch(
                "app_core.agent_run_authorization_factory.plugin_lifecycle_lock",
                side_effect=nullcontext,
            ),
            patch(
                "app_core.plugin_catalog.plugin_lifecycle_lock", side_effect=nullcontext
            ),
        ):
            authorization = create_agent_run_authorization(
                run, image_digest="sha256:" + "a" * 64
            )
        content = b"snapshot bytes"
        key = (
            self.checkpoint(session=session, run=run, content=content)
            if state == "staged"
            else self.snapshot(
                session=session, run=run, content=content, current=state == "committed"
            )
        )
        from django.db.migrations.loader import MigrationLoader

        old = (
            MigrationLoader(connection)
            .project_state([("app_core", "0031_merge_lifecycle_and_presentation")])
            .apps
        )
        publications = old.get_model("app_core", "WorkspaceSnapshotPublication").objects
        row = publications.create(
            id="archive_" + run.pk,
            purpose="executionCheckpoint" if state == "staged" else "sessionCommit",
            purpose_key=hashlib.sha256(run.pk.encode()).hexdigest(),
            checkpoint_id="checkpoint:first" if state == "staged" else "",
            storage_key=key,
            request_digest="sha256:" + "d" * 64,
            generation=1,
            encoding="frameV1",
            snapshot_sha256="sha256:" + hashlib.sha256(content).hexdigest(),
            snapshot_size_bytes=len(content),
            expanded_size_bytes=len(content),
            file_count=1,
            session_id=session.pk,
            agent_run_id=run.pk,
            authorization_id=authorization.pk,
        )
        if state != "pending":
            publications.filter(pk=row.pk).update(
                state=state, confirmed_at=timezone.now()
            )
        return row.pk, key

    def test_all_publication_states_retain_bytes_after_owner_purge(self):
        from .workspace_snapshot_gc import collect_workspace_snapshot_gc
        from .models import WorkspaceSnapshotPublication

        for state in ("pending", "staged", "committed"):
            with self.subTest(state=state):
                identity, key = self.publication(state)
                before = WorkspaceSnapshotPublication.objects.values().get(pk=identity)
                for dry_run in (True, False):
                    report = collect_workspace_snapshot_gc(self.now, dry_run)
                    self.assertIn(key, report.blocked)
                    self.assertNotIn(key, report.cleaned)
                    self.assertTrue(default_storage.exists(key))
                self.assertEqual(
                    WorkspaceSnapshotPublication.objects.values().get(pk=identity),
                    before,
                )

    def test_generic_storage_gc_also_rejects_an_archived_reference(self):
        from .assets import delete_stored_object_for_gc

        _, key = self.publication("committed")
        with self.assertRaisesMessage(
            ValueError, "storage_object_retained_by_archived_publication"
        ):
            delete_stored_object_for_gc(key)
        self.assertTrue(default_storage.exists(key))

    def test_unresolved_publication_blocks_activation_without_settling_or_deleting_it(
        self,
    ):
        from .native_archive import (
            require_native_archive_activation,
            NativeArchiveBlocked,
        )
        from .models import WorkspaceSnapshotPublication

        identity, key = self.publication("pending")
        before = WorkspaceSnapshotPublication.objects.values().get(pk=identity)
        with self.assertRaises(NativeArchiveBlocked):
            require_native_archive_activation()
        response = self.client.get("/health")
        self.assertEqual(response.status_code, 503, response.content)
        self.assertTrue(default_storage.exists(key))
        self.assertEqual(
            WorkspaceSnapshotPublication.objects.values().get(pk=identity), before
        )


import json
from .models import AgentRun, SessionEnvironment, SessionEvent, Artifact

ARCHIVED_PROFILE = {
    "schema": "runtime.e2b_environment_profile.v1",
    "provider": "e2b",
    "providerConfigDigest": "sha256:" + "a" * 64,
    "templateRef": {
        "templateId": "old-template",
        "buildId": "11111111-1111-4111-8111-111111111111",
    },
    "workspaceRoot": "/mnt/data",
    "user": "agent",
    "network": "disabled",
    "lifetime": "retained",
    "timeoutSeconds": 3600,
    "inputBudgetBytes": 1048576,
}


class NativeArchivePublicHistoryTests(TestCase):
    create_owner = snapshot_fixture.WorkspaceSnapshotGcTests.create_owner
    store = snapshot_fixture.WorkspaceSnapshotGcTests.store

    def setUp(self):
        snapshot_fixture.WorkspaceSnapshotGcTests.setUp(self)
        self.session, self.run = self.create_owner()
        profile = json.loads(json.dumps(ARCHIVED_PROFILE))
        digest = (
            "sha256:"
            + hashlib.sha256(
                json.dumps(profile, sort_keys=True, separators=(",", ":")).encode()
            ).hexdigest()
        )
        SessionEnvironment.objects.filter(
            pk=self.session.current_environment_id
        ).update(
            environment_profile=profile,
            environment_profile_digest=digest,
            environment_profile_operation_id="old-binding",
            environment_profile_bound_at=self.old,
        )
        self.profile = profile
        self.client.force_login(self.owner)

    def test_old_profile_reads_metadata_history_and_files_but_new_run_requires_new_environment(
        self,
    ):
        self.assertEqual(
            self.client.get(f"/api/sessions/{self.session.id}").status_code, 200
        )
        event = SessionEvent.objects.create(
            eventId="archived-output",
            workspace=self.workspace,
            session=self.session,
            agent_run=self.run,
            sequence=1,
            agent_run_sequence=1,
            projects_to_agent_run_stream=True,
            payload={
                "type": "tool_result",
                "payload": {"modelContent": "retained reply"},
            },
            createdAtMs=1,
        )
        page = {
            "sessionId": self.session.id,
            "blocks": [{"text": event.payload["payload"]["modelContent"]}],
        }
        with patch(
            "app_core.http.workspaces.request_transcript_page", return_value=page
        ) as reader:
            response = self.client.get(f"/api/sessions/{self.session.id}/transcript")
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json(), page)
            self.assertEqual(reader.call_args.args[0]["sourceHighWater"], "1")
        content = b"archived artifact bytes"
        key = self.store(
            f"workspaces/{self.workspace.id}/sessions/{self.session.id}/artifacts/result.txt",
            content,
        )
        artifact = Artifact.objects.create(
            workspace=self.workspace,
            session=self.session,
            agent_run=self.run,
            createdBy=self.owner,
            displayName="result.txt",
            safeFilename="result.txt",
            contentType="text/plain",
            storageKey=key,
            sizeBytes=len(content),
            sha256="sha256:" + hashlib.sha256(content).hexdigest(),
            status="published",
            publishedAt=self.old,
        )
        response = self.client.get(f"/api/artifacts/{artifact.pk}/download")
        self.assertEqual(response.status_code, 200)
        try:
            from .tests import async_iterator_bytes

            data = (
                async_iterator_bytes(response.streaming_content)
                if response.is_async
                else b"".join(response.streaming_content)
            )
            self.assertEqual(data, content)
        finally:
            # File readers must close, without ending the surrounding TestCase transaction.
            with patch("django.core.signals.request_finished.send"):
                response.close()
        before = AgentRun.objects.filter(session=self.session).count()
        with patch("app_core.http.workspaces.request_execution_profile") as runtime:
            response = self.client.post(
                f"/api/workspaces/{self.workspace.id}/sessions/{self.session.id}/messages",
                data=json.dumps(
                    {
                        "operationId": "native-reuse-rejected",
                        "text": "continue",
                        "modelConfigRef": self.model.pk,
                    }
                ),
                content_type="application/json",
            )
            self.assertEqual(response.status_code, 409, response.content)
            self.assertEqual(
                response.json()["error"],
                "environment_profile_unsupported_create_new_environment",
            )
            runtime.assert_not_called()
        self.assertEqual(AgentRun.objects.filter(session=self.session).count(), before)
        self.assertEqual(
            SessionEnvironment.objects.get(
                pk=self.session.current_environment_id
            ).environment_profile,
            self.profile,
        )

    def test_active_archived_run_blocks_native_activation(self):
        from .native_archive import (
            require_native_archive_activation,
            NativeArchiveBlocked,
        )

        AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.owner,
            modelConfig=self.model,
            prompt="old unfinished",
            status="queued",
        )
        with self.assertRaisesMessage(
            NativeArchiveBlocked,
            "native_activation_blocked_unsettled_archived_environments",
        ):
            require_native_archive_activation()

    def test_unreleased_archived_controller_blocks_native_activation(self):
        from .native_archive import (
            require_native_archive_activation,
            NativeArchiveBlocked,
        )

        SessionEnvironment.objects.filter(
            pk=self.session.current_environment_id
        ).update(
            controller_epoch=1,
            controller_run=self.run,
            controller_lease_owner="old-controller",
            controller_purpose="cleanup",
            controller_released=False,
        )
        with self.assertRaisesMessage(
            NativeArchiveBlocked,
            "native_activation_blocked_unsettled_archived_environments",
        ):
            require_native_archive_activation()

    def test_settled_public_history_does_not_manufacture_a_new_native_profile(self):
        from .native_archive import require_native_archive_activation

        require_native_archive_activation()
        self.assertEqual(
            SessionEnvironment.objects.get(
                pk=self.session.current_environment_id
            ).environment_profile,
            self.profile,
        )


class NativeArchiveLifespanTests(SimpleTestCase):
    def test_api_startup_refuses_unsettled_archive_before_accepting_requests(self):
        from asgiref.sync import async_to_sync
        from api.asgi import WorkspaceApplication
        from .native_archive import NativeArchiveBlocked

        sent = []

        async def receive():
            return {"type": "lifespan.startup"}

        async def send(message):
            sent.append(message)

        async def mcp(scope, receive, send):
            await receive()
            await send({"type": "lifespan.startup.complete"})

        app = object.__new__(WorkspaceApplication)
        app.mcp = mcp
        app.presentation_hub = None
        with patch(
            "app_core.native_archive.require_native_archive_activation",
            side_effect=NativeArchiveBlocked("unsettled"),
        ):
            with self.assertRaises(NativeArchiveBlocked):
                async_to_sync(app)({"type": "lifespan"}, receive, send)
        self.assertEqual(
            sent, [{"type": "lifespan.startup.failed", "message": "unsettled"}]
        )


class NativeArchiveServiceBoundaryTests(TestCase):
    setUp = NativeArchivePublicHistoryTests.setUp
    create_owner = snapshot_fixture.WorkspaceSnapshotGcTests.create_owner
    store = snapshot_fixture.WorkspaceSnapshotGcTests.store

    def test_freezing_a_run_never_rebinds_an_archived_profile(self):
        from .environment_profile import freeze_run_environment_profile
        from .native_archive import NativeArchiveBlocked

        before = SessionEnvironment.objects.values().get(
            pk=self.session.current_environment_id
        )
        with self.assertRaisesMessage(
            NativeArchiveBlocked,
            "environment_profile_unsupported_create_new_environment",
        ):
            freeze_run_environment_profile(self.run, "sha256:" + "a" * 64)
        self.assertEqual(
            SessionEnvironment.objects.values().get(
                pk=self.session.current_environment_id
            ),
            before,
        )

    def test_maintenance_rejects_old_profile_before_allocating_or_issuing_a_grant(self):
        from django.db import transaction
        from .environment_maintenance import MaintenanceError, _locked_environment
        from .models import EnvironmentMaintenanceOperation

        with (
            transaction.atomic(),
            self.assertRaisesMessage(
                MaintenanceError,
                "environment_profile_unsupported_create_new_environment",
            ),
        ):
            _locked_environment(self.session.current_environment_id)
        self.assertFalse(EnvironmentMaintenanceOperation.objects.exists())

    def test_management_preflight_fails_without_settling_an_active_old_run(self):
        from django.core.management import call_command, CommandError

        run = AgentRun.objects.create(
            workspace=self.workspace,
            session=self.session,
            user=self.owner,
            modelConfig=self.model,
            prompt="unfinished",
            status="queued",
        )
        with self.assertRaisesMessage(
            CommandError, "native_activation_blocked_unsettled_archived_environments"
        ):
            call_command("verify_native_archive")
        run.refresh_from_db()
        self.assertEqual(run.status, "queued")
