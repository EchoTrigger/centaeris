"""Dispatch validation is private native authority, never hosted admission."""
import hashlib
import json
import tempfile
from datetime import timedelta

from django.conf import settings
from django.contrib.auth import get_user_model
from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.db import transaction
from django.test import TestCase, override_settings
from django.utils import timezone

from .agent_run_authorization_factory import create_agent_run_authorization
from .assets import captured_input_fields
from .models import (Agent, AgentCoordinationSession, AgentDefinition, AgentDefinitionVersion,
    AgentRun, AgentWorkSession, Artifact, BusinessApplication, HostedOperationReceipt,
    ModelConfig, Session, SessionAssetLink, SessionEvent, UserAppDelegation, UserLibraryObject, Workspace, WorkspaceMembership)
from .runtime_client import build_agent_run_start


class AgentWorkValidationTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="dispatch-owner")
        self.workspace = Workspace.objects.create(name="Native dispatch", createdBy=self.user)
        self.membership = WorkspaceMembership.objects.create(workspace=self.workspace, user=self.user, role="owner")
        self.agent = Agent.objects.create(workspace=self.workspace, owner=self.user, name="Private coordinator")
        self.source = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent, origin="automation")
        AgentCoordinationSession.objects.create(agent=self.agent, session=self.source)
        self.model = ModelConfig.objects.create(displayName="Dispatch fixture model")
        self.run = self.new_run()

    def new_run(self, session=None, **fields):
        run = AgentRun.objects.create(workspace=self.workspace, session=session or self.source, user=self.user,
            modelConfig=self.model, prompt="Dispatch", status="running", **fields)
        create_agent_run_authorization(run, image_digest="sha256:" + "a" * 64)
        return run

    def validate(self, run=None, **changes):
        run = run or self.run
        body = {"schema": "workspace.agent_work.validate.v1", "agentRunId": run.id,
                "authorizationDigest": run.authorization.digest, "coordinationSessionId": run.session_id,
                "toolCallId": "dispatch-one", "objective": "  Complete objective\nUnicode: 调度  ",
                "sessionRefs": [], "fileRefs": []}
        body.update(changes)
        return self.client.post("/internal/agent-work/validate", data=json.dumps(body),
            content_type="application/json", HTTP_X_INTERNAL_TOKEN=settings.INTERNAL_API_TOKEN)

    def assert_no_work(self, sessions=1, runs=1):
        self.assertEqual(Session.objects.count(), sessions)
        self.assertEqual(AgentRun.objects.count(), runs)
        self.assertFalse(AgentWorkSession.objects.exists())
        self.assertFalse(HostedOperationReceipt.objects.exists())

    def test_private_native_validation_returns_exact_unadmitted_identity_without_writes(self):
        response = self.validate()
        self.assertEqual(response.status_code, 200, response.content)
        args = {"objective": "  Complete objective\nUnicode: 调度  ", "session_refs": [], "file_refs": []}
        encoded = json.dumps(args, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
        self.assertEqual(response.json(), {"schema": "workspace.agent_work.validated.v1",
            "agentId": self.agent.id, "sessionId": self.source.id, "agentRunId": self.run.id,
            "toolCallId": "dispatch-one", "authorizationDigest": self.run.authorization.digest,
            "inputDigest": "sha256:" + hashlib.sha256(b"workspace.agent_work.input.v1\0" + encoded).hexdigest()})
        self.assert_no_work()
        self.assertFalse(SessionEvent.objects.exists())
        self.assertEqual(build_agent_run_start(self.run)["nativeCoordinationSessionId"], self.source.id)

    def test_ordinary_session_has_no_native_tool_registration_identity_and_cannot_validate(self):
        ordinary = Session.objects.create(workspace=self.workspace, owner=self.user, agent=self.agent)
        run = self.new_run(ordinary)
        self.assertNotIn("nativeCoordinationSessionId", build_agent_run_start(run))
        self.assertEqual(self.validate(run).status_code, 403)
        self.assert_no_work(2, 2)

    def test_available_managed_coordination_cannot_validate_or_register_native_work(self):
        definition = AgentDefinition.objects.create(workspace=self.workspace, created_by=self.user,
            name="Managed coordinator", availability_scope="workspace")
        version = AgentDefinitionVersion.objects.create(definition=definition, version=1,
            name=definition.name, published_by=self.user)
        definition.published_version = version
        definition.save(update_fields=["published_version"])
        Agent.objects.filter(pk=self.agent.pk).update(definition=definition)
        self.source.refresh_from_db()
        run = self.new_run(definition_version=version)
        self.assertNotIn("nativeCoordinationSessionId", build_agent_run_start(run))
        self.assertEqual(self.validate(run).status_code, 403)
        self.assert_no_work(1, 2)

    def test_incomplete_application_origin_cannot_start_native_execution(self):
        app = BusinessApplication.objects.create(name="Dispatch fixture app", created_by=self.user, status="active")
        with self.assertRaisesMessage(ValueError, "AgentRun application origin is incomplete"):
            self.new_run(acting_app=app)
        self.assert_no_work()

    def test_membership_revocation_tampering_and_source_identity_fail_closed(self):
        for changes in ({"authorizationDigest": "sha256:" + "f" * 64},
                        {"coordinationSessionId": "foreign-session"}, {"agentRunId": "unknown-run"}):
            with self.subTest(changes=changes):
                self.assertEqual(self.validate(**changes).status_code, 403)
        self.membership.delete()
        self.assertEqual(self.validate().status_code, 403)
        self.assert_no_work()

    def test_current_managed_delegation_cannot_validate_native_dispatch(self):
        definition = AgentDefinition.objects.create(workspace=self.workspace, created_by=self.user,
            name="Delegated coordinator", availability_scope="workspace")
        version = AgentDefinitionVersion.objects.create(definition=definition, version=1,
            name=definition.name, published_by=self.user)
        definition.published_version = version
        definition.save(update_fields=["published_version"])
        Agent.objects.filter(pk=self.agent.pk).update(definition=definition)
        self.source.refresh_from_db()
        app = BusinessApplication.objects.create(name="Delegated dispatch app", created_by=self.user, status="active")
        grant = UserAppDelegation.objects.create(user=self.user, app=app, workspace=self.workspace,
            definition=definition, membership_ref=self.membership.id,
            scopes=["assistant:use", "messages:submit", "sessions:read"],
            token_digest="sha256:" + "b" * 64, expires_at=timezone.now() + timedelta(hours=1))
        run = self.new_run(definition_version=version, acting_app=app, app_delegation=grant)
        self.assertNotIn("nativeCoordinationSessionId", build_agent_run_start(run))
        self.assertEqual(self.validate(run).status_code, 403)
        self.assert_no_work(1, 2)

    def test_transport_rejects_unknown_fields_invalid_utf8_budget_and_internal_auth(self):
        for case, changes in enumerate(({"objective": " "}, {"objective": "调" * 21846},
                        {"sessionRefs": ["ref"] * 9}, {"fileRefs": ["/mnt/data/private"]},
                        {"recipientSessionId": "other"}, {"objective": 1})):
            with self.subTest(case=case):
                self.assertEqual(self.validate(**changes).status_code, 400)
        self.assertEqual(self.client.post("/internal/agent-work/validate", data="{}",
            content_type="application/json").status_code, 401)
        self.assert_no_work()

    def test_current_library_inputs_validate_but_artifacts_and_unsigned_refs_do_not(self):
        with tempfile.TemporaryDirectory(prefix="dispatch-inputs-") as storage, override_settings(MEDIA_ROOT=storage):
            content = b"Complete evidence"
            key = default_storage.save("evidence.txt", ContentFile(content))
            digest = "sha256:" + hashlib.sha256(content).hexdigest()
            item = UserLibraryObject.objects.create(owner=self.user, displayName="evidence.txt", objectKind="file",
                contentType="text/plain", storageKey=key, sizeBytes=len(content), contentGeneration=1,
                sha256=digest, status="ready")
            artifact = Artifact.objects.create(workspace=self.workspace, session=self.source, agent_run=self.run,
                createdBy=self.user, displayName="artifact.txt", contentType="text/plain", storageKey=key,
                sizeBytes=len(content), sha256=digest, contentGeneration=1, status="published", publishedAt=timezone.now())
            links = []
            for owner, field in ((item, "userLibraryObject"), (artifact, "artifact")):
                links.append(SessionAssetLink.objects.create(workspace=self.workspace, session=self.source,
                    **{field: owner}, attachedBy=self.user, capturedDisplayName=owner.displayName,
                    capturedContentType=owner.contentType, **captured_input_fields(owner)))
            run = self.new_run()
            self.assertEqual(self.validate(run, fileRefs=[links[0].id]).status_code, 200)
            self.assertEqual(self.validate(run, fileRefs=[links[1].id]).status_code, 403)
            self.assertEqual(self.validate(run, fileRefs=[item.id]).status_code, 403)
            UserLibraryObject.objects.filter(pk=item.pk).update(contentGeneration=2)
            self.assertEqual(self.validate(run, fileRefs=[links[0].id]).status_code, 403)
            self.assertEqual(self.validate(run, sessionRefs=["unknown-session"]).status_code, 403)
            self.assert_no_work(1, 2)

    def test_validation_success_followed_by_source_commit_rollback_creates_no_work(self):
        self.assertEqual(self.validate().status_code, 200)
        with transaction.atomic():
            SessionEvent.objects.create(eventId="rolled-back-dispatch", workspace=self.workspace,
                session=self.source, agent_run=self.run, sequence=1, agent_run_sequence=1,
                payload={"type": "tool_result", "payload": {"toolName": "dispatch_work",
                         "callId": "dispatch-one", "resultState": "successWithOutput"}},
                createdAtMs=1, projects_to_agent_run_stream=True)
            transaction.set_rollback(True)
        self.assertFalse(SessionEvent.objects.exists())
        self.assert_no_work()
