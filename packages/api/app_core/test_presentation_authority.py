"""Presentation authorization keeps browser, delegated and branch read boundaries."""
from asgiref.sync import async_to_sync
from django.contrib.auth import get_user_model
from django.test import Client, RequestFactory, TransactionTestCase
from django.utils import timezone

from . import models
from .app_delegations import DelegationRejected, authenticate_delegation
from .http.presentation import PresentationLoader
from .presentation_stream import PresentationScope
from .test_app_delegations import AppDelegationSessionFixture
from .test_business_agent_branches import BusinessBranchFixture
from .test_hosted_operations import OperationFixture


class AuthorityAssertions:
    def loaders(self, *, agent=None, session=None, delegated=True, version=None, branch_id=...):
        agent = agent or self.agent
        session = session or self.session
        user = getattr(self, "member", None) or self.user
        grant = getattr(self, "grant", None) if delegated else None
        if branch_id is ...:
            branch_id = getattr(self, "branch_id", None)
        result = []
        for session_id in (None, session.pk):
            request = RequestFactory().get("/", {"agentId": agent.pk})
            request.user = user
            request.app_delegation = grant
            request.app_delegation_credential_version = (
                version if version is not None else grant.credential_version
            ) if grant is not None else None
            request.business_branch_id = branch_id if grant is not None else None
            result.append((request, PresentationLoader(request,
                PresentationScope(self.workspace.pk, agent.pk, session_id))))
        return result

    def assert_authority(self, loaders, *, navigation=True, selected=True):
        for (request, loader), expected in zip(loaders, (navigation, selected)):
            with self.subTest(session_id=loader.scope.session_id):
                if isinstance(expected, tuple):
                    with self.assertRaises(DelegationRejected) as denied:
                        async_to_sync(loader.authorized)()
                    self.assertEqual((denied.exception.status, denied.exception.code), expected)
                else:
                    self.assertIs(async_to_sync(loader.authorized)(), expected)

    def assert_http(self, expected_status=200, expected_code=None, *, agent=None, session=None,
                    token=None, branch_id=None, browser=None):
        agent = agent or self.agent
        session = session or self.session
        path = f"/api/workspaces/{self.workspace.pk}/presentation/events"
        for index, session_id in enumerate((None, session.pk)):
            with self.subTest(http_session_id=session_id):
                query = {"agentId": agent.pk}
                if session_id is not None:
                    query["sessionId"] = session_id
                options = {}
                if token is not None:
                    options["HTTP_AUTHORIZATION"] = "Bearer " + token
                if branch_id is not None:
                    options["HTTP_X_CENTAERIS_BUSINESS_BRANCH_ID"] = branch_id
                response = (browser or Client()).get(path, query, **options)
                status = expected_status[index] if isinstance(expected_status, tuple) else expected_status
                self.assertEqual(response.status_code, status,
                    response.content if not response.streaming else "unexpected stream")
                if response.streaming:
                    async_to_sync(response.aclose)()
                elif expected_code is not None:
                    code = expected_code[index] if isinstance(expected_code, tuple) else expected_code
                    self.assertEqual(response.json(), {"error": code})


