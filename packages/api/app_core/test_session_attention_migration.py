"""Forward reconstruction preserves owner choices and acknowledged history."""
from django.db import transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .migration_testing import isolated_migration_database


class SessionAttentionMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0026_external_identity_login")]
    current = [("app_core", "0027_session_attention_watermarks")]

    def test_forward_reconstructs_watermarks_without_guessing_legacy_unread_origin(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            old = MigrationExecutor(database).loader.project_state(self.old).apps
            manager = lambda name: old.get_model("app_core", name).objects.using(alias)
            user = old.get_model("auth", "User").objects.using(alias).create(username="attention-migration")
            workspace = manager("Workspace").create(name="Attention upgrade", createdBy_id=user.pk)
            membership = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Attention")
            model = manager("ModelConfig").create(displayName="Attention")
            sessions = []
            for name, unread in [("read-history", False), ("unread-history", True), ("manual-empty", True)]:
                with transaction.atomic(using=alias):
                    session = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk,
                        agent_id=agent.pk, title=name, isUnread=unread)
                    environment = manager("SessionEnvironment").create(session_id=session.pk)
                    manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
                if name != "manual-empty":
                    run = manager("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
                        session_id=session.pk, environment_id=environment.pk, membership_ref=membership.pk,
                        modelConfig_id=model.pk, prompt="Synthetic history", status="completed")
                    for sequence, kind, payload in [(1, "agent_run_started", {"userObjective": "Synthetic history"}),
                        (2, "user_message", {"messageId": "message:user", "text": "Synthetic history", "attachments": []}),
                        (3, "checkpoint_ref", {"checkpointId": "checkpoint:question", "kind": "wait", "objectRef": "checkpoint:object",
                            "status": "paused_question", "payloadSha256": "sha256:" + "0" * 64, "payloadByteLength": 42, "updatedAtMs": 3}),
                        (4, "agent_run_completed", {"doneReason": "finalized"})]:
                        event = {"schemaVersion": "session.event.v1", "eventVersion": 1, "sequence": sequence,
                            "eventId": f"event:{run.pk}:{sequence}", "sessionId": session.pk, "turnId": "turn:upgrade",
                            "agentRunId": run.pk, "type": kind, "createdAtMs": sequence, "payload": payload}
                        manager("SessionEvent").create(eventId=event["eventId"], workspace_id=workspace.pk,
                            session_id=session.pk, agent_run_id=run.pk, sequence=sequence,
                            agent_run_sequence=sequence, projects_to_agent_run_stream=kind != "checkpoint_ref",
                            payload=event, createdAtMs=sequence)
                sessions.append(session)
            original_events = list(manager("SessionEvent").order_by("eventId").values())
            original_sessions = {row["id"]: row for row in manager("Session").values()}
            MigrationExecutor(database).migrate(self.current)
            upgraded = MigrationExecutor(database).loader.project_state(self.current).apps
            current = upgraded.get_model("app_core", "Session").objects.using(alias)
            read, unread, manual = [current.get(pk=session.pk) for session in sessions]
            self.assertEqual((read.latestAttentionSequence, read.readThroughSequence, read.isUnread), (4, 4, False))
            self.assertEqual((unread.latestAttentionSequence, unread.readThroughSequence, unread.isUnread), (4, 0, True))
            self.assertEqual((manual.latestAttentionSequence, manual.readThroughSequence, manual.isUnread), (0, 0, True))
            self.assertEqual([session.attentionRevision for session in (read, unread, manual)], [0, 0, 0])
            events = upgraded.get_model("app_core", "SessionEvent").objects.using(alias)
            self.assertEqual(list(events.order_by("eventId").values()), original_events)
            for session in current.all():
                original = original_sessions[session.pk]
                self.assertEqual((session.owner_id, session.createdAt, session.updatedAt),
                    (original["owner_id"], original["createdAt"], original["updatedAt"]))
            with self.assertRaisesRegex(RuntimeError, "session_attention_history_requires_forward_migration"):
                MigrationExecutor(database).migrate(self.old)
