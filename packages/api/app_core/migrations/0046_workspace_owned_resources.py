"""Retire shared configuration and preserve existing workspace plugin bindings."""

from pathlib import Path

from django.conf import settings
from django.db import migrations
from django.utils import timezone


def retire_platform_configuration(apps, schema_editor):
    alias = schema_editor.connection.alias
    providers = apps.get_model("app_core", "ModelProvider").objects.using(alias)
    models = apps.get_model("app_core", "ModelConfig").objects.using(alias)
    credentials = apps.get_model("app_core", "ProviderCredential").objects.using(alias)
    mcp = apps.get_model("app_core", "McpBearerCredential").objects.using(alias)
    one_shot = apps.get_model("app_core", "OneShotCredential").objects.using(alias)
    installations = apps.get_model("app_core", "WorkspacePluginInstallation").objects.using(alias)
    enablements = apps.get_model("app_core", "WorkspacePluginEnablement").objects.using(alias)

    existing = set(installations.values_list("workspace_id", "plugin_name"))
    missing = [
        (workspace_id, name, creator_id)
        for workspace_id, name, creator_id in enablements.values_list(
            "workspace_id", "pluginName", "workspace__createdBy_id"
        )
        if (workspace_id, name) not in existing
    ]
    if missing:
        from app_core.plugin_catalog import load_plugin_catalog_at

        catalog = load_plugin_catalog_at(Path(settings.PLUGIN_CATALOG_ROOT))
        packages = {package["name"]: package for package in catalog["packages"]}
        installations.bulk_create(
            installations.model(
                workspace_id=workspace_id,
                plugin_name=name,
                package=packages[name],
                updated_by_id=creator_id,
            )
            for workspace_id, name, creator_id in missing
        )

    retired_providers = providers.filter(workspace_id__isnull=True)
    retired_models = models.filter(provider_id__in=retired_providers.values("pk"))
    now = timezone.now()
    one_shot.filter(
        modelConfig_id__in=retired_models.values("pk"), revokedAt__isnull=True
    ).update(revokedAt=now)
    retired_models.filter(isCurrent=True).update(enabled=False, isCurrent=False, updatedAt=now)
    retired_providers.filter(archivedAt__isnull=True).update(
        enabled=False, archivedAt=now, updatedAt=now
    )
    # Historical runs retain their provider/model identity. Credentials are separate.
    credentials.filter(provider_id__in=retired_providers.values("pk")).delete()
    mcp.filter(workspace_id__isnull=True).delete()


class Migration(migrations.Migration):
    dependencies = [("app_core", "0045_one_shot_cleanup")]
    operations = [migrations.RunPython(retire_platform_configuration)]
