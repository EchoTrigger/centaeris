from django.db import migrations, models
import django.db.models.deletion


def preserve_existing_titles(apps, schema_editor):
    apps.get_model("app_core", "Session").objects.using(schema_editor.connection.alias).exclude(
        title="New chat").update(titleRevision=1)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0059_workspace_guide_manual_steps")]
    operations = [
        migrations.AddField(model_name="session", name="titleRevision",
            field=models.PositiveBigIntegerField(default=0, db_default=0)),
        migrations.RunPython(preserve_existing_titles, migrations.RunPython.noop),
        migrations.CreateModel(name="WorkspaceModelDefaults", fields=[
            ("id", models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name="ID")),
            ("titleThinkingMode", models.CharField(max_length=64, default="low", blank=True)),
            ("workspace", models.OneToOneField(on_delete=django.db.models.deletion.CASCADE, to="app_core.workspace")),
            ("titleModelConfig", models.ForeignKey(null=True, blank=True, on_delete=django.db.models.deletion.PROTECT, to="app_core.modelconfig")),
        ]),
        migrations.CreateModel(name="SessionTitleJob", fields=[
            ("id", models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name="ID")),
            ("titleRevision", models.PositiveBigIntegerField()),
            ("thinkingMode", models.CharField(max_length=64, blank=True)),
            ("createdAt", models.DateTimeField(auto_now_add=True)),
            ("status", models.CharField(max_length=16, default="pending")),
            ("session", models.OneToOneField(on_delete=django.db.models.deletion.CASCADE, to="app_core.session")),
            ("firstRun", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, to="app_core.agentrun")),
            ("modelConfig", models.ForeignKey(null=True, on_delete=django.db.models.deletion.PROTECT, to="app_core.modelconfig")),
        ]),
    ]
