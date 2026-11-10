"""Public invitation/session acceptance for workspace resource administrators."""
import json
import logging
from urllib.parse import parse_qs, urlparse

from django.contrib.auth import get_user_model
from django.test import Client, TestCase

from app_core.models import ModelProvider, Workspace, WorkspaceMembership


class WorkspaceResourceInvitationAcceptanceTests(TestCase):
    password = "ResourceAdmin!2026Accept"

    def setUp(self):
        self.owner = get_user_model().objects.create_user(
            username="resource-owner@example.test", password="WorkspaceOwner!2026Accept"
        )
        self.workspace = Workspace.objects.create(name="Inviting workspace", createdBy=self.owner)
        self.foreign = Workspace.objects.create(name="Foreign workspace", createdBy=self.owner)
        self.owner_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace, user=self.owner, role="owner"
        )
        WorkspaceMembership.objects.create(workspace=self.foreign, user=self.owner, role="owner")
        self.base = f"/api/workspaces/{self.workspace.id}"
        self.foreign_base = f"/api/workspaces/{self.foreign.id}"
        self.owner_client = Client()
        self.owner_client.force_login(self.owner)

    @staticmethod
    def send(client, method, path, body=None):
        return getattr(client, method)(path, data=json.dumps(body or {}), content_type="application/json")

    def invited_admin(self):
        email = "invited-resource-admin@example.test"
        issued = self.send(self.owner_client, "post", self.base + "/invitations", {
            "email": email, "role": "admin",
        })
        self.assertEqual(issued.status_code, 201, issued.content)
        token = parse_qs(urlparse(issued.json()["inviteUrl"]).fragment)["token"][0]
        client = Client()
        accepted = self.send(client, "post", "/api/invitations/accept", {
            "token": token, "name": "Invited administrator", "password": self.password,
        })
        self.assertEqual(accepted.status_code, 200, accepted.content)
        self.assertEqual(accepted.json()["role"], "admin")
        user = get_user_model().objects.get(username=email)
        membership = WorkspaceMembership.objects.get(user=user, workspace=self.workspace)
        self.assertEqual(membership.role, "admin")
        self.assertFalse(user.is_staff)
        self.assertFalse(user.is_superuser)
        return client, user, membership

    def create_provider(self, client):
        secret = "synthetic-new-write-only-provider-key"
        events = []
        class Capture(logging.Handler):
            def emit(self, record):
                events.append(record.getMessage())
        handler = Capture()
        logger = logging.getLogger()
        logger.addHandler(handler)
        try:
            response = self.send(client, "post", self.base + "/model-providers", {
                "displayName": "Invited administrator provider", "api": "openai-completions",
                "apiBase": "https://api.example.invalid/v1", "secret": secret,
            })
        finally:
            logger.removeHandler(handler)
        self.assertEqual(response.status_code, 201, response.content)
        self.assertNotIn(secret.encode(), response.content)
        self.assertFalse(any(secret in event for event in events))
        provider = response.json()["provider"]
        self.assertEqual(ModelProvider.objects.get(pk=provider["id"]).workspace_id, self.workspace.id)
        return provider

    def assert_fresh_admin_role(self, client):
        response = client.get("/api/workspaces")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual([(item["id"], item["role"]) for item in response.json()["workspaces"]],
                         [(self.workspace.id, "admin")])
        current = client.get("/api/me")
        self.assertEqual(current.status_code, 200, current.content)
        self.assertFalse(current.json()["user"]["isSuperuser"])

    def test_invited_admin_resource_rights_are_immediate_and_survive_refresh_and_real_relogin(self):
        client, user, _membership = self.invited_admin()
        self.assert_fresh_admin_role(client)
        provider = self.create_provider(client)
        self.assert_fresh_admin_role(client)
        response = self.send(client, "post", "/api/logout")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(client.get(self.base + "/model-providers").status_code, 401)
        response = self.send(client, "post", "/api/login", {
            "email": user.username, "password": self.password,
        })
        self.assertEqual(response.status_code, 200, response.content)
        self.assertFalse(response.json()["user"]["isSuperuser"])
        self.assert_fresh_admin_role(client)
        secret = "synthetic-replacement-after-real-login"
        response = self.send(client, "post", self.base + f"/model-providers/{provider['id']}/credential/rotate", {"secret": secret})
        self.assertEqual(response.status_code, 200, response.content)
        self.assertNotIn(secret.encode(), response.content)
        for path in (self.foreign_base + "/model-providers", "/api/admin/model-providers"):
            denied = client.get(path)
            self.assertIn(denied.status_code, (403, 404), denied.content)
        substituted = self.send(client, "post", self.foreign_base + f"/model-providers/{provider['id']}/credential/rotate", {"secret": "synthetic-denied"})
        self.assertEqual(substituted.status_code, 404, substituted.content)
        self.assertEqual(WorkspaceMembership.objects.filter(workspace=self.workspace, role="owner").count(), 1)

    def test_resource_admin_cannot_transfer_remove_or_demote_owner_or_delete_workspace(self):
        client, _user, membership = self.invited_admin()
        self.create_provider(client)
        transfer = self.send(client, "post", self.base + "/owner-transfer", {
            "targetMembershipId": membership.id, "currentPassword": self.password,
        })
        self.assertEqual(transfer.status_code, 404, transfer.content)
        remove = client.delete(self.base + f"/members/{self.owner_membership.id}")
        self.assertEqual(remove.status_code, 409, remove.content)
        demote = self.send(client, "patch", self.base + f"/members/{self.owner_membership.id}", {"role": "admin"})
        self.assertEqual(demote.status_code, 409, demote.content)
        # No workspace deletion API is introduced by resource administration.
        delete = client.delete(self.base)
        self.assertIn(delete.status_code, (404, 405), delete.content)
        self.workspace.refresh_from_db()
        self.assertEqual(self.workspace.status, "active")
        self.owner_membership.refresh_from_db()
        self.assertEqual(self.owner_membership.role, "owner")
        self.assertEqual(WorkspaceMembership.objects.filter(workspace=self.workspace, role="owner").count(), 1)

    def test_demotion_revokes_resource_writes_in_existing_authenticated_session(self):
        client, user, membership = self.invited_admin()
        provider = self.create_provider(client)
        demoted = self.send(self.owner_client, "patch", self.base + f"/members/{membership.id}", {"role": "member"})
        self.assertEqual(demoted.status_code, 200, demoted.content)
        denied = self.send(client, "post", self.base + f"/model-providers/{provider['id']}/credential/rotate", {"secret": "synthetic-denied"})
        self.assertEqual(denied.status_code, 404, denied.content)
        user.refresh_from_db()
        self.assertFalse(user.is_superuser)
        listed = client.get("/api/workspaces")
        self.assertEqual(listed.status_code, 200, listed.content)
        self.assertEqual(listed.json()["workspaces"][0]["role"], "member")
