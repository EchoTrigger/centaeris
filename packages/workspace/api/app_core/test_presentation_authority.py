"""Presentation reads recheck user, Workspace and Session authority."""
from asgiref.sync import async_to_sync
from django.contrib.auth import get_user_model
from django.test import Client, RequestFactory, TransactionTestCase
from django.utils import timezone
from . import models
from .http.presentation import PresentationLoader
from .presentation_stream import PresentationScope
from .session_record_test_fixtures import SessionRecordFixture

class BrowserPresentationAuthorityTests(SessionRecordFixture, TransactionTestCase):
    serialized_rollback = True

    def loaders(self, *, session=None):
        selected = session or self.session
        result = []
        for session_id in (None, selected.pk):
            request = RequestFactory().get("/")
            request.user = self.user
            result.append(PresentationLoader(request, PresentationScope(self.workspace.pk, session_id)))
        return result

    def assert_authority(self, loaders, *, navigation=True, selected=True):
        for loader, expected in zip(loaders, (navigation, selected)):
            with self.subTest(session_id=loader.scope.session_id):
                self.assertIs(async_to_sync(loader.authorized)(), expected)

    def assert_http(self, status=200, *, browser=None, session=None):
        path = f"/api/workspaces/{self.workspace.pk}/presentation/events"
        for session_id in (None, (session or self.session).pk):
            response = (browser or self.client).get(path, {"sessionId": session_id} if session_id else {})
            self.assertEqual(response.status_code, status)
            if response.streaming:
                async_to_sync(response.aclose)()

    def test_workspace_navigation_and_selected_session_are_available(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        self.assert_http()
        self.assertEqual({row["id"] for row in async_to_sync(loaders[0].navigation)()}, {self.session.pk})
        models.Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=timezone.now())
        self.assertEqual(async_to_sync(loaders[0].navigation)(), [])

    def test_user_workspace_and_session_lifecycle_are_rechecked(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        for query, denied, restored in (
            (get_user_model().objects.filter(pk=self.user.pk), {"is_active": False}, {"is_active": True}),
            (models.Workspace.objects.filter(pk=self.workspace.pk), {"status": "deleted"}, {"status": "active"}),
        ):
            query.update(**denied)
            self.assert_authority(loaders, navigation=False, selected=False)
            query.update(**restored)
            self.assert_authority(loaders)
        models.Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=timezone.now())
        self.assert_authority(loaders, navigation=True, selected=False)

    def test_membership_roles_allow_reads_and_rejoin_restores_browser_access(self):
        loaders = self.loaders()
        for role in ("owner", "admin", "member"):
            models.WorkspaceMembership.objects.filter(pk=self.membership.pk).update(role=role)
            self.assert_authority(loaders)
        self.membership.delete()
        self.assert_authority(loaders, navigation=False, selected=False)
        models.WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user)
        self.assert_authority(loaders)

    def test_anonymous_and_foreign_owned_session_reads_are_rejected(self):
        self.assert_http(401, browser=Client())
        foreign = models.Session.objects.create(workspace=self.workspace, owner=self.other)
        self.assert_authority(self.loaders(session=foreign), navigation=True, selected=False)
        response = self.client.get(f"/api/workspaces/{self.workspace.pk}/presentation/events", {"sessionId": foreign.pk})
        self.assertEqual(response.status_code, 404)
        self.assertEqual(response.json(), {"error": "presentation_scope_not_found"})

    def test_agent_run_api_session_is_outside_browser_presentation_scope(self):
        session = models.Session.objects.create(
            workspace=self.workspace, owner=self.user, purpose="agentRunApi")
        self.assert_authority(self.loaders(session=session), navigation=True, selected=False)
        response = self.client.get(f"/api/workspaces/{self.workspace.pk}/presentation/events",
            {"sessionId": session.pk})
        self.assertEqual(response.status_code, 404)

    def test_selected_session_must_belong_to_the_requested_workspace(self):
        workspace = models.Workspace.objects.create(name="Other sessions", createdBy=self.user)
        models.WorkspaceMembership.objects.create(workspace=workspace, user=self.user, role="owner")
        session = models.Session.objects.create(workspace=workspace, owner=self.user)
        self.assert_authority(self.loaders(session=session), navigation=True, selected=False)
        request = RequestFactory().get("/")
        request.user = self.user
        loader = PresentationLoader(request, PresentationScope(workspace.pk, session.pk))
        self.assertIs(async_to_sync(loader.authorized)(), True)
        response = self.client.get(f"/api/workspaces/{self.workspace.pk}/presentation/events",
            {"sessionId": session.pk})
        self.assertEqual(response.status_code, 404)