class BrowserPresentationAuthorityTests(AuthorityAssertions, OperationFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        self.session = models.Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)

    def test_private_agent_entry_allows_workspace_navigation_and_selected_session(self):
        self.assert_authority(self.loaders())
        self.assert_http(browser=self.client)
        empty = models.Agent.objects.create(workspace=self.workspace, owner=self.user, name="Empty")
        request = RequestFactory().get("/")
        request.user = self.user
        loader = PresentationLoader(request, PresentationScope(self.workspace.pk, empty.pk))
        self.assertIs(async_to_sync(loader.authorized)(), True)
        self.assertEqual({row["id"] for row in async_to_sync(loader.navigation)()}, {self.session.pk})
        models.Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=timezone.now())
        self.assertEqual(async_to_sync(loader.navigation)(), [])

    def test_current_user_workspace_and_session_lifecycle_are_rechecked_independently_of_agent(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        targets = [(get_user_model().objects.filter(pk=self.user.pk), {"is_active": False}, {"is_active": True}),
            (models.Workspace.objects.filter(pk=self.workspace.pk), {"status": "deleted"}, {"status": "active"})]
        for query, denied, restored in targets:
            with self.subTest(changes=denied):
                query.update(**denied)
                self.assert_authority(loaders, navigation=False, selected=False)
                query.update(**restored)
                self.assert_authority(loaders)
        models.Agent.objects.filter(pk=self.agent.pk).update(status="deleted", deletedAt=timezone.now())
        self.assert_authority(loaders, navigation=False, selected=True)
        self.assert_http((404, 200), "presentation_scope_not_found", browser=self.client)
        models.Agent.objects.filter(pk=self.agent.pk).update(status="active", deletedAt=None)
        models.Session.objects.filter(pk=self.session.pk).update(status="deleted", deletedAt=timezone.now())
        self.assert_authority(loaders, navigation=True, selected=False)

    def test_membership_roles_keep_reads_and_private_agent_rejoin_restores_browser_authority(self):
        loaders = self.loaders()
        membership = models.WorkspaceMembership.objects.get(workspace=self.workspace, user=self.user)
        for role in ("owner", "admin", "member"):
            models.WorkspaceMembership.objects.filter(pk=membership.pk).update(role=role)
            self.assert_authority(loaders)
        membership.delete()
        self.assert_authority(loaders, navigation=False, selected=False)
        models.WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user)
        self.assert_authority(loaders)

    def test_http_anonymous_cross_agent_and_foreign_owner_are_rejected(self):
        self.assert_http(401, "authentication_required")
        other = get_user_model().objects.create_user(username="presentation-other")
        foreign = models.Agent.objects.create(workspace=self.workspace, owner=other, name="Foreign")
        self.assert_http(404, "presentation_scope_not_found", agent=foreign, browser=self.client)
        mine = models.Agent.objects.create(workspace=self.workspace, owner=self.user, name="Another")
        request = RequestFactory().get("/")
        request.user = self.user
        wrong = PresentationLoader(request, PresentationScope(self.workspace.pk, mine.pk, self.session.pk))
        self.assertIs(async_to_sync(wrong.authorized)(), False)


