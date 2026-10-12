"""Bound note history independently from durable save-operation receipts."""

from datetime import timedelta

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import transaction
from django.utils import timezone

from .library_notes import NoteSaveError, _receipt, _replay, _revision, request_digest, validate_save
from .models import LibraryNoteRevision, UserLibraryObject


def _plan(note):
    revisions = list(note.noteRevisions.order_by("-generation"))
    current = next((item for item in revisions if item.id == note.noteHead_id), None)
    if current is None:
        return []
    protected = [item for item in revisions if item.protected and item.id != current.id]
    retained = {current.id, *(item.id for item in protected)}
    size = sum(item.sizeBytes for item in revisions if item.id in retained)
    max_bytes = settings.NOTE_HISTORY_MAX_BYTES
    max_count = settings.NOTE_HISTORY_MAX_VERSIONS
    if size > max_bytes or len(retained) > max_count:
        raise NoteSaveError("note_history_protection_capacity", 413)
    cutoff = timezone.now() - timedelta(days=settings.NOTE_HISTORY_DAYS)
    days = set()
    for index, revision in enumerate(revisions):
        day = revision.createdAt.date()
        candidate = index < settings.NOTE_HISTORY_RECENT or (revision.createdAt >= cutoff and day not in days)
        days.add(day)
        if (candidate and revision.id not in retained and len(retained) < max_count
                and size + revision.sizeBytes <= max_bytes):
            retained.add(revision.id)
            size += revision.sizeBytes
    return [item.id for item in revisions if item.id not in retained]


def prune_history(note, *, dry_run=False):
    planned = _plan(note)
    if not dry_run:
        LibraryNoteRevision.objects.filter(id__in=planned).delete()
    return planned


def collect_history(note_id, *, dry_run=False):
    owner_id = UserLibraryObject.objects.values_list("owner_id", flat=True).get(pk=note_id)
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=owner_id)
        note = UserLibraryObject.objects.select_for_update().get(pk=note_id)
        if note.purgedAt is not None:
            planned = list(note.noteRevisions.values_list("id", flat=True))
            if not dry_run:
                note.noteHead = None
                note.save(update_fields=["noteHead"])
                note.noteRevisions.all().delete()
            return planned
        return prune_history(note, dry_run=dry_run)


def protect_revision(owner, note_id, generation, protected):
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=owner.pk)
        note = UserLibraryObject.objects.select_for_update().get(pk=note_id, owner=owner, status="ready")
        revision = note.noteRevisions.get(generation=generation)
        if protected:
            others = list(note.noteRevisions.filter(protected=True).exclude(pk=revision.pk))
            if (sum(item.sizeBytes for item in others) + revision.sizeBytes
                    > settings.NOTE_HISTORY_MAX_BYTES - settings.NOTE_MAX_BYTES
                    or len(others) + 1 >= settings.NOTE_HISTORY_MAX_VERSIONS):
                raise NoteSaveError("note_history_protection_capacity", 413)
        LibraryNoteRevision.objects.filter(pk=revision.pk).update(protected=protected)


def restore_revision(owner, note_id, *, save_id, base_generation, revision_generation):
    from .http.library import _require_library_name_available
    validate_save(save_id, "restore", "")
    digest = request_digest("restore", note_id, {"baseGeneration": base_generation,
                                                "revisionGeneration": revision_generation})
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=owner.pk)
        replay = _replay(owner, save_id, digest)
        if replay is not None:
            return replay
        note = UserLibraryObject.objects.select_for_update().get(pk=note_id, owner=owner, status="ready", objectKind="note")
        if note.contentGeneration != base_generation:
            raise NoteSaveError("note_generation_conflict")
        revision = note.noteRevisions.get(generation=revision_generation)
        _require_library_name_available(owner, note.parentFolder, revision.displayName, exclude_id=note.id)
        _revision(note, revision.displayName, revision.markdown, revision.markdown.encode("utf-8"),
                  owner, note.contentGeneration + 1)
        return _receipt(owner, note, save_id, digest)
