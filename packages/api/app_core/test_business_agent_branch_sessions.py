"""Owner inspection is bounded by authoritative branch and work identities."""
from django.db import connection
from django.test import Client, TestCase
from django.test.utils import CaptureQueriesContext
from django.utils import timezone

from . import models
from .test_business_agent_branches import BusinessBranchFixture


class BusinessAgentBranchSessionTests(BusinessBranchFixture, TestCase):
    def setUp(self):
        super().setUp()
        self.a = self.resolve("A").json()
        self.b = self.resolve("B").json()
        self.sessions_url = self.branch_sessions_url(self.a)

    def branch_sessions_url(self, branch, *, root=None):
        return f"/api/agents/{root or self.root.pk}/business-branches/{branch['branchId']}/sessions"

    def inspect(self, query=None, *, url=None, client=None):
        return (client or self.member_client).get(url or self.sessions_url,
            query if query is not None else {"appId": self.application["id"]})

    def source_run(self, branch):
        return models.AgentRun.objects.create(workspace=self.workspace, user=self.member,
            session_id=branch["sessionId"], modelConfig=self.root.model_config,
            prompt="Synthetic source, never execute", status="queued")

    def work(self, branch, *, source=None, ordinal=None):
        fields = {} if ordinal is None else {"id": f"session_business_work_{ordinal:04d}"}
        session = models.Session.objects.create(workspace=self.workspace, owner=self.member,
            agent_id=branch["agentId"], title="Synthetic work 中文", origin="automation", **fields)
        child = models.AgentRun.objects.create(workspace=self.workspace, user=self.member,
            session=session, modelConfig=self.root.model_config,
            prompt="Synthetic child, never execute", status="completed")
        source = source or self.source_run(branch)
        receipt = models.HostedOperationReceipt.objects.create(user=self.member, workspace=self.workspace,
            command="submitMessage", operationId=session.pk, requestDigest="a" * 64,
            sessionId=session.pk, agentRunId=child.pk, turnId=child.turn_id)
        binding = models.AgentWorkSession.objects.create(session=session,
            coordination_session_id=branch["sessionId"], source_run=source,
            source_turn_id=source.turn_id, source_call_id=session.pk,
            source_event_id=session.pk, operation=receipt)
        return session, binding

    def other_branch(self, *, root=None, application=None):
        root = root or models.Agent.objects.create(workspace=self.workspace, owner=self.member,
            name="Another business root", model_config=self.root.model_config)
        agent = models.Agent.objects.create(workspace=self.workspace, owner=self.member,
            name="Another branch", model_config=self.root.model_config)
        session = models.Session.objects.create(workspace=self.workspace, owner=self.member, agent=agent)
        models.AgentCoordinationSession.objects.create(agent=agent, session=session)
        branch = models.BusinessAgentBranch.objects.create(root_agent=root, agent=agent,
            session=session, app_id=(application or self.application)["id"], business_user_id="Other tree")
        return {"branchId": branch.pk, "agentId": agent.pk, "sessionId": session.pk,
                "rootAgentId": root.pk}

    def test_owner_metadata_lists_only_authoritative_coordination_and_work(self):
        own, binding = self.work(self.a)
        sibling, _ = self.work(self.b)
        ordinary = models.Session.objects.create(workspace=self.workspace, owner=self.member,
            agent_id=self.a["agentId"], title="Ordinary session")
        root_ordinary = models.Session.objects.create(workspace=self.workspace, owner=self.member, agent=self.root)
        response = self.inspect()
        self.assertEqual(response.status_code, 200, response.content)
        coordination = models.Session.objects.get(pk=self.a["sessionId"])
        self.assertEqual(response.json(), {"branchId": self.a["branchId"],
            "coordinationSession": {"sessionId": coordination.pk, "title": coordination.title,
                "status": "active", "createdAt": coordination.createdAt.isoformat()},
            "workSessions": [{"sessionId": own.pk, "title": own.title, "status": "active",
                "sourceAgentRunId": binding.source_run_id, "createdAt": own.createdAt.isoformat()}],
            "nextAfterSessionId": None})
        self.assertEqual(response["Cache-Control"], "no-store")
        self.assertFalse({ordinary.pk, sibling.pk, root_ordinary.pk, self.root_session.pk}
            & {row["sessionId"] for row in response.json()["workSessions"]})
        self.assertNotIn("Synthetic child, never execute", response.content.decode())
        self.assertNotIn("Private root history", response.content.decode())

    def test_endpoint_requires_owner_cookie_and_current_membership(self):
        self.assertEqual(self.inspect(client=Client()).status_code, 401)
        self.assertEqual(self.api("get", self.sessions_url + "?appId=" + self.application["id"],
            branch=self.a["branchId"]).status_code, 401)
        mixed = self.member_client.get(self.sessions_url, {"appId": self.application["id"]},
            HTTP_AUTHORIZATION="Bearer " + self.token)
        self.assertEqual(mixed.status_code, 400)
        outsider = Client()
        outsider.force_login(self.other)
        self.assertEqual(self.inspect(client=outsider).status_code, 404)
        models.WorkspaceMembership.objects.filter(pk=self.membership.pk).delete()
        self.assertEqual(self.inspect().status_code, 404)

    def test_branch_namespace_rejects_wrong_root_app_and_branch(self):
        other_root = self.other_branch()
        other_app = self.app()
        other_application_branch = self.other_branch(root=self.root, application=other_app)
        cases = [self.branch_sessions_url(self.a, root=other_root["rootAgentId"]),
            self.branch_sessions_url(other_root), self.branch_sessions_url(other_application_branch),
            self.sessions_url.replace(self.a["branchId"], "business_branch_missing")]
        for url in cases:
            with self.subTest(url=url):
                response = self.inspect(url=url)
                self.assertEqual(response.status_code, 404, response.content)
                self.assertEqual(response.json(), {"error": "business_branch_not_found"})
        self.assertEqual(self.inspect({"appId": other_app["id"]}).status_code, 404)

    def test_queries_are_exact_single_valued_and_bounded(self):
        base = "?appId=" + self.application["id"]
        invalid = ["", "?appId=", base + "&unknown=x", base + "&appId=" + self.application["id"],
            "?appId=" + "x" * 65, base + "&afterSessionId=" + "x" * 65,
            base + "&afterSessionId=", base + "&afterSessionId=x&afterSessionId=y",
            base + "&limit=1&limit=2", base + "&after_session_id=x"]
        invalid.extend(base + "&limit=" + value for value in
            ("", "0", "101", "1.0", "-1", "true", "１２", "9" * 5000))
        for query in invalid:
            with self.subTest(query=query[:100]):
                response = self.member_client.get(self.sessions_url + query)
                self.assertEqual(response.status_code, 400, response.content)
                self.assertEqual(response.json(), {"error": "business_branch_request_invalid"})

    def test_work_pagination_has_default_and_maximum_and_no_loss(self):
        source = self.source_run(self.a)
        sessions = [self.work(self.a, source=source, ordinal=number)[0] for number in range(101)]
        first = self.inspect()
        self.assertEqual(first.status_code, 200, first.content)
        self.assertEqual(len(first.json()["workSessions"]), 50)
        self.assertEqual(first.json()["nextAfterSessionId"], sessions[49].pk)
        maximum = self.inspect({"appId": self.application["id"], "limit": "100"})
        self.assertEqual(maximum.status_code, 200, maximum.content)
        self.assertEqual(len(maximum.json()["workSessions"]), 100)
        self.assertEqual(maximum.json()["nextAfterSessionId"], sessions[99].pk)
        result, cursor = [], None
        for _ in range(4):
            query = {"appId": self.application["id"], "limit": "37"}
            if cursor is not None:
                query["afterSessionId"] = cursor
            response = self.inspect(query)
            self.assertEqual(response.status_code, 200, response.content)
            result.extend(row["sessionId"] for row in response.json()["workSessions"])
            cursor = response.json()["nextAfterSessionId"]
            if cursor is None:
                break
        else:
            self.fail("pagination did not finish within the expected bounded pages")
        self.assertEqual(result, [session.pk for session in sessions])
        self.assertEqual(len(set(result)), 101)

    def test_cursor_must_be_an_actual_work_session_of_the_same_tree(self):
        self.work(self.a)
        sibling, _ = self.work(self.b)
        ordinary = models.Session.objects.create(workspace=self.workspace, owner=self.member,
            agent_id=self.a["agentId"])
        other_root_work, _ = self.work(self.other_branch())
        other_app_work, _ = self.work(self.other_branch(root=self.root, application=self.app()))
        for cursor in (sibling.pk, ordinary.pk, self.a["sessionId"], self.root_session.pk,
                       other_root_work.pk, other_app_work.pk, "session_missing"):
            with self.subTest(cursor=cursor):
                response = self.inspect({"appId": self.application["id"], "afterSessionId": cursor})
                self.assertEqual(response.status_code, 400, response.content)

    def test_work_binding_must_match_source_run_and_branch_identity(self):
        work, binding = self.work(self.a)
        sibling_source = self.source_run(self.b)
        original_source = binding.source_run_id
        models.AgentWorkSession.objects.filter(pk=work.pk).update(source_run=sibling_source)
        response = self.inspect()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["workSessions"], [])
        models.AgentWorkSession.objects.filter(pk=work.pk).update(source_run_id=original_source)
        models.AgentRun.objects.filter(pk=original_source).update(user=self.other)
        self.assertEqual(self.inspect().json()["workSessions"], [])
        models.AgentRun.objects.filter(pk=original_source).update(user=self.member)
        other_workspace = models.Workspace.objects.create(name="Another workspace", createdBy=self.member)
        models.AgentRun.objects.filter(pk=original_source).update(workspace=other_workspace)
        self.assertEqual(self.inspect().json()["workSessions"], [])
        models.AgentRun.objects.filter(pk=original_source).update(workspace=self.workspace)
        models.Session.objects.filter(pk=work.pk).update(workspace=other_workspace)
        self.assertEqual(self.inspect().json()["workSessions"], [])
        models.Session.objects.filter(pk=work.pk).update(workspace=self.workspace)
        models.Session.objects.filter(pk=work.pk).update(agent_id=self.b["agentId"])
        self.assertEqual(self.inspect().json()["workSessions"], [])
        models.Session.objects.filter(pk=work.pk).update(agent_id=self.a["agentId"], owner=self.other)
        self.assertEqual(self.inspect().json()["workSessions"], [])

    def test_coordination_metadata_requires_matching_agent_owner_and_workspace(self):
        session = models.Session.objects.get(pk=self.a["sessionId"])
        other_workspace = models.Workspace.objects.create(name="Another workspace", createdBy=self.member)
        for invalid in ({"owner": self.other}, {"workspace": other_workspace}, {"agent_id": self.b["agentId"]}):
            with self.subTest(invalid=list(invalid)):
                models.Session.objects.filter(pk=session.pk).update(**invalid)
                response = self.inspect()
                self.assertEqual(response.status_code, 404, response.content)
                self.assertEqual(response.json(), {"error": "business_branch_not_found"})
                models.Session.objects.filter(pk=session.pk).update(owner=self.member,
                    workspace=self.workspace, agent_id=self.a["agentId"])
        ordinary = models.Session.objects.create(workspace=self.workspace, owner=self.member,
            agent_id=self.a["agentId"])
        models.AgentCoordinationSession.objects.filter(agent_id=self.a["agentId"]).update(session=ordinary)
        self.assertEqual(self.inspect().status_code, 404)
        self.assertEqual(models.AgentCoordinationSession.objects.get(agent_id=self.a["agentId"]).session_id, ordinary.pk)

    def test_coordination_requires_real_binding_and_never_repairs_it(self):
        before = models.Session.objects.count()
        models.AgentCoordinationSession.objects.filter(agent_id=self.a["agentId"]).delete()
        with CaptureQueriesContext(connection) as queries:
            response = self.inspect()
        self.assertEqual(response.status_code, 404, response.content)
        self.assertEqual(response.json(), {"error": "business_branch_not_found"})
        self.assertEqual(models.Session.objects.count(), before)
        self.assertFalse(models.AgentCoordinationSession.objects.filter(agent_id=self.a["agentId"]).exists())
        self.assertFalse(any(query["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for query in queries))

    def test_owner_can_inspect_retained_deleted_sessions_without_writes(self):
        work, _ = self.work(self.a)
        coordination = models.Session.objects.get(pk=self.a["sessionId"])
        for session in (work, coordination):
            session.status, session.deletedAt = "deleted", timezone.now()
            session.save()
        branch_agent = models.Agent.objects.get(pk=self.a["agentId"])
        branch_agent.status, branch_agent.deletedAt = "deleted", timezone.now()
        branch_agent.save()
        self.root.status, self.root.deletedAt = "deleted", timezone.now()
        self.root.save()
        with CaptureQueriesContext(connection) as queries:
            response = self.inspect()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["coordinationSession"]["status"], "deleted")
        self.assertEqual(response.json()["workSessions"][0]["status"], "deleted")
        self.assertFalse(any(query["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for query in queries))
