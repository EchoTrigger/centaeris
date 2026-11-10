"""Reconstruct attention identity while preserving ambiguous legacy owner choices."""
from django.db import migrations, models
from django.db.models import Max, Q


def reconstruct_attention(apps, schema_editor):
    Session = apps.get_model("app_core", "Session")
    Event = apps.get_model("app_core", "SessionEvent")
    alias = schema_editor.connection.alias
    attention = Q(payload__type__in=("agent_run_completed", "agent_run_failed", "agent_run_interrupted")) | Q(
        payload__type="checkpoint_ref", payload__payload__status="paused_question",
    )
    for session in Session.objects.using(alias).order_by("pk").iterator(chunk_size=500):
        history = Event.objects.using(alias).filter(session_id=session.pk)
        watermarks = history.aggregate(high_water=Max("sequence"), attention=Max("sequence", filter=attention))
        # The old boolean did not record whether unread was automatic or manual.
        # Preserve every true value as manual rather than discard an owner action.
        # A previously read Session remains read even when its facts are reprojected.
        Session.objects.using(alias).filter(pk=session.pk).update(
            latestAttentionSequence=watermarks["attention"] or 0,
            readThroughSequence=0 if session.isUnread else (watermarks["high_water"] or 0),
        )


def guard_history_reversal(apps, schema_editor):
    Session = apps.get_model("app_core", "Session")
    if Session.objects.using(schema_editor.connection.alias).filter(
        Q(latestAttentionSequence__gt=0) | Q(readThroughSequence__gt=0) | Q(attentionRevision__gt=0),
    ).exists():
        raise RuntimeError("session_attention_history_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0026_external_identity_login")]
    operations = [
        migrations.AddField(model_name="session", name="attentionRevision",
            field=models.PositiveBigIntegerField(db_default=0, default=0)),
        migrations.AddField(model_name="session", name="latestAttentionSequence",
            field=models.PositiveBigIntegerField(db_default=0, default=0)),
        migrations.AddField(model_name="session", name="readThroughSequence",
            field=models.PositiveBigIntegerField(db_default=0, default=0)),
        migrations.RunPython(reconstruct_attention, migrations.RunPython.noop),
        migrations.RunPython(migrations.RunPython.noop, guard_history_reversal),
    ]
