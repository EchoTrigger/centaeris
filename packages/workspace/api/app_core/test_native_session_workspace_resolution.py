import json

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import DatabaseError, connection
from django.test import TransactionTestCase
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, ModelConfig, Session, Workspace, WorkspaceMembership
from .runtime_contract import session_workspace_for_session
from .test_native_projected_input_authority import (
    native_authority_sql, native_run_authority_fixture, native_run_authority_params,
)
from .testing import create_session


AUTHORITY_SQL = native_authority_sql("native_session_workspace_resolution.sql")


class NativeSessionWorkspaceResolutionTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="native-workspace-owner")
        self.workspace = Workspace.objects.create(name="Workspace baseline fixture", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.model = ModelConfig.objects.create()
        self.session = create_session(workspace=self.workspace, owner=self.user)

    def new_run(self, status="running"):
        return AgentRun.objects.create(
            workspace=self.workspace, session=self.session, user=self.user, modelConfig=self.model,
            prompt="Resolve a workspace baseline", status=status,
        )

    def activate(self):
        self.run = self.new_run()
        self.grant = create_agent_run_authorization(self.run, image_digest="sha256:" + "a" * 64)
        self.lease_owner, self.controller = native_run_authority_fixture(self, self.run)

    def advance(self, run, generation=1):
        Session.objects.filter(pk=self.session.pk).update(
            workspaceGeneration=generation, workspaceStorageKey="session-workspace/fixture",
            workspaceSnapshotSha256="sha256:" + "b" * 64,
            workspaceSnapshotSizeBytes=35, workspaceExpandedSizeBytes=12, workspaceFileCount=1,
            workspaceLastAdvancedAgentRun=run,
        )
        self.session.refresh_from_db()

    def native(self):
        params = native_run_authority_params(self.run, self.lease_owner, self.controller)
        with connection.cursor() as cursor:
            cursor.execute(AUTHORITY_SQL, list(params.values()))
            return cursor.fetchall()

    def legacy(self):
        return self.client.post(
            "/internal/agent-runs/session-workspace/resolve",
            {"schema": "runtime.session_workspace.resolve.v1",
             "jobId": "agent_run.lifecycle:" + self.run.pk, "leaseOwner": self.lease_owner,
             "agentRunId": self.run.pk, "authorizationDigest": self.grant.digest},
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
        )

    def assert_resolution(self, disposition, *, error=None):
        self.session.refresh_from_db()
        current = session_workspace_for_session(self.session)
        signed = self.grant.payload["sessionWorkspace"]
        [(actual, current_json, signed_json)] = self.native()
        self.assertEqual((actual, json.loads(current_json), json.loads(signed_json)),
                         (disposition, current, signed))
        response = self.legacy()
        if error is None:
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json(), {
                "schema": "runtime.session_workspace.resolved.v1", "disposition": disposition,
                "sessionWorkspace": current if disposition == "advanced" else signed,
            })
        else:
            self.assertEqual(response.status_code, 409, response.content)
            self.assertEqual(response.json(), {"error": error})

    def test_matching_empty_and_unknown_baselines(self):
        self.activate()
        self.assert_resolution("empty")
        Session.objects.filter(pk=self.session.pk).update(workspaceBaselineKnown=False)
        self.assert_resolution("baseline_unknown", error="session_workspace_baseline_unavailable")

    def test_matching_signed_snapshot_requires_download(self):
        origin = self.new_run(status="completed")
        create_agent_run_authorization(origin, image_digest="sha256:" + "a" * 64)
        self.advance(origin)
        self.activate()
        self.assert_resolution("download")
        Session.objects.filter(pk=self.session.pk).update(workspaceFileCount=2)
        self.assert_resolution("baseline_conflict", error="session_workspace_baseline_conflict")

    def test_only_exact_same_run_one_generation_advance_wins_over_conflict(self):
        self.activate()
        self.advance(self.run)
        self.assert_resolution("advanced")
        self.advance(self.run, generation=2)
        self.assert_resolution("baseline_conflict", error="session_workspace_baseline_conflict")
        self.advance(self.new_run(status="completed"))
        self.assert_resolution("baseline_conflict", error="session_workspace_baseline_conflict")

    def test_no_native_grant_for_non_running_or_expired_lease_or_removed_membership(self):
        self.activate()
        self.assert_resolution("empty")
        job_id = "agent_run.lifecycle:" + self.run.pk
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET status='leased' WHERE job_id=%s", [job_id])
        self.assertEqual(self.native(), [])
        self.assertEqual(self.legacy().json(), {"error": "session_workspace_lease_lost"})
        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE runtime.runtime_jobs SET status='running', lease_expires_at_ms=0 WHERE job_id=%s", [job_id],
            )
        self.assertEqual(self.native(), [])
        self.assertEqual(self.legacy().json(), {"error": "session_workspace_lease_lost"})
        with connection.cursor() as cursor:
            cursor.execute(
                "UPDATE runtime.runtime_jobs SET lease_expires_at_ms="
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000 WHERE job_id=%s", [job_id],
            )
        self.assert_resolution("empty")
        self.membership.delete()
        self.assertEqual(self.native(), [])
        self.assertEqual(self.legacy().json(), {"error": "session_workspace_agent_run_not_found"})

    def test_no_native_grant_for_purged_session(self):
        self.activate()
        deleted = timezone.now()
        Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=deleted, purgedAt=deleted)
        self.assertEqual(self.native(), [])
        self.assertEqual(self.legacy().json(), {"error": "session_workspace_session_unavailable"})

    def test_native_resolve_refuses_to_read_through_an_inflight_session_publication(self):
        self.activate()
        with connection.Database.connect(**connection.get_connection_params()) as publication:
            with publication.cursor() as cursor:
                cursor.execute(
                    'SELECT id FROM public.app_core_session WHERE id=%s FOR UPDATE', [self.session.pk],
                )
                self.assertEqual(cursor.fetchone(), (self.session.pk,))
                with self.assertRaises(DatabaseError) as error:
                    self.native()
                self.assertEqual(error.exception.__cause__.sqlstate, "55P03")
        self.assert_resolution("empty")
