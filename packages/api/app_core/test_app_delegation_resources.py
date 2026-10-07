import hashlib
import tempfile
from unittest.mock import patch

from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.core.files.uploadedfile import SimpleUploadedFile
from django.test import TestCase, override_settings
from django.utils import timezone

from app_core.assets import captured_input_fields
from app_core.models import Agent, AgentRun, Artifact, ModelConfig, Session, SessionAssetLink, SessionCitationProjection, Source, SourceGrant, SourceObject, UserLibraryObject, WorkspaceGroup
from app_core.test_app_delegations import AppDelegationSessionFixture
from app_core.test_agent_definitions import PROFILE


class AppDelegationResourceTests(AppDelegationSessionFixture, TestCase):
    def setUp(self):
        super().setUp()
        storage = self.enterContext(tempfile.TemporaryDirectory(prefix="centaeris-delegated-resources-"))
        self.enterContext(override_settings(MEDIA_ROOT=storage))

    def fixture_run(self, session=None):
        session = session or self.session
        version = session.agent.definition.published_version if session.agent.definition_id else None
        return AgentRun.objects.create(workspace=self.workspace, session=session, user=self.member,
            modelConfig=ModelConfig.objects.create(displayName="Fixture"), prompt="Fixture",
            definition_version=version, agent_instructions=version.instructions if version else "")

    def library_object(self, name="evidence.txt"):
        content = b"Primary evidence"
        storage_key = default_storage.save("delegation-fixture/" + name, ContentFile(content))
        return UserLibraryObject.objects.create(owner=self.member, displayName=name, objectKind="file",
            contentType="text/plain", sizeBytes=len(content), sha256="sha256:" + hashlib.sha256(content).hexdigest(),
            storageKey=storage_key, status="ready", contentGeneration=1)

    def link(self, item, session=None):
        return SessionAssetLink.objects.create(workspace=self.workspace, session=session or self.session,
            userLibraryObject=item, attachedBy=self.member, capturedDisplayName=item.displayName,
            capturedContentType=item.contentType, **captured_input_fields(item))

    def test_uploads_keep_session_asset_links_and_message_refs_separate(self):
        first = self.library_object()
        first_link = self.link(first)
        uploaded = self.bearer_client.post(f"/api/sessions/{self.session.id}/uploads",
            {"files": SimpleUploadedFile("second.txt", b"Second evidence", content_type="text/plain")},
            HTTP_AUTHORIZATION="Bearer " + self.token)
        self.assertEqual(uploaded.status_code, 201, uploaded.content)
        body = uploaded.json()
        second_link = SessionAssetLink.objects.get(id=body["assets"][0]["id"])
        self.assertEqual(second_link.session_id, self.session.id)
        self.assertEqual(second_link.attachedBy_id, self.member.id)
        self.assertEqual(second_link.userLibraryObject_id, body["libraryObjects"][0]["id"])
        self.assertEqual(self.api("get", f"/api/sessions/{self.session.id}/assets").json(),
                         self.member_client.get(f"/api/sessions/{self.session.id}/assets").json())
        with patch("app_core.http.workspaces.request_execution_profile", return_value=PROFILE), \
             patch("app_core.http.workspaces.schedule_agent_run_lifecycle", return_value="inserted"):
            accepted = self.api("post", self.base + f"/sessions/{self.session.id}/messages",
                {"operationId": "attachment-message", "text": "Read second evidence",
                 "modelConfigRef": ModelConfig.objects.create(displayName="Fixture").id,
                 "attachmentRefs": [second_link.id]})
        self.assertEqual(accepted.status_code, 202, accepted.content)
        run = AgentRun.objects.get(id=accepted.json()["agentRunId"])
        self.assertEqual(run.authorization.payload["messageAssetRefs"], [second_link.id])
        self.assertEqual({asset["inputRef"] for asset in run.authorization.payload["assetRefs"]},
                         {first_link.id, second_link.id})

    def test_private_library_is_unavailable_until_linked_to_the_granted_assistant(self):
        item = self.library_object()
        path = f"/api/library/{item.id}/download"
        denied = self.api("get", path)
        self.assertEqual(denied.status_code, 404, denied.content)
        self.link(item)
        allowed = self.api("get", path)
        self.assertEqual(allowed.status_code, 200)
        from app_core.tests import streaming_response_bytes
        self.assertEqual(streaming_response_bytes(allowed), b"Primary evidence")
        self.assertEqual(self.api("get", "/api/library").status_code, 401)
        self.assertEqual(self.api("post", f"/api/sessions/{self.session.id}/assets",
                         {"assetKind": "userLibraryObject", "assetId": item.id}).status_code, 401)

    def test_another_assistants_link_does_not_authorize_download(self):
        private = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent=private)
        item = self.library_object()
        self.link(item, session)
        denied = self.api("get", f"/api/library/{item.id}/download")
        self.assertEqual(denied.status_code, 404, denied.content)

    def test_linked_source_download_still_requires_the_users_current_source_acl(self):
        item = self.library_object()
        source = Source.objects.create(workspace=self.workspace, sourceType="fileTree", name="Fixture",
                                       status="ready", createdBy=self.admin)
        obj = SourceObject.objects.create(workspace=self.workspace, source=source, objectType="file",
            displayPath="evidence.txt", displayName="evidence.txt", contentType=item.contentType,
            sizeBytes=item.sizeBytes, sha256=item.sha256, storageKey=item.storageKey, status="ready", contentGeneration=1)
        group = WorkspaceGroup.objects.create(workspace=self.workspace, name="Fixture readers", createdBy=self.admin)
        group.members.add(self.membership)
        access = SourceGrant.objects.create(workspace=self.workspace, source=source, workspaceGroup=group, createdBy=self.admin)
        path = f"/api/source-objects/{obj.id}/download"
        self.assertEqual(self.api("get", path).status_code, 404)
        SessionAssetLink.objects.create(workspace=self.workspace, session=self.session, sourceObject=obj,
            attachedBy=self.member, capturedDisplayName=obj.displayName, capturedContentType=obj.contentType,
            **captured_input_fields(obj))
        response = self.api("get", path)
        self.assertEqual(response.status_code, 200)
        from app_core.tests import streaming_response_bytes
        self.assertEqual(streaming_response_bytes(response), b"Primary evidence")
        access.delete()
        self.assertEqual(self.api("get", path).status_code, 404)

    def test_private_assistants_artifact_cannot_be_downloaded_with_the_managed_grant(self):
        private = Agent.objects.create(workspace=self.workspace, owner=self.member, name="Private")
        session = Session.objects.create(workspace=self.workspace, owner=self.member, agent=private)
        run, item = self.fixture_run(session), self.library_object()
        artifact = Artifact.objects.create(workspace=self.workspace, session=session, agent_run=run,
            createdBy=self.member, displayName="private.txt", safeFilename="private.txt", contentType=item.contentType,
            sizeBytes=item.sizeBytes, sha256=item.sha256, storageKey=item.storageKey, status="published", publishedAt=timezone.now())
        response = self.api("get", f"/api/artifacts/{artifact.id}/download")
        self.assertEqual(response.status_code, 404, response.content)

    def test_citations_and_stable_artifact_download_keep_existing_contracts(self):
        run = self.fixture_run()
        item = self.library_object()
        citation = SessionCitationProjection.objects.create(citationId="citation:delegation-fixture",
            workspace=self.workspace, session=self.session, agent_run=run, sequence=1, inputRef="fixture-input",
            ownerRef=item.id, ownerKind="userLibraryObject", displayName=item.displayName, evidenceKind="userProvided",
            ownerSha256=item.sha256, sourceToolCallId="fixture-call", locator={"startLine": 1, "endLine": 1})
        detail_path = f"/api/citations/{citation.citationId}"
        detail = self.api("get", detail_path)
        self.assertEqual(detail.status_code, 200, detail.content)
        self.assertEqual(detail.json(), self.member_client.get(detail_path).json())
        self.assertEqual(detail.json()["citation"]["downloadUrl"], f"/api/library/{item.id}/download")
        from app_core.tests import streaming_response_bytes
        preview = self.api("get", detail_path + "/preview")
        self.assertEqual(preview.status_code, 200)
        self.assertEqual(streaming_response_bytes(preview), b"Primary evidence")
        artifact = Artifact.objects.create(workspace=self.workspace, session=self.session, agent_run=run,
            createdBy=self.member, displayName="answer.txt", safeFilename="answer.txt", contentType=item.contentType,
            sizeBytes=item.sizeBytes, sha256=item.sha256, storageKey=item.storageKey, status="published", publishedAt=timezone.now())
        downloaded = self.api("get", f"/api/artifacts/{artifact.id}/download")
        self.assertEqual(downloaded.status_code, 200)
        self.assertEqual(streaming_response_bytes(downloaded), b"Primary evidence")
        self.assertIn('answer.txt', downloaded["Content-Disposition"])

    def test_history_calls_reuse_browser_payloads_and_session_identity(self):
        page = {"schema": "transcript.page.v1", "sessionId": self.session.id,
            "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation-fixture",
            "sourceHighWater": "0", "blocks": [], "olderCursor": None, "hasOlder": False,
            "resumeCursors": [{"streamId": "workspace-transcript.v1", "cursor": "0"}]}
        with patch("app_core.http.workspaces.request_transcript_page", return_value=page) as read_page:
            path = f"/api/sessions/{self.session.id}/transcript"
            response = self.api("get", path)
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json(), self.member_client.get(path).json())
            self.assertEqual(read_page.call_args.args[0]["sessionId"], self.session.id)
        patches = {"schema": "transcript.patch.page.v1", "sessionId": self.session.id,
                   "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation-fixture",
                   "throughSourceHighWater": "0", "nextSourceHighWater": "0", "patches": [], "hasMore": False}
        with patch("app_core.http.workspaces.request_transcript_patches", return_value=patches) as read_patches:
            path = f"/api/sessions/{self.session.id}/transcript/patches?afterSourceHighWater=0&projectionGeneration=generation-fixture"
            response = self.api("get", path)
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json(), self.member_client.get(path).json())
            self.assertEqual(read_patches.call_args.args[0]["sessionId"], self.session.id)
        content = {"schema": "transcript.content.range.v1", "sessionId": self.session.id,
                   "projectionVersion": "transcript.projection.v1", "projectionGeneration": "generation-fixture",
                   "refId": "session-event:fixture", "revision": "1", "byteLength": "5",
                   "startOffset": "0", "endOffset": "5", "content": "Hello", "hasMore": False}
        with patch("app_core.http.workspaces.request_transcript_content", return_value=content) as read_content:
            path = f"/api/sessions/{self.session.id}/transcript/content?projectionGeneration=generation-fixture&refId=session-event:fixture&revision=1&byteLength=5&offset=0"
            response = self.api("get", path)
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json(), self.member_client.get(path).json())
            self.assertEqual(read_content.call_args.args[0]["sessionId"], self.session.id)
        for suffix in ["/transcript/turn-metadata?sequence=0", "/transcript/active-agent-run?sourceHighWater=0",
                       "/context-usage", "/transcript/citations?sequence=0"]:
            path = f"/api/sessions/{self.session.id}" + suffix
            response = self.api("get", path)
            self.assertEqual(response.status_code, 200, response.content)
            self.assertEqual(response.json(), self.member_client.get(path).json())

    def test_cancel_preserves_requested_disposition_and_does_not_fabricate_terminal(self):
        run = self.fixture_run()
        with patch("app_core.http.workspaces.request_agent_run_cancellation", return_value={"disposition": "requested"}) as cancel:
            response = self.api("post", f"/api/sessions/{self.session.id}/agent-runs/{run.id}/cancel", {})
        self.assertEqual(response.status_code, 202, response.content)
        self.assertEqual(response.json(), {"agentRunId": run.id, "status": "queued", "disposition": "requested"})
        cancel.assert_called_once()
        run.refresh_from_db()
        self.assertEqual(run.transitionReason, "agent_run_cancel_requested")
        self.assertIsNone(run.completedAt)

    def test_revocation_during_upload_storage_rolls_back_links_and_library_rows(self):
        from app_core.http.library import _store_upload_batch
        def store_then_revoke(*args, **kwargs):
            stored = _store_upload_batch(*args, **kwargs)
            self.assertEqual(self.member_client.delete(f"/api/account/app-delegations/{self.grant.id}").status_code, 204)
            return stored
        with patch("app_core.http.library._store_upload_batch", side_effect=store_then_revoke):
            response = self.bearer_client.post(f"/api/sessions/{self.session.id}/uploads",
                {"files": SimpleUploadedFile("revoked.txt", b"Rejected evidence", content_type="text/plain")},
                HTTP_AUTHORIZATION="Bearer " + self.token)
        self.assertEqual(response.status_code, 403, response.content)
        self.assertFalse(UserLibraryObject.objects.filter(displayName="revoked.txt").exists())
        self.assertFalse(self.session.assetLinks.exists())
