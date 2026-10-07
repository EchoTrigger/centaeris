from django.db import migrations
from app_core.session_payload import SessionPayloadField


def refuse_lossy_reverse(apps, schema_editor):
    events = apps.get_model("app_core", "SessionEvent").objects.using(schema_editor.connection.alias)
    if events.filter(payload__has_key="__centaerisSessionStorage").exists():
        raise ValueError("session_payload_migration_cannot_discard_canonical_output")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0005_agent_input_attachments")]
    # Existing v1 wire records remain unchanged. The field adds a private,
    # lossless encoding only when PostgreSQL cannot represent a control byte.
    operations = [migrations.AlterField(model_name="sessionevent", name="payload", field=SessionPayloadField()),
                  migrations.RunPython(migrations.RunPython.noop, refuse_lossy_reverse)]
