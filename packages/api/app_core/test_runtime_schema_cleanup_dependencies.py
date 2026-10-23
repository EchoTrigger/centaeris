"""Real PostgreSQL dependencies in Runtime fixture namespace teardown."""
from django.db import DatabaseError, connection
from django.test import TransactionTestCase

from .runtime_schema_test_helpers import isolate_runtime_schema


class RuntimeSchemaDependencyCleanupTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        isolate_runtime_schema(self)

    def cleanup_with_previous_fact(self):
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA runtime")
            cursor.execute("CREATE TABLE runtime.previous_fact(value text)")
            cursor.execute("INSERT INTO runtime.previous_fact VALUES('unchanged')")
        cleanups = []
        class Fixture:
            assertTrue = self.assertTrue
            addCleanup = cleanups.append
        isolate_runtime_schema(Fixture())
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA runtime")
        return cleanups.pop()

    def assert_previous_fact(self):
        with connection.cursor() as cursor:
            cursor.execute("SELECT value FROM runtime.previous_fact")
            self.assertEqual(cursor.fetchall(), [("unchanged",)])

    def test_known_power_admission_view_is_removed_before_its_tables(self):
        cleanup = self.cleanup_with_previous_fact()
        succeeded = False
        with connection.cursor() as cursor:
            cursor.execute("CREATE TABLE runtime.provider_environment_power_operations("
                "environment_id text,operation_sequence bigint,action text,ack_json text)")
            cursor.execute("CREATE VIEW runtime.provider_environment_power_admission AS "
                "SELECT DISTINCT ON(p.environment_id) p.environment_id,(p.action<>'resume' OR p.ack_json IS NULL) AS blocked "
                "FROM runtime.provider_environment_power_operations p ORDER BY p.environment_id,p.operation_sequence DESC")
        try:
            cleanup()
            succeeded = True
            self.assert_previous_fact()
        finally:
            if not succeeded:
                with connection.cursor() as cursor:
                    cursor.execute("DROP VIEW IF EXISTS runtime.provider_environment_power_admission")
                cleanup()

    def test_current_creation_and_provider_trigger_functions_are_removed_without_cascade(self):
        cleanup = self.cleanup_with_previous_fact()
        succeeded = False
        names = ("protect_environment_creation_attempt", "protect_lifecycle_lease_purpose",
            "protect_provider_creation_operation", "protect_provider_run_cleanup_receipt",
            "protect_provider_environment_binding", "protect_retained_provider_run_use",
            "protect_provider_environment_retirement", "protect_provider_environment_power",
            "resident_capacity_release", "resident_capacity_job_change")
        with connection.cursor() as cursor:
            cursor.execute("CREATE TABLE runtime.fixture_owned(value integer)")
            for name in names:
                cursor.execute("CREATE FUNCTION runtime." + name + "() RETURNS trigger LANGUAGE plpgsql "
                               "AS $$ BEGIN RETURN NEW; END $$")
        try:
            cleanup()
            succeeded = True
            self.assert_previous_fact()
        finally:
            if not succeeded:
                with connection.cursor() as cursor:
                    for name in names:
                        cursor.execute("DROP FUNCTION IF EXISTS runtime." + name + "()")
                cleanup()

    def test_unknown_view_dependency_still_blocks_cleanup_and_preserves_all_bytes(self):
        cleanup = self.cleanup_with_previous_fact()
        with connection.cursor() as cursor:
            cursor.execute("CREATE TABLE runtime.fixture_owned(value integer)")
            cursor.execute("INSERT INTO runtime.fixture_owned VALUES(7)")
            cursor.execute("CREATE VIEW runtime.foreign_view AS SELECT value FROM runtime.fixture_owned")
        try:
            with self.assertRaises(DatabaseError):
                cleanup()
            with connection.cursor() as cursor:
                cursor.execute("SELECT value FROM runtime.foreign_view")
                self.assertEqual(cursor.fetchall(), [(7,)])
        finally:
            with connection.cursor() as cursor:
                cursor.execute("DROP VIEW runtime.foreign_view")
            cleanup()
        self.assert_previous_fact()
