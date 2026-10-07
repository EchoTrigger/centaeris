import hashlib
import json
from datetime import timedelta
from urllib.parse import parse_qs, urlparse

from django.contrib.auth import get_user_model
from django.test import TestCase
from django.utils import timezone

from app_core.models import (
    Agent,
    Source,
    SourceGrant,
    Workspace,
    WorkspaceGroup,
    WorkspaceInvitation,
    WorkspaceMembership,
)


class WorkspaceInvitationTests(TestCase):
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
        self.owner_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.owner,
            role="owner",
        )

    def issue(self, email: str, role: str = "member"):
        self.client.force_login(self.owner)
        response = self.client.post(
            f"/api/workspaces/{self.workspace.id}/invitations",
            data=json.dumps({"email": email, "role": role}),
            content_type="application/json",
        )
        self.assertEqual(response.status_code, 201, response.content)
        invitation_url = response.json()["inviteUrl"]
        token = parse_qs(urlparse(invitation_url).fragment)["token"][0]
        return response, token

    def preview(self, token: str):
        return self.client.post(
            "/api/invitations/preview",
            data=json.dumps({"token": token}),
            content_type="application/json",
        )

    def accept(self, token: str, payload: dict):
        return self.client.post(
            "/api/invitations/accept",
            data=json.dumps({"token": token, **payload}),
            content_type="application/json",
        )

    def test_invitation_is_durable_but_does_not_create_account_or_membership(self):
        response, token = self.issue("Invitee@Example.test", "admin")

        invitation = WorkspaceInvitation.objects.get()
        self.assertEqual(invitation.email, "invitee@example.test")
        self.assertEqual(invitation.role, "admin")
        self.assertEqual(invitation.status, "pending")
        self.assertEqual(
            invitation.token_digest,
            f"sha256:{hashlib.sha256(token.encode()).hexdigest()}",
        )
        self.assertNotIn(token, invitation.token_digest)
        self.assertFalse(
            get_user_model().objects.filter(username="invitee@example.test").exists()
        )
        self.assertEqual(WorkspaceMembership.objects.count(), 1)
        self.assertEqual(response.json()["invitation"]["id"], invitation.id)

        listed = self.client.get(
            f"/api/workspaces/{self.workspace.id}/invitations"
        )
        self.assertEqual(listed.status_code, 200)
        self.assertEqual(
            [item["id"] for item in listed.json()["invitations"]],
            [invitation.id],
        )

        preview = self.preview(token)
        self.assertEqual(preview.status_code, 200)
        self.assertEqual(
            preview.json(),
            {
                "workspaceId": self.workspace.id,
                "workspaceName": "Default",
                "email": "invitee@example.test",
                "role": "admin",
                "accountExists": False,
                "expiresAt": invitation.expires_at.isoformat(),
            },
        )
        self.assertEqual(
            self.client.get(f"/api/invitations/{token}").status_code,
            404,
        )
        self.assertEqual(
            self.client.post(
                f"/api/invitations/{token}/accept",
                data=json.dumps({}),
                content_type="application/json",
            ).status_code,
            404,
        )

    def test_invitation_token_requests_fail_loudly(self):
        invalid_preview = self.client.post(
            "/api/invitations/preview",
            data=json.dumps({"token": "banana", "extra": True}),
            content_type="application/json",
        )
        invalid_accept = self.client.post(
            "/api/invitations/accept",
            data=json.dumps({"name": "Missing token"}),
            content_type="application/json",
        )

        self.assertEqual(invalid_preview.status_code, 400)
        self.assertEqual(
            invalid_preview.json(),
            {"error": "workspace_invitation_preview_invalid"},
        )
        self.assertEqual(invalid_accept.status_code, 400)
        self.assertEqual(
            invalid_accept.json(),
            {"error": "workspace_invitation_accept_invalid"},
        )

    def test_member_cannot_invite_and_unknown_role_loud_fails(self):
        member = get_user_model().objects.create_user(
            username="member@example.test",
            email="member@example.test",
        )
        WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=member,
            role="member",
        )
        self.client.force_login(member)

        denied = self.client.post(
            f"/api/workspaces/{self.workspace.id}/invitations",
            data=json.dumps({"email": "new@example.test", "role": "member"}),
            content_type="application/json",
        )
        self.assertEqual(denied.status_code, 404)

        self.client.force_login(self.owner)
        invalid = self.client.post(
            f"/api/workspaces/{self.workspace.id}/invitations",
            data=json.dumps({"email": "new@example.test", "role": "banana"}),
            content_type="application/json",
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(invalid.json()["error"], "workspace_invitation_invalid")

    def test_existing_account_accepts_then_rejoin_gets_new_membership_identity(self):
        account = get_user_model().objects.create_user(
            username="existing@example.test",
            email="existing@example.test",
            password="ExistingAccount!2026Secure",
        )
        all_members = WorkspaceGroup.objects.create(
            workspace=self.workspace,
            name="全体成员",
            kind="all_members",
            createdBy=self.owner,
        )
        source = Source.objects.create(
            workspace=self.workspace,
            sourceType="fileTree",
            name="共享资料",
            status="ready",
            createdBy=self.owner,
        )
        SourceGrant.objects.create(
            workspace=self.workspace,
            source=source,
            workspaceGroup=all_members,
            createdBy=self.owner,
        )
        first_invite, first_token = self.issue(account.email, "member")
        self.client.force_login(account)

        accepted = self.accept(first_token, {})

        self.assertEqual(accepted.status_code, 200, accepted.content)
        first_membership = WorkspaceMembership.objects.get(
            workspace=self.workspace,
            user=account,
        )
        self.assertEqual(accepted.json()["membershipId"], first_membership.id)
        self.assertFalse(accepted.json()["userCreated"])
        first_invitation = WorkspaceInvitation.objects.get(
            id=first_invite.json()["invitation"]["id"]
        )
        self.assertEqual(first_invitation.status, "accepted")
        self.assertEqual(
            first_invitation.accepted_membership_ref,
            first_membership.id,
        )
        self.assertEqual(first_membership.invited_by, self.owner)
        first_agent = Agent.objects.get(workspace=self.workspace, owner=account)
        self.assertEqual(first_agent.name, "Centaeris")
        visible_sources = self.client.get(
            f"/api/workspaces/{self.workspace.id}/sources"
        )
        self.assertEqual(visible_sources.status_code, 200)
        self.assertEqual(
            [item["id"] for item in visible_sources.json()["sources"]],
            [source.id],
        )
        self.assertFalse(all_members.members.exists())
        replayed = self.accept(first_token, {})
        self.assertEqual(replayed.status_code, 409)
        self.assertEqual(replayed.json()["error"], "invitation_not_pending")

        first_membership_id = first_membership.id
        first_membership.delete()
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/sources"
            ).status_code,
            404,
        )
        _, second_token = self.issue(account.email, "admin")
        self.client.force_login(account)
        rejoined = self.accept(second_token, {})

        self.assertEqual(rejoined.status_code, 200, rejoined.content)
        self.assertNotEqual(rejoined.json()["membershipId"], first_membership_id)
        self.assertEqual(rejoined.json()["role"], "admin")
        self.assertEqual(
            list(
                Agent.objects.filter(workspace=self.workspace, owner=account).values_list(
                    "id", flat=True
                )
            ),
            [first_agent.id],
        )
        self.assertEqual(
            self.client.get(
                f"/api/workspaces/{self.workspace.id}/sources"
            ).status_code,
            200,
        )

    def test_new_account_is_created_and_logged_in_only_when_accepting(self):
        _, token = self.issue("new-account@example.test")
        self.client.logout()

        accepted = self.accept(
            token,
            {
                "name": "New Account",
                "password": "SunlitHarbor!2026Secure",
            },
        )

        self.assertEqual(accepted.status_code, 200, accepted.content)
        self.assertTrue(accepted.json()["userCreated"])
        account = get_user_model().objects.get(username="new-account@example.test")
        self.assertEqual(account.email, "new-account@example.test")
        self.assertEqual(account.first_name, "New Account")
        self.assertTrue(account.check_password("SunlitHarbor!2026Secure"))
        membership = WorkspaceMembership.objects.get(
            workspace=self.workspace,
            user=account,
        )
        self.assertEqual(membership.id, accepted.json()["membershipId"])
        self.assertTrue(
            Agent.objects.filter(
                workspace=self.workspace,
                owner=account,
                name="Centaeris",
                status="active",
            ).exists()
        )
        self.assertEqual(self.client.get("/api/me").status_code, 200)

    def test_new_account_password_uses_django_policy_before_acceptance(self):
        _, token = self.issue("weak-password@example.test")
        self.client.logout()

        rejected = self.accept(
            token,
            {"name": "Weak Password", "password": "short"},
        )

        self.assertEqual(rejected.status_code, 400)
        self.assertEqual(rejected.json()["error"], "password_invalid")
        self.assertFalse(
            get_user_model().objects.filter(
                username="weak-password@example.test"
            ).exists()
        )
        self.assertEqual(
            WorkspaceInvitation.objects.get().status,
            "pending",
        )

    def test_existing_account_requires_matching_login(self):
        invited = get_user_model().objects.create_user(
            username="invited@example.test",
            email="invited@example.test",
        )
        other = get_user_model().objects.create_user(
            username="other@example.test",
            email="other@example.test",
        )
        _, token = self.issue(invited.email)
        self.client.logout()

        anonymous = self.accept(token, {})
        self.assertEqual(anonymous.status_code, 401)
        self.assertEqual(anonymous.json()["error"], "invitation_login_required")

        self.client.force_login(other)
        mismatch = self.accept(token, {})
        self.assertEqual(mismatch.status_code, 403)
        self.assertEqual(mismatch.json()["error"], "invitation_account_mismatch")
        self.assertFalse(
            WorkspaceMembership.objects.filter(
                workspace=self.workspace,
                user=invited,
            ).exists()
        )

    def test_revoke_resend_and_expiry_invalidate_old_tokens(self):
        first, first_token = self.issue("resend@example.test")
        second, second_token = self.issue("resend@example.test")
        first_invitation = WorkspaceInvitation.objects.get(
            id=first.json()["invitation"]["id"]
        )
        self.assertEqual(first_invitation.status, "revoked")
        self.assertEqual(first_invitation.revoked_by, self.owner)
        self.assertEqual(
            self.preview(first_token).status_code,
            404,
        )

        second_id = second.json()["invitation"]["id"]
        revoked = self.client.delete(
            f"/api/workspaces/{self.workspace.id}/invitations/{second_id}"
        )
        self.assertEqual(revoked.status_code, 200)
        self.client.logout()
        self.assertEqual(self.accept(second_token, {}).status_code, 409)

        _, expired_token = self.issue("expired@example.test")
        invitation = WorkspaceInvitation.objects.get(email="expired@example.test")
        invitation.expires_at = timezone.now() - timedelta(seconds=1)
        invitation.save(update_fields=["expires_at", "updated_at"])
        self.client.logout()
        expired = self.accept(
            expired_token,
            {
                "name": "Expired Account",
                "password": "SunlitHarbor!2026Secure",
            },
        )
        self.assertEqual(expired.status_code, 410)
        invitation.refresh_from_db()
        self.assertEqual(invitation.status, "expired")

    def test_admin_can_invite_and_list_members(self):
        admin = get_user_model().objects.create_user(
            username="admin@example.test",
            email="admin@example.test",
        )
        admin_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=admin,
            role="admin",
        )
        self.client.force_login(admin)
        invited = self.client.post(
            f"/api/workspaces/{self.workspace.id}/invitations",
            data=json.dumps({"email": "reader@example.test", "role": "member"}),
            content_type="application/json",
        )
        self.assertEqual(invited.status_code, 201, invited.content)

        members = self.client.get(f"/api/workspaces/{self.workspace.id}/members")
        self.assertEqual(members.status_code, 200)
        self.assertEqual(
            {item["membershipId"] for item in members.json()["members"]},
            {self.owner_membership.id, admin_membership.id},
        )
