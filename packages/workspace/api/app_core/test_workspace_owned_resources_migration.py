"""0045 history survives resource retirement on an isolated PostgreSQL database."""

import hashlib
import json
from datetime import timedelta
from pathlib import Path
from tempfile import TemporaryDirectory

from django.contrib.auth import get_user_model
from django.db import transaction
from django.db.migrations.exceptions import IrreversibleError
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase, override_settings
from django.utils import timezone

from .http.serialization import serialize_model
from .http.workspaces import _authorized_transcript_session
from .migration_authorization_fixtures import (
    historical_digest,
    historical_fixture,
    historical_signature,
)
from .migration_testing import isolated_migration_database
from .models import McpBearerCredential, ModelConfig, Workspace
from .plugin_catalog import activation_digest, plugin_package_root
from .workspace_plugins import workspace_plugin_catalog
from .workspace_resources import credential_for_workspace, models_for_workspace


def _write_package(root, version, *, immutable=False):
    name = "legacy-plugin"
    relative_path = ".centaeris-plugin/plugin.json"
    content = json.dumps({"name": name, "version": version, "paths": {}}).encode("utf-8")
    digest = hashlib.sha256(b"centaeris.plugin.tree.v1\0")
    for data in (relative_path.encode("utf-8"), content):
        digest.update(len(data).to_bytes(8, "big"))
        digest.update(data)
    package = {
        "name": name, "version": version, "packageDigest": f"sha256:{digest.hexdigest()}",
        "skills": [], "cli": [], "hooks": [], "mcpServers": [],
    }
    package_root = root / name
    if immutable:
        package_root = root / ".packages" / digest.hexdigest() / name
    manifest = package_root / relative_path
    manifest.parent.mkdir(parents=True)
    manifest.write_bytes(content)
    return package, package_root


