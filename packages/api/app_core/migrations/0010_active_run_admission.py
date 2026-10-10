from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0009_storage_gc_backoff")]

    operations = [
        migrations.AddIndex(
            model_name="agentrun",
            index=models.Index(
                fields=["workspace", "id"],
                condition=models.Q(status__in=["queued", "running"]),
                name="agent_run_active_admission",
            ),
        ),
    ]
