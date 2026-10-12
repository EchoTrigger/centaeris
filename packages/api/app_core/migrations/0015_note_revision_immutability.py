from django.db import migrations


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


class Migration(migrations.Migration):
    dependencies = [("app_core", "0014_library_note_revisions")]
    operations = [migrations.RunSQL(FORWARD, REVERSE)]
