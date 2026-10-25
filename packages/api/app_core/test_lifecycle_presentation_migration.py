"""Both released migration histories converge without replacing durable facts."""
import json
from pathlib import Path

from django.db import DatabaseError, transaction
from django.db.migrations.executor import MigrationExecutor
from django.db.migrations.loader import MigrationLoader
from django.test import SimpleTestCase, TransactionTestCase

from .environment_maintenance import _digest
from .environment_maintenance_authorization import sign_maintenance_authorization
from .migration_testing import isolated_migration_database
from .runtime_contract import authorization_digest, authorization_signature


LIFECYCLE = [("app_core", "0028_provider_maintenance_target")]
PRESENTATION = [("app_core", "0030_session_manual_unread")]
MERGED = [("app_core", "0031_merge_lifecycle_and_presentation")]
FIXTURES = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures"
KEY = "migration-history-test-key"


def fixture(relative):
    return json.loads((FIXTURES / relative).read_text(encoding="utf-8"))


class LifecyclePresentationGraphTests(SimpleTestCase):
    def test_one_upgrade_target_includes_both_existing_histories(self):
        loader = MigrationLoader(None)
        leaves = loader.graph.leaf_nodes("app_core")
        self.assertEqual(len(leaves), 1, "A deployment must have one unambiguous forward migration target")
        ancestors = set(loader.graph.forwards_plan(leaves[0]))
        self.assertTrue(set(LIFECYCLE + PRESENTATION) <= ancestors)


class LifecyclePresentationMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def seed_history(self, database, target):
        alias = database.alias
        apps = MigrationExecutor(database).loader.project_state(target).apps
        manager = lambda name: apps.get_model("app_core", name).objects.using(alias)
        user = apps.get_model("auth", "User").objects.using(alias).create(username="merged-history-owner")
        workspace = manager("Workspace").create(name="Preserved workspace", createdBy_id=user.pk)
        member = manager("WorkspaceMembership").create(workspace_id=workspace.pk, user_id=user.pk, role="owner")
        agent = manager("Agent").create(workspace_id=workspace.pk, owner_id=user.pk, name="Preserved agent")
        model = manager("ModelConfig").create(displayName="Preserved model")
        attention = ({"isManuallyUnread": True, "latestAttentionSequence": 4,
            "readThroughSequence": 2, "attentionRevision": 7} if target == PRESENTATION else {"isUnread": True})
        with transaction.atomic(using=alias):
            session = manager("Session").create(workspace_id=workspace.pk, owner_id=user.pk,
                agent_id=agent.pk, title="Preserved session", **attention)
            environment = manager("SessionEnvironment").create(session_id=session.pk)
            manager("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
        run = manager("AgentRun").create(workspace_id=workspace.pk, user_id=user.pk,
            session_id=session.pk, environment_id=environment.pk, membership_ref=member.pk,
            modelConfig_id=model.pk, prompt="Preserved objective", status="completed")
        event = {"schemaVersion": "session.event.v1", "eventVersion": 1, "sequence": 4,
            "eventId": f"event:{run.pk}:completed", "sessionId": session.pk, "turnId": "turn:history",
            "agentRunId": run.pk, "type": "agent_run_completed", "createdAtMs": 4,
            "payload": {"doneReason": "finalized"}}
        manager("SessionEvent").create(eventId=event["eventId"], workspace_id=workspace.pk,
            session_id=session.pk, agent_run_id=run.pk, sequence=4, agent_run_sequence=4,
            projects_to_agent_run_stream=True, payload=event, createdAtMs=4)
        payload = fixture("agent_run_authorization/v1/valid.json") | {
            "id": "authorization-merged-history", "workspaceId": workspace.pk, "userId": str(user.pk),
            "sessionId": session.pk, "agentId": agent.pk, "agentRunId": run.pk, "modelConfigRef": model.pk}
        authorization = manager("AgentRunAuthorization").create(id=payload["id"], agent_run_id=run.pk,
            payload=payload, digest=authorization_digest(payload), signature=authorization_signature(payload, KEY))
        request = {"program": "/bin/true", "args": [], "cwd": "/app", "timeoutMs": 1000}
        operation = manager("EnvironmentMaintenanceOperation").create(operation_id="operation-native-history",
            workspace_id=workspace.pk, session_id=session.pk, environment_id=environment.pk,
            instance_id="instance_" + "a" * 64, container_id="b" * 64, host_id="native-history",
            allocation_execution_id="native-origin", profile_digest="sha256:" + "c" * 64,
            request=request, request_digest=_digest(request))
        grant_payload = fixture("environment_maintenance_authorization/v1/valid.json")["payload"]
        grant_payload.update(workspaceId=workspace.pk, sessionId=session.pk, environmentId=environment.pk,
            controllerOwner={"kind": "maintenanceOperation", "operationId": operation.pk},
            requestDigest=operation.request_digest)
        grant = manager("EnvironmentMaintenanceGrant").create(id="grant-native-history", operation_id=operation.pk,
            previous_controller_epoch=grant_payload["controllerEpoch"] - 1,
            controller_epoch=grant_payload["controllerEpoch"],
            controller_lease_digest=grant_payload["controllerLeaseDigest"],
            controller_purpose=grant_payload["controllerPurpose"], expires_at_ms=grant_payload["expiresAtMs"],
            payload=grant_payload, digest=_digest(grant_payload),
            signature=sign_maintenance_authorization(grant_payload, KEY))
        manager("EnvironmentMaintenanceOperation").filter(pk=operation.pk).update(current_grant_id=grant.pk)
        ids = {"workspace": workspace.pk, "session": session.pk, "environment": environment.pk,
            "agent": agent.pk, "run": run.pk, "authorization": authorization.pk}
        if target == LIFECYCLE:
            self.create_provider_operation(manager, ids)
            self.create_publication(manager, ids)
        else:
            manager("PresentationChange").create(key="session:" + session.pk + ":state", revision=11)
        preserved = ["Workspace", "WorkspaceMembership", "Agent", "SessionEnvironment", "AgentRun",
            "SessionEvent", "AgentRunAuthorization", "EnvironmentMaintenanceOperation", "EnvironmentMaintenanceGrant"]
        preserved += (["WorkspaceSnapshotPublication"] if target == LIFECYCLE
            else ["PresentationChange", "PresentationChangeOutbox"])
        before = {name: list(manager(name).order_by("pk").values()) for name in preserved}
        session.refresh_from_db(using=alias)
        return ids, before, (session.owner_id, session.createdAt, session.updatedAt)

    def create_provider_operation(self, manager, ids):
        provider_target = fixture("provider_environment_maintenance_authorization/v1/valid.json")["payload"]["providerTarget"]
        request = {"program": "/bin/true", "args": [], "cwd": "/app", "timeoutMs": 1000}
        return manager("EnvironmentMaintenanceOperation").create(operation_id="operation-provider-history",
            workspace_id=ids["workspace"], session_id=ids["session"], environment_id=ids["environment"],
            profile_digest="sha256:" + "d" * 64, request=request, request_digest=_digest(request),
            provider_target=provider_target)

    def create_publication(self, manager, ids):
        return manager("WorkspaceSnapshotPublication").create(id="publication-history", session_id=ids["session"],
            agent_run_id=ids["run"], authorization_id=ids["authorization"], purpose="sessionCommit",
            purpose_key="e" * 64, checkpoint_id="", storage_key="preserved/snapshot", request_digest="sha256:" + "f" * 64,
            generation=1, encoding="frameV1", snapshot_sha256="sha256:" + "a" * 64,
            snapshot_size_bytes=12, expanded_size_bytes=3, file_count=1, state="pending")

    def assert_upgrade(self, target):
        with isolated_migration_database(target) as database:
            self.assertEqual(database.vendor, "postgresql", "Durable protection requires real PostgreSQL")
            alias = database.alias
            ids, before, session_identity = self.seed_history(database, target)
            MigrationExecutor(database).migrate(MERGED)
            executor = MigrationExecutor(database)
            executor.loader.check_consistent_history(database)
            self.assertEqual(executor.migration_plan(MERGED), [])
            apps = executor.loader.project_state(MERGED).apps
            manager = lambda name: apps.get_model("app_core", name).objects.using(alias)
            for name, expected in before.items():
                actual = list(manager(name).order_by("pk").values())
                if name == "EnvironmentMaintenanceOperation" and target == PRESENTATION:
                    for row in actual:
                        self.assertIsNone(row.pop("provider_target"))
                self.assertEqual(actual, expected, name + " history changed during forward integration")
            session = manager("Session").get(pk=ids["session"])
            self.assertEqual((session.owner_id, session.createdAt, session.updatedAt), session_identity)
            self.assertEqual((session.isManuallyUnread, session.latestAttentionSequence,
                session.readThroughSequence, session.attentionRevision),
                (True, 4, 2, 7) if target == PRESENTATION else (True, 4, 0, 0))
            if target == PRESENTATION:
                self.create_provider_operation(manager, ids)
                self.create_publication(manager, ids)
            operation = manager("EnvironmentMaintenanceOperation").get(pk="operation-provider-history")
            with self.assertRaises(DatabaseError), transaction.atomic(using=alias):
                manager("EnvironmentMaintenanceOperation").filter(pk=operation.pk).update(
                    provider_target=operation.provider_target | {"sandboxId": "different-sandbox"})
            with self.assertRaises(DatabaseError), transaction.atomic(using=alias):
                manager("WorkspaceSnapshotPublication").filter(pk="publication-history").update(storage_key="different/snapshot")
            # A real write must still invalidate navigation after both histories converge.
            from .presentation_changes import drain_pending_changes, read_presentation_versions
            drain_pending_changes(using=alias)
            versions = read_presentation_versions(ids["workspace"], ids["agent"], ids["session"], using=alias)
            manager("Session").filter(pk=ids["session"]).update(isManuallyUnread=False)
            drain_pending_changes(using=alias)
            changed = read_presentation_versions(ids["workspace"], ids["agent"], ids["session"], using=alias)
            self.assertEqual({kind for kind in versions if versions[kind] != changed[kind]}, {"navigation"})

    def test_lifecycle_history_upgrades_without_losing_responsibility_or_signed_facts(self):
        self.assert_upgrade(LIFECYCLE)

    def test_presentation_history_upgrades_without_losing_attention_or_signed_facts(self):
        self.assert_upgrade(PRESENTATION)
