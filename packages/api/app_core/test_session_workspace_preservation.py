"""Stopped-instance preservation uses cleanup authority and the normal Session CAS."""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
from unittest.mock import patch

from django.conf import settings
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import connection
from django.test import TransactionTestCase

from .environment_authorization import build_environment_authorization
from .agent_run_authorization_factory import create_agent_run_authorization
from .environment_lifecycle import acquire_environment_cleanup_controller, acquire_environment_controller, release_environment_controller
from .models import AgentRun, Session, SessionWorkspacePreservation
from .runtime_contract import session_workspace_for_session
from .test_execution_publication_fencing import ExecutionPublicationFixture, prepare_workspace_files


class SessionWorkspacePreservationTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True
    def setUp(self):
        super().setUp()
        self.allocation, self.instance, self.container = "original-allocation", "instance_" + "a" * 64, "b" * 64
        self.original_grant = deepcopy((self.authorization.payload, self.authorization.digest, self.authorization.signature))
        self.baseline = session_workspace_for_session(self.session)
        self.execute = acquire_environment_controller(self.run.pk, self.owner)
        AgentRun.objects.filter(pk=self.run.pk).update(status="failed")
        self.cleanup = acquire_environment_cleanup_controller(self.run.pk, self.owner)
        self.run.refresh_from_db()
        self.environment_authorization = build_environment_authorization(self.run, self.authorization,
            self.cleanup, execution_mode="retained")
        Session.objects.filter(pk=self.session.pk).update(workspaceBaselineKnown=False)
        with connection.cursor() as cursor:
            cursor.execute("CREATE TABLE runtime.execution_instances(execution_id text PRIMARY KEY, "
                "allocation_execution_id text,instance_id text,environment_id text,session_id text,agent_run_id text,"
                "host_id text,authorization_digest text,image_digest text,profile_digest text,allocation_mode text)")
            cursor.execute("CREATE TABLE runtime.resident_sandboxes(execution_id text PRIMARY KEY,instance_id text,"
                "environment_id text,host_id text,workspace_id text,container_id text,state text,allocation_mode text)")
            cursor.execute("INSERT INTO runtime.execution_instances VALUES(%s,%s,%s,%s,%s,%s,'host',%s,%s,%s,'retained')",
                [self.allocation,self.allocation,self.instance,self.run.environment_id,self.session.pk,self.run.pk,
                 self.authorization.digest,self.authorization.payload["environmentProfile"]["imageDigest"],
                 self.authorization.payload["environmentProfileDigest"]])
            cursor.execute("INSERT INTO runtime.resident_sandboxes VALUES(%s,%s,%s,'host',%s,%s,'resident','retained')",
                [self.allocation,self.instance,self.run.environment_id,self.session.workspace_id,self.container])
        self.addCleanup(self._drop_allocation)

    def _drop_allocation(self):
        with connection.cursor() as cursor:
            cursor.execute("DROP TABLE runtime.resident_sandboxes")
            cursor.execute("DROP TABLE runtime.execution_instances")


    def preserve_body(self, content=b"stopped workspace", **changes):
        snapshot = b""
        if content is not None:
            manifest = json.dumps({"schema":"workspace.snapshot.v1","files":[{"path":"notes.txt",
                "sizeBytes":len(content),"sha256":"sha256:"+hashlib.sha256(content).hexdigest(),"executable":False}]},
                separators=(",",":")).encode()
            snapshot = manifest
            contents = getattr(self, "_workspace_file_contents", {})
            contents["sha256:"+hashlib.sha256(content).hexdigest()] = content
            self._workspace_file_contents = contents
        metadata = {"schema":"runtime.session_workspace.preserve.v2","jobId":self.job_id,"leaseOwner":self.owner,
            "agentRunId":self.run.pk,"authorizationDigest":self.authorization.digest,
            "environmentAuthorization":self.environment_authorization,"allocationExecutionId":self.allocation,
            "instanceId":self.instance,"containerId":self.container,
            "profileDigest":self.authorization.payload["environmentProfileDigest"],"baseline":self.baseline,
            "snapshotSha256":"sha256:"+hashlib.sha256(snapshot).hexdigest() if snapshot else "",
            "snapshotEncoding":"fileManifestV1" if snapshot else "frameV1",
            "snapshotSizeBytes":len(snapshot),"expandedSizeBytes":len(content) if content is not None else 0,
            "fileCount":1 if content is not None else 0,**changes}
        encoded=json.dumps(metadata,separators=(",",":")).encode()
        return len(encoded).to_bytes(4,"big")+encoded+snapshot,snapshot

    def preserve(self, body):
        prepare_workspace_files(self, body)
        return self.client.post("/internal/agent-runs/session-workspace/preserve",data=body,
            content_type="application/octet-stream",HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def legacy_archive_baseline(self, *, cleanup):
        path = Path(__file__).resolve().parents[3] / "tests/workspace/fixtures/workspace_archive/v1/valid.json"
        vector = json.loads(path.read_text(encoding="utf-8"))
        archive = bytes.fromhex(vector["tarZstdV1Hex"])
        digest = "sha256:" + hashlib.sha256(archive).hexdigest()
        key = f"workspaces/{self.session.workspace_id}/sessions/{self.session.pk}/snapshots/1/{digest[7:]}.snapshot"
        self.assertEqual(default_storage.save(key, ContentFile(archive)), key)
        Session.objects.filter(pk=self.session.pk).update(workspaceGeneration=1, workspaceBaselineKnown=True,
            workspaceSnapshotEncoding="tarZstdV1", workspaceStorageKey=key, workspaceSnapshotSha256=digest,
            workspaceSnapshotSizeBytes=len(archive), workspaceFileCount=len(vector["manifest"]["files"]),
            workspaceExpandedSizeBytes=sum(file["sizeBytes"] for file in vector["manifest"]["files"]),
            workspaceLastAdvancedAgentRun=self.run)
        self.session.refresh_from_db()
        release_environment_controller(self.run.pk, self.cleanup["controllerEpoch"], self.owner)
        prior_job = self.job_id
        def remove_prior_job():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s", [prior_job])
        self.addCleanup(remove_prior_job)
        self.run = AgentRun.objects.create(workspace=self.run.workspace, session=self.session, user=self.run.user,
            modelConfig=self.run.modelConfig, prompt="replace archive baseline", status="failed" if cleanup else "running")
        self.authorization = create_agent_run_authorization(self.run, image_digest="sha256:" + "a" * 64)
        self.job_id = f"agent_run.lifecycle:{self.run.pk}"
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,lease_expires_at_ms,"
                "idempotency_key,session_id,payload_ref) VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                [self.job_id,self.owner,f"{self.job_id}:{self.authorization.digest}",self.session.pk,
                 f"record:agent_run:{self.run.pk}"])
        self.baseline = session_workspace_for_session(self.session)
        if cleanup:
            self.cleanup = acquire_environment_cleanup_controller(self.run.pk, self.owner)
            self.environment_authorization = build_environment_authorization(self.run, self.authorization,
                self.cleanup, execution_mode="perRun")
        return key, archive

    def assert_snapshot_publication(self, *, preserve, content):
        original_key, archive = self.legacy_archive_baseline(cleanup=preserve)
        body, snapshot = self.preserve_body(content)
        if preserve:
            response = self.preserve(body)
            self.assertEqual(response.status_code, 200, response.content)
        else:
            from .http import internal
            length = int.from_bytes(body[:4], "big")
            source = json.loads(body[4:4 + length])
            metadata = {name: source[name] for name in internal.SESSION_WORKSPACE_COMMIT_FIELDS}
            metadata["schema"] = "runtime.session_workspace.commit.v2"
            encoded = json.dumps(metadata, separators=(",", ":")).encode()
            response = self._post(len(encoded).to_bytes(4, "big") + encoded + snapshot)
            self.assertEqual(response.status_code, 201, response.content)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 2)
        self.assertEqual(self.session.workspaceSnapshotEncoding, "fileManifestV1" if content is not None else "frameV1")
        self.assertEqual(response.json()["sessionWorkspace"], session_workspace_for_session(self.session))
        if preserve:
            receipt = SessionWorkspacePreservation.objects.get(pk=response.json()["preservationReceiptId"])
            self.assertEqual(receipt.snapshot_encoding, "fileManifestV1" if content is not None else "frameV1")
            self.assertEqual(receipt.workspace, response.json()["sessionWorkspace"])
        with default_storage.open(original_key, "rb") as stored:
            self.assertEqual(stored.read(), archive)

    def test_regular_manifest_commit_replaces_legacy_archive_encoding(self):
        self.assert_snapshot_publication(preserve=False, content=b"new frame")

    def test_regular_empty_commit_replaces_legacy_archive_encoding(self):
        self.assert_snapshot_publication(preserve=False, content=None)

    def test_preserved_manifest_replaces_legacy_archive_encoding(self):
        self.assert_snapshot_publication(preserve=True, content=b"new stopped frame")

    def test_preserved_empty_root_replaces_legacy_archive_encoding(self):
        self.assert_snapshot_publication(preserve=True, content=None)

    def test_cleanup_commits_unknown_baseline_and_replays_lost_ack_without_changing_history(self):
        body,snapshot=self.preserve_body()
        receipts = []
        for disposition in ("committed","idempotent"):
            response=self.preserve(body)
            self.assertEqual(response.status_code,200,response.content)
            self.assertEqual(set(response.json()),{"schema","disposition","sessionWorkspace","preservationReceiptId"})
            self.assertEqual(response.json()["schema"],"runtime.session_workspace.preserve.result.v1")
            self.assertEqual(response.json()["disposition"],disposition)
            receipts.append(response.json()["preservationReceiptId"])
        self.assertEqual(receipts[0], receipts[1])
        self.assertEqual(SessionWorkspacePreservation.objects.count(), 1)
        receipt = SessionWorkspacePreservation.objects.get(pk=receipts[0])
        self.assertEqual((receipt.agent_run_id, receipt.origin_agent_run_id, receipt.allocation_execution_id,
            receipt.instance_id, receipt.container_id, receipt.profile_digest),
            (self.run.pk, self.run.pk, self.allocation, self.instance, self.container,
             self.authorization.payload["environmentProfileDigest"]))
        self.assertEqual(receipt.baseline, self.baseline)
        with self.assertRaisesMessage(ValueError, "session_workspace_preservation_immutable"):
            receipt.save()
        self._assert_snapshot(snapshot)
        self.session.refresh_from_db()
        self.assertTrue(self.session.workspaceBaselineKnown)
        self.authorization.refresh_from_db()
        self.assertEqual((self.authorization.payload,self.authorization.digest,self.authorization.signature),self.original_grant)

    def test_execute_authority_and_stale_target_are_rejected_without_baseline_changes(self):
        execute=build_environment_authorization(self.run,self.authorization,self.execute,execution_mode="retained")
        for changes in ({"environmentAuthorization":execute},{"containerId":"c"*64},
                        {"instanceId":"instance_"+"d"*64},{"authorizationDigest":"sha256:"+"e"*64}):
            with self.subTest(changes=changes):
                body,_=self.preserve_body(**changes)
                response=self.preserve(body)
                self.assertEqual(response.status_code,409,response.content)
                self.session.refresh_from_db()
                self.assertFalse(self.session.workspaceBaselineKnown)
                self.assertEqual(self.session.workspaceGeneration,0)

    def test_terminal_cleanup_control_lease_can_preserve_an_observed_empty_root(self):
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_purpose='control' WHERE job_id=%s", [self.job_id])
        body, _ = self.preserve_body(None)
        response = self.preserve(body)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["sessionWorkspace"], {"generation": 1, "snapshotSha256": "",
            "snapshotSizeBytes": 0, "expandedSizeBytes": 0, "fileCount": 0})
        self.session.refresh_from_db()
        self.assertTrue(self.session.workspaceBaselineKnown)
        self.assertEqual(self.session.workspaceStorageKey, "")

    def test_control_cleanup_lease_preserves_and_pins_nonempty_file_content(self):
        from .workspace_file_objects import file_key, live_reservation_keys
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_purpose='control' WHERE job_id=%s", [self.job_id])
        content = b"control cleanup file"
        body, snapshot = self.preserve_body(content)
        response = self.preserve(body)
        self.assertEqual(response.status_code, 200, response.content)
        self._assert_snapshot(snapshot)
        self.assertIn(file_key(self.session, "sha256:"+hashlib.sha256(content).hexdigest()),
                      live_reservation_keys(self.session))

    def test_per_run_cleanup_authority_can_preserve_the_exact_releasing_retained_instance(self):
        self.environment_authorization = build_environment_authorization(self.run, self.authorization,
            self.cleanup, execution_mode="perRun")
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.resident_sandboxes SET state='releasing' WHERE execution_id=%s", [self.allocation])
        body, snapshot = self.preserve_body()
        response = self.preserve(body)
        self.assertEqual(response.status_code, 200, response.content)
        self._assert_snapshot(snapshot)

    def test_later_valid_cleanup_lease_reuses_the_same_receipt(self):
        body, _ = self.preserve_body()
        first = self.preserve(body)
        self.assertEqual(first.status_code, 200, first.content)
        self._lease(self.replacement)
        self.owner = self.replacement
        authority = acquire_environment_cleanup_controller(self.run.pk, self.owner)
        self.environment_authorization = build_environment_authorization(self.run, self.authorization,
            authority, execution_mode="perRun")
        replay, _ = self.preserve_body()
        response = self.preserve(replay)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["disposition"], "idempotent")
        self.assertEqual(response.json()["preservationReceiptId"], first.json()["preservationReceiptId"])

    def test_receipt_failure_rolls_back_the_head_and_authority_marker(self):
        from .http import internal
        body, _ = self.preserve_body()
        with self.assertLogs(internal.logger, level="ERROR"), \
                patch.object(SessionWorkspacePreservation.objects, "get_or_create", side_effect=RuntimeError("receipt store failed")):
            response = self.preserve(body)
        self.assertEqual(response.status_code, 500, response.content)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration, 0)
        self.assertFalse(self.session.workspaceBaselineKnown)
        self.assertFalse(SessionWorkspacePreservation.objects.exists())

    def test_lost_ack_replay_after_later_commit_keeps_the_newer_head(self):
        body, _ = self.preserve_body()
        first = self.preserve(body)
        self.assertEqual(first.status_code, 200, first.content)
        self.session.refresh_from_db()
        later = AgentRun.objects.create(workspace=self.run.workspace, session=self.session, user=self.run.user,
            modelConfig=self.run.modelConfig, prompt="later committed files", status="running")
        grant = create_agent_run_authorization(later, image_digest="sha256:" + "a" * 64)
        job = f"agent_run.lifecycle:{later.pk}"
        owner = "worker:later-session-commit"
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,lease_expires_at_ms,"
                "idempotency_key,session_id,payload_ref) VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                [job,owner,f"{job}:{grant.digest}",self.session.pk,f"record:agent_run:{later.pk}"])
        def remove_later_job():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s", [job])
        self.addCleanup(remove_later_job)
        metadata = json.dumps({"schema":"runtime.session_workspace.commit.v2","jobId":job,"leaseOwner":owner,
            "agentRunId":later.pk,"authorizationDigest":grant.digest,"snapshotSha256":"",
            "snapshotEncoding":"frameV1","snapshotSizeBytes":0,"expandedSizeBytes":0,"fileCount":0},separators=(",",":")).encode()
        committed = self.client.post("/internal/agent-runs/session-workspace/commit",
            data=len(metadata).to_bytes(4,"big")+metadata,content_type="application/octet-stream",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(committed.status_code,201,committed.content)
        self.session.refresh_from_db()
        head = session_workspace_for_session(self.session)
        self.assertEqual(head["generation"],2)
        replay = self.preserve(body)
        self.assertEqual(replay.status_code,200,replay.content)
        self.assertEqual(replay.json()["preservationReceiptId"],first.json()["preservationReceiptId"])
        self.assertEqual(replay.json()["disposition"],"idempotent")
        self.session.refresh_from_db()
        self.assertEqual(session_workspace_for_session(self.session),head)
        rebase, _ = self.preserve_body(b"stale root",baseline=head)
        rejected = self.preserve(rebase)
        self.assertEqual(rejected.status_code,409,rejected.content)
        self.assertEqual(rejected.json(),{"error":"session_workspace_baseline_conflict"})

    def test_lease_replacement_during_upload_cannot_publish(self):
        from .http import internal
        store=internal._store_workspace_snapshot
        def replaced(*args,**kwargs):
            store(*args,**kwargs)
            self._lease(self.replacement)
        body,_=self.preserve_body()
        with patch.object(internal,"_store_workspace_snapshot",replaced):
            response=self.preserve(body)
        self.assertEqual(response.status_code,409,response.content)
        self.session.refresh_from_db()
        self.assertEqual(self.session.workspaceGeneration,0)
        self.assertFalse(self.session.workspaceBaselineKnown)

    def test_newer_head_is_preserved_instead_of_overwritten(self):
        body,snapshot=self.preserve_body()
        self.assertEqual(self.preserve(body).status_code,200)
        changed,_=self.preserve_body(b"later uncommitted capture")
        rejected=self.preserve(changed)
        self.assertEqual(rejected.status_code,409,rejected.content)
        self.assertEqual(rejected.json(),{"error":"session_workspace_baseline_conflict"})
        self._assert_snapshot(snapshot)
