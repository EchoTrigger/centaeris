"""The bounded active-Run index upgrades and reverses without changing receipts."""

from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database
from .models import AgentRun


class RunAdmissionMigrationTests(TransactionTestCase):
    serialized_rollback = True
    migrate_from = [("app_core", "0009_storage_gc_backoff")]
    migrate_to = [("app_core", "0010_active_run_admission")]

    def test_forward_index_covers_active_runs_and_reverse_preserves_all_run_and_receipt_facts(self):
        with isolated_migration_database(self.migrate_from) as database:
            self.assertEqual(database.vendor, "postgresql")
            historical = MigrationExecutor(database).loader.project_state(self.migrate_from).apps
            alias = database.alias
            user = historical.get_model("auth", "User").objects.using(alias).create(username="admission-upgrade")
            workspace = historical.get_model("app_core", "Workspace").objects.using(alias).create(
                name="Admission upgrade", createdBy=user)
            membership = historical.get_model("app_core", "WorkspaceMembership").objects.using(alias).create(
                workspace=workspace, user=user, role="owner")
            agent = historical.get_model("app_core", "Agent").objects.using(alias).create(
                workspace=workspace, owner=user, name="Admission")
            session = historical.get_model("app_core", "Session").objects.using(alias).create(
                workspace=workspace, owner=user, agent=agent)
            model = historical.get_model("app_core", "ModelConfig").objects.using(alias).create(displayName="Admission")
            Run = historical.get_model("app_core", "AgentRun")
            runs = Run.objects.using(alias)
            receipts = historical.get_model("app_core", "HostedOperationReceipt").objects.using(alias)
            now = timezone.now()
            cases = [("queued-new", "queued", None), ("queued-recovered", "queued", now),
                     ("running", "running", now), ("completed", "completed", now),
                     ("failed", "failed", now), ("cancelled", "cancelled", now)]
            for identity, state, started in cases:
                run = runs.create(
                    id=identity, turn_id=f"turn-{identity}", membership_ref=membership.pk,
                    workspace=workspace, session=session, user=user, modelConfig=model,
                    prompt=f"retained {identity}", status=state, startedAt=started,
                    completedAt=now if state in {"completed", "failed", "cancelled"} else None,
                )
                receipts.create(
                    user=user, workspace=workspace, command="submitMessage", operationId=f"operation-{identity}",
                    requestDigest="a" * 64, sessionId=session.pk, agentRunId=run.pk, turnId=run.turn_id,
                )
            run_facts = list(runs.order_by("id").values())
            receipt_facts = list(receipts.order_by("operationId").values())
            table = Run._meta.db_table
            with database.cursor() as cursor:
                before = database.introspection.get_constraints(cursor, table)
            before_indexes = {index.name for index in Run._meta.indexes}

            MigrationExecutor(database).migrate(self.migrate_to)
            upgraded = MigrationExecutor(database).loader.project_state(self.migrate_to).apps.get_model("app_core", "AgentRun")
            added = [index for index in upgraded._meta.indexes if index.name not in before_indexes]
            self.assertEqual(len(added), 1)
            index = added[0]
            declared = next(item for item in AgentRun._meta.indexes if item.name == index.name)
            self.assertEqual(index.deconstruct(), declared.deconstruct())
            self.assertEqual(declared.fields, ["workspace", "id"])
            with database.cursor() as cursor:
                after = database.introspection.get_constraints(cursor, table)
                self.assertTrue(after[index.name]["index"])
                self.assertEqual(after[index.name]["columns"], ["workspace_id", "id"])
                self.assertTrue(set(before).issubset(after))
                cursor.execute(
                    "SELECT pg_get_expr(i.indpred, i.indrelid) FROM pg_index i "
                    "JOIN pg_class c ON c.oid=i.indexrelid WHERE c.relname=%s AND i.indrelid=%s::regclass",
                    [index.name, table],
                )
                predicate = cursor.fetchone()[0]
                self.assertIsNotNone(predicate)
                # Execute the database's own predicate; SQL spellings are not a contract.
                cursor.execute(f"SELECT id FROM {database.ops.quote_name(table)} WHERE {predicate}")
                indexed_ids = {row[0] for row in cursor.fetchall()}
            expected = {identity for identity, state, _ in cases if state in {"queued", "running"}}
            self.assertEqual(indexed_ids, expected)
            self.assertEqual(set(runs.filter(declared.condition).values_list("id", flat=True)), expected)
            self.assertEqual(list(runs.order_by("id").values()), run_facts)
            self.assertEqual(list(receipts.order_by("operationId").values()), receipt_facts)

            MigrationExecutor(database).migrate(self.migrate_from)
            with database.cursor() as cursor:
                restored = database.introspection.get_constraints(cursor, table)
            self.assertEqual(restored, before)
            self.assertEqual(list(runs.order_by("id").values()), run_facts)
            self.assertEqual(list(receipts.order_by("operationId").values()), receipt_facts)
