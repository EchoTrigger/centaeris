from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0054_session_body_search")]

    operations = [
        migrations.AddField(
            model_name="session",
            name="purgeRequestedAt",
            field=models.DateTimeField(blank=True, editable=False, null=True),
        ),
    ]
