"""Guide state is introduced without altering existing workspace records."""
from django.db import IntegrityError, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database


class WorkspaceGuideMigrationTests(TransactionTestCase):
    serialized_rollback = True
    previous = [("app_core", "0057_issue_notifications")]
    target = [("app_core", "0058_user_workspace_guide_state")]

    def test_forward_preserves_workspace_and_enforces_user_workspace_key(self):
        with isolated_migration_database(self.previous) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(self.previous).apps
            user = historical.get_model("auth", "User").objects.using(alias).create(username="guide-upgrade")
            workspace = historical.get_model("app_core", "Workspace").objects.using(alias).create(name="Existing", createdBy_id=user.pk)
            before = {field.attname: getattr(workspace, field.attname) for field in workspace._meta.concrete_fields}
            MigrationExecutor(database).migrate(self.target)
            current = MigrationExecutor(database).loader.project_state(self.target).apps
            after = current.get_model("app_core", "Workspace").objects.using(alias).get(pk=workspace.pk)
            self.assertEqual({field.attname: getattr(after, field.attname) for field in after._meta.concrete_fields}, before)
            states = current.get_model("app_core", "UserWorkspaceGuideState").objects.using(alias)
            self.assertEqual(states.count(), 0)
            state = states.create(user_id=user.pk, workspace_id=workspace.pk)
            self.assertIsNone(state.dismissedAt)
            state.dismissedAt = timezone.now()
            state.save(using=alias, update_fields=["dismissedAt"])
            self.assertEqual(states.get(pk=state.pk).dismissedAt, state.dismissedAt)
            with self.assertRaises(IntegrityError), transaction.atomic(using=alias):
                states.create(user_id=user.pk, workspace_id=workspace.pk)

    def test_manual_overrides_upgrade_preserves_dismissal_and_initializes_empty(self):
        previous = [("app_core", "0058_user_workspace_guide_state")]
        target = [("app_core", "0059_workspace_guide_manual_steps")]
        with isolated_migration_database(previous) as database:
            alias = database.alias
            historical = MigrationExecutor(database).loader.project_state(previous).apps
            user = historical.get_model("auth", "User").objects.using(alias).create(username="manual-upgrade")
            workspace = historical.get_model("app_core", "Workspace").objects.using(alias).create(name="Existing", createdBy_id=user.pk)
            dismissed = timezone.now()
            state = historical.get_model("app_core", "UserWorkspaceGuideState").objects.using(alias).create(
                user_id=user.pk, workspace_id=workspace.pk, dismissedAt=dismissed,
            )
            MigrationExecutor(database).migrate(target)
            current = MigrationExecutor(database).loader.project_state(target).apps
            states = current.get_model("app_core", "UserWorkspaceGuideState").objects.using(alias)
            upgraded = states.get(pk=state.pk)
            self.assertEqual(upgraded.dismissedAt, dismissed)
            self.assertEqual(upgraded.manualSteps, {})
            upgraded.manualSteps = {"issue": False}
            upgraded.save(using=alias, update_fields=["manualSteps"])
            self.assertEqual(states.get(pk=state.pk).manualSteps, {"issue": False})
            self.assertEqual(current.get_model("app_core", "Workspace").objects.using(alias).get(pk=workspace.pk).name, "Existing")
