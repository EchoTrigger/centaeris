"""Integrity-checked material reads and bounded commit stream storage."""

import hashlib

from django.core.files import File
from django.core.files.storage import default_storage

from .assets import delete_stored_object
from .material_contract import KnowledgeError, sha256_bytes


def read_stored(storage_key: str, size_bytes: int, expected_hash: str) -> bytes:
    if not default_storage.exists(storage_key):
        raise KnowledgeError("knowledge_storage_missing")
    with default_storage.open(storage_key, "rb") as source:
        content = source.read(size_bytes + 1)
    if len(content) != size_bytes or sha256_bytes(content) != expected_hash:
        raise KnowledgeError("knowledge_storage_integrity_mismatch")
    return content


def store_stream(request, storage_key: str, size_bytes: int, expected_hash: str) -> bool:
    if default_storage.exists(storage_key):
        _validate_stored(storage_key, size_bytes, expected_hash)
        _discard_exact(request, size_bytes, expected_hash)
        return False
    reader = _HashingReader(request, size_bytes)
    stored_key = default_storage.save(storage_key, File(reader, name=storage_key.rsplit("/", 1)[-1]))
    if stored_key != storage_key:
        delete_stored_object(stored_key)
        raise KnowledgeError("knowledge_storage_key_conflict")
    try:
        reader.require_complete(expected_hash)
        _validate_stored(storage_key, size_bytes, expected_hash)
    except Exception:
        # save returned this exact key, so this invocation owns the new object.
        # The caller cannot register it for rollback until this function returns.
        delete_stored_object(storage_key)
        raise
    return True


def _validate_stored(storage_key: str, size_bytes: int, expected_hash: str) -> None:
    if not default_storage.exists(storage_key):
        raise KnowledgeError("knowledge_storage_missing")
    digest = hashlib.sha256()
    actual_size = 0
    with default_storage.open(storage_key, "rb") as source:
        while chunk := source.read(1024 * 1024):
            actual_size += len(chunk)
            if actual_size > size_bytes:
                raise KnowledgeError("knowledge_storage_integrity_mismatch")
            digest.update(chunk)
    if actual_size != size_bytes or f"sha256:{digest.hexdigest()}" != expected_hash:
        raise KnowledgeError("knowledge_storage_integrity_mismatch")


class _HashingReader:
    def __init__(self, source, size_bytes: int):
        self.source = source
        self.remaining = size_bytes
        self.digest = hashlib.sha256()

    def read(self, size: int = -1) -> bytes:
        if not self.remaining:
            return b""
        requested = self.remaining if size is None or size < 0 else min(size, self.remaining)
        chunk = self.source.read(requested)
        if not chunk:
            raise KnowledgeError("knowledge_commit_body_truncated", 400)
        self.remaining -= len(chunk)
        self.digest.update(chunk)
        return chunk

    def require_complete(self, expected_hash: str):
        if self.remaining or f"sha256:{self.digest.hexdigest()}" != expected_hash:
            raise KnowledgeError("knowledge_commit_integrity_mismatch")


def _discard_exact(request, size_bytes: int, expected_hash: str):
    reader = _HashingReader(request, size_bytes)
    while reader.read(64 * 1024):
        pass
    reader.require_complete(expected_hash)
