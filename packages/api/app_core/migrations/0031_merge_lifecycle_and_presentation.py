"""Converge lifecycle responsibility and presentation histories without rewriting either."""
from django.db import migrations


class Migration(migrations.Migration):
    dependencies = [
        ("app_core", "0028_provider_maintenance_target"),
        ("app_core", "0030_session_manual_unread"),
    ]
    operations = []
