"""Remove unrequested Session configuration without changing Run snapshots."""
from django.db import migrations


def replace_session_capture(apps, schema_editor):
    if schema_editor.connection.vendor == "sqlite":
        return  # SQLite checks migration structure; PostgreSQL owns notifications.
    if schema_editor.connection.vendor != "postgresql":
        raise RuntimeError("presentation_changes_require_postgresql")
    # Freeze the post-0038 Session metadata contract without the retired column.
    # PL/pgSQL record references survive DROP COLUMN, so replace before removing it.
    columns = ("id", "workspace_id", "owner_id", "project_id", "current_environment_id",
        "title", "origin", "status", "deletedAt", "purgedAt", "isPinned", "isManuallyUnread",
        "attentionRevision", "latestAttentionSequence", "readThroughSequence", "updatedAt")
    quote = schema_editor.quote_name
    changes = ",".join("CASE WHEN OLD."+quote(c)+" IS DISTINCT FROM NEW."+quote(c)+" THEN '"+c+"' END" for c in columns)
    all_columns = "ARRAY["+",".join("'"+c+"'" for c in columns)+"]::text[]"
    row = "jsonb_build_object('id',R.id,'workspace_id',R.workspace_id,'mutation',TG_OP)"
    schema_editor.execute(f"""CREATE OR REPLACE FUNCTION public.workspace_presentation_capture_session_v1()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE changed text[];
BEGIN
 IF TG_OP='UPDATE' THEN
  changed := array_remove(ARRAY[{changes}],NULL);
  IF cardinality(changed)=0 THEN RETURN NULL; END IF;
 ELSE changed := {all_columns}; END IF;
 IF TG_OP<>'INSERT' THEN PERFORM public.workspace_presentation_apply_v1('session',{row.replace('R.', 'OLD.')},changed); END IF;
 IF TG_OP<>'DELETE' THEN PERFORM public.workspace_presentation_apply_v1('session',{row.replace('R.', 'NEW.')},changed); END IF;
 RETURN NULL;
END; $$;""")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0038_session_ownership")]
    operations = [
        migrations.RunPython(replace_session_capture, migrations.RunPython.noop),
        migrations.RemoveField(model_name="session", name="instructions"),
    ]
