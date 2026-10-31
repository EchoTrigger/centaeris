"""Index Session-scoped file objects and their current publication owners."""
from django.db import migrations, models
import django.db.models.deletion


class Migration(migrations.Migration):
    dependencies = [("app_core", "0035_workspace_resource_contract")]
    operations = [
        migrations.AddField(model_name="workspacesnapshotobject", name="snapshotEncoding",
            field=models.CharField(max_length=16, default="frameV1")),
        migrations.RemoveConstraint(model_name="session", name="session_workspace_encoding_valid"),
        migrations.AddConstraint(model_name="session", constraint=models.CheckConstraint(
            condition=(models.Q(workspaceSnapshotEncoding__in=["frameV1", "tarZstdV1", "fileManifestV1"])
                & (models.Q(workspaceSnapshotEncoding="frameV1") | models.Q(workspaceSnapshotSizeBytes__gt=0))),
            name="session_workspace_encoding_valid")),
        migrations.RemoveConstraint(model_name="workspacesnapshotobject", name="workspace_object_kind"),
        migrations.AddConstraint(model_name="workspacesnapshotobject", constraint=models.CheckConstraint(
            condition=models.Q(kind__in=["snapshot", "temporary", "file", "filePack"]), name="workspace_object_kind")),
        migrations.CreateModel(name="WorkspaceFilePack", fields=[
            ("object", models.OneToOneField(primary_key=True, serialize=False,
                on_delete=django.db.models.deletion.CASCADE, related_name="filePack", to="app_core.workspacesnapshotobject")),
            ("sizeBytes", models.PositiveBigIntegerField()),
            ("verifiedPublicationToken", models.CharField(max_length=32, blank=True, default="")),
        ]),
        migrations.CreateModel(name="WorkspaceFileObject", fields=[
            ("object", models.OneToOneField(primary_key=True, serialize=False,
                on_delete=django.db.models.deletion.CASCADE, related_name="fileContent", to="app_core.workspacesnapshotobject")),
            ("sha256", models.CharField(max_length=71)),
            ("sizeBytes", models.PositiveBigIntegerField()),
            ("pack", models.ForeignKey(null=True, blank=True, on_delete=django.db.models.deletion.PROTECT,
                related_name="files", to="app_core.workspacefilepack")),
            ("packOffset", models.PositiveBigIntegerField(default=0)),
            ("verifiedPublicationToken", models.CharField(max_length=32, blank=True, default="")),
        ]),
        migrations.CreateModel(name="WorkspaceFileReservation", fields=[
            ("id", models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name="ID")),
            ("jobId", models.CharField(max_length=256)),
            ("leaseOwner", models.CharField(max_length=160)),
            ("authorizationDigest", models.CharField(max_length=71)),
            ("publicationToken", models.CharField(max_length=32)),
            ("agent_run", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="workspaceFileReservations", to="app_core.agentrun")),
            ("file", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE,
                related_name="reservations", to="app_core.workspacefileobject")),
        ]),
        migrations.AddConstraint(model_name="workspacefilereservation", constraint=models.UniqueConstraint(
            fields=("file", "jobId", "leaseOwner"), name="workspace_file_reservation_identity")),
        migrations.AddIndex(model_name="workspacefilereservation", index=models.Index(
            fields=["jobId", "authorizationDigest"], name="workspace_file_reservation_job")),
        migrations.CreateModel(name="WorkspaceSnapshotFileReference", fields=[
            ("id", models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name="ID")),
            ("snapshot", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE,
                related_name="fileReferences", to="app_core.workspacesnapshotobject")),
            ("file", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT,
                related_name="snapshotReferences", to="app_core.workspacefileobject")),
        ]),
        migrations.AddConstraint(model_name="workspacesnapshotfilereference", constraint=models.UniqueConstraint(
            fields=("snapshot", "file"), name="workspace_snapshot_file_identity")),
    ]
