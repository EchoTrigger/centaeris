import uuid

from django.conf import settings
from django.db import migrations, models
import django.db.models.deletion


class Migration(migrations.Migration):
    dependencies = [
        ("app_core", "0060_session_titles"),
        migrations.swappable_dependency(settings.AUTH_USER_MODEL),
    ]

    operations = [migrations.CreateModel(
        name="IssueExecution",
        fields=[
            ("id", models.UUIDField(default=uuid.uuid4, editable=False, primary_key=True, serialize=False)),
            ("issueVersion", models.PositiveIntegerField()),
            ("issueTitle", models.CharField(max_length=240)),
            ("issueDescription", models.TextField(blank=True, default="")),
            ("createdAt", models.DateTimeField(auto_now_add=True)),
            ("issue", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, related_name="executions", to="app_core.issue")),
            ("issuer", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name="issued_issue_executions", to=settings.AUTH_USER_MODEL)),
            ("session", models.OneToOneField(on_delete=django.db.models.deletion.CASCADE, related_name="issue_execution", to="app_core.session")),
            ("agent_run", models.OneToOneField(on_delete=django.db.models.deletion.CASCADE, related_name="issue_execution", to="app_core.agentrun")),
        ],
        options={
            "indexes": [models.Index(fields=["issue", "issuer", "-createdAt", "id"], name="issue_execution_owner_created")],
            "constraints": [models.CheckConstraint(condition=models.Q(issueVersion__gte=1), name="issue_execution_version_positive")],
        },
    )]
