"""Structural migration checks use SQLite; production constraints use PostgreSQL."""

import os
from importlib import import_module
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
from unittest import skipUnless
from unittest.mock import Mock

from django.db import IntegrityError, NotSupportedError, connection, transaction
from django.db.migrations.exceptions import IrreversibleError
from django.db.migrations.executor import MigrationExecutor
from django.db.migrations.recorder import MigrationRecorder
from django.db.migrations.state import ProjectState
from django.test import SimpleTestCase, TransactionTestCase
from django.utils import timezone

from .migration_testing import isolated_migration_database


PRE_NOTE_SCHEMA = [("app_core", "0013_remove_model_quota_domain")]
NOTE_SCHEMA = [("app_core", "0014_library_note_revisions")]
IMMUTABLE_NOTE_SCHEMA = [("app_core", "0015_note_revision_immutability")]


class NoteSQLiteMigrationTests(SimpleTestCase):
    def test_empty_sqlite_database_migrates_without_model_drift(self):
        api_root = Path(__file__).resolve().parents[1]
        for arguments in (
            ["migrate", "--noinput"],
            ["makemigrations", "--check", "--dry-run", "--skip-checks"],
        ):
            with self.subTest(command=arguments[0]):
                result = subprocess.run(
                    [sys.executable, str(api_root / "manage.py"), *arguments,
                     "--settings=api.migration_test_settings"],
                    cwd=api_root, env={**os.environ, "PYTHONIOENCODING": "utf-8"},
                    capture_output=True, text=True, encoding="utf-8", timeout=60,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unknown_backend_refuses_to_install_or_remove_constraints(self):
        migration = import_module("app_core.migrations.0015_note_revision_immutability")
        operation = migration.Migration.operations[0]
        editor = SimpleNamespace(connection=SimpleNamespace(vendor="unknown", alias="default"), execute=Mock())
        state = ProjectState()
        for direction in (operation.database_forwards, operation.database_backwards):
            with self.subTest(direction=direction.__name__):
                with self.assertRaisesRegex(NotSupportedError, "note_revision_immutability_backend_unsupported"):
                    direction("app_core", editor, state, state)
        editor.execute.assert_not_called()


@skipUnless(connection.vendor == "postgresql", "Production note constraints require PostgreSQL")
class NotePostgreSQLMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_old_file_note_schema_upgrade_preserves_live_and_recoverable_note_metadata(self):
        with isolated_migration_database(PRE_NOTE_SCHEMA) as isolated:
            alias = isolated.alias
            old = MigrationExecutor(isolated).loader.project_state(PRE_NOTE_SCHEMA).apps
            user = old.get_model("auth", "User").objects.using(alias).create(username="old-file-note-owner")
            notes = old.get_model("app_core", "UserLibraryObject").objects.using(alias)
            ready = notes.create(
                owner_id=user.pk, objectKind="note", displayName="原始标题", status="ready",
                contentGeneration=7, storageKey="users/legacy/ready.md", sizeBytes=123,
                sha256="sha256:" + "1" * 64, contentType="text/markdown",
            )
            deleted = notes.create(
                owner_id=user.pk, objectKind="note", displayName="可恢复笔记", status="deleted",
                deletedAt=timezone.now(), deletedFromStatus="ready", contentGeneration=8,
                storageKey="users/legacy/deleted.md", sizeBytes=456,
                sha256="sha256:" + "2" * 64, contentType="text/markdown",
            )
            originals = {pk: notes.values().get(pk=pk) for pk in (ready.pk, deleted.pk)}
            executor = MigrationExecutor(isolated)
            executor.migrate(IMMUTABLE_NOTE_SCHEMA)
            current = executor.loader.project_state(IMMUTABLE_NOTE_SCHEMA).apps
            for pk, original in originals.items():
                migrated = current.get_model("app_core", "UserLibraryObject").objects.using(alias).values().get(pk=pk)
                self.assertEqual(migrated, {**original, "noteHead_id": None})
            self.assertFalse(current.get_model("app_core", "LibraryNoteRevision").objects.using(alias).exists())
            self.assertFalse(current.get_model("app_core", "LibraryNoteSaveReceipt").objects.using(alias).exists())

    def test_empty_schema_can_reverse_and_reinstall_immutability(self):
        with isolated_migration_database(IMMUTABLE_NOTE_SCHEMA) as isolated:
            MigrationExecutor(isolated).migrate(NOTE_SCHEMA)
            MigrationExecutor(isolated).migrate(IMMUTABLE_NOTE_SCHEMA)
            MigrationExecutor(isolated).migrate(PRE_NOTE_SCHEMA)
            self.assertNotIn("app_core_librarynoterevision", isolated.introspection.table_names())
            self.assertNotIn("app_core_librarynotesavereceipt", isolated.introspection.table_names())
            MigrationExecutor(isolated).migrate(IMMUTABLE_NOTE_SCHEMA)
            revision = self._create_revision(isolated)
            self._assert_body_rewrite_rejected(isolated, revision)

    def test_populated_schema_reversal_retains_rows_and_installed_trigger(self):
        for retained in ("revision", "receipt"):
            with self.subTest(retained=retained), isolated_migration_database(IMMUTABLE_NOTE_SCHEMA) as isolated:
                apps = MigrationExecutor(isolated).loader.project_state(IMMUTABLE_NOTE_SCHEMA).apps
                if retained == "revision":
                    revision = self._create_revision(isolated)
                    retained_model = apps.get_model("app_core", "LibraryNoteRevision")
                else:
                    user = apps.get_model("auth", "User").objects.using(isolated.alias).create(username="receipt-owner")
                    retained_model = apps.get_model("app_core", "LibraryNoteSaveReceipt")
                    retained_model.objects.using(isolated.alias).create(
                        owner_id=user.pk, saveId="1" * 32, requestDigest="2" * 64,
                        noteId="removed-note", appliedGeneration=2, resultMetadata={"appliedGeneration": 2},
                    )
                for target in (NOTE_SCHEMA, PRE_NOTE_SCHEMA):
                    with self.subTest(target=target):
                        with self.assertRaisesRegex(IrreversibleError, "note_history_requires_export_and_retained_schema"):
                            MigrationExecutor(isolated).migrate(target)
                        self.assertEqual(retained_model.objects.using(isolated.alias).count(), 1)
                        self.assertIn(IMMUTABLE_NOTE_SCHEMA[0], MigrationRecorder(isolated).applied_migrations())
                if retained == "receipt":
                    revision = self._create_revision(isolated)
                self._assert_body_rewrite_rejected(isolated, revision)

    def _create_revision(self, isolated):
        apps = MigrationExecutor(isolated).loader.project_state(IMMUTABLE_NOTE_SCHEMA).apps
        alias = isolated.alias
        user = apps.get_model("auth", "User").objects.using(alias).create(username="immutable-schema-owner")
        note = apps.get_model("app_core", "UserLibraryObject").objects.using(alias).create(
            owner_id=user.pk, objectKind="note", displayName="note", status="ready",
        )
        return apps.get_model("app_core", "LibraryNoteRevision").objects.using(alias).create(
            note_id=note.pk, generation=1, displayName="note", markdown="retained",
            sha256="sha256:" + "0" * 64, sizeBytes=8, createdBy_id=user.pk,
        )

    def _assert_body_rewrite_rejected(self, isolated, revision):
        with self.assertRaisesRegex(IntegrityError, "note_revision_immutable"), transaction.atomic(using=isolated.alias):
            with isolated.cursor() as cursor:
                cursor.execute("UPDATE app_core_librarynoterevision SET markdown = %s WHERE id = %s",
                               ["rewritten", revision.pk])
        revision.refresh_from_db(using=isolated.alias)
        self.assertEqual(revision.markdown, "retained")
