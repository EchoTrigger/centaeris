"""Runtime integration fixtures restore their isolated PostgreSQL namespace."""
from django.db import connection, DatabaseError
from django.contrib.contenttypes.models import ContentType
from django.test import TransactionTestCase

from .runtime_schema_test_helpers import isolate_runtime_schema


class RuntimeSchemaCleanupTests(TransactionTestCase):
    serialized_rollback = True

    def test_cleanup_allows_the_next_serialized_initial_data_restore(self):
        initial_types = list(ContentType.objects.order_by("pk").values_list("pk", "app_label", "model"))
        self.assertTrue(initial_types)
        initial_data = connection.creation.serialize_db_to_string()
        self._fixture_teardown()
        connection.creation.deserialize_db_from_string(initial_data)
        self.assertEqual(list(ContentType.objects.order_by("pk").values_list("pk", "app_label", "model")),
                         initial_types)

    def setUp(self):
        self.assertEqual(connection.vendor, "postgresql")
        isolate_runtime_schema(self)

    def create_runtime_schema(self):
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA runtime")
            cursor.execute("CREATE TABLE runtime.schema_migrations(version integer PRIMARY KEY)")
            for name in ("notify_runtime_job_ready_v1", "verify_execution_allocation",
                         "verify_resident_allocation", "protect_environment_retirement",
                         "protect_environment_maintenance_completion",
                         "protect_agent_run_cleanup_receipt",
                         "protect_environment_provision_completion"):
                cursor.execute("CREATE FUNCTION runtime." + name + "() RETURNS trigger "
                               "LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$")

    def isolated_cleanup(self):
        cleanups = []
        class Fixture:
            assertTrue = self.assertTrue
            addCleanup = cleanups.append
        isolate_runtime_schema(Fixture())
        return cleanups.pop()

    def test_complete_runtime_functions_are_removed_and_previous_schema_is_restored(self):
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA runtime")
            cursor.execute("CREATE TABLE runtime.previous_fact(value text)")
            cursor.execute("INSERT INTO runtime.previous_fact VALUES ('unchanged')")
        cleanup = self.isolated_cleanup()
        self.create_runtime_schema()
        cleanup()
        with connection.cursor() as cursor:
            cursor.execute("SELECT value FROM runtime.previous_fact")
            self.assertEqual(cursor.fetchall(), [("unchanged",)])
            cursor.execute("SELECT count(*) FROM pg_proc p JOIN pg_namespace n "
                           "ON n.oid=p.pronamespace WHERE n.nspname='runtime'")
            self.assertEqual(cursor.fetchone()[0], 0)

    def test_unrecognized_function_blocks_cleanup_without_cascading_into_other_objects(self):
        cleanup = self.isolated_cleanup()
        self.create_runtime_schema()
        with connection.cursor() as cursor:
            cursor.execute("CREATE FUNCTION runtime.unrecognized() RETURNS integer "
                           "LANGUAGE sql AS 'SELECT 7'")
        try:
            with self.assertRaises(DatabaseError):
                cleanup()
            with connection.cursor() as cursor:
                cursor.execute("SELECT runtime.unrecognized()")
                self.assertEqual(cursor.fetchone()[0], 7)
        finally:
            with connection.cursor() as cursor:
                cursor.execute("DROP FUNCTION runtime.unrecognized()")
            cleanup()
