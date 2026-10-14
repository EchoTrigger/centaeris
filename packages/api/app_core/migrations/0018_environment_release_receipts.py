"""Reconstruct only the last provably released controller, never overwritten Runs."""
import hashlib
import json

import django.db.models.deletion
from django.db import migrations, models
import app_core.models


def reconstruct_releases(apps, schema_editor):
    alias = schema_editor.connection.alias
    Environment = apps.get_model("app_core", "SessionEnvironment")
    Run = apps.get_model("app_core", "AgentRun")
    Grant = apps.get_model("app_core", "AgentRunAuthorization")
    Session = apps.get_model("app_core", "Session")
    Receipt = apps.get_model("app_core", "AgentRunEnvironmentRelease")
    for environment in Environment.objects.using(alias).filter(controller_released=True,
            controller_run__isnull=False).iterator():
        run = Run.objects.using(alias).filter(pk=environment.controller_run_id).first()
        grant = Grant.objects.using(alias).filter(agent_run_id=environment.controller_run_id).first()
        session = Session.objects.using(alias).filter(pk=environment.session_id).first()
        if (run is None or grant is None or session is None or not isinstance(grant.payload, dict)
                or environment.controller_purpose != "cleanup"
                or not 1 <= environment.controller_epoch <= 2**63 - 1
                or not environment.controller_lease_owner or run.environment_id != environment.pk
                or run.session_id != environment.session_id or run.status not in {"completed", "failed", "cancelled"}
                or grant.payload.get("schema") != "workspace.agent_run_authorization.v1"
                or grant.payload.get("id") != grant.pk or grant.payload.get("agentId") != session.agent_id
                or grant.payload.get("modelConfigRef") != run.modelConfig_id
                or grant.payload.get("thinkingMode") != (run.thinkingMode or None)
                or grant.payload.get("agentRunId") != run.pk or grant.payload.get("sessionId") != run.session_id
                or grant.payload.get("workspaceId") != run.workspace_id
                or grant.payload.get("userId") != str(run.user_id)
                or "sha256:" + hashlib.sha256(json.dumps(grant.payload, ensure_ascii=False, sort_keys=True,
                    separators=(",", ":")).encode("utf-8")).hexdigest() != grant.digest):
            raise RuntimeError("environment_release_reconstruction_binding_invalid")
        Receipt.objects.using(alias).create(agent_run_id=run.pk, environment_id=environment.pk,
            authorization_digest=grant.digest, controller_epoch=environment.controller_epoch,
            controller_lease_digest="sha256:" + hashlib.sha256(b"workspace:environment-controller-lease:v1\0"
                + environment.controller_lease_owner.encode("utf-8")).hexdigest(), reconstructed=True)


IMMUTABLE = """
CREATE FUNCTION enforce_environment_release_immutable() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'environment_release_immutable' USING ERRCODE='23514';
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER environment_release_immutable BEFORE UPDATE OR DELETE ON app_core_agentrunenvironmentrelease
FOR EACH ROW EXECUTE FUNCTION enforce_environment_release_immutable();
"""


def guard_reversal(apps, schema_editor):
    if apps.get_model("app_core", "AgentRunEnvironmentRelease").objects.using(schema_editor.connection.alias).exists():
        raise RuntimeError("environment_release_history_requires_forward_migration")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0017_environment_controllers")]
    operations = [
        migrations.CreateModel(name="AgentRunEnvironmentRelease", fields=[
            ("id", models.CharField(default=app_core.models.new_environment_release_id, max_length=64, primary_key=True, serialize=False)),
            ("authorization_digest", models.CharField(max_length=71)),
            ("controller_epoch", models.PositiveBigIntegerField()),
            ("controller_lease_digest", models.CharField(max_length=71)),
            ("recorded_at", models.DateTimeField(auto_now_add=True)),
            ("reconstructed", models.BooleanField(default=False)),
            ("agent_run", models.OneToOneField(on_delete=django.db.models.deletion.PROTECT, related_name="environment_release", to="app_core.agentrun")),
            ("environment", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name="releases", to="app_core.sessionenvironment")),
        ], options={"constraints": [models.CheckConstraint(condition=models.Q(controller_epoch__gt=0), name="environment_release_epoch_positive")]}),
        migrations.RunPython(reconstruct_releases, migrations.RunPython.noop),
        migrations.RunSQL(IMMUTABLE, "DROP TRIGGER environment_release_immutable ON app_core_agentrunenvironmentrelease; DROP FUNCTION enforce_environment_release_immutable();"),
        migrations.RunPython(migrations.RunPython.noop, guard_reversal),
    ]
