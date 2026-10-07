"""Recovery reads committed sources and leaves admission to the existing consumer."""
import json
import threading
from concurrent.futures import ThreadPoolExecutor
from datetime import timedelta
from types import SimpleNamespace
from unittest.mock import patch
from django.conf import settings
from django.db import close_old_connections, connection, transaction
from django.test import Client
from django.test import TransactionTestCase
from django.test.utils import CaptureQueriesContext
from django.utils import timezone
from . import test_agent_work as fixture


class AgentWorkRecoveryTests(TransactionTestCase):
    serialized_rollback = True
    setUp = fixture.AgentWorkTests.setUp
    request_fact = fixture.AgentWorkTests.request_fact
    dependencies = fixture.AgentWorkTests.dependencies
    post = fixture.AgentWorkTests.post

    def discover(self, after=None, through=None, limit=100):
        return self.client.post("/internal/agent-work/discover", content_type="application/json",
            data=json.dumps({"schema":"workspace.agent_work.discover.v1", "limit":limit,
                             "after":after, "through":through}),
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def test_missed_trigger_discovers_committed_source_without_admitting_it(self):
        from .models import AgentRun, HostedOperationReceipt, Session
        profile, schedule = self.dependencies()
        event = self.request_fact()
        response = self.discover()
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual([entry["sourceEventId"] for entry in response.json()["entries"]], [event.eventId])
        self.assertEqual((Session.objects.count(), AgentRun.objects.count(), HostedOperationReceipt.objects.count()), (1,1,0))
        profile.assert_not_called()
        schedule.assert_not_called()

    def test_receipt_filter_handles_duplicates_missing_binding_and_deleted_child(self):
        from .models import AgentWorkSession, Session
        _, schedule = self.dependencies()
        first = self.request_fact()
        admitted = self.post(first).json()
        duplicate = self.request_fact()
        AgentWorkSession.objects.get().delete()
        Session.objects.filter(id=admitted["operation"]["sessionId"]).update(status="deleted",deletedAt=timezone.now(),deletedBy=self.user,purgedAt=timezone.now())
        page = self.discover().json()
        self.assertEqual([entry["sourceEventId"] for entry in page["entries"]], [None,None])
        self.assertEqual(self.discover().json(), page)
        schedule.assert_called_once()

    def test_receipt_filter_is_exactly_user_workspace_command_and_operation_scoped(self):
        from django.contrib.auth import get_user_model
        from .agent_work import work_operation_id
        from .models import HostedOperationReceipt
        event = self.request_fact()
        foreign = get_user_model().objects.create_user(username="foreign-receipt")
        HostedOperationReceipt.objects.create(user=foreign, workspace=self.workspace, command="submitMessage",
            operationId=work_operation_id(self.run.id,self.run.turn_id,"dispatch-one"), requestDigest="f"*64,
            sessionId=self.source.id, agentRunId=self.run.id, turnId=self.run.turn_id)
        self.assertEqual(self.discover().json()["entries"][0]["sourceEventId"], event.eventId)

    def test_same_identity_condition_restore_admits_but_new_membership_cannot(self):
        from .models import AgentRun, HostedOperationReceipt, WorkspaceMembership
        _, schedule = self.dependencies()
        event = self.request_fact()
        original_authorization = (self.run.authorization.id,self.run.authorization.digest,self.run.authorization.signature)
        self.user.is_active = False
        self.user.save(update_fields=["is_active"])
        self.assertEqual(self.post(event).status_code, 403)
        self.assertEqual(self.discover().json()["entries"][0]["sourceEventId"], event.eventId)
        self.user.is_active = True
        self.user.save(update_fields=["is_active"])
        self.assertEqual(self.post(event).status_code, 201)
        other = self.request_fact(call_id="still-pending")
        old_membership = self.membership.id
        self.membership.delete()
        new_member = WorkspaceMembership.objects.create(workspace=self.workspace,user=self.user,role="owner")
        self.assertNotEqual(new_member.id,old_membership)
        self.assertEqual(self.post(other).status_code,403)
        self.run.refresh_from_db()
        self.assertEqual(self.run.membership_ref,old_membership)
        self.assertEqual((self.run.authorization.id,self.run.authorization.digest,self.run.authorization.signature),original_authorization)
        self.assertEqual((AgentRun.objects.count(),HostedOperationReceipt.objects.count()),(2,1))
        schedule.assert_called_once()

    def test_same_model_binding_reenabled_allows_current_materializer_admission(self):
        from .models import ModelConfig
        _, schedule = self.dependencies()
        event = self.request_fact()
        ModelConfig.objects.filter(pk=self.model.pk).update(enabled=False)
        self.assertEqual(self.post(event).status_code,403)
        ModelConfig.objects.filter(pk=self.model.pk).update(enabled=True)
        self.assertEqual(self.post(event).status_code,201)
        schedule.assert_called_once()

    def test_access_restore_requires_original_input_generation_and_identity(self):
        import hashlib
        import tempfile
        from django.core.files.base import ContentFile
        from django.core.files.storage import default_storage
        from django.test import override_settings
        from .agent_run_authorization_factory import create_agent_run_authorization
        from .models import AgentRun,SessionAssetLink,UserLibraryObject,HostedOperationReceipt
        _, schedule = self.dependencies()
        with tempfile.TemporaryDirectory(prefix="recovery-input-") as storage, override_settings(MEDIA_ROOT=storage):
            content=b"original input"
            key=default_storage.save("recovery/input.txt",ContentFile(content))
            item=UserLibraryObject.objects.create(owner=self.user,displayName="input.txt",objectKind="file",
                contentType="text/plain",storageKey=key,sizeBytes=len(content),contentGeneration=1,
                sha256="sha256:"+hashlib.sha256(content).hexdigest(),status="ready")
            link=SessionAssetLink.objects.create(workspace=self.workspace,session=self.source,userLibraryObject=item,
                attachedBy=self.user,capturedDisplayName=item.displayName,capturedContentType=item.contentType,
                capturedOwnerKind="userLibraryObject",capturedOwnerId=item.id,capturedContentGeneration=1,
                capturedSizeBytes=item.sizeBytes,capturedSha256=item.sha256)
            self.run=AgentRun.objects.create(workspace=self.workspace,session=self.source,user=self.user,
                modelConfig=self.model,prompt="Original input")
            create_agent_run_authorization(self.run,image_digest=fixture.PROFILE["imageDigest"])
            self.args["file_refs"]=[link.id]
            event=self.request_fact()
            UserLibraryObject.objects.filter(pk=item.pk).update(status="processing")
            self.assertEqual(self.post(event).status_code,403)
            UserLibraryObject.objects.filter(pk=item.pk).update(status="ready")
            self.assertEqual(self.post(event).status_code,201)
            stale=self.request_fact(call_id="stale-version")
            UserLibraryObject.objects.filter(pk=item.pk).update(contentGeneration=2)
            self.assertEqual(self.post(stale).status_code,403)
            self.assertEqual(HostedOperationReceipt.objects.count(),1)
            schedule.assert_called_once()

    def test_rewritten_deleted_and_untrusted_sources_never_create_work(self):
        from .models import HostedOperationReceipt, Session, SessionEvent
        _, schedule = self.dependencies()
        first = self.request_fact()
        SessionEvent.objects.filter(pk=first.pk).update(projects_to_agent_run_stream=False)
        bad = self.request_fact(call_changes={"providerId":"foreign"},call_id="bad")
        deleted = self.request_fact(call_id="deleted")
        Session.objects.filter(pk=self.source.pk).update(status="deleted",deletedAt=timezone.now(),deletedBy=self.user,purgedAt=timezone.now())
        page = self.discover().json()
        self.assertEqual([entry["cursor"]["eventId"] for entry in page["entries"]],[bad.eventId,deleted.eventId])
        self.assertIsNone(page["entries"][0]["sourceEventId"])
        self.assertEqual(self.post(deleted).status_code,403)
        self.assertFalse(HostedOperationReceipt.objects.exists())
        schedule.assert_not_called()

    def test_frozen_upper_finishes_despite_new_sources_and_next_pass_revisits_old(self):
        events = [self.request_fact(call_id=f"call-{i}") for i in range(4)]
        page = self.discover(limit=2).json()
        upper = page["through"]
        newer = self.request_fact(call_id="newer")
        second = self.discover(after=page["next"],through=upper,limit=2).json()
        self.assertEqual([e["sourceEventId"] for e in second["entries"]],[e.eventId for e in events[2:]])
        self.assertIsNone(second["next"])
        next_pass = self.discover().json()
        self.assertEqual([e["sourceEventId"] for e in next_pass["entries"]],[e.eventId for e in events]+[newer.eventId])

    def test_late_commit_before_walked_cursor_is_seen_on_next_pass(self):
        from .agent_run_authorization_factory import create_agent_run_authorization
        from .models import Agent,AgentCoordinationSession,AgentRun,Session
        first = self.request_fact(call_id="first")
        agent = Agent.objects.create(workspace=self.workspace,owner=self.user,name="Other native")
        source = Session.objects.create(workspace=self.workspace,owner=self.user,agent=agent)
        AgentCoordinationSession.objects.create(agent=agent,session=source)
        run = AgentRun.objects.create(workspace=self.workspace,session=source,user=self.user,modelConfig=self.model,prompt="Other")
        create_agent_run_authorization(run,image_digest=fixture.PROFILE["imageDigest"])
        other = SimpleNamespace(source=source,run=run,workspace=self.workspace,args=self.args)
        def walk():
            close_old_connections()
            try:
                newest = fixture.AgentWorkTests.request_fact(other,call_id="newest")
                client = Client()
                def page(after=None,through=None):
                    return client.post("/internal/agent-work/discover",content_type="application/json",
                        data=json.dumps({"schema":"workspace.agent_work.discover.v1","limit":1,"after":after,"through":through}),
                        HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN).json()
                a = page()
                b = page(a["next"],a["through"])
                self.assertEqual([a["entries"][0]["sourceEventId"],b["entries"][0]["sourceEventId"]],[first.eventId,newest.eventId])
                self.assertIsNone(b["next"])
            finally:
                close_old_connections()
        with transaction.atomic():
            late = self.request_fact(call_id="late")
            with ThreadPoolExecutor(max_workers=1) as pool:
                pool.submit(walk).result(timeout=15)
        self.assertIn(late.eventId,[e["sourceEventId"] for e in self.discover().json()["entries"]])

    def test_schedule_failure_remains_admitted_and_existing_lifecycle_repairs_it(self):
        from .models import AgentRun,HostedOperationReceipt
        _, schedule = self.dependencies()
        schedule.side_effect = RuntimeError("scheduler down")
        event = self.request_fact()
        operation = self.post(event).json()["operation"]
        self.assertIsNone(self.discover().json()["entries"][0]["sourceEventId"])
        with patch("app_core.http.internal.get_runtime_job",return_value=None), \
             patch("app_core.http.internal.schedule_agent_run_lifecycle",return_value="inserted") as repaired:
            response = self.client.post("/internal/agent-run-lifecycle/reconcile",content_type="application/json",
                data=json.dumps({"schema":"runtime.agent_run_lifecycle.reconcile.v1","limit":100}),
                HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(response.status_code,200,response.content)
        self.assertIn(operation["agentRunId"],[call.args[0].id for call in repaired.call_args_list])
        self.assertEqual((AgentRun.objects.count(),HostedOperationReceipt.objects.count()),(2,1))

    def test_discovery_has_strict_transport_and_cursors(self):
        self.assertEqual(self.discover(limit=True).status_code,400)
        self.assertEqual(self.discover(limit=101).status_code,400)
        self.assertEqual(self.discover(after={"insertedAt":"naive","eventId":"x"}).status_code,400)
        response = self.client.post("/internal/agent-work/discover",content_type="application/json",data="{}")
        self.assertEqual(response.status_code,401)

    def test_actual_orm_predicate_uses_partial_index_without_forcing_planner(self):
        from .agent_work_recovery import source_page
        from .models import SessionEvent
        event = self.request_fact()
        padding = [SessionEvent(eventId=f"recovery-pad-{i}",workspace=self.workspace,session=self.source,
            agent_run=self.run,sequence=i+10,agent_run_sequence=i+10,createdAtMs=i,
            projects_to_agent_run_stream=True,payload={"type":"tool_call","payload":{}}) for i in range(2048)]
        SessionEvent.objects.bulk_create(padding)
        after = (event.insertedAt - timedelta(microseconds=1), event.eventId)
        through = (event.insertedAt, event.eventId)
        with CaptureQueriesContext(connection) as queries:
            rows, upper, next_cursor = source_page(after, through, 100)
        self.assertEqual(len(queries), 1)
        sql = queries[0]["sql"]
        with connection.cursor() as cursor:
            cursor.execute("ANALYZE app_core_sessionevent")
            cursor.execute("EXPLAIN (FORMAT JSON) " + sql)
            plan = cursor.fetchone()[0]
        self.assertIn("agent_work_recovery_scan",json.dumps(plan))
        self.assertEqual(rows[0]["eventId"],event.eventId)
        self.assertEqual(upper["eventId"],event.eventId)
        self.assertIsNone(next_cursor)
        print("work-recovery-index-plan: " + json.dumps({"sql":sql,"plan":plan}))
