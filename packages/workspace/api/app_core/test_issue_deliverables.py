import hashlib
import json
from tempfile import TemporaryDirectory

from asgiref.sync import async_to_sync
from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import Client, TestCase, TransactionTestCase, override_settings
from django.utils import timezone

from .migration_testing import isolated_migration_database
from .models import (Artifact, Issue, IssueDeliverable, IssueDeliverableResponse, IssueEvent,
    IssueExecution, IssueNotification, Workspace, WorkspaceMembership)
from .session_record_test_fixtures import SessionRecordFixture


class IssueDeliverableTests(SessionRecordFixture, TestCase):
    def setUp(self):
        super().setUp()
        temp = TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        settings = override_settings(MEDIA_ROOT=temp.name)
        settings.enable()
        self.addCleanup(settings.disable)
        self.third = get_user_model().objects.create_user(username="unselected-member")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=self.third, role="member")
        self.issue = Issue.objects.create(workspace=self.workspace, creator=self.user,
            assignee=self.other, title="Review a report")
        self.run = self.message_run(self.session)
        self.execution = IssueExecution.objects.create(issue=self.issue, issuer=self.user,
            session=self.session, agent_run=self.run, issueVersion=1,
            issueTitle=self.issue.title, issueDescription="")
        body = b"published report"
        key = default_storage.save("deliverables/report.txt", ContentFile(body))
        self.artifact = Artifact.objects.create(workspace=self.workspace, session=self.session,
            agent_run=self.run, createdBy=self.user, displayName="report.txt", safeFilename="report.txt",
            contentType="text/plain", storageKey=key, sizeBytes=len(body),
            sha256="sha256:" + hashlib.sha256(body).hexdigest(), status="published", publishedAt=timezone.now())
        self.url = f"/api/workspaces/{self.workspace.pk}/issues/{self.issue.pk}/deliverables"

    def submit(self, **changes):
        data = {"version": 1, "executionId": str(self.execution.pk), "agentRunId": self.run.pk,
            "summary": "Selected result", "artifactIds": [self.artifact.pk],
            "recipientId": str(self.other.pk), "replacesId": None, **changes}
        return self.client.post(self.url, json.dumps(data), content_type="application/json")

    def respond(self, item, **changes):
        return self.client.post(f"{self.url}/{item['id']}/responses", json.dumps({
            "version": 2, "action": "feedback", "text": "Please clarify the evidence", **changes}),
            content_type="application/json")

    def test_selected_recipient_receives_result_and_download_but_not_private_session(self):
        response = self.submit()
        self.assertEqual(response.status_code, 201, response.content)
        item = response.json()["deliverable"]
        self.assertEqual(response.json()["version"], 2)
        self.assertEqual((item["revision"], item["summary"], item["isCurrent"]), (1, "Selected result", True))
        frozen = IssueDeliverable.objects.get(pk=item["id"]).artifacts[0]
        self.assertEqual((frozen["sha256"], frozen["contentGeneration"]), (self.artifact.sha256, 1))
        self.assertEqual(list(IssueNotification.objects.values_list("recipient_id", "kind")), [(self.other.pk, "deliverable")])
        event = IssueEvent.objects.get(issue=self.issue)
        self.assertEqual(event.changes, {"deliverableId": {"before": None, "after": item["id"]}})
        inbox = f"/api/workspaces/{self.workspace.pk}/notifications"
        self.assertEqual(self.client.get(inbox).json()["notifications"], [])
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(inbox).json()["notifications"][0]["event"]["id"], event.pk)
        self.assertEqual(self.client.get(self.url).json()["deliverables"][0]["id"], item["id"])
        self.assertEqual(self.client.get(f"/api/sessions/{self.session.pk}").status_code, 404)
        self.assertEqual(self.client.get(f"/api/artifacts/{self.artifact.pk}/download").status_code, 404)
        download = self.client.get(item["artifacts"][0]["downloadUrl"])
        self.assertEqual(download.status_code, 200)
        async def read():
            return b"".join([chunk async for chunk in download])
        self.assertEqual(async_to_sync(read)(), b"published report")
        self.assertIn("attachment", download["Content-Disposition"])
        self.client.force_login(self.third)
        self.assertEqual(self.client.get(inbox).json()["notifications"], [])
        self.assertEqual(self.client.get(self.url).json()["deliverables"], [])
        self.assertEqual(self.client.get(item["artifacts"][0]["downloadUrl"]).status_code, 404)

    def test_self_submission_reaches_inbox_without_self_acceptance_notices(self):
        response = self.submit(recipientId=str(self.user.pk))
        self.assertEqual(response.status_code, 201, response.content)
        item = response.json()["deliverable"]
        inbox = f"/api/workspaces/{self.workspace.pk}/notifications"
        receipt = self.client.get(inbox).json()
        self.assertEqual((receipt["total"], receipt["unreadCount"]), (1, 1))
        notice = receipt["notifications"][0]
        self.assertEqual((notice["kind"], notice["event"]["type"]), ("deliverable", "deliverableSubmitted"))
        self.assertEqual(notice["event"]["changes"]["deliverableId"]["after"], item["id"])
        accepted = self.respond(item, action="accept", text="")
        self.assertEqual(accepted.status_code, 200, accepted.content)
        self.assertEqual(self.client.get(inbox).json()["total"], 1)
        self.client.force_login(self.other)
        self.assertEqual(self.client.get(inbox).json()["notifications"], [])

    def test_feedback_replacement_and_acceptance_preserve_revisions_and_issue_status(self):
        item = self.submit().json()["deliverable"]
        self.assertEqual(self.respond(item, action="accept", text="").status_code, 403)
        self.client.force_login(self.other)
        feedback = self.respond(item)
        self.assertEqual(feedback.status_code, 200, feedback.content)
        self.assertEqual(feedback.json()["version"], 3)
        self.assertEqual(feedback.json()["deliverable"]["responses"][0]["text"], "Please clarify the evidence")
        self.client.force_login(self.user)
        next_item = self.submit(version=3, summary="Revised result", replacesId=item["id"]).json()["deliverable"]
        self.assertEqual((next_item["revision"], next_item["replacesId"]), (2, item["id"]))
        self.assertEqual(IssueDeliverable.objects.get(pk=item["id"]).summary, "Selected result")
        self.assertEqual(self.respond(item, version=4).status_code, 409)
        self.client.force_login(self.other)
        accepted = self.respond(next_item, version=4, action="accept", text="Confirmed")
        self.assertEqual(accepted.status_code, 200, accepted.content)
        self.assertEqual(accepted.json()["version"], 5)
        self.assertEqual(self.respond(next_item, version=5, action="accept").status_code, 409)
        self.issue.refresh_from_db()
        self.assertEqual((self.issue.status, self.issue.assignee_id), ("todo", self.other.pk))
        self.assertEqual(list(IssueDeliverable.objects.order_by("revision").values_list("summary", flat=True)), ["Selected result", "Revised result"])
        self.assertEqual(IssueDeliverableResponse.objects.count(), 2)

    def test_conflict_unavailable_artifacts_and_recipient_changes_create_no_partial_results(self):
        item = self.submit().json()["deliverable"]
        self.assertEqual(self.submit().status_code, 409)
        self.assertEqual(self.submit(version=2, recipientId=str(self.third.pk), replacesId=item["id"]).status_code, 404)
        Artifact.objects.filter(pk=self.artifact.pk).update(status="deleted", deletedAt=timezone.now())
        self.assertEqual(self.submit(version=2).status_code, 403)
        self.assertEqual(IssueDeliverable.objects.count(), 1)
        self.assertEqual(IssueEvent.objects.count(), 1)

    def test_download_rechecks_current_membership_and_frozen_artifact_version(self):
        item = self.submit().json()["deliverable"]
        self.client.force_login(self.other)
        Artifact.objects.filter(pk=self.artifact.pk).update(contentGeneration=2)
        self.assertEqual(self.client.get(item["artifacts"][0]["downloadUrl"]).status_code, 404)
        Artifact.objects.filter(pk=self.artifact.pk).update(contentGeneration=1)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.other).delete()
        self.assertEqual(self.client.get(self.url).status_code, 404)
        self.assertEqual(self.client.get(item["artifacts"][0]["downloadUrl"]).status_code, 404)

    def test_private_issue_scope_prevents_unselected_members_and_recipients(self):
        Issue.objects.filter(pk=self.issue.pk).update(visibility="participants")
        self.assertEqual(self.submit(recipientId=str(self.third.pk)).status_code, 400)
        self.assertEqual(self.submit().status_code, 201)
        self.client.force_login(self.third)
        self.assertEqual(self.client.get(self.url).status_code, 404)

    def test_schema_csrf_archiving_foreign_execution_and_empty_content(self):
        for changes in ({"version": "1"}, {"extra": True}, {"executionId": "bad"},
                {"recipientId": "not-a-user"}, {"summary": "", "artifactIds": []},
                {"artifactIds": [self.artifact.pk, self.artifact.pk]}):
            with self.subTest(changes=changes):
                self.assertEqual(self.submit(**changes).status_code, 400)
        self.client.force_login(self.other)
        self.assertEqual(self.submit().status_code, 404)
        self.client.force_login(self.user)
        csrf = Client(enforce_csrf_checks=True)
        csrf.force_login(self.user)
        self.assertEqual(csrf.post(self.url, json.dumps({"version": 1, "executionId": str(self.execution.pk),
            "agentRunId": self.run.pk, "summary": "Result", "artifactIds": [], "recipientId": str(self.other.pk)}),
            content_type="application/json").status_code, 403)
        Issue.objects.filter(pk=self.issue.pk).update(archivedAt=timezone.now())
        self.assertEqual(self.submit().status_code, 409)
        self.assertFalse(IssueDeliverable.objects.exists())


