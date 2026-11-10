from django.db import migrations, models


def require_no_active_uploads(apps, schema_editor):
    alias = schema_editor.connection.alias
    Lease = apps.get_model("app_core", "UploadLease")
    Capacity = apps.get_model("app_core", "UploadCapacity")
    if (Lease.objects.using(alias).exists()
            or Capacity.objects.using(alias).filter(reservedBytes__gt=0).exists()
            or Capacity.objects.using(alias).filter(activeUploads__gt=0).exists()):
        raise RuntimeError("upload_capacity_rollback_requires_confirmed_cleanup")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0010_active_run_admission")]
    operations = [
        migrations.CreateModel(name="UploadCapacity", fields=[
            ("id", models.PositiveSmallIntegerField(default=1, primary_key=True, serialize=False)),
            ("reservedBytes", models.PositiveBigIntegerField(default=0)),
            ("activeUploads", models.PositiveIntegerField(default=0)),
        ]),
        migrations.CreateModel(name="UploadLease", fields=[
            ("id", models.CharField(max_length=32, primary_key=True, serialize=False)),
            ("poolRef", models.CharField(max_length=32)),
            ("byteHold", models.PositiveBigIntegerField()),
            ("uploadSlot", models.BooleanField()),
            ("state", models.CharField(default="reserved", max_length=16)),
        ]),
        migrations.RunPython(migrations.RunPython.noop, require_no_active_uploads),
    ]
