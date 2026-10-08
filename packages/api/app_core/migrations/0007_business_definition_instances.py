from django.db import migrations, models


def mark_existing_branches(apps, schema_editor):
    Agent = apps.get_model("app_core", "Agent")
    Branch = apps.get_model("app_core", "BusinessAgentBranch")
    Agent.objects.filter(pk__in=Branch.objects.values("agent_id")).update(is_business_instance=True)


def refuse_managed_conversation_loss(apps, schema_editor):
    Agent = apps.get_model("app_core", "Agent")
    if Agent.objects.filter(is_business_instance=True, definition__isnull=False).exists():
        raise RuntimeError("Cannot downgrade persistent published conversations")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0006_session_event_payload_storage")]
    operations = [
        migrations.AddField(model_name="agent", name="is_business_instance", field=models.BooleanField(default=False)),
        migrations.RemoveConstraint(model_name="agent", name="agent_definition_owner_unique"),
        migrations.RunPython(mark_existing_branches, refuse_managed_conversation_loss),
        migrations.AddConstraint(model_name="agent", constraint=models.UniqueConstraint(
            fields=("workspace", "owner", "definition"), condition=models.Q(is_business_instance=False),
            name="agent_definition_owner_unique")),
    ]
