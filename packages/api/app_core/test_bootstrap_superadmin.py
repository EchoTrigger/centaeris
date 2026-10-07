import os
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.contrib.auth.models import AnonymousUser
from django.core.management import call_command
from django.core.management.base import CommandError
from django.db import IntegrityError, transaction
from django.test import TestCase

from app_core.models import (
    Agent,
    AgentRun,
    ModelConfig,
    Session,
    Workspace,
    WorkspaceGroup,
    WorkspaceMembership,
)
from app_core.testing import create_session
from app_core.agent_run_authorization_factory import create_agent_run_authorization
from app_core.runtime_client import build_agent_run_start
from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES,
    agent_run_membership_is_current,
    workspace_membership_for,
)


class BootstrapSuperadminTests(TestCase):
    email = "root@example.test"
    password = "BlueCircuit!2026Secure"

    def bootstrap(self, **values):
        environment = {
            "BOOTSTRAP_SUPERADMIN_EMAIL": self.email,
            "BOOTSTRAP_SUPERADMIN_PASSWORD": self.password,
            **values,
        }
        with patch.dict(os.environ, environment, clear=False):
            call_command("bootstrap_superadmin")

    def test_creates_superadmin(self):
        self.bootstrap()

        user = get_user_model().objects.get(username=self.email)
        self.assertTrue(user.is_active)
        self.assertTrue(user.is_staff)
        self.assertTrue(user.is_superuser)
        self.assertTrue(user.check_password(self.password))
        workspace = Workspace.objects.get(createdBy=user, name="Default")
        self.assertTrue(workspace.members.filter(id=user.id).exists())
        membership = WorkspaceMembership.objects.get(workspace=workspace, user=user)
        self.assertEqual(membership.role, "owner")
        self.assertTrue(membership.id.startswith("wsm_"))
        workspaceGroup = WorkspaceGroup.objects.get(workspace=workspace, name="全体成员")
        self.assertEqual(workspaceGroup.kind, "all_members")
        self.assertFalse(workspaceGroup.members.exists())
        agent = Agent.objects.get(workspace=workspace, owner=user)
        self.assertEqual(agent.name, "Centaeris")
        self.assertEqual(agent.description, "默认 Agent")
        self.assertEqual(agent.status, "active")

    def test_existing_superadmin_is_not_mutated(self):
        self.bootstrap()
        user = get_user_model().objects.get(username=self.email)
        colleague = get_user_model().objects.create_user(
            username="colleague@example.test",
            email="colleague@example.test",
            password="AnotherLongPassword!2026",
        )
        workspace = Workspace.objects.get(createdBy=user, name="Default")
        workspace.members.add(colleague)
        self.bootstrap(BOOTSTRAP_SUPERADMIN_PASSWORD="AnotherLongPassword!2026")

        self.assertTrue(user.check_password(self.password))
        self.assertFalse(user.check_password("AnotherLongPassword!2026"))
        self.assertEqual(
            Workspace.objects.filter(createdBy=user, name="Default").count(), 1
        )
        self.assertEqual(Agent.objects.filter(workspace=workspace, owner=user).count(), 1)
        self.assertEqual(
            WorkspaceGroup.objects.filter(
                workspace__createdBy=user,
                name="全体成员",
            ).count(),
            1,
        )
        workspace_group = WorkspaceGroup.objects.get(
            workspace=workspace,
            name="全体成员",
        )
        self.assertEqual(workspace_group.kind, "all_members")
        self.assertFalse(workspace_group.members.exists())

    def test_different_bootstrap_identity_cannot_create_second_default_workspace(self):
        legacy = get_user_model().objects.create_superuser(
            username="legacy@example.test",
            email="legacy@example.test",
            password="AnotherLongPassword!2026",
        )
        Workspace.objects.create(name="Default", createdBy=legacy)

        with self.assertRaisesRegex(CommandError, "different bootstrap superadmin"):
            self.bootstrap()

        self.assertFalse(get_user_model().objects.filter(username=self.email).exists())
        self.assertEqual(Workspace.objects.filter(name="Default").count(), 1)

    def test_existing_non_owner_membership_stops_bootstrap(self):
        self.bootstrap()
        membership = WorkspaceMembership.objects.get(
            workspace__name="Default",
            user__username=self.email,
        )
        membership.role = "member"
        membership.save(update_fields=["role", "updated_at"])

        with self.assertRaisesRegex(CommandError, "must own"):
            self.bootstrap()

    def test_workspace_group_kind_loud_fails(self):
        self.bootstrap()
        workspace = Workspace.objects.get(name="Default")
        user = get_user_model().objects.get(username=self.email)

        with self.assertRaisesRegex(
            ValueError,
            "unsupported WorkspaceGroup.kind: banana",
        ):
            WorkspaceGroup.objects.create(
                workspace=workspace,
                name="Invalid",
                kind="banana",
                createdBy=user,
            )

    def test_existing_non_superadmin_stops_bootstrap(self):
        get_user_model().objects.create_user(username=self.email, email=self.email, password=self.password)

        with self.assertRaisesRegex(CommandError, "not the active bootstrap superadmin"):
            self.bootstrap()

    def test_missing_password_stops_bootstrap(self):
        with patch.dict(os.environ, {"BOOTSTRAP_SUPERADMIN_EMAIL": self.email}, clear=True):
            with self.assertRaisesRegex(CommandError, "BOOTSTRAP_SUPERADMIN_PASSWORD"):
                call_command("bootstrap_superadmin")


