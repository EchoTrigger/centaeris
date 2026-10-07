"""Shared Workspace material evidence contracts; no runtime or transport dependency."""

import hashlib
from dataclasses import dataclass


MAX_READ_BYTES = 50 * 1024
MAX_READ_LINES = 2_000
MAX_SEARCH_SNIPPET_BYTES = 8 * 1024


class KnowledgeError(RuntimeError):
    def __init__(self, code: str, status: int = 409):
        super().__init__(code)
        self.code = code
        self.status = status


@dataclass(frozen=True)
class BoundInput:
    resolved: dict
    storage_key: str
    owner: object
    input_identity: dict
    representation_id: str


def sha256_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"
