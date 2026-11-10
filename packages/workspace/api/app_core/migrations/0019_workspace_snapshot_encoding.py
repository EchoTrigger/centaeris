"""Existing immutable objects stay frameV1; never reinterpret their digests."""
from django.db import migrations, models


def guard_reversal(apps, schema_editor):
    sessions = apps.get_model("app_core", "Session").objects.using(schema_editor.connection.alias)
    if sessions.exclude(workspaceSnapshotEncoding="frameV1").exists():
        raise RuntimeError("workspace_snapshot_encoding_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0018_environment_release_receipts")]
    operations = [
        migrations.AddField(
            model_name="session",
            name="workspaceSnapshotEncoding",
            field=models.CharField(default="frameV1", max_length=16),
        ),
        migrations.AddConstraint(
            model_name="session",
            constraint=models.CheckConstraint(
                condition=(
                    models.Q(workspaceSnapshotEncoding__in=["frameV1", "tarZstdV1"])
                    & (models.Q(workspaceSnapshotEncoding="frameV1")
                       | models.Q(workspaceSnapshotSizeBytes__gt=0))
                ),
                name="session_workspace_encoding_valid",
            ),
        ),
        migrations.RunPython(migrations.RunPython.noop, guard_reversal),
    ]
