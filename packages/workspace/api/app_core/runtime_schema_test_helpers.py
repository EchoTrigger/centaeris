"""Exact cleanup of functions owned by native Runtime integration fixtures."""
import uuid

from django.db import connection, transaction


def detach_runtime_session_event_objects(cursor):
    cursor.execute("SELECT pg_get_triggerdef(t.oid),t.tgenabled,n.nspname,p.proname "
                   "FROM pg_trigger t JOIN pg_proc p ON p.oid=t.tgfoid "
                   "JOIN pg_namespace n ON n.oid=p.pronamespace "
                   "WHERE t.tgrelid=to_regclass('public.app_core_sessionevent') "
                   "AND t.tgname='session_event_source_mutation' AND NOT t.tgisinternal")
    trigger = cursor.fetchone()
    if trigger and trigger[2:] != ("runtime", "record_session_source_mutation"):
        raise RuntimeError("unexpected Session source mutation trigger function")
    cursor.execute("SELECT pg_get_indexdef(i.oid),x.indrelid=to_regclass('public.app_core_sessionevent') "
                   "FROM pg_class i JOIN pg_namespace n ON n.oid=i.relnamespace "
                   "JOIN pg_index x ON x.indexrelid=i.oid "
                   "WHERE n.nspname='public' AND i.relname='session_tool_call_origins'")
    index = cursor.fetchone()
    if index and not index[1]:
        raise RuntimeError("unexpected Session tool call origins index table")
    if trigger:
        cursor.execute("DROP TRIGGER session_event_source_mutation ON public.app_core_sessionevent")
    if index:
        cursor.execute("DROP INDEX public.session_tool_call_origins")
    return trigger, index


def isolate_runtime_schema(test):
    """Restore the caller's original namespace after this fixture's cleanup."""
    test.assertTrue(connection.settings_dict["NAME"].startswith("test_"))
    saved_schema = connection.ops.quote_name("native_consume_previous_" + uuid.uuid4().hex)
    with transaction.atomic(), connection.cursor() as cursor:
        cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='runtime')")
        previous_schema_exists = cursor.fetchone()[0]
        previous_trigger, previous_index = detach_runtime_session_event_objects(cursor)
        if previous_schema_exists:
            cursor.execute("ALTER SCHEMA runtime RENAME TO " + saved_schema)

    def restore_runtime_schema():
        with transaction.atomic(), connection.cursor() as cursor:
            detach_runtime_session_event_objects(cursor)
            cursor.execute("SELECT tablename FROM pg_tables WHERE schemaname='runtime'")
            tables = [row[0] for row in cursor.fetchall()]
            if tables:
                cursor.execute("DROP TABLE " + ", ".join(
                    "runtime." + connection.ops.quote_name(table) for table in sorted(tables)))
            remove_runtime_trigger_functions(cursor)
            cursor.execute("DROP SCHEMA IF EXISTS runtime")
            if previous_schema_exists:
                cursor.execute("ALTER SCHEMA " + saved_schema + " RENAME TO runtime")
            if previous_trigger:
                cursor.execute(previous_trigger[0])
                mode = {"O": "ENABLE", "D": "DISABLE", "R": "ENABLE REPLICA", "A": "ENABLE ALWAYS"}
                cursor.execute("ALTER TABLE public.app_core_sessionevent " + mode[previous_trigger[1]] +
                               " TRIGGER session_event_source_mutation")
            if previous_index:
                cursor.execute(previous_index[0])
    test.addCleanup(restore_runtime_schema)


def remove_runtime_trigger_functions(cursor):
    # Tables (and their triggers) must already be removed. Deliberately avoid
    # CASCADE: unexpected dependencies or functions must still block cleanup.
    for name in ("notify_runtime_job_ready_v1", "notify_subagent_presentation_v1", "verify_execution_allocation",
                 "verify_resident_allocation", "protect_environment_retirement",
                 "protect_environment_maintenance_completion",
                 "protect_agent_run_cleanup_receipt",
                 "protect_environment_provision_completion",
                 "protect_environment_creation_attempt", "protect_lifecycle_lease_purpose",
                 "resident_capacity_release", "resident_capacity_job_change", "protect_model_input_image",
                 "protect_session_history_entry", "record_session_source_mutation"):
        cursor.execute("DROP FUNCTION IF EXISTS runtime." + name + "()")
