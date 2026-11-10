"""Display epoch removal preserves facts, delivered positions and useful dirtiness."""
from unittest import skipUnless

from django.db import connection, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


@skipUnless(connection.vendor == "postgresql", "presentation triggers require PostgreSQL")
class PresentationAuthorityRemovalMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0028_presentation_changes")]
    current = [("app_core", "0029_remove_presentation_authority")]

    def test_forward_removal_preserves_sources_and_each_remaining_display_scope(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            apps = MigrationExecutor(database).loader.project_state(self.old).apps
            manager = lambda name: apps.get_model("app_core", name).objects.using(alias)
            user = apps.get_model("auth", "User").objects.using(alias).create(username="remove-display-epoch")
            workspace = manager("Workspace").create(name="Epoch upgrade", createdBy_id=user.pk)
            membership = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Epoch")
            model = manager("ModelConfig").create(displayName="Epoch")
            with transaction.atomic(using=alias):
                session = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk,
                    agent_id=agent.pk, title="Retain transcript", isUnread=True,
                    latestAttentionSequence=1, readThroughSequence=1, attentionRevision=3)
                environment = manager("SessionEnvironment").create(session_id=session.pk)
                manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
            run = manager("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
                session_id=session.pk, environment_id=environment.pk, membership_ref=membership.pk,
                modelConfig_id=model.pk, prompt="Persistent fixture", status="completed")
            event = manager("SessionEvent").create(eventId="epoch:committed", workspace_id=workspace.pk,
                session_id=session.pk, agent_run_id=run.pk, sequence=1, agent_run_sequence=1,
                projects_to_agent_run_stream=True, payload={"type": "assistant_message",
                    "payload": {"modelMarkdown": "Persistent fixture"}}, createdAtMs=1)
            source_names = ("Agent", "Session", "SessionEnvironment", "AgentRun", "SessionEvent", "WorkspaceMembership")
            facts = {name: list(manager(name).order_by("pk").values()) for name in source_names}
            keys = {"navigation": f"agent:{agent.pk}:navigation", "history": f"agent:{agent.pk}:history",
                "work": f"agent:{agent.pk}:work", "state": f"session:{session.pk}:state",
                "context": f"session:{session.pk}:context"}
            for index, key in enumerate(keys.values(), start=11):
                manager("PresentationChange").create(key=key, revision=index)
            manager("PresentationChange").create(key="authority", revision=77)
            self.assertTrue(manager("PresentationChangeOutbox").filter(key="authority").exists())
            versions = list(manager("PresentationChange").exclude(key="authority").order_by("key").values())
            pending = list(manager("PresentationChangeOutbox").exclude(key="authority").order_by("id").values())

            MigrationExecutor(database).migrate(self.current)
            apps = MigrationExecutor(database).loader.project_state(self.current).apps
            for name in source_names:
                self.assertEqual(list(manager(name).order_by("pk").values()), facts[name])
            self.assertEqual(list(manager("PresentationChange").order_by("key").values()), versions)
            self.assertEqual(list(manager("PresentationChangeOutbox").order_by("id").values()), pending)
            # This migration predates workspace navigation: read its historical keys.
            positions = dict(manager("PresentationChange").values_list("key", "revision"))
            self.assertEqual({kind: positions.get(key, 0) for kind, key in keys.items()},
                {kind: index for index, kind in enumerate(keys, start=11)})

            # Auth-only writes produce no display work, while the same upgraded
            # database still observes direct source SQL for all five data views.
            manager("PresentationChangeOutbox").all().delete()
            apps.get_model("auth", "User").objects.using(alias).filter(pk=user.pk).update(is_active=False)
            manager("WorkspaceMembership").filter(pk=membership.pk).update(role="admin")
            self.assertEqual(manager("PresentationChangeOutbox").count(), 0)
            with database.cursor() as cursor:
                cursor.execute('UPDATE app_core_session SET title=%s WHERE id=%s', ["Renamed", session.pk])
                cursor.execute('UPDATE app_core_agentrun SET status=%s WHERE id=%s', ["running", run.pk])
                cursor.execute('UPDATE app_core_agent SET name=%s WHERE id=%s', ["Renamed agent", agent.pk])
            self.assertEqual(set(manager("PresentationChangeOutbox").values_list("key", flat=True)), set(keys.values()))
            manager("PresentationChangeOutbox").all().delete()
            with database.cursor() as cursor:
                cursor.execute('UPDATE app_core_session SET status=%s,"deletedAt"=clock_timestamp() WHERE id=%s',
                    ["deleted", session.pk])
            self.assertEqual(set(manager("PresentationChangeOutbox").values_list("key", flat=True)),
                {keys[kind] for kind in ("navigation", "state", "history", "work")})
            self.assertEqual(manager("SessionEvent").get(pk=event.pk).payload, event.payload)
