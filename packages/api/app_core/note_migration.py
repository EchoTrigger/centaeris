"""Bounded, restartable note backfill and explicit rollback materialization."""

import hashlib

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.storage import default_storage
from django.db import transaction

from .assets import store_bytes
from .models import LibraryNoteRevision, UserLibraryObject


def _identity(note):
    return (note.owner_id, note.storageKey, note.contentGeneration, note.sha256,
            note.sizeBytes, note.displayName, note.status, note.noteHead_id)


def migrate_note(note_id, *, dry_run=False):
    captured = UserLibraryObject.objects.get(pk=note_id, objectKind="note", purgedAt__isnull=True)
    if captured.noteHead_id:
        return "already_migrated"
    with default_storage.open(captured.storageKey, "rb") as source:
        body = source.read(getattr(settings, "NOTE_MAX_BYTES", 1024 * 1024) + 1)
    if (len(body) != captured.sizeBytes or "sha256:" + hashlib.sha256(body).hexdigest() != captured.sha256
            or captured.contentGeneration <= 0):
        raise ValueError("note_migration_identity_mismatch")
    markdown = body.decode("utf-8")
    if dry_run:
        return "would_migrate"
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=captured.owner_id)
        note = UserLibraryObject.objects.select_for_update().get(pk=note_id)
        if _identity(note) != _identity(captured) or note.purgedAt is not None:
            raise ValueError("note_migration_source_changed")
        revision, _ = LibraryNoteRevision.objects.get_or_create(
            note=note, generation=note.contentGeneration,
            defaults={"displayName": note.displayName, "markdown": markdown, "sha256": note.sha256,
                      "sizeBytes": note.sizeBytes, "createdBy_id": note.owner_id},
        )
        if (revision.markdown != markdown or revision.sha256 != note.sha256
                or revision.sizeBytes != note.sizeBytes or revision.displayName != note.displayName):
            raise ValueError("note_migration_revision_conflict")
        note.noteHead = revision
        note.save(update_fields=["noteHead"])
    return "migrated"


def export_note(note_id, *, dry_run=False):
    from .note_content import content_source
    captured = UserLibraryObject.objects.select_related("noteHead").get(pk=note_id, objectKind="note", purgedAt__isnull=True)
    if not captured.noteHead_id:
        return "already_file_backed"
    with content_source(captured).open() as source:
        body = source.read()
    if dry_run:
        return "would_export"
    metadata = store_bytes(body, f"users/{captured.owner_id}/library", captured.displayName + ".md", "text/markdown")
    # Failed publication leaves an orphan for the existing scanner; never delete
    # bytes here after an uncertain database commit outcome.
    with default_storage.open(metadata["storageKey"], "rb") as verification:
        if verification.read() != body:
            raise ValueError("note_export_verification_failed")
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=captured.owner_id)
        note = UserLibraryObject.objects.select_for_update().get(pk=note_id)
        if _identity(note) != _identity(captured) or note.purgedAt is not None:
            raise ValueError("note_migration_source_changed")
        note.storageKey, note.noteHead = metadata["storageKey"], None
        note.save(update_fields=["storageKey", "noteHead"])
    return "exported"
