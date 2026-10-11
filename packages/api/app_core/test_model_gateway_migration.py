"""Upgrade populated provider credentials without changing host-capacity data."""
from django.db.migrations.executor import MigrationExecutor
from django.test import TransactionTestCase

from .credentials import encrypt_credential_secret
from .migration_testing import isolated_migration_database


class ModelGatewayMigrationTests(TransactionTestCase):
    serialized_rollback = True

    def test_forward_migration_preserves_credentials_models_and_upload_occupancy(self):
        previous = [("app_core", "0012_work_return_audit")]
        target = [("app_core", "0013_remove_model_quota_domain")]
        with isolated_migration_database(previous) as database:
            executor = MigrationExecutor(database)
            apps = executor.loader.project_state(previous).apps
            def records(name, app="app_core"):
                return apps.get_model(app, name).objects.using(database.alias)
            user = records("User", "auth").create(username="gateway-upgrade")
            domain = records("ModelQuotaDomain").create(
                id="old-account", maxConcurrent=2, enabled=True, cooldownUntilMs=12345)
            provider = records("ModelProvider").create(
                displayName="Gateway", api="openai-completions", apiBase="https://models.example.com/v1")
            secret = encrypt_credential_secret("synthetic-migration-secret")
            credential = records("ProviderCredential").create(
                provider_id=provider.pk, quotaDomain_id=domain.pk, displayName="Retained credential",
                encryptedSecret=secret, version=3, createdBy_id=user.pk, updatedBy_id=user.pk)
            model = records("ModelConfig").create(provider_id=provider.pk, modelName="retained-model")
            records("UploadCapacity").update_or_create(pk=1, defaults={"reservedBytes": 123, "activeUploads": 1})
            executor.migrate(target)
            apps = executor.loader.project_state(target).apps
            retained = records("ProviderCredential").get(pk=credential.pk)
            self.assertEqual((retained.encryptedSecret, retained.version, retained.provider_id,
                              retained.createdBy_id, retained.updatedBy_id),
                             (secret, 3, provider.pk, user.pk, user.pk))
            self.assertEqual(records("ModelConfig").get(pk=model.pk).provider_id, provider.pk)
            capacity = records("UploadCapacity").get(pk=1)
            self.assertEqual((capacity.reservedBytes, capacity.activeUploads), (123, 1))
            # Reverse restores an empty legacy policy schema, never an invented account binding.
            MigrationExecutor(database).migrate(previous)
            apps = MigrationExecutor(database).loader.project_state(previous).apps
            self.assertIsNone(records("ProviderCredential").get(pk=credential.pk).quotaDomain_id)
            self.assertEqual(records("ModelQuotaDomain").count(), 0)
