import logging

from django.db.models import Count
from ninja import Router, Status

from app_core.models import (
    AgentRunAuthorization,
    McpBearerCredential,
    WorkspacePluginEnablement,
)
from app_core.plugin_catalog import (
    activation_digest,
    load_plugin_catalog,
    load_plugin_interfaces,
    plugin_lifecycle_lock,
)
from app_core.plugin_install_source import (
    PluginInstallSourceError,
    UploadedZip,
)
from app_core.plugin_lifecycle import (
    PluginLifecycleError,
    install_plugin_from_source,
    remove_plugin,
)

from .response_schema import (
    COMMON_ERROR_RESPONSES,
    GlobalPluginEnvelope,
    GlobalPluginsEnvelope,
)
from .security import superuser_auth


logger = logging.getLogger(__name__)
router = Router(tags=["plugin-lifecycle"], by_alias=True)


@router.get(
    "/admin/plugins",
    auth=superuser_auth,
    response={200: GlobalPluginsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_global_plugins(request):
    if _has_unexpected_input(request):
        return Status(400, {"error": "plugin_lifecycle_request_invalid"})
    try:
        return {"plugins": _serialize_global_plugins()}
    except (OSError, ValueError):
        logger.exception("Global Plugin catalog is unavailable")
        return Status(503, {"error": "plugin_lifecycle_unavailable"})


@router.post(
    "/admin/plugins/upload",
    auth=superuser_auth,
    response={200: GlobalPluginEnvelope} | COMMON_ERROR_RESPONSES,
)
def upload_global_plugin(request):
    if (
        request.GET
        or request.POST
        or set(request.FILES) != {"file"}
        or len(request.FILES.getlist("file")) != 1
    ):
        return Status(400, {"error": "plugin_lifecycle_request_invalid"})
    upload = request.FILES["file"]
    try:
        package = install_plugin_from_source(
            UploadedZip(upload),
            ensure_update_allowed=_ensure_update_allowed,
        )
        return {"plugin": _global_plugin(package["name"])}
    except PluginInstallSourceError as error:
        status = 413 if error.code == "plugin_archive_too_large" else 400
        return Status(status, {"error": error.code})
    except PluginLifecycleError as error:
        return _lifecycle_error(error)
    except (OSError, RuntimeError, ValueError):
        logger.exception("Upload global Plugin failed")
        return Status(503, {"error": "plugin_lifecycle_unavailable"})


@router.delete(
    "/admin/plugins/{plugin_name}",
    auth=superuser_auth,
    response={204: None} | COMMON_ERROR_RESPONSES,
)
def remove_global_plugin(request, plugin_name: str):
    if _has_unexpected_input(request):
        return Status(400, {"error": "plugin_lifecycle_request_invalid"})
    try:
        with plugin_lifecycle_lock():
            if WorkspacePluginEnablement.objects.filter(
                pluginName=plugin_name
            ).exists():
                return Status(409, {"error": "plugin_enabled_in_workspaces"})
            if McpBearerCredential.objects.filter(plugin_name=plugin_name).exists():
                return Status(409, {"error": "plugin_credentials_configured"})
            if _plugin_has_active_agent_runs(plugin_name):
                return Status(409, {"error": "plugin_in_active_agent_runs"})
            remove_plugin(plugin_name)
        return 204, None
    except PluginLifecycleError as error:
        return _lifecycle_error(error)
    except (OSError, ValueError):
        logger.exception("Remove global Plugin failed")
        return Status(503, {"error": "plugin_lifecycle_unavailable"})


def _lifecycle_error(error: PluginLifecycleError):
    if error.code == "plugin_package_invalid":
        status = 400
    elif error.code in {
        "plugin_already_installed",
        "plugin_in_active_agent_runs",
        "plugin_not_installed",
    }:
        status = 409
    else:
        status = 400
    return Status(status, {"error": error.code})


def _has_unexpected_input(request) -> bool:
    return bool(request.GET or request.body)


def _ensure_update_allowed(plugin_name: str) -> None:
    if _plugin_has_active_agent_runs(plugin_name):
        raise PluginLifecycleError("plugin_in_active_agent_runs")


def _plugin_has_active_agent_runs(plugin_name: str) -> bool:
    payloads = AgentRunAuthorization.objects.filter(
        agent_run__status__in=("queued", "running")
    ).values_list("payload", flat=True)
    return any(
        any(
            package["name"] == plugin_name
            for package in payload["pluginActivation"]["packages"]
        )
        for payload in payloads.iterator()
    )


def _global_plugin(plugin_name: str) -> dict:
    plugin = next(
        (
            item
            for item in _serialize_global_plugins()
            if item["name"] == plugin_name
        ),
        None,
    )
    if plugin is None:
        raise PluginLifecycleError("plugin_not_installed")
    return plugin


def _serialize_global_plugins() -> list[dict]:
    with plugin_lifecycle_lock():
        catalog = load_plugin_catalog()
        packages = {package["name"]: package for package in catalog["packages"]}
        states = [
            {"name": package["name"], "version": package["version"]}
            for package in catalog["packages"]
        ]
        enabled_counts = {
            item["pluginName"]: item["count"]
            for item in WorkspacePluginEnablement.objects.values("pluginName")
            .annotate(count=Count("workspace_id"))
            .order_by()
        }
        credential_counts = {
            item["plugin_name"]: item["count"]
            for item in McpBearerCredential.objects.values("plugin_name")
            .annotate(count=Count("id"))
            .order_by()
        }
        active_names = _active_plugin_names()
        serialized = []
        for state in states:
            name = state["name"]
            package = packages[name]
            errors = []
            try:
                singleton = {
                    "schema": "plugin_activation_snapshot_v1",
                    "digest": activation_digest([package]),
                    "packages": [package],
                }
                interface = load_plugin_interfaces(singleton)[name]
            except (OSError, ValueError):
                logger.exception("Installed Plugin manifest is unavailable: %s", name)
                interface = {
                    "displayName": name,
                    "shortDescription": "",
                    "capabilities": [],
                }
                errors.append("plugin_manifest_invalid")
            enabled_count = enabled_counts.get(name, 0)
            credential_count = credential_counts.get(name, 0)
            serialized.append(
                {
                    **state,
                    **interface,
                    "enabledWorkspaceCount": enabled_count,
                    "credentialCount": credential_count,
                    "removable": (
                        enabled_count == 0
                        and credential_count == 0
                        and name not in active_names
                    ),
                    "errors": errors,
                }
            )
        return serialized


def _active_plugin_names() -> set[str]:
    payloads = AgentRunAuthorization.objects.filter(
        agent_run__status__in=("queued", "running")
    ).values_list("payload", flat=True)
    return {
        package["name"]
        for payload in payloads.iterator()
        for package in payload["pluginActivation"]["packages"]
    }
