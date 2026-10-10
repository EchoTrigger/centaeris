import importlib
import shlex
from pathlib import Path
from types import SimpleNamespace
from unittest import TestCase

from django.apps import apps
from django.db import connection
from django.db.migrations.autodetector import MigrationAutodetector
from django.db.migrations.loader import MigrationLoader
from django.db.migrations.state import ProjectState
from django.core.management import call_command
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase
from .migration_testing import isolated_migration_database


migration = importlib.import_module("app_core.migrations.0001_initial")
REPOSITORY_ROOT = Path(__file__).resolve().parents[3]


class RecordingSchemaEditor:
    def __init__(self, vendor):
        self.connection = SimpleNamespace(vendor=vendor)
        self.statements = []

    def execute(self, statement):
        self.statements.append(statement)


class InitialSchemaTests(TestCase):
    def test_postgresql_creates_and_drops_the_tool_result_expression_index(self):
        editor = RecordingSchemaEditor("postgresql")
        migration.create_tool_result_lookup_index(None, editor)
        migration.drop_tool_result_lookup_index(None, editor)
        self.assertEqual(len(editor.statements), 2)
        self.assertIn("CREATE INDEX session_event_tool_result_lookup", editor.statements[0])
        self.assertIn("payload #>> '{payload,callId}'", editor.statements[0])
        self.assertIn("WHERE payload ->> 'type' = 'tool_result'", editor.statements[0])
        self.assertEqual(editor.statements[1], "DROP INDEX session_event_tool_result_lookup")

    def test_private_sqlite_schema_gate_omits_postgresql_expression_sql(self):
        editor = RecordingSchemaEditor("sqlite")
        migration.create_tool_result_lookup_index(None, editor)
        migration.drop_tool_result_lookup_index(None, editor)
        self.assertEqual(editor.statements, [])

    def test_unknown_database_rejects_without_writes(self):
        editor = RecordingSchemaEditor("unknown")
        for operation in (migration.create_tool_result_lookup_index, migration.drop_tool_result_lookup_index):
            with self.assertRaisesRegex(RuntimeError, "requires PostgreSQL"):
                operation(None, editor)
        self.assertEqual(editor.statements, [])

    def test_current_migration_chain_matches_all_current_models(self):
        loader = MigrationLoader(None)
        leaves = loader.graph.leaf_nodes("app_core")
        self.assertEqual(len(leaves), 1)
        self.assertEqual(
            MigrationAutodetector(loader.project_state(), ProjectState.from_apps(apps)).changes(
                graph=loader.graph, trim_to_apps={"app_core"}),
            {},
        )
        self.assertTrue(migration.Migration.initial)
        numbered = {path.stem for path in Path(__file__).with_name("migrations").glob("[0-9]*.py")}
        reachable = {name for app, name in loader.graph.forwards_plan(leaves[0]) if app == "app_core"}
        self.assertEqual(numbered, reachable)


def release_migration_command():
    commands = []
    script = (REPOSITORY_ROOT / "scripts/workspace/docker-release-gate.sh").read_text(encoding="utf-8")
    for line in script.splitlines():
        if "manage.py" not in line:
            continue
        tokens = shlex.split(line)
        position = tokens.index("manage.py")
        commands.append(tokens[position + 1:])
    if len(commands) != 1:
        raise AssertionError("release gate must select one database migration check")
    return commands[0]


class InitialDatabaseSchemaTests(TransactionTestCase):
    serialized_rollback = True

    def test_release_migration_check_rejects_pending_without_applying_and_accepts_current(self):
        command, *arguments = release_migration_command()
        with isolated_migration_database([("app_core", "0007_business_definition_instances")]) as database:
            before = set(MigrationLoader(database).applied_migrations)
            with self.assertRaises(SystemExit) as rejected:
                call_command(command, *arguments, database=database.alias, verbosity=0)
            self.assertEqual(rejected.exception.code, 1)
            self.assertEqual(set(MigrationLoader(database).applied_migrations), before)
        call_command(command, *arguments, verbosity=0)

    def test_fresh_database_retains_all_declared_constraints_indexes_and_foreign_keys(self):
        with connection.cursor() as cursor:
            for model in apps.get_app_config("app_core").get_models():
                with self.subTest(model=model.__name__):
                    constraints = connection.introspection.get_constraints(cursor, model._meta.db_table)
                    for item in [*model._meta.constraints, *model._meta.indexes]:
                        self.assertIn(item.name, constraints)
                    foreign_keys = {item["foreign_key"] for item in constraints.values() if item["foreign_key"]}
                    for field in model._meta.local_fields:
                        if field.is_relation and field.db_constraint:
                            self.assertIn((field.related_model._meta.db_table, field.target_field.column), foreign_keys)

    def test_postgresql_tool_result_index_is_installed_with_exact_predicate(self):
        self.assertEqual(connection.vendor, "postgresql")
        with connection.cursor() as cursor:
            cursor.execute("SELECT indexdef FROM pg_indexes WHERE schemaname='public' AND indexname='session_event_tool_result_lookup'")
            row = cursor.fetchone()
        self.assertIsNotNone(row)
        self.assertIn("session_id", row[0])
        self.assertIn("'{payload,callId}'", row[0])
        self.assertIn("'tool_result'", row[0])
