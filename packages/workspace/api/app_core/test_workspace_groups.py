import json
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.test import TestCase

from app_core.models import (
    Source,
    SourceGrant,
    Workspace,
    WorkspaceGroup,
    WorkspaceMembership,
)


User = get_user_model()


class WorkspaceGroupTests(TestCase):
    def setUp(self):
        self.owner = User.objects.create_user(
            username="owner@example.test",
            password="owner-password",
        )
        self.admin = User.objects.create_user(
            username="admin@example.test",
            password="admin-password",
        )
        self.member = User.objects.create_user(
            username="member@example.test",
            password="member-password",
        )
        self.workspace = Workspace.objects.create(
            name="Default",
            createdBy=self.owner,
        )
        self.owner_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.owner,
            role="owner",
        )
        self.admin_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.admin,
            role="admin",
        )
        self.member_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )
        self.all_members = WorkspaceGroup.objects.create(
            workspace=self.workspace,
            name="全体成员",
            kind="all_members",
            createdBy=self.owner,
        )
        self.custom_group = WorkspaceGroup.objects.create(
            workspace=self.workspace,
            name="Research",
            kind="custom",
            createdBy=self.owner,
        )

    def request(self, user, method: str, path: str, payload=None):
        self.client.force_login(user)
        callback = getattr(self.client, method)
        if payload is None:
            return callback(path)
        return callback(
            path,
            data=json.dumps(payload),
            content_type="application/json",
        )

    def test_group_list_is_workspace_visible_but_member_roster_is_admin_only(self):
        self.custom_group.members.add(self.member_membership)
        groups = self.request(
            self.member,
            "get",
            f"/api/workspaces/{self.workspace.id}/groups",
        )
        self.assertEqual(groups.status_code, 200, groups.content)
        self.assertEqual(
            {item["id"] for item in groups.json()["groups"]},
            {self.all_members.id, self.custom_group.id},
        )

        denied = self.request(
            self.member,
            "get",
            f"/api/workspaces/{self.workspace.id}/groups/{self.custom_group.id}/members",
        )
        self.assertEqual(denied.status_code, 404)

        dynamic = self.request(
            self.owner,
            "get",
            f"/api/workspaces/{self.workspace.id}/groups/{self.all_members.id}/members",
        )
        self.assertEqual(dynamic.status_code, 200, dynamic.content)
        self.assertEqual(
            {item["membershipId"] for item in dynamic.json()["members"]},
            {
                self.owner_membership.id,
                self.admin_membership.id,
                self.member_membership.id,
            },
        )
        self.assertFalse(self.all_members.members.exists())

    def test_admin_creates_renames_manages_and_deletes_custom_group(self):
        created = self.request(
            self.admin,
            "post",
            f"/api/workspaces/{self.workspace.id}/groups",
            {"name": "  Finance  "},
        )
        self.assertEqual(created.status_code, 201, created.content)
        group_id = created.json()["group"]["id"]
        self.assertEqual(created.json()["group"]["name"], "Finance")

        renamed = self.request(
            self.admin,
            "patch",
            f"/api/workspaces/{self.workspace.id}/groups/{group_id}",
            {"name": "Operations"},
        )
        self.assertEqual(renamed.status_code, 200, renamed.content)

        member_path = (
            f"/api/workspaces/{self.workspace.id}/groups/{group_id}/members/"
            f"{self.admin_membership.id}"
        )
        self.assertEqual(
            self.request(self.admin, "put", member_path).status_code,
            200,
        )
        self.assertEqual(
            self.request(self.admin, "put", member_path).status_code,
            200,
        )
        group = WorkspaceGroup.objects.get(id=group_id)
        self.assertTrue(group.members.filter(id=self.admin_membership.id).exists())
        self.assertEqual(
            self.request(self.admin, "delete", member_path).status_code,
            200,
        )
        self.assertEqual(
            self.request(self.admin, "delete", member_path).status_code,
            200,
        )

        source = Source.objects.create(
            workspace=self.workspace,
            sourceType="fileTree",
            name="Operations source",
            createdBy=self.owner,
        )
        grant = SourceGrant.objects.create(
            workspace=self.workspace,
            source=source,
            workspaceGroup=group,
            createdBy=self.owner,
        )
        deleted = self.request(
            self.admin,
            "delete",
            f"/api/workspaces/{self.workspace.id}/groups/{group_id}",
        )
        self.assertEqual(deleted.status_code, 200, deleted.content)
        self.assertFalse(WorkspaceGroup.objects.filter(id=group_id).exists())
        self.assertFalse(SourceGrant.objects.filter(id=grant.id).exists())

    def test_system_group_is_immutable_and_member_cannot_mutate_groups(self):
        system_path = (
            f"/api/workspaces/{self.workspace.id}/groups/{self.all_members.id}"
        )
        renamed = self.request(
            self.owner,
            "patch",
            system_path,
            {"name": "Everyone"},
        )
        self.assertEqual(renamed.status_code, 409)
        self.assertEqual(
            renamed.json()["error"],
            "workspace_system_group_immutable",
        )
        self.assertEqual(
            self.request(self.owner, "delete", system_path).status_code,
            409,
        )
        system_member_path = (
            f"{system_path}/members/{self.member_membership.id}"
        )
        self.assertEqual(
            self.request(self.owner, "put", system_member_path).status_code,
            409,
        )

        self.assertEqual(
            self.request(
                self.member,
                "post",
                f"/api/workspaces/{self.workspace.id}/groups",
                {"name": "banana"},
            ).status_code,
            404,
        )
        self.assertEqual(
            self.request(
                self.member,
                "delete",
                f"/api/workspaces/{self.workspace.id}/groups/{self.custom_group.id}",
            ).status_code,
            404,
        )

    def test_group_membership_uses_exact_current_membership_identity(self):
        member_path = (
            f"/api/workspaces/{self.workspace.id}/groups/{self.custom_group.id}/members/"
            f"{self.member_membership.id}"
        )
        self.assertEqual(
            self.request(self.owner, "put", member_path).status_code,
            200,
        )
        old_membership_id = self.member_membership.id
        removed = self.request(
            self.owner,
            "delete",
            f"/api/workspaces/{self.workspace.id}/members/{old_membership_id}",
        )
        self.assertEqual(removed.status_code, 200, removed.content)
        self.assertFalse(self.custom_group.members.exists())

        new_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )
        self.assertNotEqual(new_membership.id, old_membership_id)
        self.assertFalse(
            self.custom_group.members.filter(id=new_membership.id).exists()
        )
        stale = self.request(self.owner, "put", member_path)
        self.assertEqual(stale.status_code, 404)
        self.assertEqual(stale.json()["error"], "workspace_member_not_found")

    def test_group_member_mutation_rejects_cross_workspace_id(self):
        other = User.objects.create_user(
            username="other@example.test",
            password="other-password",
        )
        other_workspace = Workspace.objects.create(
            name="Other",
            createdBy=other,
        )
        other_membership = WorkspaceMembership.objects.create(
            workspace=other_workspace,
            user=other,
            role="owner",
        )
        path = (
            f"/api/workspaces/{self.workspace.id}/groups/{self.custom_group.id}/members/"
            f"{other_membership.id}"
        )
        rejected = self.request(self.owner, "put", path)
        self.assertEqual(rejected.status_code, 404)
        self.assertEqual(rejected.json()["error"], "workspace_member_not_found")

    def test_group_validation_and_delete_transaction_loud_fail(self):
        endpoint = f"/api/workspaces/{self.workspace.id}/groups"
        invalid = self.request(
            self.owner,
            "post",
            endpoint,
            {"name": "", "banana": True},
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json()["error"], "workspace_group_invalid")
        duplicate = self.request(
            self.owner,
            "post",
            endpoint,
            {"name": self.custom_group.name},
        )
        self.assertEqual(duplicate.status_code, 409)
        unchanged = self.request(
            self.owner,
            "patch",
            f"{endpoint}/{self.custom_group.id}",
            {"name": self.custom_group.name},
        )
        self.assertEqual(unchanged.status_code, 409)

        source = Source.objects.create(
            workspace=self.workspace,
            sourceType="fileTree",
            name="Research source",
            createdBy=self.owner,
        )
        grant = SourceGrant.objects.create(
            workspace=self.workspace,
            source=source,
            workspaceGroup=self.custom_group,
            createdBy=self.owner,
        )
        with patch.object(
            WorkspaceGroup,
            "delete",
            side_effect=RuntimeError("group delete failed"),
        ):
            failed = self.request(
                self.owner,
                "delete",
                f"{endpoint}/{self.custom_group.id}",
            )
        self.assertEqual(failed.status_code, 500)
        self.assertTrue(
            WorkspaceGroup.objects.filter(id=self.custom_group.id).exists()
        )
        self.assertTrue(SourceGrant.objects.filter(id=grant.id).exists())
