"""Validate stored bytes without owning authorization, capture, or publication.

An output is a private staging sink. It may contain partial or complete-looking
bytes after failure and must never be published until validation returns. Input
and output byte counts refer to stored bytes; expanded size counts file bodies,
not tar metadata/padding. No uncompressed temporary file is created here.
"""
import hashlib
import json
import tarfile

from backports import zstd

from .runtime_contract import require_sha256, validate_virtual_path


IO_BYTES = 64 * 1024
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_PATH_BYTES = 4096
MAX_PATH_DEPTH = 64
MAX_WINDOW_LOG = 20
MANIFEST_PATH = "workspace-manifest.json"
CONTENT_PREFIX = "workspace/"


class SnapshotValidationError(ValueError):
    pass


def validate_snapshot_stream(source, candidate, *, output=None, on_read=None):
    """Validate one explicitly encoded object, copying its exact stored bytes.

    ``on_read(count)`` observes actual source bytes including an EOF probe. The
    caller supplies policy/admission limits and a fenced private staging sink.
    A successful return contains the explicitly versioned manifest, or None for no object.
    """
    _validate_descriptor(candidate)
    source = _StoredReader(source, candidate, output, on_read)
    if candidate["snapshotSizeBytes"] == 0:
        source.require_complete()
        return None
    if candidate["encoding"] == "frameV1":
        size = int.from_bytes(_read_exact(source, 4), "big")
        if not 0 < size <= MAX_MANIFEST_BYTES:
            raise SnapshotValidationError("snapshot manifest size is invalid")
        manifest = _manifest(_read_exact(source, size), candidate)
        for file in manifest["files"]:
            _verify_file(source, file)
    else:
        manifest = _validate_archive(_ZstdReader(source), candidate)
    source.require_complete()
    return manifest


def _validate_descriptor(candidate):
    if (not isinstance(candidate.get("encoding"), str)
            or candidate["encoding"] not in {"frameV1", "tarZstdV1"}):
        raise SnapshotValidationError("snapshot encoding is invalid")
    for key in ["snapshotSizeBytes", "expandedSizeBytes", "fileCount"]:
        if type(candidate.get(key)) is not int or candidate[key] < 0:
            raise SnapshotValidationError("snapshot descriptor is invalid")
    if candidate["snapshotSizeBytes"] == 0:
        if (candidate.get("snapshotSha256") != "" or candidate["expandedSizeBytes"]
                or candidate["fileCount"] or candidate["encoding"] != "frameV1"):
            raise SnapshotValidationError("empty snapshot descriptor is invalid")
    else:
        try:
            require_sha256("snapshotSha256", candidate.get("snapshotSha256"))
        except ValueError as error:
            raise SnapshotValidationError(str(error)) from error
        if not 0 < candidate["fileCount"] <= 2_147_483_647:
            raise SnapshotValidationError("snapshot file count is invalid")


def manifest_files(manifest_bytes, candidate, *, canonical_order=False):
    """Project regular files after checking all nodes and the stored descriptor.

    Released v1 frames retain their existing key-order policy. V2 has an exact
    canonical order so directory/link metadata shares the manifest identity.
    """
    return _manifest(manifest_bytes, candidate, canonical_order=canonical_order)["files"]


