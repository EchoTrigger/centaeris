"""Both Workspace migration paths retain data when they converge."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class EnvironmentIssueMigrationTests(TransactionTestCase):
    serialized_rollback = True
    merged = [("app_core", "0064_merge_environment_resources")]

    def test_each_source_path_preserves_its_records_and_gains_the_other_capability(self):
        for source in ("0056_workspace_environment_resources", "0063_issue_deliverables_and_templates"):
            with self.subTest(source=source), isolated_migration_database([("app_core", source)]) as database:
                executor = MigrationExecutor(database)
                apps = executor.loader.project_state([("app_core", source)]).apps

                def manager(name):
                    return apps.get_model("app_core", name).objects.using(database.alias)

                user = apps.get_model("auth", "User").objects.using(database.alias).create(username="merged-owner")
                workspace = manager("Workspace").create(name="Retained workspace", createdBy_id=user.pk)
                with transaction.atomic(using=database.alias):
                    session = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk)
                    environment = manager("SessionEnvironment").create(session_id=session.pk)
                    manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
                names = ["Workspace", "Session", "SessionEnvironment"]
                if source == "0063_issue_deliverables_and_templates":
                    manager("Issue").create(workspace_id=workspace.pk, creator_id=user.pk, title="Retained issue")
                    names.append("Issue")
                before = {name: list(manager(name).order_by("pk").values()) for name in names}

                executor = MigrationExecutor(database)
                executor.migrate(self.merged)
                apps = executor.loader.project_state(self.merged).apps
                self.assertEqual(executor.loader.graph.leaf_nodes("app_core"), self.merged)
                for name, rows in before.items():
                    fields = tuple(rows[0])
                    self.assertEqual(list(manager(name).order_by("pk").values(*fields)), rows)
                current = manager("Workspace").get(pk=workspace.pk)
                self.assertEqual(current.execution_settings_revision, 1)
                added = manager("SessionEnvironment").create(session_id=session.pk)
                added.refresh_from_db(using=database.alias)
                self.assertEqual(added.resource_defaults, current.execution_resources)
                self.assertEqual(added.resource_defaults_revision, 1)
                issue = manager("Issue").create(workspace_id=workspace.pk, creator_id=user.pk, title="New issue")
                self.assertEqual(manager("Issue").get(pk=issue.pk).title, "New issue")
