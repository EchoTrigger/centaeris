"""Released records remain platform shared; forward upgrade never promotes users."""

from django.db.migrations.executor import MigrationExecutor
from django.db.migrations.exceptions import IrreversibleError
from django.test import TransactionTestCase
from .migration_testing import isolated_migration_database


class WorkspaceResourceMigrationTests(TransactionTestCase):
    def test_forward_preserves_roles_owners_secret_bytes_and_shared_consumers(self):
        previous = [("app_core", "0031_merge_lifecycle_and_presentation")]
        target = [("app_core", "0032_workspace_resource_scope")]
        with isolated_migration_database(previous) as db:
            executor = MigrationExecutor(db)
            apps = executor.loader.project_state(previous).apps

            def objects(name, app="app_core"):
                return apps.get_model(app, name).objects.using(db.alias)

            owner = objects("User", "auth").create(
                username="owner", is_superuser=False, is_staff=False
            )
            admin = objects("User", "auth").create(
                username="admin", is_superuser=False, is_staff=False
            )
            workspaces = [
                objects("Workspace").create(name=name, createdBy_id=owner.pk)
                for name in ("A", "B")
            ]
            owner_ids = []
            for workspace in workspaces:
                owner_ids.append(
                    (
                        workspace.pk,
                        objects("WorkspaceMembership")
                        .create(
                            workspace_id=workspace.pk, user_id=owner.pk, role="owner"
                        )
                        .pk,
                    )
                )
                objects("WorkspaceMembership").create(
                    workspace_id=workspace.pk, user_id=admin.pk, role="admin"
                )
                objects("WorkspacePluginEnablement").create(
                    workspace_id=workspace.pk, pluginName="banana"
                )
            provider = objects("ModelProvider").create(
                displayName="Shared API",
                api="openai-completions",
                apiBase="https://example.invalid/v1",
            )
            credential = objects("ProviderCredential").create(
                provider_id=provider.pk,
                displayName="Shared",
                encryptedSecret="opaque-ciphertext",
                version=4,
                createdBy_id=owner.pk,
                updatedBy_id=owner.pk,
            )
            model = objects("ModelConfig").create(
                provider_id=provider.pk, modelName="retained"
            )
            custodian = objects("User", "auth").create(
                username="custodian", is_superuser=True, is_staff=True
            )
            mcp = objects("McpBearerCredential").create(
                plugin_name="banana",
                credential_ref="source-one",
                display_name="Shared MCP",
                encrypted_secret="opaque-mcp-ciphertext",
                version=3,
                created_by_id=custodian.pk,
                updated_by_id=custodian.pk,
            )
            definition = objects("AgentDefinition").create(
                workspace_id=workspaces[0].pk,
                name="Approved assistant",
                created_by_id=owner.pk,
            )
            from .plugin_catalog import activation_digest

            packages = [
                {
                    "name": "banana",
                    "version": "1.0.0",
                    "packageDigest": "sha256:" + "a" * 64,
                    "skills": [],
                    "cli": [],
                    "hooks": [],
                    "mcpServers": [
                        {"path": "mcp.json", "digest": "sha256:" + "b" * 64}
                    ],
                }
            ]
            snapshot = {
                "schema": "plugin_activation_snapshot_v1",
                "digest": activation_digest(packages),
                "packages": packages,
            }
            version = objects("AgentDefinitionVersion").create(
                definition_id=definition.pk,
                version=1,
                name="Approved assistant",
                plugin_activation=snapshot,
                published_by_id=owner.pk,
            )
            objects("AgentDefinition").filter(pk=definition.pk).update(
                published_version_id=version.pk
            )
            approval = objects("AgentConnectorCredentialApproval").create(
                workspace_id=workspaces[0].pk,
                definition_id=definition.pk,
                plugin_name="banana",
                server_id="ledger",
                resource_path="mcp.json",
                resource_digest="sha256:" + "b" * 64,
                credential_id=mcp.pk,
                credential_version=3,
                approved_by_id=custodian.pk,
            )
            binding = objects("AgentConnectorBinding").create(
                workspace_id=workspaces[0].pk,
                definition_id=definition.pk,
                plugin_name="banana",
                server_id="ledger",
                approval_id=approval.pk,
                updated_by_id=owner.pk,
            )
            before_approval = (
                objects("AgentConnectorCredentialApproval").values().get(pk=approval.pk)
            )
            before_binding = (
                objects("AgentConnectorBinding").values().get(pk=binding.pk)
            )
            executor.migrate(target)
            apps = executor.loader.project_state(target).apps
            self.assertIsNone(objects("ModelProvider").get(pk=provider.pk).workspace_id)
            self.assertIsNone(
                objects("McpBearerCredential").get(pk=mcp.pk).workspace_id
            )
            self.assertEqual(
                objects("ProviderCredential").get(pk=credential.pk).encryptedSecret,
                "opaque-ciphertext",
            )
            self.assertEqual(
                objects("McpBearerCredential").get(pk=mcp.pk).encrypted_secret,
                "opaque-mcp-ciphertext",
            )
            self.assertEqual(
                objects("ModelConfig").get(pk=model.pk).provider_id, provider.pk
            )
            self.assertEqual(objects("WorkspacePluginEnablement").count(), 2)
            for workspace_id, membership_id in owner_ids:
                actual = objects("WorkspaceMembership").get(
                    workspace_id=workspace_id, role="owner"
                )
                self.assertEqual((actual.pk, actual.user_id), (membership_id, owner.pk))
            self.assertEqual(
                objects("AgentConnectorCredentialApproval")
                .values()
                .get(pk=approval.pk),
                before_approval,
            )
            self.assertEqual(
                objects("AgentConnectorBinding").values().get(pk=binding.pk),
                before_binding,
            )
            self.assertEqual(
                objects("AgentDefinitionVersion").get(pk=version.pk).plugin_activation,
                snapshot,
            )
            self.assertEqual(
                objects("McpBearerCredential").get(pk=mcp.pk).credential_ref,
                "source-one",
            )
            self.assertEqual(objects("McpBearerCredential").get(pk=mcp.pk).version, 3)
            self.assertEqual(
                objects("WorkspaceMembership").filter(role="owner").count(), 2
            )
            self.assertEqual(
                objects("WorkspaceMembership").filter(role="admin").count(), 2
            )
            self.assertFalse(objects("User", "auth").get(pk=admin.pk).is_superuser)
            self.assertFalse(objects("User", "auth").get(pk=owner.pk).is_superuser)
            self.assertEqual(objects("WorkspacePluginInstallation").count(), 0)
            # Scope removal would turn local credentials into globally shared ones.
            # A forward-only deployment must refuse that destructive downgrade.
            with self.assertRaises(IrreversibleError):
                MigrationExecutor(db).migrate(previous)
