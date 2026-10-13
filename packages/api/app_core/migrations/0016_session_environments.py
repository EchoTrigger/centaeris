import uuid

import django.db.models.deletion
from django.db import NotSupportedError, migrations, models

import app_core.models


FORWARD = """
CREATE FUNCTION enforce_session_environment_binding() RETURNS trigger AS $$
DECLARE selected_environment varchar(64);
BEGIN
    SELECT current_environment_id INTO selected_environment FROM app_core_session WHERE id=NEW.id;
    IF NOT FOUND THEN RETURN NULL; END IF;
    IF selected_environment IS NULL OR NOT EXISTS (
        SELECT 1 FROM app_core_sessionenvironment WHERE id=selected_environment AND session_id=NEW.id
    ) THEN
        RAISE EXCEPTION 'session_environment_binding_invalid' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE CONSTRAINT TRIGGER session_environment_binding
AFTER INSERT OR UPDATE OF id, current_environment_id ON app_core_session DEFERRABLE INITIALLY DEFERRED
FOR EACH ROW EXECUTE FUNCTION enforce_session_environment_binding();

CREATE FUNCTION enforce_environment_identity() RETURNS trigger AS $$
BEGIN
    IF ROW(NEW.id, NEW.session_id, NEW.created_at) IS DISTINCT FROM ROW(OLD.id, OLD.session_id, OLD.created_at) THEN
        RAISE EXCEPTION 'session_environment_identity_immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER session_environment_identity BEFORE UPDATE OF id, session_id, created_at ON app_core_sessionenvironment
FOR EACH ROW EXECUTE FUNCTION enforce_environment_identity();

CREATE FUNCTION enforce_run_environment_binding() RETURNS trigger AS $$
BEGIN
    IF TG_OP='UPDATE' AND ROW(NEW.id, NEW.session_id, NEW.environment_id)
       IS DISTINCT FROM ROW(OLD.id, OLD.session_id, OLD.environment_id) THEN
        RAISE EXCEPTION 'agent_run_environment_binding_immutable' USING ERRCODE = '23514';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM app_core_sessionenvironment WHERE id=NEW.environment_id AND session_id=NEW.session_id)
       OR (TG_OP='INSERT' AND NOT EXISTS (
           SELECT 1 FROM app_core_session WHERE id=NEW.session_id AND current_environment_id=NEW.environment_id
       )) THEN
        RAISE EXCEPTION 'agent_run_environment_binding_invalid' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER agent_run_environment_binding BEFORE INSERT OR UPDATE OF id, session_id, environment_id ON app_core_agentrun
FOR EACH ROW EXECUTE FUNCTION enforce_run_environment_binding();
"""

REVERSE = """
DROP TRIGGER agent_run_environment_binding ON app_core_agentrun;
DROP FUNCTION enforce_run_environment_binding();
DROP TRIGGER session_environment_identity ON app_core_sessionenvironment;
DROP FUNCTION enforce_environment_identity();
DROP TRIGGER session_environment_binding ON app_core_session;
DROP FUNCTION enforce_session_environment_binding();
"""


def _backend_sql(schema_editor, sql):
    vendor = schema_editor.connection.vendor
    if vendor == "postgresql":
        schema_editor.execute(sql, params=None)
    elif vendor != "sqlite":
        raise NotSupportedError(f"session_environment_backend_unsupported: {vendor}")
    # SQLite is a structural migration gate, not an execution authorization backend.


def install_bindings(apps, schema_editor):
    _backend_sql(schema_editor, FORWARD)


def remove_bindings(apps, schema_editor):
    _backend_sql(schema_editor, REVERSE)


def guard_environment_reversal(apps, schema_editor):
    if apps.get_model("app_core", "SessionEnvironment").objects.using(schema_editor.connection.alias).exists():
        raise RuntimeError("session_environment_reversal_requires_empty_environments")


def backfill_environments(apps, schema_editor):
    Session = apps.get_model("app_core", "Session")
    Environment = apps.get_model("app_core", "SessionEnvironment")
    Run = apps.get_model("app_core", "AgentRun")
    alias = schema_editor.connection.alias
    sessions = Session.objects.using(alias).order_by("pk")
    # Include deleted and purged owners: Run bindings and signed history survive
    # content retention. Never rewrite historical authorization payloads.
    for session in sessions.iterator(chunk_size=500):
        environment = Environment.objects.using(alias).create(
            id="environment_" + uuid.uuid4().hex, session_id=session.pk,
        )
        sessions.filter(pk=session.pk).update(current_environment_id=environment.pk)
        Run.objects.using(alias).filter(session_id=session.pk).update(environment_id=environment.pk)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0015_note_revision_immutability")]
    operations = [
        migrations.CreateModel(
            name="SessionEnvironment",
            fields=[
                ("id", models.CharField(default=app_core.models.new_session_environment_id, max_length=64, primary_key=True, serialize=False)),
                ("created_at", models.DateTimeField(auto_now_add=True)),
                ("session", models.ForeignKey(on_delete=django.db.models.deletion.PROTECT, related_name="environments", to="app_core.session")),
            ],
        ),
        migrations.AddField(
            model_name="session", name="current_environment",
            field=models.ForeignKey(blank=True, editable=False, null=True, on_delete=django.db.models.deletion.PROTECT, related_name="+", to="app_core.sessionenvironment"),
        ),
        migrations.AddField(
            model_name="agentrun", name="environment",
            field=models.ForeignKey(editable=False, null=True, on_delete=django.db.models.deletion.PROTECT, related_name="agent_runs", to="app_core.sessionenvironment"),
        ),
        migrations.RunPython(backfill_environments, migrations.RunPython.noop),
        migrations.AlterField(
            model_name="agentrun", name="environment",
            field=models.ForeignKey(editable=False, on_delete=django.db.models.deletion.PROTECT, related_name="agent_runs", to="app_core.sessionenvironment"),
        ),
        migrations.RunPython(install_bindings, remove_bindings),
        migrations.RunPython(migrations.RunPython.noop, guard_environment_reversal),
    ]
