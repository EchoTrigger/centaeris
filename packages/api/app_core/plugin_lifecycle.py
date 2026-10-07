import json
import os
import shutil
import uuid
from pathlib import Path
from typing import Callable

from django.conf import settings

from .plugin_catalog import (
    PLUGIN_ACTIVATION_SCHEMA,
    activation_digest,
    load_plugin_catalog,
    plugin_lifecycle_lock,
    validate_plugin_activation,
)
from .plugin_install_source import PluginInstallSource, discard_staging
from .runtime_client import request_plugin_inspection


class PluginLifecycleError(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


def initialize_plugin_catalog() -> None:
    with plugin_lifecycle_lock():
        _load_or_initialize_installed_catalog()


def install_plugin_from_source(
    source: PluginInstallSource,
    *,
    ensure_update_allowed: Callable[[str], None] | None = None,
) -> dict:
    root = Path(settings.PLUGIN_CATALOG_ROOT)
    with plugin_lifecycle_lock():
        _load_or_initialize_installed_catalog()
    staging = root / f".upload-{uuid.uuid4().hex}"
    try:
        package_root = source.normalize_into(staging)
        package_path = package_root.relative_to(root).as_posix()
        try:
            package = request_plugin_inspection(package_path)
        except ValueError as error:
            raise PluginLifecycleError("plugin_package_invalid") from error
        _snapshot([package])
        with plugin_lifecycle_lock():
            installed = load_plugin_catalog()
            current = _package(installed, package["name"])
            if current is not None and current["packageDigest"] == package["packageDigest"]:
                raise PluginLifecycleError("plugin_already_installed")
            if current is None:
                try:
                    catalog = _snapshot([*installed["packages"], package])
                except ValueError as error:
                    raise PluginLifecycleError("plugin_package_invalid") from error
                _install_candidate(package_root, package, catalog)
            else:
                if ensure_update_allowed is not None:
                    ensure_update_allowed(package["name"])
                packages = [
                    package if item["name"] == package["name"] else item
                    for item in installed["packages"]
                ]
                try:
                    catalog = _snapshot(packages)
                except ValueError as error:
                    raise PluginLifecycleError("plugin_package_invalid") from error
                _replace_candidate(package_root, package, catalog)
        return package
    finally:
        discard_staging(staging)


def remove_plugin(plugin_name: str) -> None:
    with plugin_lifecycle_lock():
        installed = load_plugin_catalog()
        if _package(installed, plugin_name) is None:
            raise PluginLifecycleError("plugin_not_installed")
        _remove_package(plugin_name, installed)


def _load_or_initialize_installed_catalog() -> dict:
    root = Path(settings.PLUGIN_CATALOG_ROOT)
    root.mkdir(parents=True, exist_ok=True)
    if (root / "catalog.snapshot.json").exists():
        return load_plugin_catalog()
    if any(root.iterdir()):
        raise ValueError("plugin_catalog_missing_for_nonempty_root")
    catalog = _snapshot([])
    _write_catalog(catalog)
    return catalog


def _install_candidate(candidate: Path, package: dict, catalog: dict) -> None:
    root = Path(settings.PLUGIN_CATALOG_ROOT)
    target = root / package["name"]
    if target.exists():
        raise ValueError(f"plugin_installation_path_conflict:{package['name']}")
    os.replace(candidate, target)
    try:
        _write_catalog(catalog)
    except Exception:
        os.replace(target, candidate)
        raise


def _replace_candidate(candidate: Path, package: dict, catalog: dict) -> None:
    root = Path(settings.PLUGIN_CATALOG_ROOT)
    target = root / package["name"]
    backup = root / f".backup-{package['name']}-{uuid.uuid4().hex}"
    if not target.is_dir():
        raise ValueError(f"plugin_package_missing:{package['name']}")
    os.replace(target, backup)
    try:
        try:
            os.replace(candidate, target)
        except Exception:
            os.replace(backup, target)
            raise
        try:
            _write_catalog(catalog)
        except Exception:
            os.replace(target, candidate)
            os.replace(backup, target)
            raise
        shutil.rmtree(backup)
    finally:
        shutil.rmtree(backup, ignore_errors=True)


def _remove_package(plugin_name: str, installed: dict) -> None:
    root = Path(settings.PLUGIN_CATALOG_ROOT)
    target = root / plugin_name
    removed = root / f".remove-{plugin_name}-{uuid.uuid4().hex}"
    if not target.is_dir():
        raise ValueError(f"plugin_package_missing:{plugin_name}")
    os.replace(target, removed)
    try:
        _write_catalog(
            _snapshot(
                [
                    package
                    for package in installed["packages"]
                    if package["name"] != plugin_name
                ]
            )
        )
    except Exception:
        os.replace(removed, target)
        raise
    shutil.rmtree(removed)


def _write_catalog(catalog: dict) -> None:
    root = Path(settings.PLUGIN_CATALOG_ROOT)
    temporary = root / f".catalog-{uuid.uuid4().hex}.tmp"
    encoded = (
        json.dumps(catalog, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    ).encode("utf-8")
    try:
        with temporary.open("xb") as handle:
            handle.write(encoded)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, root / "catalog.snapshot.json")
    finally:
        temporary.unlink(missing_ok=True)


def _snapshot(packages: list[dict]) -> dict:
    packages = sorted(
        packages,
        key=lambda package: (package["name"], package["packageDigest"]),
    )
    catalog = {
        "schema": PLUGIN_ACTIVATION_SCHEMA,
        "digest": activation_digest(packages),
        "packages": packages,
    }
    validate_plugin_activation(catalog)
    return catalog


def _package(catalog: dict, plugin_name: str) -> dict | None:
    return next(
        (
            package
            for package in catalog["packages"]
            if package["name"] == plugin_name
        ),
        None,
    )
