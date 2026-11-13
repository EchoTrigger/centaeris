"""Existing issue history survives inbox introduction without a backfill."""
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class NotificationMigrationTests(TransactionTestCase):
    previous = [("app_core", "0056_issues")]
    target = [("app_core", "0057_issue_notifications")]

    def test_forward_preserves_history_without_creating_notifications(self):
        with isolated_migration_database(self.previous) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(self.previous).apps
            user = historical.get_model("auth", "User").objects.using(alias).create(username="inbox-upgrade")
            workspace = historical.get_model("app_core", "Workspace").objects.using(alias).create(name="Existing", createdBy_id=user.pk)
            membership = historical.get_model("app_core", "WorkspaceMembership").objects.using(alias).create(workspace_id=workspace.pk, user_id=user.pk)
            issue = historical.get_model("app_core", "Issue").objects.using(alias).create(workspace_id=workspace.pk, creator_id=user.pk, assignee_id=user.pk, title="Existing issue")
            event = historical.get_model("app_core", "IssueEvent").objects.using(alias).create(issue_id=issue.pk, actor_id=user.pk, type="created", changes={"assigneeId": {"before": None, "after": str(user.pk)}})
            records = [(name, record.pk, {field.attname: getattr(record, field.attname) for field in record._meta.concrete_fields}) for name, record in (("Workspace", workspace), ("WorkspaceMembership", membership), ("Issue", issue), ("IssueEvent", event))]
            MigrationExecutor(database).migrate(self.target)
            current = MigrationExecutor(database).loader.project_state(self.target).apps
            for name, key, before in records:
                after = current.get_model("app_core", name).objects.using(alias).get(pk=key)
                self.assertEqual({field.attname: getattr(after, field.attname) for field in after._meta.concrete_fields}, before)
            notifications = current.get_model("app_core", "IssueNotification").objects.using(alias)
            self.assertEqual(notifications.count(), 0)
            receipt = notifications.create(event_id=event.pk, recipient_id=user.pk, issueTitle=issue.title, kind="assigned")
            self.assertIsNone(receipt.readAt)
            self.assertIsNone(receipt.archivedAt)
