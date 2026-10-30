"""An absent legacy snapshot is unknown, rather than evidence of an empty root."""
from django.db import migrations, models
import django.db.models.deletion


def reconstruct_workspace_authority(apps, schema_editor):
    sessions = apps.get_model("app_core", "Session").objects.using(schema_editor.connection.alias)
    # Generation advances only with an authoritative Session workspace commit.
    # This also preserves explicitly committed empty workspaces. Run counts,
    # missing residents and present-day defaults cannot prove an empty root.
    sessions.filter(workspaceGeneration__gt=0, workspaceLastAdvancedAgentRun__isnull=False).update(
        workspaceBaselineKnown=True)


def reject_reverse(apps, schema_editor):
    raise RuntimeError("session_workspace_authority_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0032_agent_avatar_image")]
    operations = [
        migrations.AddField(model_name="session", name="workspaceBaselineKnown",
            field=models.BooleanField(default=False, db_default=False, editable=False)),
        migrations.RunPython(reconstruct_workspace_authority, reject_reverse),
        migrations.AlterField(model_name="session", name="workspaceBaselineKnown",
            field=models.BooleanField(default=True, db_default=True, editable=False)),
        migrations.AddConstraint(model_name="session", constraint=models.CheckConstraint(
            condition=(models.Q(workspaceBaselineKnown=True) | models.Q(workspaceGeneration=0)),
            name="session_workspace_authority_consistent")),
        migrations.CreateModel(name="SessionWorkspacePreservation", fields=[
            ("id", models.CharField(primary_key=True, max_length=71, serialize=False)),
            ("authorization_digest", models.CharField(max_length=71)),
            ("origin_authorization_digest", models.CharField(max_length=71)),
            ("allocation_execution_id", models.CharField(max_length=160)),
            ("instance_id", models.CharField(max_length=80)),
            ("container_id", models.CharField(max_length=64)),
            ("profile_digest", models.CharField(max_length=71)),
            ("baseline", models.JSONField()), ("workspace", models.JSONField()),
            ("snapshot_encoding", models.CharField(max_length=16, default="frameV1")),
            ("storage_key", models.CharField(max_length=1000, blank=True)),
            ("creator_lease_digest", models.CharField(max_length=71)),
            ("creator_controller_epoch", models.PositiveBigIntegerField()),
            ("created_at", models.DateTimeField(auto_now_add=True)),
            ("session", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="workspacePreservations", to="app_core.session")),
            ("agent_run", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="workspacePreservations", to="app_core.agentrun")),
            ("origin_agent_run", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="originWorkspacePreservations", to="app_core.agentrun")),
            ("environment", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="workspacePreservations", to="app_core.sessionenvironment")),
        ]),
    ]
