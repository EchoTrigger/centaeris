from importlib import import_module

from django.db import NotSupportedError, migrations

guard_note_schema_reversal = import_module(
    "app_core.migrations.0014_library_note_revisions"
).guard_note_schema_reversal


FORWARD = """
CREATE FUNCTION prevent_note_revision_rewrite() RETURNS trigger AS $$
BEGIN
    IF ROW(NEW."note_id", NEW.generation, NEW."displayName", NEW.markdown,
           NEW.sha256, NEW."sizeBytes", NEW."createdAt", NEW."createdBy_id")
       IS DISTINCT FROM
       ROW(OLD."note_id", OLD.generation, OLD."displayName", OLD.markdown,
           OLD.sha256, OLD."sizeBytes", OLD."createdAt", OLD."createdBy_id") THEN
        RAISE EXCEPTION 'note_revision_immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER note_revision_immutable BEFORE UPDATE ON app_core_librarynoterevision
FOR EACH ROW EXECUTE FUNCTION prevent_note_revision_rewrite();
"""
REVERSE = """
DROP TRIGGER note_revision_immutable ON app_core_librarynoterevision;
DROP FUNCTION prevent_note_revision_rewrite();
"""


def _run_postgresql_sql(schema_editor, sql):
    vendor = schema_editor.connection.vendor
    if vendor == "postgresql":
        schema_editor.execute(sql, params=None)
    elif vendor != "sqlite":
        raise NotSupportedError(f"note_revision_immutability_backend_unsupported: {vendor}")
    # SQLite is used only by the structural migration gate. Production note
    # immutability is enforced and tested with PostgreSQL.


def install_immutability(apps, schema_editor):
    _run_postgresql_sql(schema_editor, FORWARD)


def remove_immutability(apps, schema_editor):
    _run_postgresql_sql(schema_editor, REVERSE)


class Migration(migrations.Migration):
    dependencies = [("app_core", "0014_library_note_revisions")]
    operations = [
        migrations.RunPython(install_immutability, remove_immutability),
        # Reversal runs in reverse order: refuse populated schemas before the
        # trigger can be removed, including when reversing only this migration.
        migrations.RunPython(migrations.RunPython.noop, guard_note_schema_reversal),
    ]