class IssueDeliverableMigrationTests(TransactionTestCase):
    def test_forward_restores_only_missing_self_submission_receipts(self):
        previous = [("app_core", "0064_merge_environment_resources")]
        target = [("app_core", "0065_self_deliverable_receipts")]
        with isolated_migration_database(previous) as database:
            alias = database.alias
            old = MigrationExecutor(database).loader.project_state(previous).apps
            def create(model_name, **values):
                return old.get_model("app_core", model_name).objects.using(alias).create(**values)
            user = old.get_model("auth", "User").objects.using(alias).create(username="self-delivery-upgrade")
            other = old.get_model("auth", "User").objects.using(alias).create(username="other-recipient")
            workspace = create("Workspace", name="Existing", createdBy_id=user.pk)
            memberships = {person.pk: create("WorkspaceMembership", workspace_id=workspace.pk,
                user_id=person.pk, role="member") for person in (user, other)}
            issue = create("Issue", workspace_id=workspace.pk, creator_id=user.pk,
                title="Existing issue", version=4)
            with transaction.atomic(using=alias):
                session = create("Session", workspace_id=workspace.pk, owner_id=user.pk)
                environment = create("SessionEnvironment", session_id=session.pk)
                old.get_model("app_core", "Session").objects.using(alias).filter(pk=session.pk).update(
                    current_environment_id=environment.pk)
            model = create("ModelConfig", displayName="Migration model")
            run = create("AgentRun", workspace_id=workspace.pk, session_id=session.pk, user_id=user.pk,
                modelConfig_id=model.pk, membership_ref=memberships[user.pk].pk,
                environment_id=environment.pk, prompt="Synthetic input", status="completed")
            execution = create("IssueExecution", issue_id=issue.pk, issuer_id=user.pk,
                session_id=session.pk, agent_run_id=run.pk, issueVersion=1, issueTitle=issue.title)
            events = []
            for revision, recipient in enumerate((user, user, other), start=1):
                item = create("IssueDeliverable", issue_id=issue.pk, execution_id=execution.pk,
                    agent_run_id=run.pk, author_id=user.pk, recipient_id=recipient.pk,
                    revision=revision, summary=f"Result {revision}")
                events.append(create("IssueEvent", issue_id=issue.pk, actor_id=user.pk,
                    type="deliverableSubmitted", changes={"deliverableId": {"before": None, "after": str(item.pk)}}))
            create("IssueEvent", issue_id=issue.pk, actor_id=user.pk, type="deliverableAccepted",
                changes=events[0].changes)
            received_at = timezone.now()
            existing = create("IssueNotification", event_id=events[1].pk, recipient_id=user.pk,
                issueTitle="Original title", kind="deliverable", readAt=received_at, archivedAt=received_at)
            executor = MigrationExecutor(database)
            executor.migrate(target)
            new = executor.loader.project_state(target).apps
            notices = new.get_model("app_core", "IssueNotification").objects.using(alias)
            restored = notices.get(event_id=events[0].pk)
            self.assertEqual((restored.recipient_id, restored.kind, restored.createdAt),
                (user.pk, "deliverable", events[0].createdAt))
            self.assertEqual((restored.readAt, restored.archivedAt), (None, None))
            preserved = notices.get(pk=existing.pk)
            self.assertEqual((preserved.issueTitle, preserved.readAt, preserved.archivedAt, preserved.createdAt),
                ("Original title", received_at, received_at, existing.createdAt))
            self.assertEqual(notices.count(), 2)
            current_issue = new.get_model("app_core", "Issue").objects.using(alias).get(pk=issue.pk)
            self.assertEqual((current_issue.version, current_issue.status), (4, "todo"))
            migration = executor.loader.get_migration(*target[0])
            with database.schema_editor() as schema_editor:
                migration.operations[0].code(new, schema_editor)
            self.assertEqual(notices.count(), 2)

    def test_forward_preserves_existing_issue_and_notification_records(self):
        previous = [("app_core", "0062_issue_materials")]
        target = [("app_core", "0063_issue_deliverables_and_templates")]
        with isolated_migration_database(previous) as database:
            alias = database.alias
            old = MigrationExecutor(database).loader.project_state(previous).apps
            user = old.get_model("auth", "User").objects.using(alias).create(username="deliverable-upgrade")
            workspace = old.get_model("app_core", "Workspace").objects.using(alias).create(name="Existing", createdBy_id=user.pk)
            issue = old.get_model("app_core", "Issue").objects.using(alias).create(workspace_id=workspace.pk,
                creator_id=user.pk, title="Existing issue", version=4)
            event = old.get_model("app_core", "IssueEvent").objects.using(alias).create(issue_id=issue.pk,
                actor_id=user.pk, type="created", changes={})
            notice = old.get_model("app_core", "IssueNotification").objects.using(alias).create(
                event_id=event.pk, recipient_id=user.pk, issueTitle="Existing issue", kind="assigned")
            MigrationExecutor(database).migrate(target)
            new = MigrationExecutor(database).loader.project_state(target).apps
            current = new.get_model("app_core", "Issue").objects.using(alias).get(pk=issue.pk)
            self.assertEqual((current.title, current.version, current.visibility, current.templateKey,
                current.templateSnapshot, current.properties), ("Existing issue", 4, "workspace", "", {}, {}))
            self.assertEqual(new.get_model("app_core", "IssueNotification").objects.using(alias).get(pk=notice.pk).kind, "assigned")
            self.assertFalse(new.get_model("app_core", "IssueDeliverable").objects.using(alias).exists())
