"""The additive custom-avatar migration preserves published private profile data."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class AgentAvatarMigrationTests(TransactionTestCase):
    old = [("app_core", "0031_workspace_resource_scope")]
    current = [("app_core", "0032_agent_avatar_image")]

    def test_forward_keeps_owners_memberships_profiles_and_history_with_no_custom_avatar(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            previous = MigrationExecutor(database).loader.project_state(self.old).apps
            manager = lambda name: previous.get_model("app_core", name).objects.using(alias)
            user = previous.get_model("auth", "User").objects.using(alias).create(username="avatar-upgrade", password="preserved-hash", is_superuser=False)
            workspace = manager("Workspace").create(name="Avatar upgrade", createdBy_id=user.pk)
            membership = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            for kind in ("centaeris", "banana"):
                agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Legacy " + kind, avatar_kind=kind, description="Retained overview", instructions="Retained instructions")
                with transaction.atomic(using=alias):
                    session = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk, title="Legacy history")
                    environment = manager("SessionEnvironment").create(session_id=session.pk)
                    manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
            agents = list(manager("Agent").order_by("id").values())
            sessions = list(manager("Session").order_by("id").values())
            original_user = previous.get_model("auth", "User").objects.using(alias).values().get(pk=user.pk)
            original_membership = manager("WorkspaceMembership").values().get(pk=membership.pk)
            MigrationExecutor(database).migrate(self.current)
            updated = MigrationExecutor(database).loader.project_state(self.current).apps
            rows = list(updated.get_model("app_core", "Agent").objects.using(alias).order_by("id").values())
            self.assertEqual(len(rows), len(agents))
            for before, after in zip(agents, rows, strict=True):
                self.assertIsNone(after.pop("avatar_image"))
                self.assertEqual(after.pop("avatar_digest"), "")
                self.assertEqual(after, before)
            self.assertEqual(list(updated.get_model("app_core", "Session").objects.using(alias).order_by("id").values()), sessions)
            self.assertEqual(updated.get_model("auth", "User").objects.using(alias).values().get(pk=user.pk), original_user)
            self.assertEqual(updated.get_model("app_core", "WorkspaceMembership").objects.using(alias).values().get(pk=membership.pk), original_membership)
