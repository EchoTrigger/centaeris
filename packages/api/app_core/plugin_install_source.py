import os
import shutil
import stat
import unicodedata
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Protocol


MAX_PLUGIN_ARCHIVE_BYTES = 64 * 1024 * 1024
MAX_PLUGIN_ARCHIVE_ENTRIES = 4096
MAX_PLUGIN_ARCHIVE_FILE_BYTES = 32 * 1024 * 1024
MAX_PLUGIN_ARCHIVE_EXPANDED_BYTES = 256 * 1024 * 1024
MAX_PLUGIN_ARCHIVE_PATH_BYTES = 1024
COPY_CHUNK_BYTES = 64 * 1024
PLUGIN_MANIFEST_PATH = PurePosixPath(".centaeris-plugin/plugin.json")


class PluginInstallSourceError(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


class PluginInstallSource(Protocol):
    """Normalize an external carrier into one local Plugin package directory."""

    def normalize_into(self, staging_root: Path) -> Path: ...


@dataclass(frozen=True)
class UploadedZip:
    upload: object

    def normalize_into(self, staging_root: Path) -> Path:
        archive = staging_root / "upload.zip"
        expanded = staging_root / "expanded"
        package = staging_root / "package"
        staging_root.mkdir(parents=True, exist_ok=False)
        try:
            _copy_upload(self.upload, archive)
            expanded.mkdir()
            _extract_zip(archive, expanded)
            package_root = _plugin_package_root(expanded)
            os.replace(package_root, package)
            archive.unlink(missing_ok=True)
            return package
        except PluginInstallSourceError:
            raise
        except (OSError, zipfile.BadZipFile, zipfile.LargeZipFile) as error:
            raise PluginInstallSourceError("plugin_archive_invalid") from error


def _copy_upload(upload, destination: Path) -> None:
    declared_size = getattr(upload, "size", None)
    if type(declared_size) is int and declared_size > MAX_PLUGIN_ARCHIVE_BYTES:
        raise PluginInstallSourceError("plugin_archive_too_large")
    total = 0
    with destination.open("xb") as output:
        chunks = (
            upload.chunks(COPY_CHUNK_BYTES)
            if hasattr(upload, "chunks")
            else iter(lambda: upload.read(COPY_CHUNK_BYTES), b"")
        )
        for chunk in chunks:
            total += len(chunk)
            if total > MAX_PLUGIN_ARCHIVE_BYTES:
                raise PluginInstallSourceError("plugin_archive_too_large")
            output.write(chunk)
    if total == 0:
        raise PluginInstallSourceError("plugin_archive_invalid")


def _extract_zip(archive_path: Path, destination: Path) -> None:
    try:
        archive = zipfile.ZipFile(archive_path, "r", allowZip64=True)
    except (OSError, zipfile.BadZipFile, zipfile.LargeZipFile) as error:
        raise PluginInstallSourceError("plugin_archive_invalid") from error
    with archive:
        entries = archive.infolist()
        if not entries:
            raise PluginInstallSourceError("plugin_archive_invalid")
        if len(entries) > MAX_PLUGIN_ARCHIVE_ENTRIES:
            raise PluginInstallSourceError("plugin_archive_too_large")
        plans = _validated_entries(entries)
        expanded_bytes = 0
        for info, relative, is_directory in plans:
            target = destination.joinpath(*relative.parts)
            if is_directory:
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            written = 0
            try:
                source = archive.open(info, "r")
            except (RuntimeError, zipfile.BadZipFile) as error:
                raise PluginInstallSourceError("plugin_archive_invalid") from error
            with source, target.open("xb") as output:
                while True:
                    chunk = source.read(COPY_CHUNK_BYTES)
                    if not chunk:
                        break
                    written += len(chunk)
                    expanded_bytes += len(chunk)
                    if (
                        written > MAX_PLUGIN_ARCHIVE_FILE_BYTES
                        or expanded_bytes > MAX_PLUGIN_ARCHIVE_EXPANDED_BYTES
                    ):
                        raise PluginInstallSourceError("plugin_archive_too_large")
                    output.write(chunk)
            if written != info.file_size:
                raise PluginInstallSourceError("plugin_archive_invalid")
            mode = (info.external_attr >> 16) & 0o777
            if mode:
                os.chmod(target, mode)


def _validated_entries(
    entries: list[zipfile.ZipInfo],
) -> list[tuple[zipfile.ZipInfo, PurePosixPath, bool]]:
    plans = []
    explicit_paths = set()
    casefold_paths: dict[str, str] = {}
    declared_total = 0
    kinds: dict[str, str] = {}
    for info in entries:
        relative, is_directory = _validated_entry_path(info)
        canonical = relative.as_posix()
        if canonical in explicit_paths:
            raise PluginInstallSourceError("plugin_archive_invalid")
        explicit_paths.add(canonical)
        for index in range(1, len(relative.parts) + 1):
            prefix = PurePosixPath(*relative.parts[:index]).as_posix()
            folded = prefix.casefold()
            previous = casefold_paths.setdefault(folded, prefix)
            if previous != prefix:
                raise PluginInstallSourceError("plugin_archive_invalid")
            if index < len(relative.parts) and kinds.get(prefix) == "file":
                raise PluginInstallSourceError("plugin_archive_invalid")
        existing_kind = kinds.get(canonical)
        kind = "directory" if is_directory else "file"
        if existing_kind is not None and existing_kind != kind:
            raise PluginInstallSourceError("plugin_archive_invalid")
        if kind == "file" and any(
            path.startswith(f"{canonical}/") for path in kinds
        ):
            raise PluginInstallSourceError("plugin_archive_invalid")
        kinds[canonical] = kind
        if info.flag_bits & 0x1:
            raise PluginInstallSourceError("plugin_archive_invalid")
        if not is_directory:
            if info.file_size < 0 or info.file_size > MAX_PLUGIN_ARCHIVE_FILE_BYTES:
                raise PluginInstallSourceError("plugin_archive_too_large")
            declared_total += info.file_size
            if declared_total > MAX_PLUGIN_ARCHIVE_EXPANDED_BYTES:
                raise PluginInstallSourceError("plugin_archive_too_large")
        plans.append((info, relative, is_directory))
    return plans


def _validated_entry_path(info: zipfile.ZipInfo) -> tuple[PurePosixPath, bool]:
    original = info.orig_filename
    if not isinstance(original, str) or not original or "\x00" in original:
        raise PluginInstallSourceError("plugin_archive_invalid")
    if unicodedata.normalize("NFC", original) != original:
        raise PluginInstallSourceError("plugin_archive_invalid")
    if "\\" in original or original.startswith("/") or ":" in original:
        raise PluginInstallSourceError("plugin_archive_invalid")
    if len(original.encode("utf-8")) > MAX_PLUGIN_ARCHIVE_PATH_BYTES:
        raise PluginInstallSourceError("plugin_archive_invalid")
    trimmed = original[:-1] if original.endswith("/") else original
    if not trimmed:
        raise PluginInstallSourceError("plugin_archive_invalid")
    relative = PurePosixPath(trimmed)
    if any(
        part in {"", ".", ".."}
        or any(unicodedata.category(character).startswith("C") for character in part)
        for part in relative.parts
    ):
        raise PluginInstallSourceError("plugin_archive_invalid")
    mode = (info.external_attr >> 16) & 0xFFFF
    entry_type = stat.S_IFMT(mode)
    is_directory = info.is_dir()
    allowed_type = stat.S_IFDIR if is_directory else stat.S_IFREG
    if entry_type not in {0, allowed_type}:
        # This rejects Unix symlinks, devices and other special entries. ZIP has
        # no portable hard-link entry; Unix encodings use a non-regular type.
        raise PluginInstallSourceError("plugin_archive_invalid")
    return relative, is_directory


def _plugin_package_root(expanded: Path) -> Path:
    if (expanded / PLUGIN_MANIFEST_PATH).is_file():
        return expanded
    children = list(expanded.iterdir())
    if (
        len(children) == 1
        and children[0].is_dir()
        and (children[0] / PLUGIN_MANIFEST_PATH).is_file()
    ):
        return children[0]
    raise PluginInstallSourceError("plugin_package_layout_invalid")


def discard_staging(staging_root: Path) -> None:
    shutil.rmtree(staging_root, ignore_errors=True)
