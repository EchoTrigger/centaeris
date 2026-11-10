from datetime import timedelta

from django.db import migrations, models
from django.utils import timezone


def preserve_failed_attempts(apps, schema_editor):
    Resource = apps.get_model("app_core", "DerivedResource")
    resources = Resource.objects.using(schema_editor.connection.alias)
    now = timezone.now()
    # Retain legacy attempts without imposing a historical retry ceiling.
    # The next claim applies the deployment's current policy before deletion.
    resources.filter(state="failed").update(nextCleanupAt=now + timedelta(days=1))


class Migration(migrations.Migration):
    dependencies = [("app_core", "0007_business_definition_instances")]

    operations = [
        migrations.AddField(
            model_name="derivedresource", name="nextCleanupAt",
            field=models.DateTimeField(blank=True, null=True),
        ),
        migrations.AddField(
            model_name="derivedresource", name="quarantinedAt",
            field=models.DateTimeField(blank=True, null=True),
        ),
        migrations.CreateModel(
            name="StorageCleanupRetry",
            fields=[
                ("keyDigest", models.CharField(max_length=64, primary_key=True, serialize=False)),
                ("storageKey", models.CharField(max_length=1000)),
                ("collector", models.CharField(max_length=32)),
                ("state", models.CharField(default="pending", max_length=32)),
                ("cleanupAttempts", models.PositiveIntegerField(default=0)),
                ("nextCleanupAt", models.DateTimeField(blank=True, null=True)),
                ("quarantinedAt", models.DateTimeField(blank=True, null=True)),
                ("leaseOwner", models.CharField(blank=True, default="", max_length=96)),
                ("leaseExpiresAt", models.DateTimeField(blank=True, null=True)),
                ("lastFailure", models.TextField(blank=True, default="")),
                ("cleanedAt", models.DateTimeField(blank=True, null=True)),
                ("createdAt", models.DateTimeField(auto_now_add=True)),
                ("updatedAt", models.DateTimeField(auto_now=True)),
            ],
        ),
        # Downgrading would discard the durable retry ceiling and is unsafe.
        migrations.RunPython(preserve_failed_attempts),
    ]
