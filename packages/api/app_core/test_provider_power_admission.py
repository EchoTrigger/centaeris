"""Paused work queues without granting execution or hiding invalid Job leases."""
import json
from contextlib import ExitStack, contextmanager, nullcontext
from types import SimpleNamespace
from unittest.mock import MagicMock, Mock, patch

from django.test import SimpleTestCase

from . import environment_lifecycle, environment_profile
from .test_provider_cleanup_receipt import provider_profile


def retained_environment():
    profile = provider_profile() | {"lifetime": "retained"}
    return SimpleNamespace(pk="environment-1", session_id="session-1",
        environment_profile=profile,
        environment_profile_digest=environment_profile.environment_profile_digest(profile),
        controller_epoch=3, controller_released=True,
        controller_maintenance_operation_id=None, controller_run_id=None,
        save=Mock())


@contextmanager
def admission_database(*, retiring=False, blocked=False, order=None):
    cursor = MagicMock()
    cursor.fetchone.return_value = (retiring, blocked)
    if order is not None:
        cursor.execute.side_effect = lambda *args: order.append("admission")
    with patch("app_core.environment_profile.connection") as database:
        database.cursor.return_value.__enter__.return_value = cursor
        yield cursor


@contextmanager
def controller_binding(*, current=("execute",), blocked=True, retiring=False,
                       terminal=False):
    environment = retained_environment()
    session = SimpleNamespace(pk=environment.session_id,
        current_environment_id=environment.pk, deletedAt=None)
    run = SimpleNamespace(pk="run-2", session_id=session.pk,
        environment_id=environment.pk, workspace_id="workspace-1",
        membership_ref="member-1", user_id="user-1", thinkingMode="",
        status="cancelled" if terminal else "queued")
    grant = SimpleNamespace(payload={"thinkingMode": None},
        digest="sha256:" + "a" * 64, signature="fixture-signature")
    order = []
    job_cursor = MagicMock()
    job_cursor.fetchone.return_value = current
    job_cursor.execute.side_effect = lambda *args: order.append("job")
    with ExitStack() as stack:
        for name, value in (
            ("AgentRun", run), ("Session", session),
            ("SessionEnvironment", environment),
            ("Workspace", SimpleNamespace(pk=run.workspace_id, status="active")),
            ("WorkspaceMembership", SimpleNamespace(pk=run.membership_ref)),
            ("AgentRunAuthorization", grant),
        ):
            model = stack.enter_context(patch.object(environment_lifecycle, name))
            model.objects.only.return_value.get.return_value = value
            locked = model.objects.select_for_update.return_value
            locked.get.return_value = value
            locked.select_related.return_value.get.return_value = value
            locked.filter.return_value.first.return_value = value
        database = stack.enter_context(patch.object(environment_lifecycle, "connection"))
        database.cursor.return_value.__enter__.return_value = job_cursor
        policy_cursor = stack.enter_context(admission_database(
            retiring=retiring, blocked=blocked, order=order))
        stack.enter_context(patch.object(environment_lifecycle, "authorization_digest",
            return_value=grant.digest))
        stack.enter_context(patch.object(environment_lifecycle, "agent_run_binding_matches",
            return_value=True))
        stack.enter_context(patch.object(environment_lifecycle,
            "verify_agent_run_authorization_signature"))
        stack.enter_context(patch.object(environment_lifecycle, "_stored_release", return_value=None))
        stack.enter_context(patch.object(environment_lifecycle, "_translate_contention",
            side_effect=lambda operation: operation()))
        stack.enter_context(patch("app_core.session_event.committed_session_terminal_state",
            return_value="cancelled" if terminal else None))
        yield run, environment, order, policy_cursor


