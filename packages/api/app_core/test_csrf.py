import hashlib
from datetime import timedelta

from django.contrib.auth import get_user_model
from django.test import Client, TestCase
from django.utils import timezone

from app_core.assets import tombstone_stored_object
from app_core.models import (
    Agent,
    Session,
    Source,
    SourceGrant,
    UserLibraryObject,
    Workspace,
    WorkspaceGroup,
    WorkspaceInvitation,
    WorkspaceMembership,
)


class CsrfTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(
            username="csrf@example.test",
            email="csrf@example.test",
            password="CorrectBatteryHorse!2026",
        )
        self.client = Client(enforce_csrf_checks=True)

    def test_login_rejects_missing_token(self):
        response = self.client.post(
            "/api/login",
            data='{"email":"csrf@example.test","password":"CorrectBatteryHorse!2026"}',
            content_type="application/json",
        )

        self.assertEqual(response.status_code, 403)

    def test_login_accepts_issued_token(self):
        csrf = self.client.get("/api/csrf")
        token = csrf.json()["csrfToken"]

        response = self.client.post(
            "/api/login",
            data='{"email":"csrf@example.test","password":"CorrectBatteryHorse!2026"}',
            content_type="application/json",
            HTTP_X_CSRFTOKEN=token,
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["user"]["email"], self.user.email)

    def test_invitation_account_creation_rejects_missing_token(self):
        workspace = Workspace.objects.create(name="Default", createdBy=self.user)
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=self.user,
            role="owner",
        )
        token = "csrf-invitation-token"
        WorkspaceInvitation.objects.create(
            workspace=workspace,
            email="invited@example.test",
            role="member",
            token_digest=f"sha256:{hashlib.sha256(token.encode()).hexdigest()}",
            invited_by=self.user,
            expires_at=timezone.now() + timedelta(hours=1),
        )

        response = self.client.post(
            "/api/invitations/accept",
            data=(
                '{"token":"csrf-invitation-token","name":"Invited",'
                '"password":"SunlitHarbor!2026Secure"}'
            ),
            content_type="application/json",
        )

        self.assertEqual(response.status_code, 403)
        self.assertFalse(
            get_user_model().objects.filter(username="invited@example.test").exists()
        )

    def test_workspace_member_mutations_reject_missing_token(self):
        target = get_user_model().objects.create_user(
            username="target@example.test",
            email="target@example.test",
            password="TargetAccount!2026Secure",
        )
        workspace = Workspace.objects.create(name="Default", createdBy=self.user)
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=self.user,
            role="owner",
        )
        target_membership = WorkspaceMembership.objects.create(
            workspace=workspace,
            user=target,
            role="member",
        )
        self.client.force_login(self.user)
        member_endpoint = (
            f"/api/workspaces/{workspace.id}/members/{target_membership.id}"
        )

        role_change = self.client.patch(
            member_endpoint,
            data='{"role":"admin"}',
            content_type="application/json",
        )
        removal = self.client.delete(member_endpoint)
        transfer = self.client.post(
            f"/api/workspaces/{workspace.id}/owner-transfer",
            data=(
                '{"targetMembershipId":"'
                f"{target_membership.id}"
                '","currentPassword":"CorrectBatteryHorse!2026"}'
            ),
            content_type="application/json",
        )

        self.assertEqual(role_change.status_code, 403)
        self.assertEqual(removal.status_code, 403)
        self.assertEqual(transfer.status_code, 403)
        target_membership.refresh_from_db()
        self.assertEqual(target_membership.role, "member")

    def test_workspace_group_mutations_reject_missing_token(self):
        workspace = Workspace.objects.create(name="Default", createdBy=self.user)
        membership = WorkspaceMembership.objects.create(
            workspace=workspace,
            user=self.user,
            role="owner",
        )
        group = WorkspaceGroup.objects.create(
            workspace=workspace,
            name="Research",
            createdBy=self.user,
        )
        self.client.force_login(self.user)

        created = self.client.post(
            f"/api/workspaces/{workspace.id}/groups",
            data='{"name":"Finance"}',
            content_type="application/json",
        )
        added = self.client.put(
            f"/api/workspaces/{workspace.id}/groups/{group.id}/members/{membership.id}"
        )

        self.assertEqual(created.status_code, 403)
        self.assertEqual(added.status_code, 403)
        self.assertFalse(group.members.exists())

    def test_agent_mutations_reject_missing_token(self):
        workspace = Workspace.objects.create(name="Default", createdBy=self.user)
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=self.user,
            role="owner",
        )
        agent = Agent.objects.create(
            workspace=workspace,
            owner=self.user,
            name="Private",
        )
        self.client.force_login(self.user)

        created = self.client.post(
            f"/api/workspaces/{workspace.id}/agents",
            data='{"name":"Denied"}',
            content_type="application/json",
        )
        updated = self.client.patch(
            f"/api/agents/{agent.id}",
            data='{"name":"Denied"}',
            content_type="application/json",
        )
        deleted = self.client.delete(f"/api/agents/{agent.id}")

        self.assertEqual(created.status_code, 403)
        self.assertEqual(updated.status_code, 403)
        self.assertEqual(deleted.status_code, 403)
        agent.refresh_from_db()
        self.assertEqual(agent.name, "Private")
        self.assertEqual(agent.status, "active")

    def test_agent_and_session_restore_reject_missing_token(self):
        workspace = Workspace.objects.create(name="Default", createdBy=self.user)
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=self.user,
            role="owner",
        )
        agent = Agent.objects.create(
            workspace=workspace,
            owner=self.user,
            name="Trash",
        )
        session = Session.objects.create(
            workspace=workspace,
            owner=self.user,
            agent=agent,
        )
        deleted_at = timezone.now()
        Agent.objects.filter(id=agent.id).update(
            status="deleted",
            deletedAt=deleted_at,
        )
        Session.objects.filter(id=session.id).update(
            status="deleted",
            deletedAt=deleted_at,
        )
        self.client.force_login(self.user)

        agent_restore = self.client.post(f"/api/agents/{agent.id}/restore")
        session_restore = self.client.post(f"/api/sessions/{session.id}/restore")
        agent_permanent = self.client.delete(f"/api/agents/{agent.id}/trash")
        session_permanent = self.client.delete(f"/api/sessions/{session.id}/trash")

        self.assertEqual(agent_restore.status_code, 403)
        self.assertEqual(session_restore.status_code, 403)
        self.assertEqual(agent_permanent.status_code, 403)
        self.assertEqual(session_permanent.status_code, 403)
        agent.refresh_from_db()
        session.refresh_from_db()
        self.assertEqual(agent.status, "deleted")
        self.assertEqual(session.status, "deleted")

    def test_library_restore_rejects_missing_token(self):
        item = UserLibraryObject.objects.create(
            owner=self.user,
            displayName="Trash",
            objectKind="folder",
            contentType="application/vnd.centaeris.folder",
            sizeBytes=0,
            status="ready",
        )
        tombstone_stored_object(item)
        self.client.force_login(self.user)

        response = self.client.post(f"/api/library/{item.id}/restore")
        permanent = self.client.delete(f"/api/library/{item.id}/trash")

        self.assertEqual(response.status_code, 403)
        self.assertEqual(permanent.status_code, 403)
        item.refresh_from_db()
        self.assertEqual(item.status, "deleted")

    def test_source_grant_mutations_reject_missing_token(self):
        workspace = Workspace.objects.create(name="Default", createdBy=self.user)
        WorkspaceMembership.objects.create(
            workspace=workspace,
            user=self.user,
            role="owner",
        )
        group = WorkspaceGroup.objects.create(
            workspace=workspace,
            name="Research",
            createdBy=self.user,
        )
        source = Source.objects.create(
            workspace=workspace,
            sourceType="fileTree",
            name="Research",
            createdBy=self.user,
        )
        grant = SourceGrant.objects.create(
            workspace=workspace,
            source=source,
            workspaceGroup=group,
            createdBy=self.user,
        )
        endpoint = f"/api/workspaces/{workspace.id}/sources/{source.id}/grants"
        self.client.force_login(self.user)

        permanent = self.client.delete(
            f"/api/workspaces/{workspace.id}/sources/{source.id}/trash"
        )
        self.assertEqual(permanent.status_code, 403)

        created = self.client.post(
            endpoint,
            data=(
                '{"workspaceGroupId":"'
                f"{group.id}"
                '","accessLevel":"read"}'
            ),
            content_type="application/json",
        )
        updated = self.client.patch(
            f"{endpoint}/{grant.id}",
            data='{"accessLevel":"write"}',
            content_type="application/json",
        )
        deleted = self.client.delete(f"{endpoint}/{grant.id}")

        self.assertEqual(created.status_code, 403)
        self.assertEqual(updated.status_code, 403)
        self.assertEqual(deleted.status_code, 403)
        grant.refresh_from_db()
        self.assertEqual(grant.accessLevel, "read")
