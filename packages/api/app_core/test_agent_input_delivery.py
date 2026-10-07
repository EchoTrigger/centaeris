"""The input carrier reaches one coordinator; Read comes only from its main fact."""
from contextlib import contextmanager
from copy import deepcopy
import json
import hashlib
from unittest.mock import patch

from django.conf import settings
from django.db import transaction
from django.test import TransactionTestCase
from django.test.utils import CaptureQueriesContext
from django.db import connection

from . import models, test_agent_inputs, test_agent_messages
from .runtime_client import build_agent_run_start
from .test_agent_work import PROFILE


class AgentInputDeliveryTests(TransactionTestCase):
    serialized_rollback = True
    bind = test_agent_messages.AgentMessageTests.bind
    submit = test_agent_inputs.AgentInputTests.submit
    input_url = test_agent_inputs.AgentInputTests.input_url
    commit = test_agent_messages.AgentMessageTests.commit

    def setUp(self):
        test_agent_messages.AgentMessageTests.setUp(self)
        self.session = self.bind()
        self.selected = models.ModelConfig.objects.create(displayName="Coordinator policy",
            thinkingMode="high", thinkingModes=["low", "high"])
        self.agent.model_config, self.agent.thinking_mode = self.selected, "low"
        self.agent.save(update_fields=["model_config", "thinking_mode"])

    @contextmanager
    def runtime(self, *, profile=None, schedule=None):
        with patch("app_core.runtime_client.request_execution_profile", side_effect=profile,
                   return_value=PROFILE) as profile_rpc, \
             patch("app_core.runtime_client.schedule_agent_run_lifecycle", side_effect=schedule,
                   return_value="inserted") as schedule_rpc:
            yield profile_rpc, schedule_rpc

    def delivery(self, input_id):
        return models.AgentInputDelivery.objects.select_related("queue__agent_run", "input").get(input__input_id=input_id)

    def dispatch(self):
        from .agent_input_delivery import dispatch_agent_inputs
        return dispatch_agent_inputs(self.agent.pk)

    def read(self, input_id):
        rows = self.client.get(self.input_url).json()["inputs"]
        return next(row["read"] for row in rows if row["inputId"] == input_id)

    def uptake(self, run, ids, *, purpose="main", request_id="main-one"):
        event = self.commit(run, "model_request_started", {"requestId": request_id,
            "purpose": purpose, "observations": [{"kind": "input_uptake", "inputIds": ids}]})
        # Core commits model requests internally; they do not project into the UI stream.
        models.SessionEvent.objects.filter(pk=event.pk).update(projects_to_agent_run_stream=False)
        event.projects_to_agent_run_stream = False
        return event

    def test_idle_admission_binds_exact_initial_identity_and_owner_policy_without_read(self):
        body = "  original\nUnicode: 中文  "
        with self.runtime() as (profile, schedule):
            response = self.submit("initial-one", body)
        self.assertEqual(response.status_code, 201, response.content)
        self.assertEqual(models.AgentRun.objects.count(), 1)
        delivered = self.delivery("initial-one")
        run = delivered.queue.agent_run
        self.assertEqual((run.session_id, run.modelConfig_id, run.thinkingMode),
            (self.session.pk, self.selected.pk, "low"))
        self.assertEqual(run.authorization.payload["thinkingMode"], "low")
        self.assertEqual(build_agent_run_start(run)["initialInput"],
            {"type": "userInput", "inputId": "initial-one", "message": body, "attachmentRefs": []})
        self.assertEqual(delivered.queue.initial_input_id, delivered.input_id)
        self.assertEqual(delivered.input.body, body)
        self.assertIsNone(self.read("initial-one"))
        self.assertEqual((profile.call_count, schedule.call_count), (1, 1))

    def test_busy_inputs_share_one_run_without_changing_initial_or_authorization_snapshot(self):
        with self.runtime():
            self.submit("initial-one", "original")
        first = self.delivery("initial-one")
        run = first.queue.agent_run
        proof = deepcopy(run.authorization.payload), run.authorization.digest, run.authorization.signature
        self.agent.model_config, self.agent.thinking_mode = None, ""
        self.agent.save(update_fields=["model_config", "thinking_mode"])
        with self.runtime(profile=AssertionError("busy admission has no profile RPC")):
            self.assertEqual(self.submit("active-two", "  later exact body  ").status_code, 201)
        second = self.delivery("active-two")
        self.assertEqual(first.queue_id, second.queue_id)
        self.assertEqual(models.AgentRun.objects.count(), 1)
        run.refresh_from_db()
        run.authorization.refresh_from_db()
        self.assertEqual((run.authorization.payload, run.authorization.digest, run.authorization.signature), proof)
        self.assertEqual(build_agent_run_start(run)["initialInput"]["inputId"], "initial-one")
        self.assertIsNone(self.read("active-two"))

    def test_replay_and_lost_schedule_response_keep_one_fact_one_binding_and_one_run(self):
        with self.runtime(schedule=OSError("synthetic lost schedule response")):
            accepted = self.submit("stable-one", "exact body")
        self.assertEqual(accepted.status_code, 201, accepted.content)
        identity = self.delivery("stable-one").queue.agent_run_id
        with self.runtime(profile=AssertionError("replay keeps admitted identity")):
            replay = self.submit("stable-one", "exact body")
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(self.delivery("stable-one").queue.agent_run_id, identity)
        self.assertEqual((models.AgentInput.objects.count(), models.AgentInputDelivery.objects.count(),
            models.AgentRun.objects.count()), (1, 1, 1))

    def test_profile_failure_retains_original_input_for_idempotent_later_admission(self):
        with self.runtime(profile=OSError("profile unavailable")):
            response = self.submit("pending-one", "  preserved pending body  ")
        self.assertEqual(response.status_code, 201, response.content)
        self.assertEqual(models.AgentInput.objects.get().body, "  preserved pending body  ")
        self.assertFalse(models.AgentRun.objects.exists())
        with self.runtime():
            self.dispatch()
        self.assertEqual(models.AgentRun.objects.count(), 1)
        self.assertEqual(self.delivery("pending-one").input.body, "  preserved pending body  ")

    def test_settings_are_rechecked_after_profile_rpc_before_committed_initial_admission(self):
        def changed_policy():
            self.agent.thinking_mode = "high"
            self.agent.save(update_fields=["thinking_mode"])
            return PROFILE
        with self.runtime(profile=changed_policy):
            self.submit("policy-race", "exact body")
        run = self.delivery("policy-race").queue.agent_run
        self.assertEqual((run.thinkingMode, run.authorization.payload["thinkingMode"]), ("high", "high"))

    def test_closed_queue_retains_late_input_until_committed_final_without_fork(self):
        with self.runtime():
            self.submit("initial-one", "first")
        first = self.delivery("initial-one")
        run = first.queue.agent_run
        models.AgentInputQueue.objects.filter(pk=first.queue_id).update(accepting=False)
        with self.runtime(profile=AssertionError("closed active owner cannot fork")):
            self.assertEqual(self.submit("late-two", "late retained body").status_code, 201)
        self.assertEqual(models.AgentRun.objects.count(), 1)
        self.assertFalse(models.AgentInputDelivery.objects.filter(input__input_id="late-two").exists())
        self.commit(run, "agent_run_completed", {"summary": "done"})
        with self.runtime():
            self.dispatch()
        second = self.delivery("late-two")
        self.assertNotEqual(second.queue.agent_run_id, run.pk)
        self.assertEqual(models.AgentRun.objects.filter(status__in=["queued", "running"]).count(), 1)
        self.assertEqual(build_agent_run_start(second.queue.agent_run)["initialInput"]["inputId"], "late-two")
        self.assertEqual(build_agent_run_start(run)["initialInput"]["inputId"], "initial-one")

    def test_current_membership_cannot_launder_a_revoked_original_pending_input(self):
        with self.runtime(profile=OSError("preserve pending input")):
            self.submit("revoked-one", "retained")
        self.membership.delete()
        models.WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        with self.runtime(profile=AssertionError("revoked input must reject before RPC")):
            self.dispatch()
        self.assertFalse(models.AgentRun.objects.exists())
        self.assertFalse(models.AgentInputDelivery.objects.exists())
        self.assertEqual(models.AgentInput.objects.get().body, "retained")

    def test_lifecycle_terminal_projection_hands_pending_input_to_one_next_coordinator(self):
        with self.runtime():
            self.submit("initial-one", "first")
        first = self.delivery("initial-one")
        run = first.queue.agent_run
        models.AgentInputQueue.objects.filter(pk=first.queue_id).update(accepting=False)
        with self.runtime(profile=AssertionError("active closed owner cannot fork")):
            self.submit("late-two", "retained for handoff")
        self.commit(run, "agent_run_completed", {"summary": "done"})
        with self.runtime(), patch("app_core.http.internal.get_runtime_job", return_value={"status": "succeeded"}):
            response = self.client.post("/internal/agent-run-lifecycle/reconcile", content_type="application/json",
                data=json.dumps({"schema": "runtime.agent_run_lifecycle.reconcile.v1", "limit": 100}),
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertNotEqual(self.delivery("late-two").queue.agent_run_id, run.pk)
        self.assertEqual(models.AgentRun.objects.filter(status__in=["queued", "running"]).count(), 1)

    def test_main_uptake_exposes_exact_reference_and_batch_ids_in_inputs_and_history(self):
        with self.runtime():
            self.submit("initial-one", "first exact body")
            self.submit("active-two", "second exact body")
        run = self.delivery("initial-one").queue.agent_run
        event = self.uptake(run, ["initial-one", "active-two"])
        later = self.uptake(run, ["initial-one", "active-two"], request_id="later-main")
        # Session ledger order owns Read even when a later event has an older clock.
        models.SessionEvent.objects.filter(pk=event.pk).update(createdAtMs=200)
        models.SessionEvent.objects.filter(pk=later.pk).update(createdAtMs=100)
        event.createdAtMs = 200
        expected = {"agentRunId": run.pk, "eventId": event.eventId,
            "requestId": "main-one", "createdAtMs": event.createdAtMs}
        self.assertEqual(self.read("initial-one"), expected)
        self.assertEqual(self.read("active-two"), expected)
        with CaptureQueriesContext(connection) as queries:
            history = self.client.get(f"/api/agents/{self.agent.pk}/history").json()
        self.assertEqual([row["input"]["read"] for row in history["items"]], [expected, expected])
        self.assertEqual(sum("UNION ALL" in query["sql"] for query in queries), 1)

    def test_stored_manifest_uptake_projects_only_the_owned_committed_main_request(self):
        from .test_native_consumption_runtime import isolate_runtime_schema
        isolate_runtime_schema(self)
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA IF NOT EXISTS runtime")
            cursor.execute("CREATE TABLE runtime.model_observation_contents("
                "session_id text,content_digest text,kind text,content_json text)")
            cursor.execute("CREATE TABLE runtime.model_observation_manifests("
                "session_id text,manifest_digest text,parent_digest text,manifest_json text)")
        with self.runtime():
            self.submit("initial-one", "first exact body")
            self.submit("active-two", "second exact body")
        run = self.delivery("initial-one").queue.agent_run
        canonical = lambda document: json.dumps(document, sort_keys=True, separators=(",", ":"))
        content = canonical({"kind": "input_uptake", "inputIds": ["initial-one", "active-two"]})
        digest = lambda domain, body: "sha256:" + hashlib.sha256(domain + body.encode()).hexdigest()
        content_digest = digest(b"centaeris.model_observation_content.v1\0", content)
        root = canonical({"parentDigest": None, "observationCount": 1,
            "changes": [{"index": 0, "kind": "input_uptake", "contentDigest": content_digest}]})
        root_digest = digest(b"centaeris.model_observation_manifest.v1\0", root)
        child = canonical({"parentDigest": root_digest, "observationCount": 1, "changes": []})
        child_digest = digest(b"centaeris.model_observation_manifest.v1\0", child)
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.model_observation_contents VALUES(%s,%s,%s,%s)",
                [self.session.pk, content_digest, "input_uptake", content])
            cursor.executemany("INSERT INTO runtime.model_observation_manifests VALUES(%s,%s,%s,%s)",
                [(self.session.pk, root_digest, None, root),
                 (self.session.pk, child_digest, root_digest, child)])
        payload = {"requestId": "stored-uptake", "purpose": "compaction",
            "observations": {"manifestDigest": child_digest}}
        self.commit(run, "model_request_started", payload)
        self.assertIsNone(self.read("initial-one"))
        payload["purpose"] = "main"
        event = self.commit(run, "model_request_started", payload)
        models.SessionEvent.objects.filter(pk=event.pk).update(projects_to_agent_run_stream=False)
        expected = {"agentRunId": run.pk, "eventId": event.eventId,
            "requestId": "stored-uptake", "createdAtMs": event.createdAtMs}
        self.assertEqual(self.read("initial-one"), expected)
        self.assertEqual(self.read("active-two"), expected)
        history = self.client.get(f"/api/agents/{self.agent.pk}/history").json()
        self.assertEqual([row["input"]["read"] for row in history["items"]], [expected, expected])

    def test_claim_ack_compaction_failed_commit_and_foreign_run_do_not_establish_read(self):
        with self.runtime():
            self.submit("initial-one", "exact body")
        delivered = self.delivery("initial-one")
        run = delivered.queue.agent_run
        models.AgentInputDelivery.objects.filter(pk=delivered.pk).update(
            claim_token="claim-one", claim_lease_owner="owner-one", acknowledged_at_ms=1)
        self.assertIsNone(self.read("initial-one"))
        self.uptake(run, ["initial-one"], purpose="compaction", request_id="compaction")
        self.assertIsNone(self.read("initial-one"))
        try:
            with transaction.atomic():
                self.uptake(run, ["initial-one"], request_id="rolled-back")
                raise RuntimeError("synthetic safe point commit failure")
        except RuntimeError:
            pass
        self.assertIsNone(self.read("initial-one"))
        foreign = models.AgentRun.objects.create(workspace=self.workspace, session=self.session,
            user=self.user, modelConfig=self.selected, prompt="another execution", status="completed")
        self.uptake(foreign, ["initial-one"], request_id="foreign-request")
        self.assertIsNone(self.read("initial-one"))

    def test_read_refresh_preserves_original_body_order_and_cursor_after_restart(self):
        with self.runtime():
            self.submit("initial-one", "  exact old body  ")
        url = f"/api/agents/{self.agent.pk}/history"
        before = self.client.get(url).json()["items"][0]
        run = self.delivery("initial-one").queue.agent_run
        event = self.uptake(run, ["initial-one"])
        after = self.client.get(url).json()["items"][0]
        self.assertEqual(after["cursor"], before["cursor"])
        self.assertEqual(after["input"]["body"], "  exact old body  ")
        self.assertEqual(after["input"]["read"]["eventId"], event.eventId)
        self.assertEqual(models.AgentInput.objects.get().body, before["input"]["body"])
