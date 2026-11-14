from django.db import migrations, models


class Migration(migrations.Migration):
    dependencies = [("app_core", "0058_user_workspace_guide_state")]

    operations = [migrations.AddField(
        model_name="userworkspaceguidestate",
        name="manualSteps",
        field=models.JSONField(default=dict),
    )]
