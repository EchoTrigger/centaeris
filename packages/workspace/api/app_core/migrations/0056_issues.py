import app_core.models
from django.conf import settings
from django.db import migrations, models
import django.db.models.deletion


class Migration(migrations.Migration):
    dependencies = [
        ("app_core", "0055_session_purge_intent"),
        migrations.swappable_dependency(settings.AUTH_USER_MODEL),
    ]

    operations = [
        migrations.CreateModel(
            name="Issue",
            fields=[
                ("id", models.CharField(default=app_core.models.new_issue_id, max_length=64, primary_key=True, serialize=False)),
                ("title", models.CharField(max_length=240)),
                ("description", models.TextField(blank=True, default="")),
                ("status", models.CharField(default="todo", max_length=16)),
                ("version", models.PositiveIntegerField(default=1)),
                ("archivedAt", models.DateTimeField(blank=True, null=True)),
                ("createdAt", models.DateTimeField(auto_now_add=True)),
                ("updatedAt", models.DateTimeField(auto_now=True)),
                ("workspace", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, related_name="issues", to="app_core.workspace")),
                ("assignee", models.ForeignKey(blank=True, null=True, on_delete=django.db.models.deletion.PROTECT, related_name="assigned_issues", to=settings.AUTH_USER_MODEL)),
                ("creator", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name="created_issues", to=settings.AUTH_USER_MODEL)),
            ],
            options={
                "indexes": [models.Index(fields=["workspace", "-updatedAt", "id"], name="issue_workspace_updated")],
                "constraints": [
                    models.CheckConstraint(condition=models.Q(status__in=["backlog", "todo", "inProgress", "done", "canceled"]), name="issue_status_valid"),
                    models.CheckConstraint(condition=models.Q(version__gte=1), name="issue_version_positive"),
                ],
            },
        ),
        migrations.CreateModel(
            name="IssueEvent",
            fields=[
                ("id", models.CharField(default=app_core.models.new_issue_event_id, max_length=64, primary_key=True, serialize=False)),
                ("type", models.CharField(max_length=16)),
                ("changes", models.JSONField(default=dict)),
                ("createdAt", models.DateTimeField(auto_now_add=True)),
                ("issue", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, related_name="events", to="app_core.issue")),
                ("actor", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name="issue_events", to=settings.AUTH_USER_MODEL)),
            ],
            options={
                "indexes": [models.Index(fields=["issue", "createdAt", "id"], name="issue_event_chronological")],
                "constraints": [models.CheckConstraint(condition=models.Q(type__in=["created", "updated", "archived", "restored"]), name="issue_event_type_valid")],
            },
        ),
    ]
