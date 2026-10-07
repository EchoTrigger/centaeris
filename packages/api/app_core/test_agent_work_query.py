"""Read committed dispatch identity without creating or repairing hosted work."""
import json
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from django.conf import settings
from django.db import close_old_connections, connection, transaction
from django.test import Client, TransactionTestCase
from django.test.utils import CaptureQueriesContext
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, AgentWorkSession, HostedOperationReceipt, Session, SessionEvent
from . import test_agent_work


class AgentWorkQueryTests(TransactionTestCase):
    serialized_rollback = True
    setUp = test_agent_work.AgentWorkTests.setUp
    request_fact = test_agent_work.AgentWorkTests.request_fact
    dependencies = test_agent_work.AgentWorkTests.dependencies
    post = test_agent_work.AgentWorkTests.post

    def query(self, client=None, **changes):
        body = {"schema": "workspace.agent_work.query.v1", "agentRunId": self.run.id,
            "authorizationDigest": self.run.authorization.digest, "coordinationSessionId": self.source.id,
            "toolCallId": "query-one", "sourceAgentRunId": self.run.id, "sourceToolCallId": "dispatch-one"}
        body.update(changes)
        with CaptureQueriesContext(connection) as queries:
            response = (client or self.client).post("/internal/agent-work/query", data=json.dumps(body),
                content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertFalse(any(q["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for q in queries))
        if response.status_code == 200:
            self.assertTrue(any("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY" in q["sql"] for q in queries))
        return response

    def test_no_committed_success_is_not_recorded_and_call_only_retains_identity(self):
        response = self.query()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["status"], "notRecorded")
        self.assertIsNone(response.json()["source"])
        event = self.request_fact()
        event.delete()
        response = self.query()
        self.assertEqual(response.json()["status"], "notRecorded")
        self.assertEqual(response.json()["source"]["sourceTurnId"], self.run.turn_id)
        self.assertIsNone(response.json()["source"]["sourceEventId"])
        self.assertIsNone(response.json()["operation"])

    def test_uncommitted_source_is_invisible_then_committed_source_is_pending(self):
        with transaction.atomic():
            event = self.request_fact()
            def query_other_connection():
                close_old_connections()
                try:
                    return self.query(Client()).json()
                finally:
                    close_old_connections()
            with ThreadPoolExecutor(max_workers=1) as pool:
                result = pool.submit(query_other_connection).result(timeout=10)
            self.assertEqual(result["status"], "notRecorded")
        response = self.query()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["status"], "pending")
        self.assertEqual(response.json()["source"]["sourceEventId"], event.eventId)
        self.assertEqual(response.json()["source"]["sourceTurnId"], self.run.turn_id)
        self.assertTrue(response.json()["source"]["projectsToAgentRunStream"])
        self.assertIsNone(response.json()["operation"])
        self.assertEqual(Session.objects.count(), 1)

    def test_admission_and_terminal_child_status_are_separate_and_query_never_writes(self):
        self.dependencies()
        first = self.post(self.request_fact())
        self.assertEqual(first.status_code, 201, first.content)
        child = AgentRun.objects.get(id=first.json()["operation"]["agentRunId"])
        for status in ("queued", "completed", "failed", "cancelled"):
            AgentRun.objects.filter(pk=child.pk).update(status=status)
            with patch("app_core.agent_work.materialize_work_request", side_effect=AssertionError("no admission")), \
                 patch("app_core.agent_work._replay", side_effect=AssertionError("no repair")), \
                 patch("app_core.agent_work.request_execution_profile", side_effect=AssertionError("no profile")), \
                 patch("app_core.agent_work.schedule_agent_run_lifecycle", side_effect=AssertionError("no schedule")), \
                 CaptureQueriesContext(connection) as queries:
                response = self.query()
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json()["status"], "admitted")
            self.assertEqual(response.json()["operation"], first.json()["operation"])
            self.assertEqual(response.json()["work"], {"available": True, "agentRunStatus": status, "returns": [], "output": None})
            self.assertFalse(any(q["sql"].lstrip().upper().startswith(("INSERT", "UPDATE", "DELETE")) for q in queries))
        self.assertEqual(Session.objects.count(), 2)
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)

    def test_same_run_call_across_turns_is_ambiguous_even_with_one_admission(self):
        self.dependencies()
        self.assertEqual(self.post(self.request_fact()).status_code, 201)
        for record in list(self.source.events.order_by("sequence")):
            sequence = self.source.events.count() + 1
            wire = {**record.payload, "eventId": "other-turn:" + record.eventId,
                "turnId": "other-core-turn", "sequence": sequence}
            SessionEvent.objects.create(eventId=wire["eventId"], workspace=self.workspace, session=self.source,
                agent_run=self.run, sequence=sequence, agent_run_sequence=sequence, payload=wire,
                createdAtMs=sequence, projects_to_agent_run_stream=True)
        response = self.query()
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "agent_work_identity_ambiguous")

    def test_duplicate_success_uses_original_source_and_conflicting_input_fails(self):
        event = self.request_fact()
        self.request_fact()
        response = self.query()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["source"]["sourceEventId"], event.eventId)
        self.args = {**self.args, "objective": "Different objective"}
        self.request_fact()
        self.assertEqual(self.query().status_code, 409)

    def test_rewrite_projection_and_public_child_deletion_retain_admission(self):
        from .deleted_resource_gc import expire_trash
        self.dependencies()
        event = self.request_fact()
        first = self.post(event)
        self.assertEqual(first.status_code, 201)
        # Reproduce Runtime rewrite's retained-row projection update, rather
        # than invoking the full Core rewrite planner in this API fixture.
        self.source.events.update(projects_to_agent_run_stream=False)
        child = AgentRun.objects.get(id=first.json()["operation"]["agentRunId"])
        AgentRun.objects.filter(pk=child.pk).update(status="completed")
        self.client.force_login(self.user)
        url = "/api/sessions/" + child.session_id
        self.assertEqual(self.client.delete(url).status_code, 200)
        child.session.refresh_from_db()
        self.assertIsNotNone(child.session.purgedAt, "public Session deletion retains a permanent tombstone")
        expire_trash(timezone.now(), False)
        response = self.query()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["status"], "admitted")
        self.assertEqual(response.json()["operation"], first.json()["operation"])
        self.assertEqual(response.json()["work"], {"available": False, "agentRunStatus": None, "returns": [], "output": None})
        self.assertFalse(response.json()["source"]["projectsToAgentRunStream"])
        self.assertTrue(SessionEvent.objects.filter(pk=event.pk).exists())
        self.assertEqual(AgentWorkSession.objects.count(), 1)

    def test_rewritten_unadmitted_success_remains_a_recorded_request(self):
        event = self.request_fact()
        self.source.events.update(projects_to_agent_run_stream=False)
        response = self.query()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["status"], "pending")
        self.assertEqual(response.json()["source"]["sourceEventId"], event.eventId)
        self.assertFalse(response.json()["source"]["projectsToAgentRunStream"])

    def test_receipt_lookup_does_not_repair_binding_or_poison_an_unrelated_locator(self):
        self.dependencies()
        first = self.post(self.request_fact())
        AgentWorkSession.objects.get().delete()
        response = self.query()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["operation"], first.json()["operation"])
        self.assertFalse(AgentWorkSession.objects.exists())
        response = self.query(sourceToolCallId="never-submitted")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["status"], "notRecorded")
        self.assertIsNone(response.json()["source"])

    def test_current_caller_source_and_child_permissions_are_required(self):
        self.dependencies()
        first = self.post(self.request_fact())
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        caller = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
            modelConfig=self.model, prompt="Query historical source", status="running")
        create_agent_run_authorization(caller, image_digest=test_agent_work.PROFILE["imageDigest"])
        changes = {"agentRunId": caller.id, "authorizationDigest": caller.authorization.digest}
        self.assertEqual(self.query(**changes).status_code, 200)
        from django.contrib.auth import get_user_model
        foreign = get_user_model().objects.create_user(username="other-work-owner")
        Session.objects.filter(pk=first.json()["operation"]["sessionId"]).update(owner=foreign)
        self.assertEqual(self.query(**changes).status_code, 403)
        self.membership.delete()
        self.assertEqual(self.query(**changes).status_code, 403)

    def test_untrusted_source_and_strict_transport_fail_loudly(self):
        self.request_fact(call_changes={"toolContractDigest": "sha256:" + "f" * 64})
        self.assertEqual(self.query().status_code, 409)
        for changes in ({"source_turn_id": "invented"}, {"sourceToolCallId": "/path"},
                        {"sourceAgentRunId": 1}, {"sourceTurnId": "invented"}):
            self.assertEqual(self.query(**changes).status_code, 400)
        self.assertEqual(self.query(authorizationDigest="sha256:" + "f" * 64).status_code, 403)
        self.assertEqual(self.query(sourceAgentRunId="unknown").status_code, 403)
        self.assertEqual(self.client.post("/internal/agent-work/query", data="{}",
            content_type="application/json").status_code, 401)

    def test_failed_result_is_not_recorded_but_incomplete_or_unknown_result_fails(self):
        for index, state in enumerate(("failed", None, "unknown-state")):
            call_id = f"dispatch-result-{index}"
            self.request_fact(call_id=call_id, result_changes={"resultState": state})
            response = self.query(sourceToolCallId=call_id)
            if state == "failed":
                self.assertEqual(response.status_code, 200, response.content)
                self.assertEqual(response.json()["status"], "notRecorded")
                self.assertIsNone(response.json()["source"]["sourceEventId"])
            else:
                self.assertEqual(response.status_code, 409, response.content)
                self.assertEqual(response.json()["error"], "agent_work_request_untrusted")

    def test_ordinary_managed_and_delegated_callers_cannot_query_native_work(self):
        from datetime import timedelta
        from .models import Agent, AgentDefinition, AgentDefinitionVersion, BusinessApplication, UserAppDelegation
        ordinary = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        run = AgentRun.objects.create(workspace=self.workspace, session=ordinary, user=self.user,
            modelConfig=self.model, prompt="Ordinary query", status="running")
        create_agent_run_authorization(run, image_digest=test_agent_work.PROFILE["imageDigest"])
        self.assertEqual(self.query(agentRunId=run.id, authorizationDigest=run.authorization.digest,
            coordinationSessionId=ordinary.id).status_code, 403)
        definition = AgentDefinition.objects.create(workspace=self.workspace, created_by=self.user,
            name="Managed query", availability_scope="workspace")
        version = AgentDefinitionVersion.objects.create(definition=definition, version=1,
            name=definition.name, published_by=self.user)
        definition.published_version = version
        definition.save(update_fields=["published_version"])
        agent = Agent.objects.create(workspace=self.workspace, owner=self.user, definition=definition, name=version.name)
        session = Session.objects.create(workspace=self.workspace, owner=self.user, agent=agent)
        from .models import AgentCoordinationSession
        AgentCoordinationSession.objects.create(agent=agent, session=session)
        app = BusinessApplication.objects.create(name="Query app", created_by=self.user, status="active")
        grant = UserAppDelegation.objects.create(user=self.user, app=app, workspace=self.workspace,
            definition=definition, membership_ref=self.membership.id,
            scopes=["assistant:use", "messages:submit", "sessions:read"], token_digest="sha256:" + "b" * 64,
            expires_at=timezone.now() + timedelta(hours=1))
        for fields in ({}, {"acting_app": app, "app_delegation": grant}):
            run = AgentRun.objects.create(workspace=self.workspace, session=session, user=self.user,
                modelConfig=self.model, definition_version=version, prompt="Managed query", status="running", **fields)
            create_agent_run_authorization(run, image_digest=test_agent_work.PROFILE["imageDigest"])
            self.assertEqual(self.query(agentRunId=run.id, authorizationDigest=run.authorization.digest,
                coordinationSessionId=session.id).status_code, 403)
