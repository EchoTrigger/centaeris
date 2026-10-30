"""Workspace installations pin immutable packages alongside the shared catalog."""

import os
import uuid
from pathlib import Path
from django.conf import settings
from django.db import transaction
from .models import (
    WorkspacePluginInstallation,
    WorkspacePluginEnablement,
    AgentRunAuthorization,
)
from .plugin_catalog import (
    load_plugin_catalog,
    plugin_lifecycle_lock,
    activation_digest,
    validate_plugin_activation,
)
from .plugin_install_source import discard_staging
from .plugin_lifecycle import PluginLifecycleError
from .runtime_client import request_plugin_inspection
from .workspace_resources import workspace_resource_admin


def workspace_plugin_catalog(workspace, *, require_packages=True):
    packages = {
        p["name"]: p
        for p in load_plugin_catalog(require_packages=require_packages)["packages"]
    }
    for install in WorkspacePluginInstallation.objects.filter(workspace=workspace):
        packages[install.plugin_name] = install.package
    selected = [packages[name] for name in sorted(packages)]
    snapshot = {
        "schema": "plugin_activation_snapshot_v1",
        "digest": activation_digest(selected),
        "packages": selected,
    }
    validate_plugin_activation(snapshot)
    return snapshot


def _active_package(workspace_id, name):
    return any(
        any(
            package.get("name") == name
            for package in payload.get("pluginActivation", {}).get("packages", [])
        )
        for payload in AgentRunAuthorization.objects.filter(
            agent_run__workspace_id=workspace_id,
            agent_run__status__in=["queued", "running"],
        ).values_list("payload", flat=True)
    )


def install_workspace_plugin(user, workspace_id, source):
    # Hold current membership through both inspection and committing the binding.
    with transaction.atomic():
        workspace = workspace_resource_admin(user, workspace_id)
        root = Path(settings.PLUGIN_CATALOG_ROOT).resolve(strict=True)
        staging = root / f".upload-{uuid.uuid4().hex}"
        try:
            package_root = source.normalize_into(staging)
            package = request_plugin_inspection(
                package_root.relative_to(root).as_posix()
            )
            validate_plugin_activation(
                {
                    "schema": "plugin_activation_snapshot_v1",
                    "digest": activation_digest([package]),
                    "packages": [package],
                }
            )
            with plugin_lifecycle_lock():
                current = (
                    WorkspacePluginInstallation.objects.select_for_update()
                    .filter(workspace=workspace, plugin_name=package["name"])
                    .first()
                )
                if current and current.package == package:
                    raise PluginLifecycleError("plugin_already_installed")
                if current and _active_package(workspace_id, package["name"]):
                    raise PluginLifecycleError("plugin_in_active_agent_runs")
                digest = package["packageDigest"].removeprefix("sha256:")
                target = root / ".packages" / digest / package["name"]
                # Canonical metadata is untrusted, and symlink parents must never
                # redirect a tenant upload into a global package or external path.
                for path in (root / ".packages", root / ".packages" / digest, target):
                    if path.is_symlink():
                        raise ValueError("plugin_storage_symlink")
                target.parent.mkdir(parents=True, exist_ok=True)
                if not target.exists():
                    os.replace(package_root, target)
                WorkspacePluginInstallation.objects.update_or_create(
                    workspace=workspace,
                    plugin_name=package["name"],
                    defaults={"package": package, "updated_by": user},
                )
                WorkspacePluginEnablement.objects.get_or_create(
                    workspace=workspace, pluginName=package["name"]
                )
            return package
        finally:
            discard_staging(staging)


def remove_workspace_plugin(user, workspace_id, name):
    with transaction.atomic():
        workspace = workspace_resource_admin(user, workspace_id)
        with plugin_lifecycle_lock():
            install = (
                WorkspacePluginInstallation.objects.select_for_update()
                .filter(workspace=workspace, plugin_name=name)
                .first()
            )
            if install is None:
                raise PluginLifecycleError("plugin_not_installed")
            if _active_package(workspace_id, name):
                raise PluginLifecycleError("plugin_in_active_agent_runs")
            WorkspacePluginEnablement.objects.filter(
                workspace=workspace, pluginName=name
            ).delete()
            install.delete()
            # Content-addressed bytes remain immutable for existing signed Run,
            # environment and published-definition references. No global removal.
