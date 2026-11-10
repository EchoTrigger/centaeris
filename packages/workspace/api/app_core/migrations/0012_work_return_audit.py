from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0011_upload_capacity")]
    operations = [migrations.CreateModel(name="WorkReturnAuditCursor", fields=[
        ("id", models.CharField(max_length=32, primary_key=True, serialize=False)),
        ("after", models.CharField(max_length=64, null=True)),
        ("through", models.CharField(max_length=64, null=True)),
        ("nextPageAtMs", models.BigIntegerField(default=0)),
        ("leaseOwner", models.CharField(blank=True, default="", max_length=64)),
        ("leaseExpiresAtMs", models.BigIntegerField(default=0)),
    ])]
