"""Exact cleanup of functions owned by native Runtime integration fixtures."""


def remove_runtime_trigger_functions(cursor):
    # Tables (and their triggers) must already be removed. Deliberately avoid
    # CASCADE: unexpected dependencies or functions must still block cleanup.
    for name in ("notify_runtime_job_ready_v1", "verify_execution_allocation",
                 "verify_resident_allocation", "protect_environment_retirement"):
        cursor.execute("DROP FUNCTION IF EXISTS runtime." + name + "()")
