"""Read an already-authorized immutable body; never grant access here."""

from dataclasses import dataclass
import hashlib
from io import BytesIO

from django.core.files.storage import default_storage

from .models import UserLibraryObject


@dataclass(frozen=True)
class StoredObject:
    storage_key: str

    def open(self):
        return default_storage.open(self.storage_key, "rb")


@dataclass(frozen=True)
class NoteRevision:
    revision_id: int
    body: bytes

    def open(self):
        return BytesIO(self.body)


ContentSource = StoredObject | NoteRevision


def content_source(owner) -> ContentSource:
    if isinstance(owner, UserLibraryObject) and owner.objectKind == "note" and owner.noteHead_id:
        revision = owner.noteHead
        if (revision.note_id != owner.id or revision.generation != owner.contentGeneration or revision.sha256 != owner.sha256
                or revision.sizeBytes != owner.sizeBytes or revision.displayName != owner.displayName):
            raise ValueError("note_head_identity_mismatch")
        body = revision.markdown.encode("utf-8")
        if len(body) != revision.sizeBytes or "sha256:" + hashlib.sha256(body).hexdigest() != revision.sha256:
            raise ValueError("note_revision_identity_mismatch")
        # Capture the bytes before returning: later history GC cannot truncate a read.
        return NoteRevision(revision.id, body)
    if not owner.storageKey or not default_storage.exists(owner.storageKey):
        raise FileNotFoundError("content_not_available")
    return StoredObject(owner.storageKey)


async def content_response(source, content_type, filename, *, as_attachment, content_length):
    from .http.storage_stream import stored_file_response
    if isinstance(source, str):
        source = StoredObject(source)
    if isinstance(source, StoredObject):
        return await stored_file_response(source.storage_key, content_type, filename,
                                          as_attachment=as_attachment, content_length=content_length)
    async def selected_open():
        return source.open()
    return await stored_file_response("", content_type, filename, as_attachment=as_attachment,
                                      content_length=content_length, authorized_open=selected_open)