class DefinitionPresentationAuthorityTests(AuthorityAssertions, AppDelegationSessionFixture, TransactionTestCase):
    serialized_rollback = True

    def test_definition_grant_and_cookie_allow_the_same_owned_managed_agent(self):
        self.assert_authority(self.loaders())
        self.assert_authority(self.loaders(delegated=False))
        self.assert_http(token=self.token)
        self.assert_http(browser=self.member_client)

    def test_definition_visibility_is_current_for_cookie_and_delegated_connections(self):
        delegated, browser = self.loaders(), self.loaders(delegated=False)
        self.assert_authority(delegated)
        self.assert_authority(browser)
        for changes, restored in [({"availability_scope": "none"}, {"availability_scope": "workspace"}),
                ({"status": "disabled"}, {"status": "active"})]:
            models.AgentDefinition.objects.filter(pk=self.definition["id"]).update(**changes)
            self.assert_authority(delegated, navigation=False, selected=False)
            self.assert_authority(browser, navigation=False, selected=True)
            self.assert_http(403, "delegation_not_available", token=self.token)
            self.assert_http((404, 200), "presentation_scope_not_found", browser=self.member_client)
            models.AgentDefinition.objects.filter(pk=self.definition["id"]).update(**restored)
        self.assertEqual(self.availability(self.definition, "members", [self.membership.pk]).status_code, 200)
        self.assert_authority(delegated)
        models.AgentDefinitionMember.objects.filter(definition_id=self.definition["id"], membership=self.membership).delete()
        self.assert_authority(delegated, navigation=False, selected=False)
        self.assert_authority(browser, navigation=False, selected=True)

    def test_member_roles_remain_allowed_but_replacement_does_not_restore_a_grant(self):
        loaders = self.loaders()
        models.WorkspaceMembership.objects.filter(pk=self.admin_membership.pk).update(role="admin")
        for role in ("owner", "admin", "member"):
            models.WorkspaceMembership.objects.filter(pk=self.membership.pk).update(role=role)
            self.assert_authority(loaders)
        self.membership.delete()
        self.assert_authority(loaders, navigation=False, selected=False)
        models.WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member)
        self.assert_authority(loaders, navigation=(403, "delegation_not_available"), selected=False)
        self.assert_authority(self.loaders(delegated=False))
        self.assert_http(403, "delegation_not_available", token=self.token)

    def test_cached_grant_rechecks_rotation_and_new_credential_can_read(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        rotated = self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
            {"expectedCredentialVersion": 1}, client=self.member_client)
        self.assertEqual(rotated.status_code, 200, rotated.content)
        self.assert_authority(loaders, navigation=(401, "delegation_invalid"), selected=False)
        self.assert_http(401, "delegation_invalid", token=self.token)
        token = rotated.json()["accessToken"]
        self.grant = authenticate_delegation(token, "sessions:read")
        self.assert_authority(self.loaders())
        self.assert_http(token=token)

    def test_scope_expiry_application_and_revocation_errors_keep_their_codes(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        for changes, restored, code in [
                ({"scopes": ["assistant:use"]}, {"scopes": self.grant.scopes}, "delegation_scope_forbidden"),
                ({"expires_at": timezone.now()}, {"expires_at": self.grant.expires_at}, "delegation_not_available"),
                ({"revoked_at": timezone.now()}, {"revoked_at": None}, "delegation_not_available")]:
            models.UserAppDelegation.objects.filter(pk=self.grant.pk).update(**changes)
            self.assert_authority(loaders, navigation=(403, code), selected=False)
            self.assert_http(403, code, token=self.token)
            models.UserAppDelegation.objects.filter(pk=self.grant.pk).update(**restored)
        models.BusinessApplication.objects.filter(pk=self.application["id"]).update(status="revoked")
        self.assert_authority(loaders, navigation=(403, "delegation_not_available"), selected=False)
        self.assert_http(403, "delegation_not_available", token=self.token)

    def test_frozen_request_identity_and_credential_are_not_reselected_after_open(self):
        loaders = self.loaders()
        for request, loader in loaders:
            request.user = self.other
            request.app_delegation = None
            request.app_delegation_credential_version = 99
            request.business_branch_id = "different-branch"
        self.assert_authority(loaders)


class BranchPresentationAuthorityTests(AuthorityAssertions, BusinessBranchFixture, TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        super().setUp()
        self.branch = self.resolve("presentation-A").json()
        self.sibling = self.resolve("presentation-B").json()
        self.branch_id = self.branch["branchId"]
        self.agent = models.Agent.objects.get(pk=self.branch["agentId"])
        self.session = models.Session.objects.get(pk=self.branch["sessionId"])

    def test_branch_allows_bound_navigation_and_selected_coordination(self):
        self.assert_authority(self.loaders())
        self.assert_http(token=self.token, branch_id=self.branch_id)
        self.assert_authority(self.loaders(delegated=False))

    def test_missing_wrong_sibling_and_root_branch_scopes_preserve_errors(self):
        self.assert_http(400, "business_branch_required", token=self.token)
        self.assert_http(404, "business_branch_not_found", token=self.token, branch_id="missing-branch")
        self.assert_http(404, ("business_branch_not_found", "presentation_scope_not_found"),
            token=self.token, branch_id=self.sibling["branchId"])
        self.assert_authority(self.loaders(branch_id=None),
            navigation=(400, "business_branch_required"), selected=False)
        sibling_agent = models.Agent.objects.get(pk=self.sibling["agentId"])
        sibling_session = models.Session.objects.get(pk=self.sibling["sessionId"])
        self.assert_authority(self.loaders(branch_id=self.sibling["branchId"]),
            navigation=(404, "business_branch_not_found"), selected=False)
        self.assert_authority(self.loaders(agent=sibling_agent, session=sibling_session),
            navigation=(404, "business_branch_not_found"), selected=False)
        self.assert_authority(self.loaders(agent=self.root, session=self.root_session),
            navigation=(404, "business_branch_not_found"), selected=False)
        self.assert_http(404, ("business_branch_not_found", "presentation_scope_not_found"),
            token=self.token, branch_id=self.branch_id, agent=self.root, session=self.root_session)
        unbound = models.Session.objects.create(workspace=self.workspace, owner=self.member, agent=self.agent)
        self.assert_authority(self.loaders(session=unbound), navigation=True, selected=False)
        path = f"/api/workspaces/{self.workspace.pk}/presentation/events"
        response = Client().get(path, {"agentId": self.agent.pk, "sessionId": unbound.pk},
            HTTP_AUTHORIZATION="Bearer " + self.token, HTTP_X_CENTAERIS_BUSINESS_BRANCH_ID=self.branch_id)
        self.assertEqual(response.status_code, 404)
        self.assertEqual(response.json(), {"error": "presentation_scope_not_found"})

    def test_cached_branch_and_credential_recheck_rotation_with_new_token_allowed(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        rotated = self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
            {"expectedCredentialVersion": 1}, client=self.member_client)
        self.assertEqual(rotated.status_code, 200, rotated.content)
        self.assert_authority(loaders, navigation=(401, "delegation_invalid"), selected=False)
        self.assert_http(401, "delegation_invalid", token=self.token, branch_id=self.branch_id)
        token = rotated.json()["accessToken"]
        self.grant = authenticate_delegation(token, "sessions:read")
        self.assert_authority(self.loaders())
        self.assert_http(token=token, branch_id=self.branch_id)

    def test_member_rejoin_does_not_restore_the_branch_grant(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        self.membership.delete()
        self.assert_authority(loaders, navigation=False, selected=False)
        models.WorkspaceMembership.objects.create(workspace=self.workspace, user=self.member)
        self.assert_authority(loaders, navigation=(403, "delegation_not_available"), selected=False)
        self.assert_http(403, "delegation_not_available", token=self.token, branch_id=self.branch_id)

    def test_branch_root_agent_session_and_coordination_binding_lifecycle_are_current(self):
        loaders = self.loaders()
        for query, navigation in (
                (models.Agent.objects.filter(pk=self.root.pk), (403, "delegation_not_available")),
                (models.Agent.objects.filter(pk=self.agent.pk), False),
                (models.Session.objects.filter(pk=self.session.pk), (404, "business_branch_not_found"))):
            self.assert_authority(loaders)
            query.update(status="deleted", deletedAt=timezone.now())
            # The navigation list validates the Branch before evaluating any Sessions.
            self.assert_authority(loaders, navigation=navigation, selected=False)
            query.update(status="active", deletedAt=None)
        models.AgentCoordinationSession.objects.filter(agent=self.agent, session=self.session).delete()
        self.assert_authority(loaders, navigation=(404, "business_branch_not_found"), selected=False)

    def test_frozen_branch_id_ignores_later_original_request_header_and_cache_changes(self):
        loaders = self.loaders()
        self.assert_authority(loaders)
        sibling = models.BusinessAgentBranch.objects.get(pk=self.sibling["branchId"])
        for request, loader in loaders:
            request.user = self.other
            request.app_delegation_credential_version = 99
            request.business_branch_id = sibling.pk
            request.META["HTTP_X_CENTAERIS_BUSINESS_BRANCH_ID"] = sibling.pk
            loader.request.business_branch = sibling
        self.assert_authority(loaders)
        rows = async_to_sync(loaders[0][1].navigation)()
        self.assertEqual({row["id"] for row in rows}, {self.session.pk})
