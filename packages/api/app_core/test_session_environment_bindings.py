"""Stable business environments without changing per-Run execution grants."""
from django.db import IntegrityError, connection, transaction
from django.test import TestCase, TransactionTestCase
from django.test.utils import CaptureQueriesContext
from unittest.mock import PropertyMock, patch

from .models import AgentRun, Session, SessionEnvironment, WorkspaceMembership
from .test_business_agent_branches import BusinessBranchFixture
from .test_execution_publication_fencing import ExecutionPublicationFixture
from .test_hosted_operations import OperationFixture


class SessionEnvironmentHttpTests(OperationFixture, TestCase):
    def environment(self, session_id):
        response = self.client.get(f"/api/sessions/{session_id}")
        self.assertEqual(response.status_code, 200, response.content)
        value = response.json()["session"]
        self.assertIn("currentEnvironmentId", value)
        self.assertEqual(value["executionMode"], "perRun")
        self.assertTrue(value["currentEnvironmentId"])
        return value["currentEnvironmentId"]

    def run_environment(self, session_id, run_id):
        return self.client.get(f"/api/sessions/{session_id}/agent-runs/{run_id}/environment")

    def test_created_session_and_successive_runs_share_one_persisted_environment(self):
        created = self.create()
        self.assertEqual(created.status_code, 201, created.content)
        session_id = created.json()["sessionId"]
        environment_id = self.environment(session_id)
        self.assertEqual(self.create().json(), created.json())
        url = self.create_url + f"/{session_id}/messages"
        first = self.post(url)
        self.assertEqual(first.status_code, 202, first.content)
        self.assertEqual(self.post(url).json(), first.json())
        first_run = AgentRun.objects.get(pk=first.json()["agentRunId"])
        signed = (first_run.authorization.payload, first_run.authorization.digest, first_run.authorization.signature)
        AgentRun.objects.filter(pk=first_run.pk).update(status="completed")
        second = self.post(url, operationId="submit-2")
        self.assertEqual(second.status_code, 202, second.content)
        self.assertNotEqual(second.json()["agentRunId"], first_run.pk)
        for run_id in (first_run.pk, second.json()["agentRunId"]):
            response = self.run_environment(session_id, run_id)
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json(), {"sessionId": session_id, "agentRunId": run_id,
                "environmentId": environment_id, "executionMode": "perRun"})
            self.assertEqual(AgentRun.objects.get(pk=run_id).environment_id, environment_id)
        first_run.refresh_from_db()
        self.assertEqual((first_run.authorization.payload, first_run.authorization.digest,
                          first_run.authorization.signature), signed)
        self.assertEqual(self.environment(session_id), environment_id)

    def test_new_message_allocates_a_distinct_environment_and_reads_do_not_write(self):
        first = self.post()
        second = self.post(operationId="other-submit")
        self.assertEqual(first.status_code, 202, first.content)
        self.assertEqual(second.status_code, 202, second.content)
        first_id, second_id = first.json()["sessionId"], second.json()["sessionId"]
        self.assertNotEqual(self.environment(first_id), self.environment(second_id))
        from .models import SessionEnvironment
        before = list(SessionEnvironment.objects.order_by("pk").values())
        self.environment(first_id)
        self.run_environment(first_id, first.json()["agentRunId"])
        self.assertEqual(list(SessionEnvironment.objects.order_by("pk").values()), before)

    def test_run_environment_is_scoped_to_the_authorized_session(self):
        first = self.post()
        second = self.post(operationId="other-submit")
        session_id, run_id = first.json()["sessionId"], first.json()["agentRunId"]
        self.assertEqual(self.run_environment(session_id, run_id).status_code, 200)
        self.assertEqual(self.run_environment(second.json()["sessionId"], run_id).status_code, 404)
        WorkspaceMembership.objects.filter(workspace=self.workspace, user=self.user).delete()
        self.assertEqual(self.run_environment(session_id, run_id).status_code, 404)
        self.assertEqual(AgentRun.objects.count(), 2)
        self.assertEqual(Session.objects.count(), 2)

    def test_deleted_session_and_unrelated_user_cannot_inspect_run_environment(self):
        from django.contrib.auth import get_user_model
        from django.utils import timezone
        accepted = self.post().json()
        session_id, run_id = accepted["sessionId"], accepted["agentRunId"]
        stranger = get_user_model().objects.create_user(username="environment-stranger")
        self.workspace.members.add(stranger)
        self.client.force_login(stranger)
        self.assertEqual(self.run_environment(session_id, run_id).status_code, 404)
        self.client.force_login(self.user)
        Session.objects.filter(pk=session_id).update(status="deleted", deletedAt=timezone.now())
        self.assertEqual(self.run_environment(session_id, run_id).status_code, 404)

    def test_caller_cannot_select_an_environment_in_message_input(self):
        created = self.create().json()
        environment_id = self.environment(created["sessionId"])
        response = self.post(environmentId=environment_id)
        self.assertEqual(response.status_code, 400, response.content)
        self.assertFalse(AgentRun.objects.exists())


