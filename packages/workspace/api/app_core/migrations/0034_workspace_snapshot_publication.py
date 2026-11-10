"""Fence exact object keys across streaming, recovery publication and GC."""
from django.db import migrations, models
import django.db.models.deletion
import app_core.models


def guard_reverse(apps,schema_editor):
    alias=schema_editor.connection.alias
    if (apps.get_model("app_core","WorkspaceSnapshotObject").objects.using(alias).exists()
            or apps.get_model("app_core","StorageCleanupRetry").objects.using(alias).exclude(publicationToken="").exists()):
        raise RuntimeError("workspace_snapshot_publication_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0033_session_workspace_authority")]
    operations = [
        migrations.CreateModel(name="WorkspaceSnapshotObject", fields=[
            ("keyDigest", models.CharField(primary_key=True, max_length=64, serialize=False)),
            ("storageKey", models.CharField(max_length=1000, unique=True)),
            ("kind", models.CharField(max_length=16)), ("state", models.CharField(max_length=16, default="live")),
            ("publicationToken", models.CharField(max_length=32, default=app_core.models.new_workspace_publication_token)),
            ("registered", models.BooleanField(default=False)),
            ("collectedAt", models.DateTimeField(null=True, blank=True)),
            ("createdAt", models.DateTimeField(auto_now_add=True)), ("updatedAt", models.DateTimeField(auto_now=True)),
            ("session", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="workspaceSnapshotObjects", to="app_core.session")),
        ]),
        migrations.AddConstraint(model_name="workspacesnapshotobject", constraint=models.CheckConstraint(
            condition=models.Q(kind__in=["snapshot", "temporary"]), name="workspace_object_kind")),
        migrations.AddConstraint(model_name="workspacesnapshotobject", constraint=models.CheckConstraint(
            condition=(models.Q(state="live", collectedAt__isnull=True) | models.Q(state="collected", collectedAt__isnull=False)),
            name="workspace_object_state")),
        migrations.CreateModel(name="WorkspaceSnapshotUpload", fields=[
            ("id", models.CharField(primary_key=True, max_length=32, serialize=False)),
            ("jobId", models.CharField(max_length=256)), ("leaseOwner", models.CharField(max_length=160)),
            ("authorizationDigest", models.CharField(max_length=71)),
            ("finalPublicationToken", models.CharField(max_length=32)),
            ("temporaryPublicationToken", models.CharField(max_length=32)),
            ("state", models.CharField(max_length=16, default="streaming")),
            ("createdAt", models.DateTimeField(auto_now_add=True)), ("updatedAt", models.DateTimeField(auto_now=True)),
            ("finishedAt", models.DateTimeField(null=True, blank=True)),
            ("agent_run", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="workspaceSnapshotUploads", to="app_core.agentrun")),
            ("finalObject", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="finalUploads", to="app_core.workspacesnapshotobject")),
            ("temporaryObject", models.OneToOneField(on_delete=django.db.models.deletion.PROTECT,
                related_name="temporaryUpload", to="app_core.workspacesnapshotobject")),
        ]),
        migrations.AddConstraint(model_name="workspacesnapshotupload", constraint=models.CheckConstraint(
            condition=models.Q(state__in=["streaming", "staged", "committed", "abandoned"]), name="workspace_upload_state")),
        migrations.AddIndex(model_name="workspacesnapshotupload", index=models.Index(
            fields=["jobId", "authorizationDigest"], name="workspace_upload_job")),
        migrations.AddField(model_name="storagecleanupretry", name="publicationToken",
            field=models.CharField(max_length=32, blank=True, default="")),
        migrations.RunPython(migrations.RunPython.noop,guard_reverse),
    ]
