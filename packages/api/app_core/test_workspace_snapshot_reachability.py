"""Reachability, grace and ownership determine Session snapshot reclamation."""
from copy import deepcopy
from datetime import timedelta
import hashlib
import json
import os
from unittest.mock import patch

from django.conf import settings
from django.db import connection
from django.test import TransactionTestCase

from .agent_run_authorization_factory import create_agent_run_authorization
from .http import internal
from .models import Agent, Session, WorkspaceSnapshotObject, WorkspaceSnapshotUpload
from .runtime_schema_test_helpers import isolate_runtime_schema
from . import test_workspace_snapshot_gc as fixtures
from .workspace_snapshot_gc import collect_workspace_snapshot_gc
from .test_execution_publication_fencing import prepare_workspace_files, post_workspace_snapshot


class WorkspaceSnapshotReachabilityTests(TransactionTestCase):
    serialized_rollback = True
    create_owner = fixtures.WorkspaceSnapshotGcTests.create_owner
    store = fixtures.WorkspaceSnapshotGcTests.store
    snapshot = fixtures.WorkspaceSnapshotGcTests.snapshot
    checkpoint = fixtures.WorkspaceSnapshotGcTests.checkpoint

    def setUp(self):
        fixtures.WorkspaceSnapshotGcTests.setUp(self)
        self.old = self.now - timedelta(days=35)
        self.session, self.run = self.create_owner()
        self.manifest = {"schema":"runtime.workspace_snapshot.roots.v1","sessionId":self.session.pk,
            "sessionSequence":0,"authorityFingerprint":"sha256:"+"a"*64,"roots":[],"protectedAgentRunIds":[]}
        self.enterContext(patch("app_core.workspace_snapshot_roots.current_witness", return_value=(0,"sha256:"+"a"*64)))
        def roots(session):
            self.assertFalse(connection.in_atomic_block, "Runtime root collection must precede API owner locks")
            return deepcopy(self.manifest | {"sessionId":session.pk})
        self.enterContext(patch("app_core.workspace_snapshot_reachability_gc.request_workspace_snapshot_roots", side_effect=roots))
        isolate_runtime_schema(self)
        with connection.cursor() as cursor:
            cursor.execute("CREATE SCHEMA runtime")
            cursor.execute("CREATE TABLE runtime.runtime_jobs(job_id text PRIMARY KEY,job_kind text,status text,"
                "lease_owner text,lease_expires_at_ms bigint,idempotency_key text,session_id text,payload_ref text,"
                "lease_purpose text DEFAULT 'execute')")
            cursor.execute("CREATE TABLE runtime.execution_instances(execution_id text,allocation_execution_id text,"
                "instance_id text,environment_id text,session_id text,host_id text,profile_digest text)")
            cursor.execute("CREATE TABLE runtime.resident_sandboxes(execution_id text,instance_id text,host_id text,"
                "environment_id text,container_id text,workspace_id text)")

    def collect(self, *, dry_run=False):
        return collect_workspace_snapshot_gc(self.now-timedelta(days=1),dry_run)

    def live_upload_job(self):
        from .models import AgentRun
        self.authorization=create_agent_run_authorization(self.run,image_digest="sha256:"+"a"*64)
        self.job_id=f"agent_run.lifecycle:{self.run.pk}"
        self.owner="worker:upload-root-fixture"
        AgentRun.objects.filter(pk=self.run.pk).update(status="running")
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s,'execute')",
                [self.job_id,self.owner,f"{self.job_id}:{self.authorization.digest}",self.session.pk,f"record:agent_run:{self.run.pk}"])

    def test_abandoned_publication_does_not_root_orphaned_final_under_live_job(self):
        from .test_execution_publication_fencing import ExecutionPublicationFixture
        self.live_upload_job()
        body,_=ExecutionPublicationFixture._body(self)
        link=internal._link_workspace_snapshot
        def abort_after_link(*args):
            self.assertTrue(link(*args))
            raise RuntimeError("simulated transaction rollback after immutable link")
        with patch.object(internal,"_link_workspace_snapshot",abort_after_link):
            response=post_workspace_snapshot(self,body)
        self.assertEqual(response.status_code,500,response.content)
        upload=WorkspaceSnapshotUpload.objects.get(agent_run=self.run, finalObject__kind="snapshot")
        self.assertEqual(upload.state,"abandoned")
        final=upload.finalObject.storageKey
        self.assertTrue((self.root/final).exists())
        WorkspaceSnapshotObject.objects.update(createdAt=self.old,updatedAt=self.old)
        WorkspaceSnapshotUpload.objects.update(updatedAt=self.old)
        os.utime(self.root/final,(self.old.timestamp(),self.old.timestamp()))
        self.assertEqual(self.collect().cleaned,[final])

    def test_pending_stage_survives_finished_http_and_replacement_cleanup_lease(self):
        from .test_execution_publication_fencing import ExecutionPublicationFixture
        self.live_upload_job()
        body,snapshot=ExecutionPublicationFixture._body(self)
        metadata_size=int.from_bytes(body[:4],"big")
        metadata=json.loads(body[4:4+metadata_size])
        metadata.update(schema=internal.EXECUTION_WORKSPACE_STAGE_SCHEMA,checkpointId="checkpoint:pending-handoff")
        encoded=json.dumps(metadata,separators=(",", ":")).encode()
        body=len(encoded).to_bytes(4,"big")+encoded+snapshot
        response=post_workspace_snapshot(self,body,"/internal/agent-runs/execution-workspace/stage")
        self.assertEqual(response.status_code,201,response.content)
        upload=WorkspaceSnapshotUpload.objects.get(agent_run=self.run, finalObject__kind="snapshot")
        self.assertEqual(upload.state,"staged")
        self.assertIsNotNone(upload.finishedAt)
        final=upload.finalObject.storageKey
        WorkspaceSnapshotObject.objects.update(createdAt=self.old,updatedAt=self.old)
        WorkspaceSnapshotUpload.objects.update(updatedAt=self.old)
        os.utime(self.root/final,(self.old.timestamp(),self.old.timestamp()))
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_owner='worker:replacement-stage-owner' WHERE job_id=%s",[self.job_id])
        self.assertEqual(self.collect().cleaned,[])
        self.assertTrue((self.root/final).exists(),"an HTTP-complete stage without a Core reference remains pending across lease handoff")
        self.assertTrue(WorkspaceSnapshotUpload.objects.filter(pk=upload.pk).exists())

    def test_active_session_retains_only_current_head_and_selected_runtime_root(self):
        old = self.snapshot(generation=1, content=b"unreachable old generation")
        head = self.snapshot(generation=2, content=b"current head",current=True)
        selected = self.checkpoint(content=b"selected recovery")
        orphan = self.checkpoint(content=b"unselected historical recovery",checkpoint_id="checkpoint:old")
        self.manifest["roots"] = [{"objectRef":selected}]
        report = self.collect()
        self.assertEqual(set(report.cleaned),{old,orphan})
        self.assertEqual(report.failures,[])
        self.assertTrue((self.root/head).exists())
        self.assertTrue((self.root/selected).exists())
        self.assertFalse((self.root/old).exists())
        self.assertFalse((self.root/orphan).exists())
        self.assertEqual(set(WorkspaceSnapshotObject.objects.filter(state="collected").values_list("storageKey",flat=True)),{old,orphan})

    def test_protected_run_frozen_baseline_survives_but_settled_history_does_not(self):
        frozen = self.snapshot(generation=1,content=b"frozen baseline",current=True)
        grant = create_agent_run_authorization(self.run,image_digest="sha256:"+"a"*64)
        head = self.snapshot(generation=2,content=b"new head",current=True)
        self.manifest["protectedAgentRunIds"]=[self.run.pk]
        self.assertEqual(self.collect().cleaned,[])
        self.assertTrue((self.root/frozen).exists())
        self.manifest["protectedAgentRunIds"]=[]
        self.assertEqual(self.collect().cleaned,[frozen])
        self.assertTrue((self.root/head).exists())
        grant.refresh_from_db()
        self.assertEqual(grant.payload["sessionWorkspace"]["generation"],1)

    def test_unknown_baseline_and_stale_runtime_witness_fail_closed(self):
        key=self.snapshot()
        Session.objects.filter(pk=self.session.pk).update(workspaceBaselineKnown=False)
        self.assertEqual(self.collect().cleaned,[])
        self.assertTrue((self.root/key).exists())
        Session.objects.filter(pk=self.session.pk).update(workspaceBaselineKnown=True)
        with patch("app_core.workspace_snapshot_roots.current_witness",return_value=(1,"sha256:"+"b"*64)):
            report=self.collect()
        self.assertEqual(report.cleaned,[])
        self.assertTrue((self.root/key).exists())
        self.assertFalse(WorkspaceSnapshotObject.objects.exists())

    def test_witness_change_after_selection_aborts_unlink(self):
        key=self.snapshot()
        with patch("app_core.workspace_snapshot_roots.current_witness",side_effect=[(0,"sha256:"+"a"*64),(1,"sha256:"+"b"*64)]):
            report=self.collect()
        self.assertEqual(report.cleaned,[])
        self.assertTrue((self.root/key).exists())
        self.assertEqual(WorkspaceSnapshotObject.objects.get(storageKey=key).state,"collected")

    def test_preservation_receipt_roots_only_exact_holding_or_outstanding_cleanup(self):
        from .models import SessionWorkspacePreservation
        create_agent_run_authorization(self.run,image_digest="sha256:"+"a"*64)
        preserved=self.snapshot(generation=1,content=b"preserved stopped bytes")
        head=self.snapshot(generation=2,content=b"later committed head",current=True)
        environment=self.session.current_environment
        receipt=SessionWorkspacePreservation.objects.create(id="sha256:"+"c"*64,session=self.session,
            agent_run=self.run,origin_agent_run=self.run,environment=environment,
            authorization_digest=self.run.authorization.digest,origin_authorization_digest=self.run.authorization.digest,
            allocation_execution_id="original",instance_id="instance_"+"d"*64,container_id="e"*64,
            profile_digest=environment.environment_profile_digest,baseline=self.run.authorization.payload["sessionWorkspace"],
            workspace={"generation":1},storage_key=preserved,creator_lease_digest="sha256:"+"f"*64,creator_controller_epoch=1)
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.execution_instances VALUES('original','original',%s,%s,%s,'host',%s)",
                [receipt.instance_id,environment.pk,self.session.pk,receipt.profile_digest])
            cursor.execute("INSERT INTO runtime.resident_sandboxes VALUES('original',%s,'host',%s,%s,%s)",
                [receipt.instance_id,environment.pk,receipt.container_id,self.workspace.pk])
        self.assertEqual(self.collect().cleaned,[])
        with connection.cursor() as cursor:
            cursor.execute("DELETE FROM runtime.resident_sandboxes WHERE execution_id='original'")
        self.manifest["protectedAgentRunIds"]=[self.run.pk]
        self.assertEqual(self.collect().cleaned,[])
        self.manifest["protectedAgentRunIds"]=[]
        self.assertEqual(self.collect().cleaned,[preserved])
        self.assertTrue((self.root/head).exists())
        self.assertTrue(SessionWorkspacePreservation.objects.filter(pk=receipt.pk).exists())

    def test_grace_and_dry_run_leave_bytes_and_publication_state_unchanged(self):
        old=self.snapshot()
        fresh=self.snapshot(generation=2,content=b"fresh",modified=self.now)
        preview=self.collect(dry_run=True)
        self.assertEqual(preview.planned,[old])
        self.assertFalse(WorkspaceSnapshotObject.objects.exists())
        self.assertTrue((self.root/old).exists())
        self.assertTrue((self.root/fresh).exists())
        self.assertEqual(self.collect().cleaned,[old])
        self.assertTrue((self.root/fresh).exists())

    def test_live_stream_is_protected_but_completed_commit_relies_on_reachable_roots(self):
        self.authorization=create_agent_run_authorization(self.run,image_digest="sha256:"+"a"*64)
        self.job_id=f"agent_run.lifecycle:{self.run.pk}"
        self.owner="worker:streaming-gc-owner"
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s,'execute')",
                [self.job_id,self.owner,f"{self.job_id}:{self.authorization.digest}",self.session.pk,f"record:agent_run:{self.run.pk}"])
        from .test_execution_publication_fencing import ExecutionPublicationFixture
        body,snapshot=ExecutionPublicationFixture._body(self)
        from .models import AgentRun
        AgentRun.objects.filter(pk=self.run.pk).update(status="running")
        store=internal._store_workspace_snapshot
        observed=[]
        def during_stream(*args,**kwargs):
            result=store(*args,**kwargs)
            if not observed:
                upload=WorkspaceSnapshotUpload.objects.get(agent_run=self.run, finalObject__kind="snapshot")
                WorkspaceSnapshotObject.objects.update(createdAt=self.old,updatedAt=self.old)
                WorkspaceSnapshotUpload.objects.update(createdAt=self.old,updatedAt=self.old)
                os.utime(self.root/upload.temporaryObject.storageKey,(self.old.timestamp(),self.old.timestamp()))
                observed.append(self.collect())
            return result
        with patch.object(internal,"_store_workspace_snapshot",during_stream):
            response=post_workspace_snapshot(self,body)
        self.assertEqual(response.status_code,201,response.content)
        self.assertEqual(observed[0].cleaned,[])
        self.session.refresh_from_db()
        final=self.session.workspaceStorageKey
        # Remove the committed head reference to model a later authoritative empty head.
        Session.objects.filter(pk=self.session.pk).update(workspaceStorageKey="",workspaceSnapshotSha256="",workspaceSnapshotEncoding="frameV1",
            workspaceSnapshotSizeBytes=0,workspaceExpandedSizeBytes=0,workspaceFileCount=0)
        os.utime(self.root/final,(self.old.timestamp(),self.old.timestamp()))
        WorkspaceSnapshotObject.objects.filter(storageKey=final).update(createdAt=self.old,updatedAt=self.old)
        WorkspaceSnapshotUpload.objects.update(updatedAt=self.old)
        self.assertEqual(self.collect().cleaned,[final])

    def test_collected_identical_content_can_be_committed_by_a_new_run_and_collected_again(self):
        from .models import AgentRun,StorageCleanupRetry
        from .test_execution_publication_fencing import ExecutionPublicationFixture
        self.authorization=create_agent_run_authorization(self.run,image_digest="sha256:"+"a"*64)
        self.job_id=f"agent_run.lifecycle:{self.run.pk}"
        self.owner="worker:first-orphaned-upload"
        _,snapshot=ExecutionPublicationFixture._body(self)
        old=self.snapshot(content=snapshot)
        self.assertEqual(self.collect().cleaned,[old])
        old_token=WorkspaceSnapshotObject.objects.get(storageKey=old).publicationToken
        self.run=AgentRun.objects.create(workspace=self.workspace,session=self.session,user=self.session.owner,
            modelConfig=self.model,prompt="same bytes after old attempt settled",status="running")
        self.authorization=create_agent_run_authorization(self.run,image_digest="sha256:"+"a"*64)
        self.job_id=f"agent_run.lifecycle:{self.run.pk}"
        self.owner="worker:fresh-identical-content"
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s,'execute')",
                [self.job_id,self.owner,f"{self.job_id}:{self.authorization.digest}",self.session.pk,f"record:agent_run:{self.run.pk}"])
        body,same=ExecutionPublicationFixture._body(self)
        self.assertEqual(same,snapshot)
        response=post_workspace_snapshot(self,body)
        self.assertEqual(response.status_code,201,response.content)
        object=WorkspaceSnapshotObject.objects.get(storageKey=old)
        self.assertNotEqual(object.publicationToken,old_token)
        self.assertEqual(StorageCleanupRetry.objects.get(storageKey=old).state,"pending")
        self.assertTrue((self.root/old).exists())
        Session.objects.filter(pk=self.session.pk).update(workspaceGeneration=2,workspaceStorageKey="",workspaceSnapshotSha256="",workspaceSnapshotEncoding="frameV1",
            workspaceSnapshotSizeBytes=0,workspaceExpandedSizeBytes=0,workspaceFileCount=0)
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=0 WHERE job_id=%s",[self.job_id])
        WorkspaceSnapshotObject.objects.update(createdAt=self.old,updatedAt=self.old)
        WorkspaceSnapshotUpload.objects.update(updatedAt=self.old)
        os.utime(self.root/old,(self.old.timestamp(),self.old.timestamp()))
        self.assertEqual(self.collect().cleaned,[old])
        retry=StorageCleanupRetry.objects.get(storageKey=old)
        self.assertEqual((retry.state,retry.publicationToken),("cleaned",object.publicationToken))

    def test_expired_request_and_absent_temporary_metadata_retire_after_grace(self):
        from .models import AgentRun
        from .test_execution_publication_fencing import ExecutionPublicationFixture
        self.authorization=create_agent_run_authorization(self.run,image_digest="sha256:"+"a"*64)
        self.job_id=f"agent_run.lifecycle:{self.run.pk}"
        self.owner="worker:metadata-retirement"
        AgentRun.objects.filter(pk=self.run.pk).update(status="running")
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s,'execute')",
                [self.job_id,self.owner,f"{self.job_id}:{self.authorization.digest}",self.session.pk,f"record:agent_run:{self.run.pk}"])
        body,_=ExecutionPublicationFixture._body(self)
        response=post_workspace_snapshot(self,body)
        self.assertEqual(response.status_code,201,response.content)
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_expires_at_ms=0 WHERE job_id=%s",[self.job_id])
        WorkspaceSnapshotUpload.objects.update(updatedAt=self.old)
        WorkspaceSnapshotObject.objects.filter(kind="temporary").update(collectedAt=self.old,updatedAt=self.old)
        self.collect()
        self.assertFalse(WorkspaceSnapshotUpload.objects.exists())
        self.assertFalse(WorkspaceSnapshotObject.objects.filter(kind="temporary").exists())

    def test_absent_temporary_metadata_retires_even_after_all_upload_rows_are_gone(self):
        key=f"workspaces/{self.workspace.pk}/sessions/{self.session.pk}/snapshots/1/.centaeris-immutable-abcdefgh.tmp"
        WorkspaceSnapshotObject.objects.create(keyDigest=hashlib.sha256(key.encode()).hexdigest(),storageKey=key,
            session=self.session,kind="temporary",state="collected",collectedAt=self.old,registered=True)
        self.assertFalse(WorkspaceSnapshotUpload.objects.exists())
        self.collect()
        self.assertFalse(WorkspaceSnapshotObject.objects.filter(storageKey=key).exists())