class SessionEnvironmentPersistenceTests(OperationFixture, TestCase):
    def setUp(self):
        super().setUp()
        first, second = self.post(), self.post(operationId="other-submit")
        self.run = AgentRun.objects.get(pk=first.json()["agentRunId"])
        self.other = AgentRun.objects.get(pk=second.json()["agentRunId"])

    def test_environment_and_run_identity_reject_bulk_and_raw_rebinding(self):
        for model, identity, values in (
            (AgentRun, self.run.pk, {"environment_id": self.other.environment_id}),
            (AgentRun, self.run.pk, {"session_id": self.other.session_id}),
            (SessionEnvironment, self.run.environment_id, {"session_id": self.other.session_id}),
        ):
            for method in ("queryset", "bulk", "raw"):
                with self.subTest(model=model.__name__, method=method), self.assertRaises(IntegrityError):
                    with transaction.atomic():
                        if method == "queryset":
                            model.objects.filter(pk=identity).update(**values)
                        elif method == "bulk":
                            obj = model.objects.get(pk=identity)
                            for name, value in values.items():
                                setattr(obj, name, value)
                            model.objects.bulk_update([obj], list(values))
                        else:
                            field, value = next(iter(values.items()))
                            quote = connection.ops.quote_name
                            with connection.cursor() as cursor:
                                cursor.execute(f"UPDATE {quote(model._meta.db_table)} SET {quote(field)}=%s WHERE id=%s", [value, identity])
        self.run.refresh_from_db()
        self.assertEqual(self.run.environment.session_id, self.run.session_id)

    def test_session_current_environment_must_exist_and_belong_to_the_session_at_commit(self):
        for environment in (None, self.other.environment):
            with self.subTest(environment=environment), self.assertRaises(IntegrityError):
                with transaction.atomic():
                    Session.objects.filter(pk=self.run.session_id).update(current_environment=environment)
                    with connection.cursor() as cursor:
                        cursor.execute("SET CONSTRAINTS ALL IMMEDIATE")

    def test_model_creation_seam_binds_ordinary_automation_and_direct_runs(self):
        for origin in ("user", "automation"):
            session = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent, origin=origin)
            self.assertEqual(session.current_environment.session_id, session.pk)
            run = AgentRun.objects.create(workspace=self.workspace, user=self.user, session=session,
                modelConfig=self.model, prompt="model seam")
            self.assertEqual(run.environment_id, session.current_environment_id)
            self.assertEqual(SessionEnvironment.objects.filter(session=session).count(), 1)

    def test_bulk_creation_cannot_skip_or_select_a_foreign_binding(self):
        with self.assertRaises(IntegrityError), transaction.atomic():
            Session.objects.bulk_create([Session(workspace=self.workspace, owner=self.user, agent=self.agent)])
            with connection.cursor() as cursor:
                cursor.execute("SET CONSTRAINTS ALL IMMEDIATE")
        with self.assertRaises(IntegrityError), transaction.atomic():
            AgentRun.objects.bulk_create([AgentRun(workspace=self.workspace, user=self.user, session=self.run.session,
                modelConfig=self.model, prompt="foreign", membership_ref=self.run.membership_ref,
                environment=self.other.environment)])
        with self.assertRaisesRegex(ValueError, "environment_binding_invalid"):
            AgentRun.objects.create(workspace=self.workspace, user=self.user, session=self.run.session,
                modelConfig=self.model, prompt="foreign", environment=self.other.environment)

    def test_missing_binding_is_not_repaired_on_save_or_authorization(self):
        from .agent_run_authorization_factory import create_agent_run_authorization
        original = self.run.environment_id
        self.run.environment_id = None
        with self.assertRaisesRegex(ValueError, "binding_immutable"):
            self.run.save()
        with self.assertRaisesRegex(ValueError, "environment binding mismatch"):
            create_agent_run_authorization(self.run, image_digest="sha256:" + "a" * 64)
        self.run.refresh_from_db()
        self.assertEqual(self.run.environment_id, original)

    def test_failed_initial_environment_rolls_back_and_the_same_instance_can_retry(self):
        session = Session(workspace=self.workspace, owner=self.user, agent=self.agent)
        session_id = session.pk
        before = SessionEnvironment.objects.count()
        with patch.object(SessionEnvironment, "save", side_effect=RuntimeError("synthetic environment failure")):
            with self.assertRaisesRegex(RuntimeError, "synthetic environment failure"):
                session.save()
        self.assertFalse(Session.objects.filter(pk=session_id).exists())
        self.assertEqual(SessionEnvironment.objects.count(), before)
        session.save()
        self.assertEqual(session.pk, session_id)
        self.assertEqual(session.current_environment.session_id, session_id)
        self.assertEqual(SessionEnvironment.objects.count(), before + 1)

    def test_deferred_check_uses_final_state_and_preserves_historical_run_binding(self):
        session = self.run.session
        original = session.current_environment_id
        replacement = SessionEnvironment.objects.create(session=session)
        with transaction.atomic():
            Session.objects.filter(pk=session.pk).update(current_environment=None)
            Session.objects.filter(pk=session.pk).update(current_environment=replacement)
            Session.objects.filter(pk=session.pk).update(current_environment_id=original)
            with connection.cursor() as cursor:
                cursor.execute("SET CONSTRAINTS ALL IMMEDIATE")
                cursor.execute("SET CONSTRAINTS ALL DEFERRED")
        self.run.refresh_from_db()
        self.assertEqual(self.run.environment_id, original)
        with self.assertRaises(IntegrityError), transaction.atomic():
            SessionEnvironment.objects.filter(pk=original).delete()
        with self.assertRaises(IntegrityError), transaction.atomic():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM app_core_sessionenvironment WHERE id=%s", [original])
                cursor.execute("SET CONSTRAINTS ALL IMMEDIATE")

    def test_platform_credential_binding_adds_no_database_round_trip(self):
        from .platform_mcp_auth import authorize_context
        from .material_access import MaterialAccessContext
        from .material_identity import processing_spec_digest
        from .test_platform_mcp import processing_specification
        specification = processing_specification()
        context = MaterialAccessContext(self.run.pk, self.run.authorization.digest,
            specification, processing_spec_digest(specification))
        with patch("app_core.session_environments.agent_run_environment_matches", return_value=True):
            with CaptureQueriesContext(connection) as baseline:
                authorize_context(context)
        with CaptureQueriesContext(connection) as checked:
            authorized = authorize_context(context)
        self.assertEqual(authorized.agent_run.environment_id, self.run.environment_id)
        self.assertEqual(len(checked), len(baseline), "environment checking added an authorization round trip")

    def test_title_update_keeps_the_existing_scope_check_and_write_query_budget(self):
        session = self.run.session
        session.title = "A title update"
        with CaptureQueriesContext(connection) as queries:
            session.save(update_fields=["title"])
        self.assertLessEqual(len(queries), 2, "title update added a separate environment lookup")
        session.refresh_from_db()
        self.assertEqual(session.title, "A title update")

    def test_run_status_save_keeps_the_existing_identity_check_and_write_query_budget(self):
        _ = self.run.session
        self.run.status = "completed"
        with CaptureQueriesContext(connection) as queries:
            self.run.save(update_fields=["status"])
        self.assertLessEqual(len(queries), 3, "Run status save added a separate environment lookup")
        self.run.refresh_from_db()
        self.assertEqual(self.run.status, "completed")