def _manifest(manifest_bytes, candidate, *, canonical_order=False):
    try:
        if not 0 < len(manifest_bytes) <= MAX_MANIFEST_BYTES:
            raise ValueError("snapshot manifest size is invalid")
        manifest = json.loads(manifest_bytes.decode("utf-8"))
        if not isinstance(manifest, dict):
            raise ValueError("snapshot manifest fields are invalid")
        schema = manifest.get("schema")
        if schema == "workspace.snapshot.v1":
            fields = {"schema", "files"}
            directories, symlinks = [], []
        elif schema == "workspace.snapshot.v2":
            fields = {"schema", "files", "directories", "symlinks"}
            directories, symlinks = manifest.get("directories"), manifest.get("symlinks")
        else:
            raise ValueError("snapshot manifest schema is invalid")
        if set(manifest) != fields:
            raise ValueError("snapshot manifest fields are invalid")
        files = manifest["files"]
        if (not all(isinstance(nodes, list) for nodes in (files, directories, symlinks))
                or not (len(files) + len(directories) + len(symlinks))
                or len(files) + len(directories) + len(symlinks) != candidate["fileCount"]):
            raise ValueError("snapshot manifest node count is invalid")
        previous = None
        expanded = 0
        paths, leaves = set(), set()
        for file in files:
            if not isinstance(file, dict) or set(file) != {"path", "sizeBytes", "sha256", "executable"}:
                raise ValueError("snapshot file fields are invalid")
            _node_path(file["path"])
            require_sha256("workspace snapshot file sha256", file["sha256"])
            if (type(file["sizeBytes"]) is not int or file["sizeBytes"] < 0
                    or type(file["executable"]) is not bool
                    or previous is not None and previous >= file["path"]):
                raise ValueError("snapshot file metadata is invalid")
            expanded += file["sizeBytes"]
            if expanded > candidate["expandedSizeBytes"]:
                raise ValueError("snapshot expanded size exceeds descriptor")
            previous = file["path"]
            paths.add(file["path"])
            leaves.add(file["path"])
        for nodes, links in ((directories, False), (symlinks, True)):
            previous = None
            for node in nodes:
                if links:
                    if not isinstance(node, dict) or set(node) != {"path", "target"}:
                        raise ValueError("snapshot symlink fields are invalid")
                    path, target = node["path"], node["target"]
                    if (not isinstance(target, str) or not target or "\0" in target
                            or len(target.encode("utf-8")) > MAX_PATH_BYTES):
                        raise ValueError("snapshot symlink target is invalid")
                else:
                    path = node
                _node_path(path)
                if path in paths or previous is not None and previous >= path:
                    raise ValueError("snapshot node paths are invalid")
                paths.add(path)
                if links:
                    leaves.add(path)
                previous = path
        for path in paths:
            components = path.split("/")
            if any("/".join(components[:depth]) in leaves for depth in range(1, len(components))):
                raise ValueError("snapshot node parent is not a directory")
        if expanded != candidate["expandedSizeBytes"]:
            raise ValueError("snapshot expanded size does not match descriptor")
        document = manifest
        if canonical_order or schema == "workspace.snapshot.v2":
            document = {"schema": manifest["schema"], "files": [
                {name: file[name] for name in ["path", "sizeBytes", "sha256", "executable"]}
                for file in files]}
            if schema == "workspace.snapshot.v2":
                document["directories"] = directories
                document["symlinks"] = [{"path": link["path"], "target": link["target"]} for link in symlinks]
        canonical = json.dumps(document, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if canonical != manifest_bytes:
            raise ValueError("snapshot manifest is not canonical")
    except (TypeError, ValueError) as error:
        raise SnapshotValidationError(str(error)) from error
    return manifest


def _node_path(path):
    validate_virtual_path(path)
    if len(path.encode("utf-8")) > MAX_PATH_BYTES or len(path.split("/")) > MAX_PATH_DEPTH:
        raise ValueError("snapshot node path is invalid")


class _StoredReader:
    def __init__(self, source, candidate, output, on_read):
        self.source = source
        self.candidate = candidate
        self.output = output
        self.on_read = on_read
        self.remaining = candidate["snapshotSizeBytes"]
        self.digest = hashlib.sha256()

    def read(self, size):
        requested = min(size, self.remaining, IO_BYTES)
        if requested == 0:
            return b""
        chunk = self._read(requested)
        if not chunk or len(chunk) > requested:
            raise SnapshotValidationError("snapshot stored size is invalid")
        self.remaining -= len(chunk)
        self.digest.update(chunk)
        if self.output is not None:
            remaining = memoryview(chunk)
            while remaining:
                count = self.output.write(remaining)
                if not isinstance(count, int) or not 0 < count <= len(remaining):
                    raise OSError("snapshot staging sink did not write bytes")
                remaining = remaining[count:]
        return chunk

    def _read(self, size):
        chunk = self.source.read(size)
        if self.on_read is not None:
            self.on_read(len(chunk))
        return chunk

    def require_complete(self):
        if self.remaining or self._read(1):
            raise SnapshotValidationError("snapshot stored size is invalid")
        if (self.candidate["snapshotSizeBytes"]
                and "sha256:" + self.digest.hexdigest() != self.candidate["snapshotSha256"]):
            raise SnapshotValidationError("snapshot stored digest is invalid")


class _ZstdReader:
    def __init__(self, source):
        self.source = source
        self.decoder = zstd.ZstdDecompressor(options={zstd.DecompressionParameter.window_log_max: MAX_WINDOW_LOG})

    def read(self, size):
        while not self.decoder.eof:
            chunk = b""
            if self.decoder.needs_input:
                chunk = self.source.read(IO_BYTES)
                if not chunk:
                    raise SnapshotValidationError("compressed snapshot is truncated")
            try:
                output = self.decoder.decompress(chunk, max_length=min(size, IO_BYTES))
            except zstd.ZstdError as error:
                raise SnapshotValidationError("compressed snapshot is invalid") from error
            if output:
                return output
        if self.decoder.unused_data or self.source.read(1):
            raise SnapshotValidationError("compressed snapshot has trailing bytes")
        return b""


def _read_exact(source, size):
    result = bytearray()
    while len(result) != size:
        chunk = source.read(min(size - len(result), IO_BYTES))
        if not chunk:
            raise SnapshotValidationError("snapshot content is truncated")
        result.extend(chunk)
    return bytes(result)


def _verify_file(source, file):
    remaining = file["sizeBytes"]
    digest = hashlib.sha256()
    while remaining:
        chunk = _read_exact(source, min(remaining, IO_BYTES))
        digest.update(chunk)
        remaining -= len(chunk)
    if "sha256:" + digest.hexdigest() != file["sha256"]:
        raise SnapshotValidationError("snapshot file integrity mismatch")


def _header(source):
    try:
        # Parse exactly one raw header. TarFile iteration would transparently
        # process extensions and can allocate their untrusted declared sizes.
        return tarfile.TarInfo.frombuf(_read_exact(source, 512), "utf-8", "surrogateescape")
    except tarfile.HeaderError as error:
        raise SnapshotValidationError("snapshot tar header is invalid") from error


def _regular(header, mode):
    if (header.type not in {tarfile.REGTYPE, tarfile.AREGTYPE} or header.mode != mode
            or header.uid != 0 or header.gid != 0 or header.mtime != 0 or header.linkname):
        raise SnapshotValidationError("snapshot tar entry metadata is invalid")


def _padding(source, size):
    _read_exact(source, -size % 512)


def _validate_archive(source, candidate):
    header = _header(source)
    _regular(header, 0o644)
    if header.name != MANIFEST_PATH or not 0 < header.size <= MAX_MANIFEST_BYTES:
        raise SnapshotValidationError("snapshot archive manifest is invalid")
    manifest = _manifest(_read_exact(source, header.size), candidate, canonical_order=True)
    _padding(source, header.size)
    for file in manifest["files"]:
        header = _header(source)
        path = header.name
        if header.type == tarfile.GNUTYPE_LONGNAME:
            if (path != "././@LongLink" or not 100 < header.size <= MAX_PATH_BYTES + len(CONTENT_PREFIX) + 1):
                raise SnapshotValidationError("snapshot long name is invalid")
            name = _read_exact(source, header.size)
            if not name.endswith(b"\0") or b"\0" in name[:-1]:
                raise SnapshotValidationError("snapshot long name is invalid")
            try:
                path = name[:-1].decode("utf-8")
            except UnicodeDecodeError as error:
                raise SnapshotValidationError("snapshot long name is invalid") from error
            _padding(source, header.size)
            header = _header(source)
        _regular(header, 0o755 if file["executable"] else 0o644)
        if path != CONTENT_PREFIX + file["path"] or header.size != file["sizeBytes"]:
            raise SnapshotValidationError("snapshot tar file does not match manifest")
        _verify_file(source, file)
        _padding(source, header.size)
    if _read_exact(source, 1024) != b"\0" * 1024 or source.read(1):
        raise SnapshotValidationError("snapshot archive has invalid end or trailing bytes")
    return manifest
