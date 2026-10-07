"""One trusted application's external subject owns one durable isolated branch."""
import json
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from unittest.mock import patch

from django.db import close_old_connections, connection, transaction
from django.test import Client, TestCase, TransactionTestCase
from django.test.utils import CaptureQueriesContext

from . import models
from .app_delegations import authenticate_delegation, require_request_delegation
from .test_app_delegations import AppDelegationFixture


SCOPES = ["assistant:use", "messages:submit", "sessions:read", "artifacts:read"]
BRANCH_HEADER = "HTTP_X_CENTAERIS_BUSINESS_BRANCH_ID"


class BusinessBranchFixture(AppDelegationFixture):
    def setUp(self):
        super().setUp()
        self.root = models.Agent.objects.create(workspace=self.workspace, owner=self.member,
            name="Business root", description="Maintained policy", instructions="Exact policy\n中文",
            avatar_kind="banana", model_config=models.ModelConfig.objects.create(displayName="Fixture"))
        self.root_session = models.Session.objects.create(workspace=self.workspace, owner=self.member, agent=self.root)
        models.AgentCoordinationSession.objects.create(agent=self.root, session=self.root_session)
        models.AgentInput.objects.create(agent=self.root, session=self.root_session, input_id="private-root-input",
            membership_ref=self.membership.pk, sequence=1, accepted_source_sequence=0,
            body="Private root history", created_at_ms=1)
        self.application = self.app()
        self.grant, self.token = self.issue_grant(self.application)
        self.bearer = Client()
        self.resolve_url = f"/api/agents/{self.root.pk}/business-branches/resolve"
        self.list_url = f"/api/agents/{self.root.pk}/business-branches"

    def issue_grant(self, application):
        issued = self.send("post", "/api/account/app-delegations", {
            "appId": application["id"], "workspaceId": self.workspace.pk, "agentId": self.root.pk,
            "scopes": SCOPES, "expiresInSeconds": None}, client=self.member_client)
        self.assertEqual(issued.status_code, 201, issued.content)
        return models.UserAppDelegation.objects.get(pk=issued.json()["delegation"]["id"]), issued.json()["accessToken"]

    def api(self, method, path, payload=None, *, token=None, branch=None):
        kwargs = {"HTTP_AUTHORIZATION": "Bearer " + (token or self.token)}
        if payload is not None:
            kwargs.update(data=json.dumps(payload), content_type="application/json")
        if branch is not None:
            kwargs[BRANCH_HEADER] = branch
        return getattr(self.bearer, method)(path, **kwargs)

    def resolve(self, subject="external-user", **kwargs):
        return self.api("post", self.resolve_url, {"businessUserId": subject}, **kwargs)


