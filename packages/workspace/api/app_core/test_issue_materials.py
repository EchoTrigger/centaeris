import hashlib
import json
from tempfile import TemporaryDirectory
from unittest.mock import patch

from django.core.files.uploadedfile import SimpleUploadedFile
from django.db.migrations.executor import MigrationExecutor
from django.test import Client, TestCase, TransactionTestCase, override_settings
from django.utils import timezone

from .migration_testing import isolated_migration_database
from .models import (AgentRun, Issue, IssueEvent, IssueMaterial, ModelConfig, SessionAssetLink,
                     Source, SourceObject, Workspace, WorkspaceMembership)
from .session_record_test_fixtures import SessionRecordFixture
from .test_hosted_operations import PROFILE


class IssueMaterialTests(SessionRecordFixture, TestCase):
    def setUp(self):
        super().setUp()
        temp = TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        settings = override_settings(MEDIA_ROOT=temp.name)
        settings.enable()
        self.addCleanup(settings.disable)
        self.issue = Issue.objects.create(workspace=self.workspace, creator=self.user,
                                          assignee=self.user, title="Check reimbursement")
        self.url = f"/api/workspaces/{self.workspace.id}/issues/{self.issue.id}/materials"
        self.execution_url = self.url.removesuffix("materials") + "executions"
        result = self.client.post("/api/library", {"files": SimpleUploadedFile("private.txt", b"receipt")})
        self.assertEqual(result.status_code, 201, result.content)
        self.asset_id = result.json()["objects"][0]["id"]
        self.model = ModelConfig.objects.create(displayName="Issue model")
        for target, result in (("request_execution_profile", PROFILE), ("schedule_agent_run_lifecycle", "inserted")):
            mock = patch(f"app_core.http.workspaces.{target}", return_value=result)
            setattr(self, target, mock.start())
            self.addCleanup(mock.stop)

    def attach(self, **fields):
        return self.client.post(self.url, json.dumps({"version": 1,
            "assetKind": "userLibraryObject", "assetId": self.asset_id, **fields}), content_type="application/json")

    def execute(self, **fields):
        return self.client.post(self.execution_url, json.dumps({"version": 2,
            "operationId": "material-execution", "modelConfigRef": self.model.pk, **fields}), content_type="application/json")

    def test_reference_revision_activity_and_detach_preserve_original(self):
        result = self.attach()
        self.assertEqual(result.status_code, 200, result.content)
        self.assertEqual(result.json()["version"], 2)
        material = result.json()["materials"][0]
        self.assertEqual((material["displayName"], material["available"]), ("private.txt", True))
        self.assertEqual(self.attach().status_code, 409)
        self.assertEqual(self.attach(version=2).json()["version"], 2)
        self.assertEqual(IssueEvent.objects.get(issue=self.issue).changes,
                          {"materials": {"before": 0, "after": 1}})
        removed = self.client.delete(f"{self.url}/{material['id']}", json.dumps({"version": 2}), content_type="application/json")
        self.assertEqual(removed.json(), {"version": 3, "materials": []})
        self.assertEqual(len(self.client.get("/api/library").json()["objects"]), 1)

    def test_private_metadata_is_redacted_and_other_member_cannot_dispatch(self):
        self.assertEqual(self.attach().status_code, 200)
        self.client.force_login(self.other)
        row = self.client.get(self.url).json()["materials"][0]
        self.assertFalse(row["available"])
        for field in ("assetId", "displayName", "contentType", "sizeBytes"):
            self.assertIsNone(row[field])
        self.assertEqual(self.attach(version=2).status_code, 403)
        response = self.execute()
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(AgentRun.objects.exists())
        self.schedule_agent_run_lifecycle.assert_not_called()

    def test_dispatch_freezes_real_inputs_and_replay_does_not_rebind(self):
        self.attach()
        response = self.execute()
        self.assertEqual(response.status_code, 202, response.content)
        run = AgentRun.objects.get(pk=response.json()["agentRunId"])
        link = SessionAssetLink.objects.get(session=run.session)
        self.assertEqual(link.userLibraryObject_id, self.asset_id)
        self.assertEqual(link.capturedSha256, "sha256:" + hashlib.sha256(b"receipt").hexdigest())
        self.assertEqual(run.authorization.payload["messageAssetRefs"], [link.pk])
        IssueMaterial.objects.filter(issue=self.issue).delete()
        Issue.objects.filter(pk=self.issue.pk).update(version=3)
        replay = self.execute()
        self.assertEqual(replay.json(), response.json())
        self.assertEqual(SessionAssetLink.objects.filter(session=run.session).count(), 1)
        self.issue.refresh_from_db()
        self.assertEqual((self.issue.status, self.issue.assignee_id), ("todo", self.user.pk))

    def test_changed_and_deleted_materials_reject_new_execution_without_orphans(self):
        from .models import UserLibraryObject
        self.attach()
        UserLibraryObject.objects.filter(pk=self.asset_id).update(contentGeneration=2)
        self.assertFalse(self.client.get(self.url).json()["materials"][0]["available"])
        response = self.execute()
        self.assertEqual((response.status_code, response.json()["error"]), (409, "issue_material_version_changed"))
        UserLibraryObject.objects.filter(pk=self.asset_id).update(status="deleted", deletedAt=timezone.now(),
            deletedBy=self.user, deletedFromStatus="ready", deletionGeneration=1)
        self.assertEqual(self.execute().status_code, 403)
        self.assertFalse(AgentRun.objects.exists())
        self.assertFalse(SessionAssetLink.objects.exists())

    def test_source_references_follow_workspace_and_current_grants(self):
        source = Source.objects.create(workspace=self.workspace, name="Rules", status="ready",
            sourceType="uploadedFile", createdBy=self.user)
        item = SourceObject.objects.create(workspace=self.workspace, source=source, objectType="file",
            displayPath="rules.txt", displayName="rules.txt", contentType="text/plain", sizeBytes=0,
            sha256="sha256:" + hashlib.sha256(b"").hexdigest(), storageKey="rules", sourceVersion="v1", status="ready")
        attached = self.attach(assetKind="sourceObject", assetId=item.pk)
        self.assertEqual(attached.status_code, 200, attached.content)
        # Owner has control over sources; ordinary members require a source grant.
        self.client.force_login(self.other)
        self.assertFalse(self.client.get(self.url).json()["materials"][0]["available"])
        self.assertEqual(self.execute().status_code, 403)
        self.client.force_login(self.user)
        foreign = Workspace.objects.create(name="Elsewhere", createdBy=self.user)
        WorkspaceMembership.objects.create(workspace=foreign, user=self.user, role="owner")
        other_issue = Issue.objects.create(workspace=foreign, creator=self.user, title="Other")
        response = self.client.post(f"/api/workspaces/{foreign.id}/issues/{other_issue.id}/materials",
            json.dumps({"version": 1, "assetKind": "sourceObject", "assetId": item.pk}), content_type="application/json")
        self.assertEqual(response.status_code, 403)

    def test_archive_membership_strict_schema_and_csrf(self):
        for fields in ({"extra": 1}, {"version": "1"}, {"assetKind": "artifact"}):
            self.assertEqual(self.attach(**fields).status_code, 400)
        client = Client(enforce_csrf_checks=True)
        client.force_login(self.user)
        self.assertEqual(client.post(self.url, json.dumps({"version": 1, "assetKind": "userLibraryObject",
            "assetId": self.asset_id}), content_type="application/json").status_code, 403)
        Issue.objects.filter(pk=self.issue.pk).update(archivedAt=timezone.now())
        self.assertEqual(self.attach().status_code, 409)
        self.membership.delete()
        self.assertEqual(self.client.get(self.url).status_code, 404)


class IssueMaterialMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_forward_preserves_existing_issues_without_materials(self):
        previous, target = [("app_core", "0061_issue_executions")], [("app_core", "0062_issue_materials")]
        with isolated_migration_database(previous) as database:
            alias = database.alias
            old = MigrationExecutor(database).loader.project_state(previous).apps
            user = old.get_model("auth", "User").objects.using(alias).create(username="materials-upgrade")
            workspace = old.get_model("app_core", "Workspace").objects.using(alias).create(name="Existing", createdBy_id=user.pk)
            issue = old.get_model("app_core", "Issue").objects.using(alias).create(workspace_id=workspace.pk,
                creator_id=user.pk, title="Existing issue", version=4, status="inProgress")
            MigrationExecutor(database).migrate(target)
            new = MigrationExecutor(database).loader.project_state(target).apps
            current = new.get_model("app_core", "Issue").objects.using(alias).get(pk=issue.pk)
            self.assertEqual((current.title, current.version, current.status), ("Existing issue", 4, "inProgress"))
            self.assertFalse(new.get_model("app_core", "IssueMaterial").objects.using(alias).exists())
