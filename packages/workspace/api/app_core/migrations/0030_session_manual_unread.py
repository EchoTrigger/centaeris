"""Name the stored owner choice separately from derived unread presentation."""
from django.db import migrations


def _replace_session_capture(schema_editor, manual_column):
    if schema_editor.connection.vendor == "sqlite":
        return  # Schema-drift checks use SQLite; presentation readers require PG.
    if schema_editor.connection.vendor != "postgresql":
        raise RuntimeError("presentation_changes_require_postgresql")
    # Freeze this migration's physical schema. PL/pgSQL record field references
    # are source text and are not rewritten by ALTER TABLE RENAME COLUMN.
    columns = ("id", "workspace_id", "owner_id", "agent_id", "project_id", "current_environment_id",
        "title", "origin", "status", "deletedAt", "purgedAt", "isPinned", manual_column,
        "attentionRevision", "latestAttentionSequence", "readThroughSequence", "updatedAt")
    quote = schema_editor.quote_name
    changes = ",".join("CASE WHEN OLD." + quote(column) + " IS DISTINCT FROM NEW." + quote(column)
        + " THEN '" + column + "' END" for column in columns)
    all_columns = "ARRAY[" + ",".join("'" + column + "'" for column in columns) + "]::text[]"
    row = "jsonb_build_object('id',R.id,'agent_id',R.agent_id)"
    schema_editor.execute(f"""CREATE OR REPLACE FUNCTION public.workspace_presentation_capture_session_v1()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE changed text[];
BEGIN
    IF TG_OP='UPDATE' THEN
        changed := array_remove(ARRAY[{changes}],NULL);
        IF cardinality(changed)=0 THEN RETURN NEW; END IF;
    ELSE changed := {all_columns}; END IF;
    IF TG_OP<>'INSERT' THEN
        PERFORM public.workspace_presentation_apply_v1('session',{row.replace('R.', 'OLD.')},changed);
    END IF;
    IF TG_OP<>'DELETE' THEN
        PERFORM public.workspace_presentation_apply_v1('session',{row.replace('R.', 'NEW.')},changed);
    END IF;
    RETURN NULL;
END;
$$;""")


def install(apps, schema_editor):
    _replace_session_capture(schema_editor, "isManuallyUnread")


def uninstall(apps, schema_editor):
    # The reverse runs before RenameField's reverse. Function field references
    # resolve when the trigger fires, after this atomic migration has completed.
    # Keep the current apply function from 0029; restore only the column capture.
    _replace_session_capture(schema_editor, "isUnread")


class Migration(migrations.Migration):
    atomic = True
    dependencies = [("app_core", "0029_remove_presentation_authority")]
    operations = [
        migrations.RenameField(model_name="session", old_name="isUnread", new_name="isManuallyUnread"),
        migrations.RunPython(install, uninstall),
    ]
