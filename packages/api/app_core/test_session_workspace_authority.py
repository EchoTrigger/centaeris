"""An absent legacy workspace descriptor does not prove an empty workspace."""
import json
from types import SimpleNamespace

from django.conf import settings
from django.test import SimpleTestCase, TestCase

from .agent_run_authorization_factory import create_agent_run_authorization
from .models import AgentRun, Session
from .test_hosted_operations import OperationFixture
from .testing import leased_lifecycle_job_fixture


class SessionWorkspaceAuthorityContractTests(SimpleTestCase):
    def test_unknown_baseline_cannot_be_projected_into_signed_empty_authority(self):
        from .runtime_contract import build_agent_run_authorization_payload
        from .test_agent_run_authorization import fixture
        wire = fixture()
        run = SimpleNamespace(
            session=SimpleNamespace(agent_id=wire["agentId"], workspaceBaselineKnown=False,
                workspaceGeneration=0, workspaceSnapshotSha256="", workspaceSnapshotSizeBytes=0,
                workspaceExpandedSizeBytes=0, workspaceFileCount=0),
            workspace_id=wire["workspaceId"], user_id=wire["userId"], session_id=wire["sessionId"],
            id=wire["agentRunId"], modelConfig_id=wire["modelConfigRef"], thinkingMode=wire["thinkingMode"],
            environment=SimpleNamespace(environment_profile=wire["environmentProfile"],
                environment_profile_digest=wire["environmentProfileDigest"]),
            definition_version_id="fixture-version",
            definition_version=SimpleNamespace(plugin_activation=wire["pluginActivation"]))
        with self.assertRaisesMessage(ValueError, "session_workspace_baseline_unavailable"):
            build_agent_run_authorization_payload(run, wire["id"], image_digest="sha256:" + "b" * 64,
                declared_inputs=[])


class SessionWorkspaceAuthorityTests(OperationFixture, TestCase):
    def test_new_session_has_authoritative_empty_baseline(self):
        session = Session.objects.get(pk=self.create().json()["sessionId"])
        self.assertTrue(session.workspaceBaselineKnown)

    def test_unknown_legacy_baseline_cannot_authorize_an_empty_new_instance(self):
        session = Session.objects.get(pk=self.create().json()["sessionId"])
        Session.objects.filter(pk=session.pk).update(workspaceBaselineKnown=False)
        session.refresh_from_db()
        run = AgentRun.objects.create(session=session, workspace=self.workspace, user=self.user,
            modelConfig=self.model, prompt="continue original session",
            definition_version=self.agent.definition.published_version,
            agent_instructions=self.agent.definition.published_version.instructions)
        with self.assertRaisesMessage(ValueError, "session_workspace_baseline_unavailable"):
            create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        self.assertFalse(hasattr(run, "authorization"))
        self.assertEqual(session.workspaceGeneration, 0)

    def test_resolve_rejects_unknown_baseline_even_for_preexisting_signed_authorization(self):
        response = self.post()
        self.assertEqual(response.status_code, 202, response.content)
        run = AgentRun.objects.get(pk=response.json()["agentRunId"])
        owner = leased_lifecycle_job_fixture(self, run)
        Session.objects.filter(pk=run.session_id).update(workspaceBaselineKnown=False)
        resolved = self.client.post("/internal/agent-runs/session-workspace/resolve",
            data=json.dumps({"schema": "runtime.session_workspace.resolve.v1",
                "jobId": f"agent_run.lifecycle:{run.pk}", "leaseOwner": owner,
                "agentRunId": run.pk, "authorizationDigest": run.authorization.digest}),
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
        self.assertEqual(resolved.status_code, 409, resolved.content)
        self.assertEqual(resolved.json(), {"error": "session_workspace_baseline_unavailable"})

    def test_explicit_empty_commit_records_authority_and_is_idempotent(self):
        response = self.post()
        run = AgentRun.objects.get(pk=response.json()["agentRunId"])
        owner = leased_lifecycle_job_fixture(self, run)
        Session.objects.filter(pk=run.session_id).update(workspaceBaselineKnown=False)
        metadata = json.dumps({"schema": "runtime.session_workspace.commit.v2",
            "jobId": f"agent_run.lifecycle:{run.pk}", "leaseOwner": owner,
            "agentRunId": run.pk, "authorizationDigest": run.authorization.digest,
            "snapshotEncoding": "frameV1", "snapshotSha256": "", "snapshotSizeBytes": 0, "expandedSizeBytes": 0, "fileCount": 0},
            separators=(",", ":")).encode()
        body = len(metadata).to_bytes(4, "big") + metadata
        for disposition in ("committed", "idempotent"):
            committed = self.client.post("/internal/agent-runs/session-workspace/commit", data=body,
                content_type="application/octet-stream", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)
            self.assertEqual(committed.status_code, 201 if disposition == "committed" else 200, committed.content)
            self.assertEqual(committed.json()["disposition"], disposition)
        run.session.refresh_from_db()
        self.assertTrue(run.session.workspaceBaselineKnown)
        self.assertEqual(run.session.workspaceGeneration, 1)
        self.assertEqual(run.session.workspaceLastAdvancedAgentRun_id, run.pk)
