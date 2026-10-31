"""Downloads identify authoritative stored encoding without changing object bytes."""
import hashlib
import json
from pathlib import Path

from django.conf import settings
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import connection
from django.test import TransactionTestCase

from . import test_execution_publication_fencing as fixtures
from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun,Session
from .tests import streaming_response_bytes


class WorkspaceSnapshotDownloadEncodingTests(fixtures.ExecutionPublicationFixture,TransactionTestCase):
    serialized_rollback=True
    _checkpoint_download_body=fixtures.ExecutionPublicationFencingTests._checkpoint_download_body

    def download_head(self,encoding):
        vector=json.loads((Path(__file__).resolve().parents[3]/"tests/workspace/fixtures/workspace_archive/v1/valid.json").read_text(encoding="utf-8"))
        snapshot=bytes.fromhex(vector[encoding+"Hex"])
        sha="sha256:"+hashlib.sha256(snapshot).hexdigest()
        key=f"workspaces/{self.session.workspace_id}/sessions/{self.session.pk}/snapshots/1/{sha[7:]}.snapshot"
        default_storage.save(key,ContentFile(snapshot))
        Session.objects.filter(pk=self.session.pk).update(workspaceGeneration=1,workspaceStorageKey=key,
            workspaceSnapshotSha256=sha,workspaceSnapshotEncoding=encoding,workspaceSnapshotSizeBytes=len(snapshot),
            workspaceExpandedSizeBytes=sum(file["sizeBytes"] for file in vector["manifest"]["files"]),
            workspaceFileCount=len(vector["manifest"]["files"]),workspaceLastAdvancedAgentRun=self.run)
        self.session.refresh_from_db()
        AgentRun.objects.filter(pk=self.run.pk).update(status="completed")
        reader=AgentRun.objects.create(session=self.session,workspace=self.run.workspace,user=self.run.user,
            modelConfig=self.run.modelConfig,prompt="restore exact stored bytes",status="running")
        grant=create_agent_run_authorization(reader,image_digest="sha256:"+"a"*64)
        job=f"agent_run.lifecycle:{reader.pk}"
        owner="worker:encoding-download"
        with connection.cursor() as cursor:
            cursor.execute("INSERT INTO runtime.runtime_jobs(job_id,job_kind,status,lease_owner,lease_expires_at_ms,"
                "idempotency_key,session_id,payload_ref) VALUES(%s,'agent_run.lifecycle','running',%s,"
                "(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000,%s,%s,%s)",
                [job,owner,f"{job}:{grant.digest}",self.session.pk,f"record:agent_run:{reader.pk}"])
        def delete_reader_job():
            with connection.cursor() as cursor:
                cursor.execute("DELETE FROM runtime.runtime_jobs WHERE job_id=%s",[job])
        self.addCleanup(delete_reader_job)
        response=self.client.post("/internal/agent-runs/session-workspace/download",data=json.dumps({
            "schema":"runtime.session_workspace.download.v1","jobId":job,"leaseOwner":owner,
            "agentRunId":reader.pk,"authorizationDigest":grant.digest}),content_type="application/json",
            HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            HTTP_X_WORKSPACE_SNAPSHOT_ENCODING="tarZstdV1" if encoding=="frameV1" else "frameV1")
        received=streaming_response_bytes(response)
        self.assertEqual(response.status_code,200,received)
        self.assertEqual(response.get("X-Workspace-Snapshot-Encoding"),encoding)
        self.assertEqual(response["X-Content-Sha256"],sha)
        self.assertEqual(response["Content-Length"],str(len(snapshot)))
        self.assertEqual(received,snapshot)
        with default_storage.open(key,"rb") as stored:
            self.assertEqual(stored.read(),snapshot)
        self.session.refresh_from_db()
        self.assertEqual((self.session.workspaceGeneration,self.session.workspaceSnapshotEncoding,self.session.workspaceStorageKey),(1,encoding,key))

    def test_session_frame_header_matches_its_locked_stored_head(self):
        self.download_head("frameV1")

    def test_session_archive_header_preserves_original_compressed_identity(self):
        self.download_head("tarZstdV1")

    def test_execution_download_uses_its_immutable_manifest_encoding(self):
        body,snapshot,_=self._checkpoint_download_body()
        response=self.client.post("/internal/agent-runs/execution-workspace/download",data=json.dumps(body),
            content_type="application/json",HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN,
            HTTP_X_WORKSPACE_SNAPSHOT_ENCODING="tarZstdV1")
        received=streaming_response_bytes(response)
        self.assertEqual(response.status_code,200,received)
        self.assertEqual(response.get("X-Workspace-Snapshot-Encoding"),"fileManifestV1")
        self.assertEqual(response["Content-Length"],str(len(snapshot)))
        self.assertEqual(response["X-Content-Sha256"],"sha256:"+hashlib.sha256(snapshot).hexdigest())
        self.assertEqual(received,snapshot)
