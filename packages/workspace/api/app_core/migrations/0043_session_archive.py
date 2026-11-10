from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0042_personal_library_materials")]

    operations = [
        migrations.AddField(
            model_name="session",
            name="archivedAt",
            field=models.DateTimeField(blank=True, null=True),
        ),
    ]
