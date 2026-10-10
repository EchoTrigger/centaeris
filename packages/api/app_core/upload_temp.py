"""Exact request namespaces on a marked, disjoint temporary volume."""
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import uuid

from django.conf import settings

MARKER = ".centaeris-upload-temp.v1"
LEASE_NAME = re.compile(r"[0-9a-f]{32}\Z")
FILE_NAME = re.compile(r"(?:body|part)-[a-z0-9_]{8}\.tmp\Z")


def regular(path):
    metadata = path.lstat()
    return (stat.S_ISREG(metadata.st_mode) and metadata.st_nlink == 1
            and not getattr(metadata, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))


def checked_upload_temp_root():
    configured = settings.FILE_UPLOAD_TEMP_DIR
    if not configured or not os.path.isabs(configured):
        raise ValueError("upload_temp_root_invalid")
    root = Path(os.path.abspath(configured))
    media = os.path.normcase(os.path.abspath(settings.MEDIA_ROOT))
    try:
        common = os.path.commonpath([os.path.normcase(str(root)), media])
    except ValueError:
        common = ""
    if common in {os.path.normcase(str(root)), media}:
        raise ValueError("upload_temp_root_overlaps_storage")
    current = Path(root.anchor)
    for part in root.parts[1:]:
        current /= part
        if os.path.lexists(current):
            metadata = current.lstat()
            if (not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode)
                    or getattr(metadata, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)):
                raise ValueError("upload_temp_root_aliased")
    return root


def initialize_upload_temp_root():
    root = checked_upload_temp_root()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    marker = root / MARKER
    if not marker.exists():
        if list(root.iterdir()):
            raise ValueError("upload_temp_root_unmanaged")
        pending = root / (".upload-marker-" + uuid.uuid4().hex)
        try:
            with pending.open("xb") as handle:
                handle.write(json.dumps({"schema": "centaeris.upload_temp.v1", "poolRef": uuid.uuid4().hex}).encode())
                handle.flush()
                os.fsync(handle.fileno())
            try:
                os.link(pending, marker)
            except FileExistsError:
                pass
        finally:
            pending.unlink(missing_ok=True)
    if not regular(marker) or marker.stat().st_size > 512:
        raise ValueError("upload_temp_marker_invalid")
    body = json.loads(marker.read_text(encoding="utf-8"))
    if (not isinstance(body, dict) or set(body) != {"schema", "poolRef"} or body["schema"] != "centaeris.upload_temp.v1"
            or not isinstance(body["poolRef"], str) or LEASE_NAME.fullmatch(body["poolRef"]) is None):
        raise ValueError("upload_temp_marker_invalid")
    return root, body["poolRef"]


def namespace_files(root, pool_ref, lease_id):
    if LEASE_NAME.fullmatch(lease_id) is None:
        raise ValueError("upload_temp_lease_invalid")
    directory = root / lease_id
    if not os.path.lexists(directory):
        return []
    metadata = directory.lstat()
    if (not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode)
            or getattr(metadata, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)):
        raise ValueError("upload_temp_namespace_aliased")
    marker = directory / "request.json"
    if not marker.exists() or not regular(marker) or marker.stat().st_size > 512:
        raise ValueError("upload_temp_request_marker_invalid")
    body = json.loads(marker.read_text(encoding="utf-8"))
    if body != {"schema": "centaeris.upload_request.v1", "poolRef": pool_ref, "leaseId": lease_id}:
        raise ValueError("upload_temp_request_identity_mismatch")
    files = []
    for path in directory.iterdir():
        if path.name == "request.json":
            continue
        if FILE_NAME.fullmatch(path.name) is None or not regular(path):
            raise ValueError("upload_temp_unmanaged_object")
        files.append(path)
    return files


def remove_namespace(root, pool_ref, lease_id):
    if checked_upload_temp_root() != root or initialize_upload_temp_root()[1] != pool_ref:
        raise ValueError("upload_temp_pool_changed")
    directory = root / lease_id
    for path in namespace_files(root, pool_ref, lease_id):
        # Repeat exact ownership checks before each unlink.
        if path not in namespace_files(root, pool_ref, lease_id):
            raise ValueError("upload_temp_namespace_changed")
        path.unlink()
    if directory.exists():
        if namespace_files(root, pool_ref, lease_id):
            raise OSError("upload_temp_cleanup_unconfirmed")
        (directory / "request.json").unlink()
        directory.rmdir()
    if directory.exists():
        raise OSError("upload_temp_cleanup_unconfirmed")


class UploadTempTracker:
    def __init__(self, root, pool_ref, lease_id):
        self.root, self.pool_ref, self.lease_id = root, pool_ref, lease_id
        self.handles = []
        self.directory = root / lease_id
        self.directory.mkdir(mode=0o700)
        with (self.directory / "request.json").open("x", encoding="utf-8") as marker:
            json.dump({"schema": "centaeris.upload_request.v1", "poolRef": pool_ref, "leaseId": lease_id}, marker)
            marker.flush()
            os.fsync(marker.fileno())

    def open_file(self, *, body=False):
        handle = tempfile.NamedTemporaryFile(prefix="body-" if body else "part-",
            suffix=".tmp", mode="w+b", dir=self.directory, delete=False)
        self.handles.append(handle)
        return handle

    def cleanup(self):
        failed = False
        for handle in self.handles:
            try:
                handle.close()
                if not handle.closed:
                    failed = True
            except BaseException:
                failed = True
        if failed:
            return False
        try:
            remove_namespace(self.root, self.pool_ref, self.lease_id)
        except (OSError, ValueError):
            return False
        return True
