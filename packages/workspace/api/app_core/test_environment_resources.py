"""Workspace resource defaults cross the database boundary exactly once per Environment."""
import copy

from django.contrib.auth.models import User
from django.db import IntegrityError, transaction
from django.test import TestCase, override_settings

from .models import Session, SessionEnvironment, Workspace


@override_settings(SANDBOX_CPU_MILLI=1000, SANDBOX_MEMORY_BYTES=2 * 1024**3,
    SANDBOX_WORKSPACE_BYTES=4 * 1024**3, SANDBOX_PIDS_LIMIT=512)
class EnvironmentResourceDefaultsTests(TestCase):
    def setUp(self):
        self.user = User.objects.create_user(username="resource-owner")
        self.workspace = Workspace.objects.create(name="Resources", createdBy=self.user)
        self.session = Session.objects.create(workspace=self.workspace, owner=self.user)

    def test_model_and_bulk_inserts_snapshot_the_authoritative_workspace_resources(self):
        resources = {"cpuMilli": 2000, "memoryBytes": 4 * 1024**3, "pidsLimit": 512,
            "workspaceBytes": 10 * 1024**3, "storageBytes": 10 * 1024**3}
        Workspace.objects.filter(pk=self.workspace.pk).update(
            execution_resources=resources, execution_settings_revision=2, execution_network="publicInternet")
        created = SessionEnvironment.objects.create(session=self.session,
            resource_defaults={"forged": True}, resource_defaults_revision=999)
        bulk = SessionEnvironment(session=self.session, resource_defaults={"forged": True}, resource_defaults_revision=999)
        SessionEnvironment.objects.bulk_create([bulk])
        bulk.refresh_from_db()
        for environment in (created, bulk):
            self.assertEqual(environment.resource_defaults, resources)
            self.assertEqual(environment.resource_defaults_revision, 2)
            self.assertEqual(environment.network, "publicInternet")
        self.session.current_environment.refresh_from_db()
        self.assertEqual(self.session.current_environment.resource_defaults["cpuMilli"], 1000)
        self.assertEqual(self.session.current_environment.resource_defaults_revision, 1)

    def test_model_and_direct_database_updates_cannot_replace_the_snapshot(self):
        environment = self.session.current_environment
        replacements = {"resource_defaults": environment.resource_defaults | {"cpuMilli": 2000},
            "resource_defaults_revision": 2}
        for field, value in replacements.items():
            with self.subTest(field=field):
                setattr(environment, field, value)
                with self.assertRaisesRegex(ValueError, "environment_resource_defaults_immutable"):
                    environment.save(update_fields=[field])
                environment.refresh_from_db()
                with self.assertRaisesRegex(IntegrityError, "environment_resource_defaults_immutable"), transaction.atomic():
                    SessionEnvironment.objects.filter(pk=environment.pk).update(**{field: value})
                setattr(environment, field, value)
                with self.assertRaisesRegex(IntegrityError, "environment_resource_defaults_immutable"), transaction.atomic():
                    SessionEnvironment.objects.bulk_update([environment], [field])
                environment.refresh_from_db()

    def test_database_rejects_noncanonical_resource_objects_and_transport_overflow(self):
        original = copy.deepcopy(self.workspace.execution_resources)
        missing = dict(original)
        del missing["pidsLimit"]
        for resources in (None, [], missing, original | {"extra": 1},
                original | {"cpuMilli": True}, original | {"memoryBytes": "1024"},
                original | {"workspaceBytes": 1.5}, original | {"storageBytes": 0},
                original | {"cpuMilli": 2**32}, original | {"pidsLimit": 2**32},
                original | {"memoryBytes": 2**64}, original | {"workspaceBytes": 2**64},
                original | {"storageBytes": 2**64}):
            with self.subTest(resources=resources), self.assertRaises(IntegrityError), transaction.atomic():
                Workspace.objects.filter(pk=self.workspace.pk).update(execution_resources=resources)
        maximum = {"cpuMilli": 2**32 - 1, "pidsLimit": 2**32 - 1,
            "memoryBytes": 2**64 - 1, "workspaceBytes": 2**64 - 1, "storageBytes": 2**64 - 1}
        Workspace.objects.filter(pk=self.workspace.pk).update(execution_resources=maximum)
        environment = SessionEnvironment.objects.create(session=self.session)
        self.assertEqual(environment.resource_defaults, maximum)
