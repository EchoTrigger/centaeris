"""Forward target migration preserves native history and guards provider responsibility."""
import importlib
import json
import uuid
from pathlib import Path

from django.contrib.auth.models import User
from django.db import DatabaseError, connection, transaction
from django.db.migrations.loader import MigrationLoader
from django.test import TransactionTestCase

from .environment_maintenance import _digest
from .environment_maintenance_authorization import sign_maintenance_authorization
from .models import EnvironmentMaintenanceOperation, Workspace
from .testing import create_session


class ProviderMaintenanceTargetTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        self.user = User.objects.create_user(username='provider-target-owner')
        self.workspace = Workspace.objects.create(name='Provider target', createdBy=self.user)
        self.session = create_session(workspace=self.workspace, owner=self.user)
        self.environment = self.session.current_environment
        fixture = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json'
        self.target = json.loads(fixture.read_text(encoding='utf-8'))['payload']['providerTarget']
        self.request = {'program': '/bin/true', 'args': [], 'cwd': '/mnt/data', 'timeoutMs': 1000}

    def fields(self):
        return {'operation_id': 'operation-' + uuid.uuid4().hex, 'workspace_id': self.workspace.pk,
            'session_id': self.session.pk, 'environment_id': self.environment.pk, 'profile_digest': 'sha256:' + 'c' * 64,
            'request': self.request, 'request_digest': _digest(self.request)}

    def native_fields(self):
        return {'instance_id': 'instance_' + 'a' * 64, 'container_id': 'b' * 64,
            'host_id': 'native-host', 'allocation_execution_id': 'native-origin'}

    def test_provider_target_is_persisted_without_native_aliases_and_cannot_be_rebound(self):
        operation = EnvironmentMaintenanceOperation.objects.create(**self.fields(), provider_target=self.target)
        operation.refresh_from_db()
        self.assertEqual(operation.provider_target, self.target)
        self.assertEqual((operation.instance_id, operation.container_id, operation.host_id, operation.allocation_execution_id),
            (None, None, None, None))
        operation.provider_target = self.target | {'sandboxId': 'another-sandbox'}
        with self.assertRaisesRegex(ValueError, 'immutable'):
            operation.save()
        with transaction.atomic(), self.assertRaises(DatabaseError):
            EnvironmentMaintenanceOperation.objects.filter(pk=operation.pk).update(
                provider_target=self.target | {'sandboxId': 'another-sandbox'})
        operation.refresh_from_db()
        self.assertEqual(operation.provider_target, self.target)

    def test_existing_native_operation_facts_cannot_be_changed_through_sql(self):
        operation = EnvironmentMaintenanceOperation.objects.create(**self.fields(), **self.native_fields())
        with transaction.atomic(), self.assertRaises(DatabaseError):
            EnvironmentMaintenanceOperation.objects.filter(pk=operation.pk).update(
                request=self.request | {'program': '/bin/false'})
        operation.refresh_from_db()
        self.assertEqual(operation.request, self.request)

    def test_mixed_or_untyped_targets_are_rejected_before_persisting_a_responsibility(self):
        invalid = [None, {}, [], self.target | {'containerId': 'b' * 64},
            self.target | {'sandboxId': 'sandbox_未知'}, self.target | {'provider': 'docker'}]
        for target in invalid:
            with self.subTest(target=target), transaction.atomic(), self.assertRaises((ValueError, DatabaseError)):
                EnvironmentMaintenanceOperation.objects.create(**self.fields(), provider_target=target)
        for changes in ({'container_id': 'b' * 64}, {'operation_kind': 'provision'}):
            with self.subTest(changes=changes), transaction.atomic(), self.assertRaises((ValueError, DatabaseError)):
                EnvironmentMaintenanceOperation.objects.create(**self.fields(), provider_target=self.target, **changes)
        self.assertEqual(EnvironmentMaintenanceOperation.objects.count(), 0)

    def test_forward_migration_preserves_native_operation_and_signed_history_exactly(self):
        loader = MigrationLoader(connection)
        before = loader.project_state([('app_core', '0027_workspace_snapshot_publication')])
        migration = importlib.import_module('app_core.migrations.0028_provider_maintenance_target').Migration(
            '0028_provider_maintenance_target', 'app_core')
        namespace = connection.ops.quote_name('provider_target_' + uuid.uuid4().hex)
        with connection.cursor() as cursor:
            cursor.execute('CREATE SCHEMA ' + namespace)
            cursor.execute('SHOW search_path')
            previous_path = cursor.fetchone()[0]
            cursor.execute('SET search_path TO ' + namespace + ',public')
        try:
            OldOperation = before.apps.get_model('app_core', 'EnvironmentMaintenanceOperation')
            OldGrant = before.apps.get_model('app_core', 'EnvironmentMaintenanceGrant')
            with connection.schema_editor() as editor:
                editor.create_model(OldOperation)
                editor.create_model(OldGrant)
            values = self.fields() | self.native_fields()
            old_operation = OldOperation.objects.create(**values)
            path = Path(__file__).resolve().parents[3] / 'tests/workspace/fixtures/environment_maintenance_authorization/v1/valid.json'
            payload = json.loads(path.read_text(encoding='utf-8'))['payload']
            payload.update(workspaceId=self.workspace.pk, sessionId=self.session.pk, environmentId=self.environment.pk,
                controllerOwner={'kind': 'maintenanceOperation', 'operationId': old_operation.pk},
                requestDigest=values['request_digest'])
            old_grant = OldGrant.objects.create(id='old-native-grant', operation=old_operation,
                previous_controller_epoch=payload['controllerEpoch'] - 1, controller_epoch=payload['controllerEpoch'],
                controller_lease_digest=payload['controllerLeaseDigest'], controller_purpose=payload['controllerPurpose'],
                expires_at_ms=payload['expiresAtMs'], payload=payload, digest=_digest(payload),
                signature=sign_maintenance_authorization(payload, 'test-key'))
            OldOperation.objects.filter(pk=old_operation.pk).update(current_grant=old_grant)
            original_operation = OldOperation.objects.values().get(pk=old_operation.pk)
            original_grant = OldGrant.objects.values().get(pk=old_grant.pk)
            original_wire = json.dumps(original_grant, sort_keys=True, separators=(',', ':')).encode()
            state = before
            with connection.schema_editor() as editor:
                for step in migration.operations:
                    next_state = state.clone()
                    step.state_forwards('app_core', next_state)
                    step.database_forwards('app_core', editor, state, next_state)
                    state = next_state
            ConvertedOperation = state.apps.get_model('app_core', 'EnvironmentMaintenanceOperation')
            ConvertedGrant = state.apps.get_model('app_core', 'EnvironmentMaintenanceGrant')
            converted = ConvertedOperation.objects.values().get(pk=old_operation.pk)
            self.assertIsNone(converted.pop('provider_target'))
            self.assertEqual(converted, original_operation)
            self.assertEqual(json.dumps(ConvertedGrant.objects.values().get(pk=old_grant.pk),
                sort_keys=True, separators=(',', ':')).encode(), original_wire)
            provider_values = self.fields() | {'provider_target': self.target}
            saved = ConvertedOperation.objects.create(**provider_values)
            self.assertEqual(saved.provider_target, self.target)
            for changes in ({'provider_target': self.target | {'containerId': 'b' * 64}},
                    {'container_id': 'b' * 64}, {'provider_target': None}, {'operation_kind': 'provision'}):
                with self.subTest(changes=changes), transaction.atomic(), self.assertRaises(DatabaseError):
                    ConvertedOperation.objects.create(**(self.fields() | {'provider_target': self.target} | changes))
            with transaction.atomic(), self.assertRaises(DatabaseError):
                ConvertedOperation.objects.filter(pk=saved.pk).update(provider_target=self.target | {'sandboxId': 'another'})
        finally:
            with connection.cursor() as cursor:
                cursor.execute("SELECT set_config('search_path',%s,false)", [previous_path])
                cursor.execute('DROP SCHEMA ' + namespace + ' CASCADE')
