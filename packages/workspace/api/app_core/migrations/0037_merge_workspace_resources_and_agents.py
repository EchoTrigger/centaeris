"""Join the released workspace resource and shared Agent migration branches."""
from django.db import migrations


class Migration(migrations.Migration):
    dependencies = [
        ("app_core", "0036_workspace_file_objects"),
        ("app_core", "0036_workspace_navigation_changes"),
    ]
    operations = []
