from django.db import migrations, models


def reject_unreconstructable_question_waits(apps, schema_editor):
    if schema_editor.connection.vendor != "postgresql":
        raise RuntimeError("question_waits_require_postgresql")
    with schema_editor.connection.cursor() as cursor:
        # Fresh deployments migrate Django before Runtime creates its schema.
        cursor.execute("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='runtime')")
        if not cursor.fetchone()[0]:
            return
        cursor.execute("SELECT EXISTS(SELECT 1 FROM runtime.checkpoints WHERE kind='wait' "
            "AND done_reason='question' AND payload_json::jsonb->>'schema'='runtime.await_question.v1')")
        if cursor.fetchone()[0]:
            raise RuntimeError("legacy_question_wait_requires_authoritative_reconstruction")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0039_remove_session_instructions")]
    operations = [
        migrations.RunPython(reject_unreconstructable_question_waits, migrations.RunPython.noop),
        migrations.RemoveConstraint(model_name="hostedoperationreceipt", name="hosted_operation_result_valid"),
        migrations.AddConstraint(model_name="hostedoperationreceipt", constraint=models.CheckConstraint(
            condition=(models.Q(command="createSession", agentRunId__isnull=True, turnId__isnull=True)
                | models.Q(command__in=["submitMessage", "consumeWorkReturn", "respondQuestion"], agentRunId__isnull=False, turnId__isnull=False)
                | models.Q(command="consumeWorkReturn", agentRunId__isnull=True, turnId__isnull=True)),
            name="hosted_operation_result_valid")),
    ]
