from django.db import NotSupportedError, migrations


ADD_CONSTRAINT = """
ALTER TABLE public.app_core_librarynoterevision
ADD CONSTRAINT note_revision_body_identity CHECK (
    "sizeBytes" = pg_catalog.octet_length(pg_catalog.convert_to(markdown, 'UTF8'))
    AND sha256 = 'sha256:' || pg_catalog.encode(pg_catalog.sha256(pg_catalog.convert_to(markdown, 'UTF8')), 'hex')
) NOT VALID
"""


def _postgres(schema_editor, sql):
    vendor = schema_editor.connection.vendor
    if vendor == "postgresql":
        schema_editor.execute(sql, params=None)
    elif vendor != "sqlite":
        raise NotSupportedError(f"note_revision_body_identity_backend_unsupported: {vendor}")


def add_constraint(apps, schema_editor):
    _postgres(schema_editor, ADD_CONSTRAINT)


def drop_constraint(apps, schema_editor):
    _postgres(schema_editor, "ALTER TABLE public.app_core_librarynoterevision DROP CONSTRAINT note_revision_body_identity")


class Migration(migrations.Migration):
    dependencies = [("app_core", "0051_agent_api_release_cleanup")]
    operations = [migrations.RunPython(add_constraint, drop_constraint)]
