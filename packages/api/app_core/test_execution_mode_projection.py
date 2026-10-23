"""Public execution modes project each immutable Environment profile, without dispatch."""
from datetime import datetime, timezone
import os
from types import SimpleNamespace
from unittest.mock import patch

from django.db import connection
from django.test import SimpleTestCase, TestCase, TransactionTestCase
from django.test.utils import CaptureQueriesContext
from django.utils import timezone as django_timezone

from .environment_profile import environment_profile_digest
from .http.serialization import serialize_session
from .models import Agent, AgentRun, Session, SessionEnvironment
from . import test_agent_work_presentation as presentation
from .test_e2b_environment_profile import profile as e2b_profile
from .test_hosted_operations import OperationFixture
from .test_unified_environment_profile import profile as legacy_profile


class ExecutionModeProjectionContractTests(SimpleTestCase):
    def session_payload(self, profile, *, digest=None):
        environment = SimpleNamespace(
            environment_profile=profile,
            environment_profile_digest=(
                digest if digest is not None else environment_profile_digest(profile) if profile is not None else ""
            ),
        )
        session = SimpleNamespace(
            id="session-current", workspace_id="workspace-owned", agent_id="agent-owned",
            project_id=None, current_environment_id="environment-current",
            current_environment=environment, title="Session", origin="user", status="active",
            deletedAt=None, isPinned=False, isUnread=False,
            updatedAt=datetime(2026, 10, 23, tzinfo=timezone.utc),
        )
        with patch("app_core.agent_work_presentation.initial_input_origin", return_value=None):
            return serialize_session(session, has_active_agent_run=False)

    def test_retained_projection_uses_frozen_profile_instead_of_process_default(self):
        selected = e2b_profile() | {"lifetime": "retained"}
        with patch.dict(os.environ, {"AGENT_RUN_EXECUTION_MODE": "perRun"}):
            self.assertEqual(self.session_payload(selected)["executionMode"], "retained")

    def test_unbound_legacy_and_per_run_profiles_keep_default_without_worker_override(self):
        with patch.dict(os.environ, {"AGENT_RUN_EXECUTION_MODE": "retained"}):
            for selected in (None, legacy_profile(), e2b_profile()):
                with self.subTest(profile=selected):
                    self.assertEqual(self.session_payload(selected)["executionMode"], "perRun")

    def test_unknown_corrupt_or_unverified_profile_is_not_projected_as_a_supported_mode(self):
        for selected in (
            {"schema": "unsupported.environment.v1"},
            e2b_profile() | {"lifetime": "persistent"},
            e2b_profile() | {"lifetime": "retained", "unexpected": True},
        ):
            with self.subTest(profile=selected), self.assertRaises(ValueError):
                self.session_payload(selected, digest="sha256:" + "a" * 64)
        with self.assertRaises(ValueError):
            self.session_payload(e2b_profile() | {"lifetime": "retained"}, digest="sha256:" + "0" * 64)


