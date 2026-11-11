from django.contrib.postgres.indexes import GinIndex, OpClass
from django.contrib.postgres.operations import AddIndexConcurrently, CreateExtension
from django.db import migrations, models
from django.db.models.functions import Lower


SEARCH_TEXT_FUNCTION = """
CREATE FUNCTION public.workspace_session_search_text(event jsonb)
RETURNS text LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
  SELECT CASE
    WHEN event->>'type' = 'user_message' THEN event#>>'{payload,text}'
    WHEN event->>'type' = 'assistant_message' THEN event#>>'{payload,modelMarkdown}'
    WHEN event->>'type' = 'phase_event'
      AND event#>>'{payload,stage}' = 'model_process_summary'
      THEN event#>>'{payload,message}'
    ELSE NULL
  END
$$;
"""


class Migration(migrations.Migration):
    atomic = False
    dependencies = [("app_core", "0053_validate_library_note_body_constraint")]
    operations = [
        CreateExtension("pg_bigm"),
        migrations.RunSQL(SEARCH_TEXT_FUNCTION, "DROP FUNCTION public.workspace_session_search_text(jsonb)"),
        AddIndexConcurrently("session", GinIndex(OpClass(Lower("title"), name="gin_bigm_ops"),
            name="session_title_bigm")),
        AddIndexConcurrently("sessionevent", GinIndex(
            OpClass(Lower(models.Func("payload", function="workspace_session_search_text",
                output_field=models.TextField())), name="gin_bigm_ops"), name="session_event_body_bigm",
            condition=models.Q(payload__type__in=["user_message", "phase_event", "assistant_message"]))),
    ]
