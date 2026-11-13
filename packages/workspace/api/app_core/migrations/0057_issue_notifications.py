import app_core.models
from django.conf import settings
from django.db import migrations, models
import django.db.models.deletion


class Migration(migrations.Migration):
    dependencies = [
        ("app_core", "0056_issues"),
        migrations.swappable_dependency(settings.AUTH_USER_MODEL),
    ]

    operations = [migrations.CreateModel(
        name="IssueNotification",
        fields=[
            ("id", models.CharField(default=app_core.models.new_notification_id, max_length=64, primary_key=True, serialize=False)),
            ("issueTitle", models.CharField(max_length=240)),
            ("kind", models.CharField(max_length=16)),
            ("readAt", models.DateTimeField(blank=True, null=True)),
            ("archivedAt", models.DateTimeField(blank=True, null=True)),
            ("createdAt", models.DateTimeField(auto_now_add=True)),
            ("event", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, related_name="notifications", to="app_core.issueevent")),
            ("recipient", models.ForeignKey(on_delete=django.db.models.deletion.CASCADE, related_name="issue_notifications", to=settings.AUTH_USER_MODEL)),
        ],
        options={
            "indexes": [models.Index(fields=["recipient", "-createdAt", "-id"], name="notification_recipient_created")],
            "constraints": [
                models.UniqueConstraint(fields=["event", "recipient"], name="notification_event_recipient_unique"),
                models.CheckConstraint(condition=models.Q(kind__in=["assigned", "statusChanged"]), name="notification_kind_valid"),
            ],
        },
    )]
