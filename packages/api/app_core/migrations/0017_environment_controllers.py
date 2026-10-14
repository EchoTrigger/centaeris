from django.db import migrations, models
import django.db.models.deletion


def guard_controller_reversal(apps, schema_editor):
    Environment = apps.get_model("app_core", "SessionEnvironment")
    if Environment.objects.using(schema_editor.connection.alias).filter(controller_epoch__gt=0).exists():
        raise RuntimeError("environment_controller_history_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0016_session_environments")]
    operations = [
        migrations.AddField(model_name="sessionenvironment", name="controller_epoch",
            field=models.PositiveBigIntegerField(default=0, editable=False)),
        migrations.AddField(model_name="sessionenvironment", name="controller_run",
            field=models.ForeignKey(blank=True, editable=False, null=True,
                on_delete=django.db.models.deletion.PROTECT, related_name="+", to="app_core.agentrun")),
        migrations.AddField(model_name="sessionenvironment", name="controller_lease_owner",
            field=models.CharField(blank=True, default="", editable=False, max_length=160)),
        migrations.AddField(model_name="sessionenvironment", name="controller_released",
            field=models.BooleanField(default=True, editable=False)),
        migrations.AddField(model_name="sessionenvironment", name="controller_purpose",
            field=models.CharField(choices=[("execute", "Execute"), ("cleanup", "Cleanup")],
                default="", editable=False, max_length=7)),
        migrations.AddConstraint(model_name="sessionenvironment", constraint=models.CheckConstraint(
            condition=(models.Q(controller_run__isnull=True, controller_lease_owner="", controller_released=True, controller_purpose="")
                | (models.Q(controller_run__isnull=False, controller_epoch__gt=0, controller_purpose__in=["execute", "cleanup"])
                   & ~models.Q(controller_lease_owner=""))), name="environment_controller_complete")),
        migrations.RunPython(migrations.RunPython.noop, guard_controller_reversal),
    ]