class ProviderPowerAdmissionTests(SimpleTestCase):
    def test_paused_environment_still_freezes_profile_for_queued_work(self):
        environment = retained_environment()
        session = SimpleNamespace(pk=environment.session_id,
            current_environment_id=environment.pk)
        run = SimpleNamespace(pk="run-2", session_id=session.pk,
            environment_id=environment.pk, session=session)
        with patch("app_core.environment_profile.transaction.atomic", return_value=nullcontext()), \
                patch("app_core.models.Session.objects.select_for_update") as sessions, \
                patch("app_core.models.SessionEnvironment.objects.select_for_update") as environments, \
                admission_database(blocked=True) as cursor:
            sessions.return_value.get.return_value = session
            environments.return_value.get.return_value = environment
            actual = environment_profile.freeze_run_environment_profile(run)
        self.assertIs(actual, environment)
        self.assertIs(run.environment, environment)
        cursor.execute.assert_called_once()
        environment.save.assert_not_called()

    def test_retirement_still_refuses_new_run_profile(self):
        with admission_database(retiring=True, blocked=True):
            with self.assertRaisesMessage(ValueError, "environment_retiring"):
                environment_profile.require_environment_admission(retained_environment())

    def test_pause_blocks_valid_execute_lease_before_controller_mutation(self):
        with controller_binding() as (run, environment, order, cursor):
            with self.assertRaisesMessage(ValueError, "environment_power_pending") as caught:
                environment_lifecycle.acquire_environment_controller(run.pk, "worker-current")
            self.assertEqual(type(caught.exception).__name__, "EnvironmentPowerPending")
            self.assertEqual(order, ["job", "admission"])
            cursor.execute.assert_called_once()
            self.assertEqual(environment.controller_epoch, 3)
            self.assertTrue(environment.controller_released)
            environment.save.assert_not_called()

    def test_expired_or_control_lease_cannot_receive_a_power_wait(self):
        for current in (None, ("control",)):
            with self.subTest(current=current), controller_binding(current=current) as fixture:
                run, environment, order, cursor = fixture
                with self.assertRaisesMessage(ValueError, "environment_controller_lease_rejected"):
                    environment_lifecycle.acquire_environment_controller(run.pk, "worker-stale")
                self.assertEqual(order, ["job"])
                cursor.execute.assert_not_called()
                environment.save.assert_not_called()

    def test_resumed_environment_grants_only_after_the_current_job_fence(self):
        with controller_binding(blocked=False) as (run, environment, order, cursor):
            authority = environment_lifecycle.acquire_environment_controller(run.pk, "worker-current")
            self.assertEqual(authority["controllerEpoch"], 4)
            self.assertEqual(authority["controllerPurpose"], "execute")
            self.assertEqual(order, ["job", "admission"])
            cursor.execute.assert_called_once()
            environment.save.assert_called_once()

    def test_terminal_cleanup_does_not_require_a_running_environment(self):
        with controller_binding(current=("control",), terminal=True) as fixture:
            run, environment, order, cursor = fixture
            authority = environment_lifecycle.acquire_environment_cleanup_controller(run.pk, "worker-control")
            self.assertEqual(authority["controllerPurpose"], "cleanup")
            self.assertEqual(order, ["job"])
            cursor.execute.assert_not_called()
            environment.save.assert_called_once()

    def test_retirement_takes_precedence_over_a_power_wait(self):
        with controller_binding(retiring=True) as (run, environment, order, _):
            with self.assertRaisesMessage(ValueError, "environment_retiring"):
                environment_lifecycle.acquire_environment_controller(run.pk, "worker-current")
            self.assertEqual(order, ["job", "admission"])
            environment.save.assert_not_called()


class ProviderPowerResolveTests(SimpleTestCase):
    def resolve(self, *, acquire_error=None, terminal=None, released=False):
        run = SimpleNamespace(pk="run-2", environment=retained_environment())
        body = {"schema": "runtime.agent_run_lifecycle.resolve.v1",
            "jobId": "agent_run.lifecycle:run-2", "agentRunId": "run-2",
            "authorizationDigest": "sha256:" + "a" * 64,
            "leaseOwner": "worker-current", "executionMode": "retained"}
        with patch("app_core.http.internal._retained_run_projection", return_value=(run, terminal)), \
                patch("app_core.http.internal.get_environment_release_receipt",
                    return_value={"releaseReceiptId": "release-2"} if released else None), \
                patch("app_core.http.internal.acquire_environment_controller",
                    side_effect=acquire_error, return_value={"controllerEpoch": 4}) as execute, \
                patch("app_core.http.internal.acquire_environment_cleanup_controller",
                    return_value={"controllerPurpose": "cleanup"}) as cleanup, \
                patch("app_core.http.internal.build_agent_run_start",
                    return_value={"authority": "fixture-execute"}) as start, \
                patch("app_core.http.internal.build_agent_run_cleanup",
                    return_value={"authority": "fixture-cleanup"}):
            response = self.client.post("/internal/agent-run-lifecycle/resolve",
                data=json.dumps(body), content_type="application/json",
                HTTP_X_INTERNAL_TOKEN="test-internal-token")
        return response, execute, cleanup, start

    def test_power_wait_is_a_successful_http_disposition_without_authority(self):
        pending_type = getattr(environment_profile, "EnvironmentPowerPending", ValueError)
        response, execute, cleanup, start = self.resolve(
            acquire_error=pending_type("environment_power_pending"))
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json(), {
            "schema": "runtime.agent_run_lifecycle.resolved.v1",
            "disposition": "waiting", "waitReason": "environment_power_pending"})
        execute.assert_called_once_with("run-2", "worker-current")
        cleanup.assert_not_called()
        start.assert_not_called()

    def test_invalid_lease_or_retirement_remains_a_rejection(self):
        for reason in ("environment_controller_lease_rejected", "environment_retiring",
                       "environment_power_pending"):
            with self.subTest(reason=reason):
                response, _, cleanup, start = self.resolve(acquire_error=ValueError(reason))
                self.assertEqual(response.status_code, 409)
                self.assertEqual(response.json(), {"error": reason})
                cleanup.assert_not_called()
                start.assert_not_called()

    def test_terminal_cleanup_and_released_replay_bypass_power_wait(self):
        for released in (False, True):
            with self.subTest(released=released):
                response, execute, cleanup, start = self.resolve(terminal="cancelled", released=released)
                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.json()["disposition"], "released" if released else "terminal")
                execute.assert_not_called()
                start.assert_not_called()
                if released:
                    cleanup.assert_not_called()
                else:
                    cleanup.assert_called_once()

    def test_running_environment_keeps_existing_ready_authority(self):
        response, execute, cleanup, start = self.resolve()
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), {
            "schema": "runtime.agent_run_lifecycle.resolved.v1",
            "disposition": "ready", "agentRunStart": {"authority": "fixture-execute"}})
        execute.assert_called_once()
        start.assert_called_once()
        cleanup.assert_not_called()
