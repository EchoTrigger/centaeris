"""Committed dispatch facts materialize one owned work Session and admission."""
import json
import tempfile
import hashlib
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import close_old_connections, connection, transaction
from django.test import Client, TransactionTestCase, override_settings

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import Agent, AgentCoordinationSession, AgentRun, HostedOperationReceipt, ModelConfig, Session, SessionAssetLink, SessionEvent, UserLibraryObject, Workspace, WorkspaceMembership


PROFILE = {"schema": "runtime.execution_profile.v1", "imageCapability": "workspace_general_v1",
           "imageDigest": "sha256:" + "a" * 64}


class AgentWorkTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="work-owner")
        self.workspace = Workspace.objects.create(name="Work dispatch", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Private coordinator")
        self.source = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent, origin="automation")
        AgentCoordinationSession.objects.create(agent=self.agent, session=self.source)
        self.model = ModelConfig.objects.create(displayName="Synthetic work model")
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
            modelConfig=self.model, prompt="Dispatch work", status="running")
        create_agent_run_authorization(self.run, image_digest=PROFILE["imageDigest"])
        self.args = {"objective": "Full work objective.\nKeep exact text.", "session_refs": [], "file_refs": []}

    def request_fact(self, *, call_changes=None, result_changes=None, call_id="dispatch-one"):
        from .agent_work import work_contract_digest
        records = [("tool_call", {"callId": call_id, "toolName": "dispatch_work",
                    "providerId": "workspace.agent_work", "toolContractDigest": work_contract_digest(),
                    "normalizedInput": self.args, **(call_changes or {})}),
                   ("tool_result", {"callId": call_id, "toolName": "dispatch_work",
                    "resultState": "successWithOutput", "modelContent": "validated request, not admission",
                    **(result_changes or {})})]
        for kind, payload in records:
            sequence = self.source.events.count() + 1
            wire = {"schemaVersion": "session.event.v1", "eventVersion": 1, "eventId": f"work:{self.run.id}:{sequence}",
                "sessionId": self.source.id, "agentRunId": self.run.id, "turnId": self.run.turn_id,
                "type": kind, "sequence": sequence, "createdAtMs": sequence, "payload": payload}
            result = SessionEvent.objects.create(eventId=wire["eventId"], workspace=self.workspace, session=self.source,
                agent_run=self.run, sequence=sequence, agent_run_sequence=sequence, payload=wire,
                createdAtMs=sequence, projects_to_agent_run_stream=True)
        return result

    def post(self, event, client=None):
        return (client or self.client).post("/internal/agent-work/materialize", content_type="application/json",
            data=json.dumps({"schema": "workspace.agent_work.materialize.v1", "sourceEventId": event.eventId}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def dependencies(self):
        from contextlib import ExitStack
        stack = ExitStack()
        self.addCleanup(stack.close)
        profile = stack.enter_context(patch("app_core.agent_work.request_execution_profile", return_value=PROFILE))
        schedule = stack.enter_context(patch("app_core.agent_work.schedule_agent_run_lifecycle", return_value="inserted"))
        return profile, schedule

    def test_materializes_only_after_source_commit_with_a_distinct_admission_receipt(self):
        from .agent_work import materialize_work_request, WorkMaterializationError
        from .models import AgentWorkSession
        profile, schedule = self.dependencies()
        with transaction.atomic():
            event = self.request_fact()
            with self.assertRaises(WorkMaterializationError):
                materialize_work_request(event.eventId)
            with ThreadPoolExecutor(max_workers=1) as pool:
                def before_commit():
                    close_old_connections()
                    try:
                        return self.post(event, Client()).status_code
                    finally:
                        close_old_connections()
                self.assertEqual(pool.submit(before_commit).result(timeout=10), 404)
            self.assertEqual(Session.objects.count(), 1)
            self.assertFalse(HostedOperationReceipt.objects.exists())
        self.assertEqual(Session.objects.count(), 1, "request fact itself cannot create work")
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        response = self.post(event)
        self.assertEqual(response.status_code, 201, response.content)
        receipt = HostedOperationReceipt.objects.get()
        child = AgentRun.objects.get(pk=receipt.agentRunId)
        binding = AgentWorkSession.objects.get()
        self.assertEqual(binding.session_id, child.session_id)
        self.assertEqual(binding.coordination_session_id, self.source.id)
        self.assertEqual(binding.source_event_id, event.eventId)
        self.assertEqual(binding.source_run_id, self.run.id)
        self.assertEqual(binding.source_call_id, "dispatch-one")
        self.assertEqual(binding.operation_id, receipt.pk)
        self.assertNotEqual(child.id, self.run.id)
        self.assertEqual(child.prompt, self.args["objective"])
        self.assertEqual((child.user_id, child.workspace_id, child.session.agent_id),
                         (self.user.id, self.workspace.id, self.agent.id))
        self.assertIsNone(child.app_delegation_id)
        self.assertFalse(child.session.events.exists())
        self.assertEqual(child.authorization.payload["assetRefs"], [])
        self.assertEqual(response.json()["operation"]["status"], "accepted")
        self.assertEqual(response.json()["operation"]["agentRunId"], child.id)
        schedule.assert_called_once()

    def test_replay_and_duplicate_success_keep_the_same_run_after_failed_admission_execution(self):
        profile, schedule = self.dependencies()
        event = self.request_fact()
        first = self.post(event)
        self.assertEqual(first.status_code, 201, first.content)
        child_id = first.json()["operation"]["agentRunId"]
        AgentRun.objects.filter(pk=child_id).update(status="failed")
        self.model.enabled = False
        self.model.save(update_fields=["enabled"])
        profile.side_effect = RuntimeError("profile offline")
        replay = self.post(event)
        duplicate = self.request_fact()
        self.assertEqual(replay.status_code, 200, replay.content)
        self.assertEqual(self.post(duplicate).json(), first.json())
        self.assertEqual(replay.json(), first.json())
        from .models import AgentWorkSession
        AgentWorkSession.objects.get().delete()
        self.assertEqual(self.post(duplicate).json(), first.json(), "lost relation index is rebuilt from the original source and receipt")
        self.assertEqual(AgentRun.objects.count(), 2)
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        schedule.assert_called_once()

    def test_untrusted_or_failed_source_records_do_not_create_work(self):
        profile, schedule = self.dependencies()
        cases = [({"providerId": "foreign.provider"}, {}),
                 ({"toolContractDigest": "sha256:" + "f" * 64}, {}),
                 ({}, {"resultState": "failure"}),
                 ({"normalizedInput": {**self.args, "recipientAgentId": self.agent.id}}, {})]
        for call, result in cases:
            with self.subTest(call=call, result=result):
                event = self.request_fact(call_changes=call, result_changes=result)
                self.assertEqual(self.post(event).status_code, 409)
        self.assertEqual(Session.objects.count(), 1)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        profile.assert_not_called()
        schedule.assert_not_called()

    def test_current_membership_and_session_refs_reject_without_materialization(self):
        profile, schedule = self.dependencies()
        self.args["session_refs"] = ["unknown-session"]
        event = self.request_fact()
        self.assertEqual(self.post(event).status_code, 403)
        self.args["session_refs"] = []
        event = self.request_fact(call_id="dispatch-two")
        self.membership.delete()
        self.assertEqual(self.post(event).status_code, 403)
        self.assertEqual(Session.objects.count(), 1)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        schedule.assert_not_called()

    def test_selected_assets_are_explicit_and_current_not_inherited_from_all_source_inputs(self):
        self.dependencies()
        with tempfile.TemporaryDirectory(prefix="work-inputs-") as storage, override_settings(MEDIA_ROOT=storage):
            links = []
            for index in range(2):
                content = str(index).encode()
                key = default_storage.save(f"work/input-{index}.txt", ContentFile(content))
                item = UserLibraryObject.objects.create(owner=self.user, displayName=f"input-{index}.txt", objectKind="file",
                    contentType="text/plain", storageKey=key, sizeBytes=len(content), contentGeneration=1,
                    sha256="sha256:" + hashlib.sha256(content).hexdigest(), status="ready")
                links.append(SessionAssetLink.objects.create(workspace=self.workspace, session=self.source,
                    userLibraryObject=item, attachedBy=self.user, capturedDisplayName=item.displayName,
                    capturedContentType=item.contentType, capturedOwnerKind="userLibraryObject", capturedOwnerId=item.id,
                    capturedContentGeneration=1, capturedSizeBytes=item.sizeBytes, capturedSha256=item.sha256))
            # A fresh signed source Run explicitly authorizes these inputs.
            self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
                modelConfig=self.model, prompt="Selected evidence", status="running")
            create_agent_run_authorization(self.run, image_digest=PROFILE["imageDigest"])
            self.args["file_refs"] = [links[0].id]
            response = self.post(self.request_fact())
            self.assertEqual(response.status_code, 201, response.content)
            child = AgentRun.objects.get(pk=response.json()["operation"]["agentRunId"])
            assets = child.authorization.payload["assetRefs"]
            self.assertEqual(len(assets), 1)
            self.assertEqual(assets[0]["inputIdentity"]["ownerId"], links[0].userLibraryObject_id)
            self.assertNotEqual(assets[0]["inputRef"], links[0].id)

    def test_atomic_failure_rolls_back_child_relation_and_receipt_and_schedule_failure_keeps_acceptance(self):
        from .models import AgentWorkSession
        profile, schedule = self.dependencies()
        event = self.request_fact()
        with patch("app_core.agent_work.create_agent_run_authorization", side_effect=RuntimeError("fixture rollback")):
            self.assertEqual(self.post(event).status_code, 500)
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.assertFalse(AgentWorkSession.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())
        schedule.assert_not_called()
        schedule.side_effect = RuntimeError("fixture scheduler unavailable")
        first = self.post(event)
        self.assertEqual(first.status_code, 201, first.content)
        self.assertEqual(self.post(event).json(), first.json())
        child = AgentRun.objects.get(pk=first.json()["operation"]["agentRunId"])
        self.assertEqual(child.transitionReason, "agent_run_lifecycle_schedule_pending")
        schedule.assert_called_once()

    def test_concurrent_materialization_has_one_work_session_and_one_schedule(self):
        from .models import AgentWorkSession
        profile, schedule = self.dependencies()
        event = self.request_fact()
        barrier = threading.Barrier(2)
        profile.side_effect = lambda: (barrier.wait(timeout=10), PROFILE)[1]

        def materialize(_):
            close_old_connections()
            try:
                response = self.post(event, Client())
                return response.status_code, response.json()
            finally:
                close_old_connections()

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(materialize, range(2)))
        self.assertEqual(sorted(code for code, _ in results), [200, 201], results)
        self.assertEqual(results[0][1], results[1][1])
        self.assertEqual(AgentWorkSession.objects.count(), 1)
        self.assertEqual(Session.objects.count(), 2)
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        schedule.assert_called_once()

    def _invalidate_source_under_rewrite_lock(self, event_ids):
        # Reproduce Runtime append's Session lock and tombstone projection SQL,
        # not an entire Core rewrite or a hosted authority revocation.
        with connection.cursor() as cursor:
            cursor.execute("SELECT workspace_id FROM app_core_session WHERE id=%s FOR UPDATE", [self.source.id])
            self.assertIsNotNone(cursor.fetchone())
            cursor.execute(
                'UPDATE app_core_sessionevent SET projects_to_agent_run_stream=FALSE '
                'WHERE session_id=%s AND "eventId"=ANY(%s)',
                [self.source.id, event_ids],
            )
            self.assertEqual(cursor.rowcount, len(event_ids))

    def _completed_request_fact(self):
        # Supply the terminal, latest-user tail and no FileFact prerequisite of
        # legal Core rewrite, while these API races still control only its SQL.
        def append(kind, payload):
            sequence = self.source.events.count() + 1
            wire = {"schemaVersion": "session.event.v1", "eventVersion": 1,
                "eventId": f"work:{self.run.id}:{sequence}", "sessionId": self.source.id,
                "agentRunId": self.run.id, "turnId": self.run.turn_id, "type": kind,
                "sequence": sequence, "createdAtMs": sequence, "payload": payload}
            SessionEvent.objects.create(eventId=wire["eventId"], workspace=self.workspace,
                session=self.source, agent_run=self.run, sequence=sequence, agent_run_sequence=sequence,
                payload=wire, createdAtMs=sequence, projects_to_agent_run_stream=True)

        append("agent_run_started", {"userObjective": self.run.prompt})
        append("user_message", {"messageId": f"message:{self.run.id}:user",
            "text": self.run.prompt, "attachments": []})
        content = "validated request, not admission"
        event = self.request_fact(call_changes={"displayTarget": "Private Agent work"},
            result_changes={"fullOutputPath": None, "outputStartByte": None,
                "outputByteLength": len(content.encode("utf-8")), "outputComplete": True,
                "summary": "Request recorded", "operations": [], "modelInputImages": [], "latencyMs": 0})
        append("assistant_message", {"messageId": f"message:{self.run.turn_id}:assistant",
            "modelMarkdown": "Dispatch request recorded.", "artifactRefs": [], "status": "done"})
        append("agent_run_completed", {"doneReason": "finalized"})
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        return event

    def _wait_for_session_blocker(self, waiting_pid, *, finished=None):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            with connection.cursor() as cursor:
                cursor.execute("SELECT pg_backend_pid(), pg_blocking_pids(%s)", [waiting_pid])
                own_pid, blockers = cursor.fetchone()
            if own_pid in blockers:
                return True
            if finished is not None and finished.is_set():
                return False
            threading.Event().wait(0.01)
        self.fail("second PostgreSQL connection never blocked on the source Session")

    def test_materializer_admission_serializes_rewrite_after_source_validation(self):
        from .agent_work import _locked_source
        from .models import AgentWorkSession
        _, schedule = self.dependencies()
        event = self._completed_request_fact()
        source_event_ids = list(self.source.events.values_list("eventId", flat=True))
        after_validation = threading.Barrier(2)
        rewrite_started, rewrite_done = threading.Event(), threading.Event()
        observed = {}
        source_checks = 0

        def rewrite():
            close_old_connections()
            try:
                after_validation.wait(timeout=10)
                with transaction.atomic():
                    with connection.cursor() as cursor:
                        cursor.execute("SET LOCAL lock_timeout='10s'")
                        cursor.execute("SELECT pg_backend_pid()")
                        observed["rewrite_pid"] = cursor.fetchone()[0]
                    rewrite_started.set()
                    self._invalidate_source_under_rewrite_lock(source_event_ids)
                rewrite_done.set()
            finally:
                close_old_connections()

        def pause_after_second_validation(event_id):
            nonlocal source_checks
            source = _locked_source(event_id)
            source_checks += 1
            if source_checks == 2:
                after_validation.wait(timeout=10)
                self.assertTrue(rewrite_started.wait(timeout=10))
                observed["rewrite_blocked"] = self._wait_for_session_blocker(
                    observed["rewrite_pid"], finished=rewrite_done,
                )
                observed["rewrite_committed_before_admission"] = rewrite_done.is_set()
                observed["active_before_admission"] = self.source.events.filter(
                    projects_to_agent_run_stream=True,
                ).count()
            return source

        with ThreadPoolExecutor(max_workers=1) as pool:
            rewriting = pool.submit(rewrite)
            with patch("app_core.agent_work._locked_source", side_effect=pause_after_second_validation):
                response = self.post(event)
            rewriting.result(timeout=10)
        self.assertEqual(response.status_code, 201, response.content)
        binding = AgentWorkSession.objects.get()
        self.assertEqual(binding.source_event_id, event.eventId)
        self.assertEqual(binding.source_run_id, self.run.id)
        self.assertEqual(binding.source_turn_id, self.run.turn_id)
        self.assertEqual(binding.source_call_id, "dispatch-one")
        self.assertEqual(binding.operation.operationId, response.json()["operation"]["operationId"])
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 2)
        self.assertEqual(Session.objects.count(), 2)
        self.assertFalse(self.source.events.filter(projects_to_agent_run_stream=True).exists())
        schedule.assert_called_once()
        self.assertFalse(observed["rewrite_committed_before_admission"],
            f"source invalidation committed before a 201 child admission: {observed}")
        self.assertTrue(observed["rewrite_blocked"], observed)
        self.assertEqual(observed["active_before_admission"], 6)

    def _assert_rewrite_winning_source_lock_rejects_materialization(self, source_kind, *, append_tombstone=False):
        from .agent_work import _locked_source, materialize_work_request
        from .models import AgentWorkSession
        profile, schedule = self.dependencies()
        event = self._completed_request_fact()
        target = self.source.events.get(payload__type=source_kind)
        profile_entered, rewrite_locked, second_check = (
            threading.Event(), threading.Event(), threading.Event()
        )
        observed = {}
        source_checks = 0
        materializer_done = threading.Event()

        def execution_profile():
            profile_entered.set()
            self.assertTrue(rewrite_locked.wait(timeout=10))
            return PROFILE

        def observe_second_check(event_id):
            nonlocal source_checks
            source_checks += 1
            if source_checks == 2:
                with connection.cursor() as cursor:
                    cursor.execute("SET LOCAL lock_timeout='10s'")
                    cursor.execute("SELECT pg_backend_pid()")
                    observed["materializer_pid"] = cursor.fetchone()[0]
                second_check.set()
            return _locked_source(event_id)

        def materialize():
            close_old_connections()
            try:
                response = self.post(event, Client())
                return response.status_code, response.json()
            finally:
                materializer_done.set()
                close_old_connections()

        profile.side_effect = execution_profile
        def observed_materialization(event_id):
            try:
                return materialize_work_request(event_id)
            except Exception as error:
                observed["error_type"] = type(error).__name__
                observed["sqlstate"] = getattr(error.__cause__, "sqlstate", None)
                raise

        with patch("app_core.agent_work._locked_source", side_effect=observe_second_check), \
                patch("app_core.agent_work.materialize_work_request", side_effect=observed_materialization):
            with ThreadPoolExecutor(max_workers=1) as pool:
                materializing = pool.submit(materialize)
                self.assertTrue(profile_entered.wait(timeout=10))
                with transaction.atomic():
                    with connection.cursor() as cursor:
                        cursor.execute("SELECT workspace_id FROM app_core_session WHERE id=%s FOR UPDATE", [self.source.id])
                        self.assertIsNotNone(cursor.fetchone())
                    rewrite_locked.set()
                    self.assertTrue(second_check.wait(timeout=10))
                    self._wait_for_session_blocker(observed["materializer_pid"], finished=materializer_done)
                    self._invalidate_source_under_rewrite_lock([target.eventId])
                    if append_tombstone:
                        # Runtime rewrite also inserts new source facts. Their
                        # deferred Workspace FK must not wait on this admission.
                        sequence = self.source.events.count() + 1
                        wire = {**event.payload, "eventId": "rewrite:" + event.eventId,
                            "type": "tombstone", "sequence": sequence,
                            "payload": {"targetEventIds": [target.eventId],
                                "tombstoneId": "rewrite:" + event.eventId, "reasonType": "rewrite_last_user_input"}}
                        SessionEvent.objects.create(eventId=wire["eventId"], workspace=self.workspace,
                            session=self.source, agent_run=self.run, sequence=sequence,
                            agent_run_sequence=sequence, payload=wire, createdAtMs=sequence,
                            projects_to_agent_run_stream=False)
                status, body = materializing.result(timeout=10)
        self.assertEqual(status, 503, {"response": body, "sessions": Session.objects.count(),
            "runs": AgentRun.objects.count(), "receipts": HostedOperationReceipt.objects.count(), **observed})
        self.assertEqual(body["error"], "agent_work_source_busy")
        retry = self.post(event)
        self.assertEqual(retry.status_code, 409, retry.content)
        self.assertEqual(retry.json()["error"], "agent_work_request_untrusted")
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        self.assertFalse(AgentWorkSession.objects.exists())
        schedule.assert_not_called()

    def test_rewrite_winning_source_call_lock_rejects_materialization(self):
        self._assert_rewrite_winning_source_lock_rejects_materialization("tool_call")

    def test_rewrite_winning_source_success_lock_rejects_materialization(self):
        self._assert_rewrite_winning_source_lock_rejects_materialization("tool_result")

    def test_rewrite_source_append_can_commit_without_a_workspace_lock_cycle(self):
        self._assert_rewrite_winning_source_lock_rejects_materialization("tool_result", append_tombstone=True)

    def test_rewrite_committed_after_unlocked_source_read_rejects_materialization(self):
        from .agent_work import _source_request
        from .models import AgentWorkSession
        profile, schedule = self.dependencies()
        event = self._completed_request_fact()
        read_source = threading.Barrier(2)
        rewrite_done = threading.Event()
        admission_started = False
        paused = False

        def execution_profile():
            nonlocal admission_started
            admission_started = True
            return PROFILE

        def source_read(event_id):
            nonlocal paused
            result = _source_request(event_id)
            if admission_started and not paused:
                paused = True
                read_source.wait(timeout=10)
                self.assertTrue(rewrite_done.wait(timeout=10))
            return result

        def rewrite():
            close_old_connections()
            try:
                read_source.wait(timeout=10)
                with transaction.atomic():
                    self._invalidate_source_under_rewrite_lock([event.eventId])
                rewrite_done.set()
            finally:
                close_old_connections()

        profile.side_effect = execution_profile
        with ThreadPoolExecutor(max_workers=1) as pool:
            rewriting = pool.submit(rewrite)
            with patch("app_core.agent_work._source_request", side_effect=source_read):
                response = self.post(event)
            rewriting.result(timeout=10)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertEqual(response.json()["error"], "agent_work_request_untrusted")
        self.assertEqual(Session.objects.count(), 1)
        self.assertEqual(AgentRun.objects.count(), 1)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        self.assertFalse(AgentWorkSession.objects.exists())
        schedule.assert_not_called()

    def test_call_identity_conflicts_and_independent_requests_have_distinct_work_sessions(self):
        from .models import AgentWorkSession
        _, schedule = self.dependencies()
        first = self.post(self.request_fact())
        self.assertEqual(first.status_code, 201, first.content)
        self.args = {**self.args, "objective": "A different full objective"}
        self.assertEqual(self.post(self.request_fact()).status_code, 409)
        second = self.post(self.request_fact(call_id="dispatch-two"))
        self.assertEqual(second.status_code, 201, second.content)
        self.assertNotEqual(first.json()["operation"]["sessionId"], second.json()["operation"]["sessionId"])
        self.assertNotEqual(first.json()["operation"]["agentRunId"], second.json()["operation"]["agentRunId"])
        self.assertEqual(AgentWorkSession.objects.count(), 2)
        self.assertEqual(schedule.call_count, 2)
        binding = AgentWorkSession.objects.first()
        with self.assertRaisesRegex(ValueError, "immutable"):
            binding.save()

    def test_ordinary_and_foreign_owner_sources_cannot_materialize_and_route_requires_exact_internal_identity(self):
        profile, schedule = self.dependencies()
        event = self.request_fact()
        response = self.client.post("/internal/agent-work/materialize", content_type="application/json",
            data=json.dumps({"schema": "workspace.agent_work.materialize.v1", "sourceEventId": event.eventId}))
        self.assertEqual(response.status_code, 401)
        response = self.client.post("/internal/agent-work/materialize", content_type="application/json",
            data=json.dumps({"schema": "workspace.agent_work.materialize.v1", "source_event_id": event.eventId}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(response.status_code, 400)
        foreign = get_user_model().objects.create_user(username="foreign-work-owner")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=foreign)
        AgentRun.objects.filter(pk=self.run.pk).update(user=foreign)
        self.assertEqual(self.post(event).status_code, 403)
        AgentRun.objects.filter(pk=self.run.pk).update(user=self.user)
        AgentCoordinationSession.objects.get().delete()
        self.assertEqual(self.post(event).status_code, 403)
        self.assertEqual(Session.objects.count(), 1)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        profile.assert_not_called()
        schedule.assert_not_called()

    def test_unavailable_original_work_is_not_replaced_on_replay(self):
        from django.utils import timezone
        _, schedule = self.dependencies()
        event = self.request_fact()
        first = self.post(event)
        self.assertEqual(first.status_code, 201, first.content)
        Session.objects.filter(pk=first.json()["operation"]["sessionId"]).update(status="deleted", deletedAt=timezone.now())
        response = self.post(event)
        self.assertEqual(response.status_code, 410, response.content)
        self.assertEqual(Session.objects.count(), 2)
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        schedule.assert_called_once()

    def test_selected_input_generation_and_artifact_session_scope_are_rechecked_after_request_commit(self):
        from django.utils import timezone
        from .assets import captured_input_fields
        from .models import Artifact
        _, schedule = self.dependencies()
        with tempfile.TemporaryDirectory(prefix="work-current-input-") as storage, override_settings(MEDIA_ROOT=storage):
            key = default_storage.save("work/current.txt", ContentFile(b"current input"))
            sha256 = "sha256:" + hashlib.sha256(b"current input").hexdigest()
            item = UserLibraryObject.objects.create(owner=self.user, displayName="current.txt", objectKind="file",
                contentType="text/plain", storageKey=key, sizeBytes=13, contentGeneration=1, sha256=sha256, status="ready")
            artifact = Artifact.objects.create(workspace=self.workspace, session=self.source, agent_run=self.run,
                createdBy=self.user, displayName="result.txt", contentType="text/plain", storageKey=key,
                sizeBytes=13, sha256=sha256, contentGeneration=1, status="published", publishedAt=timezone.now())
            library_link = SessionAssetLink.objects.create(workspace=self.workspace, session=self.source, userLibraryObject=item,
                attachedBy=self.user, capturedDisplayName=item.displayName, capturedContentType=item.contentType, **captured_input_fields(item))
            artifact_link = SessionAssetLink.objects.create(workspace=self.workspace, session=self.source, artifact=artifact,
                attachedBy=self.user, capturedDisplayName=artifact.displayName, capturedContentType=artifact.contentType, **captured_input_fields(artifact))
            self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
                modelConfig=self.model, prompt="Current selected input", status="running")
            create_agent_run_authorization(self.run, image_digest=PROFILE["imageDigest"])
            self.args["file_refs"] = [library_link.id]
            event = self.request_fact()
            UserLibraryObject.objects.filter(pk=item.pk).update(contentGeneration=2)
            self.assertEqual(self.post(event).status_code, 403)
            self.args["file_refs"] = [artifact_link.id]
            response = self.post(self.request_fact(call_id="dispatch-artifact"))
            self.assertEqual(response.status_code, 403, response.content)
            self.assertEqual(response.json()["error"], "agent_work_file_scope_not_supported")
            self.assertEqual(Session.objects.count(), 1)
            self.assertFalse(HostedOperationReceipt.objects.exists())
            schedule.assert_not_called()

    def test_available_managed_private_source_is_explicitly_unsupported(self):
        from .models import AgentDefinition, AgentDefinitionVersion
        profile, schedule = self.dependencies()
        definition = AgentDefinition.objects.create(workspace=self.workspace, created_by=self.user,
            name="Managed coordinator", availability_scope="workspace")
        version = AgentDefinitionVersion.objects.create(definition=definition, version=1,
            name=definition.name, published_by=self.user)
        definition.published_version = version
        definition.save(update_fields=["published_version"])
        self.agent = Agent.objects.create(workspace=self.workspace, owner=self.user,
            definition=definition, name=version.name)
        self.source = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent, origin="automation")
        AgentCoordinationSession.objects.create(agent=self.agent, session=self.source)
        self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
            modelConfig=self.model, definition_version=version, agent_instructions=version.instructions,
            prompt="Managed source", status="running")
        create_agent_run_authorization(self.run, image_digest=PROFILE["imageDigest"])
        response = self.post(self.request_fact())
        self.assertEqual(response.status_code, 403, response.content)
        self.assertEqual(response.json()["error"], "agent_work_source_not_supported")
        self.assertEqual(Session.objects.count(), 2)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        profile.assert_not_called()
        schedule.assert_not_called()

    def test_selected_source_grant_is_current_for_admission_and_child_reads(self):
        from .assets import captured_input_fields, DeferredInputResolutionError
        from .deferred_input import resolve_deferred_input
        from .models import Source, SourceGrant, SourceObject, WorkspaceGroup
        _, schedule = self.dependencies()
        self.membership.role = "member"
        self.membership.save(update_fields=["role"])
        admin = get_user_model().objects.create_user(username="work-source-admin")
        WorkspaceMembership.objects.create(workspace=self.workspace, user=admin, role="owner")
        with tempfile.TemporaryDirectory(prefix="work-source-input-") as storage, override_settings(MEDIA_ROOT=storage):
            content = b"Selected source evidence"
            key = default_storage.save("work/source.txt", ContentFile(content))
            source = Source.objects.create(workspace=self.workspace, sourceType="fileTree", name="Evidence",
                status="ready", createdBy=admin)
            item = SourceObject.objects.create(workspace=self.workspace, source=source, objectType="file",
                displayPath="source.txt", displayName="source.txt", contentType="text/plain", sizeBytes=len(content),
                sha256="sha256:" + hashlib.sha256(content).hexdigest(), storageKey=key, status="ready", contentGeneration=1)
            group = WorkspaceGroup.objects.create(workspace=self.workspace, name="Work readers", createdBy=admin)
            group.members.add(self.membership)
            grant = SourceGrant.objects.create(workspace=self.workspace, source=source, workspaceGroup=group, createdBy=admin)
            link = SessionAssetLink.objects.create(workspace=self.workspace, session=self.source, sourceObject=item,
                attachedBy=self.user, capturedDisplayName=item.displayName, capturedContentType=item.contentType,
                **captured_input_fields(item))
            self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
                modelConfig=self.model, prompt="Explicit current source", status="running")
            create_agent_run_authorization(self.run, image_digest=PROFILE["imageDigest"])
            self.args["file_refs"] = [link.id]
            first_event = self.request_fact()
            first = self.post(first_event)
            self.assertEqual(first.status_code, 201, first.content)
            child = AgentRun.objects.get(pk=first.json()["operation"]["agentRunId"])
            child_ref = child.authorization.payload["assetRefs"][0]["inputRef"]
            self.assertEqual(resolve_deferred_input(child, child_ref, child.authorization.digest)["objectRef"], item.id)
            next_event = self.request_fact(call_id="dispatch-after-source-change")
            grant.delete()
            self.assertEqual(self.post(next_event).status_code, 403)
            with self.assertRaises(DeferredInputResolutionError):
                resolve_deferred_input(child, child_ref, child.authorization.digest)
            self.assertEqual(self.post(first_event).json(), first.json(), "receipt replay grants no input read access")
            self.assertEqual(Session.objects.count(), 2)
            self.assertEqual(HostedOperationReceipt.objects.count(), 1)
            schedule.assert_called_once()

    def test_change_after_input_validation_cannot_upgrade_the_child_signed_generation(self):
        from .assets import captured_input_fields, DeferredInputResolutionError
        from .deferred_input import input_storage_batch, resolve_deferred_input
        self.dependencies()
        with tempfile.TemporaryDirectory(prefix="work-input-race-") as storage, override_settings(MEDIA_ROOT=storage):
            content = b"original generation"
            key = default_storage.save("work/race.txt", ContentFile(content))
            item = UserLibraryObject.objects.create(owner=self.user, displayName="race.txt", objectKind="file",
                contentType="text/plain", storageKey=key, sizeBytes=len(content), contentGeneration=1,
                sha256="sha256:" + hashlib.sha256(content).hexdigest(), status="ready")
            link = SessionAssetLink.objects.create(workspace=self.workspace, session=self.source, userLibraryObject=item,
                attachedBy=self.user, capturedDisplayName=item.displayName, capturedContentType=item.contentType,
                **captured_input_fields(item))
            self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
                modelConfig=self.model, prompt="Exact input generation", status="running")
            create_agent_run_authorization(self.run, image_digest=PROFILE["imageDigest"])
            self.args["file_refs"] = [link.id]
            event = self.request_fact()

            def validate_then_change(run, digest):
                resolve = input_storage_batch(run, digest)
                def changed(ref):
                    result = resolve(ref)
                    UserLibraryObject.objects.filter(pk=item.pk).update(contentGeneration=2)
                    return result
                return changed

            with patch("app_core.agent_work.input_storage_batch", side_effect=validate_then_change):
                response = self.post(event)
            self.assertEqual(response.status_code, 201, response.content)
            child = AgentRun.objects.get(pk=response.json()["operation"]["agentRunId"])
            asset = child.authorization.payload["assetRefs"][0]
            self.assertEqual(asset["inputIdentity"]["generation"], 1)
            with self.assertRaises(DeferredInputResolutionError):
                resolve_deferred_input(child, asset["inputRef"], child.authorization.digest)

    def test_lost_original_success_does_not_reconstruct_a_different_origin_from_a_duplicate(self):
        from .models import AgentWorkSession
        _, schedule = self.dependencies()
        event = self.request_fact()
        first = self.post(event)
        self.assertEqual(first.status_code, 201, first.content)
        duplicate = self.request_fact()
        AgentWorkSession.objects.get().delete()
        event.delete()
        response = self.post(duplicate)
        self.assertEqual(response.status_code, 409, response.content)
        self.assertFalse(AgentWorkSession.objects.exists())
        self.assertEqual(AgentRun.objects.count(), 2)
        self.assertEqual(HostedOperationReceipt.objects.count(), 1)
        schedule.assert_called_once()

    def test_new_admission_uses_current_model_selection_rules(self):
        _, schedule = self.dependencies()
        for index, changes in enumerate(({"enabled": False}, {"isCurrent": False}, {"thinkingModes": []})):
            with self.subTest(changes=changes):
                ModelConfig.objects.filter(pk=self.model.pk).update(enabled=True, isCurrent=True, thinkingModes=["high"])
                self.run = AgentRun.objects.create(workspace=self.workspace, session=self.source, user=self.user,
                    modelConfig=self.model, thinkingMode="high", prompt="Explicit source model", status="running")
                create_agent_run_authorization(self.run, image_digest=PROFILE["imageDigest"])
                event = self.request_fact(call_id=f"dispatch-model-{index}")
                ModelConfig.objects.filter(pk=self.model.pk).update(**changes)
                self.assertEqual(self.post(event).status_code, 403)
        self.assertEqual(Session.objects.count(), 1)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        schedule.assert_not_called()
