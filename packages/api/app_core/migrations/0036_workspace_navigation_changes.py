"""Wake workspace Session navigation across historical Agent identities."""
from django.db import migrations


WORKSPACE_NAVIGATION = """
CREATE OR REPLACE FUNCTION public.workspace_presentation_agent_v1(agent_id text,kinds text[])
RETURNS void LANGUAGE plpgsql AS $$
DECLARE kind text; owner_workspace_id text;
BEGIN
    IF agent_id IS NULL THEN RETURN; END IF;
    FOREACH kind IN ARRAY kinds LOOP
        IF kind='navigation' THEN
            SELECT a.workspace_id INTO owner_workspace_id FROM public.app_core_agent a WHERE a.id=agent_id;
            PERFORM public.workspace_presentation_bump_v1('workspace:'||owner_workspace_id||':navigation');
        ELSE
            PERFORM public.workspace_presentation_bump_v1('agent:'||agent_id||':'||kind);
        END IF;
    END LOOP;
END;
$$;
"""

AGENT_NAVIGATION = """
CREATE OR REPLACE FUNCTION public.workspace_presentation_agent_v1(agent_id text,kinds text[])
RETURNS void LANGUAGE plpgsql AS $$
DECLARE kind text;
BEGIN
    IF agent_id IS NULL THEN RETURN; END IF;
    FOREACH kind IN ARRAY kinds LOOP
        PERFORM public.workspace_presentation_bump_v1('agent:'||agent_id||':'||kind);
    END LOOP;
END;
$$;
"""


def _replace_helper(schema_editor, sql):
    if schema_editor.connection.vendor == "sqlite":
        return  # Schema-generation checks do not run live presentation readers.
    if schema_editor.connection.vendor != "postgresql":
        raise RuntimeError("presentation_changes_require_postgresql")
    schema_editor.execute(sql)


def install(apps, schema_editor):
    _replace_helper(schema_editor, WORKSPACE_NAVIGATION)


def uninstall(apps, schema_editor):
    _replace_helper(schema_editor, AGENT_NAVIGATION)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0035_workspace_shared_agents")]
    operations = [migrations.RunPython(install, uninstall)]