class WorkspaceAccessTests(TestCase):
    def setUp(self):
        self.owner = get_user_model().objects.create_user(
            username="owner@example.test",
            email="owner@example.test",
            password="BlueCircuit!2026Secure",
        )
        self.workspace = Workspace.objects.create(
            name="Default",
            createdBy=self.owner,
        )
        self.membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.owner,
            role="owner",
        )

    def test_workspace_list_returns_exact_role(self):
        self.client.force_login(self.owner)

        response = self.client.get("/api/workspaces")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {
                "workspaces": [
                    {
                        "id": self.workspace.id,
                        "name": "Default",
                        "description": "",
                        "status": "active",
                        "role": "owner",
                    }
                ]
            },
        )

    def test_workspace_access_uses_membership_role_and_active_workspace(self):
        self.assertEqual(
            workspace_membership_for(
                self.owner,
                self.workspace.id,
                allowed_roles=WORKSPACE_ADMIN_ROLES,
            ),
            self.membership,
        )
        self.assertIsNone(
            workspace_membership_for(AnonymousUser(), self.workspace.id)
        )
        with self.assertRaisesRegex(ValueError, "unsupported workspace role set"):
            workspace_membership_for(
                self.owner,
                self.workspace.id,
                allowed_roles={"banana"},
            )

        self.workspace.status = "archived"
        self.workspace.save(update_fields=["status", "updatedAt"])
        self.assertIsNone(workspace_membership_for(self.owner, self.workspace.id))

    def test_django_staff_does_not_grant_workspace_admin_access(self):
        staff_member = get_user_model().objects.create_user(
            username="staff-member@example.test",
            is_staff=True,
        )
        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=staff_member,
            role="member",
        )
        self.client.force_login(staff_member)

        response = self.client.post(
            f"/api/workspaces/{self.workspace.id}/sources",
            data={"sourceType": "uploadedFile", "name": "Denied"},
            content_type="application/json",
        )

        self.assertEqual(response.status_code, 404)

    def test_membership_identity_changes_after_removal_and_rejoin(self):
        old_membership_id = self.membership.id
        self.membership.delete()

        replacement = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.owner,
            role="owner",
        )

        self.assertNotEqual(replacement.id, old_membership_id)

    def test_agent_run_keeps_exact_membership_identity_after_rejoin(self):
        session = create_session(workspace=self.workspace, owner=self.owner)
        model = ModelConfig.objects.create(displayName="Test model")
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=session,
            user=self.owner,
            modelConfig=model,
            prompt="remember this membership",
        )

        self.assertEqual(agent_run.membership_ref, self.membership.id)
        self.assertTrue(agent_run_membership_is_current(agent_run))
        create_agent_run_authorization(
            agent_run, image_digest=f"sha256:{'a' * 64}"
        )

        self.membership.delete()
        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.owner,
            role="owner",
        )

        self.assertFalse(agent_run_membership_is_current(agent_run))
        with self.assertRaisesRegex(RuntimeError, "no longer current"):
            build_agent_run_start(agent_run)

    def test_membership_role_and_single_owner_are_database_invariants(self):
        invalid = WorkspaceMembership(
            workspace=self.workspace,
            user=get_user_model().objects.create_user(username="banana@example.test"),
            role="banana",
        )
        with self.assertRaisesRegex(ValueError, "unsupported WorkspaceMembership.role"):
            invalid.save()

        second_owner = get_user_model().objects.create_user(
            username="second-owner@example.test"
        )
        with self.assertRaises(IntegrityError), transaction.atomic():
            WorkspaceMembership.objects.create(
                workspace=self.workspace,
                user=second_owner,
                role="owner",
            )
