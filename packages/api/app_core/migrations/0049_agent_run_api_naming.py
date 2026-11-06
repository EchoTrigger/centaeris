from importlib import import_module

import app_core.models
from django.db import migrations, models


def require_no_legacy_api_sessions(apps, schema_editor):
    session = apps.get_model("app_core", "Session")
    if session.objects.using(schema_editor.connection.alias).filter(purpose="oneShot").exists():
        raise RuntimeError("Finish and clean up existing API runs before upgrading to Agent Run API.")


def rename_postgresql_objects(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return

    quote = schema_editor.quote_name
    with schema_editor.connection.cursor() as cursor:
        for table in ("app_core_agentrunapicredential", "app_core_agentrunapioperation"):
            # PostgreSQL keeps generated constraint and index names when a table is renamed.
            cursor.execute(
                "SELECT conname FROM pg_constraint WHERE conrelid = %s::regclass AND conname LIKE %s",
                [table, "app_core_oneshot%"],
            )
            for (name,) in cursor.fetchall():
                new_name = name.replace("oneshot", "agentrunapi", 1)
                cursor.execute(f"ALTER TABLE {quote(table)} RENAME CONSTRAINT {quote(name)} TO {quote(new_name)}")
            cursor.execute(
                """
                SELECT c.relname FROM pg_class c JOIN pg_index i ON c.oid = i.indexrelid
                WHERE i.indrelid = %s::regclass AND c.relname LIKE %s
                """,
                [table, "app_core_oneshot%"],
            )
            for (name,) in cursor.fetchall():
                new_name = name.replace("oneshot", "agentrunapi", 1)
                cursor.execute(f"ALTER INDEX {quote(name)} RENAME TO {quote(new_name)}")
        cursor.execute("ALTER SEQUENCE app_core_oneshotoperation_id_seq RENAME TO app_core_agentrunapioperation_id_seq")


def install_cleanup_guards(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        return

    legacy = import_module("app_core.migrations.0045_one_shot_cleanup")
    reclaimable = (
        legacy.RECLAIMABLE
        .replace("one_shot_session_cleanup_allowed", "agent_run_api_session_cleanup_allowed")
        .replace("app_core_oneshotoperation", "app_core_agentrunapioperation")
        .replace("'oneShot'", "'agentRunApi'")
    )
    profile = import_module("app_core.migrations.0023_unified_environment_profile").GUARDS
    profile = profile.split("CREATE TRIGGER", 1)[0].replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION", 1)
    profile = profile.replace(
        "    IF TG_OP='DELETE' THEN",
        "    IF TG_OP='DELETE' THEN\n"
        "        IF agent_run_api_session_cleanup_allowed(OLD.session_id) THEN RETURN OLD; END IF;",
        1,
    )
    release = """
CREATE OR REPLACE FUNCTION enforce_environment_release_immutable() RETURNS trigger AS $$
BEGIN
 IF TG_OP='DELETE' AND EXISTS(SELECT 1 FROM app_core_agentrun r
     WHERE r.id=OLD.agent_run_id AND agent_run_api_session_cleanup_allowed(r.session_id)) THEN
  RETURN OLD;
 END IF;
 RAISE EXCEPTION 'environment_release_immutable' USING ERRCODE='23514';
END;
$$ LANGUAGE plpgsql;
"""
    schema_editor.execute(
        reclaimable + profile + release + "DROP FUNCTION one_shot_session_cleanup_allowed(text);",
        params=None,
    )


class Migration(migrations.Migration):
    dependencies = [("app_core", "0048_one_shot_credential_digest_index")]

    operations = [
        migrations.RunPython(require_no_legacy_api_sessions, migrations.RunPython.noop),
        migrations.RenameModel("OneShotCredential", "AgentRunApiCredential"),
        migrations.RenameModel("OneShotOperation", "AgentRunApiOperation"),
        migrations.AlterField(
            model_name="agentrunapicredential",
            name="id",
            field=models.CharField(
                default=app_core.models.new_agent_run_api_credential_id,
                max_length=64,
                primary_key=True,
                serialize=False,
            ),
        ),
        migrations.RemoveConstraint("session", "session_purpose_valid"),
        migrations.AddConstraint(
            "session",
            models.CheckConstraint(condition=models.Q(purpose__in=("interactive", "agentRunApi")), name="session_purpose_valid"),
        ),
        migrations.RenameIndex("agentrunapioperation", "agent_run_api_cleanup_due", old_name="one_shot_cleanup_due"),
        migrations.RemoveConstraint("agentrunapioperation", "one_shot_operation_unique"),
        migrations.RemoveConstraint("agentrunapioperation", "one_shot_run_status_valid"),
        migrations.RemoveConstraint("agentrunapioperation", "one_shot_cleanup_status_valid"),
        migrations.AddConstraint(
            "agentrunapioperation",
            models.UniqueConstraint(fields=("credential", "operationId"), name="agent_run_api_operation_unique"),
        ),
        migrations.AddConstraint(
            "agentrunapioperation",
            models.CheckConstraint(
                condition=models.Q(runStatus__in=("queued", "running", "completed", "failed", "cancelled")),
                name="agent_run_api_run_status_valid",
            ),
        ),
        migrations.AddConstraint(
            "agentrunapioperation",
            models.CheckConstraint(
                condition=models.Q(cleanupStatus__in=("pending", "cleaning", "completed", "failed")),
                name="agent_run_api_cleanup_status_valid",
            ),
        ),
        migrations.RunPython(rename_postgresql_objects),
        migrations.RunPython(install_cleanup_guards),
    ]