class ExecutionModeProjectionHttpTests(OperationFixture, TestCase):
    def admit(self, mode, operation_id):
        self.profile.return_value = {
            "schema": "runtime.execution_profile.v2",
            "environmentProfile": e2b_profile() | {"lifetime": mode},
        }
        # Runtime's power-admission tables are outside this Django projection fixture.
        with patch("app_core.environment_profile.require_environment_admission"):
            response = self.post(operationId=operation_id)
        self.assertEqual(response.status_code, 202, response.content)
        return AgentRun.objects.select_related("session", "environment").get(pk=response.json()["agentRunId"])

    def session_response(self, session_id):
        response = self.client.get(f"/api/sessions/{session_id}")
        self.assertEqual(response.status_code, 200, response.content)
        return response.json()["session"]

    def environment_response(self, run):
        response = self.client.get(f"/api/sessions/{run.session_id}/agent-runs/{run.pk}/environment")
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response["Cache-Control"], "no-store")
        return response.json()

    def test_session_detail_reports_retained_environment_without_mutating_or_dispatching(self):
        run = self.admit("retained", "retained-session")
        before = list(SessionEnvironment.objects.order_by("pk").values())
        calls = self.schedule.call_count
        self.assertEqual(self.session_response(run.session_id)["executionMode"], "retained")
        self.assertEqual(list(SessionEnvironment.objects.order_by("pk").values()), before)
        self.assertEqual(self.schedule.call_count, calls)

    def test_run_environment_reports_its_frozen_retained_mode(self):
        run = self.admit("retained", "retained-run")
        self.assertEqual(self.environment_response(run), {
            "sessionId": run.session_id, "agentRunId": run.pk,
            "environmentId": run.environment_id, "executionMode": "retained",
        })

    def test_same_agent_sessions_keep_their_own_modes_after_advertisement_changes(self):
        retained = self.admit("retained", "retained-first")
        per_run = self.admit("perRun", "per-run-second")
        self.assertEqual(retained.session.agent_id, per_run.session.agent_id)
        self.assertNotEqual(retained.environment_id, per_run.environment_id)
        response = self.client.get(f"/api/workspaces/{self.workspace.pk}/sessions")
        self.assertEqual(response.status_code, 200, response.content)
        rows = {row["id"]: row for row in response.json()["sessions"]}
        self.assertEqual(rows[retained.session_id]["executionMode"], "retained")
        self.assertEqual(rows[per_run.session_id]["executionMode"], "perRun")
        self.assertEqual(self.environment_response(retained)["executionMode"], "retained")
        self.assertEqual(self.environment_response(per_run)["executionMode"], "perRun")
        retained.environment.refresh_from_db()
        self.assertEqual(retained.environment.environment_profile["lifetime"], "retained")

    def test_historical_run_mode_does_not_follow_the_sessions_new_current_environment(self):
        run = self.admit("retained", "historical-retained")
        AgentRun.objects.filter(pk=run.pk).update(status="completed")
        replacement = SessionEnvironment.objects.create(session=run.session)
        Session.objects.filter(pk=run.session_id).update(current_environment=replacement)
        self.assertEqual(self.session_response(run.session_id)["executionMode"], "perRun")
        historical = self.environment_response(run)
        self.assertEqual(historical["environmentId"], run.environment_id)
        self.assertEqual(historical["executionMode"], "retained")

    def test_trashed_agent_sessions_keep_modes_without_per_session_query_growth(self):
        run = self.admit("retained", "trashed-retained")
        Agent.objects.filter(pk=self.agent.pk).update(status="deleted", deletedAt=django_timezone.now())
        url = f"/api/agents/{self.agent.pk}/trash/sessions"
        with CaptureQueriesContext(connection) as first_queries:
            first = self.client.get(url)
        self.assertEqual(first.status_code, 200, first.content)
        self.assertEqual([(row["id"], row["executionMode"]) for row in first.json()["sessions"]],
                         [(run.session_id, "retained")])
        for _ in range(12):
            Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        with CaptureQueriesContext(connection) as many_queries:
            many = self.client.get(url)
        self.assertEqual(many.status_code, 200, many.content)
        modes = {row["id"]: row["executionMode"] for row in many.json()["sessions"]}
        self.assertEqual(modes.pop(run.session_id), "retained")
        self.assertEqual(list(modes.values()), ["perRun"] * 12)
        self.assertEqual(len(many_queries), len(first_queries),
                         f"queries grew from {len(first_queries)} to {len(many_queries)} for trashed Agent sessions")

    def test_unbound_session_and_legacy_run_keep_the_default_per_run_projection(self):
        created = self.create()
        self.assertEqual(created.status_code, 201, created.content)
        self.assertEqual(self.session_response(created.json()["sessionId"])["executionMode"], "perRun")
        accepted = self.post(operationId="legacy-run")
        self.assertEqual(accepted.status_code, 202, accepted.content)
        run = AgentRun.objects.get(pk=accepted.json()["agentRunId"])
        self.assertEqual(self.environment_response(run)["executionMode"], "perRun")


class ExecutionModeWorkListTests(TransactionTestCase):
    serialized_rollback = True
    setUp = presentation.AgentWorkPresentationTests.setUp
    request_fact = presentation.AgentWorkPresentationTests.request_fact
    dependencies = presentation.AgentWorkPresentationTests.dependencies
    post = presentation.AgentWorkPresentationTests.post
    listing = presentation.AgentWorkPresentationTests.listing

    def test_work_list_projects_each_bound_sessions_frozen_mode(self):
        profile, _ = self.dependencies()
        children = []
        for mode in ("retained", "perRun"):
            profile.return_value = {
                "schema": "runtime.execution_profile.v2",
                "environmentProfile": e2b_profile() | {"lifetime": mode},
            }
            with patch("app_core.environment_profile.require_environment_admission"), \
                    patch("app_core.agent_work_return_jobs.schedule_work_return_job"):
                accepted = self.post(self.request_fact(call_id=f"dispatch-{mode}"))
            self.assertEqual(accepted.status_code, 201, accepted.content)
            children.append(AgentRun.objects.get(pk=accepted.json()["operation"]["agentRunId"]))
        response = self.listing()
        self.assertEqual(response.status_code, 200, response.content)
        modes = {row["id"]: row["executionMode"] for row in response.json()["sessions"]}
        self.assertEqual(modes, {children[0].session_id: "retained", children[1].session_id: "perRun"})
