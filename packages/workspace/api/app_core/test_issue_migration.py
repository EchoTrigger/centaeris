"""Issue tables upgrade an existing workspace without rewriting its authority."""
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class IssueMigrationTests(TransactionTestCase):
    previous = [("app_core", "0055_session_purge_intent")]
    target = [("app_core", "0056_issues")]

    def test_forward_preserves_workspace_and_can_persist_issue_activity(self):
        with isolated_migration_database(self.previous) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(self.previous).apps
            user = historical.get_model("auth", "User").objects.using(alias).create(username="issue-upgrade")
            workspace = historical.get_model("app_core", "Workspace").objects.using(alias).create(
                name="Existing workspace", createdBy_id=user.pk)
            member = historical.get_model("app_core", "WorkspaceMembership").objects.using(alias).create(
                workspace_id=workspace.pk, user_id=user.pk, role="owner")
            before = historical.get_model("app_core", "Workspace").objects.using(alias).get(pk=workspace.pk)
            MigrationExecutor(database).migrate(self.target)
            current = MigrationExecutor(database).loader.project_state(self.target).apps
            after = current.get_model("app_core", "Workspace").objects.using(alias).get(pk=workspace.pk)
            self.assertEqual({field.attname: getattr(after, field.attname) for field in after._meta.concrete_fields},
                {field.attname: getattr(before, field.attname) for field in before._meta.concrete_fields})
            self.assertTrue(current.get_model("app_core", "WorkspaceMembership").objects.using(alias).filter(pk=member.pk).exists())
            issue = current.get_model("app_core", "Issue").objects.using(alias).create(
                workspace_id=workspace.pk, creator_id=user.pk, title="First real issue")
            event = current.get_model("app_core", "IssueEvent").objects.using(alias).create(
                issue_id=issue.pk, actor_id=user.pk, type="created", changes={"title": {"before": None, "after": issue.title}})
            self.assertEqual(issue.version, 1)
            self.assertEqual(event.changes["title"]["after"], "First real issue")

