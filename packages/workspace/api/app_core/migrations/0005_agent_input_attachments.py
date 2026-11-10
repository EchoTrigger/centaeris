from django.db import migrations, models


def refuse_lossy_reverse(apps, schema_editor):
    if apps.get_model("app_core", "AgentInput").objects.using(schema_editor.connection.alias).exclude(attachments=[]).exists():
        raise ValueError("agent_input_attachment_migration_cannot_discard_captures")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0004_business_agent_branches")]
    operations = [migrations.AddField(model_name="agentinput", name="attachments",
        field=models.JSONField(default=list)),
        migrations.RunPython(migrations.RunPython.noop, refuse_lossy_reverse)]
