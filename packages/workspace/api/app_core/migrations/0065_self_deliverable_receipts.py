from django.db import migrations, models


def backfill_self_deliverable_receipts(apps, schema_editor):
    alias = schema_editor.connection.alias
    deliverables = apps.get_model("app_core", "IssueDeliverable")
    events = apps.get_model("app_core", "IssueEvent")
    notifications = apps.get_model("app_core", "IssueNotification")
    memberships = apps.get_model("app_core", "WorkspaceMembership")
    eligible = memberships.objects.using(alias).filter(
        workspace_id=models.OuterRef("issue__workspace_id"),
        user_id=models.OuterRef("recipient_id"), role__in=["owner", "admin", "member"])
    for item in deliverables.objects.using(alias).filter(
            author_id=models.F("recipient_id"), recipient__is_active=True
        ).filter(models.Exists(eligible)).select_related("issue").iterator():
        event = events.objects.using(alias).get(issue_id=item.issue_id, actor_id=item.author_id,
            type="deliverableSubmitted", changes__deliverableId__after=str(item.pk))
        notice, created = notifications.objects.using(alias).get_or_create(
            event_id=event.pk, recipient_id=item.recipient_id,
            defaults={"issueTitle": item.issue.title, "kind": "deliverable"})
        if created:
            notifications.objects.using(alias).filter(pk=notice.pk).update(createdAt=event.createdAt)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0064_merge_environment_resources")]

    operations = [migrations.RunPython(backfill_self_deliverable_receipts)]
