from django.conf import settings
from django.db import migrations, models
import django.db.models.deletion


class Migration(migrations.Migration):
    dependencies = [
        ("app_core", "0057_issue_notifications"),
        migrations.swappable_dependency(settings.AUTH_USER_MODEL),
    ]

    operations = [migrations.CreateModel(
        name="UserWorkspaceGuideState",
        fields=[
            ("id", models.BigAutoField(auto_created=True, primary_key=True, serialize=False, verbose_name="ID")),
            ("dismissedAt", models.DateTimeField(blank=True, null=True)),
            ("user", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, to=settings.AUTH_USER_MODEL)),
            ("workspace", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, to="app_core.workspace")),
        ],
        options={"constraints": [models.UniqueConstraint(
            fields=("user", "workspace"), name="user_workspace_guide_unique",
        )]},
    )]
