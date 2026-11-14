from django.db import migrations, models

from app_core.environment_resources import default_environment_resources


RESOURCE_VALIDATOR = """
CREATE FUNCTION public.workspace_environment_resources_valid_v1(resources jsonb)
RETURNS boolean AS $$
DECLARE
    field text;
    amount numeric;
BEGIN
    IF jsonb_typeof(resources) IS DISTINCT FROM 'object'
       OR NOT resources ?& ARRAY['cpuMilli','memoryBytes','pidsLimit','workspaceBytes','storageBytes']
       OR resources - ARRAY['cpuMilli','memoryBytes','pidsLimit','workspaceBytes','storageBytes'] <> '{}'::jsonb THEN
        RETURN false;
    END IF;
    FOREACH field IN ARRAY ARRAY['cpuMilli','memoryBytes','pidsLimit','workspaceBytes','storageBytes'] LOOP
        IF jsonb_typeof(resources->field) IS DISTINCT FROM 'number'
           OR resources->>field !~ '^[0-9]+$' THEN
            RETURN false;
        END IF;
        amount := (resources->>field)::numeric;
        IF amount < 1 OR amount > (CASE WHEN field IN ('cpuMilli','pidsLimit')
            THEN 4294967295::numeric ELSE 18446744073709551615::numeric END) THEN
            RETURN false;
        END IF;
    END LOOP;
    RETURN true;
END;
$$ LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE;
"""


SNAPSHOT_GUARD = """
CREATE FUNCTION public.protect_environment_resource_defaults() RETURNS trigger AS $$
BEGIN
    IF TG_OP='INSERT' THEN
        SELECT w.execution_resources, w.execution_settings_revision
        INTO NEW.resource_defaults, NEW.resource_defaults_revision
        FROM app_core_workspace w JOIN app_core_session s ON s.workspace_id=w.id
        WHERE s.id=NEW.session_id FOR SHARE OF w;
    ELSIF ROW(NEW.resource_defaults, NEW.resource_defaults_revision)
          IS DISTINCT FROM ROW(OLD.resource_defaults, OLD.resource_defaults_revision) THEN
        RAISE EXCEPTION 'environment_resource_defaults_immutable' USING ERRCODE='23514';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
-- This name orders the locking snapshot before environment_profile_binding,
-- whose existing INSERT guard reads the same Workspace network default.
CREATE TRIGGER environment_defaults_snapshot BEFORE INSERT OR UPDATE ON app_core_sessionenvironment
FOR EACH ROW EXECUTE FUNCTION public.protect_environment_resource_defaults();
"""


def backfill_resource_defaults(apps, schema_editor):
    environments = apps.get_model("app_core", "SessionEnvironment").objects.using(schema_editor.connection.alias)
    for environment in environments.select_related("session__workspace").iterator():
        resources = (environment.environment_profile["resources"] if environment.environment_profile is not None
            else environment.session.workspace.execution_resources)
        environments.filter(pk=environment.pk).update(
            resource_defaults=resources, resource_defaults_revision=None)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0055_session_purge_intent")]

    operations = [
        migrations.AddField("workspace", "execution_resources", models.JSONField(default=default_environment_resources)),
        migrations.AddField("workspace", "execution_settings_revision",
            models.PositiveBigIntegerField(default=1, editable=False)),
        migrations.AddField("sessionenvironment", "resource_defaults",
            models.JSONField(default=default_environment_resources, editable=False)),
        migrations.AddField("sessionenvironment", "resource_defaults_revision",
            models.PositiveBigIntegerField(default=None, editable=False, null=True)),
        migrations.RunPython(backfill_resource_defaults, migrations.RunPython.noop),
        migrations.RunSQL(RESOURCE_VALIDATOR,
            "DROP FUNCTION public.workspace_environment_resources_valid_v1(jsonb);"),
        migrations.AddConstraint("workspace", models.CheckConstraint(
            condition=models.Func("execution_resources", function="public.workspace_environment_resources_valid_v1",
                output_field=models.BooleanField()), name="workspace_execution_resources_valid")),
        migrations.AddConstraint("workspace", models.CheckConstraint(
            condition=models.Q(execution_settings_revision__gt=0), name="workspace_execution_settings_revision_positive")),
        migrations.AddConstraint("sessionenvironment", models.CheckConstraint(
            condition=models.Func("resource_defaults", function="public.workspace_environment_resources_valid_v1",
                output_field=models.BooleanField()), name="environment_resource_defaults_valid")),
        migrations.AddConstraint("sessionenvironment", models.CheckConstraint(
            condition=(models.Q(resource_defaults_revision__isnull=True) | models.Q(resource_defaults_revision__gt=0)),
            name="environment_resource_defaults_revision_positive")),
        migrations.RunSQL(SNAPSHOT_GUARD, """
            DROP TRIGGER environment_defaults_snapshot ON app_core_sessionenvironment;
            DROP FUNCTION public.protect_environment_resource_defaults();
        """),
    ]