class BusinessAgentBranchTests(BusinessBranchFixture, TestCase):
    def test_resolve_creates_one_fresh_branch_without_root_history_or_new_owner(self):
        run = models.AgentRun.objects.create(workspace=self.workspace, user=self.member,
            session=self.root_session, modelConfig=self.root.model_config, prompt="Private root fixture", status="completed")
        root_snapshot = {"workspaceGeneration": 1, "workspaceStorageKey": "synthetic/root/snapshot.tar",
            "workspaceSnapshotSha256": "sha256:" + "b" * 64, "workspaceSnapshotSizeBytes": 8192,
            "workspaceExpandedSizeBytes": 16384, "workspaceFileCount": 3, "workspaceLastAdvancedAgentRun_id": run.pk}
        for field, value in root_snapshot.items():
            setattr(self.root_session, field, value)
        self.root_session.save()
        before_agent_count = models.Agent.objects.count()
        first = self.resolve(" 用户 A ")
        self.assertEqual(first.status_code, 201, first.content)
        branch = first.json()
        self.assertEqual(set(branch), {"schema", "branchId", "rootAgentId", "businessUserId", "agentId", "sessionId"})
        self.assertEqual((branch["schema"], branch["rootAgentId"], branch["businessUserId"]),
                         ("agent.business_branch.v1", self.root.pk, " 用户 A "))
        self.assertNotEqual(branch["agentId"], self.root.pk)
        agent = models.Agent.objects.get(pk=branch["agentId"])
        session = models.Session.objects.get(pk=branch["sessionId"])
        self.assertEqual((agent.owner_id, agent.workspace_id, agent.definition_id), (self.member.pk, self.workspace.pk, None))
        for field in ("name", "description", "instructions", "avatar_kind", "model_config_id", "thinking_mode"):
            self.assertEqual(getattr(agent, field), getattr(self.root, field))
        self.assertEqual(models.AgentCoordinationSession.objects.get(agent=agent).session_id, session.pk)
        self.assertFalse(agent.inputs.exists())
        self.assertFalse(session.events.exists())
        self.assertEqual((session.workspaceGeneration, session.workspaceStorageKey, session.workspaceFileCount), (0, "", 0))
        self.assertEqual((session.workspaceSnapshotSha256, session.workspaceSnapshotSizeBytes,
            session.workspaceExpandedSizeBytes, session.workspaceLastAdvancedAgentRun_id), ("", 0, 0, None))
        self.root_session.refresh_from_db()
        self.assertEqual({field: getattr(self.root_session, field) for field in root_snapshot}, root_snapshot)
        again = self.resolve(" 用户 A ")
        self.assertEqual(again.status_code, 200, again.content)
        self.assertEqual(again.json(), branch)
        self.assertEqual(models.Agent.objects.count(), before_agent_count + 1)
        self.assertEqual(self.root.inputs.get().body, "Private root history")

    def test_subject_is_exact_case_sensitive_unicode_and_not_a_caller_selected_resource(self):
        records = [self.resolve(subject) for subject in ("USER", "user", "用户", "é", "e\u0301", " user ")]
        self.assertTrue(all(response.status_code == 201 for response in records), [response.content for response in records])
        self.assertEqual(len({response.json()["branchId"] for response in records}), 6)
        self.assertEqual([response.json()["businessUserId"] for response in records], ["USER", "user", "用户", "é", "e\u0301", " user "])
        for body in ({}, {"businessUserId": ""}, {"businessUserId": " \t "}, {"businessUserId": "x\0y"},
                     {"businessUserId": "x\ny"}, {"businessUserId": "x" * 257}, {"businessUserId": 1},
                     {"businessUserId": True}, {"businessUserId": "\ud800"},
                     {"business_user_id": "alias"}, {"businessUserId": "new", "agentId": self.root.pk}):
            denied = self.api("post", self.resolve_url, body)
            self.assertEqual(denied.status_code, 400, denied.content)
            self.assertEqual(denied.json(), {"error": "business_branch_request_invalid"})

    def test_resolve_requires_native_bearer_and_exact_root_and_forbids_branch_header(self):
        cookie = self.send("post", self.resolve_url, {"businessUserId": "user"}, client=self.member_client)
        self.assertEqual(cookie.status_code, 403, cookie.content)
        root_other = models.Agent.objects.create(workspace=self.workspace, owner=self.member, name="Other root")
        wrong = self.api("post", f"/api/agents/{root_other.pk}/business-branches/resolve", {"businessUserId": "user"})
        self.assertEqual(wrong.status_code, 404, wrong.content)
        existing = self.resolve().json()
        denied = self.resolve("another", branch=existing["branchId"])
        self.assertEqual(denied.status_code, 400, denied.content)
        self.assertEqual(denied.json(), {"error": "business_branch_header_invalid"})
        _, _, issued = self.delegation()
        managed = self.resolve(token=issued.json()["accessToken"])
        self.assertEqual(managed.status_code, 403, managed.content)

    def test_native_usage_requires_branch_and_never_exposes_root_or_another_subject(self):
        a = self.resolve("A").json()
        b = self.resolve("B").json()
        root_history = f"/api/agents/{self.root.pk}/inputs"
        missing = self.api("get", root_history)
        self.assertEqual(missing.status_code, 400, missing.content)
        self.assertEqual(missing.json(), {"error": "business_branch_required"})
        for path in (root_history, f"/api/agents/{b['agentId']}/inputs", f"/api/sessions/{b['sessionId']}"):
            response = self.api("get", path, branch=a["branchId"])
            self.assertEqual(response.status_code, 404, response.content)
        allowed = self.api("get", f"/api/agents/{a['agentId']}/inputs", branch=a["branchId"])
        self.assertEqual(allowed.status_code, 200, allowed.content)
        self.assertEqual(allowed.json()["inputs"], [])
        self.assertNotIn(b"Private root history", allowed.content)

    def test_app_namespace_and_reissued_grant_share_only_their_own_existing_branch(self):
        original = self.resolve("A").json()
        _, new_token = self.issue_grant(self.application)
        self.assertEqual(self.resolve("A", token=new_token).json(), original)
        other_app = self.app()
        _, other_token = self.issue_grant(other_app)
        other = self.resolve("A", token=other_token)
        self.assertEqual(other.status_code, 201, other.content)
        self.assertNotEqual(other.json()["branchId"], original["branchId"])
        denied = self.api("get", f"/api/agents/{original['agentId']}/inputs",
            token=other_token, branch=original["branchId"])
        self.assertEqual(denied.status_code, 404, denied.content)
        self.assertEqual(denied.json(), {"error": "business_branch_not_found"})

    def test_frozen_request_branch_cannot_be_reselected_from_changed_header_before_acceptance(self):
        a = self.resolve("A").json()
        b = self.resolve("B").json()
        from .agent_inputs import accept_agent_input
        def changing_header(*args, **kwargs):
            request = kwargs["request"]
            request.META[BRANCH_HEADER] = b["branchId"]
            request.__dict__.pop("headers", None)
            self.assertEqual(request.headers["X-Centaeris-Business-Branch-Id"], b["branchId"])
            return accept_agent_input(*args, **kwargs)
        with patch("app_core.http.agent_inputs.accept_agent_input", side_effect=changing_header), \
             patch("app_core.agent_inputs._dispatch_after_commit"):
            response = self.api("post", f"/api/agents/{a['agentId']}/inputs",
                {"schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "A-input", "body": "Only A"}, branch=a["branchId"])
        self.assertEqual(response.status_code, 201, response.content)
        fact = models.AgentInput.objects.get(input_id="A-input")
        self.assertEqual((fact.agent_id, fact.session_id, fact.acting_app_id, fact.app_delegation_id, fact.credential_version),
                         (a["agentId"], a["sessionId"], self.application["id"], self.grant.pk, 1))
        self.assertFalse(models.AgentInput.objects.filter(agent_id=b["agentId"]).exists())

    def test_owner_maintenance_list_is_read_only_scoped_and_keeps_deleted_branch_identity(self):
        a = self.resolve("A").json()
        agent = models.Agent.objects.get(pk=a["agentId"])
        models.Session.objects.create(workspace=self.workspace, owner=self.member, agent=agent)
        with CaptureQueriesContext(connection) as queries:
            response = self.member_client.get(self.list_url, {"appId": self.application["id"]})
        self.assertEqual(response.status_code, 200, response.content)
        row = response.json()["branches"][0]
        self.assertEqual(set(row), {"branchId", "rootAgentId", "businessUserId", "agentId", "sessionId", "status", "sessionCount", "createdAt"})
        self.assertEqual((row["branchId"], row["status"], row["sessionCount"]), (a["branchId"], "active", 2))
        self.assertFalse(any(query["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for query in queries))
        self.assertEqual(self.api("get", self.list_url + "?appId=" + self.application["id"]).status_code, 401)
        for query in ("", "?appId=x&extra=1", "?appId=x&appId=y"):
            self.assertEqual(self.member_client.get(self.list_url + query).status_code, 400)
        self.assertEqual(self.platform_client.get(self.list_url, {"appId": self.application["id"]}).status_code, 404)
        from django.utils import timezone
        agent.status, agent.deletedAt = "deleted", timezone.now()
        agent.save()
        deleted = self.member_client.get(self.list_url, {"appId": self.application["id"]}).json()["branches"][0]
        self.assertEqual((deleted["branchId"], deleted["agentId"], deleted["sessionId"], deleted["status"]),
                         (a["branchId"], a["agentId"], a["sessionId"], "deleted"))
        self.assertEqual(self.resolve("A").status_code, 410)

    def test_mapping_is_immutable_and_branch_cannot_be_a_new_delegation_root(self):
        branch = self.resolve().json()
        record = models.BusinessAgentBranch.objects.get(pk=branch["branchId"])
        record.business_user_id = "different"
        with self.assertRaisesMessage(ValueError, "business_agent_branch_is_immutable"):
            record.save()
        response = self.send("post", "/api/account/app-delegations", {
            "appId": self.application["id"], "workspaceId": self.workspace.pk, "agentId": branch["agentId"],
            "scopes": SCOPES, "expiresInSeconds": None}, client=self.member_client)
        self.assertEqual(response.status_code, 404, response.content)

    def test_mapping_rejects_queryset_mutations_and_instance_deletion(self):
        branch = self.resolve().json()
        record = models.BusinessAgentBranch.objects.get(pk=branch["branchId"])
        record.business_user_id = "different"
        mutations = [
            lambda: models.BusinessAgentBranch.objects.filter(pk=record.pk).update(business_user_id="different"),
            lambda: models.BusinessAgentBranch.objects.bulk_update([record], ["business_user_id"]),
            lambda: models.BusinessAgentBranch.objects.filter(pk=record.pk).delete(),
            lambda: record.delete(),
        ]
        for mutate in mutations:
            with self.subTest(mutation=mutate), transaction.atomic():
                with self.assertRaisesMessage(ValueError, "business_agent_branch_is_immutable"):
                    mutate()

    def test_external_identity_responses_are_not_cacheable(self):
        for response in (self.resolve("A"), self.resolve("A"),
                         self.member_client.get(self.list_url, {"appId": self.application["id"]})):
            self.assertEqual(response["Cache-Control"], "no-store")

    def test_persistent_subject_survives_ten_years_rotation_revocation_and_reissued_grant(self):
        from datetime import timedelta
        from django.utils import timezone
        original = self.resolve("A").json()
        with patch("app_core.app_delegations.timezone.now", return_value=timezone.now() + timedelta(days=3650)):
            self.assertEqual(self.resolve("A").json(), original)
        with patch("app_core.agent_inputs._dispatch_after_commit"):
            accepted = self.api("post", f"/api/agents/{original['agentId']}/inputs", {
                "schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "first-input", "body": "Stable history"}, branch=original["branchId"])
        self.assertEqual(accepted.status_code, 201, accepted.content)
        rotated = self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
            {"expectedCredentialVersion": 1}, client=self.member_client)
        self.assertEqual(rotated.status_code, 200, rotated.content)
        old_token, self.token = self.token, rotated.json()["accessToken"]
        self.assertEqual(self.resolve("A", token=old_token).status_code, 401)
        self.assertEqual(self.resolve("A").json(), original)
        self.member_client.delete(f"/api/account/app-delegations/{self.grant.pk}")
        self.assertEqual(self.resolve("A").status_code, 403)
        _, reissued = self.issue_grant(self.application)
        self.assertEqual(self.resolve("A", token=reissued).json(), original)
        with patch("app_core.agent_inputs._dispatch_after_commit"):
            replay = self.api("post", f"/api/agents/{original['agentId']}/inputs", {
                "schema": "agent.input.submit.v1", "attachmentRefs": [], "inputId": "first-input", "body": "Stable history"},
                branch=original["branchId"], token=reissued)
        self.assertEqual(replay.status_code, 403, replay.content)
        input_fact = models.AgentInput.objects.get(input_id="first-input")
        self.assertEqual((input_fact.body, input_fact.app_delegation_id, input_fact.credential_version),
                         ("Stable history", self.grant.pk, 1))

    def test_maintenance_keyset_pages_are_bounded_strict_and_do_not_drop_rows(self):
        created = {self.resolve(str(index)).json()["branchId"] for index in range(5)}
        seen, after = [], None
        for _ in range(3):
            query = {"appId": self.application["id"], "limit": 2}
            if after:
                query["afterBranchId"] = after
            response = self.member_client.get(self.list_url, query)
            self.assertEqual(response.status_code, 200, response.content)
            data = response.json()
            self.assertEqual(set(data), {"branches", "nextAfterId"})
            self.assertLessEqual(len(data["branches"]), 2)
            seen.extend(row["branchId"] for row in data["branches"])
            after = data["nextAfterId"]
        self.assertIsNone(after)
        self.assertEqual(set(seen), created)
        self.assertEqual(len(seen), len(created))
        for query in ({"limit": 0}, {"limit": 201}, {"limit": "1.0"}, {"limit": "9" * 5000},
                      {"afterBranchId": "other-namespace"}, {"after_branch_id": seen[0]}):
            denied = self.member_client.get(self.list_url, {"appId": self.application["id"], **query})
            self.assertEqual(denied.status_code, 400, denied.content)

    def test_resolve_rechecks_rotation_and_revocation_after_authentication_before_allocating(self):
        from .business_agent_branches import resolve_business_branch
        def rotate_before_resolve(*args, **kwargs):
            rotated = self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
                {"expectedCredentialVersion": 1}, client=self.member_client)
            self.assertEqual(rotated.status_code, 200, rotated.content)
            self.token = rotated.json()["accessToken"]
            return resolve_business_branch(*args, **kwargs)
        with patch("app_core.http.business_agent_branches.resolve_business_branch", side_effect=rotate_before_resolve):
            self.assertEqual(self.resolve().status_code, 401)
        def revoke_before_resolve(*args, **kwargs):
            self.member_client.delete(f"/api/account/app-delegations/{self.grant.pk}")
            return resolve_business_branch(*args, **kwargs)
        with patch("app_core.http.business_agent_branches.resolve_business_branch", side_effect=revoke_before_resolve):
            self.assertEqual(self.resolve().status_code, 403)
        self.assertFalse(models.BusinessAgentBranch.objects.exists())

    def test_session_authority_rechecks_bound_subject_and_credential_version(self):
        from .app_delegations import session_authority_is_current
        a, b = self.resolve("A").json(), self.resolve("B").json()
        args = (self.member.pk, a["sessionId"], self.grant.pk, "sessions:read")
        self.assertTrue(session_authority_is_current(*args, credential_version=1, business_branch_id=a["branchId"]))
        self.assertFalse(session_authority_is_current(*args, credential_version=1, business_branch_id=b["branchId"]))
        self.assertFalse(session_authority_is_current(*args, credential_version=1))
        self.send("post", f"/api/account/app-delegations/{self.grant.pk}/rotate",
            {"expectedCredentialVersion": 1}, client=self.member_client)
        self.assertFalse(session_authority_is_current(*args, credential_version=1, business_branch_id=a["branchId"]))
        self.assertTrue(session_authority_is_current(*args, credential_version=2, business_branch_id=a["branchId"]))

    def test_workspace_session_list_exposes_only_bound_coordination_and_actual_work(self):
        a, b = self.resolve("A").json(), self.resolve("B").json()
        def work_for(branch, operation_id):
            agent = models.Agent.objects.get(pk=branch["agentId"])
            coordination = models.Session.objects.get(pk=branch["sessionId"])
            source = models.AgentRun.objects.create(workspace=self.workspace, user=self.member,
                session=coordination, modelConfig=self.root.model_config, prompt="Synthetic source")
            work = models.Session.objects.create(workspace=self.workspace, owner=self.member, agent=agent, origin="automation")
            child = models.AgentRun.objects.create(workspace=self.workspace, user=self.member,
                session=work, modelConfig=self.root.model_config, prompt="Synthetic work")
            receipt = models.HostedOperationReceipt.objects.create(user=self.member, workspace=self.workspace,
                command="submitMessage", operationId=operation_id, requestDigest="a" * 64,
                sessionId=work.pk, agentRunId=child.pk, turnId=child.turn_id)
            models.AgentWorkSession.objects.create(session=work, coordination_session=coordination,
                source_run=source, source_turn_id=source.turn_id, source_call_id=operation_id,
                source_event_id=operation_id, operation=receipt)
            return work
        own_work = work_for(a, "synthetic-work-A")
        sibling_work = work_for(b, "synthetic-work-B")
        root_ordinary = models.Session.objects.create(workspace=self.workspace, owner=self.member, agent=self.root)
        own_ordinary = models.Session.objects.create(workspace=self.workspace, owner=self.member,
            agent_id=a["agentId"])
        url = self.base + "/sessions"
        for query in ("", "?agentId=" + a["agentId"]):
            response = self.api("get", url + query, branch=a["branchId"])
            self.assertEqual(response.status_code, 200, response.content)
            ids = {row["id"] for row in response.json()["sessions"]}
            self.assertEqual(ids, {a["sessionId"], own_work.pk})
            self.assertFalse(ids & {self.root_session.pk, root_ordinary.pk, own_ordinary.pk, sibling_work.pk, b["sessionId"]})
        self.assertEqual(self.api("get", url + "?agentId=" + b["agentId"], branch=a["branchId"]).status_code, 404)
        self.assertEqual(self.api("get", url + "?agentId=", branch=a["branchId"]).status_code, 400)
        self.assertEqual(self.api("get", url).status_code, 400)
        browser = self.member_client.get(url)
        self.assertEqual(browser.status_code, 200, browser.content)
        self.assertIn(root_ordinary.pk, {row["id"] for row in browser.json()["sessions"]})


class BusinessBranchConcurrencyTests(BusinessBranchFixture, TransactionTestCase):
    serialized_rollback = True

    def test_concurrent_resolve_allocates_one_agent_and_one_coordination_identity(self):
        barrier = Barrier(2)
        before = models.Agent.objects.count()
        def resolve():
            close_old_connections()
            try:
                client = Client()
                barrier.wait(timeout=10)
                return client.post(self.resolve_url, json.dumps({"businessUserId": "same-user"}),
                    content_type="application/json", HTTP_AUTHORIZATION="Bearer " + self.token)
            finally:
                close_old_connections()
        with ThreadPoolExecutor(max_workers=2) as pool:
            responses = list(pool.map(lambda _: resolve(), range(2)))
        self.assertEqual(sorted(response.status_code for response in responses), [200, 201])
        self.assertEqual(responses[0].json(), responses[1].json())
        self.assertEqual(models.Agent.objects.count(), before + 1)
        self.assertEqual(models.BusinessAgentBranch.objects.count(), 1)
