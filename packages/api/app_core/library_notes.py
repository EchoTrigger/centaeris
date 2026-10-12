"""Conditional, idempotent note commits. No storage I/O inside transactions."""

import hashlib
import json
from uuid import UUID

from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import transaction

from .assets import tombstone_superseded_derived_resources
from .models import LibraryNoteRevision, LibraryNoteSaveReceipt, UserLibraryLink, UserLibraryObject


class NoteSaveError(ValueError):
    def __init__(self, code, status=409):
        self.code, self.status = code, status
        super().__init__(code)


def request_digest(operation, note_id, payload):
    return hashlib.sha256(json.dumps([operation, note_id, payload], ensure_ascii=False,
                                    sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()


def validate_save(save_id, display_name, markdown):
    from .http.library import _require_library_display_name
    try:
        if not isinstance(save_id, str) or len(save_id) != 32 or UUID(hex=save_id).hex != save_id:
            raise ValueError("invalid save ID")
        display_name = _require_library_display_name(display_name)
        if "\x00" in markdown or "\x00" in display_name:
            raise ValueError("invalid text")
        body = markdown.encode("utf-8")
    except (ValueError, AttributeError, UnicodeError):
        raise NoteSaveError("library_note_request_invalid", 400) from None
    if len(body) > getattr(settings, "NOTE_MAX_BYTES", 1024 * 1024):
        raise NoteSaveError("note_body_too_large", 413)
    return display_name, body


def _replay(owner, save_id, digest):
    receipt = LibraryNoteSaveReceipt.objects.filter(owner=owner, saveId=save_id).first()
    if receipt is None:
        return None
    if receipt.requestDigest != digest:
        raise NoteSaveError("note_save_id_conflict")
    if not UserLibraryObject.objects.filter(pk=receipt.noteId, owner=owner, status="ready",
                                           objectKind="note", purgedAt__isnull=True).exists():
        raise NoteSaveError("library_note_not_found", 404)
    return receipt.resultMetadata


def _receipt(owner, note, save_id, digest):
    from .http.serialization import serialize_library_object
    result = {"object": serialize_library_object(note), "saveId": save_id,
              "appliedGeneration": note.contentGeneration}
    LibraryNoteSaveReceipt.objects.create(owner=owner, saveId=save_id, requestDigest=digest,
                                         noteId=note.id, appliedGeneration=note.contentGeneration,
                                         resultMetadata=result)
    return result


def _revision(note, display_name, markdown, body, actor, generation):
    revision = LibraryNoteRevision.objects.create(
        note=note, generation=generation, displayName=display_name, markdown=markdown,
        sha256="sha256:" + hashlib.sha256(body).hexdigest(), sizeBytes=len(body), createdBy=actor,
    )
    note.noteHead = revision
    note.displayName, note.contentGeneration = display_name, generation
    note.sha256, note.sizeBytes = revision.sha256, revision.sizeBytes
    note.save(update_fields=["noteHead", "displayName", "contentGeneration", "sha256", "sizeBytes", "updatedAt"])
    tombstone_superseded_derived_resources(note)
    from .note_history import prune_history
    prune_history(note)


def require_current_revision(note):
    from .note_content import content_source
    if note.noteHead_id is None:
        raise NoteSaveError("note_migration_required")
    try:
        content_source(note)
    except ValueError:
        raise NoteSaveError("note_migration_required") from None


def create_note(owner, *, save_id, display_name, markdown, parent_folder_id=""):
    from .http.library import _library_parent, _require_library_name_available
    display_name, body = validate_save(save_id, display_name, markdown)
    digest = request_digest("create", "", {"displayName": display_name, "markdown": markdown,
                                           "parentFolderId": parent_folder_id})
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=owner.pk)
        replay = _replay(owner, save_id, digest)
        if replay is not None:
            return replay
        parent = _library_parent(owner, parent_folder_id)
        _require_library_name_available(owner, parent, display_name)
        note = UserLibraryObject.objects.create(owner=owner, displayName=display_name, objectKind="note",
                                               contentType="text/markdown", status="ready", parentFolder=parent)
        _revision(note, display_name, markdown, body, owner, 1)
        UserLibraryLink.objects.create(libraryObject=note, sourceKind="manual")
        return _receipt(owner, note, save_id, digest)


def save_note(owner, note_id, *, save_id, base_generation, display_name, markdown):
    from .http.library import _require_library_name_available
    display_name, body = validate_save(save_id, display_name, markdown)
    if type(base_generation) is not int or base_generation < 0:
        raise NoteSaveError("library_note_request_invalid", 400)
    digest = request_digest("update", note_id, {"displayName": display_name, "markdown": markdown,
                                               "baseGeneration": base_generation})
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=owner.pk)
        replay = _replay(owner, save_id, digest)
        if replay is not None:
            return replay
        try:
            note = UserLibraryObject.objects.select_for_update().get(pk=note_id, owner=owner, objectKind="note",
                                                                    status="ready", purgedAt__isnull=True)
        except UserLibraryObject.DoesNotExist:
            raise NoteSaveError("library_note_not_found", 404) from None
        require_current_revision(note)
        if note.contentGeneration != base_generation:
            raise NoteSaveError("note_generation_conflict")
        _require_library_name_available(owner, note.parentFolder, display_name, exclude_id=note.id)
        if note.noteHead.displayName != display_name or note.noteHead.markdown != markdown:
            _revision(note, display_name, markdown, body, owner, note.contentGeneration + 1)
        return _receipt(owner, note, save_id, digest)


def rename_note(owner, note_id, *, save_id, base_generation, display_name, parent_folder_id, move):
    from .http.library import _library_parent, _require_library_name_available
    display_name, _ = validate_save(save_id, display_name, "")
    digest = request_digest("rename", note_id, {"displayName": display_name, "baseGeneration": base_generation,
                                               "move": move, "parentFolderId": parent_folder_id})
    with transaction.atomic():
        get_user_model().objects.select_for_update().get(pk=owner.pk)
        replay = _replay(owner, save_id, digest)
        if replay is not None:
            return replay
        note = UserLibraryObject.objects.select_for_update().get(pk=note_id, owner=owner, objectKind="note", status="ready")
        require_current_revision(note)
        if type(base_generation) is not int or note.contentGeneration != base_generation:
            raise NoteSaveError("note_generation_conflict")
        parent = _library_parent(owner, parent_folder_id, moving_object_id=note.id) if move else note.parentFolder
        _require_library_name_available(owner, parent, display_name, exclude_id=note.id)
        if move:
            note.parentFolder = parent
            note.save(update_fields=["parentFolder", "updatedAt"])
        if note.displayName != display_name:
            markdown = note.noteHead.markdown
            _revision(note, display_name, markdown, markdown.encode("utf-8"), owner, note.contentGeneration + 1)
        return _receipt(owner, note, save_id, digest)