class WorkspaceOwnedResourceUpgradeTests(TransactionTestCase):
    serialized_rollback = True
    previous = [("app_core", "0045_one_shot_cleanup")]
    target = [("app_core", "0046_workspace_owned_resources")]

    def test_historical_upgrade_preserves_bindings_and_history_without_shared_secrets(self):
        with TemporaryDirectory(prefix="workspace-resource-upgrade-") as temporary:
            root = Path(temporary).resolve()
            legacy, legacy_root = _write_package(root, "1.0.0")
            pinned, pinned_root = _write_package(root, "2.0.0", immutable=True)
            catalog = {
                "schema": "plugin_activation_snapshot_v1",
                "digest": activation_digest([legacy]), "packages": [legacy],
            }
            (root / "catalog.snapshot.json").write_text(json.dumps(catalog), encoding="utf-8")
            original_bytes = {
                path.relative_to(root): path.read_bytes() for path in root.rglob("*") if path.is_file()
            }
            with override_settings(PLUGIN_CATALOG_ROOT=root), isolated_migration_database(
                self.previous, route_current_models=True,
            ) as database:
                self.assertEqual(database.vendor, "postgresql")
                alias = database.alias
                apps = MigrationExecutor(database).loader.project_state(self.previous).apps

                def objects(name):
                    return apps.get_model("app_core", name).objects.using(alias)

                users = [apps.get_model("auth", "User").objects.using(alias).create(username=name)
                         for name in ("upgrade-owner-a", "upgrade-owner-b")]
                workspaces = [objects("Workspace").create(name=name, createdBy_id=user.pk)
                              for name, user in zip(("A", "B"), users)]
                memberships = [objects("WorkspaceMembership").create(
                    workspace_id=workspace.pk, user_id=user.pk, role="owner",
                ) for workspace, user in zip(workspaces, users)]

                providers, configs, provider_keys = [], [], []
                for scope in (None, *workspaces):
                    provider = objects("ModelProvider").create(
                        workspace_id=scope.pk if scope else None,
                        displayName=f"Provider {scope.name if scope else 'shared'}",
                        api="openai-completions", apiBase="https://example.invalid/v1",
                    )
                    providers.append(provider)
                    configs.append(objects("ModelConfig").create(
                        provider_id=provider.pk, modelName="historical-model",
                    ))
                    provider_keys.append(objects("ProviderCredential").create(
                        provider_id=provider.pk, displayName="Synthetic key",
                        encryptedSecret=f"opaque-{provider.pk}", version=4,
                        createdBy_id=users[0].pk, updatedBy_id=users[0].pk,
                    ))
                for scope in (None, workspaces[0]):
                    objects("McpBearerCredential").create(
                        workspace_id=scope.pk if scope else None,
                        plugin_name=legacy["name"], credential_ref="api-key", display_name="Key",
                        encrypted_secret=f"opaque-{scope.pk if scope else 'shared'}", version=3,
                        created_by_id=users[0].pk, updated_by_id=users[0].pk,
                    )
                one_shot = [objects("OneShotCredential").create(
                    workspace_id=workspaces[0].pk, user_id=users[0].pk,
                    membership_ref=memberships[0].pk, name=name,
                    modelConfig_id=config.pk, secretDigest="a" * 64,
                ) for name, config in (("Shared", configs[0]), ("Local", configs[1]))]
                revoked_at = timezone.now() - timedelta(days=1)
                already_revoked = objects("OneShotCredential").create(
                    workspace_id=workspaces[0].pk, user_id=users[0].pk,
                    membership_ref=memberships[0].pk, name="Already revoked",
                    modelConfig_id=configs[0].pk, secretDigest="b" * 64, revokedAt=revoked_at,
                )
                for workspace in workspaces:
                    objects("WorkspacePluginEnablement").create(
                        workspace_id=workspace.pk, pluginName=legacy["name"],
                    )
                installation = objects("WorkspacePluginInstallation").create(
                    workspace_id=workspaces[1].pk, plugin_name=pinned["name"],
                    package=pinned, updated_by_id=users[1].pk,
                )
                original_installation = objects("WorkspacePluginInstallation").values().get(pk=installation.pk)

                with transaction.atomic(using=alias):
                    session = objects("Session").create(
                        workspace_id=workspaces[0].pk, owner_id=users[0].pk, title="Retained history",
                    )
                    environment = objects("SessionEnvironment").create(session_id=session.pk)
                    objects("Session").filter(pk=session.pk).update(current_environment_id=environment.pk)
                run = objects("AgentRun").create(
                    workspace_id=workspaces[0].pk, user_id=users[0].pk,
                    membership_ref=memberships[0].pk, session_id=session.pk, environment_id=environment.pk,
                    modelConfig_id=configs[0].pk, prompt="Retained prompt", thinkingMode="high", status="completed",
                )
                payload = historical_fixture() | {
                    "id": "authorization-" + run.pk, "workspaceId": workspaces[0].pk,
                    "userId": str(users[0].pk), "sessionId": session.pk, "agentRunId": run.pk,
                    "modelConfigRef": configs[0].pk, "pluginActivation": catalog,
                }
                objects("AgentRunAuthorization").create(
                    id=payload["id"], agent_run_id=run.pk, payload=payload,
                    digest=historical_digest(payload), signature=historical_signature(payload, "test-key"),
                )
                objects("SessionEvent").create(
                    eventId=f"{run.pk}:answer", workspace_id=workspaces[0].pk, session_id=session.pk,
                    agent_run_id=run.pk, sequence=1, agent_run_sequence=1, projects_to_agent_run_stream=True,
                    payload={"type": "assistant_message", "payload": {"modelMarkdown": "Retained answer"}},
                    createdAtMs=1,
                )
                preserved = {name: list(objects(name).order_by("pk").values()) for name in (
                    "WorkspaceMembership", "WorkspacePluginEnablement", "Session", "SessionEnvironment",
                    "AgentRun", "AgentRunAuthorization", "SessionEvent",
                )}
                local_facts = {
                    "ModelProvider": list(objects("ModelProvider").filter(workspace_id__isnull=False).order_by("pk").values()),
                    "ModelConfig": list(objects("ModelConfig").filter(provider__workspace_id__isnull=False).order_by("pk").values()),
                    "ProviderCredential": list(objects("ProviderCredential").filter(provider__workspace_id__isnull=False).order_by("pk").values()),
                    "McpBearerCredential": list(objects("McpBearerCredential").filter(workspace_id__isnull=False).order_by("pk").values()),
                }

                MigrationExecutor(database).migrate(self.target)
                apps = MigrationExecutor(database).loader.project_state(self.target).apps
                for name, expected in preserved.items():
                    self.assertEqual(list(objects(name).order_by("pk").values()), expected, name)
                for name, expected in local_facts.items():
                    self.assertEqual(list(objects(name).filter(pk__in=[row["id"] for row in expected])
                                          .order_by("pk").values()), expected, name)
                self.assertEqual(objects("WorkspacePluginInstallation").values().get(pk=installation.pk), original_installation)
                created = objects("WorkspacePluginInstallation").get(workspace_id=workspaces[0].pk)
                self.assertEqual((created.plugin_name, created.package, created.updated_by_id),
                                 (legacy["name"], legacy, users[0].pk))

                shared_provider = objects("ModelProvider").get(pk=providers[0].pk)
                shared_model = objects("ModelConfig").get(pk=configs[0].pk)
                self.assertFalse(shared_provider.enabled)
                self.assertIsNotNone(shared_provider.archivedAt)
                self.assertFalse(shared_model.enabled)
                self.assertFalse(shared_model.isCurrent)
                self.assertFalse(objects("ProviderCredential").filter(pk=provider_keys[0].pk).exists())
                self.assertFalse(objects("McpBearerCredential").filter(workspace_id__isnull=True).exists())
                self.assertIsNotNone(objects("OneShotCredential").get(pk=one_shot[0].pk).revokedAt)
                self.assertIsNone(objects("OneShotCredential").get(pk=one_shot[1].pk).revokedAt)
                self.assertEqual(objects("OneShotCredential").get(pk=already_revoked.pk).revokedAt, revoked_at)

                for workspace, config, package, path in zip(workspaces, configs[1:], (legacy, pinned), (legacy_root, pinned_root)):
                    current_workspace = Workspace.objects.get(pk=workspace.pk)
                    self.assertEqual(workspace_plugin_catalog(current_workspace)["packages"], [package])
                    self.assertEqual(plugin_package_root(package), path)
                    self.assertEqual(list(models_for_workspace(workspace.pk).exclude(provider__isnull=True)
                                          .values_list("pk", flat=True)), [config.pk])
                self.assertEqual(credential_for_workspace(workspaces[0].pk, legacy["name"], "api-key").workspace_id,
                                 workspaces[0].pk)
                with self.assertRaises(McpBearerCredential.DoesNotExist):
                    credential_for_workspace(workspaces[1].pk, legacy["name"], "api-key")
                current_users = [get_user_model().objects.get(pk=user.pk) for user in users]
                self.assertEqual(_authorized_transcript_session(current_users[0], session.pk).pk, session.pk)
                self.assertIsNone(_authorized_transcript_session(current_users[1], session.pk))
                self.assertEqual(serialize_model(ModelConfig.objects.get(pk=configs[0].pk))["providerId"], providers[0].pk)
                self.assertEqual({path.relative_to(root): path.read_bytes() for path in root.rglob("*") if path.is_file()},
                                 original_bytes)

                applied = set(MigrationExecutor(database).loader.applied_migrations)
                with self.assertRaises(IrreversibleError):
                    MigrationExecutor(database).migrate(self.previous)
                self.assertEqual(set(MigrationExecutor(database).loader.applied_migrations), applied)
                self.assertFalse(objects("ProviderCredential").filter(pk=provider_keys[0].pk).exists())
