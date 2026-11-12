import hashlib
import json
from datetime import timedelta
from urllib.parse import parse_qs, urlparse

from django.contrib.auth import get_user_model
from django.test import TestCase
from django.utils import timezone

from app_core.models import (
    AgentRun,
    ModelConfig,
    Session,
    Source,
    UserLibraryObject,
    Workspace,
    WorkspaceGroup,
    WorkspaceInvitation,
    WorkspaceMembership,
)
from app_core.workspace_access import agent_run_membership_is_current
from app_core.testing import create_session


class WorkspaceMemberLifecycleTests(TestCase):
    owner_password = "BlueCircuit!2026Secure"

    def setUp(self):
        User = get_user_model()
        self.owner = User.objects.create_user(
            username="owner@example.test",
            email="owner@example.test",
            password=self.owner_password,
        )
        self.admin = User.objects.create_user(
            username="admin@example.test",
            email="admin@example.test",
            password="AdminAccount!2026Secure",
        )
        self.peer_admin = User.objects.create_user(
            username="peer-admin@example.test",
            email="peer-admin@example.test",
            password="PeerAdmin!2026Secure",
        )
        self.member = User.objects.create_user(
            username="member@example.test",
            email="member@example.test",
            password="MemberAccount!2026Secure",
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
        self.peer_admin_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.peer_admin,
            role="admin",
        )
        self.member_membership = WorkspaceMembership.objects.create(
            workspace=self.workspace,
            user=self.member,
            role="member",
        )

    def update_role(self, actor, membership_id: str, role: str):
        self.client.force_login(actor)
        return self.client.patch(
            f"/api/workspaces/{self.workspace.id}/members/{membership_id}",
            data=json.dumps({"role": role}),
            content_type="application/json",
        )

    def remove_member(self, actor, membership_id: str):
        self.client.force_login(actor)
        return self.client.delete(
            f"/api/workspaces/{self.workspace.id}/members/{membership_id}"
        )

    def issue_invitation(self, actor, email: str, role: str = "member"):
        self.client.force_login(actor)
        response = self.client.post(
            f"/api/workspaces/{self.workspace.id}/invitations",
            data=json.dumps({"email": email, "role": role}),
            content_type="application/json",
        )
        self.assertEqual(response.status_code, 201, response.content)
        token = parse_qs(
            urlparse(response.json()["inviteUrl"]).fragment
        )["token"][0]
        return response, token

    def test_only_owner_can_promote_members_and_demote_administrators(self):
        denied = self.update_role(
            self.member,
            self.peer_admin_membership.id,
            "member",
        )
        self.assertEqual(denied.status_code, 404)

        promoted = self.update_role(
            self.owner,
            self.member_membership.id,
            "admin",
        )
        self.assertEqual(promoted.status_code, 200, promoted.content)
        self.assertEqual(promoted.json()["member"]["role"], "admin")
        self.assertEqual(
            promoted.json()["member"]["membershipId"],
            self.member_membership.id,
        )

        demoted = self.update_role(
            self.owner,
            self.peer_admin_membership.id,
            "member",
        )
        self.assertEqual(demoted.status_code, 200, demoted.content)
        self.assertEqual(demoted.json()["member"]["role"], "member")

        unchanged = self.update_role(
            self.owner,
            self.peer_admin_membership.id,
            "member",
        )
        self.assertEqual(unchanged.status_code, 409)
        self.assertEqual(
            unchanged.json()["error"],
            "workspace_member_role_unchanged",
        )
        self_operation = self.update_role(
            self.admin,
            self.admin_membership.id,
            "member",
        )
        self.assertEqual(self_operation.status_code, 409)
        self.assertEqual(
            self_operation.json()["error"],
            "workspace_member_self_operation_forbidden",
        )
        owner_target = self.update_role(
            self.admin,
            self.owner_membership.id,
            "member",
        )
        self.assertEqual(owner_target.status_code, 409)
        self.assertEqual(
            owner_target.json()["error"],
            "workspace_owner_transfer_required",
        )

        invalid = self.update_role(
            self.admin,
            self.peer_admin_membership.id,
            "banana",
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(
            invalid.json()["error"],
            "workspace_member_role_invalid",
        )

    def test_removal_preserves_history_resources_and_pending_invitations(self):
        custom_group = WorkspaceGroup.objects.create(
            workspace=self.workspace,
            name="研发",
            kind="custom",
            createdBy=self.owner,
        )
        custom_group.members.add(self.peer_admin_membership)
        WorkspaceGroup.objects.create(
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
            createdBy=self.peer_admin,
        )
        personal_object = UserLibraryObject.objects.create(
            owner=self.peer_admin,
            displayName="私人资料",
            objectKind="folder",
            status="ready",
        )
        model = ModelConfig.objects.create(id="fake-model", displayName="Fake")
        session = create_session(
            workspace=self.workspace,
            owner=self.peer_admin,
        )
        agent_run = AgentRun.objects.create(
            workspace=self.workspace,
            session=session,
            user=self.peer_admin,
            modelConfig=model,
            prompt="before removal",
            agent_instructions="",
        )
        self.assertTrue(agent_run_membership_is_current(agent_run))
        pending, pending_token = self.issue_invitation(
            self.peer_admin,
            "pending@example.test",
        )
        old_membership_id = self.peer_admin_membership.id

        removed = self.remove_member(self.owner, old_membership_id)

        self.assertEqual(removed.status_code, 200, removed.content)
        self.assertFalse(
            WorkspaceMembership.objects.filter(id=old_membership_id).exists()
        )
        self.assertFalse(
            custom_group.members.filter(id=self.peer_admin_membership.id).exists()
        )
        self.assertTrue(get_user_model().objects.filter(id=self.peer_admin.id).exists())
        self.assertTrue(Source.objects.filter(id=source.id).exists())
        self.assertTrue(UserLibraryObject.objects.filter(id=personal_object.id).exists())
        self.assertFalse(agent_run_membership_is_current(agent_run))
        invitation = WorkspaceInvitation.objects.get(
            id=pending.json()["invitation"]["id"]
        )
        self.assertEqual(invitation.status, "pending")
        self.client.logout()
        self.assertEqual(
            self.client.post(
                "/api/invitations/preview",
                data=json.dumps({"token": pending_token}),
                content_type="application/json",
            ).status_code,
            200,
        )

        repeated = self.remove_member(self.owner, old_membership_id)
        self.assertEqual(repeated.status_code, 404)
        self.assertEqual(
            repeated.json()["error"],
            "workspace_member_not_found",
        )

        _, rejoin_token = self.issue_invitation(
            self.owner,
            self.peer_admin.email,
        )
        self.client.force_login(self.peer_admin)
        rejoined = self.client.post(
            "/api/invitations/accept",
            data=json.dumps({"token": rejoin_token}),
            content_type="application/json",
        )
        self.assertEqual(rejoined.status_code, 200, rejoined.content)
        self.assertNotEqual(rejoined.json()["membershipId"], old_membership_id)
        self.assertFalse(
            custom_group.members.filter(id=self.peer_admin_membership.id).exists()
        )
        stale_role_change = self.update_role(self.owner, old_membership_id, "member")
        self.assertEqual(stale_role_change.status_code, 404)
        self.assertEqual(
            stale_role_change.json()["error"],
            "workspace_member_not_found",
        )

    def test_admin_cannot_grant_or_change_administrator_roles(self):
        for target, role in (
            (self.member_membership, "admin"),
            (self.peer_admin_membership, "member"),
            (self.peer_admin_membership, "admin"),
        ):
            with self.subTest(target=target.id, role=role):
                original_role = target.role
                response = self.update_role(self.admin, target.id, role)
                self.assertEqual(response.status_code, 403, response.content)
                self.assertEqual(response.json()["error"], "workspace_member_management_forbidden")
                target.refresh_from_db()
                self.assertEqual(target.role, original_role)

    def test_member_removal_respects_role_hierarchy_and_self_protection(self):
        removed = self.remove_member(self.admin, self.peer_admin_membership.id)
        self.assertEqual(removed.status_code, 403, removed.content)
        self.assertEqual(removed.json()["error"], "workspace_member_management_forbidden")
        self.assertTrue(WorkspaceMembership.objects.filter(id=self.peer_admin_membership.id).exists())

        removed = self.remove_member(self.admin, self.member_membership.id)
        self.assertEqual(removed.status_code, 200, removed.content)
        self.assertFalse(WorkspaceMembership.objects.filter(id=self.member_membership.id).exists())

        removed = self.remove_member(self.owner, self.peer_admin_membership.id)
        self.assertEqual(removed.status_code, 200, removed.content)

        admin_self = self.remove_member(self.admin, self.admin_membership.id)
        self.assertEqual(admin_self.status_code, 409)
        self.assertEqual(
            admin_self.json()["error"],
            "workspace_member_self_operation_forbidden",
        )
        owner_self = self.remove_member(self.owner, self.owner_membership.id)
        self.assertEqual(owner_self.status_code, 409)
        self.assertEqual(
            owner_self.json()["error"],
            "workspace_member_self_operation_forbidden",
        )
        denied = self.remove_member(self.member, self.admin_membership.id)
        self.assertEqual(denied.status_code, 404)

    def test_admin_invitation_management_stays_below_actor_role(self):
        issued, _ = self.issue_invitation(self.owner, "candidate@example.test", "admin")
        invitation = WorkspaceInvitation.objects.get(id=issued.json()["invitation"]["id"])
        original_digest = invitation.token_digest
        self.client.force_login(self.admin)
        endpoint = f"/api/workspaces/{self.workspace.id}/invitations"
        for email, role in (
            ("new-admin@example.test", "admin"),
            (invitation.email, "admin"),
            (invitation.email, "member"),
        ):
            with self.subTest(email=email, role=role):
                response = self.client.post(endpoint, data=json.dumps({"email": email, "role": role}),
                    content_type="application/json")
                self.assertEqual(response.status_code, 403, response.content)
                self.assertEqual(response.json()["error"], "workspace_invitation_management_forbidden")
        self.assertFalse(WorkspaceInvitation.objects.filter(email="new-admin@example.test").exists())
        denied = self.client.delete(f"{endpoint}/{invitation.id}")
        self.assertEqual(denied.status_code, 403, denied.content)
        invitation.refresh_from_db()
        self.assertEqual(invitation.status, "pending")
        self.assertEqual(invitation.token_digest, original_digest)

        reissued, _ = self.issue_invitation(self.owner, invitation.email, "admin")
        invitation.refresh_from_db()
        self.assertEqual(invitation.status, "revoked")
        response = self.client.delete(f"{endpoint}/{reissued.json()['invitation']['id']}")
        self.assertEqual(response.status_code, 200, response.content)

        issued, _ = self.issue_invitation(self.owner, "reader@example.test")
        reissued, _ = self.issue_invitation(self.admin, "reader@example.test")
        self.assertEqual(WorkspaceInvitation.objects.get(id=issued.json()["invitation"]["id"]).status, "revoked")
        response = self.client.delete(f"{endpoint}/{reissued.json()['invitation']['id']}")
        self.assertEqual(response.status_code, 200, response.content)

        expired, _ = self.issue_invitation(self.owner, "expired-admin@example.test", "admin")
        WorkspaceInvitation.objects.filter(id=expired.json()["invitation"]["id"]).update(
            expires_at=timezone.now() - timedelta(seconds=1))
        self.issue_invitation(self.admin, "expired-admin@example.test", "member")
        self.assertEqual(WorkspaceInvitation.objects.get(id=expired.json()["invitation"]["id"]).status, "expired")

    def test_existing_administrator_invitation_survives_issuer_removal(self):
        token = "existing-administrator-invitation"
        expires_at = timezone.now() + timedelta(hours=1)
        # An already-issued grant remains valid independently of its issuer.
        invitation = WorkspaceInvitation.objects.create(workspace=self.workspace,
            email="invited-admin@example.test", role="admin", invited_by=self.admin,
            token_digest="sha256:" + hashlib.sha256(token.encode()).hexdigest(), expires_at=expires_at)
        invitee = get_user_model().objects.create_user(username=invitation.email, email=invitation.email)
        removed = self.remove_member(self.owner, self.admin_membership.id)
        self.assertEqual(removed.status_code, 200, removed.content)
        self.client.force_login(invitee)
        response = self.client.post("/api/invitations/accept", data=json.dumps({"token": token}),
            content_type="application/json")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["role"], "admin")
        invitation.refresh_from_db()
        self.assertEqual(invitation.status, "accepted")
        self.assertEqual(invitation.expires_at, expires_at)

    def test_owner_transfer_reauthenticates_and_preserves_membership_identities(self):
        self.client.force_login(self.owner)
        endpoint = f"/api/workspaces/{self.workspace.id}/owner-transfer"
        wrong_password = self.client.post(
            endpoint,
            data=json.dumps(
                {
                    "targetMembershipId": self.member_membership.id,
                    "currentPassword": "wrong-password",
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(wrong_password.status_code, 403)
        self.assertEqual(
            wrong_password.json()["error"],
            "workspace_owner_reauthentication_failed",
        )
        self.owner_membership.refresh_from_db()
        self.member_membership.refresh_from_db()
        self.assertEqual(self.owner_membership.role, "owner")
        self.assertEqual(self.member_membership.role, "member")

        transferred = self.client.post(
            endpoint,
            data=json.dumps(
                {
                    "targetMembershipId": self.member_membership.id,
                    "currentPassword": self.owner_password,
                }
            ),
            content_type="application/json",
        )

        self.assertEqual(transferred.status_code, 200, transferred.content)
        self.assertEqual(
            transferred.json()["owner"]["membershipId"],
            self.member_membership.id,
        )
        self.assertEqual(transferred.json()["owner"]["role"], "owner")
        self.assertEqual(
            transferred.json()["previousOwner"]["membershipId"],
            self.owner_membership.id,
        )
        self.assertEqual(transferred.json()["previousOwner"]["role"], "admin")
        self.owner_membership.refresh_from_db()
        self.member_membership.refresh_from_db()
        self.assertEqual(self.owner_membership.role, "admin")
        self.assertEqual(self.member_membership.role, "owner")
        repeated_by_previous_owner = self.client.post(
            endpoint,
            data=json.dumps(
                {
                    "targetMembershipId": self.admin_membership.id,
                    "currentPassword": self.owner_password,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(repeated_by_previous_owner.status_code, 404)

    def test_owner_transfer_rejects_self_inactive_target_and_invalid_payload(self):
        self.client.force_login(self.owner)
        endpoint = f"/api/workspaces/{self.workspace.id}/owner-transfer"
        self_transfer = self.client.post(
            endpoint,
            data=json.dumps(
                {
                    "targetMembershipId": self.owner_membership.id,
                    "currentPassword": self.owner_password,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(self_transfer.status_code, 409)
        self.assertEqual(
            self_transfer.json()["error"],
            "workspace_owner_transfer_to_self",
        )

        stale_target = self.client.post(
            endpoint,
            data=json.dumps(
                {
                    "targetMembershipId": "wsm_missing",
                    "currentPassword": self.owner_password,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(stale_target.status_code, 404)
        self.assertEqual(
            stale_target.json()["error"],
            "workspace_member_not_found",
        )

        self.member.is_active = False
        self.member.save(update_fields=["is_active"])
        inactive = self.client.post(
            endpoint,
            data=json.dumps(
                {
                    "targetMembershipId": self.member_membership.id,
                    "currentPassword": self.owner_password,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(inactive.status_code, 409)
        self.assertEqual(
            inactive.json()["error"],
            "workspace_owner_transfer_target_inactive",
        )

        invalid = self.client.post(
            endpoint,
            data=json.dumps(
                {
                    "targetMembershipId": self.member_membership.id,
                    "currentPassword": "",
                    "banana": True,
                }
            ),
            content_type="application/json",
        )
        self.assertEqual(invalid.status_code, 400)
        self.assertEqual(
            invalid.json()["error"],
            "workspace_owner_transfer_invalid",
        )
