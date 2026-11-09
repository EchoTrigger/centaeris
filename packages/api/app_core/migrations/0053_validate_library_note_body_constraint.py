from django.db import migrations


def validate_constraint(apps, schema_editor):
    if schema_editor.connection.vendor == "postgresql":
        schema_editor.execute(
            "ALTER TABLE public.app_core_librarynoterevision VALIDATE CONSTRAINT note_revision_body_identity",
            params=None,
        )


class Migration(migrations.Migration):
    dependencies = [("app_core", "0052_library_note_body_constraint")]
    # Validation is monotonic while the constraint exists. Reversing its
    # addition removes the constraint without changing any revision data.
    operations = [migrations.RunPython(validate_constraint, migrations.RunPython.noop)]
