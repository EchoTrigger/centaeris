"""Forward encoding metadata preserves old objects and signed authorizations."""
import hashlib
import json
from pathlib import Path

from django.db import IntegrityError, transaction
from django.db.migrations.executor import MigrationExecutor
from django.test import SimpleTestCase, TransactionTestCase

from .migration_testing import isolated_migration_database
from .runtime_contract import authorization_digest, authorization_signature
from .test_agent_run_authorization import fixture, KEY


class WorkspaceSnapshotEncodingDefaultTests(SimpleTestCase):
    def test_new_empty_session_identifies_its_no_object_encoding(self):
        from .models import Session
        self.assertEqual(Session().workspaceSnapshotEncoding, "frameV1")


class WorkspaceSnapshotEncodingMigrationTests(TransactionTestCase):
    serialized_rollback = True
    old = [("app_core", "0018_environment_release_receipts")]
    current = [("app_core", "0019_workspace_snapshot_encoding")]

    def test_forward_preserves_existing_rows_and_grants_and_reverse_refuses_compressed_history(self):
        with isolated_migration_database(self.old) as database:
            alias = database.alias
            apps = MigrationExecutor(database).loader.project_state(self.old).apps
            model = lambda name: apps.get_model("app_core", name).objects.using(alias)
            user = apps.get_model("auth", "User").objects.using(alias).create(username="encoding-migration")
            workspace = model("Workspace").create(name="Stored snapshots", createdBy_id=user.pk)
            member = model("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
            agent = model("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Retained")
            config = model("ModelConfig").create(displayName="Retained")
            sessions = []
            for _ in range(2):
                with transaction.atomic(using=alias):
                    session = model("Session").create(workspace_id=workspace.pk, owner_id=user.pk, agent_id=agent.pk)
                    environment = model("SessionEnvironment").create(session_id=session.pk)
                    model("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
                sessions.append(session)
            run = model("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
                session_id=sessions[0].pk,
                environment_id=model("Session").get(pk=sessions[0].pk).current_environment_id,
                membership_ref=member.pk,
                modelConfig_id=config.pk, prompt="retained", status="completed")
            payload = fixture() | {"id": "authorization-" + run.pk, "workspaceId": workspace.pk,
                "userId": str(user.pk), "sessionId": sessions[0].pk, "agentId": agent.pk,
                "agentRunId": run.pk, "modelConfigRef": config.pk}
            model("AgentRunAuthorization").create(id=payload["id"], agent_run_id=run.pk,
                payload=payload, digest=authorization_digest(payload), signature=authorization_signature(payload, KEY))
            vectors = json.loads((Path(__file__).resolve().parents[3] /
                "tests/workspace/fixtures/workspace_archive/v1/valid.json").read_text(encoding="utf-8"))
            legacy = bytes.fromhex(vectors["frameV1Hex"])
            model("Session").filter(pk=sessions[0].pk).update(workspaceGeneration=1,
                workspaceStorageKey="retained/unchanged.snapshot", workspaceSnapshotSha256="sha256:" + hashlib.sha256(legacy).hexdigest(),
                workspaceSnapshotSizeBytes=len(legacy), workspaceExpandedSizeBytes=len(bytes.fromhex(vectors["bodyHex"])),
                workspaceFileCount=len(vectors["manifest"]["files"]), workspaceLastAdvancedAgentRun_id=run.pk)
            before = list(model("Session").order_by("pk").values())
            grants = list(model("AgentRunAuthorization").values())
            MigrationExecutor(database).migrate(self.current)
            upgraded = MigrationExecutor(database).loader.project_state(self.current).apps
            rows = upgraded.get_model("app_core", "Session").objects.using(alias)
            for after, previous in zip(rows.order_by("pk").values(), before, strict=True):
                self.assertEqual(after.pop("workspaceSnapshotEncoding"), "frameV1")
                self.assertEqual(after, previous)
            self.assertEqual(list(model("AgentRunAuthorization").values()), grants)
            # Metadata-only rollback is safe while every row still uses the old format.
            MigrationExecutor(database).migrate(self.old)
            self.assertEqual(list(model("Session").order_by("pk").values()), before)
            MigrationExecutor(database).migrate(self.current)
            for session, value in [(sessions[0], "unknown"), (sessions[1], "tarZstdV1")]:
                with self.assertRaises(IntegrityError), transaction.atomic(using=alias):
                    rows.filter(pk=session.pk).update(workspaceSnapshotEncoding=value)
            archive = bytes.fromhex(vectors["tarZstdV1Hex"])
            rows.filter(pk=sessions[0].pk).update(workspaceSnapshotEncoding="tarZstdV1",
                workspaceSnapshotSha256="sha256:" + hashlib.sha256(archive).hexdigest(), workspaceSnapshotSizeBytes=len(archive))
            with self.assertRaisesRegex(RuntimeError, "workspace_snapshot_encoding_requires_forward_migration"):
                MigrationExecutor(database).migrate(self.old)
            self.assertEqual(rows.get(pk=sessions[0].pk).workspaceSnapshotEncoding, "tarZstdV1")
            self.assertEqual(list(model("AgentRunAuthorization").values()), grants)
