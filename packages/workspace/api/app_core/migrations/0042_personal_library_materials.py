from django.db import migrations, models
import django.db.models.deletion


def preserve_existing_run_scope(apps, schema_editor):
    runs = apps.get_model("app_core", "AgentRun").objects.using(schema_editor.connection.alias)
    scope = apps.get_model("app_core", "RunMaterialScope").objects.using(schema_editor.connection.alias)
    batch = []
    for run_id in runs.values_list("id", flat=True).iterator(chunk_size=1000):
        batch.append(scope.model(agent_run_id=run_id, personal_library=False))
        if len(batch) == 1000:
            scope.bulk_create(batch)
            batch.clear()
    if batch:
        scope.bulk_create(batch)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0041_workspace_avatar")]
    operations = [
        migrations.CreateModel(name="RunMaterialScope", fields=[
            ("agent_run", models.OneToOneField(primary_key=True, serialize=False, on_delete=django.db.models.deletion.CASCADE, to="app_core.agentrun")),
            ("personal_library", models.BooleanField(default=False)),
        ]),
        migrations.CreateModel(name="SessionMaterialInput", fields=[
            ("id", models.CharField(primary_key=True, serialize=False, max_length=96)),
            ("identity_key", models.CharField(max_length=64)),
            ("input_ref", models.CharField(max_length=1024)),
            ("resolved", models.JSONField()), ("confirmed", models.BooleanField(default=False)),
            ("session", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, related_name="materialInputs", to="app_core.session")),
        ], options={"constraints": [
            models.UniqueConstraint(fields=("session", "identity_key"), name="session_material_identity_unique"),
            models.UniqueConstraint(fields=("session", "input_ref"), name="session_material_ref_unique"),
        ]}),
        migrations.CreateModel(name="RunMaterialAdmission", fields=[
            ("id", models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name="ID")),
            ("confirmed", models.BooleanField(default=False)),
            ("settled_absent", models.BooleanField(default=False)),
            ("agent_run", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, to="app_core.agentrun")),
            ("material_input", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, related_name="admissions", to="app_core.sessionmaterialinput")),
            ("origin_call", models.ForeignKey(null=True, on_delete=django.db.models.deletion.SET_NULL, to="app_core.sessionevent")),
        ], options={"constraints": [models.UniqueConstraint(fields=("agent_run", "material_input", "origin_call"), name="run_material_admission_unique")]}),
        migrations.RunPython(preserve_existing_run_scope, migrations.RunPython.noop),
    ]
