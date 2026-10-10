"""Forward upgrade preserves existing finite grants and original input bytes."""
from datetime import timedelta

from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database


class PersistentDelegationMigrationTests(TransactionTestCase):
    serialized_rollback = True
    migrate_from = [("app_core", "0001_initial")]
    migrate_to = [("app_core", "0002_persistent_app_delegations")]

    def test_existing_expiry_credentials_origins_and_input_body_are_preserved(self):
        with isolated_migration_database(self.migrate_from) as isolated:
            alias = isolated.alias
            old = MigrationExecutor(isolated).loader.project_state(self.migrate_from).apps
            user = old.get_model("auth", "User").objects.using(alias).create(username="upgrade-owner")
            workspace = old.get_model("app_core", "Workspace").objects.using(alias).create(name="Upgrade", createdBy=user)
            membership = old.get_model("app_core", "WorkspaceMembership").objects.using(alias).create(workspace=workspace, user=user, role="owner")
            app = old.get_model("app_core", "BusinessApplication").objects.using(alias).create(name="Upgrade app", status="active", created_by=user)
            definition = old.get_model("app_core", "AgentDefinition").objects.using(alias).create(workspace=workspace,
                name="Upgrade assistant", created_by=user)
            agent = old.get_model("app_core", "Agent").objects.using(alias).create(workspace=workspace, owner=user, name="Native Agent")
            session = old.get_model("app_core", "Session").objects.using(alias).create(workspace=workspace, owner=user, agent=agent)
            grants = old.get_model("app_core", "UserAppDelegation").objects.using(alias)
            grants.create(user=user, app=app, workspace=workspace, definition=definition,
                membership_ref=membership.pk, scopes=["sessions:read"], token_digest="sha256:" + "a" * 64,
                expires_at=timezone.now() + timedelta(hours=12))
            grants.create(user=user, app=app, workspace=workspace, definition=definition,
                membership_ref=membership.pk, scopes=["sessions:read"], token_digest="sha256:" + "b" * 64,
                expires_at=timezone.now() - timedelta(hours=1), revoked_at=timezone.now())
            inputs = old.get_model("app_core", "AgentInput").objects.using(alias)
            inputs.create(agent=agent, session=session, membership_ref=membership.pk, input_id="existing-input",
                sequence=1, accepted_source_sequence=0, body="  原始业务输入\nUnicode: 😀  ", created_at_ms=123)
            old_grants = list(grants.order_by("pk").values())
            old_inputs = list(inputs.order_by("pk").values())
            executor = MigrationExecutor(isolated)
            executor.migrate(self.migrate_to)
            new = executor.loader.project_state(self.migrate_to).apps
            upgraded_grants = list(new.get_model("app_core", "UserAppDelegation").objects.using(alias).order_by("pk").values())
            upgraded_inputs = list(new.get_model("app_core", "AgentInput").objects.using(alias).order_by("pk").values())
            for previous, current in zip(old_grants, upgraded_grants, strict=True):
                self.assertEqual({key: current[key] for key in previous}, previous)
                self.assertIsNone(current["agent_id"])
                self.assertEqual(current["credential_version"], 1)
            for previous, current in zip(old_inputs, upgraded_inputs, strict=True):
                self.assertEqual({key: current[key] for key in previous}, previous)
                self.assertEqual((current["acting_app_id"], current["app_delegation_id"], current["credential_version"]),
                                 (None, None, None))
            self.assertFalse(new.get_model("app_core", "AppDelegationAuditEvent").objects.using(alias).exists())
