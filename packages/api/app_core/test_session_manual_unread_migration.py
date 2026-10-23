"""A forward column rename preserves owner choices and live navigation invalidation."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database
from .presentation_changes import agent_change_key, drain_pending_changes, read_presentation_versions, session_change_key
from .session_attention import session_attention_snapshot


class SessionManualUnreadMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0029_remove_presentation_authority")]
    current = [("app_core", "0030_session_manual_unread")]

    def test_forward_preserves_attention_data_and_new_column_still_invalidates_navigation(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            old = MigrationExecutor(database).loader.project_state(self.old).apps
            manager = lambda name: old.get_model("app_core", name).objects.using(alias)
            user = old.get_model("auth", "User").objects.using(alias).create(username="manual-unread-upgrade")
            workspace = manager("Workspace").create(name="Manual unread upgrade", createdBy_id=user.pk)
            membership = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Upgrade")
            model = manager("ModelConfig").create(displayName="Manual unread upgrade")
            expected = {}
            for title, manual, latest, through, revision in [
                ("manual", True, 10, 10, 9), ("automatic", False, 10, 4, 3), ("read", False, 10, 10, 6),
            ]:
                with transaction.atomic(using=alias):
                    session = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk,
                        agent_id=agent.pk, title=title, isUnread=manual, latestAttentionSequence=latest,
                        readThroughSequence=through, attentionRevision=revision)
                    environment = manager("SessionEnvironment").create(session_id=session.pk)
                    manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
                run = manager("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
                    session_id=session.pk, environment_id=environment.pk, membership_ref=membership.pk,
                    modelConfig_id=model.pk, prompt="Synthetic history", status="completed")
                event = {"schemaVersion": "session.event.v1", "eventVersion": 1, "sequence": latest,
                    "eventId": f"event:{run.pk}:completed", "sessionId": session.pk, "turnId": "turn:upgrade",
                    "agentRunId": run.pk, "type": "agent_run_completed", "createdAtMs": latest,
                    "payload": {"doneReason": "finalized"}}
                manager("SessionEvent").create(eventId=event["eventId"], workspace_id=workspace.pk,
                    session_id=session.pk, agent_run_id=run.pk, sequence=latest, agent_run_sequence=latest,
                    projects_to_agent_run_stream=True, payload=event, createdAtMs=latest)
                session.refresh_from_db(using=alias)
                expected[session.pk] = (manual, latest, through, revision, user.pk, session.createdAt, session.updatedAt)
            original_events = list(manager("SessionEvent").order_by("eventId").values())
            MigrationExecutor(database).migrate(self.current)
            upgraded = MigrationExecutor(database).loader.project_state(self.current).apps
            current = upgraded.get_model("app_core", "Session").objects.using(alias)
            events = upgraded.get_model("app_core", "SessionEvent").objects.using(alias)
            self.assertEqual(list(events.order_by("eventId").values()), original_events)
            for session in current.all():
                self.assertEqual((session.isManuallyUnread, session.latestAttentionSequence,
                    session.readThroughSequence, session.attentionRevision, session.owner_id,
                    session.createdAt, session.updatedAt), expected[session.pk])
                manual, latest, through, revision = expected[session.pk][:4]
                self.assertEqual(session_attention_snapshot(session), {
                    "isManuallyUnread": manual, "isUnread": manual or latest > through,
                    "latestAttentionSequence": str(latest), "readThroughSequence": str(through),
                    "attentionRevision": str(revision),
                })
                # Observe the physical schema, not an ORM alias to an old column.
                with database.cursor() as cursor:
                    cursor.execute('SELECT "isManuallyUnread" FROM app_core_session WHERE id=%s', [session.pk])
                    self.assertEqual(cursor.fetchone()[0], expected[session.pk][0])
            session_id = next(iter(expected))
            drain_pending_changes(using=alias)
            before = read_presentation_versions(workspace.pk, agent.pk, session_id, using=alias)
            current.filter(pk=session_id).update(isManuallyUnread=False)
            drain_pending_changes(using=alias)
            after = read_presentation_versions(workspace.pk, agent.pk, session_id, using=alias)
            self.assertEqual({kind for kind in before if before[kind] != after[kind]}, {"navigation"})
            # Reversing this rename restores only the Session capture function,
            # not the superseded authority-producing apply function from 0028.
            MigrationExecutor(database).migrate(self.old)
            reverted = MigrationExecutor(database).loader.project_state(self.old).apps
            before = read_presentation_versions(workspace.pk, agent.pk, session_id, using=alias)
            reverted.get_model("app_core", "Session").objects.using(alias).filter(pk=session_id).update(isUnread=True)
            pending = reverted.get_model("app_core", "PresentationChangeOutbox").objects.using(alias)
            self.assertEqual(set(pending.values_list("key", flat=True)), {agent_change_key(agent.pk, "navigation")})
            drain_pending_changes(using=alias)
            after = read_presentation_versions(workspace.pk, agent.pk, session_id, using=alias)
            self.assertEqual({kind for kind in before if before[kind] != after[kind]}, {"navigation"})
            # A lifecycle change exercised the old apply function's authority
            # branch, so this proves reversal did not resurrect that broadcast.
            sessions = reverted.get_model("app_core", "Session").objects.using(alias)
            for status, deleted_at in [("deleted", timezone.now()), ("active", None)]:
                sessions.filter(pk=session_id).update(status=status, deletedAt=deleted_at)
                self.assertEqual(set(pending.values_list("key", flat=True)), {
                    agent_change_key(agent.pk, kind) for kind in ("navigation", "history", "work")
                } | {session_change_key(session_id, "state")})
                drain_pending_changes(using=alias)
            MigrationExecutor(database).migrate(self.current)
            forward = MigrationExecutor(database).loader.project_state(self.current).apps
            session = forward.get_model("app_core", "Session").objects.using(alias).get(pk=session_id)
            self.assertTrue(session.isManuallyUnread)
            self.assertEqual(session.attentionRevision, expected[session_id][3])
            events = forward.get_model("app_core", "SessionEvent").objects.using(alias)
            self.assertEqual(list(events.order_by("eventId").values()), original_events)
