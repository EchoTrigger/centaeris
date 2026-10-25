"""Exact cleanup of functions owned by native Runtime integration fixtures."""
import uuid

from django.db import connection


def isolate_runtime_schema(test):
    """Restore the caller's original namespace after this fixture's cleanup."""
    test.assertTrue(connection.settings_dict["NAME"].startswith("test_"))
    saved_schema = connection.ops.quote_name("native_consume_previous_" + uuid.uuid4().hex)
    with connection.cursor() as cursor:
        cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='runtime')")
        previous_schema_exists = cursor.fetchone()[0]
        if previous_schema_exists:
            cursor.execute("ALTER SCHEMA runtime RENAME TO " + saved_schema)

    def restore_runtime_schema():
        with connection.cursor() as cursor:
            # This fixture owns the current Runtime admission projection. Drop
            # it before its source table; unrelated views still block cleanup.
            cursor.execute("DROP VIEW IF EXISTS runtime.provider_environment_power_admission")
            cursor.execute("SELECT tablename FROM pg_tables WHERE schemaname='runtime'")
            tables = [row[0] for row in cursor.fetchall()]
            if tables:
                cursor.execute("DROP TABLE " + ", ".join(
                    "runtime." + connection.ops.quote_name(table) for table in sorted(tables)))
            remove_runtime_trigger_functions(cursor)
            cursor.execute("DROP SCHEMA IF EXISTS runtime")
            if previous_schema_exists:
                cursor.execute("ALTER SCHEMA " + saved_schema + " RENAME TO runtime")
    test.addCleanup(restore_runtime_schema)


def remove_runtime_trigger_functions(cursor):
    # Tables (and their triggers) must already be removed. Deliberately avoid
    # CASCADE: unexpected dependencies or functions must still block cleanup.
    for name in ("notify_runtime_job_ready_v1", "verify_execution_allocation",
                 "verify_resident_allocation", "protect_environment_retirement",
                 "protect_environment_maintenance_completion",
                 "protect_agent_run_cleanup_receipt",
                 "protect_environment_provision_completion",
                 "protect_environment_creation_attempt",
                 "protect_lifecycle_lease_purpose",
                 "protect_provider_creation_operation",
                 "protect_provider_run_cleanup_receipt",
                 "protect_provider_environment_binding",
                 "protect_provider_environment_provision_completion",
                 "protect_provider_environment_maintenance_completion",
                 "protect_retained_provider_run_use",
                 "protect_provider_environment_retirement",
                 "protect_provider_environment_power",
                 "resident_capacity_release",
                 "resident_capacity_job_change"):
        cursor.execute("DROP FUNCTION IF EXISTS runtime." + name + "()")