class SessionEnvironmentDelegationTests(BusinessBranchFixture, TestCase):
    def test_delegation_cannot_read_sibling_environment_or_continue_after_revoke(self):
        first, second = self.resolve("first").json(), self.resolve("second").json()
        session = Session.objects.get(pk=first["sessionId"])
        run = AgentRun.objects.create(workspace=self.workspace, user=self.member, session=session,
            modelConfig=self.root.model_config, prompt="read binding")
        url = f"/api/sessions/{session.pk}/agent-runs/{run.pk}/environment"
        response = self.api("get", url, branch=first["branchId"])
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["environmentId"], session.current_environment_id)
        self.assertEqual(response["Cache-Control"], "no-store")
        self.assertEqual(self.api("get", url, branch=second["branchId"]).status_code, 404)
        self.membership.delete()
        self.assertEqual(self.api("get", url, branch=first["branchId"]).status_code, 403)


class SessionEnvironmentWorkspaceTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def test_snapshot_consumers_reject_an_environment_outside_the_signed_session(self):
        import json
        from django.conf import settings
        from .testing import create_session
        other = create_session(workspace=self.run.workspace, owner=self.run.user)
        body, _snapshot = self._body()
        with patch.object(AgentRun, "environment", new_callable=PropertyMock,
                          return_value=other.current_environment):
            responses = [self._post(body)]
            for operation in ("resolve", "download"):
                responses.append(self.client.post("/internal/agent-runs/session-workspace/" + operation,
                    json.dumps({"schema": f"runtime.session_workspace.{operation}.v1", "jobId": self.job_id,
                        "leaseOwner": self.owner, "agentRunId": self.run.pk,
                        "authorizationDigest": self.authorization.digest}), content_type="application/json",
                    HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN))
        for response in responses:
            self.assertEqual(response.status_code, 409, response.content)
            self.assertEqual(response.json(), {"error": "session_workspace_authorization_invalid"})
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertEqual(list(self.storage_root.rglob("*.snapshot")), [])
