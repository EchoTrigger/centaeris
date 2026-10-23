"""Bounded byte streams through an explicitly provisioned private host spool."""

import hashlib
import io
import os
from pathlib import Path
import stat

MAX_TRANSFER_BYTES = 128 * 1024 * 1024
TRANSFER_CHUNK_BYTES = 64 * 1024


class BulkTransferError(ValueError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def _identity(info):
    return info.st_dev, info.st_ino


def _reparse(info):
    return stat.S_ISLNK(info.st_mode) or bool(getattr(info, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))


def _regular(info):
    return stat.S_ISREG(info.st_mode) and not _reparse(info) and info.st_nlink == 1


def _root_info(path):
    if not path.is_absolute():
        raise BulkTransferError("spoolUnavailable", "Spool root must be an existing absolute private directory")
    for entry in (path, *path.parents):
        info = os.lstat(entry)
        if _reparse(info) or not stat.S_ISDIR(info.st_mode):
            raise BulkTransferError("spoolUnavailable", "Spool root cannot traverse a link or reparse point")
    return os.lstat(path)


def _remove_owned(path, identity):
    try:
        info = os.lstat(path)
        if _regular(info) and _identity(info) == identity:
            path.unlink()
    except FileNotFoundError:
        pass


class BulkSpool:
    def __init__(self, root):
        # Privacy/ownership is a host deployment prerequisite. No ACL is changed.
        try:
            path = Path(root)
            info = _root_info(path)
            self.root = path.resolve(strict=True)
            self.identity = _identity(info)
        except OSError:
            raise BulkTransferError("spoolUnavailable", "Spool root must be an existing absolute private directory") from None

    def prepare(self, operation, payload):
        if _identity(_root_info(self.root)) != self.identity:
            raise BulkTransferError("spoolUnavailable", "Spool root identity changed")
        suffix = ".upload" if operation == "uploadFile" else ".download"
        path = self.root / (payload["transferId"] + suffix)
        if path.parent != self.root:
            raise BulkTransferError("invalidTransfer", "Transfer path escaped its spool root")
        if operation == "uploadFile":
            before = os.lstat(path)
            if not _regular(before) or before.st_size != payload["byteLength"]:
                raise BulkTransferError("invalidTransfer", "Upload must be a regular private file of the expected size")
            flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
            fd = os.open(path, flags)
            try:
                opened = os.fstat(fd)
                if not _regular(opened) or _identity(opened) != _identity(before) or opened.st_size != payload["byteLength"]:
                    raise BulkTransferError("invalidTransfer", "Upload identity changed during admission")
                source = os.fdopen(fd, "rb")
            except BaseException:
                os.close(fd)
                raise
            return PreparedTransfer(path, source, operation, payload, _identity(opened))
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
        fd = os.open(path, flags, 0o600)
        opened = None
        try:
            opened = os.fstat(fd)
            if not _regular(opened):
                raise BulkTransferError("invalidTransfer", "Download did not create a regular private file")
            file = os.fdopen(fd, "wb")
        except BaseException:
            os.close(fd)
            if opened is not None:
                _remove_owned(path, _identity(opened))
            raise
        return PreparedTransfer(path, file, operation, payload, _identity(opened))


class HashingReader(io.RawIOBase):
    """Nonseekable binary IO keeps old/new SDK uploads streamed and one-pass."""
    def __init__(self, source, length, digest):
        self.source = source
        self.length = length
        self.digest = digest
        self.count = 0
        self.hash = hashlib.sha256()
        self.eof = False

    def readable(self):
        return True

    def fileno(self):
        # Preserve the regular file's size discovery in the pinned SDK/httpx
        # stack. This does not allow seeking or a second hashing pass.
        return self.source.fileno()

    def read(self, size=-1):
        if size == 0 or self.eof:
            return b""
        bound = TRANSFER_CHUNK_BYTES if size < 0 else min(size, TRANSFER_CHUNK_BYTES)
        chunk = self.source.read(bound)
        if not chunk:
            self.eof = True
            self.verify()
            return b""
        if self.count + len(chunk) > self.length:
            raise BulkTransferError("transferLengthMismatch", "Upload exceeded its declared byte length")
        self.count += len(chunk)
        self.hash.update(chunk)
        return chunk

    def readinto(self, buffer):
        chunk = self.read(len(buffer))
        buffer[:len(chunk)] = chunk
        return len(chunk)

    def verify(self):
        if not self.eof or self.count != self.length or self.hash.hexdigest() != self.digest:
            raise BulkTransferError("transferIntegrityMismatch", "Upload length or digest did not match its expected content")


class PreparedTransfer:
    def __init__(self, path, file, operation, payload, identity):
        self.path = path
        self.file = file
        self.operation = operation
        self.payload = payload
        self.identity = identity
        self.complete = False

    def execute(self, sandbox, request_timeout, timing):
        user = {"user": self.payload["user"]} if "user" in self.payload else {}
        if self.operation == "uploadFile":
            reader = HashingReader(self.file, self.payload["byteLength"], self.payload["sha256"])
            # The SDK consumes this existing one-pass spool/hash reader. This
            # boundary includes that consumption, not provider-internal time.
            with timing.span("sdkFileUpload"):
                sandbox.files.write(self.payload["path"], reader, request_timeout=request_timeout, **user)
            reader.verify()
            length, digest = reader.count, reader.hash.hexdigest()
        else:
            length = 0
            hash = hashlib.sha256()
            # Streaming consumption includes the existing bounded spool writes
            # and hash work. Host admission and final flush/close stay outside.
            with timing.span("sdkFileDownload"):
                with sandbox.files.read(self.payload["path"], format="stream", request_timeout=request_timeout, **user) as stream:
                    for chunk in stream:
                        if not isinstance(chunk, bytes) or length + len(chunk) > self.payload["maxBytes"]:
                            raise BulkTransferError("transferLimitExceeded", "Download exceeded its bounded byte budget")
                        for offset in range(0, len(chunk), TRANSFER_CHUNK_BYTES):
                            part = chunk[offset:offset + TRANSFER_CHUNK_BYTES]
                            if self.file.write(part) != len(part):
                                raise BulkTransferError("transferWriteFailed", "Download could not write its complete chunk")
                            hash.update(part)
                            length += len(part)
            self.file.flush()
            self.file.close()
            digest = hash.hexdigest()
        self.complete = True
        return {"sandboxId": self.payload["sandboxId"], "transferId": self.payload["transferId"], "byteLength": length, "sha256": digest}

    def close(self):
        try:
            self.file.close()
        finally:
            if self.operation == "downloadFile" and not self.complete:
                # Never remove a replacement or unrelated file after a failure.
                _remove_owned(self.path, self.identity)
